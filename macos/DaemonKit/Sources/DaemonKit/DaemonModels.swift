import Foundation

// Hand-maintained Codable mirrors of `@valv/contracts-ipc` (oss/contracts/ipc/src/*.ts).
// There is no TS -> Swift codegen in this repo - if a route's request/response shape
// changes in the TS contracts package, these types must be updated to match by hand.
// Source of truth: oss/contracts/ipc/src/control.ts, oss/contracts/ipc/src/fileprovider.ts.

// MARK: - control.ts

public struct MountStatus: Codable, Hashable, Sendable {
    public let path: String
    public let folderId: String
    public let name: String
    public let scopeNodeId: String?
    public let grantId: String?
    public let canWrite: Bool
    public let syncing: Bool
    public let pendingOps: Int
    public let lastSyncedAt: String?
    public let updateRequired: Bool
    public let error: String?

    enum CodingKeys: String, CodingKey {
        case path
        case folderId = "folder_id"
        case name
        case scopeNodeId = "scope_node_id"
        case grantId = "grant_id"
        case canWrite = "can_write"
        case syncing
        case pendingOps = "pending_ops"
        case lastSyncedAt = "last_synced_at"
        case updateRequired = "update_required"
        case error
    }
}

public struct DaemonStatus: Codable, Hashable, Sendable {
    public let paused: Bool
    public let backendConnected: Bool
    public let version: String
    public let updateRequired: Bool
    public let mounts: [MountStatus]
    public let account: AccountStatus?
    public let latestVersion: String?
    public let updateAvailable: Bool?

    enum CodingKeys: String, CodingKey {
        case paused
        case backendConnected = "backend_connected"
        case version
        case updateRequired = "update_required"
        case mounts
        case account
        case latestVersion = "latest_version"
        case updateAvailable = "update_available"
    }
}

public struct AccountStatus: Codable, Hashable, Sendable {
    public let email: String?
    public let plan: String?
    public let status: String
    public let usageBytes: Int
    public let quotaBytes: Int?
    public let currentPeriodEnd: String?

    enum CodingKeys: String, CodingKey {
        case email
        case plan
        case status
        case usageBytes = "usage_bytes"
        case quotaBytes = "quota_bytes"
        case currentPeriodEnd = "current_period_end"
    }
}

public struct NodePathResponse: Codable, Hashable, Sendable {
    public let path: String
}

public struct MountRequest: Codable, Hashable, Sendable {
    public let path: String
    public let folderId: String?
    public let grantToken: String?

    enum CodingKeys: String, CodingKey {
        case path
        case folderId = "folder_id"
        case grantToken = "grant_token"
    }

    public init(path: String, folderId: String? = nil, grantToken: String? = nil) {
        self.path = path
        self.folderId = folderId
        self.grantToken = grantToken
    }
}

public struct MountResponse: Codable, Hashable, Sendable {
    public let folderId: String
    public let grantId: String?
    public let path: String

    enum CodingKeys: String, CodingKey {
        case folderId = "folder_id"
        case grantId = "grant_id"
        case path
    }
}

/// Unmounts locally only - does not touch the backend folder/grants, and does not
/// delete the locally materialized files.
public struct UnmountRequest: Codable, Hashable, Sendable {
    public let folderId: String

    enum CodingKeys: String, CodingKey {
        case folderId = "folder_id"
    }

    public init(folderId: String) {
        self.folderId = folderId
    }
}

public struct SyncRequest: Codable, Hashable, Sendable {
    public let folderId: String?

    enum CodingKeys: String, CodingKey {
        case folderId = "folder_id"
    }

    public init(folderId: String? = nil) {
        self.folderId = folderId
    }
}

public struct SyncSummary: Codable, Hashable, Sendable {
    public let createsSubmitted: Int
    public let versionsSubmitted: Int
    public let pulledOps: Int
    public let errors: Int

    enum CodingKeys: String, CodingKey {
        case createsSubmitted = "creates_submitted"
        case versionsSubmitted = "versions_submitted"
        case pulledOps = "pulled_ops"
        case errors
    }
}

// MARK: - fileprovider.ts

public struct FpItem: Codable, Hashable, Sendable {
    public enum ItemType: String, Codable, Hashable, Sendable {
        case file
        case folder
    }

    public let nodeId: String
    public let parentId: String?
    public let folderId: String
    public let name: String
    public let type: ItemType
    public let versionId: String?
    public let contentHash: String?
    public let sizeBytes: Int?
    public let serverSeq: Int
    public let deleted: Bool

    enum CodingKeys: String, CodingKey {
        case nodeId = "node_id"
        case parentId = "parent_id"
        case folderId = "folder_id"
        case name
        case type
        case versionId = "version_id"
        case contentHash = "content_hash"
        case sizeBytes = "size_bytes"
        case serverSeq = "server_seq"
        case deleted
    }

