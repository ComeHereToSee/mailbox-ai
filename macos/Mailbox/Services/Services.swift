import AppIntents
import AppKit
import Contacts
import CoreSpotlight
import EventKit
import LocalAuthentication
import ServiceManagement
import SwiftUI
import UniformTypeIdentifiers
import WebKit

// MARK: - Contacts

/// Looks up the macOS address book for recipient autocomplete.
actor ContactsService {
    static let shared = ContactsService()
    private let store = CNContactStore()
    private var authorized: Bool?

    func search(_ query: String) async -> [Contact] {
        if authorized == nil {
            let status = CNContactStore.authorizationStatus(for: .contacts)
            if status == .notDetermined {
                authorized = (try? await store.requestAccess(for: .contacts)) ?? false
            } else {
                authorized = status == .authorized
            }
        }
        guard authorized == true, !query.isEmpty else { return [] }
        let keys = [CNContactGivenNameKey, CNContactFamilyNameKey, CNContactEmailAddressesKey, CNContactOrganizationNameKey] as [CNKeyDescriptor]
        guard let found = try? store.unifiedContacts(matching: CNContact.predicateForContacts(matchingName: query), keysToFetch: keys) else {
            return []
        }
        var out: [Contact] = []
        for c in found {
            let cjk = (c.familyName + c.givenName).unicodeScalars.contains { $0.value >= 0x4E00 && $0.value <= 0x9FFF }
            let name = cjk ? c.familyName + c.givenName : [c.givenName, c.familyName].filter { !$0.isEmpty }.joined(separator: " ")
            for e in c.emailAddresses {
                out.append(Contact(email: e.value as String, name: name.isEmpty ? c.organizationName : name))
            }
        }
        return out
    }
}

// MARK: - Calendar & Reminders

@MainActor
final class CalendarService {
    static let shared = CalendarService()
    private let store = EKEventStore()

    private func eventsAccess() async -> Bool {
        (try? await store.requestFullAccessToEvents()) ?? false
    }

    private func remindersAccess() async -> Bool {
        (try? await store.requestFullAccessToReminders()) ?? false
    }

    func addInvite(_ invite: CalendarInvite, silent: Bool = false) {
        Task {
            guard await eventsAccess() else {
                AppState.shared.show(Toast(text: "没有日历权限（系统设置 → 隐私与安全性 → 日历）", isError: true))
                return
            }
            let ev = EKEvent(eventStore: store)
            ev.title = invite.summary
            ev.location = invite.location
            ev.notes = invite.description
            ev.isAllDay = invite.allDay
            if let s = invite.start {
                var start = Date(timeIntervalSince1970: TimeInterval(s))
                var end = invite.end.map { Date(timeIntervalSince1970: TimeInterval($0)) } ?? start.addingTimeInterval(3600)
                if invite.allDay {
                    // All-day dates arrive as UTC midnight; shift them to local midnight.
                    let offset = TimeInterval(-TimeZone.current.secondsFromGMT(for: start))
                    start = start.addingTimeInterval(offset)
                    end = end.addingTimeInterval(offset)
                }
                ev.startDate = start
                ev.endDate = end
            } else {
                ev.startDate = Date()
                ev.endDate = Date().addingTimeInterval(3600)
            }
            ev.calendar = store.defaultCalendarForNewEvents
            do {
                try store.save(ev, span: .thisEvent)
                if !silent { AppState.shared.show(Toast(text: "已加入日历")) }
            } catch {
                AppState.shared.show(Toast(text: "加入日历失败：\(error.localizedDescription)", isError: true))
            }
        }
    }

