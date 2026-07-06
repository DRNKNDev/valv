import DaemonKit
import SwiftUI

struct ManageFoldersWindow: View {
    @EnvironmentObject private var store: DaemonStore
    @EnvironmentObject private var domainManager: FileProviderDomainManager
    @State private var selectedFolderId: String?
    @State private var grants: [GrantEntry] = []
    @State private var resolvedScopePaths: [String: String] = [:]
    @State private var showInviteSheet = false
    @State private var showAddDeviceSheet = false
    @State private var revokeTarget: GrantEntry?
    @State private var removeTarget: MountStatus?
    @State private var loadError: String?
    @State private var isLoadingGrants = false
    @State private var isResolvingScopes = false

    private let backendClient = BackendClient()

    private var mounts: [MountStatus] {
        store.status?.mounts ?? []
    }

    private var selectedMount: MountStatus? {
        mounts.first { $0.folderId == selectedFolderId }
    }

    private var grantsForSelectedFolder: [GrantEntry] {
        guard let selectedFolderId else { return [] }
        return grants.filter { $0.folderId == selectedFolderId }
    }

    private var removeAlertTitle: String {
        guard let removeTarget else {
            return "Stop syncing this folder on this Mac?"
        }
        return "Stop syncing '\(displayName(for: removeTarget))' on this Mac?"
    }

    var body: some View {
        NavigationSplitView {
            sidebar
        } detail: {
            if let mount = selectedMount {
                detailPane(for: mount)
            } else {
                emptyState
            }
        }
        .frame(minWidth: 700, minHeight: 450)
        .task {
            await loadGrants()
            if selectedFolderId == nil {
                selectedFolderId = mounts.first?.folderId
            }
        }
        .sheet(isPresented: $showInviteSheet) {
            if let mount = selectedMount {
                InviteSheet(mount: mount, backendClient: backendClient, onCompleted: {
                    Task { await loadGrants() }
                })
            }
        }
        .sheet(isPresented: $showAddDeviceSheet) {
            if let mount = selectedMount {
                AddDeviceSheet(mount: mount, backendClient: backendClient, onCompleted: {
                    Task { await loadGrants() }
                })
            }
        }
        .alert("Revoke Access?", isPresented: .constant(revokeTarget != nil), presenting: revokeTarget) { grant in
            Button("Revoke", role: .destructive) {
                Task { await revoke(grant) }
            }
            Button("Cancel", role: .cancel) { revokeTarget = nil }
        } message: { grant in
            Text("This removes access for \(grant.granteeEmail ?? grant.deviceName ?? "this grant") on \(selectedMount?.name ?? "this folder").")
        }
        .alert(removeAlertTitle, isPresented: .constant(removeTarget != nil), presenting: removeTarget) { mount in
            Button("Stop Syncing", role: .destructive) {
                removeMount(mount)
            }
            Button("Cancel", role: .cancel) { removeTarget = nil }
        } message: { mount in
            Text("The folder keeps syncing on your other devices, and files already on this Mac stay where they are.")
        }
    }

    private var sidebar: some View {
        List {
            ForEach(mounts, id: \.folderId) { mount in
                Button {
                    selectedFolderId = mount.folderId
                } label: {
                    SidebarMountRow(mount: mount)
                }
                .buttonStyle(.plain)
                .listRowBackground(selectedFolderId == mount.folderId ? Color.accentColor.opacity(0.16) : Color.clear)
            }
        }
        .toolbar {
            ToolbarItem {
                Menu {
                    Button("Create a New Synced Folder...") { createFolder() }
                    Button("Link Existing Folder by ID...") { linkFolder(byId: true) }
                    Button("Mount via Invite Link...") { linkFolder(byId: false) }
                } label: {
                    Label("Add Folder", systemImage: "plus")
                }
            }
        }
        .navigationSplitViewColumnWidth(min: 180, ideal: 220)
    }

