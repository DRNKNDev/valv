import Combine
import DaemonKit
import SwiftUI

/// The entire onboarding experience: one continuous sequence of 6 pages in a single
/// window, no separate "tour" screen (macos-app spec). Presented via
/// `OnboardingWindowController` as a borderless, draggable, card-sized floating
/// window - not a sheet, not anchored inside the menu bar popover.
struct OnboardingContainerView: View {
    @EnvironmentObject private var store: DaemonStore
    @EnvironmentObject private var daemonManager: DaemonManager
    @EnvironmentObject private var domainManager: FileProviderDomainManager
    @StateObject private var coordinator = OnboardingCoordinator()
    let onDismiss: () -> Void

    var body: some View {
        switch coordinator.currentPage {
        case .welcome:
            WelcomeOnboardingPage(coordinator: coordinator, onDismiss: onDismiss)
        case .daemonSetup:
            DaemonSetupOnboardingPage(coordinator: coordinator, onDismiss: onDismiss)
                .environmentObject(daemonManager)
        case .signIn:
            SignInOnboardingPage(coordinator: coordinator, onDismiss: onDismiss)
                .environmentObject(store)
                .environmentObject(domainManager)
        case .firstFolder:
            FirstFolderOnboardingPage(coordinator: coordinator, onDismiss: onDismiss)
                .environmentObject(store)
                .environmentObject(domainManager)
        case .shareExplainer:
            ShareExplainerOnboardingPage(coordinator: coordinator, onDismiss: onDismiss)
        case .completion:
            CompletionOnboardingPage(coordinator: coordinator, onDismiss: onDismiss)
                .environmentObject(daemonManager)
        }
    }
}

// MARK: - Page 1: Welcome

private struct WelcomeOnboardingPage: View {
    let coordinator: OnboardingCoordinator
    let onDismiss: () -> Void

    var body: some View {
        OnboardingCardChrome(
            metadata: OnboardingPageMetadata(
                imageName: "OnboardingWelcome",
                title: "Welcome to Valv",
                description: "Sync your files across every device, and share them with anyone."
            ),
            pageIndex: OnboardingPage.welcome.rawValue,
            totalPages: OnboardingPage.allCases.count,
            canClose: true,
            onBack: coordinator.goBack,
            onClose: onDismiss
        ) {
            OnboardingPrimaryButton(title: "Continue") { coordinator.advance() }
        }
    }
}

// MARK: - Page 2: Menu-bar explainer + daemon setup (no skip)

private struct DaemonSetupOnboardingPage: View {
    @ObservedObject var coordinator: OnboardingCoordinator
    @EnvironmentObject private var daemonManager: DaemonManager
    let onDismiss: () -> Void

    var body: some View {
        OnboardingCardChrome(
            metadata: OnboardingPageMetadata(
                imageName: "OnboardingDaemonSetup",
                title: "Valv lives in your menu bar",
                description: "Check sync status, pause, and manage folders from the menu bar icon at any time."
            ),
            pageIndex: OnboardingPage.daemonSetup.rawValue,
            totalPages: OnboardingPage.allCases.count,
            canClose: false,
            onBack: coordinator.goBack,
            onClose: onDismiss
        ) {
            if let decision = daemonManager.pendingDecision {
                incompatibleDecisionContent(decision)
            } else {
                ProgressView("Setting up the sync daemon…")
                    .tint(.white)
                    .foregroundStyle(.white.opacity(0.85))
                    .task { await waitForReconciliation() }
            }
        }
    }

    @ViewBuilder
    private func incompatibleDecisionContent(_ decision: DaemonManager.ReconciliationOutcome) -> some View {
        if case .incompatibleNeedsDecision(let path, let version) = decision {
            VStack(spacing: 8) {
                Text("Valv's sync daemon needs updating.")
                    .font(.subheadline).bold()
                    .foregroundStyle(.white)
                Text("It's currently managed outside this app (found at \(path), v\(version)).")
                    .font(.caption)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.white.opacity(0.70))
                HStack {
                    Button("I'll Update It Myself") {
                        daemonManager.declineTakeover()
                        coordinator.advance()
                    }
                    .foregroundStyle(.white)
                    Button("Let Valv.app Manage It") {
                        Task {
                            await daemonManager.consentToTakeover()
                            coordinator.advance()
                        }
                    }
                    .foregroundStyle(.white)
                }
            }
        }
    }

    // reconcileOnLaunch() already ran once from DaemonManager's init; this just waits
    // for that in-flight check to settle, then auto-advances unless it produced a
    // decision that needs the user (macos-app spec: "Common path auto-advances").
    private func waitForReconciliation() async {
        while !daemonManager.hasReconciled {
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        if daemonManager.pendingDecision == nil {
            coordinator.advance()
        }
    }
}

