//
//  SpikeAppApp.swift
//  SpikeApp
//
//  Created by Aji Kisworo Mukti on 26/06/26.
//

import AppKit
import FileProvider
import SwiftUI

enum DomainRegistrar {
    static func registerDomain() {
        TraceLogger.reset()
        let domain = NSFileProviderDomain(identifier: NSFileProviderDomainIdentifier("valv-spike"), displayName: "Valv Spike")
        domain.supportsSyncingTrash = false

        NSFileProviderManager.add(domain) { error in
            if let error {
                NSLog("Failed to register file provider domain: %@", error.localizedDescription)
                TraceLogger.log("register-domain failed: \(error.localizedDescription)")
                return
            }

            TraceLogger.log("register-domain succeeded")

            guard let manager = NSFileProviderManager(for: domain) else {
                NSLog("Failed to create NSFileProviderManager for domain %@", domain.identifier.rawValue)
                TraceLogger.log("manager creation failed for domain \(domain.identifier.rawValue)")
                return
            }

            manager.signalEnumerator(for: .rootContainer) { signalError in
                if let signalError {
                    NSLog("Failed to signal root enumerator: %@", signalError.localizedDescription)
                    TraceLogger.log("signal root failed: \(signalError.localizedDescription)")
                } else {
                    TraceLogger.log("signal root succeeded")
                }
            }
        }
    }
}

@main
struct SpikeAppApp: App {
    init() {
        DomainRegistrar.registerDomain()
    }

    var body: some Scene {
        MenuBarExtra("Spike running", systemImage: "externaldrive.badge.icloud") {
            Text("Spike running")
            Divider()
            Button("Quit") {
                NSApplication.shared.terminate(nil)
            }
        }
        .menuBarExtraStyle(.window)
    }
}
