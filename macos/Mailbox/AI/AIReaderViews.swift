import AppKit
import PDFKit
import SwiftUI

/// Per-thread AI state for the reader (summary, translation, suggested replies, insight).
@MainActor
final class ReaderAIModel: ObservableObject {
    let summary = AIStreamModel()
    let translation = AIStreamModel()
    /// "AI 解读附件" (§8.5).
    let explain = AIStreamModel()
    /// A reader prompt from the prompt library (§8.8).
    let custom = AIStreamModel()

    @Published var showSummary = false
    @Published var showTranslation = false
    @Published var collapsed = false
    @Published var summaryThread = true
    @Published var summaryCached = false
    @Published var summaryModel: String?
    @Published var translateLang = "zh-CN"
    @Published var suggestions: [AIReplySuggestion] = []
    @Published var loadingSuggestions = false
    @Published var suggestionsError: String?
    @Published var suggestionsNotConfigured = false
    @Published var analyzing = false
    @Published var showInsight = false
    @Published var showExplain = false
    @Published var explainName = ""
    /// Downloading the attachment / extracting PDF text before the stream starts.
    @Published var preparingExplain = false
    @Published var explainError: String?
    @Published var showCustom = false
    @Published var customName = ""
    /// "提取日程到日历" sheet (§8.1).
    @Published var showEvents = false

    private(set) var messageId: Int64 = 0
    private var prepared = false
    private var speakWhenDone = false

    init() {
        summary.onDone = { [weak self] line in
            guard let self else { return }
            self.summaryCached = line.cached ?? false
            self.summaryModel = line.model
            if self.speakWhenDone {
                self.speakWhenDone = false
                Speech.shared.speak(AIText.speakable(self.summary.text), settings: AIState.shared.settings)
            }
        }
    }

    var hasCard: Bool { showSummary || showTranslation || showInsight || showExplain || showCustom }

    /// Called once the thread loaded: cached summary, stored insight, optional auto-summary.
    func prepare(messageId: Int64, details: [MessageDetail]) {
        guard !prepared else { return }
        prepared = true
        self.messageId = messageId
        Task {
            await AIState.shared.loadInsight(messageId)
            if let cached = try? await APIClient.shared.aiCachedSummary(messageId), !cached.text.isEmpty, !summary.running {
                summary.show(cached: cached.text)
                summaryCached = true
                summaryModel = cached.model
                showSummary = true
            } else if AIState.shared.settings.autoSummary && AIState.shared.isConfigured {
                let length = details.map { AIText.body(of: $0).count }.reduce(0, +)
                if length > 1500 { summarize(thread: details.count > 1) }
            }
        }
    }

    func summarize(thread: Bool, force: Bool = false) {
        summaryThread = thread
        showSummary = true
        collapsed = false
        summaryCached = false
        summaryModel = nil
        summary.start("/v1/ai/messages/\(messageId)/summary", body: AISummaryBody(thread: thread, force: force))
    }

    func translate(to lang: String) {
        translateLang = lang
        showTranslation = true
        collapsed = false
        translation.start("/v1/ai/messages/\(messageId)/translate", body: AITranslateBody(targetLang: lang))
    }

    func speakSummary() {
        if summary.finished || (!summary.running && summary.hasOutput && summary.error == nil) {
            Speech.shared.speak(AIText.speakable(summary.text), settings: AIState.shared.settings)
        } else {
            speakWhenDone = true
            if !summary.running { summarize(thread: summaryThread) }
        }
    }

    func speakFull(_ detail: MessageDetail?) {
        guard let d = detail else { return }
        let text = "\(d.summary.from.display)：\(d.summary.subject)。\n" + AIText.body(of: d)
        Speech.shared.speak(text, settings: AIState.shared.settings)
    }

