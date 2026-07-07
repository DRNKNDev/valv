import AppKit
import SwiftUI

/// Auto-presents onboarding for a not-signed-in user right at launch, instead of
/// requiring them to find and click "Sign In…" in the menu bar first.
/// `applicationDidFinishLaunching` fires exactly once at real process launch,
/// independent of SwiftUI's view lifecycle - `MenuBarExtra`'s content view is loaded
/// lazily on first click, and empirically even its label view's `.task` doesn't fire
/// reliably for a status-item-hosted view, so this is the reliable hook.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        NSAppleEventManager.shared().setEventHandler(
            self,
            andSelector: #selector(handleGetURLEvent(_:withReplyEvent:)),
            forEventClass: AEEventClass(kInternetEventClass),
            andEventID: AEEventID(kAEGetURL)
        )
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard !DaemonStore.shared.hasSignedIn else { return }
        OnboardingWindowController.shared.present(
            store: DaemonStore.shared,
            daemonManager: DaemonManager.shared,
            domainManager: FileProviderDomainManager.shared
        )
    }

    @objc private func handleGetURLEvent(
        _ event: NSAppleEventDescriptor,
        withReplyEvent replyEvent: NSAppleEventDescriptor
    ) {
        guard let rawURL = event.paramDescriptor(forKeyword: keyDirectObject)?.stringValue,
              let url = URL(string: rawURL),
              url.scheme == "valv",
              url.host == "auth-callback" else {
            return
        }

        AuthCallbackCenter.shared.handle(url)
    }
}

@main
struct ValvApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var store = DaemonStore.shared
    @StateObject private var daemonManager = DaemonManager.shared
    @StateObject private var domainManager = FileProviderDomainManager.shared

    var body: some Scene {
        MenuBarExtra {
            MenuBarContentView()
                .environmentObject(store)
                .environmentObject(daemonManager)
                .environmentObject(domainManager)
        } label: {
            Image(systemName: symbolName(for: store.iconState))
        }
        .menuBarExtraStyle(.window)
    }

    private func symbolName(for state: IconState) -> String {
        switch state {
        case .notSetUp: return "externaldrive.badge.questionmark"
        case .error: return "externaldrive.badge.exclamationmark"
        case .paused: return "externaldrive.badge.minus"
        case .syncing: return "externaldrive.badge.icloud"
        case .synced: return "externaldrive"
        }
    }
}
