import AppKit
import SwiftUI

/// App-wide AI state: settings, providers, smart reminders and live event fan-out.
@MainActor
final class AIState: ObservableObject {
    static let shared = AIState()
    /// Sidebar tag of the "✨ 待处理" smart mailbox.
    static let pendingMailbox = "ai:pending"

    let api = APIClient.shared

    @Published var settings = AISettings()
    @Published var providers: [AIProvider] = []
    @Published var reminders: [AIReminderItem] = []
    /// Insights fetched for opened messages (also refreshed by `ai_insight`).
    @Published var insights: [Int64: AIInsight] = [:]
    /// Latest `ai_job` progress by job id.
    @Published var jobProgress: [String: JobProgress] = [:]
    /// Bumped on every `ai_loop` event so Loop lists can refresh.
    @Published var loopEvents = 0
    @Published var lastLoopId: String?
    /// Message ids for the "✨ AI 批量处理" sheet (non-empty = shown).
    @Published var batchIds: [Int64] = []
    /// Prompt library (§8.8).
    @Published var prompts: [AIPromptPreset] = []
    /// MCP send / delete requests waiting for the user (§8.2), oldest first.
    @Published var confirmations: [AIConfirmation] = []
    /// Bumped on every `ai_confirm` event and decision so lists can refresh.
    @Published var confirmEvents = 0

    struct JobProgress: Hashable {
        var done: Int
        var total: Int
        var status: String
    }

    private var remindersTask: Task<Void, Never>?
    private var settingsTask: Task<Void, Never>?

    var isConfigured: Bool { settings.isConfigured }

    // MARK: Loading

    func reload() async {
        async let s = try? api.aiSettings()
        async let p = try? api.aiProviders()
        async let pr = try? api.aiPrompts()
        if let s = await s { settings = s }
        if let p = await p { providers = p }
        if let pr = await pr { prompts = pr }
        await reloadReminders()
    }

    /// Saves settings right away (e.g. the MCP switch, whose page reloads right after).
    func saveSettingsNow() async {
        settingsTask?.cancel()
        if let saved = try? await api.saveAISettings(settings) { settings = saved }
    }

    // MARK: Prompt library

    func reloadPrompts() async {
        if let p = try? await api.aiPrompts() { prompts = p }
    }

