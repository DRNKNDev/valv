
import Combine
import Foundation
import FileProvider
import Testing
import DaemonKit
@testable import Valv

private final class CallCounter {
    private(set) var count = 0
    func increment() { count += 1 }
}

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

    @MainActor
    @Test func domainRegistrationFailureCanRetryAndPersistSuccess() async throws {
        let suiteName = "dev.drnkn.valv.tests.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        defaults.removePersistentDomain(forName: suiteName)

        let accountId = "account-1"
        var attempt = 0
        var retryContinuation: CheckedContinuation<Void, Error>?
        let manager = FileProviderDomainManager(
            userDefaults: defaults,
            addDomain: { _ in
                attempt += 1
                if attempt == 1 {
                    throw NSError(domain: "ValvTests", code: 1)
                }
                try await withCheckedThrowingContinuation { continuation in
                    retryContinuation = continuation
                }
            },
            signalRootEnumerator: { _ in }
        )

        await manager.registerDomainIfNeeded(accountId: accountId)

        #expect(manager.registrationError != nil)
        #expect(manager.domain == nil)
        #expect(defaults.string(forKey: "dev.drnkn.valv.fileProviderDomainIdentifier") == nil)

        let retryTask = Task {
            await manager.registerDomainIfNeeded(accountId: accountId)
        }
        while retryContinuation == nil {
            await Task.yield()
        }

        #expect(manager.registrationError == nil)
        #expect(manager.domain == nil)
        #expect(defaults.string(forKey: "dev.drnkn.valv.fileProviderDomainIdentifier") == nil)

        retryContinuation?.resume()
        await retryTask.value

        #expect(manager.registrationError == nil)
        #expect(manager.domain?.identifier.rawValue == accountId)
        #expect(defaults.string(forKey: "dev.drnkn.valv.fileProviderDomainIdentifier") == accountId)
    }

    @MainActor
    @Test func domainRegistrationRepairsSystemStateEvenWhenCacheExists() async throws {
        let suiteName = "dev.drnkn.valv.tests.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        defaults.removePersistentDomain(forName: suiteName)

        let accountId = "account-1"
        defaults.set(accountId, forKey: "dev.drnkn.valv.fileProviderDomainIdentifier")
        var addCount = 0
        let manager = FileProviderDomainManager(
            userDefaults: defaults,
            addDomain: { _ in
                addCount += 1
            },
            signalRootEnumerator: { _ in }
        )

        await manager.registerDomainIfNeeded(accountId: accountId)

        #expect(addCount == 1)
        #expect(manager.registrationError == nil)
        #expect(manager.domain?.identifier.rawValue == accountId)
        #expect(defaults.string(forKey: "dev.drnkn.valv.fileProviderDomainIdentifier") == accountId)
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

    @Test func daemonVersionFloorRejectsDaemonsWithoutFileProviderRoutes() {
        let floor = DaemonManager.minimumRequiredVersion
        #expect(!DaemonManager.isVersion("0.1.0", atLeast: floor))
        #expect(!DaemonManager.isVersion("0.1.9", atLeast: floor))
        #expect(DaemonManager.isVersion("0.2.0", atLeast: floor))
        #expect(DaemonManager.isVersion("0.3.1", atLeast: floor))
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
        #expect(MenuBarContentView.summaryText(status: try withoutUpdateRequired(status), iconState: .syncing, isDisconnected: false) == "Syncing...")
        #expect(Set([IconState.notSetUp, .error, .paused, .syncing, .synced]).count == 5)
    }

    @Test func restartingDaemonCaptionTakesPrecedenceOverDisconnectedAndUpdateRequired() throws {
        let decoder = JSONDecoder()
        let updateRequiredStatus = try decoder.decode(DaemonStatus.self, from: Data("""
        {
          "paused": false,
          "backend_connected": true,
          "version": "0.1.0",
          "update_required": true,
          "mounts": []
        }
        """.utf8))

        #expect(MenuBarContentView.summaryText(
            status: nil,
            iconState: .notSetUp,
            isDisconnected: true,
            isRestartingDaemon: true
        ) == "Restarting sync service…")

        #expect(MenuBarContentView.summaryText(
            status: updateRequiredStatus,
            iconState: .syncing,
            isDisconnected: false,
            isRestartingDaemon: true
        ) == "Restarting sync service…")

        #expect(MenuBarContentView.summaryText(
            status: nil,
            iconState: .notSetUp,
            isDisconnected: true,
            isRestartingDaemon: false
        ) == UserFacingError.connectionFailureMessage)
    }

    @Test func daemonFooterTextIncludesVersionAndQuitBehavior() {
        #expect(MenuBarContentView.daemonFooterText(version: "0.2.0") == "valvd 0.2.0 · Syncs after quit")
    }

    @Test func daemonFooterTextIsAbsentWithoutVersion() {
        #expect(MenuBarContentView.daemonFooterText(version: nil) == nil)
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


    @Test func refreshStableCopyRecopiesWhenBundledIsLexicallySmallerButNewer() async throws {
        let fixture = try makeManagedDaemonFixture(bundledVersion: "0.10.0", stableVersion: "0.9.0")
        defer { fixture.cleanup() }

        await fixture.manager.reconcileOnLaunch()

        #expect(fixture.kickstartCounter.count == 1)
        #expect(try String(contentsOf: fixture.registeredValvdURL, encoding: .utf8)
            == String(contentsOf: fixture.bundledValvdURL, encoding: .utf8))
    }

    @Test func refreshStableCopyDoesNotRecopyWhenBundledIsLexicallyLargerButOlder() async throws {
        let fixture = try makeManagedDaemonFixture(bundledVersion: "0.9.0", stableVersion: "0.10.0")
        defer { fixture.cleanup() }
        let originalStableContents = try String(contentsOf: fixture.registeredValvdURL, encoding: .utf8)

        await fixture.manager.reconcileOnLaunch()

        #expect(fixture.kickstartCounter.count == 0)
        #expect(try String(contentsOf: fixture.registeredValvdURL, encoding: .utf8) == originalStableContents)
    }

    @Test func refreshStableCopyDoesNotRecopyWhenVersionsAreEqual() async throws {
        let fixture = try makeManagedDaemonFixture(bundledVersion: "0.9.0", stableVersion: "0.9.0")
        defer { fixture.cleanup() }
        let originalStableContents = try String(contentsOf: fixture.registeredValvdURL, encoding: .utf8)

        await fixture.manager.reconcileOnLaunch()

        #expect(fixture.kickstartCounter.count == 0)
        #expect(try String(contentsOf: fixture.registeredValvdURL, encoding: .utf8) == originalStableContents)
    }

    @Test func refreshStableCopyDoesNotRecopyWhenBundledVersionIsUnparseable() async throws {
        let fixture = try makeManagedDaemonFixture(bundledVersion: "not-a-version", stableVersion: "0.9.0")
        defer { fixture.cleanup() }
        let originalStableContents = try String(contentsOf: fixture.registeredValvdURL, encoding: .utf8)

        await fixture.manager.reconcileOnLaunch()

        #expect(fixture.kickstartCounter.count == 0)
        #expect(try String(contentsOf: fixture.registeredValvdURL, encoding: .utf8) == originalStableContents)
    }


    @Test func postLaunchReconciliationKickstartsWithoutRecopyOnRunningVersionMismatch() async throws {
        let fixture = try makeManagedDaemonFixture(
            bundledVersion: "0.9.0",
            stableVersion: "0.9.0",
            runningDaemonVersionProvider: { "0.5.0" }
        )
        defer { fixture.cleanup() }
        let originalStableContents = try String(contentsOf: fixture.registeredValvdURL, encoding: .utf8)

        await fixture.manager.reconcileOnLaunch()

        #expect(fixture.kickstartCounter.count == 1)
        #expect(try String(contentsOf: fixture.registeredValvdURL, encoding: .utf8) == originalStableContents)
    }

    @Test func postLaunchReconciliationDoesNothingForExternallyManagedDaemon() async throws {
        let fixture = try makeManagedDaemonFixture(
            bundledVersion: "0.9.0",
            stableVersion: "0.9.0",
            runningDaemonVersionProvider: { "0.5.0" },
            externallyManaged: true
        )
        defer { fixture.cleanup() }

        await fixture.manager.reconcileOnLaunch()

        #expect(!fixture.manager.isManagedByValv)
        #expect(fixture.kickstartCounter.count == 0)
    }

    @Test func kickstartBoundedTimeoutClearsFlagEvenWhenKickstartHangs() async throws {
        let slowKickstartNanoseconds: UInt64 = 5_000_000_000
        let boundedTimeoutNanoseconds: UInt64 = 50_000_000
        let fixture = try makeManagedDaemonFixture(
            bundledVersion: "0.10.0",
            stableVersion: "0.9.0",
            kickstartOperation: { try? await Task.sleep(nanoseconds: slowKickstartNanoseconds) },
            kickstartTimeoutNanoseconds: boundedTimeoutNanoseconds
        )
        defer { fixture.cleanup() }

        let start = DispatchTime.now()
        await fixture.manager.reconcileOnLaunch()
        let elapsedNanoseconds = DispatchTime.now().uptimeNanoseconds - start.uptimeNanoseconds

        #expect(elapsedNanoseconds < 1_000_000_000)
        #expect(!fixture.manager.isRestartingDaemon)
    }

    private struct ManagedDaemonFixture {
        let manager: DaemonManager
        let homeDirectory: URL
        let registeredValvdURL: URL
        let bundledValvdURL: URL
        let kickstartCounter: CallCounter

        func cleanup() {
            try? FileManager.default.removeItem(at: homeDirectory)
        }
    }

    private func makeManagedDaemonFixture(
        bundledVersion: String,
        stableVersion: String,
        runningDaemonVersionProvider: (() async -> String?)? = nil,
        externallyManaged: Bool = false,
        kickstartOperation: (() async throws -> Void)? = nil,
        kickstartTimeoutNanoseconds: UInt64 = DaemonManager.defaultKickstartTimeoutNanoseconds
    ) throws -> ManagedDaemonFixture {
        let homeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ValvDaemonManagerVersionTests-\(UUID().uuidString)", isDirectory: true)

        let bundledBinDirectory = homeDirectory.appendingPathComponent("bundled-bin", isDirectory: true)
        let stableBinDirectory = homeDirectory
            .appendingPathComponent("Library/Application Support/Valv/bin", isDirectory: true)
        let launchAgentURL = homeDirectory
            .appendingPathComponent("Library/LaunchAgents/dev.drnkn.valvd.plist")
        let configFileURL = homeDirectory.appendingPathComponent(".config/valv/config.toml")

        try FileManager.default.createDirectory(at: bundledBinDirectory, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: stableBinDirectory, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(
            at: launchAgentURL.deletingLastPathComponent(), withIntermediateDirectories: true
        )
        try FileManager.default.createDirectory(
            at: configFileURL.deletingLastPathComponent(), withIntermediateDirectories: true
        )
        try "".write(to: configFileURL, atomically: true, encoding: .utf8)

        let bundledValvdURL = bundledBinDirectory.appendingPathComponent("valvd")
        let bundledValvURL = bundledBinDirectory.appendingPathComponent("valv")
        try writeVersionScript(at: bundledValvdURL, version: bundledVersion)
        try writeVersionScript(at: bundledValvURL, version: bundledVersion)

        let registeredDirectory = externallyManaged
            ? homeDirectory.appendingPathComponent("external-bin", isDirectory: true)
            : stableBinDirectory
        if externallyManaged {
            try FileManager.default.createDirectory(at: registeredDirectory, withIntermediateDirectories: true)
        }
        let registeredValvdURL = registeredDirectory.appendingPathComponent("valvd")
        try writeVersionScript(at: registeredValvdURL, version: stableVersion)

        let plist: [String: Any] = ["ProgramArguments": [registeredValvdURL.path]]
        let plistData = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
        try plistData.write(to: launchAgentURL)

        let kickstartCounter = CallCounter()
        let manager = DaemonManager(
            homeDirectory: homeDirectory,
            bundledBinDirectory: bundledBinDirectory,
            startOnLaunch: false,
            kickstartOperation: {
                kickstartCounter.increment()
                try await kickstartOperation?()
            },
            runningDaemonVersionProvider: runningDaemonVersionProvider ?? { nil },
            kickstartTimeoutNanoseconds: kickstartTimeoutNanoseconds
        )

        return ManagedDaemonFixture(
            manager: manager,
            homeDirectory: homeDirectory,
            registeredValvdURL: registeredValvdURL,
            bundledValvdURL: bundledValvdURL,
            kickstartCounter: kickstartCounter
        )
    }

    private func writeVersionScript(at url: URL, version: String) throws {
        let script = "#!/bin/sh\necho \"valvd \(version)\"\n"
        try script.write(to: url, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
    }


    @MainActor
    @Test func updateManagerBadgeStateTransitionsFollowSparkleCallbacks() {
        let manager = UpdateManager(updateRequiredPublisher: Empty<Bool, Never>().eraseToAnyPublisher())

        #expect(!manager.isChecking)
        manager.handleCheckStarted()
        #expect(manager.isChecking)

        manager.handleValidUpdateFound(version: "0.3.0")
        #expect(manager.updateAvailable)
        #expect(!manager.isChecking)

        manager.handleNoUpdateFound()
        #expect(!manager.updateAvailable)
        #expect(!manager.isChecking)

        manager.handleValidUpdateFound(version: "0.4.0")
        #expect(manager.updateAvailable)
    }

    @MainActor
    @Test func updateRequiredTransitionTriggersImmediateCheckExactlyOncePerRise() {
        let triggerCounter = CallCounter()
        let manager = UpdateManager(
            updateRequiredPublisher: Empty<Bool, Never>().eraseToAnyPublisher(),
            immediateCheckOperation: { triggerCounter.increment() }
        )

        manager.handleUpdateRequiredTransition(to: true)
        #expect(triggerCounter.count == 1)

        manager.handleUpdateRequiredTransition(to: true)
        manager.handleUpdateRequiredTransition(to: true)
        #expect(triggerCounter.count == 1)

        manager.handleUpdateRequiredTransition(to: false)
        #expect(triggerCounter.count == 1)

        manager.handleUpdateRequiredTransition(to: true)
        #expect(triggerCounter.count == 2)
    }

    @MainActor
    @Test func updateRequiredPublisherWiringTriggersImmediateCheck() async {
        let triggerCounter = CallCounter()
        let subject = PassthroughSubject<Bool, Never>()
        let manager = UpdateManager(
            updateRequiredPublisher: subject.eraseToAnyPublisher(),
            immediateCheckOperation: { triggerCounter.increment() }
        )

        subject.send(false)
        #expect(triggerCounter.count == 0)

        subject.send(true)
        #expect(triggerCounter.count == 1)
        #expect(manager.updateRequired)

        subject.send(true)
        #expect(triggerCounter.count == 1)
        subject.send(false)
        subject.send(true)
        #expect(triggerCounter.count == 2)
    }

    @MainActor
    @Test func manualCheckForUpdatesFiresSeamAndFlipsIsChecking() {
        let manualCounter = CallCounter()
        let manager = UpdateManager(
            updateRequiredPublisher: Empty<Bool, Never>().eraseToAnyPublisher(),
            manualCheckOperation: { manualCounter.increment() }
        )

        #expect(!manager.isChecking)
        manager.checkForUpdates()
        #expect(manualCounter.count == 1)
        #expect(manager.isChecking)
    }

    @Test func updateBadgeShowsWhenEitherUpdateAvailableOrUpdateRequired() {
        #expect(!MenuBarContentView.showsUpdateBadge(updateAvailable: false, updateRequired: false))
        #expect(MenuBarContentView.showsUpdateBadge(updateAvailable: true, updateRequired: false))
        #expect(MenuBarContentView.showsUpdateBadge(updateAvailable: false, updateRequired: true))
        #expect(MenuBarContentView.showsUpdateBadge(updateAvailable: true, updateRequired: true))
    }


    @Test func sparkleUpdateErrorClassifiesOnlyTheExactSignatureErrorDomainAndCode() {
        #expect(SparkleUpdateError.isSignatureVerificationFailure(
            NSError(domain: "SUSparkleErrorDomain", code: 3001)
        ))

        #expect(!SparkleUpdateError.isSignatureVerificationFailure(
            NSError(domain: "SUSparkleErrorDomain", code: 3000)
        ))

        #expect(!SparkleUpdateError.isSignatureVerificationFailure(
            NSError(domain: "SUSparkleErrorDomain", code: 3002)
        ))

        #expect(!SparkleUpdateError.isSignatureVerificationFailure(
            NSError(domain: "NSURLErrorDomain", code: 3001)
        ))
    }

    @MainActor
    @Test func updateManagerReactsToSignatureVerificationAbortWithoutClearingUpdateAvailable() {
        let manager = UpdateManager(updateRequiredPublisher: Empty<Bool, Never>().eraseToAnyPublisher())
        manager.handleValidUpdateFound(version: "0.3.0")
        #expect(manager.updateAvailable)

        manager.handleAbort(error: NSError(domain: "SUSparkleErrorDomain", code: 3001))
        #expect(manager.verificationFailed)
        #expect(manager.updateAvailable)

        manager.handleAbort(error: NSError(domain: "NSURLErrorDomain", code: -1001))
        #expect(!manager.verificationFailed)
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

    @Test func configReaderIncludesDeviceIdForLaunchReconciliation() throws {
        try withTemporaryConfigDirectory { directory in
            try ConfigWriter.write(
                ConfigWriter.Values(
                    backendURL: "https://api.example.test",
                    deviceId: "device-1",
                    deviceToken: "token-1",
                    deviceName: "Test Mac"
                ),
                configDirectory: directory
            )

            let values = try #require(ConfigReader.read(configPath: ConfigWriter.configPath(in: directory)))
            #expect(values.backendURL == "https://api.example.test")
            #expect(values.deviceId == "device-1")
            #expect(values.deviceToken == "token-1")
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

    // `FpShareResponse` has no public memberwise initializer across the DaemonKit
    // module boundary (only `Decodable`'s `init(from:)` is public) - decode a small
    // JSON payload instead of constructing it directly.
    private func makeFpShareResponse(inviteUrl: String) throws -> FpShareResponse {
        try JSONDecoder().decode(FpShareResponse.self, from: Data("{\"invite_url\":\"\(inviteUrl)\"}".utf8))
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

    @MainActor
    @Test func shareWindowNodeParameterSkipsResolutionAndSubmitsDirectly() async throws {
        var resolveCallCount = 0
        let viewModel = ShareWindowViewModel(
            path: "/Users/alice/Design Docs/report.pdf",
            node: "n1",
            resolveNode: { _ in
                resolveCallCount += 1
                return "unused"
            },
            submitShare: { nodeId, email, canWrite in
                #expect(nodeId == "n1")
                #expect(email == "friend@example.com")
                #expect(canWrite == true)
                return try makeFpShareResponse(inviteUrl: "https://valvsync.com/invite/abc")
            }
        )

        await viewModel.resolveIfNeeded()
        #expect(resolveCallCount == 0)
        #expect(!viewModel.canSubmit)

        viewModel.email = "friend@example.com"
        #expect(viewModel.canSubmit)

        await viewModel.submit()
        #expect(viewModel.inviteURL == "https://valvsync.com/invite/abc")
    }

    @MainActor
    @Test func shareWindowMissingNodeResolvesBeforeEnablingSubmit() async throws {
        var resolveCallCount = 0
        let viewModel = ShareWindowViewModel(
            path: "/Users/alice/Design Docs/report.pdf",
            node: nil,
            resolveNode: { _ in
                resolveCallCount += 1
                return "n2"
            },
            submitShare: { _, _, _ in try makeFpShareResponse(inviteUrl: "unused") }
        )
        viewModel.email = "friend@example.com"
        #expect(!viewModel.canSubmit)

        await viewModel.resolveIfNeeded()

        #expect(resolveCallCount == 1)
        #expect(viewModel.canSubmit)
    }

    @MainActor
    @Test func shareWindowResolutionFailuresMapToDistinctCopyPerErrorCode() async throws {
        let notInMountViewModel = ShareWindowViewModel(
            path: "/tmp/outside.txt",
            node: nil,
            resolveNode: { _ in throw DaemonClientError.httpStatus(404, "{\"error\":\"not_in_mount\"}") },
            submitShare: { _, _, _ in throw DaemonClientError.malformedResponse }
        )
        await notInMountViewModel.resolveIfNeeded()
        guard case .failed(let notInMountMessage) = notInMountViewModel.resolution else {
            Issue.record("Expected resolution to fail")
            return
        }

        let notSyncedViewModel = ShareWindowViewModel(
            path: "/tmp/pending.txt",
            node: nil,
            resolveNode: { _ in throw DaemonClientError.httpStatus(404, "{\"error\":\"node_not_synced\"}") },
            submitShare: { _, _, _ in throw DaemonClientError.malformedResponse }
        )
        await notSyncedViewModel.resolveIfNeeded()
        guard case .failed(let notSyncedMessage) = notSyncedViewModel.resolution else {
            Issue.record("Expected resolution to fail")
            return
        }

        let unreachableViewModel = ShareWindowViewModel(
            path: "/tmp/file.txt",
            node: nil,
            resolveNode: { _ in throw DaemonClientError.connectionFailed("timed out") },
            submitShare: { _, _, _ in throw DaemonClientError.malformedResponse }
        )
        await unreachableViewModel.resolveIfNeeded()
        guard case .failed(let unreachableMessage) = unreachableViewModel.resolution else {
            Issue.record("Expected resolution to fail")
            return
        }

        #expect(notInMountMessage == "This file isn't inside a synced folder.")
        #expect(notSyncedMessage == "This file hasn't finished syncing yet.")
        #expect(unreachableMessage == UserFacingError.connectionFailureMessage)
        #expect(Set([notInMountMessage, notSyncedMessage, unreachableMessage]).count == 3)

        #expect(!notInMountViewModel.canSubmit)
    }

    @Test func shareURLQueryItemsRoundTripPathAndOptionalNode() throws {
        var components = URLComponents()
        components.scheme = "valv"
        components.host = "share"
        components.queryItems = [
            URLQueryItem(name: "path", value: "/Users/alice/Design Docs/report.pdf"),
            URLQueryItem(name: "node", value: "n1")
        ]
        let url = try #require(components.url)

        let parsed = try #require(URLComponents(url: url, resolvingAgainstBaseURL: false))
        #expect(parsed.queryItems?.first(where: { $0.name == "path" })?.value == "/Users/alice/Design Docs/report.pdf")
        #expect(parsed.queryItems?.first(where: { $0.name == "node" })?.value == "n1")
    }

    @Test func finderSyncEnablementMonitorReflectsInjectedProvider() {
        let monitorEnabled = FinderSyncEnablementMonitor(isExtensionEnabledProvider: { true })
        #expect(monitorEnabled.isEnabled)

        let monitorDisabled = FinderSyncEnablementMonitor(isExtensionEnabledProvider: { false })
        #expect(!monitorDisabled.isEnabled)
    }

    @Test func finderSyncEnablementMonitorRefreshesOnActivation() {
        var enabled = false
        let activationSubject = PassthroughSubject<Void, Never>()
        let monitor = FinderSyncEnablementMonitor(
            isExtensionEnabledProvider: { enabled },
            activationPublisher: activationSubject.eraseToAnyPublisher()
        )
        #expect(!monitor.isEnabled)

        enabled = true
        activationSubject.send(())
        #expect(monitor.isEnabled)
    }

}
