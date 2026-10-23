import AppKit
import SwiftUI

struct Toast: Identifiable, Equatable {
    let id = UUID()
    var text: String
    var undoToken: String?
    var outboxId: String?
    var isError = false
}

enum ConnectionState: Equatable {
    case connecting
    case ready
    case failed(String)
}

/// What a compose window should start with.
struct ComposeRequest: Codable, Hashable, Identifiable {
    var id = UUID()
    var accountId: String?
    var draft: Draft?
    var replyTo: Int64?
    var mode: ComposeMode = .new
    var editServerDraft: Int64?
    var to: [String] = []
    var cc: [String] = []
    var bcc: [String] = []
    var subject: String?
    var body: String?
    var attachments: [String] = []
}

@MainActor
final class AppState: ObservableObject {
    static let shared = AppState()
    let api = APIClient.shared

    @Published var connection: ConnectionState = .connecting
    @Published var accounts: [Account] = []
    @Published var folders: [String: [Folder]] = [:]
    @Published var statuses: [String: AccountStatus] = [:]
    @Published var settings = Settings()
    @Published var identities: [Identity] = []
    @Published var smartFolders: [SmartFolder] = []
    @Published var templates: [Template] = []
    @Published var counts: [String: Int] = [:]
    @Published var unreadTotal = 0 {
        didSet { NSApp.dockTile.badgeLabel = unreadTotal > 0 ? "\(unreadTotal)" : nil }
    }

    @Published var selectedMailbox: String = "unified:inbox"
    @Published var messages: [MessageSummary] = []
    @Published var selection: Set<Int64> = []
    @Published var searchText = ""
    @Published var listFilter: ListFilter = .all
    @Published var loadingMessages = false
    @Published var cursor: MessagePage.Cursor?
    @Published var toast: Toast?
    @Published var pendingCompose: ComposeRequest?
    @Published var showAddAccount = false
    @Published var showOnboarding = false
    @Published var outbox: [OutboxItem] = []
    @Published var locked = false

    enum ListFilter: String, CaseIterable, Identifiable {
        case all, unread, starred, attachments
        var id: String { rawValue }
        var title: String {
            switch self {
            case .all: return "全部"
            case .unread: return "未读"
            case .starred: return "星标"
            case .attachments: return "有附件"
            }
        }
        var query: String? {
            switch self {
            case .all: return nil
            case .unread: return "is:unread"
            case .starred: return "is:starred"
            case .attachments: return "has:attachment"
            }
        }
    }

    private var events: EventStream?
    private var reloadTask: Task<Void, Never>?
    private var countsTask: Task<Void, Never>?
    private var toastTask: Task<Void, Never>?
    private var bootstrapped = false

    var accountsById: [String: Account] { Dictionary(uniqueKeysWithValues: accounts.map { ($0.id, $0) }) }

    var allFolders: [Folder] { folders.values.flatMap { $0 } }

    func folder(_ id: Int64) -> Folder? { allFolders.first { $0.id == id } }

    func account(for message: MessageSummary) -> Account? { accounts.first { $0.id == message.accountId } }

    // MARK: - Startup

    func bootstrap() async {
        guard !bootstrapped else { return }
        bootstrapped = true
        if settings.appLock { locked = true }
        connection = .connecting
        guard await Daemon.ensureRunning() else {
            connection = .failed("无法启动后台邮件服务（Mailbox Helper）。请确认应用位于“应用程序”文件夹后重试。")
            bootstrapped = false
            return
        }
        connection = .ready
        if !UserDefaults.standard.bool(forKey: "configuredLoginItem") {
            // Keep receiving mail after a restart by default; can be turned off in Settings → General.
            try? LoginItem.setEnabled(true)
            UserDefaults.standard.set(true, forKey: "configuredLoginItem")
        }
        await RemoteBlocker.shared.prepare()
        let stream = EventStream(handler: { event in
            Task { @MainActor in AppState.shared.handle(event) }
        }, onConnectionChange: { up in
            Task { @MainActor in
                if up { await AppState.shared.reloadAll() }
            }
        })
        events = stream
        stream.start()
        let demo = CommandLine.arguments.contains("--demo")
        if demo {
            try? await api.seedDemo()
            // Insights, a finished Loop, a cached summary and a default model, so AI screens have content.
            try? await api.seedAIDemo()
        }
        await reloadAll()
        await AIState.shared.reload()
        if demo, let first = messages.first { selection = [first.id] }
        if accounts.isEmpty { showOnboarding = true }
        if settings.appLock { locked = true }
        if let req = pendingCompose {
            pendingCompose = nil
            WindowRouter.shared.compose(req)
        }
    }

