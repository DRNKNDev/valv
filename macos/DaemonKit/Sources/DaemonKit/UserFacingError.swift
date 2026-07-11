import Foundation

public protocol HTTPBodyCarrying {
    var httpStatusAndBody: (Int, String)? { get }
}

public struct UserFacingError: Sendable {
    public static let connectionFailureMessage = "Can't reach Valv's sync service. It may still be starting up."

    public let message: String
    public let detail: String?

    public init(from error: Error) {
        detail = error.localizedDescription

        if let carrying = error as? HTTPBodyCarrying,
           let (status, body) = carrying.httpStatusAndBody {
            message = Self.message(forStatus: status, body: body)
            return
        }

        if error is DaemonClientError {
            message = Self.connectionFailureMessage
            return
        }

        message = "Valv couldn't complete that request. Try again."
    }

    private static func message(forStatus status: Int, body: String) -> String {
        if let code = errorCode(from: body), let mapped = codeMessages[code] {
            return mapped
        }
        if (400 ..< 500).contains(status) {
            return "Valv's server rejected this request."
        }
        if (500 ..< 600).contains(status) {
            return "Valv's server had a problem. Try again in a moment."
        }
        return "Valv couldn't complete that request. Try again."
    }

    private static func errorCode(from body: String) -> String? {
        guard let data = body.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data),
              let dictionary = object as? [String: Any],
              let code = dictionary["error"] as? String
        else {
            return nil
        }
        return code
    }

    private static let codeMessages: [String: String] = [
        "agent_devices_cannot_create_folders": "This device can join folders but can't create new shared folders.",
        "agent_devices_cannot_create_invites": "This device can't invite people to that folder.",
        "device_required": "Sign in on this device before making that change.",
        "folder_not_found": "That folder could not be found.",
        "grant_denied": "You don't have permission to access that folder.",
        "grant_not_found": "That access grant could not be found.",
        "incomplete_grant_route_store": "Valv's server could not update sharing right now.",
        "insufficient_permission": "You don't have permission to make that change.",
        "invalid_invited_email": "Enter a valid email address.",
        "invalid_scope_node_id": "That folder location can't be shared.",
        "invite_exists": "An invite already exists for that recipient.",
        "invite_expired": "That invite link has expired.",
        "invite_not_found": "That invite link could not be found.",
        "invite_not_pending": "That invite link has already been used or revoked.",
        "name_collision": "A file or folder with that name already exists.",
        "no_grant": "You don't have access to that item.",
        "node_not_found": "That file or folder could not be found.",
        "node_not_synced": "This file hasn't finished syncing yet.",
        "not_in_mount": "This file isn't inside a synced folder.",
        "parent_not_found": "The destination folder could not be found.",
        "user_required": "Sign in with a user account to accept this invite.",
        "version_not_found": "That file version could not be found."
    ]
}

extension DaemonClientError: HTTPBodyCarrying {
    public var httpStatusAndBody: (Int, String)? {
        if case .httpStatus(let status, let body) = self {
            return (status, body)
        }
        return nil
    }
}