    func loadSuggestions() {
        guard !loadingSuggestions else { return }
        loadingSuggestions = true
        suggestionsError = nil
        suggestionsNotConfigured = false
        let id = messageId
        Task {
            do {
                suggestions = try await APIClient.shared.aiReplySuggestions(id)
                if suggestions.isEmpty { suggestionsError = "AI 没有给出建议" }
            } catch let e as APIError {
                suggestionsError = e.message
                suggestionsNotConfigured = e.code == "ai_not_configured"
            } catch {
                suggestionsError = error.localizedDescription
            }
            loadingSuggestions = false
        }
    }

    func analyze() {
        guard !analyzing else { return }
        analyzing = true
        showInsight = true
        collapsed = false
        let id = messageId
        Task {
            do {
                let i = try await APIClient.shared.analyzeAIInsight(id)
                AIState.shared.insights[id] = i
            } catch let e as APIError {
                if e.code == "ai_not_configured" {
                    AppState.shared.show(Toast(text: e.message, isError: true))
                } else {
                    AppState.shared.show(Toast(text: "分析失败：\(e.message)", isError: true))
                }
            } catch {
                AppState.shared.show(Toast(text: "分析失败：\(error.localizedDescription)", isError: true))
            }
            analyzing = false
        }
    }

    // MARK: Attachments (§8.5)

    /// Streams an explanation of an attachment of `messageId` (a message of this thread) into the card.
    /// PDFs are read locally with PDFKit and sent as `text`; images and text files are read by the engine.
    func explainAttachment(messageId mid: Int64, attachment a: Attachment) {
        explainName = a.filename
        explainError = nil
        showExplain = true
        collapsed = false
        explain.reset()
        let path = "/v1/ai/messages/\(mid)/attachments/\(a.id)/explain"
        guard Self.isPDF(a) else {
            preparingExplain = false
            explain.start(path, body: AIExplainBody(text: nil, question: nil))
            return
        }
        preparingExplain = true
        let limit = max(1000, AIState.shared.settings.maxBodyChars)
        Task {
            do {
                let url = try await APIClient.shared.attachmentFile(mid, a.id)
                let text = await Task.detached(priority: .userInitiated) { ReaderAIModel.pdfText(at: url, limit: limit) }.value
                preparingExplain = false
                guard explainName == a.filename, showExplain else { return }
                guard let text else {
                    explainError = "无法从这个 PDF 中提取文字（可能是扫描件或已加密）"
                    return
                }
                explain.start(path, body: AIExplainBody(text: text, question: nil))
            } catch {
                preparingExplain = false
                explainError = "无法读取附件：\(error.localizedDescription)"
            }
        }
    }

    nonisolated static func isPDF(_ a: Attachment) -> Bool {
        a.mimeType.lowercased() == "application/pdf" || a.filename.lowercased().hasSuffix(".pdf")
    }

    /// Text of a PDF (at most `limit` characters), or nil when it has none.
    nonisolated static func pdfText(at url: URL, limit: Int) -> String? {
        guard let doc = PDFDocument(url: url) else { return nil }
        var out = ""
        for i in 0..<doc.pageCount {
            if let s = doc.page(at: i)?.string, !s.isEmpty {
                out += s
                out += "\n"
            }
            if out.count >= limit { break }
        }
        let trimmed = out.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : String(trimmed.prefix(limit))
    }

    // MARK: Prompt library (§8.8)

    func runPrompt(_ p: AIPromptPreset, thread: Bool) {
        customName = p.name
        showCustom = true
        collapsed = false
        custom.start("/v1/ai/messages/\(messageId)/prompt", body: AIPromptBody(prompt: p.prompt, thread: thread))
    }

    func cancelAll() {
        summary.cancel()
        translation.cancel()
        explain.cancel()
        custom.cancel()
    }

    /// Closes every section of the card.
    func closeCard() {
        cancelAll()
        showSummary = false
        showTranslation = false
        showInsight = false
        showExplain = false
        showCustom = false
        preparingExplain = false
        explainError = nil
    }
}

// MARK: - Toolbar menu

