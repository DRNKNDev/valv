//
//  ValvTests.swift
//  ValvTests
//
//  Created by Aji Kisworo Mukti on 03/07/26.
//

import Foundation
import Testing
@testable import Valv

struct ValvTests {

    @Test func signInURLIncludesPairingContext() throws {
        let url = SignInDevicePairing.loginURL(
            baseURL: URL(string: "https://valvsync.com/login")!,
            deviceName: "Aji's Mac",
            state: "state-123"
        )
        let components = try #require(URLComponents(url: url, resolvingAgainstBaseURL: false))
        let queryItems = components.queryItems ?? []

        #expect(queryItems.first(where: { $0.name == "device_flow" })?.value == "1")
        #expect(queryItems.first(where: { $0.name == "device_name" })?.value == "Aji's Mac")
        #expect(queryItems.first(where: { $0.name == "state" })?.value == "state-123")
        #expect(queryItems.first(where: { $0.name == "redirect_uri" }) == nil)
    }

    @Test func matchingCallbackWritesConfig() throws {
        try withTemporaryConfigDirectory { directory in
            let callbackURL = URL(string: "valv://auth-callback?device_id=device-1&device_token=token-1&state=state-123")!

            let credentials = try SignInDevicePairing.writeConfig(
                from: callbackURL,
                expectedState: "state-123",
                backendURL: "https://api.example.test",
                deviceName: "Test Mac",
                configDirectory: directory
            )

            let contents = try String(contentsOf: ConfigWriter.configPath(in: directory), encoding: .utf8)
            #expect(credentials == SignInDevicePairing.Credentials(deviceId: "device-1", deviceToken: "token-1"))
            #expect(contents.contains("backend_url = \"https://api.example.test\""))
            #expect(contents.contains("device_id = \"device-1\""))
            #expect(contents.contains("device_token = \"token-1\""))
            #expect(contents.contains("device_name = \"Test Mac\""))
        }
    }

    @Test func missingOrMismatchedStateDoesNotTouchConfig() throws {
        try withTemporaryConfigDirectory { directory in
            let configPath = ConfigWriter.configPath(in: directory)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let originalContents = "device_id = \"existing\"\n"
            try originalContents.write(to: configPath, atomically: true, encoding: .utf8)

            let missingStateURL = URL(string: "valv://auth-callback?device_id=device-1&device_token=token-1")!
            do {
                _ = try SignInDevicePairing.writeConfig(
                    from: missingStateURL,
                    expectedState: "state-123",
                    deviceName: "Test Mac",
                    configDirectory: directory
                )
                Issue.record("Expected missing state to be rejected")
            } catch SignInDevicePairing.CallbackError.missingOrMismatchedState {
            }
            #expect(try String(contentsOf: configPath, encoding: .utf8) == originalContents)

            let mismatchedStateURL = URL(string: "valv://auth-callback?device_id=device-1&device_token=token-1&state=wrong")!
            do {
                _ = try SignInDevicePairing.writeConfig(
                    from: mismatchedStateURL,
                    expectedState: "state-123",
                    deviceName: "Test Mac",
                    configDirectory: directory
                )
                Issue.record("Expected mismatched state to be rejected")
            } catch SignInDevicePairing.CallbackError.missingOrMismatchedState {
            }
            #expect(try String(contentsOf: configPath, encoding: .utf8) == originalContents)
        }
    }

    private func withTemporaryConfigDirectory(_ body: (URL) throws -> Void) throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ValvTests-\(UUID().uuidString)", isDirectory: true)
        defer {
            try? FileManager.default.removeItem(at: directory)
        }

        try body(directory)
    }

}
