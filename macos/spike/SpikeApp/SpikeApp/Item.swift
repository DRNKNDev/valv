//
//  Item.swift
//  SpikeApp
//
//  Created by Aji Kisworo Mukti on 26/06/26.
//

import Foundation

struct Item: Identifiable, Hashable {
    let id = UUID()
    let timestamp: Date
}
