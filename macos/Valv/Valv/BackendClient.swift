import Foundation
import DaemonKit

struct GrantEntry: Decodable, Identifiable, Hashable {
    var id: String { grantId }
    let grantId: String
    let folderId: String
    let scopeNodeId: String
    let role: String
    let canRead: Bool
    let canWrite: Bool
    let userId: String?
    let deviceId: String?
    let name: String?
    let granteeEmail: String?
    let deviceName: String?
    let createdByEmail: String?

    enum CodingKeys: String, CodingKey {
        case grantId = "grant_id"
        case folderId = "folder_id"
        case scopeNodeId = "scope_node_id"
        case role
        case canRead = "can_read"
        case canWrite = "can_write"
        case userId = "user_id"
        case deviceId = "device_id"
        case name
        case granteeEmail = "grantee_email"
        case deviceName = "device_name"
        case createdByEmail = "created_by_email"
    }

    var isOwnerGrant: Bool { role == "owner" }

    var displayName: String {
        granteeEmail ?? name ?? deviceName ?? "Unknown"
    }
}

struct PendingInvite: Decodable, Identifiable, Hashable {
    var id: String { inviteId }
    let inviteId: String
    let invitedEmail: String
    let scopeNodeId: String
    let canWrite: Bool
    let createdByEmail: String?

    enum CodingKeys: String, CodingKey {
        case inviteId = "invite_id"
        case invitedEmail = "invited_email"
        case scopeNodeId = "scope_node_id"
        case canWrite = "can_write"
        case createdByEmail = "created_by_email"
    }
}

struct AccessKeyIssued: Decodable {
    let grantId: String
    let deviceId: String
    let token: String

    enum CodingKeys: String, CodingKey {
        case grantId = "grant_id"
        case deviceId = "device_id"
        case token
    }
}

struct DeviceGrantRequestBody: Encodable {
    let scope_node_id: String?
    let name: String
    let can_read: Bool
    let can_write: Bool
}

enum BackendClientError: LocalizedError {
    case notConfigured
    case httpStatus(Int, String)

    var errorDescription: String? {
        switch self {
        case .notConfigured:
            return "Not signed in yet."
        case .httpStatus(let status, let body):
            return "Backend returned HTTP \(status): \(body)"
        }
    }
}

extension BackendClientError: HTTPBodyCarrying {
    var httpStatusAndBody: (Int, String)? {
        if case .httpStatus(let status, let body) = self {
            return (status, body)
        }
        return nil
    }
}

/// Direct-to-backend client for the Manage Folders & Sharing window: `GET /grants`,
/// the folder-scoped grants/invites lists, and device-grant provisioning, using the
/// app's own device token directly, not proxied through `valvd` like the rest of
/// this app.
final class BackendClient {
    private let session = URLSession(configuration: .ephemeral)
    private let decoder = JSONDecoder()
    private let encoder = JSONEncoder()
    private let configProvider: () -> ConfigReader.Values?
    private let transport: (@Sendable (URLRequest) async throws -> (Data, HTTPURLResponse))?

    init(
        configProvider: @escaping () -> ConfigReader.Values? = { ConfigReader.read() },
        transport: (@Sendable (URLRequest) async throws -> (Data, HTTPURLResponse))? = nil
    ) {
        self.configProvider = configProvider
        self.transport = transport
    }

    func grants() async throws -> [GrantEntry] {
        try await get("/api/grants")
    }

    func folderGrants(folderId: String) async throws -> [GrantEntry] {
        try await get("/api/folders/\(folderId)/grants")
    }

    func folderInvites(folderId: String) async throws -> [PendingInvite] {
        try await get("/api/folders/\(folderId)/invites")
    }

    func cancelInvite(folderId: String, inviteId: String) async throws {
        try await sendNoBody(method: "DELETE", path: "/api/folders/\(folderId)/invites/\(inviteId)")
    }

    func createInvite(folderId: String, invitedEmail: String, canWrite: Bool) async throws -> String {
        struct Body: Encodable { let invited_email: String; let can_write: Bool }
        struct Response: Decodable { let invite_token: String }
        let response: Response = try await send(
            method: "POST",
            path: "/api/folders/\(folderId)/invites",
            body: Body(invited_email: invitedEmail, can_write: canWrite)
        )
        return response.invite_token
    }

    func createDeviceGrant(folderId: String, scopeNodeId: String?, name: String, canWrite: Bool) async throws -> AccessKeyIssued {
        try await send(
            method: "POST",
            path: "/api/folders/\(folderId)/grants",
            body: DeviceGrantRequestBody(scope_node_id: scopeNodeId, name: name, can_read: true, can_write: canWrite)
        )
    }

    /// The single atomic replace-and-revoke call. Never compose this from a
    /// separate revoke + provision: either order has a failure mode this one
    /// call does not.
    func regenerateGrant(folderId: String, grantId: String) async throws -> AccessKeyIssued {
        try await post("/api/folders/\(folderId)/grants/\(grantId)/regenerate")
    }

    func revokeGrant(folderId: String, grantId: String) async throws {
        try await sendNoBody(method: "DELETE", path: "/api/folders/\(folderId)/grants/\(grantId)")
    }

    // MARK: - Core transport

    private func get<Response: Decodable>(_ path: String) async throws -> Response {
        let data = try await performRequest(method: "GET", path: path, bodyData: nil)
        return try decoder.decode(Response.self, from: data)
    }

    private func post<Response: Decodable>(_ path: String) async throws -> Response {
        let data = try await performRequest(method: "POST", path: path, bodyData: nil)
        return try decoder.decode(Response.self, from: data)
    }

    private func send<Body: Encodable, Response: Decodable>(method: String, path: String, body: Body) async throws -> Response {
        let bodyData = try encoder.encode(body)
        let data = try await performRequest(method: method, path: path, bodyData: bodyData)
        return try decoder.decode(Response.self, from: data)
    }

    private func sendNoBody(method: String, path: String) async throws {
        _ = try await performRequest(method: method, path: path, bodyData: nil)
    }

    private func performRequest(method: String, path: String, bodyData: Data?) async throws -> Data {
        guard let config = configProvider(), let url = URL(string: "\(config.backendURL)\(path)") else {
            throw BackendClientError.notConfigured
        }
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = method
        urlRequest.setValue("Bearer \(config.deviceToken)", forHTTPHeaderField: "Authorization")
        if let bodyData {
            urlRequest.httpBody = bodyData
            urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let data: Data
        let httpResponse: HTTPURLResponse
        if let transport {
            (data, httpResponse) = try await transport(urlRequest)
        } else {
            let (responseData, response) = try await session.data(for: urlRequest)
            guard let resolved = response as? HTTPURLResponse else {
                throw BackendClientError.httpStatus(0, "no response")
            }
            data = responseData
            httpResponse = resolved
        }
        guard (200 ..< 300).contains(httpResponse.statusCode) else {
            throw BackendClientError.httpStatus(httpResponse.statusCode, String(data: data, encoding: .utf8) ?? "")
        }
        return data
    }
}
