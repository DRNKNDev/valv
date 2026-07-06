import Foundation

/// Writes `~/.config/valv/config.toml` in the exact shape `valvd`'s `config.rs::parse_config`
/// expects (`oss/crates/valvd/src/config.rs`). Only reachable now that `Valv` runs
/// unsandboxed (design.md D3-2) - the real config path, not a sandbox-container-redirected one.
enum ConfigWriter {
    static let defaultBackendURL = "https://api.valvsync.com"
    // Base web login URL; onboarding adds device-pairing query parameters per sign-in attempt.
    static let loginURL = URL(string: "https://valvsync.com/login")!

    struct Values {
        let backendURL: String
        let deviceId: String
        let deviceToken: String
        let deviceName: String
    }

    static var configDirectory: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".config/valv")
    }

    static var configPath: URL {
        configPath(in: configDirectory)
    }

    static func configPath(in configDirectory: URL) -> URL {
        configDirectory.appendingPathComponent("config.toml")
    }

    static func write(_ values: Values, configDirectory: URL = Self.configDirectory) throws {
        try FileManager.default.createDirectory(at: configDirectory, withIntermediateDirectories: true)
        let configPath = configPath(in: configDirectory)
        let contents = """
        backend_url = "\(escape(values.backendURL))"
        device_id = "\(escape(values.deviceId))"
        device_token = "\(escape(values.deviceToken))"
        device_name = "\(escape(values.deviceName))"

        """
        try contents.write(to: configPath, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: configPath.path)
    }

    static func exists() -> Bool {
        FileManager.default.fileExists(atPath: configPath.path)
    }

    private static func escape(_ value: String) -> String {
        value.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
    }
}
