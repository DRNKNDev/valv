import Combine
import DaemonKit
import Foundation

final class DaemonManager: ObservableObject {
    static let shared = DaemonManager()

    enum CLIInstallStatus {
        case notChecked
        case notInstalled
        case installedByValv
        case installedElsewhere(path: String)

        var actionTitle: String {
            switch self {
            case .notChecked, .notInstalled: return "Install Command-Line Tool…"
            case .installedByValv: return "✓ Installed"
            case .installedElsewhere(let path): return "Already installed at \(path)"
            }
        }

        var isActionable: Bool {
            switch self {
            case .notChecked, .notInstalled: return true
            case .installedByValv, .installedElsewhere: return false
            }
        }
    }

    enum ReconciliationOutcome {
        case cleanInstall
        case compatibleExisting
        case incompatibleNeedsDecision(existingPath: String, existingVersion: String)
    }

    private static let launchAgentLabel = "dev.drnkn.valvd"
    private static let minimumRequiredVersion = "0.1.0"

    @Published private(set) var isManagedByValv = false
    @Published private(set) var cliInstallStatus: CLIInstallStatus = .notChecked
    @Published var pendingDecision: ReconciliationOutcome?
    @Published var installError: String?
    @Published private(set) var hasReconciled = false
    @Published private(set) var isRestartingDaemon = false

    static let defaultKickstartTimeoutNanoseconds: UInt64 = 5_000_000_000

    private let fileManager: FileManager
    private let homeDirectory: URL
    private let bundledBinDirectoryOverride: URL?
    private let cleanInstallOperation: ((Bool) async throws -> Void)?
    private let kickstartOperation: (() async throws -> Void)?
    private let runningDaemonVersionProvider: (() async -> String?)?
    private let kickstartTimeoutNanoseconds: UInt64
    private let client: DaemonClient

    init(
        client: DaemonClient = DaemonClient(),
        fileManager: FileManager = .default,
        homeDirectory: URL? = nil,
        bundledBinDirectory: URL? = nil,
        startOnLaunch: Bool = true,
        cleanInstallOperation: ((Bool) async throws -> Void)? = nil,
        kickstartOperation: (() async throws -> Void)? = nil,
        runningDaemonVersionProvider: (() async -> String?)? = nil,
        kickstartTimeoutNanoseconds: UInt64 = DaemonManager.defaultKickstartTimeoutNanoseconds
    ) {
        self.client = client
        self.fileManager = fileManager
        self.homeDirectory = homeDirectory ?? fileManager.homeDirectoryForCurrentUser
        self.bundledBinDirectoryOverride = bundledBinDirectory
        self.cleanInstallOperation = cleanInstallOperation
        self.kickstartOperation = kickstartOperation
        self.runningDaemonVersionProvider = runningDaemonVersionProvider
        self.kickstartTimeoutNanoseconds = kickstartTimeoutNanoseconds
        if startOnLaunch {
            Task { [weak self] in
                await self?.reconcileOnLaunch()
                self?.checkCLIInstallStatus()
            }
        }
    }

    private var homeURL: URL {
        homeDirectory
    }

    private var stableBinDirectory: URL {
        homeURL.appendingPathComponent("Library/Application Support/Valv/bin")
    }

    private var launchAgentURL: URL {
        homeURL.appendingPathComponent("Library/LaunchAgents/\(Self.launchAgentLabel).plist")
    }

    private var configFileURL: URL {
        homeURL.appendingPathComponent(".config/valv/config.toml")
    }

    private var bundledBinDirectory: URL {
        bundledBinDirectoryOverride ?? Bundle.main.bundleURL.appendingPathComponent("Contents/Resources/bin")
    }


    func reconcileOnLaunch() async {
        defer { hasReconciled = true }

        guard fileManager.fileExists(atPath: launchAgentURL.path) else {
            await performCleanInstall()
            return
        }

        guard let registeredPath = readRegisteredBinaryPath() else {
            await performCleanInstall()
            return
        }

        guard fileManager.fileExists(atPath: registeredPath) else {
            await performCleanInstall(overwriteExisting: true)
            return
        }

        let existingVersion = await resolveVersion(ofBinaryAt: registeredPath)
        if isVersionCompatible(existingVersion) {
            isManagedByValv = registeredPath.hasPrefix(stableBinDirectory.path)
            if isManagedByValv {
                if !fileManager.fileExists(atPath: configFileURL.path) {
                    await performCleanInstall(overwriteExisting: true)
                    return
                }
                await refreshStableCopyIfBundledIsNewer()
                await reconcileRunningDaemonAgainstStableCopy()
            }
            return
        }

        pendingDecision = .incompatibleNeedsDecision(existingPath: registeredPath, existingVersion: existingVersion ?? "unknown")
    }

