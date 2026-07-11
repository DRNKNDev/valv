import AppKit
import DaemonKit
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
        guard ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil else { return }

        if DaemonStore.shared.hasSignedIn, let accountId = ConfigReader.read()?.deviceId {
            Task { await reconcileFileProviderDomain(accountId: accountId) }
            return
        }

        DaemonStore.shared.hasSignedIn = false
        OnboardingWindowController.shared.present(
            store: DaemonStore.shared,
            daemonManager: DaemonManager.shared,
            domainManager: FileProviderDomainManager.shared
        )
    }

    private func reconcileFileProviderDomain(accountId: String) async {
        while true {
            await FileProviderDomainManager.shared.registerDomainIfNeeded(accountId: accountId)
            guard let error = FileProviderDomainManager.shared.registrationError else { return }

            let alert = NSAlert()
            alert.messageText = "Couldn't add Valv to Finder"
            alert.informativeText = UserFacingError(from: error).message
            alert.alertStyle = .warning
            alert.addButton(withTitle: "Retry")
            alert.addButton(withTitle: "Cancel")
            NSApp.activate()
            guard alert.runModal() == .alertFirstButtonReturn else { return }
        }
    }

    @objc private func handleGetURLEvent(
        _ event: NSAppleEventDescriptor,
        withReplyEvent replyEvent: NSAppleEventDescriptor
    ) {
        guard let rawURL = event.paramDescriptor(forKeyword: keyDirectObject)?.stringValue,
              let url = URL(string: rawURL),
              url.scheme == "valv" else {
            return
        }

        switch url.host {
        case "auth-callback":
            AuthCallbackCenter.shared.handle(url)
        case "share":
            handleShareURL(url)
        default:
            break
        }
    }

    private func handleShareURL(_ url: URL) {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let path = components.queryItems?.first(where: { $0.name == "path" })?.value else {
            return
        }
        let node = components.queryItems?.first(where: { $0.name == "node" })?.value
        ShareWindowController.shared.present(path: path, node: node)
    }
}

@main
struct ValvApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var store = DaemonStore.shared
    @StateObject private var daemonManager = DaemonManager.shared
    @StateObject private var domainManager = FileProviderDomainManager.shared
    @StateObject private var updateManager = UpdateManager.shared
    @StateObject private var finderSyncMonitor = FinderSyncEnablementMonitor.shared

    var body: some Scene {
        MenuBarExtra {
            MenuBarContentView()
                .environmentObject(store)
                .environmentObject(daemonManager)
                .environmentObject(domainManager)
                .environmentObject(updateManager)
                .environmentObject(finderSyncMonitor)
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