/// The reader toolbar "✨" menu.
struct AIReaderMenu: View {
    @ObservedObject var model: ReaderAIModel
    @ObservedObject private var speech = Speech.shared
    @ObservedObject private var ai = AIState.shared
    let messageId: Int64
    let current: MessageDetail?
    let threadCount: Int
    /// The whole thread (attachments for "AI 解读附件").
    var details: [MessageDetail] = []

    private struct AttachmentRef: Identifiable {
        var messageId: Int64
        var attachment: Attachment
        var id: String { "\(messageId)-\(attachment.id)" }
    }

    private var attachments: [AttachmentRef] {
        var source: [MessageDetail] = details
        if source.isEmpty, let current { source = [current] }
        var refs: [AttachmentRef] = []
        for d in source {
            for a in d.attachments where !a.inline {
                refs.append(AttachmentRef(messageId: d.id, attachment: a))
            }
        }
        return refs
    }

    var body: some View {
        Menu {
            Section("摘要") {
                Button { model.summarize(thread: false) } label: { Label("总结这封邮件", systemImage: "text.bubble") }
                if threadCount > 1 {
                    Button { model.summarize(thread: true) } label: { Label("总结整个会话", systemImage: "text.bubble.fill") }
                }
            }
            Section("朗读") {
                Button { model.speakSummary() } label: { Label("朗读摘要", systemImage: "speaker.wave.2") }
                Button { model.speakFull(current) } label: { Label("朗读全文", systemImage: "speaker.wave.3") }
                if speech.speaking {
                    Button { speech.stop() } label: { Label("停止朗读", systemImage: "stop.circle") }
                }
            }
            Section("翻译") {
                Button { model.translate(to: "zh-CN") } label: { Label("翻译成中文", systemImage: "globe") }
                Button { model.translate(to: "en") } label: { Label("翻译成英文", systemImage: "globe") }
            }
            Section("处理") {
                Button { model.loadSuggestions() } label: { Label("建议回复", systemImage: "arrowshape.turn.up.left.circle") }
                Button { model.analyze() } label: { Label("分析待办", systemImage: "checklist") }
                Button { model.showEvents = true } label: { Label("提取日程到日历…", systemImage: "calendar.badge.plus") }
                if !attachments.isEmpty {
                    Menu {
                        ForEach(attachments) { ref in
                            Button(ref.attachment.filename) { model.explainAttachment(messageId: ref.messageId, attachment: ref.attachment) }
                        }
                    } label: {
                        Label("AI 解读附件", systemImage: "doc.text.magnifyingglass")
                    }
                }
                Button { askAI() } label: { Label("问问 AI…", systemImage: "sparkles") }
            }
            Section {
                Menu {
                    ForEach(ai.promptList(scope: "reader")) { p in
                        Button(p.name) { model.runPrompt(p, thread: threadCount > 1) }
                    }
                    if !ai.promptList(scope: "reader").isEmpty { Divider() }
                    Button("管理提示词…") { AIState.openSettings(tab: "ai_features", pane: "prompts") }
                } label: {
                    Label("我的提示词", systemImage: "text.book.closed")
                }
            }
        } label: {
            Label("AI", systemImage: "sparkles")
        }
        .help("AI 助手")
    }

    private func askAI() {
        guard let d = current else {
            WindowRouter.shared.showAssistant()
            return
        }
        AssistantModel.shared.attach(detail: d)
        WindowRouter.shared.showAssistant()
    }
}

// MARK: - Insight chips

/// "需回复" "截止：周五" "高优先级" "📅 日程 N" under the thread header.
struct AIInsightChips: View {
    @ObservedObject private var ai = AIState.shared
    let messageId: Int64
    /// Opens the "提取日程到日历" sheet.
    var onEvents: (() -> Void)? = nil