    private var emptyState: some View {
        VStack(spacing: 12) {
            Text("No folders mounted").font(.title3)
            Text("Add a folder to get started.").foregroundStyle(.secondary)
            HStack {
                Button("Create a New Synced Folder...") { createFolder() }
                Button("Link Existing Folder by ID...") { linkFolder(byId: true) }
                Button("Mount via Invite Link...") { linkFolder(byId: false) }
            }
        }
        .padding()
    }

    private func detailPane(for mount: MountStatus) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text(displayName(for: mount))
                    .font(.title2)
                    .bold()
                    .lineLimit(1)
                    .truncationMode(.tail)
                Text(mount.path).font(.caption).foregroundStyle(.secondary)
                if let error = mount.error {
                    Text(error).font(.caption).foregroundStyle(.red)
                }
            }

            HStack {
                Button("Sync Now") {
                    Task { await store.syncNow(folderId: mount.folderId) }
                }
                Button("Remove from this Mac", role: .destructive) {
                    removeTarget = mount
                }
            }

            Divider()

            HStack {
                Text("Shared With").font(.headline)
                Spacer()
                Button("Invite...") { showInviteSheet = true }
                    .buttonStyle(.borderedProminent)
                Button("Add Device...") { showAddDeviceSheet = true }
            }

            if let loadError {
                Text(loadError).font(.caption).foregroundStyle(.red)
            }

            grantsTable(for: mount)

            Spacer()
        }
        .padding()
        .task(id: mount.folderId) {
            await resolveScopes(for: grantsForSelectedFolder)
        }
    }

    @ViewBuilder
    private func grantsTable(for mount: MountStatus) -> some View {
        if isLoadingGrants || isResolvingScopes {
            HStack(spacing: 8) {
                ProgressView()
                Text("Loading sharing access...")
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, minHeight: 120)
        } else if grantsForSelectedFolder.isEmpty {
            Text("Not shared with anyone yet")
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, minHeight: 120)
        } else {
            Table(grantsForSelectedFolder) {
                TableColumn("Grantee") { grant in
                    Text(grant.granteeEmail ?? grant.deviceName ?? "Unknown")
                        .lineLimit(1)
                }
                .width(min: 180, ideal: 260)

                TableColumn("Scope") { grant in
                    Text(scopeLabel(for: grant, mount: mount))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .width(min: 120, ideal: 180)

                TableColumn("Access") { grant in
                    Text("\(grant.role.capitalized) · \(grant.canWrite ? "Read & Write" : "Read Only")")
                        .lineLimit(1)
                }
                .width(min: 130, ideal: 160)

                TableColumn("") { grant in
                    Button("Revoke") { revokeTarget = grant }
                }
                .width(min: 70, ideal: 80, max: 90)
            }
        }
    }

    // "" is the daemon's own documented resolution for a mount's root/scope node
    // itself (ipc-control-api spec: "The root/scope node itself SHALL resolve to ''"),
    // so an empty resolved path means this grant covers the entire folder.
    private func scopeLabel(for grant: GrantEntry, mount: MountStatus) -> String {
        guard let resolved = resolvedScopePaths[grant.scopeNodeId] else {
            return "Subfolder"
        }
        return resolved.isEmpty ? "Entire Folder" : resolved
    }

    private func loadGrants() async {
        isLoadingGrants = true
        defer { isLoadingGrants = false }
        do {
            grants = try await backendClient.grants()
            loadError = nil
        } catch {
            NSLog("ManageFoldersWindow loadGrants failed: %@", error.localizedDescription)
            loadError = UserFacingError(from: error).message
        }
    }

    private func resolveScopes(for entries: [GrantEntry]) async {
        let unresolved = entries.filter { resolvedScopePaths[$0.scopeNodeId] == nil }
        guard !unresolved.isEmpty else { return }
        isResolvingScopes = true
        defer { isResolvingScopes = false }
        for grant in unresolved {
            do {
                let path = try await store.nodePath(nodeId: grant.scopeNodeId)
                resolvedScopePaths[grant.scopeNodeId] = path
            } catch {
                resolvedScopePaths[grant.scopeNodeId] = "Subfolder"
            }
        }
    }

    private func revoke(_ grant: GrantEntry) async {
        revokeTarget = nil
        do {
            try await backendClient.revokeGrant(folderId: grant.folderId, grantId: grant.grantId)
            loadError = nil
        } catch {
            NSLog("ManageFoldersWindow revoke failed: %@", error.localizedDescription)
            loadError = UserFacingError(from: error).message
        }
        await loadGrants()
    }

    private func removeMount(_ mount: MountStatus) {
        removeTarget = nil
        Task {
            do {
                try await store.unmount(folderId: mount.folderId)
                if selectedFolderId == mount.folderId {
                    selectedFolderId = store.status?.mounts.first?.folderId
                }
                loadError = nil
                await domainManager.signalRootEnumerator()
            } catch {
                NSLog("ManageFoldersWindow removeMount failed: %@", error.localizedDescription)
                loadError = UserFacingError(from: error).message
            }
        }
    }

    private func createFolder() {
        Task {
            NSApp.activate(ignoringOtherApps: true)
            let panel = NSOpenPanel()
            panel.canChooseDirectories = true
            panel.canChooseFiles = false
            panel.prompt = "Select"
            guard panel.runModal() == .OK, let url = panel.url else { return }
            do {
                let response = try await store.mount(MountRequest(path: url.path))
                selectedFolderId = response.folderId
                loadError = nil
            } catch {
                NSLog("ManageFoldersWindow createFolder failed: %@", error.localizedDescription)
                loadError = UserFacingError(from: error).message
            }
            await domainManager.signalRootEnumerator()
        }
    }

    private func linkFolder(byId: Bool) {
        Task {
            NSApp.activate(ignoringOtherApps: true)
            let panel = NSOpenPanel()
            panel.canChooseDirectories = true
            panel.canChooseFiles = false
            panel.prompt = "Select"
            guard panel.runModal() == .OK, let url = panel.url else { return }

            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.messageText = byId ? "Folder ID" : "Invite Link"
            alert.informativeText = byId
                ? "Paste the folder ID from the folder owner or another Valv device."
                : "Paste the invite link you received. A raw grant token also works."
            alert.addButton(withTitle: "Add")
            alert.addButton(withTitle: "Cancel")
            let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 300, height: 24))
            alert.accessoryView = field
            alert.window.initialFirstResponder = field
            guard alert.runModal() == .alertFirstButtonReturn else { return }
            let value = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !value.isEmpty else { return }

            let request = byId
                ? MountRequest(path: url.path, folderId: value)
                : MountRequest(path: url.path, grantToken: inviteToken(from: value))
            do {
                let response = try await store.mount(request)
                selectedFolderId = response.folderId
                loadError = nil
            } catch {
                NSLog("ManageFoldersWindow linkFolder failed: %@", error.localizedDescription)
                loadError = UserFacingError(from: error).message
            }
            await domainManager.signalRootEnumerator()
        }
    }

    private func inviteToken(from value: String) -> String {
        guard let url = URL(string: value),
              let token = URLComponents(url: url, resolvingAgainstBaseURL: false)?
            .queryItems?.first(where: { $0.name == "token" || $0.name == "grant_token" })?.value
        else {
            return value
        }
        return token
    }

    private func displayName(for mount: MountStatus) -> String {
        mount.name.isEmpty ? mount.path : mount.name
    }

    private func statusColor(for mount: MountStatus) -> Color {
        if mount.error != nil { return .red }
        return mount.syncing ? .blue : .green
    }

    private func statusDescription(for mount: MountStatus) -> String {
        if mount.error != nil { return "Error" }
        if mount.syncing { return "Syncing..." }
        return "Up to date"
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

private struct SidebarMountRow: View {
    let mount: MountStatus

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            StatusBadge(color: statusColor)
                .padding(.top, 5)
            VStack(alignment: .leading, spacing: 2) {
                Text(displayName)
                    .lineLimit(1)
                Text(statusDescription)
                    .font(.caption)
                    .foregroundStyle(mount.error == nil ? Color.secondary : Color.red)
            }
        }
    }

    private var displayName: String {
        mount.name.isEmpty ? mount.path : mount.name
    }

    private var statusColor: Color {
        if mount.error != nil { return .red }
        return mount.syncing ? .blue : .green
    }

    private var statusDescription: String {
        if mount.error != nil { return "Error" }
        if mount.syncing { return "Syncing..." }
        return "Up to date"
    }
}