// MARK: - Page 3: Account explainer + sign-in (no skip)

private struct SignInOnboardingPage: View {
    @ObservedObject var coordinator: OnboardingCoordinator
    @EnvironmentObject private var store: DaemonStore
    @EnvironmentObject private var domainManager: FileProviderDomainManager
    @StateObject private var callbackCenter = AuthCallbackCenter.shared
    @State private var isWaiting = false
    let onDismiss: () -> Void

    var body: some View {
        OnboardingCardChrome(
            metadata: OnboardingPageMetadata(
                imageName: "OnboardingSignIn",
                title: "Sign in to your account",
                description: "Valv syncs through your account, so your files follow you to every device."
            ),
            pageIndex: OnboardingPage.signIn.rawValue,
            totalPages: OnboardingPage.allCases.count,
            canClose: false,
            onBack: coordinator.goBack,
            onClose: onDismiss
        ) {
            VStack(spacing: 12) {
                if let error = coordinator.signInError {
                    Text(error).font(.caption).foregroundStyle(.red)
                }

                if isWaiting {
                    ProgressView("Waiting for sign-in…")
                        .tint(.white)
                        .foregroundStyle(.white.opacity(0.85))
                } else {
                    OnboardingPrimaryButton(title: "Continue in Browser") {
                        isWaiting = true
                        NSWorkspace.shared.open(ConfigWriter.loginURL)
                    }
                }
            }
        }
        .onReceive(callbackCenter.$lastCallback.compactMap { $0 }) { url in
            handleCallback(url)
        }
    }

    private func handleCallback(_ url: URL) {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let deviceId = components.queryItems?.first(where: { $0.name == "device_id" })?.value,
              let deviceToken = components.queryItems?.first(where: { $0.name == "device_token" })?.value,
              !deviceId.isEmpty, !deviceToken.isEmpty
        else {
            coordinator.signInError = "Sign-in link was missing required information."
            isWaiting = false
            return
        }

        do {
            try ConfigWriter.write(ConfigWriter.Values(
                backendURL: ConfigWriter.defaultBackendURL,
                deviceId: deviceId,
                deviceToken: deviceToken,
                deviceName: Host.current().localizedName ?? "Valv Device"
            ))
            store.hasSignedIn = true
            Task {
                await restartDaemon()
                await store.refresh()
                // Exactly one NSFileProviderDomain for the whole account (design.md
                // D11) - registered once, here, on first successful sign-in.
                await domainManager.registerDomainIfNeeded(accountId: deviceId)
                coordinator.advance()
            }
        } catch {
            coordinator.signInError = "Couldn't save sign-in details: \(error.localizedDescription)"
            isWaiting = false
        }
    }

    // valvd reads config.toml once at startup and does not hot-reload it (design.md D4).
    private func restartDaemon() async {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        process.arguments = ["kickstart", "-k", "gui/\(getuid())/dev.drnkn.valvd"]
        try? process.run()
        process.waitUntilExit()
        try? await Task.sleep(nanoseconds: 1_000_000_000)
    }
}

// MARK: - Page 4: Finder explainer + first folder (skippable)

private struct FirstFolderOnboardingPage: View {
    @ObservedObject var coordinator: OnboardingCoordinator
    @EnvironmentObject private var store: DaemonStore
    @EnvironmentObject private var domainManager: FileProviderDomainManager
    @State private var folderIdOrLink = ""
    let onDismiss: () -> Void

    var body: some View {
        OnboardingCardChrome(
            metadata: OnboardingPageMetadata(
                imageName: "OnboardingFirstFolder",
                title: "Add your first folder",
                description: "Synced folders appear in Finder under Valv, alongside your other locations."
            ),
            pageIndex: OnboardingPage.firstFolder.rawValue,
            totalPages: OnboardingPage.allCases.count,
            canClose: true,
            onBack: coordinator.goBack,
            onClose: onDismiss
        ) {
            VStack(spacing: 12) {
                OnboardingPrimaryButton(title: "Create a New Synced Folder") { createFolder() }

                HStack {
                    TextField("Folder ID or invite link", text: $folderIdOrLink)
                        .textFieldStyle(.roundedBorder)
                    Button("Link") { linkExistingFolder() }
                        .disabled(folderIdOrLink.trimmingCharacters(in: .whitespaces).isEmpty)
                }

                Button("Skip for now") { coordinator.advance() }
                    .buttonStyle(.plain)
                    .foregroundStyle(.white.opacity(0.60))
            }
        }
    }