    var body: some View {
        if let i = ai.insights[messageId], i.isActionable || i.done || !i.events.isEmpty {
            HStack(spacing: 6) {
                Image(systemName: "sparkles").font(.caption).foregroundStyle(.purple)
                if i.done {
                    AIChip(text: "已处理", symbol: "checkmark", color: .green)
                } else {
                    if i.needsReply { AIChip(text: "需回复", symbol: "arrowshape.turn.up.left", color: .orange) }
                    if let d = i.deadline {
                        AIChip(text: "截止：" + AIFmt.deadline(d), symbol: "clock", color: AIFmt.isOverdueSoon(d) ? .red : .blue)
                            .help(i.deadlineText ?? "")
                    } else if let t = i.deadlineText, !t.isEmpty {
                        AIChip(text: "截止：" + t, symbol: "clock", color: .blue)
                    }
                    if i.priority == "high" { AIChip(text: "高优先级", symbol: "exclamationmark", color: .red) }
                    if !i.todos.isEmpty {
                        AIChip(text: "待办 \(i.todos.count)", symbol: "checklist", color: .purple)
                            .help(i.todos.map { "• " + $0 }.joined(separator: "\n"))
                    }
                }
                if !i.events.isEmpty {
                    Button {
                        onEvents?()
                    } label: {
                        AIChip(text: "📅 日程 \(i.events.count)", color: .teal)
                    }
                    .buttonStyle(.plain)
                    .help("加入日历：\n" + i.events.map { "• " + $0.title }.joined(separator: "\n"))
                }
                if !i.oneLine.isEmpty {
                    Text(i.oneLine).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.tail)
                        .layoutPriority(-1)
                        .help(i.oneLine)
                }
                Spacer(minLength: 4)
                Button(i.done ? "恢复待处理" : "标为已处理") {
                    Task { await ai.setDone(messageId, done: !i.done) }
                }
                .buttonStyle(.link)
                .font(.caption)
                .fixedSize()
            }
        }
    }
}

// MARK: - Risk banner

/// Red "AI 提示：疑似钓鱼 / 诈骗：理由" banner from `insight.risk` (§8.7).
struct AIRiskBanner: View {
    @ObservedObject private var ai = AIState.shared
    let messageId: Int64

    var body: some View {
        if let i = ai.insights[messageId], let title = i.riskTitle {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "exclamationmark.shield.fill")
                Text("AI 提示：" + title + (i.riskReason.isEmpty ? "" : "：" + i.riskReason))
                    .fontWeight(.medium)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                Spacer(minLength: 0)
            }
            .font(.callout)
            .foregroundStyle(.white)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Color.red, in: RoundedRectangle(cornerRadius: 8))
            .help("不要点击邮件中的链接，也不要提供密码、验证码或转账")
        }
    }
}

// MARK: - Thread card

/// Collapsible AI card at the top of the thread: summary, translation, todos.
struct AIThreadCard: View {
    @ObservedObject var model: ReaderAIModel
    @ObservedObject private var ai = AIState.shared
    let messageId: Int64