    /// Prompts of one scope ("compose" | "reader" | "batch"), in their saved order.
    func promptList(scope: String) -> [AIPromptPreset] {
        prompts.filter { $0.scope == scope }.sorted { a, b in
            a.sort != b.sort ? a.sort < b.sort : a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
    }

    // MARK: MCP confirmations

    /// Replaces the queue with the engine's pending list (kept as is when the request fails).
    func refreshConfirmations() async {
        guard let list = try? await api.aiConfirmations() else { return }
        let now = Int64(Date().timeIntervalSince1970)
        confirmations = list.filter { $0.expiresAt == 0 || $0.expiresAt > now }.sorted { $0.createdAt < $1.createdAt }
    }

    /// Allows / refuses a request; a 404 means it was already decided (helper notification) or expired.
    func decide(_ c: AIConfirmation, approve: Bool) async {
        confirmations.removeAll { $0.id == c.id }
        do {
            try await api.answerAIConfirmation(c.id, approve: approve)
            AppState.shared.show(Toast(text: (approve ? "已允许：" : "已拒绝：") + (c.title.isEmpty ? "MCP 请求" : c.title)))
        } catch let e as APIError where e.status == 404 {
            // Decided elsewhere or timed out: nothing to do.
        } catch {
            AppState.shared.show(Toast(text: error.localizedDescription, isError: true))
        }
        confirmEvents += 1
    }

    func reloadReminders() async {
        if let r = try? await api.aiReminders() { reminders = r }
    }

    func scheduleRemindersReload() {
        remindersTask?.cancel()
        remindersTask = Task {
            try? await Task.sleep(nanoseconds: 800_000_000)
            if Task.isCancelled { return }
            await reloadReminders()
            if AppState.shared.selectedMailbox == Self.pendingMailbox { await AppState.shared.reloadMessages() }
        }
    }

    /// Saves settings shortly after the last change so sliders and text fields stay responsive.
    func scheduleSaveSettings() {
        settingsTask?.cancel()
        let snapshot = settings
        settingsTask = Task {
            try? await Task.sleep(nanoseconds: 500_000_000)
            if Task.isCancelled { return }
            _ = try? await api.saveAISettings(snapshot)
        }
    }

    func reloadProviders() async {
        if let p = try? await api.aiProviders() { providers = p }
    }

    @discardableResult
    func loadInsight(_ id: Int64) async -> AIInsight? {
        if let i = try? await api.aiInsight(id) {
            insights[id] = i
            return i
        }
        return insights[id]
    }

    func setDone(_ id: Int64, done: Bool) async {
        do {
            try await api.setAIReminderDone(id, done: done)
            insights[id]?.done = done
            if done { reminders.removeAll { $0.message.id == id } }
            AppState.shared.show(Toast(text: done ? "已标为已处理" : "已恢复为待处理"))
            if AppState.shared.selectedMailbox == Self.pendingMailbox { await AppState.shared.reloadMessages() }
            await reloadReminders()
        } catch {
            AppState.shared.show(Toast(text: error.localizedDescription, isError: true))
        }
    }

    func insightForList(_ id: Int64) -> AIInsight? {
        reminders.first { $0.message.id == id }?.insight
    }

    /// The pending entry is a sent message still waiting for a reply (§8.7).
    func isFollowUp(_ id: Int64) -> Bool {
        reminders.first { $0.message.id == id }?.isFollowUp ?? false
    }

    // MARK: Models

    struct ModelOption: Hashable, Identifiable {
        var ref: AIModelRef
        var label: String
        var capabilities: [String] = []
        var id: String { ref.key }
    }

    /// Enabled models of enabled providers, as "服务商 · 模型".
    var modelOptions: [ModelOption] {
        providers.filter(\.enabled).flatMap { p in
            p.models.filter(\.enabled).map { m in
                ModelOption(ref: AIModelRef(provider: p.id, model: m.id), label: "\(p.name) · \(m.name)", capabilities: m.capabilities)
            }
        }
    }

    /// Models tagged with a capability ("vision", "embedding", …).
    func capableModels(_ capability: String) -> [ModelOption] {
        modelOptions.filter { $0.capabilities.contains(capability) }
    }

    func label(for ref: AIModelRef?) -> String {
        guard let ref else { return "未设置" }
        if let o = modelOptions.first(where: { $0.ref == ref }) { return o.label }
        let pname = providers.first { $0.id == ref.provider }?.name ?? ref.provider
        return "\(pname) · \(ref.model)"
    }

    // MARK: Events

    func handle(_ event: MailEvent) {
        switch event {
        case let .aiInsight(id):
            Task { await loadInsight(id) }
            scheduleRemindersReload()
        case .aiReminder:
            scheduleRemindersReload()
        case let .aiJob(jobId, done, total, status):
            jobProgress[jobId] = JobProgress(done: done, total: total, status: status)
        case let .aiLoop(loopId, _, name, status, _):
            lastLoopId = loopId
            loopEvents += 1
            if status == "failed" || status == "error" {
                AppState.shared.show(Toast(text: "✨ \(name) 运行失败", isError: true))
            }
        case let .aiAutoreply(_, ruleName, _, sent, subject):
            let what = sent ? "已自动回复" : "已生成回复草稿"
            AppState.shared.show(Toast(text: "✨ \(what)：\(subject.isEmpty ? ruleName : subject)"))
        case let .aiConfirm(id, kind, title, detail):
            if !confirmations.contains(where: { $0.id == id }) {
                confirmations.append(AIConfirmation(id: id, kind: kind, title: title, detail: detail))
            }
            confirmEvents += 1
            WindowRouter.shared.showConfirm()
            Task { await refreshConfirmations() }
        default:
            break
        }
    }

    // MARK: Navigation

    /// Opens Settings on one of the AI tabs ("ai" or "ai_features"), optionally on an AI 功能 pane.
    static func openSettings(tab: String = "ai", pane: String? = nil) {
        UserDefaults.standard.set(tab, forKey: "settingsTab")
        if let pane { UserDefaults.standard.set(normalizedPane(pane), forKey: "aiFeaturesPane") }
        WindowRouter.shared.showSettings()
    }

    /// AI 功能 panes: general | autoreply | loops | mcp | prompts | usage ("loop" is accepted for "loops").
    static func normalizedPane(_ pane: String) -> String {
        pane == "loop" ? "loops" : pane
    }
}

// MARK: - Streaming

/// Runs one NDJSON stream and publishes its text as it arrives.
@MainActor
final class AIStreamModel: ObservableObject {
    @Published private(set) var text = ""
    @Published private(set) var running = false
    @Published private(set) var error: String?
    /// The engine answered `ai_not_configured`: show a link to Settings → AI 服务.
    @Published private(set) var notConfigured = false
    @Published private(set) var result: AIStreamLine?
    /// Called on the main actor when a stream finishes successfully.
    var onDone: ((AIStreamLine) -> Void)?

