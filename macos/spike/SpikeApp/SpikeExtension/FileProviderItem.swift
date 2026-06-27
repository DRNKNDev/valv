//
//  FileProviderItem.swift
//  SpikeExtension
//
//  Created by Aji Kisworo Mukti on 26/06/26.
//

import FileProvider
import UniformTypeIdentifiers

final class FileProviderItem: NSObject, NSFileProviderItem {
    private let identifier: NSFileProviderItemIdentifier
    private let parentIdentifier: NSFileProviderItemIdentifier
    private let name: String
    private let type: UTType
    private let size: NSNumber?
    private let version: NSFileProviderItemVersion

    init(
        identifier: NSFileProviderItemIdentifier,
        parentItemIdentifier: NSFileProviderItemIdentifier = .rootContainer,
        filename: String? = nil,
        contentType: UTType? = nil,
        documentSize: NSNumber? = nil
    ) {
        self.identifier = identifier
        self.parentIdentifier = parentItemIdentifier
        self.name = filename ?? identifier.rawValue
        self.type = contentType ?? Self.inferContentType(from: filename ?? identifier.rawValue, identifier: identifier)
        self.size = documentSize
        self.version = Self.makeVersion(identifier: identifier, size: documentSize)
    }

    convenience init(record: R2ObjectRecord) {
        self.init(
            identifier: NSFileProviderItemIdentifier(record.key),
            filename: URL(fileURLWithPath: record.key).lastPathComponent,
            contentType: Self.inferContentType(from: record.key, identifier: NSFileProviderItemIdentifier(record.key), explicit: record.contentType),
            documentSize: record.size.map(NSNumber.init(value:))
        )
    }

    static var root: FileProviderItem {
        FileProviderItem(
            identifier: .rootContainer,
            parentItemIdentifier: .rootContainer,
            filename: "Valv Spike",
            contentType: .folder
        )
    }

    var itemIdentifier: NSFileProviderItemIdentifier { identifier }

    var parentItemIdentifier: NSFileProviderItemIdentifier { parentIdentifier }

    var filename: String { name }

    var contentType: UTType { type }

    var capabilities: NSFileProviderItemCapabilities {
        if identifier == .rootContainer || identifier == .workingSet {
            return [.allowsContentEnumerating, .allowsAddingSubItems]
        }

        return [.allowsReading, .allowsWriting, .allowsRenaming, .allowsReparenting, .allowsTrashing, .allowsDeleting]
    }

    var documentSize: NSNumber? { size }

    var itemVersion: NSFileProviderItemVersion { version }

    private static func inferContentType(
        from filename: String,
        identifier: NSFileProviderItemIdentifier,
        explicit: String? = nil
    ) -> UTType {
        if identifier == .rootContainer || identifier == .workingSet {
            return .folder
        }

        if let explicit, let type = UTType(mimeType: explicit) {
            return type
        }

        let fileExtension = URL(fileURLWithPath: filename).pathExtension
        if let type = UTType(filenameExtension: fileExtension) {
            return type
        }

        return .data
    }

    private static func makeVersion(identifier: NSFileProviderItemIdentifier, size: NSNumber?) -> NSFileProviderItemVersion {
        let contentVersion = Data("\(identifier.rawValue):\(size?.int64Value ?? -1)".utf8)
        let metadataVersion = Data(identifier.rawValue.utf8)
        return NSFileProviderItemVersion(contentVersion: contentVersion, metadataVersion: metadataVersion)
    }
}
