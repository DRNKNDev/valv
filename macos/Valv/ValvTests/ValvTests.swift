
import Foundation
import Testing
import DaemonKit
@testable import Valv

struct ValvTests {
    @Test func signedInFlagRoundTripsThroughUserDefaults() throws {
        let suiteName = "dev.drnkn.valv.tests.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suiteName))
        defer {
            defaults.removePersistentDomain(forName: suiteName)
        }
        defaults.removePersistentDomain(forName: suiteName)

        let store = DaemonStore(userDefaults: defaults)
        #expect(store.hasSignedIn == false)
        #expect(store.iconState == .notSetUp)

        store.hasSignedIn = true

        let freshStore = DaemonStore(userDefaults: defaults)
        #expect(freshStore.hasSignedIn == true)
    }

    @Test func signOutClearsSignedInStateWhenConfigClearFails() async throws {
        let suiteName = "dev.drnkn.valv.tests.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suiteName))
        defer {
            defaults.removePersistentDomain(forName: suiteName)
        }
        defaults.removePersistentDomain(forName: suiteName)

        let domainDefaultsKey = "dev.drnkn.valv.fileProviderDomainIdentifier"
        let originalDomainIdentifier = UserDefaults.standard.string(forKey: domainDefaultsKey)
        UserDefaults.standard.removeObject(forKey: domainDefaultsKey)
        defer {
            if let originalDomainIdentifier {
                UserDefaults.standard.set(originalDomainIdentifier, forKey: domainDefaultsKey)
            } else {
                UserDefaults.standard.removeObject(forKey: domainDefaultsKey)
            }
        }

        var didAttemptRestart = false
        let store = DaemonStore(
            userDefaults: defaults,
            clearDeviceIdentity: {
                throw NSError(
                    domain: "ValvTests",
                    code: 2,
                    userInfo: [NSLocalizedDescriptionKey: "Injected config clear failure"]
                )
            },
            restartDaemonOperation: {
                didAttemptRestart = true
            }
        )
        store.hasSignedIn = true

        try await store.signOut(domainManager: FileProviderDomainManager())

        #expect(didAttemptRestart)
        #expect(store.hasSignedIn == false)
        #expect(defaults.bool(forKey: "dev.drnkn.valv.hasSignedIn") == false)
    }

    @Test func deviceGrantBodyOmitsNilScopeAndPreservesSubfolderScope() throws {
        let encoder = JSONEncoder()

        let wholeFolderData = try encoder.encode(DeviceGrantRequestBody(
            scope_node_id: nil,
            name: "Laptop",
            can_read: true,
            can_write: false
        ))
        let wholeFolderObject = try #require(JSONSerialization.jsonObject(with: wholeFolderData) as? [String: Any])
        #expect(wholeFolderObject["scope_node_id"] == nil)
        #expect(wholeFolderObject["name"] as? String == "Laptop")
        #expect(wholeFolderObject["can_read"] as? Bool == true)
        #expect(wholeFolderObject["can_write"] as? Bool == false)

        let subfolderData = try encoder.encode(DeviceGrantRequestBody(
            scope_node_id: "node-subfolder",
            name: "Laptop",
            can_read: true,
            can_write: true
        ))
        let subfolderObject = try #require(JSONSerialization.jsonObject(with: subfolderData) as? [String: Any])
        #expect(subfolderObject["scope_node_id"] as? String == "node-subfolder")
    }

    @Test func daemonVersionComparatorUsesNumericSemverOrder() {
        #expect(DaemonManager.isVersion("0.10.0", atLeast: "0.9.0"))
        #expect(!DaemonManager.isVersion("0.8.9", atLeast: "0.9.0"))
        #expect(!DaemonManager.isVersion("not-a-version", atLeast: "0.9.0"))
    }

    @Test func updateRequiredSummaryOverridesTextWithoutChangingIconState() throws {
        let decoder = JSONDecoder()
        let status = try decoder.decode(DaemonStatus.self, from: Data("""
        {
          "paused": false,
          "backend_connected": true,
          "version": "0.1.0",
          "update_required": true,
          "mounts": [
            {
              "path": "/tmp/valv",
              "folder_id": "folder-1",
              "name": "Valv",
              "can_write": true,
              "syncing": true,
              "pending_ops": 0,
              "last_synced_at": null,
              "update_required": true
            }
          ]
        }
        """.utf8))

        #expect(MenuBarContentView.summaryText(status: status, iconState: .syncing, isDisconnected: false) == "Update Valv to keep syncing")
        #expect(MenuBarContentView.summaryText(status: withoutUpdateRequired(status), iconState: .syncing, isDisconnected: false) == "Syncing...")
        #expect(Set([IconState.notSetUp, .error, .paused, .syncing, .synced]).count == 5)
    }

