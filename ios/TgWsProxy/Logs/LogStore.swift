import Foundation

/// In-memory ring of app side events. The Rust core writes its own rotating
/// file; these lines explain what the Swift layer did around it.
final class AppLog: @unchecked Sendable {
    static let shared = AppLog()

    private let limit = 200
    private let queue = DispatchQueue(label: "com.danusha.tgwsproxy.applog")
    private var lines: [String] = []

    private init() {}

    func append(_ message: String) {
        let stamp = Self.formatter.string(from: Date())
        queue.sync {
            lines.append("\(stamp)  \(message)")
            if lines.count > limit {
                lines.removeFirst(lines.count - limit)
            }
        }
    }

    func snapshot() -> [String] {
        queue.sync { lines }
    }

    func clear() {
        queue.sync { lines.removeAll() }
    }

    private static let formatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return formatter
    }()
}

/// Reads and clears the rotating log file written by the core.
enum LogStore {
    /// Keeps the view responsive on a log that just rotated up to its limit.
    private static let maxCharacters = 200_000

    static func read() -> String {
        let url = ProxySettingsStore.logFileURL
        let core = (try? String(contentsOf: url, encoding: .utf8)) ?? ""
        let app = AppLog.shared.snapshot().joined(separator: "\n")

        var text = ""
        if !app.isEmpty {
            text += "=== app ===\n\(app)\n\n"
        }
        text += core.isEmpty ? String(localized: "The core has not written anything yet.") : "=== core ===\n\(core)"

        if text.count > maxCharacters {
            text = String(text.suffix(maxCharacters))
        }
        return text
    }

    /// Truncates the current file; rotated backups are removed as well.
    static func clear() {
        let url = ProxySettingsStore.logFileURL
        let manager = FileManager.default
        try? Data().write(to: url)
        for index in 1...3 {
            try? manager.removeItem(at: url.appendingPathExtension(String(index)))
        }
        AppLog.shared.clear()
    }

    /// Writes the visible log into a temporary file for the share sheet.
    static func exportURL() -> URL? {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("tg-ws-proxy-ios.log")
        do {
            try read().write(to: url, atomically: true, encoding: .utf8)
            return url
        } catch {
            return nil
        }
    }
}
