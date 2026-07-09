import XCTest

@testable import DaemonKit

/// Minimal raw-HTTP transport for `DaemonClient` tests. It avoids opening a real
/// listener while still exercising request encoding and HTTP response parsing.
final class MockDaemonTransport: @unchecked Sendable {
    private let queue = DispatchQueue(label: "MockDaemonTransport")
    private var responsesByRequestLine: [String: (status: Int, body: String)] = [:]
    private var requestBodiesByRequestLine: [String: [Data]] = [:]

    func respond(to requestLine: String, status: Int, body: String) {
        queue.sync {
            responsesByRequestLine[requestLine] = (status, body)
        }
    }

    func requestBodies(for requestLine: String) -> [Data] {
        queue.sync {
            requestBodiesByRequestLine[requestLine] ?? []
        }
    }

    func client() -> DaemonClient {
        DaemonClient(portProvider: { 0 }, rawTransport: { [self] method, path, body, _ in
            let key = "\(method) \(path)"
            let response = queue.sync {
                requestBodiesByRequestLine[key, default: []].append(body ?? Data())
                return responsesByRequestLine[key] ?? (404, "{\"error\":\"not_found\"}")
            }
            return Self.rawHTTPResponse(status: response.status, body: response.body)
        })
    }

    private static func rawHTTPResponse(status: Int, body: String) -> Data {
        let statusText = status == 200 ? "OK" : "Error"
        let responseText = "HTTP/1.1 \(status) \(statusText)\r\n"
            + "Content-Type: application/json\r\n"
            + "Content-Length: \(body.utf8.count)\r\n"
            + "Connection: close\r\n\r\n"
            + body
        return Data(responseText.utf8)
    }
}

final class DaemonClientTests: XCTestCase {
    func testStatusDecodesSuccessfully() async throws {
        let transport = MockDaemonTransport()
        transport.respond(
            to: "GET /status",
            status: 200,
            body: """
            {"paused":false,"backend_connected":true,"version":"1.2.3","update_required":false,"mounts":[]}
            """
        )

        let status = try await transport.client().status()

        XCTAssertFalse(status.paused)
        XCTAssertTrue(status.backendConnected)
        XCTAssertEqual(status.version, "1.2.3")
        XCTAssertEqual(status.mounts, [])
    }

    func testStatusDecodesLatestVersionAndUpdateAvailableWhenPresent() async throws {
        let transport = MockDaemonTransport()
        transport.respond(
            to: "GET /status",
            status: 200,
            body: """
            {"paused":false,"backend_connected":true,"version":"1.2.3","update_required":false,
             "mounts":[],"update_available":true,"latest_version":"1.2.3"}
            """
        )

        let status = try await transport.client().status()

        XCTAssertEqual(status.updateAvailable, true)
        XCTAssertEqual(status.latestVersion, "1.2.3")
    }

    func testStatusDecodesUpdateAvailableFalse() async throws {
        let transport = MockDaemonTransport()
        transport.respond(
            to: "GET /status",
            status: 200,
            body: """
            {"paused":false,"backend_connected":true,"version":"1.2.3","update_required":false,
             "mounts":[],"update_available":false,"latest_version":"1.2.3"}
            """
        )

        let status = try await transport.client().status()

        XCTAssertEqual(status.updateAvailable, false)
    }

    func testStatusDecodesNilLatestVersionAndUpdateAvailableWhenAbsent() async throws {
        let transport = MockDaemonTransport()
        transport.respond(
            to: "GET /status",
            status: 200,
            body: """
            {"paused":false,"backend_connected":true,"version":"1.2.3","update_required":false,"mounts":[]}
            """
        )

        let status = try await transport.client().status()

        XCTAssertNil(status.updateAvailable)
        XCTAssertNil(status.latestVersion)
    }

