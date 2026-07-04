import AppKit
import DaemonKit
import FileProviderUI

/// The Finder "Share…" action's UI. Built programmatically (no XIB) - the Xcode
/// template this target started from used a XIB-backed generic `NSViewController`
/// for a legacy macOS Services extension, not this framework's real
/// `FPUIActionExtensionViewController` base class.
final class ActionViewController: FPUIActionExtensionViewController {
    private let client = DaemonClient()
    private var itemIdentifiers: [NSFileProviderItemIdentifier] = []

    private let emailField = NSTextField()
    private let writeToggle = NSButton(checkboxWithTitle: "Allow editing", target: nil, action: nil)
    private let statusLabel = NSTextField(wrappingLabelWithString: "")
    private let inviteLinkField = NSTextField()
    private let copyButton = NSButton(title: "Copy Link", target: nil, action: nil)
    private let shareButton = NSButton(title: "Share", target: nil, action: nil)
    private let cancelButton = NSButton(title: "Cancel", target: nil, action: nil)

    override func loadView() {
        let container = NSView(frame: NSRect(x: 0, y: 0, width: 360, height: 190))

        let titleLabel = NSTextField(labelWithString: "Share this item")
        titleLabel.font = .boldSystemFont(ofSize: 14)

        emailField.placeholderString = "Email address"
        emailField.target = self
        emailField.action = #selector(shareTapped)

        writeToggle.state = .off

        statusLabel.isEditable = false
        statusLabel.isBordered = false
        statusLabel.drawsBackground = false
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.font = .systemFont(ofSize: 11)

        inviteLinkField.isEditable = false
        inviteLinkField.isSelectable = true
        inviteLinkField.isHidden = true

        copyButton.target = self
        copyButton.action = #selector(copyTapped)
        copyButton.isHidden = true

        shareButton.target = self
        shareButton.action = #selector(shareTapped)
        shareButton.keyEquivalent = "\r"

        cancelButton.target = self
        cancelButton.action = #selector(cancelTapped)
        cancelButton.keyEquivalent = "\u{1b}"

        for view in [titleLabel, emailField, writeToggle, statusLabel, inviteLinkField, copyButton, shareButton, cancelButton] {
            view.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(view)
        }

        NSLayoutConstraint.activate([
            titleLabel.topAnchor.constraint(equalTo: container.topAnchor, constant: 16),
            titleLabel.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 16),
            titleLabel.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -16),

            emailField.topAnchor.constraint(equalTo: titleLabel.bottomAnchor, constant: 12),
            emailField.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 16),
            emailField.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -16),

            writeToggle.topAnchor.constraint(equalTo: emailField.bottomAnchor, constant: 8),
            writeToggle.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 16),

            inviteLinkField.topAnchor.constraint(equalTo: writeToggle.bottomAnchor, constant: 10),
            inviteLinkField.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 16),
            inviteLinkField.trailingAnchor.constraint(equalTo: copyButton.leadingAnchor, constant: -8),

            copyButton.centerYAnchor.constraint(equalTo: inviteLinkField.centerYAnchor),
            copyButton.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -16),

            statusLabel.topAnchor.constraint(equalTo: inviteLinkField.bottomAnchor, constant: 8),
            statusLabel.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 16),
            statusLabel.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -16),

            cancelButton.bottomAnchor.constraint(equalTo: container.bottomAnchor, constant: -16),
            cancelButton.trailingAnchor.constraint(equalTo: shareButton.leadingAnchor, constant: -8),

            shareButton.bottomAnchor.constraint(equalTo: container.bottomAnchor, constant: -16),
            shareButton.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -16),
        ])

        self.view = container
    }

    override func prepare(forAction actionIdentifier: String, itemIdentifiers: [NSFileProviderItemIdentifier]) {
        self.itemIdentifiers = itemIdentifiers
    }

    @objc private func shareTapped() {
        guard let identifier = itemIdentifiers.first, case .node(let nodeId) = ValvItemIdentifier(identifier) else {
            showStatus("This item can't be shared.", isError: true)
            return
        }
        let email = emailField.stringValue.trimmingCharacters(in: .whitespaces)
        guard !email.isEmpty else {
            showStatus("Enter an email address.", isError: true)
            return
        }

        setSubmitting(true)
        let canWrite = writeToggle.state == .on
        Task {
            do {
                let response = try await client.fpShare(nodeId: nodeId, invitedEmail: email, canWrite: canWrite)
                showInviteLink(response.inviteUrl)
            } catch {
                showStatus(error.localizedDescription, isError: true)
            }
            setSubmitting(false)
        }
    }

    @objc private func cancelTapped() {
        extensionContext.cancelRequest(withError: NSError(
            domain: FPUIErrorDomain,
            code: Int(FPUIExtensionErrorCode.userCancelled.rawValue),
            userInfo: [:]
        ))
    }

    @objc private func copyTapped() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(inviteLinkField.stringValue, forType: .string)
    }

    private func setSubmitting(_ submitting: Bool) {
        shareButton.isEnabled = !submitting
        emailField.isEnabled = !submitting
        writeToggle.isEnabled = !submitting
    }

    private func showStatus(_ message: String, isError: Bool) {
        statusLabel.stringValue = message
        statusLabel.textColor = isError ? .systemRed : .secondaryLabelColor
    }

    private func showInviteLink(_ url: String) {
        inviteLinkField.stringValue = url
        inviteLinkField.isHidden = false
        copyButton.isHidden = false
        showStatus("Invite created.", isError: false)
    }
}
