import DaemonKit
import FileProvider
import UniformTypeIdentifiers

final class FileProviderItem: NSObject, NSFileProviderItem {
    enum Kind {
        /// A synthetic folder representing one entry from `GET /mounts`, shown directly
        /// under the domain's single root container. Read-only from Finder's
        /// perspective - see `capabilities` below - regardless of the mount's own
        /// `can_write` (that governs the *contents* of the mount, not the synthetic
        /// entry point itself).
        case syntheticMount(MountStatus)
        /// A real node inside a mount's tree. `item.folderId` identifies its mount
        /// directly (added to `FpItem` specifically so callers never need a separate
        /// node-to-mount lookup or cache); `mountCanWrite` is carried alongside since
        /// `FpItem` itself has no `can_write` field - that lives on the enumeration
        /// response / mount status instead.
        case node(FpItem, mountCanWrite: Bool)
    }

    let kind: Kind

    init(kind: Kind) {
        self.kind = kind
    }

    var itemIdentifier: NSFileProviderItemIdentifier {
        switch kind {
        case .syntheticMount(let mount):
            return ValvItemIdentifier.mount(folderId: mount.folderId).fileProviderIdentifier
        case .node(let item, _):
            return ValvItemIdentifier.node(nodeId: item.nodeId).fileProviderIdentifier
        }
    }

    var parentItemIdentifier: NSFileProviderItemIdentifier {
        switch kind {
        case .syntheticMount:
            return .rootContainer
        case .node(let item, _):
            guard let parentId = item.parentId else {
                // A node with no parent is the root of its mount's tree; its parent in
                // Finder's eyes is the synthetic mount entry, not `.rootContainer`
                // directly (there is exactly one level of synthetic nesting).
                return ValvItemIdentifier.mount(folderId: item.folderId).fileProviderIdentifier
            }
            return ValvItemIdentifier.node(nodeId: parentId).fileProviderIdentifier
        }
    }

    var filename: String {
        switch kind {
        case .syntheticMount(let mount):
            return mount.name.isEmpty ? mount.folderId : mount.name
        case .node(let item, _):
            return item.name
        }
    }

    var contentType: UTType {
        switch kind {
        case .syntheticMount:
            return .folder
        case .node(let item, _):
            if item.type == .folder {
                return .folder
            }
            return UTType(filenameExtension: URL(fileURLWithPath: item.name).pathExtension) ?? .data
        }
    }

    /// Real `NSFileProviderItemCapabilities` has no dedicated "can share" flag - the
    /// options are read/write/reparent/rename/trash/delete plus the enumerating/adding
    /// aliases (confirmed against the FileProvider.framework headers directly, not
    /// assumed). The Finder "Share…" action's visibility is controlled entirely by
    /// `ValvFileProviderUI`'s `NSExtensionFileProviderActionActivationRule` (see section
    /// 5), independent of this type; the backend's own write-capability check
    /// (`phase-5-sharing-api`) is the real enforcement point regardless of what the
    /// activation rule predicate can express (see design.md Risks).
    var capabilities: NSFileProviderItemCapabilities {
        switch kind {
        case .syntheticMount:
            // Read-only synthetic entry: no rename/reparent/trash/delete/add-subitems,
            // matching task 4.10's "reject outright, independent of capabilities"
            // defense-in-depth requirement in FileProviderExtension.
            return [.allowsReading, .allowsContentEnumerating]
        case .node(let item, let mountCanWrite):
            guard mountCanWrite else {
                var readOnly: NSFileProviderItemCapabilities = [.allowsReading]
                if item.type == .folder {
                    readOnly.insert(.allowsContentEnumerating)
                }
                return readOnly
            }
            var writable: NSFileProviderItemCapabilities = [
                .allowsReading, .allowsWriting, .allowsTrashing, .allowsDeleting,
                .allowsRenaming, .allowsReparenting,
            ]
            if item.type == .folder {
                writable.insert(.allowsAddingSubItems)
            }
            return writable
        }
    }

    var documentSize: NSNumber? {
        switch kind {
        case .syntheticMount:
            return nil
        case .node(let item, _):
            return item.sizeBytes.map(NSNumber.init(value:))
        }
    }

    var itemVersion: NSFileProviderItemVersion {
        switch kind {
        case .syntheticMount(let mount):
            let content = Data("mount:\(mount.folderId):\(mount.name)".utf8)
            return NSFileProviderItemVersion(contentVersion: content, metadataVersion: content)
        case .node(let item, _):
            let content = Data("\(item.nodeId):\(item.versionId ?? ""):\(item.serverSeq)".utf8)
            let metadata = Data("\(item.nodeId):\(item.name):\(item.serverSeq)".utf8)
            return NSFileProviderItemVersion(contentVersion: content, metadataVersion: metadata)
        }
    }

    /// The mount a node belongs to, for callers (`FileProviderExtension`) that need to
    /// reject cross-mount reparenting (task 4.11) without re-deriving it from the
    /// identifier scheme.
    var mountFolderId: String {
        switch kind {
        case .syntheticMount(let mount):
            return mount.folderId
        case .node(let item, _):
            return item.folderId
        }
    }
}
