import Foundation

enum SignInDevicePairing {
    struct Credentials: Equatable {
        let deviceId: String
        let deviceToken: String
    }

    enum CallbackError: Error, Equatable {
        case missingOrMismatchedState
        case missingCredentials
    }

    static func loginURL(baseURL: URL = ConfigWriter.loginURL, deviceName: String, state: String) -> URL {
        var components = URLComponents(url: baseURL, resolvingAgainstBaseURL: false)!
        components.queryItems = [
            URLQueryItem(name: "device_flow", value: "1"),
            URLQueryItem(name: "device_name", value: deviceName),
            URLQueryItem(name: "state", value: state)
        ]
        return components.url!
    }

    static func credentials(from callbackURL: URL, expectedState: String?) throws -> Credentials {
        guard let components = URLComponents(url: callbackURL, resolvingAgainstBaseURL: false),
              let receivedState = components.queryItems?.first(where: { $0.name == "state" })?.value,
              let expectedState,
              receivedState == expectedState
        else {
            throw CallbackError.missingOrMismatchedState
        }

        guard let deviceId = components.queryItems?.first(where: { $0.name == "device_id" })?.value,
              let deviceToken = components.queryItems?.first(where: { $0.name == "device_token" })?.value,
              !deviceId.isEmpty, !deviceToken.isEmpty
        else {
            throw CallbackError.missingCredentials
        }

        return Credentials(deviceId: deviceId, deviceToken: deviceToken)
    }

    @discardableResult
    static func writeConfig(
        from callbackURL: URL,
        expectedState: String?,
        backendURL: String = ConfigWriter.defaultBackendURL,
        deviceName: String,
        configDirectory: URL = ConfigWriter.configDirectory
    ) throws -> Credentials {
        let credentials = try credentials(from: callbackURL, expectedState: expectedState)
        try ConfigWriter.write(ConfigWriter.Values(
            backendURL: backendURL,
            deviceId: credentials.deviceId,
            deviceToken: credentials.deviceToken,
            deviceName: deviceName
        ), configDirectory: configDirectory)
        return credentials
    }
}
