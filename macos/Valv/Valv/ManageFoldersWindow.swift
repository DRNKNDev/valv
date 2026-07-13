import DaemonKit
import SwiftUI

enum SharedWithRow: Identifiable, Hashable {
    case grant(GrantEntry)
    case invite(PendingInvite)

    var id: String {
        switch self {
        case .grant(let grant): return "grant-\(grant.grantId)"
        case .invite(let invite): return "invite-\(invite.inviteId)"
        }
    }

    var scopeNodeId: String {
        switch self {
        case .grant(let grant): return grant.scopeNodeId
        case .invite(let invite): return invite.scopeNodeId
        }
    }
}

struct IssuedAccessKey: Identifiable {
    let id = UUID()
    let folderName: String
    let token: String
}

enum AccessKeyHandoff {
    static func defaultMountPath(folderName: String) -> String {
        let trimmed = folderName.trimmingCharacters(in: .whitespacesAndNewlines)
        return "~/valv/\(trimmed.isEmpty ? "Folder" : trimmed)"
    }

    static func mountCommand(path: String, token: String) -> String {
        "valv mount \(path) --key \(token)"
    }
}

struct ManageFoldersWindow: View {
    @EnvironmentObject private var store: DaemonStore
    @EnvironmentObject private var domainManager: FileProviderDomainManager
    @State private var selectedFolderId: String?
    @State private var grants: [GrantEntry] = []
    @State private var pendingInvites: [PendingInvite] = []
    @State private var resolvedScopePaths: [String: String] = [:]
    @State private var showInviteSheet = false
    @State private var showAddAccessKeySheet = false
    @State private var revokeTarget: GrantEntry?
    @State private var regenerateTarget: GrantEntry?
    @State private var removeTarget: MountStatus?
    @State private var issuedAccessKey: IssuedAccessKey?
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

    private var currentSharedWithRows: [SharedWithRow] {
        Self.sharedWithRows(grants: grants, invites: pendingInvites)
    }

    private var removeAlertTitle: String {
        guard let removeTarget else {
            return "Stop syncing this folder on this Mac?"
        }
        return "Stop syncing '\(displayName(for: removeTarget))' on this Mac?"
    }

    static func sharedWithRows(grants: [GrantEntry], invites: [PendingInvite]) -> [SharedWithRow] {
        grants.map { .grant($0) } + invites.map { .invite($0) }
    }

    static func isGenuinelyUnshared(grants: [GrantEntry], invites: [PendingInvite]) -> Bool {
        grants.allSatisfy(\.isOwnerGrant) && invites.isEmpty
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
            if selectedFolderId == nil {
                selectedFolderId = mounts.first?.folderId
            }
        }
        .sheet(isPresented: $showInviteSheet) {
            if let mount = selectedMount {
                InviteSheet(mount: mount, backendClient: backendClient, onCompleted: {
                    Task { await loadSharedWith(folderId: mount.folderId) }
                })
            }
        }
        .sheet(isPresented: $showAddAccessKeySheet) {
            if let mount = selectedMount {
                AddAccessKeySheet(mount: mount, backendClient: backendClient, onCompleted: {
                    Task { await loadSharedWith(folderId: mount.folderId) }
                })
            }
        }
        .sheet(item: $issuedAccessKey) { issued in
            VStack(alignment: .leading, spacing: 0) {
                Text("Access Key Regenerated").font(.headline).padding(.bottom, 8)
                AccessKeyHandoffView(folderName: issued.folderName, token: issued.token) {
                    issuedAccessKey = nil
                }
            }
            .padding(20)
            .interactiveDismissDisabled(true)
        }
        .alert("Revoke Access?", isPresented: .constant(revokeTarget != nil), presenting: revokeTarget) { grant in
            Button("Revoke", role: .destructive) {
                Task { await revoke(grant) }
            }
            Button("Cancel", role: .cancel) { revokeTarget = nil }
        } message: { grant in
            Text("This removes access for \(grant.displayName) on \(selectedMount?.name ?? "this folder").")
        }
        .alert("Regenerate Access Key?", isPresented: .constant(regenerateTarget != nil), presenting: regenerateTarget) { grant in
            Button("Regenerate", role: .destructive) {
                Task { await regenerate(grant) }
            }
            Button("Cancel", role: .cancel) { regenerateTarget = nil }
        } message: { grant in
            Text("The machine currently using '\(grant.displayName)' will lose access immediately and needs the new key to keep syncing.")
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
                Button("Add Access Key...") { showAddAccessKeySheet = true }
            }