    private var task: Task<Void, Never>?
    private var lastPath: String?
    private var lastBody: (any Encodable)?
    /// Guards against a cancelled stream finishing after a newer one started.
    private var generation = 0

    var hasOutput: Bool { !text.isEmpty }
    var finished: Bool { result != nil }
    var canRetry: Bool { lastPath != nil && !running }

    func start(_ path: String, body: any Encodable) {
        task?.cancel()
        lastPath = path
        lastBody = body
        text = ""
        error = nil
        notConfigured = false
        result = nil
        running = true
        generation += 1
        let gen = generation
        let stream = APIClient.shared.ndjson(path, body: body)
        task = Task {
            do {
                for try await line in stream {
                    if Task.isCancelled || gen != self.generation { break }
                    if let d = line.delta { self.text += d }
                    if let e = line.error, !e.isEmpty { self.error = e }
                    if line.done == true {
                        if let t = line.text, !t.isEmpty { self.text = t }
                        self.result = line
                    }
                }
                if !Task.isCancelled, gen == self.generation, self.result == nil, self.error == nil, self.text.isEmpty {
                    self.error = "AI 没有返回内容"
                }
            } catch let e as APIError {
                if !Task.isCancelled && gen == self.generation {
                    self.error = e.message
                    self.notConfigured = e.code == "ai_not_configured"
                }
            } catch {
                if !Task.isCancelled && gen == self.generation && !(error is CancellationError) {
                    self.error = error.localizedDescription
                }
            }
            guard gen == self.generation else { return }
            self.running = false
            if !Task.isCancelled, let r = self.result, self.error == nil { self.onDone?(r) }
        }
    }

    func retry() {
        guard let path = lastPath, let body = lastBody else { return }
        start(path, body: body)
    }

    func cancel() {
        task?.cancel()
        task = nil
        generation += 1
        running = false
    }

    /// Clears output and errors (keeps the last request for `retry`).
    func reset() {
        cancel()
        text = ""
        error = nil
        notConfigured = false
        result = nil
    }

    /// Shows an already-known text (e.g. a cached summary) as a finished result.
    func show(cached: String) {
        cancel()
        text = cached
        error = nil
        notConfigured = false
    }
}

// MARK: - Text helpers

enum AIText {
    /// AI plain text → editor HTML: escaped, blank lines split paragraphs, single newlines become <br>.
    static func html(_ text: String) -> String {
        let normalized = text.replacingOccurrences(of: "\r\n", with: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
        var paragraphs: [String] = []
        var current: [String] = []
        for line in normalized.components(separatedBy: "\n") {
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                if !current.isEmpty {
                    paragraphs.append(current.joined(separator: "\n"))
                    current = []
                }
            } else {
                current.append(line)
            }
        }
        if !current.isEmpty { paragraphs.append(current.joined(separator: "\n")) }
        return paragraphs
            .map { "<p>" + ComposeModel.escape($0).replacingOccurrences(of: "\n", with: "<br>") + "</p>" }
            .joined()
    }

    /// Markdown → text suitable for reading aloud.
    static func speakable(_ markdown: String) -> String {
        var out: [String] = []
        for raw in markdown.components(separatedBy: "\n") {
            var line = raw.trimmingCharacters(in: .whitespaces)
            while line.hasPrefix("#") { line.removeFirst() }
            if line.hasPrefix("- ") || line.hasPrefix("* ") || line.hasPrefix("• ") { line = String(line.dropFirst(2)) }
            line = line.replacingOccurrences(of: "**", with: "").replacingOccurrences(of: "__", with: "").replacingOccurrences(of: "`", with: "")
            let t = line.trimmingCharacters(in: .whitespaces)
            if !t.isEmpty { out.append(t) }
        }
        return out.joined(separator: "\n")
    }

