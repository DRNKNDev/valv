import AppKit
import SwiftUI

@MainActor
final class ShareWindowController {
    static let shared = ShareWindowController()

    private final class HostWindow: NSWindow {
        override var canBecomeKey: Bool { true }
        override var canBecomeMain: Bool { true }
    }

    private var window: NSWindow?
    private var windowDelegate: WindowDelegate?

    /// Activates the app first (`NSApp.activate(ignoringOtherApps:)`), since `Valv` is
    /// an `LSUIElement` with no Dock icon - matching the app's existing pre-modal
    /// activation rule. Each call replaces any window already showing, since a new
    /// `valv://share` handoff always carries its own `path`/`node`.
    func present(path: String, node: String?) {
        NSApp.activate(ignoringOtherApps: true)
        window?.close()

        let viewModel = ShareWindowViewModel(path: path, node: node)
        let rootView = ShareWindow(viewModel: viewModel)

        let contentSize = CGSize(width: 400, height: 260)
        let hosting = NSHostingView(rootView: rootView)
        hosting.frame = NSRect(origin: .zero, size: contentSize)

        let window = HostWindow(
            contentRect: NSRect(origin: .zero, size: contentSize),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.title = "Share"
        window.isReleasedWhenClosed = false
        window.contentView = hosting
        window.center()

        let delegate = WindowDelegate { [weak self] in
            self?.window = nil
            self?.windowDelegate = nil
        }
        window.delegate = delegate
        self.windowDelegate = delegate

        window.makeKeyAndOrderFront(nil)
        self.window = window
    }

    private final class WindowDelegate: NSObject, NSWindowDelegate {
        let onClose: () -> Void
        init(onClose: @escaping () -> Void) { self.onClose = onClose }
        func windowWillClose(_ notification: Notification) { onClose() }
    }
}
