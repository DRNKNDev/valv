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

/// Delivers the `valv://auth-callback` URL from `ValvApp`'s `.onOpenURL` to whichever
/// onboarding page is listening, without threading the URL through every view in
/// between.
final class AuthCallbackCenter: ObservableObject {
    static let shared = AuthCallbackCenter()

    @Published var lastCallback: URL?

    func handle(_ url: URL) {
        lastCallback = url
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