private struct InviteSheet: View {
    let mount: MountStatus
    let backendClient: BackendClient
    let onCompleted: () -> Void
    @Environment(\.dismiss) private var dismiss

    @State private var email = ""
    @State private var canWrite = true
    @State private var errorMessage: String?
    @State private var isSubmitting = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Invite to \(mount.name)").font(.headline)
            TextField("Email address", text: $email).textFieldStyle(.roundedBorder)
            Toggle("Allow editing", isOn: $canWrite)
            if let errorMessage {
                Text(errorMessage).font(.caption).foregroundStyle(.red)
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Send Invite") { submit() }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(email.trimmingCharacters(in: .whitespaces).isEmpty || isSubmitting)
            }
        }
        .padding(20)
        .frame(width: 320)
    }

    private func submit() {
        isSubmitting = true
        Task {
            do {
                // Always the folder's root node - no node picker in this window
                // (macos-app spec: "Invite and Add Device actions are always scoped to
                // the whole folder").
                _ = try await backendClient.createInvite(folderId: mount.folderId, invitedEmail: email, canWrite: canWrite)
                onCompleted()
                dismiss()
            } catch {
                NSLog("InviteSheet submit failed: %@", error.localizedDescription)
                errorMessage = UserFacingError(from: error).message
                isSubmitting = false
            }
        }
    }
}