    func testMountsDecodesArrayWithSnakeCaseFields() async throws {
        let transport = MockDaemonTransport()
        transport.respond(
            to: "GET /mounts",
            status: 200,
            body: """
            [{"path":"/Users/alice/Sync","folder_id":"f1","name":"Design Docs","can_write":false,
              "syncing":true,"pending_ops":3,"last_synced_at":null,"update_required":false}]
            """
        )

        let mounts = try await transport.client().mounts()

        XCTAssertEqual(mounts.count, 1)
        XCTAssertEqual(mounts[0].folderId, "f1")
        XCTAssertEqual(mounts[0].name, "Design Docs")
        XCTAssertFalse(mounts[0].canWrite)
        XCTAssertTrue(mounts[0].syncing)
        XCTAssertEqual(mounts[0].pendingOps, 3)
        XCTAssertNil(mounts[0].lastSyncedAt)
    }

    func testNonSuccessStatusThrowsHttpStatusError() async throws {
        let transport = MockDaemonTransport()
        transport.respond(
            to: "POST /fp/share",
            status: 403,
            body: """
            {"error":"read_only_grant"}
            """
        )

        do {
            _ = try await transport.client().fpShare(nodeId: "n1", invitedEmail: "friend@example.com")
            XCTFail("expected fpShare to throw for a 403 response")
        } catch let DaemonClientError.httpStatus(status, body) {
            XCTAssertEqual(status, 403)
            XCTAssertTrue(body.contains("read_only_grant"))
        }
    }

    func testFpShareEncodesRequestBodyAndDecodesResponse() async throws {
        let transport = MockDaemonTransport()
        transport.respond(
            to: "POST /fp/share",
            status: 200,
            body: """
            {"invite_url":"https://valv.example/invites/abc/accept"}
            """
        )

        let response = try await transport.client().fpShare(
            nodeId: "n1",
            invitedEmail: "friend@example.com"
        )

        XCTAssertEqual(response.inviteUrl, "https://valv.example/invites/abc/accept")
    }

    func testFpMoveEncodesRequestBodyAndDecodesResponse() async throws {
        let transport = MockDaemonTransport()
        transport.respond(
            to: "POST /fp/move",
            status: 200,
            body: """
            {"node_id":"n1","server_seq":8}
            """
        )

        let response = try await transport.client().fpMove(
            nodeId: "n1",
            basedOnSeq: 7,
            newName: "renamed.txt",
            newParentId: "p2"
        )

        XCTAssertEqual(response.nodeId, "n1")
        XCTAssertEqual(response.serverSeq, 8)
        let bodies = transport.requestBodies(for: "POST /fp/move")
        XCTAssertEqual(bodies.count, 1)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: bodies[0]) as? [String: Any])
        XCTAssertEqual(json["node_id"] as? String, "n1")
        XCTAssertEqual(json["based_on_seq"] as? Int, 7)
        XCTAssertEqual(json["new_name"] as? String, "renamed.txt")
        XCTAssertEqual(json["new_parent_id"] as? String, "p2")
    }

    func testFpMoveConflictBodyIsPreservedForCallers() async throws {
        let transport = MockDaemonTransport()
        transport.respond(
            to: "POST /fp/move",
            status: 409,
            body: """
            {"error":"superseded","current_seq":10}
            """
        )

        do {
            _ = try await transport.client().fpMove(
                nodeId: "n1",
                basedOnSeq: 7,
                newName: "renamed.txt",
                newParentId: nil
            )
            XCTFail("expected fpMove to throw for a 409 response")
        } catch let DaemonClientError.httpStatus(status, body) {
            XCTAssertEqual(status, 409)
            XCTAssertTrue(body.contains("\"error\":\"superseded\""))
            XCTAssertTrue(body.contains("\"current_seq\":10"))
        }
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
        let transport = MockDaemonTransport()
        transport.respond(to: "DELETE /mount", status: 204, body: "")

        try await transport.client().unmount(folderId: "folder-a")
    }

    func testUnmountUnknownFolderThrowsHttpStatusError() async throws {
        let transport = MockDaemonTransport()
        transport.respond(
            to: "DELETE /mount",
            status: 404,
            body: """
            {"error":"mount_not_found"}
            """
        )

        do {
            try await transport.client().unmount(folderId: "unknown-folder")
            XCTFail("expected unmount to throw for a 404 response")
        } catch let DaemonClientError.httpStatus(status, body) {
            XCTAssertEqual(status, 404)
            XCTAssertTrue(body.contains("mount_not_found"))
        }
    }
}
