import AppKit
import SwiftUI

/// Presents `OnboardingContainerView` inside a transparent, card-sized, draggable
/// macOS window - structurally modeled on TourKit's `TourKitWindowController`
/// (github.com/rampatra/TourKit), since that presentational shell (borderless, no
/// title bar, draggable by background, floating level, centered) doesn't depend on
/// TourKit's fixed page-content model at all.
///
/// The window has no title bar, no visible chrome, a transparent background, and is
/// sized to match the onboarding card. It can be dragged around by its background and
/// participates in Mission Control / App Exposé like a normal window.
@MainActor
final class OnboardingWindowController {
    // Matches this codebase's existing convention for cross-cutting concerns that
    // don't need SwiftUI environment-object observation (e.g. `DaemonStore.shared`,
    // `AuthCallbackCenter.shared`) - `MenuBarContentView` calls `.present(...)`
    // directly rather than threading this through the view hierarchy.
    static let shared = OnboardingWindowController()

    private final class HostWindow: NSWindow {
        override var canBecomeKey: Bool { true }
        override var canBecomeMain: Bool { true }
    }

    private var window: NSWindow?
    private var windowDelegate: WindowDelegate?

    /// Fixed size rather than TourKit's per-page auto-measure: our pages have
    /// state-dependent content (e.g. the daemon-setup page swaps between a progress
    /// view and a two-button decision prompt), so pre-measuring every state isn't
    /// worth the complexity for a six-page, first-run-only flow. Sized generously for
    /// the longest page (the first-folder page's create button + text field + link
    /// button + skip link); shorter pages get extra breathing room via `Spacer()`,
    /// exactly like TourKit's own shorter slides.
    private static let cardWidth: CGFloat = OnboardingCardChrome<EmptyView>.cardWidth
    private static let cardHeight: CGFloat = 560

    /// Presents the onboarding window. If a window is already visible, it is brought
    /// to the front instead of creating a duplicate.
    func present(
        store: DaemonStore,
        daemonManager: DaemonManager,
        domainManager: FileProviderDomainManager
    ) {
        if let existing = window {
            existing.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        let dismiss: () -> Void = { [weak self] in
            self?.close()
        }

        let rootView = OnboardingContainerView(onDismiss: dismiss)
            .environmentObject(store)
            .environmentObject(daemonManager)
            .environmentObject(domainManager)

        let contentSize = CGSize(width: Self.cardWidth, height: Self.cardHeight)
        let hosting = NSHostingView(rootView: rootView)
        hosting.frame = NSRect(origin: .zero, size: contentSize)

        let window = HostWindow(
            contentRect: NSRect(origin: .zero, size: contentSize),
            styleMask: [.borderless, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.isOpaque = false
        window.backgroundColor = .clear
        window.hasShadow = true
        window.isMovableByWindowBackground = true
        window.isReleasedWhenClosed = false
        window.level = .floating
        window.collectionBehavior = [.managed, .participatesInCycle, .fullScreenAuxiliary]
        window.contentView = hosting
        window.center()

        let delegate = WindowDelegate { [weak self] in
            self?.window = nil
            self?.windowDelegate = nil
        }
        window.delegate = delegate
        self.windowDelegate = delegate

        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)

        self.window = window
    }

    /// Closes the onboarding window if it is currently presented.
    func close() {
        window?.close()
        window = nil
        windowDelegate = nil
    }

    private final class WindowDelegate: NSObject, NSWindowDelegate {
        let onClose: () -> Void
        init(onClose: @escaping () -> Void) { self.onClose = onClose }
        func windowWillClose(_ notification: Notification) { onClose() }
    }
}
