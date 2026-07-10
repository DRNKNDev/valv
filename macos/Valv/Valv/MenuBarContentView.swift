import AppKit
import DaemonKit
import SwiftUI

private struct MenuItemButtonStyle: ButtonStyle {
    @State private var isHovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .foregroundStyle(isHovering ? Color(nsColor: .selectedMenuItemTextColor) : Color.primary)
            .background {
                RoundedRectangle(cornerRadius: 5)
                    .fill(isHovering ? Color(nsColor: .selectedMenuItemColor) : Color.clear)
            }
            .padding(.horizontal, 4)
            .contentShape(Rectangle())
            .onHover { isHovering = $0 }
    }
}

private struct StatusBadge: View {
    let color: Color

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 8, height: 8)
    }
}

struct MenuBarContentView: View {
    @EnvironmentObject private var store: DaemonStore
    @EnvironmentObject private var daemonManager: DaemonManager
    @EnvironmentObject private var domainManager: FileProviderDomainManager
    @EnvironmentObject private var updateManager: UpdateManager
    @State private var signOutError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if !store.hasSignedIn {
                notSignedInContent
            } else {
                signedInContent
            }
        }
        .padding(.vertical, 6)
        .frame(width: 320)
        .task {
            store.startPolling()
        }
    }


    private var notSignedInContent: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button("Sign In...") {
                OnboardingWindowController.shared.present(
                    store: store,
                    daemonManager: daemonManager,
                    domainManager: domainManager
                )
            }
            .buttonStyle(MenuItemButtonStyle())
            Divider()
            quitSection
        }
    }


    private var signedInContent: some View {
        VStack(alignment: .leading, spacing: 4) {
            summarySection
                .padding(.horizontal, 14)

            if store.hasLapsedPlan {
                lapsedPlanBanner
                    .padding(.horizontal, 14)
                    .padding(.top, 4)
            }

            if let status = store.status, !status.mounts.isEmpty {
                Divider()
                ScrollView {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(status.mounts, id: \.folderId) { mount in
                            MountRow(mount: mount)
                        }
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 2)
                }
                .frame(maxHeight: 320)
            }

            Divider()

            Button("Sync Now") {
                Task { await store.syncNow() }
            }
            .buttonStyle(MenuItemButtonStyle())

            Button(store.status?.paused == true ? "Resume Syncing" : "Pause Syncing") {
                Task {
                    if store.status?.paused == true {
                        await store.resume()
                    } else {
                        await store.pause()
                    }
                }
            }
            .buttonStyle(MenuItemButtonStyle())

            if store.isDisconnected {
                Button("Retry") {
                    Task { await store.refresh() }
                }
                .buttonStyle(MenuItemButtonStyle())
            }

            Button("Manage Folders & Sharing...") {
                ManageFoldersWindowController.shared.present(
                    store: store,
                    domainManager: domainManager
                )
            }
            .buttonStyle(MenuItemButtonStyle())

            Divider()

            checkForUpdatesRow

            Button("Sign Out...") {
                confirmSignOut()
            }
            .buttonStyle(MenuItemButtonStyle())
            if let signOutError {
                Text(signOutError)
                    .font(.caption2)
                    .foregroundStyle(.red)
                    .padding(.horizontal, 14)
            }

            quitSection
        }
    }

    private var summarySection: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                StatusBadge(color: color(for: store.iconState))
                Text(summaryText)
                    .font(.body.weight(.medium))
            }
        }
    }

    private var summaryText: String {
        Self.summaryText(
            status: store.status,
            iconState: store.iconState,
            isDisconnected: store.isDisconnected,
            isRestartingDaemon: daemonManager.isRestartingDaemon
        )
    }

    static func summaryText(
        status: DaemonStatus?,
        iconState: IconState,
        isDisconnected: Bool,
        isRestartingDaemon: Bool = false
    ) -> String {
        if isRestartingDaemon {
            return "Restarting sync service…"
        }
        if isDisconnected {
            return UserFacingError.connectionFailureMessage
        }
        if status?.updateRequired == true || status?.mounts.contains(where: { $0.updateRequired }) == true {
            return "Update Valv to keep syncing"
        }

        switch iconState {
        case .notSetUp: return "Not connected"
        case .error: return "Sync error"
        case .paused: return "Paused"
        case .syncing: return "Syncing..."
        case .synced: return "Up to date"
        }
    }

    private func color(for state: IconState) -> Color {
        switch state {
        case .notSetUp: return .gray
        case .error: return .red
        case .paused: return .yellow
        case .syncing: return .blue
        case .synced: return .green
        }
    }

    private var lapsedPlanBanner: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Billing attention needed")
                .font(.caption)
                .fontWeight(.semibold)
            Text("Sync may be limited until billing is updated.")
                .font(.caption2)
                .foregroundStyle(.secondary)
            Button("Manage Billing") {
                openBilling()
            }
            .font(.caption)
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.yellow.opacity(0.16))
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }

    private func openBilling() {
        guard let url = ConfigReader.read()?.webAccountURL ?? URL(string: "https://valvsync.com/account") else {
            return
        }
        NSWorkspace.shared.open(url)
    }

    private var checkForUpdatesRow: some View {
        Button {
            updateManager.checkForUpdates()
        } label: {
            HStack {
                Text("Check for Updates…")
                Spacer()
                if showsUpdateBadge {
                    StatusBadge(color: .blue)
                }
            }
        }
        .buttonStyle(MenuItemButtonStyle())
    }

    private var showsUpdateBadge: Bool {
        Self.showsUpdateBadge(
            updateAvailable: updateManager.updateAvailable,
            updateRequired: store.status?.updateRequired == true
        )
    }

    static func showsUpdateBadge(updateAvailable: Bool, updateRequired: Bool) -> Bool {
        updateAvailable || updateRequired
    }

    private var quitSection: some View {
        VStack(alignment: .leading, spacing: 2) {
            Button("Quit Valv") {
                NSApplication.shared.terminate(nil)
            }
            .buttonStyle(MenuItemButtonStyle())
            if let daemonFooterText = Self.daemonFooterText(version: store.status?.version) {
                Text(daemonFooterText)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 14)
            }
        }
    }

    static func daemonFooterText(version: String?) -> String? {
        guard let version else { return nil }
        return "valvd \(version) · Syncs after quit"
    }

    private func confirmSignOut() {
        let alert = NSAlert()
        alert.messageText = "Sign out of Valv?"
        alert.informativeText = "Syncing stops on this Mac. Files already synced here stay in place, and your other devices and collaborators are unaffected."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Sign Out").hasDestructiveAction = true
        alert.addButton(withTitle: "Cancel")

        NSApp.activate()
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        Task { await signOut() }
    }

    private func signOut() async {
        do {
            try await store.signOut(domainManager: domainManager)
            signOutError = nil
        } catch {
            NSLog("Sign out failed: %@", error.localizedDescription)
            signOutError = UserFacingError(from: error).message
        }
    }
}