    private func createFolder() {
        Task {
            let panel = NSOpenPanel()
            panel.canChooseDirectories = true
            panel.canChooseFiles = false
            panel.prompt = "Select"
            guard panel.runModal() == .OK, let url = panel.url else { return }
            if let response = try? await store.mount(MountRequest(path: url.path)) {
                coordinator.recordMountedFolder(name: url.lastPathComponent, path: response.path)
            }
            await domainManager.signalRootEnumerator()
            coordinator.advance()
        }
    }

    private func linkExistingFolder() {
        Task {
            let panel = NSOpenPanel()
            panel.canChooseDirectories = true
            panel.canChooseFiles = false
            panel.prompt = "Select"
            guard panel.runModal() == .OK, let url = panel.url else { return }

            let value = folderIdOrLink.trimmingCharacters(in: .whitespaces)
            // "Accept-invite link" resolves to a grant_token mount; a bare folder ID
            // resolves to the folder_id case (macos-app spec, page 4).
            let request: MountRequest
            if let linkURL = URL(string: value), let token = URLComponents(url: linkURL, resolvingAgainstBaseURL: false)?
                .queryItems?.first(where: { $0.name == "token" || $0.name == "grant_token" })?.value {
                request = MountRequest(path: url.path, grantToken: token)
            } else {
                request = MountRequest(path: url.path, folderId: value)
            }

            if let response = try? await store.mount(request) {
                coordinator.recordMountedFolder(name: url.lastPathComponent, path: response.path)
            }
            await domainManager.signalRootEnumerator()
            coordinator.advance()
        }
    }
}

// MARK: - Page 5: Share explainer (no action, no skip needed - just Continue)

private struct ShareExplainerOnboardingPage: View {
    let coordinator: OnboardingCoordinator
    let onDismiss: () -> Void

    var body: some View {
        OnboardingCardChrome(
            metadata: OnboardingPageMetadata(
                imageName: "OnboardingShareExplainer",
                title: "Share with anyone",
                description: "Right-click any file or folder in Finder and choose Share… to invite someone, with read-only or read-write access."
            ),
            pageIndex: OnboardingPage.shareExplainer.rawValue,
            totalPages: OnboardingPage.allCases.count,
            canClose: true,
            onBack: coordinator.goBack,
            onClose: onDismiss
        ) {
            OnboardingPrimaryButton(title: "Continue") { coordinator.advance() }
        }
    }
}

// MARK: - Page 6: Completion

private struct CompletionOnboardingPage: View {
    @ObservedObject var coordinator: OnboardingCoordinator
    @EnvironmentObject private var daemonManager: DaemonManager
    let onDismiss: () -> Void

    private var description: String {
        if let name = coordinator.mountedFolderName {
            return "\"\(name)\" is syncing."
        }
        return "You can add a folder anytime from the menu bar."
    }

    var body: some View {
        OnboardingCardChrome(
            metadata: OnboardingPageMetadata(
                imageName: "OnboardingCompletion",
                title: "You're all set",
                description: description
            ),
            pageIndex: OnboardingPage.completion.rawValue,
            totalPages: OnboardingPage.allCases.count,
            canClose: true,
            onBack: coordinator.goBack,
            onClose: onDismiss
        ) {
            VStack(spacing: 12) {
                if let path = coordinator.mountedFolderPath {
                    Button("Show Me in Finder") {
                        NSWorkspace.shared.selectFile(path, inFileViewerRootedAtPath: "")
                    }
                    .foregroundStyle(.white)
                }

                Button(daemonManager.cliInstallStatus.actionTitle) {
                    Task { await daemonManager.installCLI() }
                }
                .disabled(!daemonManager.cliInstallStatus.isActionable)
                .buttonStyle(.plain)
                .font(.caption)
                .foregroundStyle(.white.opacity(0.60))

                OnboardingPrimaryButton(title: "Done") { onDismiss() }
            }
        }
    }
}