    /// Plain text of a message for reading aloud.
    static func body(of d: MessageDetail) -> String {
        if let t = d.text, !t.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return t }
        let html = d.html ?? ""
        let stripped = html
            .replacingOccurrences(of: "<(style|script)[^>]*>[\\s\\S]*?</\\1>", with: "", options: .regularExpression)
            .replacingOccurrences(of: "<br\\s*/?>", with: "\n", options: .regularExpression)
            .replacingOccurrences(of: "</(p|div|li|tr|h[1-6])>", with: "\n", options: .regularExpression)
            .replacingOccurrences(of: "<[^>]+>", with: "", options: .regularExpression)
            .replacingOccurrences(of: "&nbsp;", with: " ")
            .replacingOccurrences(of: "&lt;", with: "<")
            .replacingOccurrences(of: "&gt;", with: ">")
            .replacingOccurrences(of: "&amp;", with: "&")
        return stripped.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    @MainActor
    static func copy(_ s: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
    }
}

enum AIFmt {
    private static func make(_ format: String) -> DateFormatter {
        let f = DateFormatter()
        f.locale = Locale(identifier: "zh_CN")
        f.dateFormat = format
        return f
    }

    private static let time = make("HH:mm")
    private static let weekday = make("EEE")
    private static let monthDay = make("M月d日")

    /// "今天 18:00" / "明天 9:00" / "周五 18:00" / "6月3日".
    static func deadline(_ ts: Int64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(ts))
        let cal = Calendar.current
        if cal.isDateInToday(date) { return "今天 " + time.string(from: date) }
        if cal.isDateInTomorrow(date) { return "明天 " + time.string(from: date) }
        if cal.isDateInYesterday(date) { return "昨天 " + time.string(from: date) }
        let days = cal.dateComponents([.day], from: cal.startOfDay(for: Date()), to: cal.startOfDay(for: date)).day ?? 99
        if days > 0 && days < 7 { return weekday.string(from: date) + " " + time.string(from: date) }
        return monthDay.string(from: date)
    }

    static func isOverdueSoon(_ ts: Int64) -> Bool {
        TimeInterval(ts) - Date().timeIntervalSince1970 < 24 * 3600
    }
}

// MARK: - Shared small views

/// Minimal Markdown: headings, bullet / numbered lists, paragraphs with inline styles.
/// With `citations`, "[n]" markers become `mailboxai-cite://n` links (handle via `.environment(\.openURL, …)`).
struct MarkdownText: View {
    let text: String
    var citations = false

    private enum Block: Hashable {
        case heading(String, Int)
        case bullet(String, Int)
        case numbered(String, String)
        case paragraph(String)
        case rule
        case gap
    }

    var body: some View {
        // Selectable text can swallow link clicks, so citation answers stay clickable instead.
        if citations {
            content
        } else {
            content.textSelection(.enabled)
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                view(for: block)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func view(for block: Block) -> some View {
        switch block {
        case let .heading(s, level):
            Text(inline(s)).font(level <= 1 ? Font.title3.weight(.semibold) : Font.headline).padding(.top, 2)
        case let .bullet(s, indent):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("•").foregroundStyle(.secondary)
                Text(inline(s)).fixedSize(horizontal: false, vertical: true)
            }
            .padding(.leading, CGFloat(indent) * 14)
        case let .numbered(n, s):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(n).monospacedDigit().foregroundStyle(.secondary)
                Text(inline(s)).fixedSize(horizontal: false, vertical: true)
            }
        case let .paragraph(s):
            Text(inline(s)).fixedSize(horizontal: false, vertical: true)
        case .rule:
            Divider()
        case .gap:
            Color.clear.frame(height: 2)
        }
    }

