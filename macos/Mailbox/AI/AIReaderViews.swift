import AppKit
import SwiftUI

/// Per-thread AI state for the reader (summary, translation, suggested replies, insight).
@MainActor
final class ReaderAIModel: ObservableObject {
    let summary = AIStreamModel()
    let translation = AIStreamModel()

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

    var hasCard: Bool { showSummary || showTranslation || showInsight }

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

    func cancelAll() {
        summary.cancel()
        translation.cancel()
    }
}

// MARK: - Toolbar menu

/// The reader toolbar "✨" menu.
struct AIReaderMenu: View {
    @ObservedObject var model: ReaderAIModel
    @ObservedObject private var speech = Speech.shared
    let messageId: Int64
    let current: MessageDetail?
    let threadCount: Int

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
                Button { askAI() } label: { Label("问问 AI…", systemImage: "sparkles") }
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

/// "需回复" "截止：周五" "高优先级" under the thread header.
struct AIInsightChips: View {
    @ObservedObject private var ai = AIState.shared
    let messageId: Int64

    var body: some View {
        if let i = ai.insights[messageId], i.isActionable || i.done {
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
                        model.cancelAll()
                        model.showSummary = false
                        model.showTranslation = false
                        model.showInsight = false
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
