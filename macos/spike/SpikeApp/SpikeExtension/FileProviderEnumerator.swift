//
//  FileProviderEnumerator.swift
//  SpikeExtension
//
//  Created by Aji Kisworo Mukti on 26/06/26.
//

import FileProvider

final class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    private static let pageSize = 200

    private let enumeratedItemIdentifier: NSFileProviderItemIdentifier
    private let r2Client: R2Client
    private let anchor = NSFileProviderSyncAnchor("an anchor".data(using: .utf8)!)

    init(enumeratedItemIdentifier: NSFileProviderItemIdentifier, r2Client: R2Client) {
        self.enumeratedItemIdentifier = enumeratedItemIdentifier
        self.r2Client = r2Client
        super.init()
    }

    func invalidate() {
        r2Client.cancelAll()
    }

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage) {
        NSLog("FileProviderEnumerator enumerateItems for %@", enumeratedItemIdentifier.rawValue)
        TraceLogger.log("enumerate start identifier=\(enumeratedItemIdentifier.rawValue) page=\(Self.startIndex(from: page))")
        guard enumeratedItemIdentifier == .rootContainer || enumeratedItemIdentifier == .workingSet else {
            observer.finishEnumerating(upTo: nil)
            return
        }

        if enumeratedItemIdentifier == .workingSet {
            TraceLogger.log("enumerate working-set skipped")
            observer.finishEnumerating(upTo: nil)
            return
        }

        Task {
            do {
                let records = try await r2Client.listObjects()
                let startIndex = Self.startIndex(from: page)
                let requestedPageSize = max(observer.suggestedPageSize ?? Self.pageSize, 1)
                let effectivePageSize = min(requestedPageSize, Self.pageSize)

                NSLog(
                    "FileProviderEnumerator suggested page size %d, effective page size %d",
                    requestedPageSize,
                    effectivePageSize
                )
                TraceLogger.log("enumerate page-size suggested=\(requestedPageSize) effective=\(effectivePageSize)")

                guard startIndex < records.count else {
                    TraceLogger.log("enumerate end start=\(startIndex) count=\(records.count) next=nil")
                    observer.finishEnumerating(upTo: nil)
                    return
                }

                let endIndex = min(startIndex + effectivePageSize, records.count)
                let items = records[startIndex..<endIndex].map(FileProviderItem.init(record:))

                NSLog("FileProviderEnumerator enumerating items %d...%d of %d", startIndex, endIndex, records.count)
                TraceLogger.log("enumerate range start=\(startIndex) end=\(endIndex) total=\(records.count)")

                observer.didEnumerate(items)

                let nextPage: NSFileProviderPage? = if endIndex < records.count {
                    NSFileProviderPage(rawValue: Data("\(endIndex)".utf8))
                } else {
                    nil
                }

                observer.finishEnumerating(upTo: nextPage)
            } catch {
                NSLog("FileProviderEnumerator failed: %@", error.localizedDescription)
                TraceLogger.log("enumerate failed: \(error.localizedDescription)")
                observer.finishEnumeratingWithError(error as NSError)
            }
        }
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor) {
        observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(anchor)
    }

    private static func startIndex(from page: NSFileProviderPage) -> Int {
        guard let rawPage = String(data: page.rawValue, encoding: .utf8), let offset = Int(rawPage) else {
            return 0
        }

        return offset
    }
}
