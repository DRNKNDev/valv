import DaemonKit
import FileProvider

final class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    private static let pageSize = 200

    private let identifier: NSFileProviderItemIdentifier
    private let client: DaemonClient

    /// Resolved lazily (and cached for this enumerator's lifetime) since resolving a
    /// real, non-mount-root node's owning mount needs an async `GET /fp/item/:node_id`
    /// call - `enumerator(for:)` in `FileProviderExtension` is a synchronous, throwing
    /// factory method and cannot itself await that lookup.
    private var resolvedFolderId: String?

    init(enumeratedItemIdentifier: NSFileProviderItemIdentifier, client: DaemonClient) {
        self.identifier = enumeratedItemIdentifier
        self.client = client
        super.init()
    }

    func invalidate() {}

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage) {
        Task {
            do {
                if identifier == .rootContainer {
                    try await enumerateRoot(observer: observer)
                } else {
                    try await enumerateMountOrNode(observer: observer, page: page, suggestedPageSize: observer.suggestedPageSize)
                }
            } catch {
                observer.finishEnumeratingWithError(error as NSError)
            }
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        guard identifier != .rootContainer else {
            // The set of mounts changes only via explicit user action (Add Folder /
            // Remove from this Mac), each of which already calls
            // `signalEnumerator(for: .rootContainer)` itself (design.md D11) - the root
            // container has no independent change-tracking anchor of its own.
            completionHandler(NSFileProviderSyncAnchor(Data()))
            return
        }
        Task {
            do {
                let folderId = try await resolveFolderId()
                let anchor = try await client.fpAnchor(folderId: folderId)
                completionHandler(NSFileProviderSyncAnchor(Data("\(anchor.serverSeq)".utf8)))
            } catch {
                completionHandler(nil)
            }
        }
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor) {
        guard identifier != .rootContainer else {
            observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
            return
        }
        Task {
            do {
                let folderId = try await resolveFolderId()
                let sinceSeq = Self.seq(from: anchor.rawValue)
                // FpChangesResponse doesn't carry can_write (it's a mount-level flag,
                // not a per-change one) - fetched alongside via fpAnchor so updated
                // items get accurate capabilities without a client-side cache.
                let mountAnchor = try await client.fpAnchor(folderId: folderId)
                let changes = try await client.fpChanges(folderId: folderId, sinceSeq: sinceSeq)

                let deletedIdentifiers = changes.items
                    .filter(\.deleted)
                    .map { ValvItemIdentifier.node(nodeId: $0.nodeId).fileProviderIdentifier }
                let updatedItems = changes.items
                    .filter { !$0.deleted }
                    .map { FileProviderItem(kind: .node($0, mountCanWrite: mountAnchor.canWrite)) }

                if !deletedIdentifiers.isEmpty {
                    observer.didDeleteItems(withIdentifiers: deletedIdentifiers)
                }
                if !updatedItems.isEmpty {
                    observer.didUpdate(updatedItems)
                }

                let newAnchor = NSFileProviderSyncAnchor(Data("\(changes.currentSeq)".utf8))
                observer.finishEnumeratingChanges(upTo: newAnchor, moreComing: changes.moreComing)
            } catch {
                observer.finishEnumeratingWithError(error as NSError)
            }
        }
    }

    // MARK: - Root (synthetic multi-mount) enumeration

    private func enumerateRoot(observer: NSFileProviderEnumerationObserver) async throws {
        let mounts = try await client.mounts()
        let items = mounts.map { FileProviderItem(kind: .syntheticMount($0)) }
        observer.didEnumerate(items)
        observer.finishEnumerating(upTo: nil)
    }

    // MARK: - Mount-root / real-folder enumeration

    private func enumerateMountOrNode(
        observer: NSFileProviderEnumerationObserver,
        page: NSFileProviderPage,
        suggestedPageSize: Int?
    ) async throws {
        let folderId = try await resolveFolderId()
        let parent = parentQueryValue()
        let offset = Self.startIndex(from: page)
        let requestedPageSize = max(suggestedPageSize ?? Self.pageSize, 1)
        let effectivePageSize = min(requestedPageSize, Self.pageSize)

        let response = try await client.fpItems(folderId: folderId, parent: parent, offset: offset, limit: effectivePageSize)
        let items = response.items.map { FileProviderItem(kind: .node($0, mountCanWrite: response.canWrite)) }
        observer.didEnumerate(items)

        let nextOffset = offset + response.items.count
        let nextPage: NSFileProviderPage? = nextOffset < response.total
            ? NSFileProviderPage(rawValue: Data("\(nextOffset)".utf8))
            : nil
        observer.finishEnumerating(upTo: nextPage)
    }

    /// `GET /fp/items`'s `parent` query value: the literal `"root"` for a mount's own
    /// synthetic entry (its real tree root), or the real node_id for a folder further
    /// down the tree.
    private func parentQueryValue() -> String {
        switch ValvItemIdentifier(identifier) {
        case .mount:
            return "root"
        case .node(let nodeId):
            return nodeId
        }
    }

    private func resolveFolderId() async throws -> String {
        if let resolvedFolderId {
            return resolvedFolderId
        }
        let folderId: String
        switch ValvItemIdentifier(identifier) {
        case .mount(let mountFolderId):
            folderId = mountFolderId
        case .node(let nodeId):
            folderId = try await client.fpItem(nodeId: nodeId).folderId
        }
        resolvedFolderId = folderId
        return folderId
    }

    private static func startIndex(from page: NSFileProviderPage) -> Int {
        guard let rawPage = String(data: page.rawValue, encoding: .utf8), let offset = Int(rawPage) else {
            return 0
        }
        return offset
    }

    private static func seq(from anchorData: Data) -> Int {
        guard let text = String(data: anchorData, encoding: .utf8), let seq = Int(text) else {
            return 0
        }
        return seq
    }
}