    func consentToTakeover() async {
        pendingDecision = nil
        await performCleanInstall(overwriteExisting: true)
    }

    func declineTakeover() {
        pendingDecision = nil
        isManagedByValv = false
    }

    private func refreshStableCopyIfBundledIsNewer() async {
        let bundledValvd = bundledBinDirectory.appendingPathComponent("valvd").path
        let stableValvd = stableBinDirectory.appendingPathComponent("valvd").path
        guard let bundledRaw = try? await runCapturingOutput(executable: bundledValvd, arguments: ["--version"]),
              let stableRaw = try? await runCapturingOutput(executable: stableValvd, arguments: ["--version"])
        else {
            return
        }
        // Strip clap's "valvd " prefix before semver comparison.
        let bundledVersion = Self.lastVersionToken(from: bundledRaw)
        let stableVersion = Self.lastVersionToken(from: stableRaw)
        guard Self.isVersion(bundledVersion, atLeast: stableVersion),
              !Self.isVersion(stableVersion, atLeast: bundledVersion)
        else {
            return
        }
        do {
            try copyBundledBinariesToStableLocation()
        } catch {
            NSLog("DaemonManager: stable copy refresh failed: %@", error.localizedDescription)
            return
        }
        await kickstartDaemonBounded()
    }

    private func reconcileRunningDaemonAgainstStableCopy() async {
        guard isManagedByValv else { return }
        guard let runningVersion = await currentRunningDaemonVersion() else { return }
        let stableValvd = stableBinDirectory.appendingPathComponent("valvd").path
        guard let stableRaw = try? await runCapturingOutput(executable: stableValvd, arguments: ["--version"]) else {
            return
        }
        let stableVersion = Self.lastVersionToken(from: stableRaw)
        guard runningVersion != stableVersion else { return }
        await kickstartDaemonBounded()
    }

    private func currentRunningDaemonVersion() async -> String? {
        if let runningDaemonVersionProvider {
            return await runningDaemonVersionProvider()
        }
        guard let status = try? await client.status() else { return nil }
        return status.version
    }

    // launchctl waitUntilExit is not cancellable; bound it with a detached timeout race.
    private func kickstartDaemonBounded() async {
        isRestartingDaemon = true

        let kickstartTask = Task { [weak self] in
            do {
                try await self?.performKickstart()
            } catch {
                NSLog("DaemonManager: kickstart failed: %@", error.localizedDescription)
            }
        }

        let timeoutNanoseconds = kickstartTimeoutNanoseconds
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            let once = KickstartResumeOnce()
            Task {
                _ = await kickstartTask.value
                if once.fireIfFirst() { continuation.resume() }
            }
            Task {
                try? await Task.sleep(nanoseconds: timeoutNanoseconds)
                if once.fireIfFirst() { continuation.resume() }
            }
        }

