import Combine
import Foundation

enum OnboardingPage: Int, CaseIterable {
    case welcome
    case daemonSetup
    case signIn
    case firstFolder
    case shareExplainer
    case completion
}

/// Delivers the `valv://auth-callback` URL from Valv's app-level URL handler to whichever
/// onboarding page is listening, and retains the expected state nonce for the
/// current browser sign-in attempt.
final class AuthCallbackCenter: ObservableObject {
    static let shared = AuthCallbackCenter()

    @Published var lastCallback: URL?
    @Published private(set) var expectedState: String?

    func handle(_ url: URL) {
        lastCallback = url
    }

    func beginSignIn(expectedState: String) {
        self.expectedState = expectedState
    }

    func clearExpectedState() {
        expectedState = nil
    }
}

final class OnboardingCoordinator: ObservableObject {
    @Published var currentPage: OnboardingPage = .welcome
    @Published var mountedFolderPath: String?
    @Published var mountedFolderName: String?
    @Published var signInError: String?

    func advance() {
        guard let next = OnboardingPage(rawValue: currentPage.rawValue + 1) else { return }
        currentPage = next
    }

    func goBack() {
        guard let previous = OnboardingPage(rawValue: currentPage.rawValue - 1) else { return }
        currentPage = previous
    }

    func recordMountedFolder(name: String, path: String) {
        mountedFolderName = name
        mountedFolderPath = path
    }
}