    func retryConnection() {
        Task { await bootstrap() }
    }

    func reloadAll() async {
        async let accs = try? api.accounts()
        async let fs = try? api.folders()
        async let st = try? api.settings()
        async let ids = try? api.identities()
        async let sf = try? api.smartFolders()
        async let tpl = try? api.templates()
        async let status = try? api.status()
        if let a = await accs { accounts = a }
        if let f = await fs { folders = Dictionary(grouping: f, by: \.accountId) }
        if let s = await st { settings = s }
        if let i = await ids { identities = i }
        if let s = await sf { smartFolders = s }
        if let t = await tpl { templates = t }
        if let s = await status {
            statuses = Dictionary(uniqueKeysWithValues: s.accounts.map { ($0.accountId, $0) })
            unreadTotal = s.unreadTotal
        }
        await reloadMessages()
        await reloadCounts()
    }

    func reloadFolders() async {
        if let f = try? await api.folders() { folders = Dictionary(grouping: f, by: \.accountId) }
    }

    // MARK: - Message list

    var effectiveQuery: String? {
        let parts = [searchText.trimmingCharacters(in: .whitespaces), listFilter.query ?? ""].filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: " ")
    }

    var isSearching: Bool { !searchText.trimmingCharacters(in: .whitespaces).isEmpty }

    /// Searches span all mail; browsing stays in the selected mailbox.
    var listMailbox: String { isSearching ? "all" : selectedMailbox }

    func select(mailbox: String) {
        guard mailbox != selectedMailbox else { return }
        selectedMailbox = mailbox
        if mailbox.hasPrefix("web:") { return }
        selection = []
        messages = []
        cursor = nil
        if mailbox.hasPrefix("folder:"), let id = Int64(mailbox.dropFirst(7)) {
            Task { try? await api.syncFolder(id) }
        }
        Task { await reloadMessages() }
    }

    func reloadMessages() async {
        loadingMessages = true
        defer { loadingMessages = false }
        if selectedMailbox == AIState.pendingMailbox && !isSearching {
            // "✨ 待处理": messages that AI flagged (needs reply / deadline / todos).
            await AIState.shared.reloadReminders()
            messages = AIState.shared.reminders.map(\.message)
            cursor = nil
            selection = selection.filter { id in messages.contains { $0.id == id } }
            return
        }
        let threads = settings.conversationView && !selectedMailbox.hasPrefix("unified:drafts")
        let limit = max(100, messages.count)
        guard let page = try? await api.messages(mailbox: listMailbox, query: effectiveQuery, threads: threads, limit: limit) else { return }
        messages = page.messages
        cursor = page.cursor
        selection = selection.filter { id in messages.contains { $0.id == id } }
        if settings.spotlightIndex && listMailbox == "unified:inbox" {
            SpotlightIndexer.index(page.messages, accounts: accountsById)
        }
    }

    func loadMore() async {
        guard let cur = cursor, !loadingMessages else { return }
        loadingMessages = true
        defer { loadingMessages = false }
        let threads = settings.conversationView
        guard let page = try? await api.messages(mailbox: listMailbox, query: effectiveQuery, threads: threads, cursor: cur) else { return }
        let known = Set(messages.map(\.id))
        messages.append(contentsOf: page.messages.filter { !known.contains($0.id) })
        cursor = page.cursor
    }

    /// Free-text part of the search box (operators removed) for IMAP SEARCH.
    private var serverSearchText: String {
        searchText.split(separator: " ").filter { !$0.contains(":") }.joined(separator: " ")
    }

    @Published var searchingServer = false

    /// Searches mail outside the local cache on every IMAP account (or the selected one).
    func searchServer() async {
        let q = serverSearchText.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return }
        searchingServer = true
        defer { searchingServer = false }
        var targets = accounts.filter { $0.kind == .imap && $0.enabled }
        if selectedMailbox.hasPrefix("account:"), let id = selectedMailbox.split(separator: ":").dropFirst().first {
            targets = targets.filter { $0.id == id }
        }
        var found = 0
        for acc in targets {
            if let results = try? await api.serverSearch(account: acc.id, folder: nil, query: q) {
                let known = Set(messages.map(\.id))
                let fresh = results.filter { !known.contains($0.id) }
                found += fresh.count
                messages.append(contentsOf: fresh)
            }
        }
        messages.sort { $0.sortDate > $1.sortDate }
        show(Toast(text: found > 0 ? "服务器上又找到 \(found) 封邮件" : "服务器上没有更多结果"))
    }

    func scheduleReload(delay: UInt64 = 300_000_000) {
        reloadTask?.cancel()
        reloadTask = Task {
            try? await Task.sleep(nanoseconds: delay)
            if Task.isCancelled { return }
            await reloadMessages()
        }
        scheduleCounts()
    }

    func scheduleCounts() {
        countsTask?.cancel()
        countsTask = Task {
            try? await Task.sleep(nanoseconds: 500_000_000)
            if Task.isCancelled { return }
            await reloadCounts()
            await reloadFolders()
        }
    }

    func reloadCounts() async {
        var boxes = ["unified:inbox", "unread", "starred", "snoozed", "unified:drafts", "vip", "unified:junk"]
        boxes += accounts.map { "account:\($0.id):inbox" }
        boxes += Category.allCases.map { "category:\($0.rawValue)" }
        guard let c = try? await api.counts(boxes) else { return }
        var out: [String: Int] = [:]
        for (k, v) in c {
            out[k] = (k == "unified:drafts" || k == "snoozed") ? (v["total"] ?? 0) : (v["unread"] ?? 0)
        }
        counts = out
    }

    // MARK: - Actions

    func act(_ ids: [Int64], _ action: MessageAction, expandThreads: Bool = true) async {
        guard !ids.isEmpty else { return }
        var targets = ids
        if expandThreads && settings.conversationView {
            // A conversation row stands for all its messages in this view.
            let threadIds = Set(messages.filter { ids.contains($0.id) && $0.threadCount > 1 }.map(\.threadId))
            if !threadIds.isEmpty, let page = try? await api.messages(mailbox: listMailbox, threads: false, limit: 1000) {
                targets += page.messages.filter { threadIds.contains($0.threadId) }.map(\.id)
            }
        }
        targets = Array(Set(targets))
        let removing: Bool
        switch action {
        case .archive, .trash, .delete, .spam, .notSpam, .moveToInbox, .move, .snooze: removing = true
        default: removing = false
        }
        if removing {
            let next = nextSelection(after: Set(ids))
            withAnimation(.easeOut(duration: 0.15)) {
                messages.removeAll { ids.contains($0.id) }
            }
            selection = next.map { [$0] } ?? []
        } else {
            applyLocally(ids, action)
        }
        do {
            let r = try await api.act(targets, action)
            show(Toast(text: action.pastTense + (ids.count > 1 ? "（\(ids.count) 封）" : ""), undoToken: r.undoToken))
        } catch {
            show(Toast(text: error.localizedDescription, isError: true))
        }
        scheduleReload(delay: 600_000_000)
    }

    private func applyLocally(_ ids: [Int64], _ action: MessageAction) {
        for i in messages.indices where ids.contains(messages[i].id) {
            switch action {
            case .read: messages[i].flags.seen = true; messages[i].threadUnread = 0
            case .unread: messages[i].flags.seen = false; messages[i].threadUnread = max(1, messages[i].threadUnread)
            case .flag: messages[i].flags.flagged = true
            case .unflag: messages[i].flags.flagged = false
            case .pin: messages[i].pinned = true
            case .unpin: messages[i].pinned = false
            default: break
            }
        }
    }

    private func nextSelection(after removed: Set<Int64>) -> Int64? {
        guard let firstIdx = messages.firstIndex(where: { removed.contains($0.id) }) else { return nil }
        let after = messages[firstIdx...].first { !removed.contains($0.id) }
        let before = messages[..<firstIdx].last { !removed.contains($0.id) }
        return (after ?? before)?.id
    }

    func undoLast() async {
        guard let t = toast else { return }
        if let token = t.undoToken {
            let ok = (try? await api.undo(token)) ?? false
            show(Toast(text: ok ? "已撤销" : "无法撤销（已同步到服务器）", isError: !ok))
            await reloadMessages()
        } else if let outbox = t.outboxId {
            if let draft = try? await api.cancelSend(outbox) {
                show(Toast(text: "已撤销发送"))
                WindowRouter.shared.compose(ComposeRequest(draft: draft))
            } else {
                show(Toast(text: "邮件已发出，无法撤销", isError: true))
            }
        }
    }

    func show(_ t: Toast) {
        toast = t
        toastTask?.cancel()
        let seconds = t.outboxId != nil ? max(3, settings.undoSendSecs) : 6
        toastTask = Task {
            try? await Task.sleep(nanoseconds: UInt64(seconds) * 1_000_000_000)
            if !Task.isCancelled, toast?.id == t.id { toast = nil }
        }
    }

    func markAllRead() async {
        try? await api.markAllRead(mailbox: selectedMailbox)
        await reloadMessages()
        await reloadCounts()
    }

    func sync() {
        Task { try? await api.syncAccount(nil) }
    }

    // MARK: - Compose

    func compose(_ req: ComposeRequest = ComposeRequest()) {
        WindowRouter.shared.compose(req)
    }

    func reply(_ id: Int64, mode: ComposeMode) {
        compose(ComposeRequest(replyTo: id, mode: mode))
    }

    func didQueueSend(_ item: OutboxItem, scheduled: Bool) {
        if scheduled {
            show(Toast(text: "已安排在 \(Fmt.full(item.sendAt)) 发送", outboxId: item.id))
        } else {
            show(Toast(text: "正在发送…", outboxId: item.id))
        }
    }

    // MARK: - Events

    func handle(_ event: MailEvent) {
        switch event {
        case .messagesChanged, .snoozed:
            scheduleReload()
            AIState.shared.scheduleRemindersReload()
        case .aiInsight, .aiReminder, .aiJob, .aiLoop, .aiAutoreply, .aiConfirm:
            AIState.shared.handle(event)
        case .aiNotify:
            // Posted by the helper as a notification; the list refreshes through new_mail / messages_changed.
            AIState.shared.scheduleRemindersReload()
        case .newMail:
            scheduleReload(delay: 100_000_000)
        case .foldersChanged:
            Task { await reloadFolders() }
        case .accountsChanged:
            Task { await reloadAll() }
        case let .accountStatus(s):
            statuses[s.accountId] = s
        case let .unreadCounts(total, _):
            unreadTotal = total
            scheduleCounts()
        case let .outbox(id, status, error):
            if toast?.outboxId == id {
                switch status {
                case .sent: show(Toast(text: "已发送"))
                case .failed: show(Toast(text: "发送失败：\(error ?? "")", isError: true))
                default: break
                }
            } else if status == .failed, let error {
                show(Toast(text: "发送失败：\(error)", isError: true))
            }
            Task { outbox = (try? await api.outbox()) ?? [] }
        case .settingsChanged:
            Task { if let s = try? await api.settings() { settings = s } }
        case .resync:
            Task { await reloadAll() }
        default:
            break
        }
    }

    // MARK: - URLs

    func handle(url: URL) async {
        if url.scheme == "mailto" {
            compose(Self.composeRequest(mailto: url))
            return
        }
        guard url.scheme == "mailboxai" else { return }
        let parts = url.pathComponents.filter { $0 != "/" }
        switch url.host {
        case "message":
            if let s = parts.first, let id = Int64(s) { await open(messageId: id) }
        case "compose":
            var req = ComposeRequest()
            let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
            for item in items {
                guard let v = item.value else { continue }
                switch item.name {
                case "to": req.to.append(contentsOf: v.split(separator: ",").map { String($0) })
                case "subject": req.subject = v
                case "body": req.body = v
                case "file": req.attachments.append(v)
                default: break
                }
            }
            compose(req)
        case "settings":
            let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
            if let tab = items.first(where: { $0.name == "tab" })?.value {
                UserDefaults.standard.set(tab, forKey: "settingsTab")
            }
            // mailboxai://settings?tab=ai_features&pane=general|autoreply|loop|mcp|prompts|usage
            if let pane = items.first(where: { $0.name == "pane" })?.value, !pane.isEmpty {
                UserDefaults.standard.set(AIState.normalizedPane(pane), forKey: "aiFeaturesPane")
            }
            WindowRouter.shared.showSettings()
        case "confirm":
            // From the helper's AI_CONFIRM notification: mailboxai://confirm/<id>
            // (may arrive before bootstrap finished, so make sure the API is reachable first).
            _ = await Daemon.ensureRunning(timeout: 10)
            await AIState.shared.refreshConfirmations()
            if AIState.shared.confirmations.isEmpty {
                WindowRouter.shared.showMain()
                show(Toast(text: "这个请求已经处理或已过期"))
            } else {
                WindowRouter.shared.showConfirm()
            }
        case "add-account":
            WindowRouter.shared.showMain()
            showAddAccount = true
        case "assistant":
            let loop = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first(where: { $0.name == "loop" })?.value
            WindowRouter.shared.showAssistant(loop: loop)
        case "pending":
            WindowRouter.shared.showMain()
            searchText = ""
            select(mailbox: AIState.pendingMailbox)
        case "draft":
            if let id = parts.first, let d = try? await api.draft(id) {
                compose(ComposeRequest(draft: d))
            } else {
                WindowRouter.shared.showMain()
            }
        default:
            WindowRouter.shared.showMain()
        }
    }

    func open(messageId id: Int64) async {
        WindowRouter.shared.showMain()
        guard let d = try? await api.message(id) else { return }
        searchText = ""
        let mailbox = "folder:\(d.summary.folderId)"
        if selectedMailbox != mailbox {
            selectedMailbox = mailbox
            await reloadMessages()
        }
        if !messages.contains(where: { $0.id == id }) {
            messages.insert(d.summary, at: 0)
        }
        selection = [id]
    }

    static func composeRequest(mailto url: URL) -> ComposeRequest {
        var req = ComposeRequest()
        let s = url.absoluteString
        let rest = s.hasPrefix("mailto:") ? String(s.dropFirst(7)) : s
        let pieces = rest.split(separator: "?", maxSplits: 1).map(String.init)
        if let to = pieces.first?.removingPercentEncoding, !to.isEmpty {
            req.to = to.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
        }
        if pieces.count > 1 {
            let comps = URLComponents(string: "x:?" + pieces[1])
            for item in comps?.queryItems ?? [] {
                guard let v = item.value else { continue }
                switch item.name.lowercased() {
                case "subject": req.subject = v
                case "body": req.body = v
                case "cc": req.cc = v.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
                case "bcc": req.bcc = v.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
                case "to": req.to += v.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
                default: break
                }
            }
        }
        return req
    }
}
