import Combine
import DaemonKit
import Foundation

/// Manages the relationship between `Valv.app` and whatever `valvd` LaunchAgent may or
/// may not already be registered on this Mac - bundling, stable-copy installation,
/// version-aware reconciliation, and CLI-on-PATH installation (macos-app spec,
/// "Valv bundles valvd and valv..." through "...without shadowing an existing install").
///
/// Deliberately runs unsandboxed (see design.md D3-2 and `Valv.entitlements`) - every
/// path this type touches (`~/Library/LaunchAgents/`, `~/Library/Application
/// Support/Valv/`, `/usr/local/bin/`) is outside what App Sandbox permits without an
/// entitlement, and there is no Apple policy requiring the *host app* (as opposed to
/// its two File Provider extensions) to be sandboxed.
// No explicit @MainActor - see DaemonStore.swift's comment on the same pattern.
final class DaemonManager: ObservableObject {
    // Matches `DaemonStore.shared`'s convention - lets `AppDelegate` reach this
    // instance to auto-present onboarding at launch, without threading it through
    // a separate initializer path.
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
    // valvd 0.1.0 is the first distributable version after the daemon shipped the
    // routes/transports this app requires: /fp/share and /fp/watch in c60f7ba, plus
    // TCP-loopback support for sandboxed macOS clients in fdc38e5. Version reporting
    // itself was wired in 4de6ea9 when the crate version moved to 0.1.0.
    private static let minimumRequiredVersion = "0.1.0"

    @Published private(set) var isManagedByValv = false
    @Published private(set) var cliInstallStatus: CLIInstallStatus = .notChecked
    @Published var pendingDecision: ReconciliationOutcome?
    @Published var installError: String?
    /// Set once `reconcileOnLaunch()` has run to completion (success, clean install, or
    /// a pending decision) - lets the onboarding daemon-setup page distinguish "still
    /// checking" from "checked, nothing needs the user" without inferring it from
    /// unrelated state.
    @Published private(set) var hasReconciled = false

    private let fileManager: FileManager
    private let homeDirectory: URL
    private let bundledBinDirectoryOverride: URL?
    private let cleanInstallOperation: ((Bool) async throws -> Void)?
    private let client: DaemonClient

    init(
        client: DaemonClient = DaemonClient(),
        fileManager: FileManager = .default,
        homeDirectory: URL? = nil,
        bundledBinDirectory: URL? = nil,
        startOnLaunch: Bool = true,
        cleanInstallOperation: ((Bool) async throws -> Void)? = nil
    ) {
        self.client = client
        self.fileManager = fileManager
        self.homeDirectory = homeDirectory ?? fileManager.homeDirectoryForCurrentUser
        self.bundledBinDirectoryOverride = bundledBinDirectory
        self.cleanInstallOperation = cleanInstallOperation
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

    // MARK: - Reconciliation (task 9's core flow)

    /// Runs on launch, before any installation action. Mirrors the `macos-app` spec's
    /// "An existing daemon registration is reconciled by version, never silently
    /// overwritten" requirement.
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
            // The plist is registered but the binary it points at is gone (e.g. the
            // whole stable install directory was removed out-of-band while the
            // launchd job itself was left behind). There's no real external install
            // to reconcile against here, so treat it like a fresh install rather
            // than asking the user to decide about a phantom "incompatible" one -
            // `resolveVersion` below can't distinguish "genuinely incompatible" from
            // "nothing there to run" and would otherwise show the wrong decision UI.
            await performCleanInstall(overwriteExisting: true)
            return
        }

        let existingVersion = await resolveVersion(ofBinaryAt: registeredPath)
        if isVersionCompatible(existingVersion) {
            isManagedByValv = registeredPath.hasPrefix(stableBinDirectory.path)
            if isManagedByValv {
                if !fileManager.fileExists(atPath: configFileURL.path) {
                    // The launchd job is registered but config.toml is gone (e.g.
                    // removed out-of-band while the job itself was left in place) -
                    // valvd exits immediately on every KeepAlive restart without it,
                    // crash-looping forever with no way for this app to notice unless
                    // it checks. Repair by reinstalling, which recreates the template.
                    await performCleanInstall(overwriteExisting: true)
                    return
                }
                await refreshStableCopyIfBundledIsNewer()
            }
            return
        }

        pendingDecision = .incompatibleNeedsDecision(existingPath: registeredPath, existingVersion: existingVersion ?? "unknown")
    }

    /// Called when the user picks "Let Valv.app manage it" in response to
    /// `pendingDecision`.
    func consentToTakeover() async {
        pendingDecision = nil
        await performCleanInstall(overwriteExisting: true)
    }

    /// Called when the user picks "I'll update it myself".
    func declineTakeover() {
        pendingDecision = nil
        isManagedByValv = false
    }

    /// "On each launch, if the app's bundled binaries are newer than the stable copy,
    /// Valv SHALL re-copy them and, if it owns the current daemon registration,
    /// restart the daemon to pick them up" (macos-app spec, "App update refreshes the
    /// stable copy"). Only called when `isManagedByValv` is already true - an
    /// externally-managed daemon's stable copy (if any) is never touched here.
    private func refreshStableCopyIfBundledIsNewer() async {
        let bundledValvd = bundledBinDirectory.appendingPathComponent("valvd").path
        let stableValvd = stableBinDirectory.appendingPathComponent("valvd").path
        guard let bundledVersion = try? await runCapturingOutput(executable: bundledValvd, arguments: ["--version"]),
              let stableVersion = try? await runCapturingOutput(executable: stableValvd, arguments: ["--version"]),
              bundledVersion != stableVersion
        else {
            return
        }
        do {
            try copyBundledBinariesToStableLocation()
            try await run(executable: "/bin/launchctl", arguments: ["kickstart", "-k", "gui/\(getuid())/\(Self.launchAgentLabel)"])
        } catch {
            NSLog("DaemonManager: stable copy refresh failed: %@", error.localizedDescription)
        }
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
        // clap's default `--version` output is "<bin-name> <version>" (e.g.
        // "valvd 0.1.0"), not a bare semver - take the last whitespace-separated
        // token so `parseVersion` gets "0.1.0", not the whole string.
        guard let output = try? await runCapturingOutput(executable: path, arguments: ["--version"]) else {
            return nil
        }
        return output.split(separator: " ").last.map(String.init)
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

    // MARK: - CLI install (task 9's PATH action)

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
            // /usr/local/bin often isn't user-writable - fall back to an
            // authorization-prompting shell command rather than failing silently.
            try? await runWithAdministratorPrivileges(command: "cp '\(source)' '\(destination)'")
        }
        checkCLIInstallStatus()
    }

    // MARK: - Process helpers

    private func run(executable: String, arguments: [String]) async throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        // Without this, Process leaves stdin unset and the child inherits ours -
        // under Xcode that's a live PTY that never delivers EOF, so any prompt the
        // child reads from stdin (e.g. valvd's `ensure_config_template`) blocks
        // forever instead of hitting a closed/empty stream and moving on.
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
