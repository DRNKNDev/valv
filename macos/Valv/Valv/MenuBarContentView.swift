import DaemonKit
import SwiftUI

/// Mimics a native `NSMenuItem`'s appearance for action rows in this custom
/// `.menuBarExtraStyle(.window)` popover: full width, no button chrome, and a
/// hover highlight in the real system menu-selection color - `NSColor
/// .selectedMenuItemColor` is the actual AppKit color semantically meant for this
/// ("the color to use for the face of selected menu items"), not a guessed value.
/// Deliberately not `.menuBarExtraStyle(.menu)` (a real native `NSMenu`) - that style
/// is documented to ignore custom shapes/views (the colored status `Circle()`s,
/// `MountRow`'s two-line layout) and would force `ManageFoldersWindow`'s `.sheet()`
/// to become a standalone window, none of which this pass is trying to change.
private struct MenuItemButtonStyle: ButtonStyle {
    @State private var isHovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 14)
            .padding(.vertical, 4)
            .foregroundStyle(isHovering ? Color.white : Color.primary)
            .background(isHovering ? Color(nsColor: .selectedMenuItemColor) : Color.clear)
            .contentShape(Rectangle())
            .onHover { isHovering = $0 }
    }
}

struct MenuBarContentView: View {
    @EnvironmentObject private var store: DaemonStore
    @EnvironmentObject private var daemonManager: DaemonManager
    @EnvironmentObject private var domainManager: FileProviderDomainManager
    @State private var showManageFolders = false
    // A real `Menu` doesn't propagate outer SwiftUI frame sizing to its actual
    // clickable/hoverable region on macOS - `.menuStyle(.borderlessButton)` plus
    // `.frame(maxWidth: .infinity)` still renders and hit-tests at the label's
    // intrinsic content size, not full row width, unlike a plain `Button`. "Add
    // Folder" is a `Button` that expands its three choices inline instead, so it
    // gets the exact same `MenuItemButtonStyle` treatment as every other row.
    @State private var showAddFolderOptions = false

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
        .sheet(isPresented: $showManageFolders) {
            ManageFoldersWindow()
                .environmentObject(store)
        }
        .task {
            store.startPolling()
        }
    }

    // MARK: - Not signed in (collapsed state)

    private var notSignedInContent: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button("Sign In…") {
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

    // MARK: - Signed in

    private var signedInContent: some View {
        VStack(alignment: .leading, spacing: 4) {
            summaryLine
                .padding(.horizontal, 14)

            if store.hasLapsedPlan {
                lapsedPlanBanner
                    .padding(.horizontal, 14)
                    .padding(.top, 4)
            }

            if let status = store.status, !status.mounts.isEmpty {
                Divider()
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(status.mounts, id: \.folderId) { mount in
                        MountRow(mount: mount)
                    }
                }
                .padding(.horizontal, 14)
            }

            Divider()

            Button(store.status?.paused == true ? "Resume" : "Pause") {
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

            Divider()

            Button {
                withAnimation(.easeInOut(duration: 0.15)) {
                    showAddFolderOptions.toggle()
                }
            } label: {
                HStack {
                    Text("Add Folder")
                    Spacer()
                    Image(systemName: showAddFolderOptions ? "chevron.down" : "chevron.right")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            .buttonStyle(MenuItemButtonStyle())

            if showAddFolderOptions {
                Button("Create a New Synced Folder…") {
                    showAddFolderOptions = false
                    addNewFolder()
                }
                .buttonStyle(MenuItemButtonStyle())

                Button("Mount Existing Folder by ID…") {
                    showAddFolderOptions = false
                    pickDirectoryThenPrompt(fieldLabel: "Folder ID") { path, folderId in
                        MountRequest(path: path, folderId: folderId)
                    }
                }
                .buttonStyle(MenuItemButtonStyle())

                Button("Mount from Invite/Grant Token…") {
                    showAddFolderOptions = false
                    pickDirectoryThenPrompt(fieldLabel: "Grant Token") { path, token in
                        MountRequest(path: path, grantToken: token)
                    }
                }
                .buttonStyle(MenuItemButtonStyle())
            }

            Button("Manage Folders & Sharing…") {
                showManageFolders = true
            }
            .buttonStyle(MenuItemButtonStyle())

            Divider()

            daemonOwnershipLine
                .padding(.horizontal, 14)

            Button(daemonManager.cliInstallStatus.actionTitle) {
                Task { await daemonManager.installCLI() }
            }
            .buttonStyle(MenuItemButtonStyle())
            .disabled(!daemonManager.cliInstallStatus.isActionable)

            Divider()

            quitSection
        }
    }

    private var summaryLine: some View {
        HStack {
            Circle()
                .fill(color(for: store.iconState))
                .frame(width: 8, height: 8)
            Text(summaryText)
                .font(.subheadline)
        }
    }

    private var summaryText: String {
        switch store.iconState {
        case .notSetUp: return "Not connected"
        case .error: return "Sync error"
        case .paused: return "Paused"
        case .syncing: return "Syncing…"
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

    private var daemonOwnershipLine: some View {
        Group {
            if let version = store.status?.version {
                Text(daemonManager.isManagedByValv
                    ? "valvd \(version) - managed by Valv"
                    : "valvd \(version) - managed externally")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                Text("Daemon not connected").font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    // Quit only ends the GUI process - valvd is a launchd-managed service independent
    // of this app's lifecycle and keeps running/syncing after this (macos-app spec).
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

    private func addNewFolder() {
        Task {
            guard let url = pickDirectory() else { return }
            _ = try? await store.mount(MountRequest(path: url.path))
            await domainManager.signalRootEnumerator()
        }
    }

    /// Shared by the "mount existing folder by ID" and "mount from grant token" cases -
    /// both need a local directory to mount at, exactly like `valv-cli mount <path>`
    /// does, plus one text value (folder ID or grant token respectively).
    private func pickDirectoryThenPrompt(fieldLabel: String, makeRequest: @escaping (String, String) -> MountRequest) {
        Task {
            guard let url = pickDirectory() else { return }
            guard let value = promptForText(title: fieldLabel, message: "Enter the \(fieldLabel.lowercased()) to mount at \(url.path):") else {
                return
            }
            _ = try? await store.mount(makeRequest(url.path, value))
            await domainManager.signalRootEnumerator()
        }
    }

    private func pickDirectory() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Select"
        guard panel.runModal() == .OK else { return nil }
        return panel.url
    }

    private func promptForText(title: String, message: String) -> String? {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.addButton(withTitle: "Mount")
        alert.addButton(withTitle: "Cancel")

        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        alert.accessoryView = field
        alert.window.initialFirstResponder = field

        guard alert.runModal() == .alertFirstButtonReturn else { return nil }
        let value = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }
}

private struct MountRow: View {
    let mount: MountStatus

    var body: some View {
        HStack {
            Circle()
                .fill(mount.error != nil ? Color.red : (mount.syncing ? Color.blue : Color.green))
                .frame(width: 6, height: 6)
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 4) {
                    Text(mount.name.isEmpty ? mount.path : mount.name)
                        .font(.subheadline)
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
            }
            Spacer()
        }
        .help(mount.error ?? "")
    }
}
