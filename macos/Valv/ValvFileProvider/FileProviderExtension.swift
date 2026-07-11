import DaemonKit
import FileProvider

final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    private let client: DaemonClient

    required init(domain: NSFileProviderDomain) {
        self.client = DaemonClient()
        super.init()
    }

    func invalidate() {}

    // MARK: - Item lookup

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        let task = Task {
            do {
                let item = try await resolveItem(for: identifier)
                completionHandler(item, nil)
                progress.completedUnitCount = 1
            } catch {
                completionHandler(nil, error)
            }
        }
        progress.cancellationHandler = { task.cancel() }
        return progress
    }

    private func resolveItem(for identifier: NSFileProviderItemIdentifier) async throws -> NSFileProviderItem {
        guard identifier != .rootContainer else {
            return FileProviderItem(kind: .root)
        }
        guard case .mount(let folderId) = ValvItemIdentifier(identifier) else {
            throw NSError(domain: NSCocoaErrorDomain, code: NSFileNoSuchFileError)
        }
        let mounts = try await client.mounts()
        guard let mount = mounts.first(where: { $0.folderId == folderId }) else {
            throw NSError(domain: NSCocoaErrorDomain, code: NSFileNoSuchFileError)
        }
        return FileProviderItem(kind: .syntheticMount(mount))
    }

    // MARK: - Content fetch

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version requestedVersion: NSFileProviderItemVersion?,
        request: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        completionHandler(nil, nil, Self.rejectionError("Valv items are links to a local folder and have no content of their own."))
        return progress
    }

    // MARK: - Create / modify / delete

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields: NSFileProviderItemFields,
        contents url: URL?,
        options: NSFileProviderCreateItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        completionHandler(nil, [], false, Self.rejectionError("Valv doesn't support adding items here. Use the menu bar to add a folder."))
        return progress
    }

    func modifyItem(
        _ item: NSFileProviderItem,
        baseVersion version: NSFileProviderItemVersion,
        changedFields: NSFileProviderItemFields,
        contents newContents: URL?,
        options: NSFileProviderModifyItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        completionHandler(nil, [], false, Self.rejectionError("Use Manage Folders to rename or remove a folder from Valv."))
        return progress
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion version: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        completionHandler(Self.rejectionError("Use Manage Folders to remove a folder from Valv."))
        return progress
    }

    private static func rejectionError(_ message: String) -> NSError {
        NSError(
            domain: NSCocoaErrorDomain,
            code: NSFeatureUnsupportedError,
            userInfo: [NSLocalizedDescriptionKey: message]
        )
    }

    // MARK: - Enumeration

    func enumerator(for containerItemIdentifier: NSFileProviderItemIdentifier, request: NSFileProviderRequest) throws -> NSFileProviderEnumerator {
        guard containerItemIdentifier == .rootContainer || containerItemIdentifier != .workingSet else {
            throw NSError(domain: NSCocoaErrorDomain, code: NSFileNoSuchFileError)
        }
        return FileProviderEnumerator(enumeratedItemIdentifier: containerItemIdentifier, client: client)
    }
}
