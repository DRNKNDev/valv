import Foundation
import Network

/// Errors surfaced by `DaemonClient`. `httpStatus` carries the daemon's raw JSON error
/// body (if any) so callers can inspect e.g. `{"error": "..."}` without re-parsing.
public enum DaemonClientError: LocalizedError, Sendable {
    case portFileUnavailable
    case invalidPortFile(String)
    case connectionFailed(String)
    case malformedResponse
    case httpStatus(Int, String)

    public var errorDescription: String? {
        switch self {
        case .portFileUnavailable:
            return "Could not read valvd's TCP loopback port file from the shared app group container"
        case .invalidPortFile(let contents):
            return "valvd's TCP port file has unexpected contents: \(contents)"
        case .connectionFailed(let reason):
            return "Failed to connect to valvd: \(reason)"
        case .malformedResponse:
            return "valvd returned a response DaemonClient could not parse"
        case .httpStatus(let status, let body):
            return "valvd returned HTTP \(status): \(body)"
        }
    }
}

/// HTTP-over-TCP-loopback client for `valvd`'s IPC API.
///
/// `valvd` binds a Unix-domain socket (used by `valv-cli` and other non-sandboxed local
/// clients) and, alongside it, a `127.0.0.1`-only TCP listener on an OS-assigned ephemeral
/// port (see `daemon-ipc-server` spec, added by `phase-5-macos-gui`). App Sandbox denies
/// `connect()` on the Unix-domain socket regardless of entitlement - validated directly,
/// not assumed - so all three Xcode targets (`Valv`, `ValvFileProvider`, `ValvFileProviderUI`)
/// use this TCP transport uniformly instead of a UDS path.
///
/// The port itself is discovered by reading a plain-text port number `valvd` writes to
/// `valvd-tcp-port` inside the shared app-group container, resolved via
/// `FileManager.containerURL(forSecurityApplicationGroupIdentifier:)` - the standard,
/// sandbox-safe API for this, not a manually-constructed home-directory path (which would
/// resolve to this process's own sandbox container, not the real shared location).
public actor DaemonClient {
    public static let appGroupIdentifier = "group.dev.drnkn.valv"
    private static let portFileName = "valvd-tcp-port"

    private let encoder: JSONEncoder
    private let decoder: JSONDecoder
    private let portProvider: @Sendable () throws -> UInt16

    public init(appGroupIdentifier: String = DaemonClient.appGroupIdentifier) {
        self.init(portProvider: {
            try Self.readPortFile(appGroupIdentifier: appGroupIdentifier)
        })
    }

    /// Test-only seam: points the client directly at a known port instead of resolving
    /// it from the app-group container's port file, which requires real entitlements
    /// that an ad-hoc-signed `swift test` binary never has (`containerURL(for
    /// SecurityApplicationGroupIdentifier:)` returns `nil` without them). Internal, not
    /// public - accessed by `DaemonKitTests` via `@testable import DaemonKit`, not part
    /// of the library's real API surface.
    init(portProvider: @escaping @Sendable () throws -> UInt16) {
        self.portProvider = portProvider
        self.encoder = JSONEncoder()
        self.decoder = JSONDecoder()
    }

    // MARK: - Control API

    public func status() async throws -> DaemonStatus {
        try await get("/status")
    }

    public func mounts() async throws -> [MountStatus] {
        try await get("/mounts")
    }

    public func mount(_ request: MountRequest) async throws -> MountResponse {
        try await post("/mount", body: request)
    }

    /// Unmounts locally only - does not touch the backend folder/grants, and does not
    /// delete the locally materialized files.
    public func unmount(folderId: String) async throws {
        let body = try encoder.encode(UnmountRequest(folderId: folderId))
        _ = try await requestData(method: "DELETE", path: "/mount", body: body)
    }

    public func pause() async throws {
        _ = try await requestData(method: "POST", path: "/pause")
    }

    public func resume() async throws {
        _ = try await requestData(method: "POST", path: "/resume")
    }

    public func sync(folderId: String? = nil) async throws -> SyncSummary {
        try await post("/sync", body: SyncRequest(folderId: folderId))
    }

    public func nodePath(nodeId: String) async throws -> NodePathResponse {
        try await get("/nodes/\(nodeId)/path")
    }

    // MARK: - File Provider API

    // `folderId` is required (not optional) even though the daemon's own query struct
    // treats it as optional: `resolve_mount_for_query` only lets it be omitted when
    // exactly one mount exists daemon-wide, and this app always deals with multiple
    // mounts by design (the synthetic multi-mount root, design.md D11) - making it
    // required here means a caller can never accidentally omit it and get a confusing
    // "folder_id is required when multiple folders are mounted" error back instead.
    public func fpItems(folderId: String, parent: String, offset: Int = 0, limit: Int = 200) async throws -> FpEnumerateResponse {
        try await get("/fp/items?folder_id=\(urlEncoded(folderId))&parent=\(urlEncoded(parent))&offset=\(offset)&limit=\(limit)")
    }

    public func fpItem(nodeId: String) async throws -> FpItem {
        try await get("/fp/item/\(nodeId)")
    }

    public func fpAnchor(folderId: String) async throws -> FpAnchorResponse {
        try await get("/fp/anchor?folder_id=\(urlEncoded(folderId))")
    }

    public func fpChanges(folderId: String, sinceSeq: Int) async throws -> FpChangesResponse {
        try await get("/fp/changes?folder_id=\(urlEncoded(folderId))&since_seq=\(sinceSeq)")
    }

    public func fpContent(nodeId: String) async throws -> FpContentResponse {
        try await get("/fp/content/\(nodeId)")
    }

    public func fpUpload(_ request: FpUploadRequest) async throws -> FpUploadQueued {
        try await post("/fp/upload", body: request)
    }

    public func fpDelete(nodeId: String, basedOnSeq: Int) async throws {
        let body = try encoder.encode(FpDeleteRequest(nodeId: nodeId, basedOnSeq: basedOnSeq))
        _ = try await requestData(method: "POST", path: "/fp/delete", body: body)
    }

    /// Long-polls until the mount's cursor advances past `sinceSeq` or the daemon's
    /// ~25s timeout elapses (see `ipc-fp-api` spec's `GET /fp/watch` requirement).
    /// Callers are expected to call this in a loop, using the returned `serverSeq`
    /// as the next call's `sinceSeq`.
    public func fpWatch(folderId: String, sinceSeq: Int) async throws -> FpWatchResponse {
        try await get("/fp/watch?folder_id=\(urlEncoded(folderId))&since_seq=\(sinceSeq)", timeout: 30)
    }

    public func fpShare(nodeId: String, invitedEmail: String, canWrite: Bool = true) async throws -> FpShareResponse {
        try await post("/fp/share", body: FpShareRequest(nodeId: nodeId, invitedEmail: invitedEmail, canWrite: canWrite))
    }

    // MARK: - JSON convenience wrappers

    private func get<Response: Decodable>(_ path: String, timeout: TimeInterval = 10) async throws -> Response {
        let data = try await requestData(method: "GET", path: path, timeout: timeout)
        return try decode(data)
    }

    private func post<Body: Encodable, Response: Decodable>(_ path: String, body: Body) async throws -> Response {
        let encoded = try encoder.encode(body)
        let data = try await requestData(method: "POST", path: path, body: encoded)
        return try decode(data)
    }

    private func decode<Response: Decodable>(_ data: Data) throws -> Response {
        do {
            return try decoder.decode(Response.self, from: data)
        } catch {
            throw DaemonClientError.malformedResponse
        }
    }

    private func urlEncoded(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .urlQueryValueAllowed) ?? value
    }

    // MARK: - Port resolution

    private static func readPortFile(appGroupIdentifier: String) throws -> UInt16 {
        guard let containerURL = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroupIdentifier
        ) else {
            throw DaemonClientError.portFileUnavailable
        }
        let portFileURL = containerURL.appendingPathComponent(Self.portFileName)
        guard let contents = try? String(contentsOf: portFileURL, encoding: .utf8) else {
            throw DaemonClientError.portFileUnavailable
        }
        let trimmed = contents.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let port = UInt16(trimmed) else {
            throw DaemonClientError.invalidPortFile(trimmed)
        }
        return port
    }

    // MARK: - Core HTTP/1.1-over-NWConnection transport

    /// Hand-rolled HTTP/1.1 request/response framing over `NWConnection`, matching
    /// `valvd`'s own minimal expectations (see design.md D2). Every payload on this API
    /// is small JSON, so this ~150-line client is proportionate; no third-party
    /// dependency (e.g. URLSession, which has no native path here anyway since the
    /// daemon's *other* listener is a Unix-domain socket URLSession can't reach either)
    /// is needed.
    private func requestData(
        method: String,
        path: String,
        body: Data? = nil,
        timeout: TimeInterval = 10
    ) async throws -> Data {
        try await withTimeout(timeout) {
            let port = try self.portProvider()
            guard let nwPort = NWEndpoint.Port(rawValue: port) else {
                throw DaemonClientError.invalidPortFile("\(port)")
            }
            let connection = NWConnection(host: "127.0.0.1", port: nwPort, using: .tcp)
            defer { connection.cancel() }

            try await self.waitUntilReady(connection)

            var requestText = "\(method) \(path) HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n"
            if let body {
                requestText += "Content-Type: application/json\r\nContent-Length: \(body.count)\r\n"
            }
            requestText += "\r\n"

            var requestData = Data(requestText.utf8)
            if let body {
                requestData.append(body)
            }
            try await self.send(requestData, over: connection)

            let raw = try await self.receiveUntilClosed(connection)
            return try self.parseHTTPResponse(raw)
        }
    }

    /// Races `operation` against a plain `Task.sleep`-based deadline. Needed because
    /// `NWConnection` has no built-in per-call timeout - without this, a `GET /fp/watch`
    /// long-poll (or any call, if `valvd` hangs or dies mid-request) could block forever.
    private func withTimeout<T: Sendable>(
        _ seconds: TimeInterval,
        operation: @escaping () async throws -> T
    ) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await operation() }
            group.addTask {
                try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
                throw DaemonClientError.connectionFailed("timed out after \(seconds)s")
            }
            guard let result = try await group.next() else {
                throw DaemonClientError.connectionFailed("timeout race produced no result")
            }
            group.cancelAll()
            return result
        }
    }

    private func waitUntilReady(_ connection: NWConnection) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            var didResume = false
            let resumeOnce: (Result<Void, Error>) -> Void = { result in
                guard !didResume else { return }
                didResume = true
                switch result {
                case .success:
                    continuation.resume()
                case .failure(let error):
                    continuation.resume(throwing: error)
                }
            }

            connection.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    resumeOnce(.success(()))
                case .failed(let error):
                    resumeOnce(.failure(DaemonClientError.connectionFailed(error.localizedDescription)))
                case .cancelled:
                    resumeOnce(.failure(DaemonClientError.connectionFailed("cancelled")))
                default:
                    break
                }
            }
            connection.start(queue: .global())
        }
    }

    private func send(_ data: Data, over connection: NWConnection) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.send(
                content: data,
                completion: .contentProcessed { error in
                    if let error {
                        continuation.resume(throwing: DaemonClientError.connectionFailed(error.localizedDescription))
                    } else {
                        continuation.resume()
                    }
                }
            )
        }
    }

    /// Reads until the peer closes the connection. Safe here because every request sets
    /// `Connection: close`, so a clean EOF always marks the true end of the response.
    private func receiveUntilClosed(_ connection: NWConnection) async throws -> Data {
        var accumulated = Data()
        while true {
            let (chunk, isComplete) = try await receiveOnce(connection)
            if let chunk {
                accumulated.append(chunk)
            }
            if isComplete {
                break
            }
        }
        return accumulated
    }

    private func receiveOnce(_ connection: NWConnection) async throws -> (Data?, Bool) {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<(Data?, Bool), Error>) in
            connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { data, _, isComplete, error in
                if let error {
                    continuation.resume(throwing: DaemonClientError.connectionFailed(error.localizedDescription))
                    return
                }
                continuation.resume(returning: (data, isComplete))
            }
        }
    }

    private func parseHTTPResponse(_ raw: Data) throws -> Data {
        guard let separatorRange = raw.range(of: Data("\r\n\r\n".utf8)) else {
            throw DaemonClientError.malformedResponse
        }
        let headerData = raw[..<separatorRange.lowerBound]
        let bodyData = raw[separatorRange.upperBound...]

        guard let headerText = String(data: headerData, encoding: .utf8) else {
            throw DaemonClientError.malformedResponse
        }
        let lines = headerText.components(separatedBy: "\r\n")
        guard let statusLine = lines.first else {
            throw DaemonClientError.malformedResponse
        }
        let statusParts = statusLine.split(separator: " ", maxSplits: 2)
        guard statusParts.count >= 2, let statusCode = Int(statusParts[1]) else {
            throw DaemonClientError.malformedResponse
        }

        guard (200 ..< 300).contains(statusCode) else {
            let bodyText = String(data: bodyData, encoding: .utf8) ?? ""
            throw DaemonClientError.httpStatus(statusCode, bodyText)
        }

        return Data(bodyData)
    }
}

private extension CharacterSet {
    /// `.urlQueryAllowed` alone still permits characters (`&`, `=`, `+`) that are
    /// structurally significant in a query string; this narrower set is safe for a
    /// single query *value*, which is all `urlEncoded(_:)` is ever used for here.
    static let urlQueryValueAllowed: CharacterSet = {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return allowed
    }()
}
