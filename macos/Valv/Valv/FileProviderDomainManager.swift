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

    private static let domainIdentifierDefaultsKey = "dev.drnkn.valv.fileProviderDomainIdentifier"

    init() {
        if let existingId = UserDefaults.standard.string(forKey: Self.domainIdentifierDefaultsKey) {
            domain = NSFileProviderDomain(
                identifier: NSFileProviderDomainIdentifier(existingId),
                displayName: "Valv"
            )
        }
    }

    /// Registers the one domain for this account, if not already registered. Safe to
    /// call more than once - a no-op after the first successful call for a given
    /// account identifier.
    func registerDomainIfNeeded(accountId: String) async {
        guard domain == nil else { return }
        let newDomain = NSFileProviderDomain(
            identifier: NSFileProviderDomainIdentifier(accountId),
            displayName: "Valv"
        )
        do {
            try await NSFileProviderManager.add(newDomain)
            domain = newDomain
            UserDefaults.standard.set(accountId, forKey: Self.domainIdentifierDefaultsKey)
            await signalRootEnumerator()
        } catch {
            NSLog("FileProviderDomainManager: failed to register domain: %@", error.localizedDescription)
        }
    }

    /// Called after a successful `POST /mount` or an unmount, so the synthetic root's
    /// enumeration (one item per `GET /mounts` entry) picks up the change without
    /// waiting for the background watch loop's next cycle.
    func signalRootEnumerator() async {
        guard let domain, let manager = NSFileProviderManager(for: domain) else { return }
        try? await manager.signalEnumerator(for: .rootContainer)
    }
}
