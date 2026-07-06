import DaemonKit
import SwiftUI

struct ManageFoldersWindow: View {
    @EnvironmentObject private var store: DaemonStore
    @StateObject private var domainManager = FileProviderDomainManager()
    @State private var selectedFolderId: String?
    @State private var grants: [GrantEntry] = []
    @State private var resolvedScopePaths: [String: String] = [:]
    @State private var showInviteSheet = false
    @State private var showAddDeviceSheet = false
    @State private var revokeTarget: GrantEntry?
    @State private var loadError: String?

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
        .frame(width: 700, height: 450)
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
    }

    private var sidebar: some View {
        List(mounts, id: \.folderId, selection: $selectedFolderId) { mount in
            VStack(alignment: .leading) {
                Text(mount.name.isEmpty ? mount.path : mount.name)
                if mount.syncing {
                    Text("Syncing…").font(.caption).foregroundStyle(.secondary)
                } else if mount.error != nil {
                    Text("Error").font(.caption).foregroundStyle(.red)
                }
            }
            .tag(mount.folderId)
        }
        .toolbar {
            ToolbarItem {
                Menu {
                    Button("Create a New Synced Folder…") { createFolder() }
                    Button("Link Existing Folder by ID…") { linkFolder(byId: true) }
                    Button("Mount via Grant Link…") { linkFolder(byId: false) }
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
                Button("Create a New Synced Folder…") { createFolder() }
                Button("Link Existing Folder by ID…") { linkFolder(byId: true) }
                Button("Mount via Grant Link…") { linkFolder(byId: false) }
            }
        }
        .padding()
    }

    private func detailPane(for mount: MountStatus) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text(mount.name.isEmpty ? mount.path : mount.name).font(.title2).bold()
                Text(mount.path).font(.caption).foregroundStyle(.secondary)
                if let error = mount.error {
                    Text(error).font(.caption).foregroundStyle(.red)
                }
            }

            HStack {
                Button("Sync Now") {
                    Task { await store.syncNow(folderId: mount.folderId) }
                }
                Button("Remove from this Mac") {
                    removeMount(mount)
                }
            }

            Divider()

            HStack {
                Text("Shared With").font(.headline)
                Spacer()
                Button("Invite…") { showInviteSheet = true }
                Button("Add Device…") { showAddDeviceSheet = true }
            }

            if let loadError {
                Text(loadError).font(.caption).foregroundStyle(.red)
            }

            Table(grantsForSelectedFolder) {
                TableColumn("Grantee") { grant in
                    Text(grant.granteeEmail ?? grant.deviceName ?? "Unknown")
                }
                TableColumn("Scope") { grant in
                    Text(scopeLabel(for: grant, mount: mount))
                }
                TableColumn("Role") { grant in
                    Text(grant.role.capitalized)
                }
                TableColumn("Permission") { grant in
                    Text(grant.canWrite ? "Read & Write" : "Read Only")
                }
                TableColumn("") { grant in
                    Button("Revoke") { revokeTarget = grant }
                }
            }

            Spacer()
        }
        .padding()
        .task(id: mount.folderId) {
            await resolveScopes(for: grantsForSelectedFolder)
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
        do {
            grants = try await backendClient.grants()
            loadError = nil
        } catch {
            loadError = error.localizedDescription
        }
    }

    private func resolveScopes(for entries: [GrantEntry]) async {
        for grant in entries where resolvedScopePaths[grant.scopeNodeId] == nil {
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
        try? await backendClient.revokeGrant(folderId: grant.folderId, grantId: grant.grantId)
        await loadGrants()
    }

    private func removeMount(_ mount: MountStatus) {
        Task {
            try? await store.unmount(folderId: mount.folderId)
            if selectedFolderId == mount.folderId {
                selectedFolderId = store.status?.mounts.first?.folderId
            }
            await domainManager.signalRootEnumerator()
        }
    }

    private func createFolder() {
        Task {
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
                loadError = error.localizedDescription
            }
            await domainManager.signalRootEnumerator()
        }
    }

    private func linkFolder(byId: Bool) {
        Task {
            let panel = NSOpenPanel()
            panel.canChooseDirectories = true
            panel.canChooseFiles = false
            panel.prompt = "Select"
            guard panel.runModal() == .OK, let url = panel.url else { return }

            let alert = NSAlert()
            alert.messageText = byId ? "Folder ID" : "Grant Token"
            alert.addButton(withTitle: "Mount")
            alert.addButton(withTitle: "Cancel")
            let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
            alert.accessoryView = field
            guard alert.runModal() == .alertFirstButtonReturn else { return }
            let value = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !value.isEmpty else { return }

            let request = byId
                ? MountRequest(path: url.path, folderId: value)
                : MountRequest(path: url.path, grantToken: value)
            do {
                let response = try await store.mount(request)
                selectedFolderId = response.folderId
                loadError = nil
            } catch {
                loadError = error.localizedDescription
            }
            await domainManager.signalRootEnumerator()
        }
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
        VStack(spacing: 12) {
            Text("Invite to \(mount.name)").font(.headline)
            TextField("Email address", text: $email).textFieldStyle(.roundedBorder)
            Toggle("Allow editing", isOn: $canWrite)
            if let errorMessage {
                Text(errorMessage).font(.caption).foregroundStyle(.red)
            }
            HStack {
                Button("Cancel") { dismiss() }
                Button("Send Invite") { submit() }
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
                errorMessage = error.localizedDescription
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
        VStack(spacing: 12) {
            Text("Add Device to \(mount.name)").font(.headline)
            if let issuedToken {
                Text("Copy this token now - it won't be shown again.")
                    .font(.caption).foregroundStyle(.secondary)
                HStack {
                    Text(issuedToken).font(.system(.body, design: .monospaced)).lineLimit(1).truncationMode(.middle)
                    Button("Copy") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(issuedToken, forType: .string)
                    }
                }
                Button("Done") {
                    onCompleted()
                    dismiss()
                }
            } else {
                TextField("Device name", text: $name).textFieldStyle(.roundedBorder)
                Toggle("Allow editing", isOn: $canWrite)
                if let errorMessage {
                    Text(errorMessage).font(.caption).foregroundStyle(.red)
                }
                HStack {
                    Button("Cancel") { dismiss() }
                    Button("Create") { submit() }
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
                errorMessage = error.localizedDescription
                isSubmitting = false
            }
        }
    }
}
