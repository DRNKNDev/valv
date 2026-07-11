import AppKit
import SwiftUI

/// Carries over `ActionViewController`'s field set (email, read/write toggle,
/// status/result line) into a standalone window, since `FIFinderSync` has no
/// view-controller hosting of its own (design D1) - `ValvFinderSync` hands off to
/// `valv://share`, which presents this window instead.
struct ShareWindow: View {
    @ObservedObject var viewModel: ShareWindowViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Share this item").font(.headline)
            Text(viewModel.path)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)

            if case .resolving = viewModel.resolution {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                    Text("Locating this file...").font(.caption).foregroundStyle(.secondary)
                }
            }

            if case .failed(let message) = viewModel.resolution {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.red)
            }

            TextField("Email address", text: $viewModel.email)
                .textFieldStyle(.roundedBorder)
                .disabled(viewModel.isSubmitting)

            Toggle("Allow editing", isOn: $viewModel.canWrite)
                .disabled(viewModel.isSubmitting)

            if let inviteURL = viewModel.inviteURL {
                HStack {
                    Text(inviteURL)
                        .font(.system(.body, design: .monospaced))
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                    Button("Copy") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(inviteURL, forType: .string)
                    }
                }
            }

            if let statusMessage = viewModel.statusMessage {
                Text(statusMessage)
                    .font(.caption)
                    .foregroundStyle(viewModel.isError ? .red : .secondary)
            }

            HStack {
                Spacer()
                Button("Share") {
                    Task { await viewModel.submit() }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!viewModel.canSubmit)
            }
        }
        .padding(20)
        .frame(width: 360)
        .task {
            await viewModel.resolveIfNeeded()
        }
    }
}