private struct MountRow: View {
    let mount: MountStatus

    var body: some View {
        Button {
            NSWorkspace.shared.open(URL(fileURLWithPath: mount.path))
        } label: {
            HStack(alignment: .top) {
                StatusBadge(color: statusColor)
                    .padding(.top, 5)
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 4) {
                        Text(displayName)
                            .font(.subheadline)
                            .lineLimit(1)
                        if !mount.canWrite {
                            Text("Read Only")
                                .font(.caption2)
                                .padding(.horizontal, 4)
                                .background(Color.secondary.opacity(0.2))
                                .clipShape(Capsule())
                        }
                    }
                    Text(mount.path)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if mount.updateRequired {
                        Text("Update Valv to keep syncing")
                            .font(.caption2)
                            .foregroundStyle(.red)
                            .lineLimit(1)
                    } else if let error = mount.error {
                        Text(error)
                            .font(.caption2)
                            .foregroundStyle(.red)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(displayName)
        .accessibilityValue("\(displayName), \(statusDescription)")
        .help(mount.error ?? "")
    }

    private var displayName: String {
        mount.name.isEmpty ? mount.path : mount.name
    }

    private var statusColor: Color {
        if mount.updateRequired { return .red }
        if mount.error != nil { return .red }
        return mount.syncing ? .blue : .green
    }

    private var statusDescription: String {
        if mount.updateRequired { return "Update required" }
        if mount.error != nil { return "Sync error" }
        if mount.syncing { return "Syncing" }
        return "Up to date"
    }
}