    /// Creates an event titled after the mail (next full hour, 1 h) and opens Calendar.
    func addEvent(from d: MessageDetail) {
        if let invite = d.invite {
            addInvite(invite)
            return
        }
        Task {
            guard await eventsAccess() else {
                AppState.shared.show(Toast(text: "没有日历权限", isError: true))
                return
            }
            let ev = EKEvent(eventStore: store)
            ev.title = d.summary.subject.isEmpty ? "邮件日程" : d.summary.subject
            ev.notes = "来自邮件：\(d.summary.from.full)\n\(d.summary.snippet)\n\nmailboxai://message/\(d.id)"
            let cal = Calendar.current
            let next = cal.nextDate(after: Date(), matching: DateComponents(minute: 0), matchingPolicy: .nextTime) ?? Date()
            ev.startDate = next
            ev.endDate = next.addingTimeInterval(3600)
            ev.calendar = store.defaultCalendarForNewEvents
            do {
                try store.save(ev, span: .thisEvent)
                AppState.shared.show(Toast(text: "已创建日程（\(Fmt.full(Int64(next.timeIntervalSince1970)))），可在日历中调整"))
                if let url = URL(string: "ical://ekevent/\(ev.calendarItemIdentifier)?method=show&options=more") {
                    NSWorkspace.shared.open(url)
                }
            } catch {
                AppState.shared.show(Toast(text: "创建日程失败：\(error.localizedDescription)", isError: true))
            }
        }
    }

    func addReminder(from d: MessageDetail) {
        Task {
            guard await remindersAccess() else {
                AppState.shared.show(Toast(text: "没有提醒事项权限", isError: true))
                return
            }
            let r = EKReminder(eventStore: store)
            r.title = "处理邮件：\(d.summary.subject.isEmpty ? d.summary.from.display : d.summary.subject)"
            r.notes = "\(d.summary.from.full)\nmailboxai://message/\(d.id)"
            r.url = URL(string: "mailboxai://message/\(d.id)")
            let cal = Calendar.current
            let tomorrow = cal.date(byAdding: .day, value: 1, to: Date()) ?? Date()
            r.dueDateComponents = cal.dateComponents([.year, .month, .day, .hour, .minute],
                                                     from: cal.date(bySettingHour: 9, minute: 0, second: 0, of: tomorrow) ?? tomorrow)
            r.calendar = store.defaultCalendarForNewReminders()
            do {
                try store.save(r, commit: true)
                AppState.shared.show(Toast(text: "已添加到提醒事项（明天 9:00）"))
            } catch {
                AppState.shared.show(Toast(text: "创建提醒失败：\(error.localizedDescription)", isError: true))
            }
        }
    }
}

// MARK: - Print / PDF

@MainActor
final class PrintService: NSObject, WKNavigationDelegate {
    private var web: WKWebView?
    private var completion: ((WKWebView) -> Void)?
    private static var active: [PrintService] = []

    static func document(for d: MessageDetail) -> String {
        func esc(_ s: String) -> String {
            s.replacingOccurrences(of: "&", with: "&amp;").replacingOccurrences(of: "<", with: "&lt;").replacingOccurrences(of: ">", with: "&gt;")
        }
        let header = """
        <div style="font:13px -apple-system;border-bottom:1px solid #ccc;padding-bottom:8px;margin-bottom:12px">
        <div style="font-size:18px;font-weight:600;margin-bottom:6px">\(esc(d.summary.subject))</div>
        <div>发件人：\(esc(d.summary.from.full))</div>
        <div>收件人：\(esc(d.summary.to.map(\.full).joined(separator: ", ")))</div>
        \(d.cc.isEmpty ? "" : "<div>抄送：\(esc(d.cc.map(\.full).joined(separator: ", ")))</div>")
        <div>日期：\(esc(Fmt.full(d.summary.date)))</div></div>
        """
        return MessageWebView.wrap(header + (d.html ?? ""))
    }