private struct AddDeviceSheet: View {
    let mount: MountStatus
    let backendClient: BackendClient
    let onCompleted: () -> Void
    @Environment(\.dismiss) private var dismiss

    @State private var name = ""
    @State private var canWrite = true
    @State private var errorMessage: String?
    @State private var isSubmitting = false
    @State private var issuedToken: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add Device to \(mount.name)").font(.headline)
            if let issuedToken {
                Text("Copy this token now - it won't be shown again.")
                    .font(.caption).foregroundStyle(.secondary)
                HStack {
                    Text(issuedToken)
                        .font(.system(.body, design: .monospaced))
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                    Button("Copy") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(issuedToken, forType: .string)
                    }
                }
                HStack {
                    Spacer()
                    Button("Done") {
                        onCompleted()
                        dismiss()
                    }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                }
            } else {
                TextField("Device name", text: $name).textFieldStyle(.roundedBorder)
                Toggle("Allow editing", isOn: $canWrite)
                if let errorMessage {
                    Text(errorMessage).font(.caption).foregroundStyle(.red)
                }
                HStack {
                    Spacer()
                    Button("Cancel") { dismiss() }
                        .keyboardShortcut(.cancelAction)
                    Button("Create") { submit() }
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                        .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty || isSubmitting)
                }
            }
        }
        .padding(20)
        .frame(width: 360)
    }

    private func submit() {
        isSubmitting = true
        Task {
            do {
                let token = try await backendClient.createDeviceGrant(
                    folderId: mount.folderId,
                    scopeNodeId: mount.scopeNodeId,
                    name: name,
                    canWrite: canWrite
                )
                issuedToken = token
            } catch {
                NSLog("AddDeviceSheet submit failed: %@", error.localizedDescription)
                errorMessage = UserFacingError(from: error).message
                isSubmitting = false
            }
        }
    }
}
