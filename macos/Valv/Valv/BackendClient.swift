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
    let granteeEmail: String?
    let deviceName: String?

    enum CodingKeys: String, CodingKey {
        case grantId = "grant_id"
        case folderId = "folder_id"
        case scopeNodeId = "scope_node_id"
        case role
        case canRead = "can_read"
        case canWrite = "can_write"
        case userId = "user_id"
        case deviceId = "device_id"
        case granteeEmail = "grantee_email"
        case deviceName = "device_name"
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

/// Direct-to-backend client for the Manage Folders & Sharing window - `GET /grants`,
/// invites, and device-grant provisioning, using the app's own device token exactly
/// like `valv-cli` already does for these same endpoints (not proxied through
/// `valvd`, unlike everything else in this app - see the `macos-app` spec's own
/// requirement for this specific carve-out).
final class BackendClient {
    private let session = URLSession(configuration: .ephemeral)
    private let decoder = JSONDecoder()
    private let encoder = JSONEncoder()

    func grants() async throws -> [GrantEntry] {
        try await get("/api/grants")
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

    func createDeviceGrant(folderId: String, scopeNodeId: String?, name: String, canWrite: Bool) async throws -> String {
        struct Response: Decodable { let token: String }
        let response: Response = try await send(
            method: "POST",
            path: "/api/folders/\(folderId)/grants",
            body: DeviceGrantRequestBody(scope_node_id: scopeNodeId, name: name, can_read: true, can_write: canWrite)
        )
        return response.token
    }

    func revokeGrant(folderId: String, grantId: String) async throws {
        try await sendNoBody(method: "DELETE", path: "/api/folders/\(folderId)/grants/\(grantId)")
    }

    // MARK: - Core transport

    private func get<Response: Decodable>(_ path: String) async throws -> Response {
        let data = try await performRequest(method: "GET", path: path, bodyData: nil)
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
        guard let config = ConfigReader.read(), let url = URL(string: "\(config.backendURL)\(path)") else {
            throw BackendClientError.notConfigured
        }
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = method
        urlRequest.setValue("Bearer \(config.deviceToken)", forHTTPHeaderField: "Authorization")
        if let bodyData {
            urlRequest.httpBody = bodyData
            urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let (data, response) = try await session.data(for: urlRequest)
        guard let httpResponse = response as? HTTPURLResponse else {
            throw BackendClientError.httpStatus(0, "no response")
        }
        guard (200 ..< 300).contains(httpResponse.statusCode) else {
            throw BackendClientError.httpStatus(httpResponse.statusCode, String(data: data, encoding: .utf8) ?? "")
        }
        return data
    }
}