    private func load(_ html: String, then: @escaping (WKWebView) -> Void) {
        let config = WKWebViewConfiguration()
        config.setURLSchemeHandler(CidSchemeHandler(), forURLScheme: "mailbox-cid")
        let web = WKWebView(frame: NSRect(x: 0, y: 0, width: 800, height: 1100), configuration: config)
        web.navigationDelegate = self
        self.web = web
        completion = then
        Self.active.append(self)
        web.loadHTMLString(html, baseURL: URL(string: "about:blank"))
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        Task {
            try? await Task.sleep(nanoseconds: 400_000_000)
            self.completion?(webView)
            self.completion = nil
            Self.active.removeAll { $0 === self }
        }
    }

    static func print(detail: MessageDetail) {
        PrintService().load(document(for: detail)) { web in
            let info = NSPrintInfo.shared
            info.topMargin = 36
            info.bottomMargin = 36
            info.leftMargin = 36
            info.rightMargin = 36
            let op = web.printOperation(with: info)
            op.view?.frame = web.bounds
            op.showsPrintPanel = true
            op.run()
        }
    }

    static func exportPDF(detail: MessageDetail) {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.pdf]
        panel.nameFieldStringValue = (detail.summary.subject.isEmpty ? "邮件" : detail.summary.subject) + ".pdf"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        PrintService().load(document(for: detail)) { web in
            web.createPDF(configuration: WKPDFConfiguration()) { result in
                if case .success(let data) = result {
                    try? data.write(to: url)
                    NSWorkspace.shared.activateFileViewerSelecting([url])
                }
            }
        }
    }
}

enum SharingText {
    static func text(for d: MessageDetail) -> String {
        var s = "【\(d.summary.subject)】\n发件人：\(d.summary.from.full)\n日期：\(Fmt.full(d.summary.date))\n\n"
        s += d.text ?? d.summary.snippet
        return s
    }
}

// MARK: - Services menu ("用 Mailbox 发送")

final class ServicesProvider: NSObject {
    @objc func composeWithSelection(_ pboard: NSPasteboard, userData: String, error: AutoreleasingUnsafeMutablePointer<NSString?>) {
        var req = ComposeRequest()
        if let urls = pboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL], !urls.isEmpty {
            req.attachments = urls.map(\.path)
        } else if let text = pboard.string(forType: .string) {
            req.body = text
        }
        Task { @MainActor in
            AppState.shared.compose(req)
            NSApp.activate(ignoringOtherApps: true)
        }
    }
}

// MARK: - Launch at login / default mail app

enum LoginItem {
    private static var service: SMAppService { .loginItem(identifier: Daemon.helperBundleId) }

    private static var agentURL: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/LaunchAgents/\(Daemon.helperBundleId).plist")
    }

    static var isEnabled: Bool {
        service.status == .enabled || FileManager.default.fileExists(atPath: agentURL.path)
    }

    static func setEnabled(_ on: Bool) throws {
        do {
            if on { try service.register() } else { try service.unregister() }
            if !on { try? FileManager.default.removeItem(at: agentURL) }
        } catch {
            // Ad-hoc signed builds may be refused by SMAppService: fall back to a LaunchAgent.
            if on {
                try installAgent()
            } else {
                try? FileManager.default.removeItem(at: agentURL)
            }
        }
    }

    private static func installAgent() throws {
        guard let helper = Daemon.helperURL else { throw APIError(status: 0, message: "找不到 MailboxAIHelper.app", code: "io") }
        let exe = helper.appendingPathComponent("Contents/MacOS/MailboxHelper").path
        let plist: [String: Any] = [
            "Label": Daemon.helperBundleId,
            "ProgramArguments": [exe],
            "RunAtLoad": true,
            "KeepAlive": ["SuccessfulExit": false],
            "ProcessType": "Interactive",
            "LimitLoadToSessionType": "Aqua",
        ]
        try FileManager.default.createDirectory(at: agentURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        let data = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
        try data.write(to: agentURL)
    }
}

enum DefaultMailApp {
    static var isDefault: Bool {
        guard let probe = URL(string: "mailto:someone@example.com"),
              let app = NSWorkspace.shared.urlForApplication(toOpen: probe) else { return false }
        return app.standardizedFileURL == Bundle.main.bundleURL.standardizedFileURL
    }

