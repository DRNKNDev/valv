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
    @State private var isConfirmingSignOut = false
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
        .alert("Sign out of Valv?", isPresented: $isConfirmingSignOut) {
            Button("Sign Out", role: .destructive) {
                Task { await signOut() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Syncing stops on this Mac. Files already synced here stay in place, and your other devices and collaborators are unaffected.")
        }
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

            Button("Sync Now") {
                Task { await store.syncNow() }
            }
            .buttonStyle(MenuItemButtonStyle())

            if store.isDisconnected {
                Button("Retry") {
                    Task { await store.refresh() }
                }
                .buttonStyle(MenuItemButtonStyle())
            }

            Divider()

            Button("Manage Folders & Sharing...") {
                ManageFoldersWindowController.shared.present(
                    store: store,
                    domainManager: domainManager
                )
            }
            .buttonStyle(MenuItemButtonStyle())

            Divider()

            accountSection

            Divider()

            Button(daemonManager.cliInstallStatus.actionTitle) {
                Task { await daemonManager.installCLI() }
            }
            .buttonStyle(MenuItemButtonStyle())
            .disabled(!daemonManager.cliInstallStatus.isActionable)

            daemonOwnershipLine
                .padding(.horizontal, 14)

            checkForUpdatesRow

            Divider()

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
            if let caption = summaryCaption {
                Text(caption)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
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

    private var summaryCaption: String? {
        if store.isDisconnected {
            guard let lastSuccessAt = store.lastSuccessAt else {
                return "No successful connection yet"
            }
            return "Last connected \(Self.relativeFormatter.localizedString(for: lastSuccessAt, relativeTo: Date()))"
        }
        guard let lastSuccessAt = store.lastSuccessAt else { return nil }
        return "Last checked \(Self.relativeFormatter.localizedString(for: lastSuccessAt, relativeTo: Date()))"
    }

    private static let relativeFormatter: RelativeDateTimeFormatter = {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter
    }()

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

    private var accountSection: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(store.status?.account?.email ?? "Signed in")
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 14)
            Button("Sign Out...") {
                isConfirmingSignOut = true
            }
            .buttonStyle(MenuItemButtonStyle())
            if let signOutError {
                Text(signOutError)
                    .font(.caption2)
                    .foregroundStyle(.red)
                    .padding(.horizontal, 14)
            }
        }
    }

    private var daemonOwnershipLine: some View {
        Group {
            if let version = store.status?.version {
                Text(Self.daemonOwnershipText(
                    version: version,
                    isManagedByValv: daemonManager.isManagedByValv,
                    updateAvailable: store.status?.updateAvailable,
                    latestVersion: store.status?.latestVersion
                ))
                .font(.caption)
                .foregroundStyle(.secondary)
            } else {
                Text("Daemon not connected").font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    static func daemonOwnershipText(
        version: String,
        isManagedByValv: Bool,
        updateAvailable: Bool?,
        latestVersion: String?
    ) -> String {
        let ownership = isManagedByValv ? "managed by Valv" : "managed externally"
        var text = "valvd \(version) - \(ownership)"
        if updateAvailable == true, let latestVersion {
            text += " - Update available (\(latestVersion))"
        }
        return text
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
        updateManager.updateAvailable || store.status?.updateRequired == true
    }

    private var quitSection: some View {
        VStack(alignment: .leading, spacing: 2) {
            Button("Quit Valv") {
                NSApplication.shared.terminate(nil)
            }
            .buttonStyle(MenuItemButtonStyle())
            Text("Syncing continues in the background")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 14)
        }
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