    private var blocks: [Block] {
        var out: [Block] = []
        for raw in text.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n") {
            let leading = raw.prefix { $0 == " " || $0 == "\t" }.count
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.isEmpty {
                if let last = out.last, last != .gap { out.append(.gap) }
                continue
            }
            if line == "---" || line == "***" {
                out.append(.rule)
                continue
            }
            if line.hasPrefix("#") {
                let level = line.prefix { $0 == "#" }.count
                let rest = line.dropFirst(level).trimmingCharacters(in: .whitespaces)
                if !rest.isEmpty {
                    out.append(.heading(rest, level))
                    continue
                }
            }
            if line.hasPrefix("- ") || line.hasPrefix("* ") || line.hasPrefix("• ") || line.hasPrefix("+ ") {
                out.append(.bullet(String(line.dropFirst(2)), min(3, leading / 2)))
                continue
            }
            if let dot = line.firstIndex(where: { $0 == "." || $0 == "、" || $0 == ")" }),
               dot != line.startIndex, line[..<dot].allSatisfy(\.isNumber), line[..<dot].count <= 3 {
                let after = line[line.index(after: dot)...]
                let rest = after.trimmingCharacters(in: .whitespaces)
                let spaced = line[dot] == "、" || after.first == " "
                if spaced && !rest.isEmpty {
                    out.append(.numbered(String(line[..<dot]) + ".", rest))
                    continue
                }
            }
            out.append(.paragraph(line))
        }
        while let last = out.last, last == .gap { out.removeLast() }
        return out
    }

    private func inline(_ s: String) -> AttributedString {
        var a = (try? AttributedString(markdown: s, options: AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(s)
        if citations { Self.linkCitations(&a) }
        return a
    }

    static func linkCitations(_ a: inout AttributedString) {
        let plain = String(a.characters)
        guard plain.contains("["), let re = try? NSRegularExpression(pattern: "\\[(\\d{1,3})\\]") else { return }
        let ns = plain as NSString
        for m in re.matches(in: plain, range: NSRange(location: 0, length: ns.length)).reversed() {
            guard let r = Range(m.range, in: plain), let nr = Range(m.range(at: 1), in: plain) else { continue }
            let lower = plain.distance(from: plain.startIndex, to: r.lowerBound)
            let length = plain.distance(from: r.lowerBound, to: r.upperBound)
            let chars = a.characters
            guard lower + length <= chars.count else { continue }
            let start = chars.index(chars.startIndex, offsetBy: lower)
            let end = chars.index(start, offsetBy: length)
            a[start..<end].link = URL(string: "mailboxai-cite://\(plain[nr])")
        }
    }
}

/// "AI 尚未设置" call to action.
struct AINotConfiguredView: View {
    var message = "还没有设置 AI 模型"

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "sparkles").foregroundStyle(.purple)
            Text(message).font(.callout)
            Spacer(minLength: 4)
            Button("前往 设置 → AI 服务") { AIState.openSettings(tab: "ai") }
                .buttonStyle(.link)
                .font(.callout)
        }
        .padding(8)
        .background(Color.purple.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
    }
}

/// Streaming status: spinner, red error with retry, or the not-configured link.
struct AIStreamStatus: View {
    @ObservedObject var stream: AIStreamModel

    var body: some View {
        if stream.notConfigured {
            AINotConfiguredView(message: stream.error ?? "还没有设置 AI 模型")
        } else if let err = stream.error {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                Text(err).font(.callout).foregroundStyle(.red).textSelection(.enabled)
                Spacer(minLength: 4)
                if stream.canRetry {
                    Button("重试") { stream.retry() }.buttonStyle(.link).font(.callout)
                }
            }
        } else if stream.running && !stream.hasOutput {
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("AI 正在思考…").font(.callout).foregroundStyle(.secondary)
            }
        }
    }
}

/// Small colored capsule used for insight chips.
struct AIChip: View {
    let text: String
    var symbol: String?
    var color: Color = .purple

    var body: some View {
        HStack(spacing: 3) {
            if let symbol { Image(systemName: symbol).font(.system(size: 9, weight: .semibold)) }
            Text(text).font(.system(size: 11, weight: .medium)).lineLimit(1)
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 2)
        .foregroundStyle(color)
        .background(color.opacity(0.12), in: Capsule())
        // Chips keep their full label; neighbouring text truncates instead.
        .fixedSize()
    }
}
