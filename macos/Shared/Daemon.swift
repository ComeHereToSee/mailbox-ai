import AppKit
import Foundation

/// Locating and launching the background helper that hosts the mail engine.
enum Daemon {
    static let helperBundleId = "com.comeheretosee.mailbox.ai.helper"
    static let appBundleId = "com.comeheretosee.mailbox.ai"

    static var dataDir: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Application Support")
        return base.appendingPathComponent("Mailbox AI", isDirectory: true)
    }

    static var infoURL: URL { dataDir.appendingPathComponent("daemon.json") }

    /// The helper app inside the main bundle (Contents/Library/LoginItems).
    static var helperURL: URL? {
        let fromApp = Bundle.main.bundleURL.appendingPathComponent("Contents/Library/LoginItems/MailboxAIHelper.app")
        if FileManager.default.fileExists(atPath: fromApp.path) { return fromApp }
        return NSWorkspace.shared.urlForApplication(withBundleIdentifier: helperBundleId)
    }

    /// Executable name of the helper (`PRODUCT_NAME` of the MailboxHelper target).
    static let helperExecutableName = "MailboxAIHelper"

    /// The helper's executable; `MailboxAIHelper --mcp` is the stdio MCP bridge (docs/AI.md §8.2).
    static var helperExecutableURL: URL? {
        helperURL?.appendingPathComponent("Contents/MacOS/\(helperExecutableName)")
    }

    /// The main app (from inside the helper bundle).
    static var mainAppURL: URL? {
        var url = Bundle.main.bundleURL
        for _ in 0..<4 { url.deleteLastPathComponent() }
        if url.pathExtension == "app", FileManager.default.fileExists(atPath: url.path) { return url }
        return NSWorkspace.shared.urlForApplication(withBundleIdentifier: appBundleId)
    }

    static var helperRunning: Bool {
        !NSRunningApplication.runningApplications(withBundleIdentifier: helperBundleId).isEmpty
    }

    static func isReachable() async -> Bool {
        guard APIClient.shared.reloadInfo() else { return false }
        return (try? await APIClient.shared.status()) != nil
    }

    /// Makes sure the engine is running, launching the helper when needed.
    static func ensureRunning(timeout: TimeInterval = 20) async -> Bool {
        if await isReachable() { return true }
        if !helperRunning, let url = helperURL {
            let config = NSWorkspace.OpenConfiguration()
            config.activates = false
            config.addsToRecentItems = false
            _ = try? await NSWorkspace.shared.openApplication(at: url, configuration: config)
        }
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            try? await Task.sleep(nanoseconds: 300_000_000)
            if await isReachable() { return true }
        }
        return false
    }

    static func openMainApp(url: URL? = nil) {
        if let url {
            NSWorkspace.shared.open(url)
            return
        }
        guard let app = mainAppURL else { return }
        NSWorkspace.shared.openApplication(at: app, configuration: NSWorkspace.OpenConfiguration())
    }
}

enum Fmt {
    static func relative(_ ts: Int64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(ts))
        let cal = Calendar.current
        if cal.isDateInToday(date) {
            return timeOnly.string(from: date)
        }
        if cal.isDateInYesterday(date) { return "昨天" }
        if let days = cal.dateComponents([.day], from: date, to: Date()).day, days < 7 {
            return weekday.string(from: date)
        }
        if cal.component(.year, from: date) == cal.component(.year, from: Date()) {
            return monthDay.string(from: date)
        }
        return fullDate.string(from: date)
    }

    /// Compact date and time for message headers; the full form goes in the tooltip.
    static func short(_ ts: Int64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(ts))
        let cal = Calendar.current
        if cal.isDateInToday(date) { return "今天 " + timeOnly.string(from: date) }
        if cal.isDateInYesterday(date) { return "昨天 " + timeOnly.string(from: date) }
        if cal.component(.year, from: date) == cal.component(.year, from: Date()) {
            return monthDayTime.string(from: date)
        }
        return fullDateTime.string(from: date)
    }

    static func full(_ ts: Int64) -> String {
        fullDateTime.string(from: Date(timeIntervalSince1970: TimeInterval(ts)))
    }

    static func size(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    private static func make(_ format: String) -> DateFormatter {
        let f = DateFormatter()
        f.locale = Locale(identifier: "zh_CN")
        f.dateFormat = format
        return f
    }

    static let timeOnly = make("HH:mm")
    static let weekday = make("EEEE")
    static let monthDay = make("M月d日")
    static let fullDate = make("yyyy/M/d")
    static let fullDateTime = make("yyyy年M月d日 EEEE HH:mm")
    static let monthDayTime = make("M月d日 HH:mm")
}
