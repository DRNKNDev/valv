import Network
import XCTest

@testable import DaemonKit

/// Minimal in-process TCP server that speaks just enough HTTP/1.1 to test
/// `DaemonClient` without a real `valvd`. Not a general-purpose test double -
/// keyed by exact request line, one canned response per registered path.
final class MockDaemonServer {
    private let listener: NWListener
    private var responsesByRequestLine: [String: (status: Int, body: String)] = [:]

    private init(listener: NWListener) {
        self.listener = listener
    }

    static func start() async throws -> MockDaemonServer {
        let listener = try NWListener(using: .tcp, on: NWEndpoint.Port(rawValue: 0)!)
        let server = MockDaemonServer(listener: listener)
        listener.newConnectionHandler = { [weak server] connection in
            server?.handle(connection)
        }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            listener.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    continuation.resume()
                case .failed(let error):
                    continuation.resume(throwing: error)
                default:
                    break
                }
            }
            listener.start(queue: .global())
        }
        return server
    }

    var port: UInt16 {
        listener.port!.rawValue
    }

    /// Registers a canned response for an exact `"METHOD /path"` request line.
    func respond(to requestLine: String, status: Int, body: String) {
        responsesByRequestLine[requestLine] = (status, body)
    }

    func stop() {
        listener.cancel()
    }

    private func handle(_ connection: NWConnection) {
        connection.start(queue: .global())
        receive(on: connection, accumulated: Data())
    }

    private func receive(on connection: NWConnection, accumulated: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { [weak self] data, _, isComplete, _ in
            guard let self else { return }
            var buffer = accumulated
            if let data {
                buffer.append(data)
            }
            // Requests here are always header-only (GET) or small enough to arrive in
            // one chunk; a real client always closes after receiving, so waiting for
            // the double-CRLF is sufficient without full Content-Length tracking.
            if let headerEnd = buffer.range(of: Data("\r\n\r\n".utf8)) {
                let headerText = String(data: buffer[..<headerEnd.lowerBound], encoding: .utf8) ?? ""
                let requestLine = headerText.components(separatedBy: "\r\n").first ?? ""
                let parts = requestLine.split(separator: " ")
                let key = parts.count >= 2 ? "\(parts[0]) \(parts[1])" : requestLine

                let response = self.responsesByRequestLine[key] ?? (404, "{\"error\":\"not_found\"}")
                let statusText = response.status == 200 ? "OK" : "Error"
                let responseText = "HTTP/1.1 \(response.status) \(statusText)\r\n"
                    + "Content-Type: application/json\r\n"
                    + "Content-Length: \(response.body.utf8.count)\r\n"
                    + "Connection: close\r\n\r\n"
                    + response.body
                connection.send(
                    content: Data(responseText.utf8),
                    completion: .contentProcessed { _ in
                        connection.cancel()
                    }
                )
                return
            }
            if isComplete {
                connection.cancel()
                return
            }
            self.receive(on: connection, accumulated: buffer)
        }
    }
}

final class DaemonClientTests: XCTestCase {
    func testStatusDecodesSuccessfully() async throws {
        let server = try await MockDaemonServer.start()
        defer { server.stop() }
        server.respond(
            to: "GET /status",
            status: 200,
            body: """
            {"paused":false,"backend_connected":true,"version":"1.2.3","mounts":[]}
            """
        )

        let client = DaemonClient(portProvider: { server.port })
        let status = try await client.status()

        XCTAssertFalse(status.paused)
        XCTAssertTrue(status.backendConnected)
        XCTAssertEqual(status.version, "1.2.3")
        XCTAssertEqual(status.mounts, [])
    }

    func testMountsDecodesArrayWithSnakeCaseFields() async throws {
        let server = try await MockDaemonServer.start()
        defer { server.stop() }
        server.respond(
            to: "GET /mounts",
            status: 200,
            body: """
            [{"path":"/Users/alice/Sync","folder_id":"f1","name":"Design Docs","can_write":false,
              "syncing":true,"pending_ops":3,"last_synced_at":null}]
            """
        )

        let client = DaemonClient(portProvider: { server.port })
        let mounts = try await client.mounts()

        XCTAssertEqual(mounts.count, 1)
        XCTAssertEqual(mounts[0].folderId, "f1")
        XCTAssertEqual(mounts[0].name, "Design Docs")
        XCTAssertFalse(mounts[0].canWrite)
        XCTAssertTrue(mounts[0].syncing)
        XCTAssertEqual(mounts[0].pendingOps, 3)
        XCTAssertNil(mounts[0].lastSyncedAt)
    }

    func testNonSuccessStatusThrowsHttpStatusError() async throws {
        let server = try await MockDaemonServer.start()
        defer { server.stop() }
        server.respond(
            to: "POST /fp/share",
            status: 403,
            body: """
            {"error":"read_only_grant"}
            """
        )

        let client = DaemonClient(portProvider: { server.port })

        do {
            _ = try await client.fpShare(nodeId: "n1", invitedEmail: "friend@example.com")
            XCTFail("expected fpShare to throw for a 403 response")
        } catch let DaemonClientError.httpStatus(status, body) {
            XCTAssertEqual(status, 403)
            XCTAssertTrue(body.contains("read_only_grant"))
        }
    }

    func testFpShareEncodesRequestBodyAndDecodesResponse() async throws {
        let server = try await MockDaemonServer.start()
        defer { server.stop() }
        server.respond(
            to: "POST /fp/share",
            status: 200,
            body: """
            {"invite_url":"https://valv.example/invites/abc/accept"}
            """
        )

        let client = DaemonClient(portProvider: { server.port })
        let response = try await client.fpShare(nodeId: "n1", invitedEmail: "friend@example.com")

        XCTAssertEqual(response.inviteUrl, "https://valv.example/invites/abc/accept")
    }

    func testPortResolutionFailureSurfacesAsPortFileUnavailable() async throws {
        let client = DaemonClient(portProvider: {
            throw DaemonClientError.portFileUnavailable
        })

        do {
            _ = try await client.status()
            XCTFail("expected status() to throw when the port cannot be resolved")
        } catch DaemonClientError.portFileUnavailable {
            // expected
        }
    }

    func testUnmountSucceedsWithNoContentResponse() async throws {
        let server = try await MockDaemonServer.start()
        defer { server.stop() }
        server.respond(to: "DELETE /mount", status: 204, body: "")

        let client = DaemonClient(portProvider: { server.port })

        // Should not throw - unmount() discards the (empty) response body.
        try await client.unmount(folderId: "folder-a")
    }

    func testUnmountUnknownFolderThrowsHttpStatusError() async throws {
        let server = try await MockDaemonServer.start()
        defer { server.stop() }
        server.respond(
            to: "DELETE /mount",
            status: 404,
            body: """
            {"error":"mount_not_found"}
            """
        )

        let client = DaemonClient(portProvider: { server.port })

        do {
            try await client.unmount(folderId: "unknown-folder")
            XCTFail("expected unmount to throw for a 404 response")
        } catch let DaemonClientError.httpStatus(status, body) {
            XCTAssertEqual(status, 404)
            XCTAssertTrue(body.contains("mount_not_found"))
        }
    }
}