        isRestartingDaemon = false
    }

    private final class KickstartResumeOnce: @unchecked Sendable {
        private let lock = NSLock()
        private var hasFired = false

        func fireIfFirst() -> Bool {
            lock.lock()
            defer { lock.unlock() }
            if hasFired { return false }
            hasFired = true
            return true
        }
    }

    private func performKickstart() async throws {
        if let kickstartOperation {
            try await kickstartOperation()
            return
        }
        try await run(executable: "/bin/launchctl", arguments: ["kickstart", "-k", "gui/\(getuid())/\(Self.launchAgentLabel)"])
    }

    func performCleanInstall(overwriteExisting: Bool = false) async {
        do {
            if let cleanInstallOperation {
                try await cleanInstallOperation(overwriteExisting)
            } else {
                try copyBundledBinariesToStableLocation()
                try await runValvDaemonInstall()
            }
            installError = nil
            isManagedByValv = true
        } catch {
            installError = error.localizedDescription
            NSLog("DaemonManager: clean install failed: %@", error.localizedDescription)
        }
    }

    private func copyBundledBinariesToStableLocation() throws {
        try fileManager.createDirectory(at: stableBinDirectory, withIntermediateDirectories: true)
        for binary in ["valvd", "valv"] {
            let source = bundledBinDirectory.appendingPathComponent(binary)
            let destination = stableBinDirectory.appendingPathComponent(binary)
            if fileManager.fileExists(atPath: destination.path) {
                try fileManager.removeItem(at: destination)
            }
            try fileManager.copyItem(at: source, to: destination)
        }
    }

    private func runValvDaemonInstall() async throws {
        let valvPath = stableBinDirectory.appendingPathComponent("valv").path
        try await run(executable: valvPath, arguments: ["daemon", "install"])
    }

    private func readRegisteredBinaryPath() -> String? {
        guard let data = try? Data(contentsOf: launchAgentURL),
              let plist = try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
              let args = plist["ProgramArguments"] as? [String], let first = args.first
        else {
            return nil
        }
        return first
    }

    private func resolveVersion(ofBinaryAt path: String) async -> String? {
        if let status = try? await client.status() {
            return status.version
        }
        guard let output = try? await runCapturingOutput(executable: path, arguments: ["--version"]) else {
            return nil
        }
        return Self.lastVersionToken(from: output)
    }

    private static func lastVersionToken(from rawVersionOutput: String) -> String {
        rawVersionOutput.split(separator: " ").last.map(String.init) ?? rawVersionOutput
    }

    private func isVersionCompatible(_ version: String?) -> Bool {
        guard let version else { return false }
        return Self.isVersion(version, atLeast: Self.minimumRequiredVersion)
    }

    static func isVersion(_ version: String, atLeast minimumVersion: String) -> Bool {
        guard let versionComponents = parseVersion(version),
              let minimumComponents = parseVersion(minimumVersion)
        else {
            return false
        }
        for index in 0..<3 {
            if versionComponents[index] > minimumComponents[index] {
                return true
            }
            if versionComponents[index] < minimumComponents[index] {
                return false
            }
        }
        return true
    }

    private static func parseVersion(_ version: String) -> [Int]? {
        let parts = version.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 3 else { return nil }
        var components: [Int] = []
        for part in parts {
            guard let component = Int(part) else { return nil }
            components.append(component)
        }
        return components
    }


    func checkCLIInstallStatus() {
        let wellKnownPaths = ["/opt/homebrew/bin/valv", "/usr/local/bin/valv", "\(homeURL.path)/.local/bin/valv"]
        let stableValvPath = stableBinDirectory.appendingPathComponent("valv").path

        for path in wellKnownPaths where fileManager.fileExists(atPath: path) {
            if let resolved = try? fileManager.destinationOfSymbolicLink(atPath: path), resolved == stableValvPath {
                cliInstallStatus = .installedByValv
            } else if isValvOwnedRegularFile(at: path, matching: stableValvPath) {
                cliInstallStatus = .installedByValv
            } else {
                cliInstallStatus = .installedElsewhere(path: path)
            }
            return
        }
        cliInstallStatus = .notInstalled
    }

    private func isValvOwnedRegularFile(at path: String, matching stableValvPath: String) -> Bool {
        guard let a = try? Data(contentsOf: URL(fileURLWithPath: path)),
              let b = try? Data(contentsOf: URL(fileURLWithPath: stableValvPath))
        else {
            return false
        }
        return a == b
    }

    func installCLI() async {
        checkCLIInstallStatus()
        guard cliInstallStatus.isActionable else { return }

        let destination = "/usr/local/bin/valv"
        let source = stableBinDirectory.appendingPathComponent("valv").path

        do {
            try fileManager.createDirectory(atPath: "/usr/local/bin", withIntermediateDirectories: true)
            if fileManager.fileExists(atPath: destination) {
                try fileManager.removeItem(atPath: destination)
            }
            try fileManager.copyItem(atPath: source, toPath: destination)
        } catch {
            try? await runWithAdministratorPrivileges(command: "cp '\(source)' '\(destination)'")
        }
        checkCLIInstallStatus()
    }


    private func run(executable: String, arguments: [String]) async throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw NSError(domain: "DaemonManager", code: Int(process.terminationStatus))
        }
    }

    private func runCapturingOutput(executable: String, arguments: [String]) async throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        let pipe = Pipe()
        process.standardOutput = pipe
        try process.run()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }

    private func runWithAdministratorPrivileges(command: String) async throws {
        let escaped = command.replacingOccurrences(of: "\"", with: "\\\"")
        try await run(executable: "/usr/bin/osascript", arguments: [
            "-e", "do shell script \"\(escaped)\" with administrator privileges",
        ])
    }
}