    var body: some View {
        if model.hasCard {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 6) {
                    Image(systemName: "sparkles").foregroundStyle(.purple)
                    Text("AI").font(.headline)
                    if let m = model.summaryModel, model.showSummary {
                        Text(m + (model.summaryCached ? " · 缓存" : "")).font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button {
                        withAnimation(.easeInOut(duration: 0.15)) { model.collapsed.toggle() }
                    } label: {
                        Image(systemName: model.collapsed ? "chevron.down" : "chevron.up")
                    }
                    .buttonStyle(.borderless)
                    .help(model.collapsed ? "展开" : "收起")
                    Button {
                        model.closeCard()
                    } label: {
                        Image(systemName: "xmark")
                    }
                    .buttonStyle(.borderless)
                    .help("关闭")
                }
                if !model.collapsed {
                    if model.showSummary {
                        AISummarySection(model: model, stream: model.summary)
                    }
                    if model.showTranslation {
                        if model.showSummary { Divider() }
                        AITranslationSection(model: model, stream: model.translation)
                    }
                    if model.showInsight {
                        if model.showSummary || model.showTranslation { Divider() }
                        insightSection
                    }
                    if model.showExplain {
                        if model.showSummary || model.showTranslation || model.showInsight { Divider() }
                        AIResultSection(title: "解读附件：" + model.explainName, symbol: "doc.text.magnifyingglass", stream: model.explain,
                                        preparing: model.preparingExplain, extraError: model.explainError) {
                            model.explain.cancel()
                            model.showExplain = false
                            model.explainError = nil
                        }
                    }
                    if model.showCustom {
                        if model.showSummary || model.showTranslation || model.showInsight || model.showExplain { Divider() }
                        AIResultSection(title: model.customName.isEmpty ? "我的提示词" : model.customName, symbol: "text.book.closed",
                                        stream: model.custom) {
                            model.custom.cancel()
                            model.showCustom = false
                        }
                    }
                }
            }
            .padding(14)
            .background(
                LinearGradient(colors: [Color.purple.opacity(0.08), Color.blue.opacity(0.05)], startPoint: .topLeading, endPoint: .bottomTrailing),
                in: RoundedRectangle(cornerRadius: 12)
            )
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Color.purple.opacity(0.25)))
        }
    }

    @ViewBuilder
    private var insightSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Label("待办分析", systemImage: "checklist").font(.subheadline.weight(.semibold))
                Spacer()
                if model.analyzing { ProgressView().controlSize(.small) }
            }
            if let i = ai.insights[messageId] {
                if !i.oneLine.isEmpty { Text(i.oneLine).font(.callout) }
                if i.todos.isEmpty {
                    Text(i.needsReply ? "需要回复，没有其他待办。" : "没有发现待办事项。").font(.callout).foregroundStyle(.secondary)
                } else {
                    ForEach(Array(i.todos.enumerated()), id: \.offset) { _, t in
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Image(systemName: "circle").font(.caption2).foregroundStyle(.secondary)
                            Text(t).font(.callout).textSelection(.enabled)
                        }
                    }
                }
                if let d = i.deadline {
                    Text("截止：\(Fmt.full(d))" + (i.deadlineText.map { "（\($0)）" } ?? "")).font(.caption).foregroundStyle(.secondary)
                }
                if let r = i.remindAt, !i.done {
                    Text("将在 \(Fmt.full(r)) 提醒你").font(.caption).foregroundStyle(.secondary)
                }
            } else if !model.analyzing {
                Text("还没有分析结果").font(.callout).foregroundStyle(.secondary)
            }
        }
    }
}

private struct AISummarySection: View {
    @ObservedObject var model: ReaderAIModel
    @ObservedObject var stream: AIStreamModel
    @ObservedObject private var speech = Speech.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(model.summaryThread ? "会话摘要" : "摘要", systemImage: "text.bubble").font(.subheadline.weight(.semibold))
            if stream.hasOutput {
                MarkdownText(text: stream.text)
            }
            AIStreamStatus(stream: stream)
            HStack(spacing: 12) {
                if stream.running { ProgressView().controlSize(.small) }
                Spacer()
                Button {
                    if speech.speaking { speech.stop() } else { model.speakSummary() }
                } label: {
                    Label(speech.speaking ? "停止" : "朗读", systemImage: speech.speaking ? "stop.circle" : "speaker.wave.2")
                }
                .disabled(!stream.hasOutput && !stream.running)
                Button {
                    AIText.copy(stream.text)
                    AppState.shared.show(Toast(text: "摘要已复制"))
                } label: {
                    Label("复制", systemImage: "doc.on.doc")
                }
                .disabled(!stream.hasOutput)
                Button {
                    model.summarize(thread: model.summaryThread, force: true)
                } label: {
                    Label("重新生成", systemImage: "arrow.clockwise")
                }
                .disabled(stream.running)
            }
            .buttonStyle(.borderless)
            .font(.callout)
        }
    }
}