    static func makeDefault() async throws {
        try await NSWorkspace.shared.setDefaultApplication(at: Bundle.main.bundleURL, toOpenURLsWithScheme: "mailto")
    }
}

// MARK: - Spotlight

enum SpotlightIndexer {
    static func index(_ messages: [MessageSummary], accounts: [String: Account]) {
        guard !messages.isEmpty else { return }
        let items = messages.map { m -> CSSearchableItem in
            let attrs = CSSearchableItemAttributeSet(contentType: .emailMessage)
            attrs.title = m.subject.isEmpty ? "（无主题）" : m.subject
            attrs.contentDescription = m.snippet
            attrs.authorNames = [m.from.display]
            attrs.authorEmailAddresses = [m.from.email]
            attrs.emailAddresses = m.to.map(\.email)
            attrs.contentCreationDate = Date(timeIntervalSince1970: TimeInterval(m.date))
            attrs.keywords = [accounts[m.accountId]?.displayLabel ?? ""] + m.labels
            return CSSearchableItem(uniqueIdentifier: "message:\(m.id)", domainIdentifier: m.accountId, attributeSet: attrs)
        }
        CSSearchableIndex.default().indexSearchableItems(items, completionHandler: nil)
    }

    static func clear() {
        CSSearchableIndex.default().deleteAllSearchableItems(completionHandler: nil)
    }

    static func messageId(from activity: NSUserActivity) -> Int64? {
        guard let id = activity.userInfo?[CSSearchableItemActivityIdentifier] as? String, id.hasPrefix("message:") else { return nil }
        return Int64(id.dropFirst(8))
    }
}

// MARK: - App lock

struct LockView: View {
    @EnvironmentObject private var state: AppState
    @State private var error: String?

    var body: some View {
        ZStack {
            Rectangle().fill(.ultraThickMaterial)
            VStack(spacing: 14) {
                Image(systemName: "lock.fill").font(.system(size: 44))
                Text("Mailbox 已锁定").font(.title2)
                Button("使用 Touch ID 或密码解锁") { unlock() }
                    .keyboardShortcut(.defaultAction)
                if let error { Text(error).foregroundStyle(.red).font(.caption) }
            }
        }
        .onAppear { unlock() }
    }

    private func unlock() {
        let ctx = LAContext()
        var err: NSError?
        guard ctx.canEvaluatePolicy(.deviceOwnerAuthentication, error: &err) else {
            state.locked = false
            return
        }
        ctx.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: "解锁 Mailbox") { ok, e in
            DispatchQueue.main.async {
                if ok { state.locked = false } else { error = e?.localizedDescription }
            }
        }
    }
}

// MARK: - Shortcuts (App Intents)

struct ComposeMailIntent: AppIntent {
    static var title: LocalizedStringResource = "写邮件"
    static var description = IntentDescription("打开 Mailbox 写一封新邮件")
    static var openAppWhenRun = true

    @Parameter(title: "收件人") var to: String?
    @Parameter(title: "主题") var subject: String?
    @Parameter(title: "正文") var body: String?

    @MainActor
    func perform() async throws -> some IntentResult {
        var req = ComposeRequest()
        if let to { req.to = to.split(whereSeparator: { ",;".contains($0) }).map { $0.trimmingCharacters(in: .whitespaces) } }
        req.subject = subject
        req.body = body
        AppState.shared.compose(req)
        return .result()
    }
}

struct SendMailIntent: AppIntent {
    static var title: LocalizedStringResource = "发送邮件"
    static var description = IntentDescription("用默认账户直接发送一封邮件")

    @Parameter(title: "收件人") var to: String
    @Parameter(title: "主题") var subject: String
    @Parameter(title: "正文") var body: String

