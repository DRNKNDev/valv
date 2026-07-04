import Foundation

/// Reads back the values `ConfigWriter` wrote to `~/.config/valv/config.toml`, for the
/// direct-to-backend calls the Manage Folders & Sharing window makes (`GET /grants`,
/// invites, device grants) - the same pattern `valv-cli` already uses (auth.rs reads
/// `device_token` from this same file), not proxied through `valvd`.
enum ConfigReader {
    struct Values {
        let backendURL: String
        let deviceToken: String
    }

    static func read() -> Values? {
        guard let contents = try? String(contentsOf: ConfigWriter.configPath, encoding: .utf8) else {
            return nil
        }
        var values: [String: String] = [:]
        for line in contents.split(separator: "\n") {
            let parts = line.split(separator: "=", maxSplits: 1)
            guard parts.count == 2 else { continue }
            let key = parts[0].trimmingCharacters(in: .whitespaces)
            var value = parts[1].trimmingCharacters(in: .whitespaces)
            if value.hasPrefix("\""), value.hasSuffix("\""), value.count >= 2 {
                value = String(value.dropFirst().dropLast())
                    .replacingOccurrences(of: "\\\"", with: "\"")
                    .replacingOccurrences(of: "\\\\", with: "\\")
            }
            values[key] = value
        }
        guard let backendURL = values["backend_url"], let deviceToken = values["device_token"] else {
            return nil
        }
        return Values(backendURL: backendURL, deviceToken: deviceToken)
    }
}