    @Test func daemonOwnershipTextShowsUpdateAvailableIndicatorWithVersion() {
        let text = MenuBarContentView.daemonOwnershipText(
            version: "0.2.0",
            isManagedByValv: true,
            updateAvailable: true,
            latestVersion: "0.3.0"
        )
        #expect(text == "valvd 0.2.0 - managed by Valv - Update available (0.3.0)")
    }

    @Test func daemonOwnershipTextOmitsIndicatorWhenUpdateAvailableIsFalse() {
        let text = MenuBarContentView.daemonOwnershipText(
            version: "0.2.0",
            isManagedByValv: true,
            updateAvailable: false,
            latestVersion: "0.2.0"
        )
        #expect(text == "valvd 0.2.0 - managed by Valv")
    }

    @Test func daemonOwnershipTextOmitsIndicatorWhenFieldsAreAbsent() {
        let text = MenuBarContentView.daemonOwnershipText(
            version: "0.2.0",
            isManagedByValv: false,
            updateAvailable: nil,
            latestVersion: nil
        )
        #expect(text == "valvd 0.2.0 - managed externally")
    }

    @Test func cleanInstallFailureSetsInstallErrorAndReconcileStillSettles() async {
        let homeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ValvDaemonManagerTests-\(UUID().uuidString)", isDirectory: true)
        defer {
            try? FileManager.default.removeItem(at: homeDirectory)
        }

        let manager = DaemonManager(
            homeDirectory: homeDirectory,
            startOnLaunch: false,
            cleanInstallOperation: { _ in
                throw NSError(
                    domain: "ValvTests",
                    code: 1,
                    userInfo: [NSLocalizedDescriptionKey: "Injected install failure"]
                )
            }
        )

        await manager.reconcileOnLaunch()

        #expect(manager.installError == "Injected install failure")
        #expect(manager.hasReconciled)
        #expect(!manager.isManagedByValv)
    }

    @MainActor
    @Test func onboardingRetryAdvancesOnlyAfterRetriedInstallSucceeds() async {
        let homeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ValvDaemonRetryTests-\(UUID().uuidString)", isDirectory: true)
        defer {
            try? FileManager.default.removeItem(at: homeDirectory)
        }

        var installAttempts = 0
        var successfulRetryContinuation: CheckedContinuation<Void, Never>?
        let manager = DaemonManager(
            homeDirectory: homeDirectory,
            startOnLaunch: false,
            cleanInstallOperation: { _ in
                installAttempts += 1
                switch installAttempts {
                case 1:
                    throw NSError(
                        domain: "ValvTests",
                        code: 1,
                        userInfo: [NSLocalizedDescriptionKey: "Initial install failure"]
                    )
                case 2:
                    throw NSError(
                        domain: "ValvTests",
                        code: 2,
                        userInfo: [NSLocalizedDescriptionKey: "Retry install failure"]
                    )
                default:
                    await withCheckedContinuation { continuation in
                        successfulRetryContinuation = continuation
                    }
                }
            }
        )
        let coordinator = OnboardingCoordinator()
        coordinator.currentPage = .daemonSetup

        await manager.reconcileOnLaunch()
        #expect(manager.hasReconciled)
        #expect(manager.installError == "Initial install failure")

        await OnboardingDaemonInstallRetry.perform(daemonManager: manager, coordinator: coordinator)
        #expect(coordinator.currentPage == .daemonSetup)
        #expect(manager.installError == "Retry install failure")

        let retryTask = Task {
            await OnboardingDaemonInstallRetry.perform(daemonManager: manager, coordinator: coordinator)
        }
        while successfulRetryContinuation == nil {
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        #expect(coordinator.currentPage == .daemonSetup)
        #expect(manager.installError == nil)

        successfulRetryContinuation?.resume()
        await retryTask.value

        #expect(coordinator.currentPage == .signIn)
        #expect(manager.installError == nil)
        #expect(installAttempts == 3)
    }

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

    private func withoutUpdateRequired(_ status: DaemonStatus) throws -> DaemonStatus {
        let data = try JSONEncoder().encode(status)
        var object = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
        object["update_required"] = false
        var mounts = try #require(object["mounts"] as? [[String: Any]])
        for index in mounts.indices {
            mounts[index]["update_required"] = false
        }
        object["mounts"] = mounts
        return try JSONDecoder().decode(DaemonStatus.self, from: JSONSerialization.data(withJSONObject: object))
    }

}
