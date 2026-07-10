import DaemonKit
import FileProvider
import os.log

final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    private static let logger = Logger(subsystem: "dev.drnkn.valv.fileprovider", category: "FileProviderExtension")

    private let domain: NSFileProviderDomain
    private let manager: NSFileProviderManager?
    private let client: DaemonClient
    private var watchTask: Task<Void, Never>?

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        self.manager = NSFileProviderManager(for: domain)
        self.client = DaemonClient()
        super.init()
        watchTask = Task { [weak self] in
            await self?.runWatchLoop()
        }
    }

    func invalidate() {
        watchTask?.cancel()
        watchTask = nil
    }

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
        switch ValvItemIdentifier(identifier) {
        case .mount(let folderId):
            let mounts = try await client.mounts()
            guard let mount = mounts.first(where: { $0.folderId == folderId }) else {
                throw NSError(domain: NSCocoaErrorDomain, code: NSFileNoSuchFileError)
            }
            return FileProviderItem(kind: .syntheticMount(mount))
        case .node(let nodeId):
            let item = try await client.fpItem(nodeId: nodeId)
            let anchor = try await client.fpAnchor(folderId: item.folderId)
            return FileProviderItem(kind: .node(item, mountCanWrite: anchor.canWrite))
        }
    }

    // MARK: - Content fetch

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version requestedVersion: NSFileProviderItemVersion?,
        request: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 100)
        let task = Task {
            do {
                guard case .node(let nodeId) = ValvItemIdentifier(itemIdentifier) else {
                    throw NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError)
                }
                let content = try await client.fpContent(nodeId: nodeId)
                let destinationURL = try await downloadAndReassemble(content, progress: progress)
                let item = try await resolveItem(for: itemIdentifier)
                completionHandler(destinationURL, item, nil)
            } catch {
                Self.logger.error("fetchContents failed for \(itemIdentifier.rawValue, privacy: .public): \(error.localizedDescription, privacy: .public)")
                completionHandler(nil, nil, error)
            }
        }
        progress.cancellationHandler = { task.cancel() }
        return progress
    }

    private func downloadAndReassemble(_ content: FpContentResponse, progress: Progress) async throws -> URL {
        let baseDirectory = try manager?.temporaryDirectoryURL() ?? FileManager.default.temporaryDirectory
        let destinationURL = baseDirectory.appendingPathComponent(UUID().uuidString)
        FileManager.default.createFile(atPath: destinationURL.path, contents: nil)
        let handle = try FileHandle(forWritingTo: destinationURL)
        defer { try? handle.close() }

        let orderedChunks = content.chunks.sorted { $0.offset < $1.offset }
        let session = URLSession(configuration: .ephemeral)
        for chunk in orderedChunks {
            guard let url = URL(string: chunk.url) else {
                throw NSError(domain: NSCocoaErrorDomain, code: NSURLErrorBadURL)
            }
            let (chunkURL, response) = try await session.download(from: url)
            guard let httpResponse = response as? HTTPURLResponse, (200 ..< 300).contains(httpResponse.statusCode) else {
                throw NSError(domain: NSCocoaErrorDomain, code: NSFileReadUnknownError)
            }
            let chunkData = try Data(contentsOf: chunkURL)
            try handle.seek(toOffset: UInt64(chunk.offset))
            handle.write(chunkData)
        }
        progress.completedUnitCount = 100
        return destinationURL
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
        let progress = Progress(totalUnitCount: 100)
        // Defense in depth (task 4.10): the synthetic root's children are entirely
        // derived from GET /mounts - Finder never legitimately creates a new top-level
        // item there (new mounts come from the menu bar's Add Folder action instead).
        guard itemTemplate.parentItemIdentifier != .rootContainer else {
            completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
            return progress
        }

        let task = Task {
            do {
                guard case .node(let parentNodeId) = ValvItemIdentifier(itemTemplate.parentItemIdentifier) else {
                    throw NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError)
                }
                guard let url else {
                    throw NSError(domain: NSCocoaErrorDomain, code: NSFileReadUnknownError)
                }
                let queued = try await client.fpUpload(FpUploadRequest(
                    nodeId: nil,
                    parentId: parentNodeId,
                    name: itemTemplate.filename,
                    basedOnSeq: nil,
                    filePath: url.path
                ))
                // Optimistic: report success immediately with a placeholder item built
                // from the template plus the daemon-assigned node_id. The real,
                // server-confirmed item arrives via the next enumerateChanges/watch
                // cycle once the daemon's upload job actually commits (design.md;
                // ipc-fp-api's "POST /fp/upload is a path handoff" - HTTP 202, daemon
                // handles chunking/upload asynchronously).
                let parentItem = try await client.fpItem(nodeId: parentNodeId)
                let placeholder = FpItem(
                    nodeId: queued.nodeId,
                    parentId: parentNodeId,
                    folderId: parentItem.folderId,
                    name: itemTemplate.filename,
                    type: .file,
                    versionId: nil,
                    contentHash: nil,
                    sizeBytes: nil,
                    serverSeq: 0,
                    deleted: false
                )
                let anchor = try await client.fpAnchor(folderId: parentItem.folderId)
                let createdItem = FileProviderItem(kind: .node(placeholder, mountCanWrite: anchor.canWrite))
                progress.completedUnitCount = 100
                completionHandler(createdItem, [], false, nil)
            } catch {
                Self.logger.error("createItem failed for \(itemTemplate.filename, privacy: .public): \(error.localizedDescription, privacy: .public)")
                completionHandler(nil, [], false, error)
            }
        }
        progress.cancellationHandler = { task.cancel() }
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
        let progress = Progress(totalUnitCount: 100)
        // Defense in depth (task 4.10): rename/reparent/trash of a synthetic mount item
        // itself is never legitimate - independent of `capabilities` already excluding
        // these operations, checked again here at the boundary.
        guard case .node(let nodeId) = ValvItemIdentifier(item.itemIdentifier) else {
            completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
            return progress
        }

        let task = Task {
            do {
                let metadataChanged = changedFields.contains(.filename) || changedFields.contains(.parentItemIdentifier)
                let sourceItem = try await client.fpItem(nodeId: nodeId)
                var basedOnSeq = sourceItem.serverSeq
                var parentIdForUpload = sourceItem.parentId ?? nodeId

                if metadataChanged {
                    let newName = changedFields.contains(.filename) ? item.filename : nil
                    let newParentId: String?
                    if changedFields.contains(.parentItemIdentifier) {
                        let sourceFolderId = sourceItem.folderId
                        let destinationFolderId = try await folderId(for: item.parentItemIdentifier)
                        guard sourceFolderId == destinationFolderId else {
                            completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
                            return
                        }
                        guard case .node(let parentNodeId) = ValvItemIdentifier(item.parentItemIdentifier) else {
                            completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
                            return
                        }
                        newParentId = parentNodeId
                        parentIdForUpload = parentNodeId
                    } else {
                        newParentId = nil
                    }

                    let move = try await client.fpMove(
                        nodeId: nodeId,
                        basedOnSeq: basedOnSeq,
                        newName: newName,
                        newParentId: newParentId
                    )
                    basedOnSeq = move.serverSeq
                }

                if changedFields.contains(.contents), let newContents {
                    _ = try await client.fpUpload(FpUploadRequest(
                        nodeId: nodeId,
                        parentId: parentIdForUpload,
                        name: item.filename,
                        basedOnSeq: basedOnSeq,
                        filePath: newContents.path
                    ))
                } else if changedFields.contains(.contents) {
                    throw NSError(domain: NSCocoaErrorDomain, code: NSFileReadUnknownError)
                }

                let updatedItem = try await resolveItem(for: item.itemIdentifier)
                progress.completedUnitCount = 100
                completionHandler(updatedItem, [], false, nil)
            } catch DaemonClientError.httpStatus(409, let body) {
                switch daemonErrorCode(from: body) {
                case "superseded":
                    do {
                        let updatedItem = try await resolveItem(for: item.itemIdentifier)
                        progress.completedUnitCount = 100
                        completionHandler(updatedItem, [], false, nil)
                    } catch {
                        completionHandler(nil, [], false, error)
                    }
                case "name_collision":
                    completionHandler(
                        nil,
                        [],
                        false,
                        NSError(
                            domain: NSFileProviderErrorDomain,
                            code: NSFileProviderError.Code.filenameCollision.rawValue
                        )
                    )
                default:
                    completionHandler(nil, [], false, DaemonClientError.httpStatus(409, body))
                }
            } catch {
                Self.logger.error("modifyItem failed for \(item.itemIdentifier.rawValue, privacy: .public): \(error.localizedDescription, privacy: .public)")
                completionHandler(nil, [], false, error)
            }
        }
        progress.cancellationHandler = { task.cancel() }
        return progress
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion version: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 100)
        guard case .node(let nodeId) = ValvItemIdentifier(identifier) else {
            completionHandler(NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
            return progress
        }

        let task = Task {
            do {
                let item = try await client.fpItem(nodeId: nodeId)
                try await client.fpDelete(nodeId: nodeId, basedOnSeq: item.serverSeq)
                progress.completedUnitCount = 100
                completionHandler(nil)
            } catch DaemonClientError.httpStatus(409, _) {
                // Superseded by a concurrent change - the extension resyncs via the
                // next enumerateChanges pass rather than treating this as a hard
                // failure (ipc-fp-api's documented 409 semantics).
                completionHandler(nil)
            } catch {
                Self.logger.error("deleteItem failed for \(identifier.rawValue, privacy: .public): \(error.localizedDescription, privacy: .public)")
                completionHandler(error)
            }
        }
        progress.cancellationHandler = { task.cancel() }
        return progress
    }

    private func folderId(for identifier: NSFileProviderItemIdentifier) async throws -> String {
        switch ValvItemIdentifier(identifier) {
        case .mount(let folderId):
            return folderId
        case .node(let nodeId):
            return try await client.fpItem(nodeId: nodeId).folderId
        }
    }

    private struct DaemonJSONErrorBody: Decodable {
        let error: String?
    }

    private func daemonErrorCode(from body: String) -> String? {
        guard let data = body.data(using: .utf8) else {
            return nil
        }
        return try? JSONDecoder().decode(DaemonJSONErrorBody.self, from: data).error
    }

    // MARK: - Enumeration

    func enumerator(for containerItemIdentifier: NSFileProviderItemIdentifier, request: NSFileProviderRequest) throws -> NSFileProviderEnumerator {
        guard containerItemIdentifier == .rootContainer || containerItemIdentifier != .workingSet else {
            throw NSError(domain: NSCocoaErrorDomain, code: NSFileNoSuchFileError)
        }
        return FileProviderEnumerator(enumeratedItemIdentifier: containerItemIdentifier, client: client)
    }

    // MARK: - Background GET /fp/watch loop (task 4.12)

    /// One independent, continuously-running long-poll loop per currently-mounted
    /// folder (not a single batched loop across all mounts - a mount whose watch call
    /// resolves quickly must not sit blocked behind a slower mount's still-pending
    /// ~25s long-poll). Each loop calls `signalEnumerator(for:)` on the domain when
    /// that mount's cursor advances. `valvd` runs as a separate OS process and cannot
    /// call `NSFileProviderManager.signalEnumerator` itself (ipc-fp-api's `GET
    /// /fp/watch` requirement) - the extension is responsible for polling and relaying
    /// the signal. The mount list itself is re-checked periodically (rather than via a
    /// push notification) so per-mount loops start/stop as mounts are added/removed
    /// from the menu bar.
    private func runWatchLoop() async {
        var tasksByFolderId: [String: Task<Void, Never>] = [:]
        while !Task.isCancelled {
            do {
                let mounts = try await client.mounts()
                let currentFolderIds = Set(mounts.map(\.folderId))

                for (folderId, task) in tasksByFolderId where !currentFolderIds.contains(folderId) {
                    task.cancel()
                    tasksByFolderId.removeValue(forKey: folderId)
                }
                for mount in mounts where tasksByFolderId[mount.folderId] == nil {
                    let folderId = mount.folderId
                    tasksByFolderId[folderId] = Task { [weak self] in
                        await self?.watchLoop(folderId: folderId)
                    }
                }
            } catch {
                Self.logger.error("watch loop failed to list mounts: \(error.localizedDescription, privacy: .public)")
            }
            try? await Task.sleep(nanoseconds: 5_000_000_000)
        }
        for task in tasksByFolderId.values {
            task.cancel()
        }
    }

    private func watchLoop(folderId: String) async {
        var sinceSeq = 0
        while !Task.isCancelled {
            do {
                let response = try await client.fpWatch(folderId: folderId, sinceSeq: sinceSeq)
                if response.serverSeq != sinceSeq {
                    sinceSeq = response.serverSeq
                    let identifier = ValvItemIdentifier.mount(folderId: folderId).fileProviderIdentifier
                    try? await manager?.signalEnumerator(for: identifier)
                }
            } catch {
                Self.logger.error("watch failed for folder \(folderId, privacy: .public): \(error.localizedDescription, privacy: .public)")
                try? await Task.sleep(nanoseconds: 5_000_000_000)
            }
        }
    }
}
