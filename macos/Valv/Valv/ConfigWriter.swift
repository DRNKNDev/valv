import Foundation

/// Writes `~/.config/valv/config.toml` in the exact shape `valvd`'s `config.rs::parse_config`
/// expects (`oss/crates/valvd/src/config.rs`). Only reachable now that `Valv` runs
/// unsandboxed (design.md D3-2) - the real config path, not a sandbox-container-redirected one.
enum ConfigWriter {
    // TODO: no hosted web login page exists yet to redirect back via valv://auth-callback
    // (design.md's own open question - this touches private/apps/web, out of this
    // change's oss/ scope). Placeholder default matching the api.valv.dev convention
    // already used elsewhere (oss/crates/valvd/src/config.rs's own prompt default).
    static let defaultBackendURL = "https://api.valv.dev"
    static let loginURL = URL(string: "https://api.valv.dev/login")!

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
        configDirectory.appendingPathComponent("config.toml")
    }

    static func write(_ values: Values) throws {
        try FileManager.default.createDirectory(at: configDirectory, withIntermediateDirectories: true)
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
