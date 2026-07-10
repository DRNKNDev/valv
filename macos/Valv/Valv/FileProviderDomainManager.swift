import Combine
import FileProvider
import Foundation

/// Owns the app's single `NSFileProviderDomain` registration (design.md D11 - one
/// domain for the whole account, not one per mount). Registered once, on first
/// successful sign-in (section 7); consulted afterward by both the onboarding flow and
/// the menu bar (section 6, to re-signal the synthetic root after a mount/unmount).
final class FileProviderDomainManager: ObservableObject {
    // Matches `DaemonStore.shared`'s convention - lets `AppDelegate` reach this
    // instance to auto-present onboarding at launch, without threading it through
    // a separate initializer path.
    static let shared = FileProviderDomainManager()

    @Published private(set) var domain: NSFileProviderDomain?
    @Published private(set) var registrationError: Error?

    private static let domainIdentifierDefaultsKey = "dev.drnkn.valv.fileProviderDomainIdentifier"
    private let userDefaults: UserDefaults
    private let addDomain: ((NSFileProviderDomain) async throws -> Void)?
    private let signalRootEnumeratorOperation: ((NSFileProviderDomain) async -> Void)?

    init(
        userDefaults: UserDefaults = .standard,
        addDomain: ((NSFileProviderDomain) async throws -> Void)? = nil,
        signalRootEnumerator: ((NSFileProviderDomain) async -> Void)? = nil
    ) {
        self.userDefaults = userDefaults
        self.addDomain = addDomain
        self.signalRootEnumeratorOperation = signalRootEnumerator
        if let existingId = userDefaults.string(forKey: Self.domainIdentifierDefaultsKey) {
            domain = NSFileProviderDomain(
                identifier: NSFileProviderDomainIdentifier(existingId),
                displayName: "Valv"
            )
        }
    }

    /// Registers or updates the expected domain. `NSFileProviderManager.add` is
    /// idempotent for an existing identifier, so every launch repairs missing system
    /// state without relying on the local UserDefaults cache.
    func registerDomainIfNeeded(accountId: String) async {
        registrationError = nil
        let expectedDomain = NSFileProviderDomain(
            identifier: NSFileProviderDomainIdentifier(accountId),
            displayName: "Valv"
        )
        do {
            if let addDomain {
                try await addDomain(expectedDomain)
            } else {
                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                    NSFileProviderManager.add(expectedDomain) { error in
                        if let error {
                            continuation.resume(throwing: error)
                        } else {
                            continuation.resume()
                        }
                    }
                }
            }
            domain = expectedDomain
            userDefaults.set(accountId, forKey: Self.domainIdentifierDefaultsKey)
            await signalRootEnumerator(for: expectedDomain)
        } catch {
            registrationError = error
            NSLog("FileProviderDomainManager: failed to register domain: %@", error.localizedDescription)
        }
    }

    func removeDomainIfRegistered() async throws {
        guard let domain else {
            userDefaults.removeObject(forKey: Self.domainIdentifierDefaultsKey)
            return
        }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            NSFileProviderManager.remove(domain) { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            }
        }
        self.domain = nil
        registrationError = nil
        userDefaults.removeObject(forKey: Self.domainIdentifierDefaultsKey)
    }

    /// Called after a successful `POST /mount` or an unmount, so the synthetic root's
    /// enumeration (one item per `GET /mounts` entry) picks up the change without
    /// waiting for the background watch loop's next cycle.
    func signalRootEnumerator() async {
        guard let domain else { return }
        await signalRootEnumerator(for: domain)
    }

    private func signalRootEnumerator(for domain: NSFileProviderDomain) async {
        if let signalRootEnumeratorOperation {
            await signalRootEnumeratorOperation(domain)
            return
        }
        guard let manager = NSFileProviderManager(for: domain) else { return }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            manager.signalEnumerator(for: .rootContainer) { _ in
                continuation.resume()
            }
        }
    }
}
