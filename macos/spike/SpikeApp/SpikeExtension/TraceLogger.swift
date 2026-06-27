import Foundation

enum TraceLogger {
    private static let appGroupIdentifier = "group.dev.drnkn.SpikeApp"
    private static let logFileName = "spike-trace.log"

    static func reset() {
        guard let logURL = logURL() else { return }
        try? FileManager.default.removeItem(at: logURL)
    }

    static func log(_ message: String) {
        guard let logURL = logURL() else { return }

        let formatter = ISO8601DateFormatter()
        let line = "\(formatter.string(from: Date())) \(message)\n"
        let data = Data(line.utf8)

        if !FileManager.default.fileExists(atPath: logURL.path) {
            try? data.write(to: logURL, options: .atomic)
            return
        }

        guard let handle = try? FileHandle(forWritingTo: logURL) else {
            return
        }

        do {
            try handle.seekToEnd()
            try handle.write(contentsOf: data)
            try handle.close()
        } catch {
            try? handle.close()
        }
    }

    private static func logURL() -> URL? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroupIdentifier)?
            .appendingPathComponent(logFileName)
    }
}
