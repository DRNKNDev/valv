import Combine
import DaemonKit
import Foundation

/// One of the five states the menu-bar icon and dropdown summary reflect, in the
/// precedence order the `macos-app` spec defines: not-set-up > error > paused >
/// syncing > synced.
enum IconState: Hashable {
    case notSetUp
    case error
    case paused
    case syncing
    case synced
}

/// Central, app-wide source of daemon state for the menu bar, onboarding, and Manage
/// Folders & Sharing window. Polls `GET /status` on an interval rather than pushing -
/// `DaemonClient.status()` is not itself a push mechanism (macos-app spec).
// No explicit @MainActor - this target's SWIFT_DEFAULT_ACTOR_ISOLATION is already
// MainActor project-wide, and an explicit annotation here triggers a spurious
// "does not conform to protocol 'ObservableObject'" compiler error.
final class DaemonStore: ObservableObject {
    static let shared = DaemonStore()

    @Published private(set) var status: DaemonStatus?
    @Published private(set) var lastError: Error?
    @Published private(set) var lastSuccessAt: Date?
    /// Set once first-run sign-in (section 7's onboarding flow) succeeds. Tracked
    /// locally rather than by reading `valvd`'s config.toml directly (this app is
    /// sandboxed - see design.md D2/D3 - and the daemon's own connectivity, not a
    /// config file read, is the actual signal `DaemonClient` can act on anyway).
    @Published var hasSignedIn: Bool {
        didSet { userDefaults.set(hasSignedIn, forKey: Self.signedInDefaultsKey) }
    }

    private static let signedInDefaultsKey = "dev.drnkn.valv.hasSignedIn"

    private let client: DaemonClient
    private let userDefaults: UserDefaults
    private let clearDeviceIdentity: () throws -> Void
    private let restartDaemonOperation: (() -> Void)?
    private var pollTask: Task<Void, Never>?

    init(
        client: DaemonClient = DaemonClient(),
        userDefaults: UserDefaults = .standard,
        clearDeviceIdentity: @escaping () throws -> Void = { try ConfigWriter.clearDeviceIdentity() },
        restartDaemonOperation: (() -> Void)? = nil
    ) {
        self.client = client
        self.userDefaults = userDefaults
        self.clearDeviceIdentity = clearDeviceIdentity
        self.restartDaemonOperation = restartDaemonOperation
        self.hasSignedIn = userDefaults.bool(forKey: Self.signedInDefaultsKey)
    }

    var iconState: IconState {
        guard hasSignedIn, let status else {
            return .notSetUp
        }
        if status.mounts.contains(where: { $0.error != nil }) {
            return .error
        }
        if status.paused {
            return .paused
        }
        if status.mounts.contains(where: { $0.syncing }) {
            return .syncing
        }
        return .synced
    }

    var isDisconnected: Bool {
        hasSignedIn && status == nil && lastError != nil
    }

    var hasLapsedPlan: Bool {
        guard let accountStatus = status?.account?.status else {
            return false
        }
        return ["past_due", "canceled", "revoked"].contains(accountStatus)
    }

    func startPolling(interval: TimeInterval = 5) {
        stopPolling()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
            }
        }
    }

    func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }

    func refresh() async {
        do {
            status = try await client.status()
            lastError = nil
            lastSuccessAt = Date()
        } catch {
            status = nil
            lastError = error
        }
    }

    func pause() async {
        try? await client.pause()
        await refresh()
    }

    func resume() async {
        try? await client.resume()
        await refresh()
    }

    func syncNow(folderId: String? = nil) async {
        _ = try? await client.sync(folderId: folderId)
        await refresh()
    }

    func mount(_ request: MountRequest) async throws -> MountResponse {
        let response = try await client.mount(request)
        await refresh()
        return response
    }

    /// Unmounts locally only - does not touch the backend folder/grants, and does not
    /// delete the locally materialized files.
    func unmount(folderId: String) async throws {
        try await client.unmount(folderId: folderId)
        await refresh()
    }

    /// `GET /nodes/:node_id/path` is a *daemon* route (`ipc-control-api` capability),
    /// not a backend one - resolves against the daemon's local SQLite mirror, which is
    /// why this goes through `DaemonClient` rather than `BackendClient` even though
    /// the Manage Folders window's other calls (`GET /grants`, invites, device grants)
    /// deliberately go straight to the backend.
    func nodePath(nodeId: String) async throws -> String {
        try await client.nodePath(nodeId: nodeId).path
    }

    func signOut(domainManager: FileProviderDomainManager) async throws {
        try await domainManager.removeDomainIfRegistered()
        do {
            try clearDeviceIdentity()
        } catch {
            NSLog("DaemonStore: failed to clear device identity during sign out: %@", error.localizedDescription)
        }
        if let restartDaemonOperation {
            restartDaemonOperation()
        } else {
            await restartDaemon()
        }
        hasSignedIn = false
        status = nil
        lastError = nil
        lastSuccessAt = nil
    }

    private func restartDaemon() async {
        await Task.detached(priority: .utility) {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
            process.arguments = ["kickstart", "-k", "gui/\(getuid())/dev.drnkn.valvd"]
            do {
                try process.run()
                process.waitUntilExit()
            } catch {
                NSLog("DaemonStore: failed to restart daemon after sign out: %@", error.localizedDescription)
            }
        }.value
    }
}
