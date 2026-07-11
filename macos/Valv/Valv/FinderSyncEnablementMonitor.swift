import AppKit
import Combine
import FinderSync

/// Wraps `FIFinderSyncController.isExtensionEnabled`, re-checked when the app becomes
/// active, so the menu-bar dropdown and onboarding's Share explainer page can offer a
/// repair path (`showExtensionManagementInterface()`) when a user disables the Finder
/// Sync extension in System Settings (macos-finder-sync spec). `isExtensionEnabledProvider`
/// is an injectable seam, matching this codebase's existing DI convention, since
/// `FIFinderSyncController`'s members are non-overridable class members that can't
/// otherwise be substituted in a test.
final class FinderSyncEnablementMonitor: ObservableObject {
    static let shared = FinderSyncEnablementMonitor()

    @Published private(set) var isEnabled: Bool

    private let isExtensionEnabledProvider: () -> Bool
    private var cancellable: AnyCancellable?

    init(
        isExtensionEnabledProvider: @escaping () -> Bool = { FIFinderSyncController.isExtensionEnabled },
        activationPublisher: AnyPublisher<Void, Never>? = nil
    ) {
        self.isExtensionEnabledProvider = isExtensionEnabledProvider
        self.isEnabled = isExtensionEnabledProvider()

        let publisher = activationPublisher ?? NotificationCenter.default
            .publisher(for: NSApplication.didBecomeActiveNotification)
            .map { _ in () }
            .eraseToAnyPublisher()
        cancellable = publisher.sink { [weak self] in self?.refresh() }
    }

    func refresh() {
        isEnabled = isExtensionEnabledProvider()
    }

    func showManagementInterface() {
        FIFinderSyncController.showExtensionManagementInterface()
    }
}