            if let loadError {
                Text(loadError).font(.caption).foregroundStyle(.red)
            }

            grantsTable(for: mount)

            Spacer()
        }
        .padding()
        .task(id: mount.folderId) {
            await loadSharedWith(folderId: mount.folderId)
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
        } else if Self.isGenuinelyUnshared(grants: grants, invites: pendingInvites) {
            Text("Not shared with anyone yet")
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, minHeight: 120)
        } else {
            Table(currentSharedWithRows) {
                TableColumn("Grantee") { row in
                    Text(grantee(for: row))
                        .lineLimit(1)
                }
                .width(min: 180, ideal: 260)

                TableColumn("Scope") { row in
                    Text(scopeLabel(forScopeNodeId: row.scopeNodeId))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .width(min: 120, ideal: 180)

                TableColumn("Access") { row in
                    Text(accessLabel(for: row))
                        .lineLimit(1)
                }
                .width(min: 150, ideal: 170)

                TableColumn("Added by") { row in
                    Text(addedByLabel(for: row))
                        .lineLimit(1)
                        .foregroundStyle(.secondary)
                }
                .width(min: 140, ideal: 200)

                TableColumn("") { row in
                    rowActions(for: row)
                }
                .width(min: 90, ideal: 170, max: 200)
            }
        }
    }

    @ViewBuilder
    private func rowActions(for row: SharedWithRow) -> some View {
        switch row {
        case .grant(let grant):
            if grant.isOwnerGrant {
                EmptyView()
            } else {
                HStack {
                    if grant.deviceId != nil {
                        Button("Regenerate") { regenerateTarget = grant }
                    }
                    Button("Revoke") { revokeTarget = grant }
                }
            }
        case .invite(let invite):
            Button("Cancel") {
                Task { await cancel(invite) }
            }
        }
    }

    private func grantee(for row: SharedWithRow) -> String {
        switch row {
        case .grant(let grant): return grant.displayName
        case .invite(let invite): return invite.invitedEmail
        }
    }

    private func accessLabel(for row: SharedWithRow) -> String {
        switch row {
        case .grant(let grant):
            return "\(grant.role.capitalized) · \(grant.canWrite ? "Read & Write" : "Read Only")"
        case .invite(let invite):
            return "Pending · \(invite.canWrite ? "Read & Write" : "Read Only")"
        }
    }

    private func addedByLabel(for row: SharedWithRow) -> String {
        switch row {
        case .grant(let grant): return grant.createdByEmail ?? "Unknown"
        case .invite(let invite): return invite.createdByEmail ?? "Unknown"
        }
    }

    // "" is the daemon's own documented resolution for a mount's root/scope node
    // itself (ipc-control-api spec: "The root/scope node itself SHALL resolve to ''"),
    // so an empty resolved path means this grant covers the entire folder.
    private func scopeLabel(forScopeNodeId scopeNodeId: String) -> String {
        guard let resolved = resolvedScopePaths[scopeNodeId] else {
            return "Subfolder"
        }
        return resolved.isEmpty ? "Entire Folder" : resolved
    }

    private func loadSharedWith(folderId: String) async {
        isLoadingGrants = true
        defer { isLoadingGrants = false }
        do {
            async let loadedGrants = backendClient.folderGrants(folderId: folderId)
            async let loadedInvites = backendClient.folderInvites(folderId: folderId)
            let (fetchedGrants, fetchedInvites) = try await (loadedGrants, loadedInvites)
            grants = fetchedGrants
            pendingInvites = fetchedInvites
            loadError = nil
        } catch {
            NSLog("ManageFoldersWindow loadSharedWith failed: %@", error.localizedDescription)
            loadError = UserFacingError(from: error).message
        }
        await resolveScopes(for: grants.map(\.scopeNodeId) + pendingInvites.map(\.scopeNodeId))
    }

    private func resolveScopes(for scopeNodeIds: [String]) async {
        let unresolved = Set(scopeNodeIds).filter { resolvedScopePaths[$0] == nil }
        guard !unresolved.isEmpty else { return }
        isResolvingScopes = true
        defer { isResolvingScopes = false }
        for scopeNodeId in unresolved {
            do {
                let path = try await store.nodePath(nodeId: scopeNodeId)
                resolvedScopePaths[scopeNodeId] = path
            } catch {
                resolvedScopePaths[scopeNodeId] = "Subfolder"
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
        await loadSharedWith(folderId: grant.folderId)
    }

    private func regenerate(_ grant: GrantEntry) async {
        regenerateTarget = nil
        do {
            let issued = try await backendClient.regenerateGrant(folderId: grant.folderId, grantId: grant.grantId)
            loadError = nil
            issuedAccessKey = IssuedAccessKey(folderName: selectedMount?.name ?? "this folder", token: issued.token)
        } catch {
            NSLog("ManageFoldersWindow regenerate failed: %@", error.localizedDescription)
            loadError = UserFacingError(from: error).message
        }
        await loadSharedWith(folderId: grant.folderId)
    }

    private func cancel(_ invite: PendingInvite) async {
        guard let folderId = selectedFolderId else { return }
        do {
            try await backendClient.cancelInvite(folderId: folderId, inviteId: invite.inviteId)
            loadError = nil
        } catch {
            NSLog("ManageFoldersWindow cancelInvite failed: %@", error.localizedDescription)
            loadError = UserFacingError(from: error).message
        }
        await loadSharedWith(folderId: folderId)
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

private struct AddAccessKeySheet: View {
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
            if let issuedToken {
                AccessKeyHandoffView(folderName: mount.name, token: issuedToken) {
                    onCompleted()
                    dismiss()
                }
            } else {
                Text("Add Access Key to \(mount.name)").font(.headline)
                Text("For a server or agent. To add your own Mac, install Valv there and sign in.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                TextField("Key name", text: $name).textFieldStyle(.roundedBorder)
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
        .frame(width: issuedToken == nil ? 360 : 420)
        .interactiveDismissDisabled(issuedToken != nil)
    }

    private func submit() {
        isSubmitting = true
        Task {
            do {
                let issued = try await backendClient.createDeviceGrant(
                    folderId: mount.folderId,
                    scopeNodeId: mount.scopeNodeId,
                    name: name,
                    canWrite: canWrite
                )
                issuedToken = issued.token
            } catch {
                NSLog("AddAccessKeySheet submit failed: %@", error.localizedDescription)
                errorMessage = UserFacingError(from: error).message
                isSubmitting = false
            }
        }
    }
}

private struct AccessKeyHandoffView: View {
    let folderName: String
    let token: String
    let onDone: () -> Void

    @State private var mountPath: String

    init(folderName: String, token: String, onDone: @escaping () -> Void) {
        self.folderName = folderName
        self.token = token
        self.onDone = onDone
        _mountPath = State(initialValue: AccessKeyHandoff.defaultMountPath(folderName: folderName))
    }

    private var mountCommand: String {
        AccessKeyHandoff.mountCommand(path: mountPath, token: token)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("This key only grants access to \(folderName).")
                .font(.callout)
            Text("It won't be shown again - copy it now.")
                .font(.caption)
                .foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 4) {
                Text("Destination on the other machine").font(.caption).foregroundStyle(.secondary)
                TextField("Path", text: $mountPath).textFieldStyle(.roundedBorder)
            }

            VStack(alignment: .leading, spacing: 4) {
                Text("Command").font(.caption).foregroundStyle(.secondary)
                Text(mountCommand)
                    .font(.system(.body, design: .monospaced))
                    .textSelection(.enabled)
                    .lineLimit(3)
                    .truncationMode(.middle)
            }

            HStack {
                Spacer()
                Button("Copy Token") {
                    copyToPasteboard(token)
                }
                Button("Copy Command") {
                    copyToPasteboard(mountCommand)
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(mountPath.trimmingCharacters(in: .whitespaces).isEmpty)
            }

            HStack {
                Spacer()
                Button("Done", action: onDone)
            }
        }
        .frame(width: 420)
    }

    private func copyToPasteboard(_ value: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
    }
}
