import Combine
import DaemonKit
import Foundation

/// Backs `ShareWindow`. `resolveNode`/`submitShare` are injectable seams (matching
/// this codebase's existing DI convention, e.g. `DaemonStore`'s
/// `restartDaemonOperation` or `FileProviderDomainManager`'s `addDomain`) so tests can
/// exercise the resolution/submit/error-mapping logic without a real `DaemonClient`
/// network call.
@MainActor
final class ShareWindowViewModel: ObservableObject {
    enum Resolution {
        case resolving
        case resolved(nodeId: String)
        case failed(message: String)
    }

    let path: String

    @Published private(set) var resolution: Resolution
    @Published var email = ""
    @Published var canWrite = true
    @Published private(set) var statusMessage: String?
    @Published private(set) var isError = false
    @Published private(set) var inviteURL: String?
    @Published private(set) var isSubmitting = false

    private let resolveNode: (String) async throws -> String
    private let submitShare: (String, String, Bool) async throws -> FpShareResponse

    init(
        path: String,
        node: String?,
        resolveNode: @escaping (String) async throws -> String = { path in
            try await DaemonClient().nodeIdForPath(path).nodeId
        },
        submitShare: @escaping (String, String, Bool) async throws -> FpShareResponse = { nodeId, email, canWrite in
            try await DaemonClient().fpShare(nodeId: nodeId, invitedEmail: email, canWrite: canWrite)
        }
    ) {
        self.path = path
        self.resolveNode = resolveNode
        self.submitShare = submitShare
        self.resolution = node.map { .resolved(nodeId: $0) } ?? .resolving
    }

    var canSubmit: Bool {
        guard case .resolved = resolution, !isSubmitting else {
            return false
        }
        return !email.trimmingCharacters(in: .whitespaces).isEmpty
    }

    func resolveIfNeeded() async {
        guard case .resolving = resolution else {
            return
        }
        do {
            let nodeId = try await resolveNode(path)
            resolution = .resolved(nodeId: nodeId)
        } catch {
            resolution = .failed(message: UserFacingError(from: error).message)
        }
    }

    func submit() async {
        guard case .resolved(let nodeId) = resolution else {
            return
        }
        let trimmedEmail = email.trimmingCharacters(in: .whitespaces)
        guard !trimmedEmail.isEmpty else {
            statusMessage = "Enter an email address."
            isError = true
            return
        }

        isSubmitting = true
        statusMessage = nil
        isError = false
        do {
            let response = try await submitShare(nodeId, trimmedEmail, canWrite)
            inviteURL = response.inviteUrl
            statusMessage = "Invite created."
            isError = false
        } catch {
            statusMessage = UserFacingError(from: error).message
            isError = true
        }
        isSubmitting = false
    }
}
