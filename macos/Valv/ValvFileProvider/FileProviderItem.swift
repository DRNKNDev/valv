import DaemonKit
import FileProvider
import UniformTypeIdentifiers

final class FileProviderItem: NSObject, NSFileProviderItem {
    enum Kind {
        /// The domain's single root container. Answered without consulting the daemon:
        /// the root is not a node in any mount's tree, and its children are the
        /// synthetic mount symlinks below.
        case root
        /// A symlink representing one entry from `GET /mounts`, shown directly under
        /// the domain's single root container. Its `symlinkTargetPath` is the mount's
        /// real local path, already maintained on disk by `fs_watch`/
        /// `materialize_mount_files` - the domain holds no bytes of its own.
        case syntheticMount(MountStatus)
    }

    let kind: Kind

    init(kind: Kind) {
        self.kind = kind
    }

    var itemIdentifier: NSFileProviderItemIdentifier {
        switch kind {
        case .root:
            return .rootContainer
        case .syntheticMount(let mount):
            return ValvItemIdentifier.mount(folderId: mount.folderId).fileProviderIdentifier
        }
    }

    var parentItemIdentifier: NSFileProviderItemIdentifier {
        switch kind {
        case .root:
            return .rootContainer
        case .syntheticMount:
            return .rootContainer
        }
    }

    var filename: String {
        switch kind {
        case .root:
            return "Valv"
        case .syntheticMount(let mount):
            return mount.name.isEmpty ? mount.folderId : mount.name
        }
    }

    var contentType: UTType {
        switch kind {
        case .root:
            return .folder
        case .syntheticMount:
            return .symbolicLink
        }
    }

    var symlinkTargetPath: String? {
        switch kind {
        case .root:
            return nil
        case .syntheticMount(let mount):
            return mount.path
        }
    }

    var capabilities: NSFileProviderItemCapabilities {
        switch kind {
        case .root:
            return [.allowsReading, .allowsContentEnumerating]
        case .syntheticMount:
            return [.allowsReading]
        }
    }

    var itemVersion: NSFileProviderItemVersion {
        switch kind {
        case .root:
            let content = Data("root".utf8)
            return NSFileProviderItemVersion(contentVersion: content, metadataVersion: content)
        case .syntheticMount(let mount):
            let content = Data("mount:\(mount.folderId):\(mount.name):\(mount.path)".utf8)
            return NSFileProviderItemVersion(contentVersion: content, metadataVersion: content)
        }
    }

}