/// A streamed Markdown result with copy / close (attachment explanations, saved prompts).
private struct AIResultSection: View {
    let title: String
    let symbol: String
    @ObservedObject var stream: AIStreamModel
    var preparing = false
    var extraError: String? = nil
    let onClose: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label(title, systemImage: symbol).font(.subheadline.weight(.semibold)).lineLimit(1)
                if stream.running || preparing { ProgressView().controlSize(.small) }
                Spacer()
                Button("复制") {
                    AIText.copy(stream.text)
                    AppState.shared.show(Toast(text: "已复制"))
                }
                .buttonStyle(.borderless)
                .disabled(!stream.hasOutput)
                Button("关闭") { onClose() }
                    .buttonStyle(.borderless)
            }
            .font(.callout)
            if preparing && !stream.running {
                Text("正在读取附件…").font(.callout).foregroundStyle(.secondary)
            }
            if let extraError {
                Label(extraError, systemImage: "exclamationmark.triangle.fill").font(.callout).foregroundStyle(.red)
            }
            if stream.hasOutput {
                MarkdownText(text: stream.text)
            }
            AIStreamStatus(stream: stream)
        }
    }
}

private struct AITranslationSection: View {
    @ObservedObject var model: ReaderAIModel
    @ObservedObject var stream: AIStreamModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label(model.translateLang == "en" ? "英文译文" : "中文译文", systemImage: "globe").font(.subheadline.weight(.semibold))
                if stream.running { ProgressView().controlSize(.small) }
                Spacer()
                Button("复制") {
                    AIText.copy(stream.text)
                    AppState.shared.show(Toast(text: "译文已复制"))
                }
                .buttonStyle(.borderless)
                .disabled(!stream.hasOutput)
                Button("关闭") {
                    stream.cancel()
                    model.showTranslation = false
                }
                .buttonStyle(.borderless)
            }
            .font(.callout)
            if stream.hasOutput {
                Text(stream.text)
                    .font(.body)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
            AIStreamStatus(stream: stream)
        }
    }
}

// MARK: - Suggested replies

/// Chips under the last message; a click opens a reply prefilled with the AI text.
struct AISuggestedReplies: View {
    @EnvironmentObject private var state: AppState
    @ObservedObject var model: ReaderAIModel
    let messageId: Int64

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: "sparkles").foregroundStyle(.purple)
                Text("建议回复").font(.subheadline.weight(.semibold))
                if model.loadingSuggestions { ProgressView().controlSize(.small) }
                Spacer()
                if !model.suggestions.isEmpty {
                    Button("换一批") {
                        model.suggestions = []
                        model.loadSuggestions()
                    }
                    .buttonStyle(.link)
                    .font(.caption)
                    .disabled(model.loadingSuggestions)
                }
            }
            if model.suggestions.isEmpty && !model.loadingSuggestions {
                if model.suggestionsNotConfigured {
                    AINotConfiguredView(message: model.suggestionsError ?? "还没有设置 AI 模型")
                } else {
                    HStack(spacing: 8) {
                        Button {
                            model.loadSuggestions()
                        } label: {
                            Label("生成回复建议", systemImage: "wand.and.stars")
                        }
                        if let err = model.suggestionsError {
                            Text(err).font(.caption).foregroundStyle(.red).lineLimit(2)
                        }
                    }
                }
            }
            if !model.suggestions.isEmpty {
                FlowLayout(spacing: 8) {
                    ForEach(model.suggestions) { s in
                        Button {
                            state.compose(ComposeRequest(replyTo: messageId, mode: .reply, body: s.text))
                        } label: {
                            Text(s.label)
                                .font(.callout)
                                .lineLimit(1)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 6)
                                .background(Color.accentColor.opacity(0.12), in: Capsule())
                                .overlay(Capsule().strokeBorder(Color.accentColor.opacity(0.35)))
                        }
                        .buttonStyle(.plain)
                        .help(s.text)
                    }
                }
            }
        }
        .padding(12)
        .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 12))
    }
}
