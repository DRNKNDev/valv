import Cocoa
import DaemonKit
import FinderSync

/// `NSExtensionPrincipalClass` (`ValvFinderSync/Info.plist`) names this type
/// `$(PRODUCT_MODULE_NAME).FinderSync` - keeping the class named `FinderSync` rather
/// than `ValvFinderSync` matches that already-set, founder-owned plist value.
///
/// `FinderSync.framework`'s `FIFinderSync` is a concrete `NSObject` subclass that
/// itself conforms to a same-named `@objc` protocol declaring every override point
/// below as `@optional` - subclassing it directly (not "NSObject, FIFinderSync")
/// is required, since Swift cannot re-declare conformance to a protocol that shares
/// its name with the class being inherited from.
final class FinderSync: FIFinderSync {
    private let client = DaemonClient()
    private var pollTask: Task<Void, Never>?

    override init() {
        super.init()
        NSLog("ValvFinderSync launched from %@", Bundle.main.bundlePath as NSString)
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refreshDirectoryURLs()
                try? await Task.sleep(nanoseconds: 5_000_000_000)
            }
        }
    }

    deinit {
        pollTask?.cancel()
    }

    // MARK: - Watched directories

    private func refreshDirectoryURLs() async {
        do {
            let mounts = try await client.mounts()
            let urls = Set(mounts.map { URL(fileURLWithPath: $0.path) })
            await MainActor.run {
                FIFinderSyncController.default().directoryURLs = urls
            }
        } catch {
            NSLog("ValvFinderSync: GET /mounts failed, keeping last-known directoryURLs: %@", error.localizedDescription)
        }
    }

    // MARK: - Menu and toolbar item support

    override func menu(for menuKind: FIMenuKind) -> NSMenu? {
        guard menuKind == .contextualMenuForItems,
              let targetURL = FIFinderSyncController.default().targetedURL(),
              isInsideWatchedDirectory(targetURL) else {
            return nil
        }
        let menu = NSMenu(title: "")
        menu.addItem(withTitle: "Share with Valv...", action: #selector(shareTapped(_:)), keyEquivalent: "")
        return menu
    }

    private func isInsideWatchedDirectory(_ url: URL) -> Bool {
        guard let directoryURLs = FIFinderSyncController.default().directoryURLs else {
            return false
        }
        let path = url.standardizedFileURL.path
        return directoryURLs.contains { watched in
            let watchedPath = watched.standardizedFileURL.path
            return path == watchedPath || path.hasPrefix(watchedPath + "/")
        }
    }

    @objc private func shareTapped(_ sender: AnyObject?) {
        guard let targetURL = FIFinderSyncController.default().targetedURL() else {
            return
        }
        let path = targetURL.path
        Task { await openShareHandoff(path: path) }
    }

    private func openShareHandoff(path: String) async {
        var components = URLComponents()
        components.scheme = "valv"
        components.host = "share"
        var queryItems = [URLQueryItem(name: "path", value: path)]
        if let response = try? await client.nodeIdForPath(path) {
            queryItems.append(URLQueryItem(name: "node", value: response.nodeId))
        }
        components.queryItems = queryItems
        guard let url = components.url else {
            return
        }
        NSWorkspace.shared.open(url)
    }
}
