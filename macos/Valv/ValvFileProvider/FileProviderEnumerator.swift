import DaemonKit
import FileProvider

final class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    private let identifier: NSFileProviderItemIdentifier
    private let client: DaemonClient

    init(enumeratedItemIdentifier: NSFileProviderItemIdentifier, client: DaemonClient) {
        self.identifier = enumeratedItemIdentifier
        self.client = client
        super.init()
    }

    func invalidate() {}

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage) {
        Task {
            do {
                guard identifier == .rootContainer else {
                    throw NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError)
                }
                try await enumerateRoot(observer: observer)
            } catch {
                observer.finishEnumeratingWithError(error as NSError)
            }
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        // The set of mounts changes only via explicit user action (Add Folder /
        // Remove from this Mac), each of which already calls
        // `signalEnumerator(for: .rootContainer)` itself - the root container has no
        // independent change-tracking anchor of its own, and no other identifier is
        // ever enumerated.
        completionHandler(NSFileProviderSyncAnchor(Data()))
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor) {
        observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
    }

    // MARK: - Root (synthetic multi-mount) enumeration

    private func enumerateRoot(observer: NSFileProviderEnumerationObserver) async throws {
        let mounts = try await client.mounts()
        let items = mounts.map { FileProviderItem(kind: .syntheticMount($0)) }
        observer.didEnumerate(items)
        observer.finishEnumerating(upTo: nil)
    }
}
