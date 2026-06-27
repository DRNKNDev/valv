//
//  ContentView.swift
//  SpikeApp
//
//  Created by Aji Kisworo Mukti on 26/06/26.
//

import SwiftUI

struct ContentView: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Valv Spike")
                .font(.headline)
            Text("File Provider host app is running.")
                .foregroundStyle(.secondary)
        }
        .padding()
    }
}

#Preview {
    ContentView()
}
