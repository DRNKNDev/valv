//
//  FileProviderExtension.swift
//  SpikeExtension
//
//  Created by Aji Kisworo Mukti on 26/06/26.
//

import FileProvider
import OSLog

final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    private static let logger = Logger(subsystem: "dev.drnkn.SpikeApp", category: "SpikeExtension")

    private let domain: NSFileProviderDomain
    private let manager: NSFileProviderManager?
    private let r2Client: R2Client

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        self.manager = NSFileProviderManager(for: domain)
        self.r2Client = R2Client(config: SpikeConfig.current)
        super.init()
        NSLog("FileProviderExtension initialized for domain %@", domain.identifier.rawValue)
        TraceLogger.log("extension init domain=\(domain.identifier.rawValue)")
    }

    func invalidate() {
        r2Client.cancelAll()
    }

    func item(for identifier: NSFileProviderItemIdentifier, request: NSFileProviderRequest, completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void) -> Progress {
        NSLog("FileProviderExtension item lookup for %@", identifier.rawValue)
        let progress = Progress(totalUnitCount: 1)

        if identifier == .rootContainer {
            completionHandler(FileProviderItem.root, nil)
            progress.completedUnitCount = 1
            return progress
        }

        let task = Task {
            do {
                guard let record = try await r2Client.item(forKey: identifier.rawValue) else {
                    completionHandler(nil, NSError(domain: NSCocoaErrorDomain, code: NSFileNoSuchFileError))
                    return
                }

                completionHandler(FileProviderItem(record: record), nil)
                progress.completedUnitCount = 1
            } catch {
                completionHandler(nil, error)
            }
        }

        progress.cancellationHandler = {
            task.cancel()
        }

        return progress
    }

    func fetchContents(for itemIdentifier: NSFileProviderItemIdentifier, version requestedVersion: NSFileProviderItemVersion?, request: NSFileProviderRequest, completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void) -> Progress {
        NSLog("FileProviderExtension fetchContents for %@", itemIdentifier.rawValue)
        TraceLogger.log("fetch start key=\(itemIdentifier.rawValue)")
        let progress = Progress(totalUnitCount: 100)

        let task = Task {
            do {
                let downloadURL = try await r2Client.presignedGetURL(for: itemIdentifier.rawValue)
                let (temporaryDownloadURL, response) = try await r2Client.session.download(from: downloadURL)
                try r2Client.validate(response: response, url: downloadURL, allowing404: false)

                let baseDirectory = try manager?.temporaryDirectoryURL() ?? FileManager.default.temporaryDirectory
                let localURL = baseDirectory
                    .appendingPathComponent(UUID().uuidString)
                    .appendingPathExtension(URL(fileURLWithPath: itemIdentifier.rawValue).pathExtension)

                if FileManager.default.fileExists(atPath: localURL.path) {
                    try FileManager.default.removeItem(at: localURL)
                }

                try FileManager.default.copyItem(at: temporaryDownloadURL, to: localURL)

                let item = try await r2Client.item(forKey: itemIdentifier.rawValue).map(FileProviderItem.init(record:)) ?? FileProviderItem(identifier: itemIdentifier)
                progress.completedUnitCount = 100
                TraceLogger.log("fetch success key=\(itemIdentifier.rawValue)")
                completionHandler(localURL, item, nil)
            } catch {
                NSLog("FileProviderExtension fetchContents failed for %@: %@", itemIdentifier.rawValue, error.localizedDescription)
                TraceLogger.log("fetch failed key=\(itemIdentifier.rawValue) error=\(error.localizedDescription)")
                completionHandler(nil, nil, error)
            }
        }

        progress.cancellationHandler = {
            task.cancel()
        }

        return progress
    }

    func createItem(basedOn itemTemplate: NSFileProviderItem, fields: NSFileProviderItemFields, contents url: URL?, options: NSFileProviderCreateItemOptions = [], request: NSFileProviderRequest, completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void) -> Progress {
        NSLog("FileProviderExtension createItem for %@", itemTemplate.filename)
        let progress = Progress(totalUnitCount: 100)
        let key = itemTemplate.filename

        let task = Task {
            do {
                if let url {
                    try await r2Client.upload(fileAt: url, to: key)
                }

                let createdItem = FileProviderItem(identifier: NSFileProviderItemIdentifier(key), filename: key)
                progress.completedUnitCount = 100
                completionHandler(createdItem, [], false, nil)
            } catch {
                NSLog("FileProviderExtension createItem failed for %@: %@", key, error.localizedDescription)
                completionHandler(nil, [], false, error)
            }
        }

        progress.cancellationHandler = {
            task.cancel()
        }

        return progress
    }

    func modifyItem(_ item: NSFileProviderItem, baseVersion version: NSFileProviderItemVersion, changedFields: NSFileProviderItemFields, contents newContents: URL?, options: NSFileProviderModifyItemOptions = [], request: NSFileProviderRequest, completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void) -> Progress {
        NSLog("FileProviderExtension modifyItem for %@", item.itemIdentifier.rawValue)
        let progress = Progress(totalUnitCount: 100)

        let task = Task {
            do {
                if changedFields.contains(.contents), let newContents {
                    try await r2Client.upload(fileAt: newContents, to: item.itemIdentifier.rawValue)
                }

                let updatedItem = FileProviderItem(identifier: item.itemIdentifier, filename: item.filename)
                progress.completedUnitCount = 100
                completionHandler(updatedItem, [], false, nil)
            } catch {
                NSLog("FileProviderExtension modifyItem failed for %@: %@", item.itemIdentifier.rawValue, error.localizedDescription)
                completionHandler(nil, [], false, error)
            }
        }

        progress.cancellationHandler = {
            task.cancel()
        }

        return progress
    }

    func deleteItem(identifier: NSFileProviderItemIdentifier, baseVersion version: NSFileProviderItemVersion, options: NSFileProviderDeleteItemOptions = [], request: NSFileProviderRequest, completionHandler: @escaping (Error?) -> Void) -> Progress {
        NSLog("FileProviderExtension deleteItem for %@", identifier.rawValue)
        let progress = Progress(totalUnitCount: 100)

        let task = Task {
            do {
                try await r2Client.delete(key: identifier.rawValue)
                progress.completedUnitCount = 100
                completionHandler(nil)
            } catch {
                NSLog("FileProviderExtension deleteItem failed for %@: %@", identifier.rawValue, error.localizedDescription)
                completionHandler(error)
            }
        }

        progress.cancellationHandler = {
            task.cancel()
        }

        return progress
    }

    func enumerator(for containerItemIdentifier: NSFileProviderItemIdentifier, request: NSFileProviderRequest) throws -> NSFileProviderEnumerator {
        guard containerItemIdentifier == .rootContainer || containerItemIdentifier == .workingSet else {
            throw NSError(domain: NSCocoaErrorDomain, code: NSFileNoSuchFileError)
        }

        return FileProviderEnumerator(enumeratedItemIdentifier: containerItemIdentifier, r2Client: r2Client)
    }
}