    func perform() async throws -> some IntentResult & ProvidesDialog {
        guard await Daemon.ensureRunning() else { return .result(dialog: "Mailbox 后台服务未运行") }
        let accounts = try await APIClient.shared.accounts().filter { $0.enabled && $0.kind != .web }
        guard let acc = accounts.first else { return .result(dialog: "还没有可用的邮箱账户") }
        var d = Draft()
        d.accountId = acc.id
        d.to = to.split(whereSeparator: { ",;".contains($0) }).map { EmailAddress(name: nil, email: $0.trimmingCharacters(in: .whitespaces)) }
        d.subject = subject
        d.html = "<div>" + ComposeModel.escape(body).replacingOccurrences(of: "\n", with: "<br>") + "</div>"
        _ = try await APIClient.shared.sendDraft(d)
        return .result(dialog: "已从 \(acc.displayLabel) 发送")
    }
}

struct UnreadCountIntent: AppIntent {
    static var title: LocalizedStringResource = "未读邮件数"

    func perform() async throws -> some IntentResult & ReturnsValue<Int> & ProvidesDialog {
        _ = await Daemon.ensureRunning()
        let s = try await APIClient.shared.status()
        return .result(value: s.unreadTotal, dialog: "共有 \(s.unreadTotal) 封未读邮件")
    }
}

struct MailboxShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(intent: ComposeMailIntent(), phrases: ["用 \(.applicationName) 写邮件"], shortTitle: "写邮件", systemImageName: "square.and.pencil")
        AppShortcut(intent: UnreadCountIntent(), phrases: ["\(.applicationName) 有多少未读邮件"], shortTitle: "未读邮件数", systemImageName: "envelope.badge")
    }
}

// MARK: - Menu commands

struct MailboxCommands: Commands {
    @ObservedObject var state: AppState

    private var selected: [Int64] { Array(state.selection) }

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("写新邮件") { state.compose() }.keyboardShortcut("n", modifiers: .command)
        }
        CommandMenu("邮件") {
            Button("检查新邮件") { state.sync() }.keyboardShortcut("n", modifiers: [.command, .shift])
            Divider()
            Button("回复") { if let id = selected.first { state.reply(id, mode: .reply) } }
                .keyboardShortcut("r", modifiers: .command).disabled(selected.count != 1)
            Button("全部回复") { if let id = selected.first { state.reply(id, mode: .replyAll) } }
                .keyboardShortcut("r", modifiers: [.command, .shift]).disabled(selected.count != 1)
            Button("转发") { if let id = selected.first { state.reply(id, mode: .forward) } }
                .keyboardShortcut("f", modifiers: [.command, .shift]).disabled(selected.count != 1)
            Divider()
            Button("归档") { Task { await state.act(selected, .archive) } }
                .keyboardShortcut("e", modifiers: [.command, .control]).disabled(selected.isEmpty)
            Button("标为已读") { Task { await state.act(selected, .read) } }
                .keyboardShortcut("u", modifiers: [.command, .shift]).disabled(selected.isEmpty)
            Button("标为未读") { Task { await state.act(selected, .unread) } }
                .keyboardShortcut("u", modifiers: [.command, .option]).disabled(selected.isEmpty)
            Button("加星标 / 取消星标") {
                let flagged = state.messages.filter { selected.contains($0.id) }.allSatisfy(\.flags.flagged)
                Task { await state.act(selected, flagged ? .unflag : .flag) }
            }
            .keyboardShortcut("l", modifiers: [.command, .shift]).disabled(selected.isEmpty)
            Button("标记为垃圾邮件") { Task { await state.act(selected, .spam) } }
                .keyboardShortcut("j", modifiers: [.command, .shift]).disabled(selected.isEmpty)
            Divider()
            Button("全部标为已读") { Task { await state.markAllRead() } }
                .keyboardShortcut("c", modifiers: [.command, .shift])
            Button("撤销") { Task { await state.undoLast() } }
                .disabled(state.toast?.undoToken == nil && state.toast?.outboxId == nil)
        }
    }
}