    // Swift's auto-synthesized memberwise initializer for a public struct is only
    // internal, not public, across module boundaries - needed explicitly here since
    // callers outside DaemonKit (e.g. ValvFileProvider's createItem, building an
    // optimistic placeholder item) construct FpItem directly, not just decode it.
    public init(
        nodeId: String,
        parentId: String?,
        folderId: String,
        name: String,
        type: ItemType,
        versionId: String?,
        contentHash: String?,
        sizeBytes: Int?,
        serverSeq: Int,
        deleted: Bool
    ) {
        self.nodeId = nodeId
        self.parentId = parentId
        self.folderId = folderId
        self.name = name
        self.type = type
        self.versionId = versionId
        self.contentHash = contentHash
        self.sizeBytes = sizeBytes
        self.serverSeq = serverSeq
        self.deleted = deleted
    }
}

public struct FpEnumerateResponse: Codable, Hashable, Sendable {
    public let items: [FpItem]
    public let total: Int
    public let syncedToSeq: Int
    public let canWrite: Bool

    enum CodingKeys: String, CodingKey {
        case items
        case total
        case syncedToSeq = "synced_to_seq"
        case canWrite = "can_write"
    }
}

public struct FpAnchorResponse: Codable, Hashable, Sendable {
    public let serverSeq: Int
    public let canWrite: Bool

    enum CodingKeys: String, CodingKey {
        case serverSeq = "server_seq"
        case canWrite = "can_write"
    }
}

/// Mirrors `FpWatchResponse` (a TS type alias for `FpAnchorResponse`) as well.
public typealias FpWatchResponse = FpAnchorResponse

public struct FpChangesResponse: Codable, Hashable, Sendable {
    public let items: [FpItem]
    public let currentSeq: Int
    public let moreComing: Bool

    enum CodingKeys: String, CodingKey {
        case items
        case currentSeq = "current_seq"
        case moreComing = "more_coming"
    }
}

public struct FpChunkDownload: Codable, Hashable, Sendable {
    public let chunkHash: String
    public let offset: Int
    public let length: Int
    public let url: String
    public let expiresIn: Int

    enum CodingKeys: String, CodingKey {
        case chunkHash = "chunk_hash"
        case offset
        case length
        case url
        case expiresIn = "expires_in"
    }
}

public struct FpContentResponse: Codable, Hashable, Sendable {
    public let versionId: String
    public let sizeBytes: Int
    public let chunks: [FpChunkDownload]

    enum CodingKeys: String, CodingKey {
        case versionId = "version_id"
        case sizeBytes = "size_bytes"
        case chunks
    }
}

public struct FpUploadRequest: Codable, Hashable, Sendable {
    public let nodeId: String?
    public let parentId: String
    public let name: String
    public let basedOnSeq: Int?
    public let filePath: String

    enum CodingKeys: String, CodingKey {
        case nodeId = "node_id"
        case parentId = "parent_id"
        case name
        case basedOnSeq = "based_on_seq"
        case filePath = "file_path"
    }

    public init(nodeId: String?, parentId: String, name: String, basedOnSeq: Int?, filePath: String) {
        self.nodeId = nodeId
        self.parentId = parentId
        self.name = name
        self.basedOnSeq = basedOnSeq
        self.filePath = filePath
    }
}

public struct FpUploadQueued: Codable, Hashable, Sendable {
    public let queued: Bool
    public let nodeId: String

    enum CodingKeys: String, CodingKey {
        case queued
        case nodeId = "node_id"
    }
}

public struct FpDeleteRequest: Codable, Hashable, Sendable {
    public let nodeId: String
    public let basedOnSeq: Int

    enum CodingKeys: String, CodingKey {
        case nodeId = "node_id"
        case basedOnSeq = "based_on_seq"
    }

    public init(nodeId: String, basedOnSeq: Int) {
        self.nodeId = nodeId
        self.basedOnSeq = basedOnSeq
    }
}

public struct FpMoveRequest: Codable, Hashable, Sendable {
    public let nodeId: String
    public let basedOnSeq: Int
    public let newName: String?
    public let newParentId: String?

    enum CodingKeys: String, CodingKey {
        case nodeId = "node_id"
        case basedOnSeq = "based_on_seq"
        case newName = "new_name"
        case newParentId = "new_parent_id"
    }

    public init(nodeId: String, basedOnSeq: Int, newName: String?, newParentId: String?) {
        self.nodeId = nodeId
        self.basedOnSeq = basedOnSeq
        self.newName = newName
        self.newParentId = newParentId
    }
}

public struct FpMoveResponse: Codable, Hashable, Sendable {
    public let nodeId: String
    public let serverSeq: Int

    enum CodingKeys: String, CodingKey {
        case nodeId = "node_id"
        case serverSeq = "server_seq"
    }
}

public struct FpShareRequest: Codable, Hashable, Sendable {
    public let nodeId: String
    public let invitedEmail: String
    public let canWrite: Bool

    enum CodingKeys: String, CodingKey {
        case nodeId = "node_id"
        case invitedEmail = "invited_email"
        case canWrite = "can_write"
    }

    public init(nodeId: String, invitedEmail: String, canWrite: Bool = true) {
        self.nodeId = nodeId
        self.invitedEmail = invitedEmail
        self.canWrite = canWrite
    }
}

public struct FpShareResponse: Codable, Hashable, Sendable {
    public let inviteUrl: String

    enum CodingKeys: String, CodingKey {
        case inviteUrl = "invite_url"
    }
}
