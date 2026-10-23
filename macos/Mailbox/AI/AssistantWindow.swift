import AppKit
import SwiftUI

/// "✨ AI 助手" window: ask-your-mailbox chat and Loop results.
@MainActor
final class AssistantModel: ObservableObject {
    static let shared = AssistantModel()

    struct Entry: Identifiable, Hashable {
        let id = UUID()
        var role: String
        var text: String
        var sources: [AISource] = []
        var model: String?
    }

    /// A message the user is reading, passed as context ("问问 AI").
    struct Context: Hashable {
        var messageId: Int64
        var subject: String
        var from: String
        var text: String
    }

    enum Tab: String, CaseIterable, Identifiable {
        case ask, loops
        var id: String { rawValue }
        var title: String { self == .ask ? "问问邮箱" : "Loop 定时任务" }
    }

    @Published var tab: Tab = .ask
    @Published var entries: [Entry] = []
    @Published var input = ""
    @Published var context: Context?
    @Published var loops: [AILoop] = []
    @Published var selectedLoop: String?
    @Published var runs: [AILoopRun] = []
    @Published var loadingRuns = false
    @Published var runningLoop: String?

    let stream = AIStreamModel()

    init() {
        stream.onDone = { [weak self] line in
            guard let self else { return }
            self.entries.append(Entry(role: "assistant", text: self.stream.text, sources: line.sources ?? [], model: line.model))
            self.stream.reset()
        }
    }

    // MARK: Ask

    func attach(detail d: MessageDetail) {
        var body = AIText.body(of: d)
        if body.count > 6000 { body = String(body.prefix(6000)) }
        context = Context(messageId: d.id, subject: d.summary.subject, from: d.summary.from.full, text: body)
        tab = .ask
    }

    func send(_ question: String? = nil) {
        let q = (question ?? input).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty, !stream.running else { return }
        input = ""
        entries.append(Entry(role: "user", text: q))
        if let ctx = context {
            let system = """
            你是用户的邮件助手。用户正在阅读下面这封邮件，请结合它回答问题；不知道就说不知道。用简洁的中文回答。

            发件人：\(ctx.from)
            主题：\(ctx.subject)

            \(ctx.text)
            """
            let history = entries.suffix(12).map { AIChatMessage(role: $0.role, content: $0.text) }
            stream.start("/v1/ai/chat", body: AIChatBody(model: nil, system: system, messages: Array(history)))
        } else {
            stream.start("/v1/ai/ask", body: AIAskBody(question: q, mailbox: nil))
        }
    }

    func clear() {
        stream.reset()
        entries = []
    }

    /// Opens the message behind citation `[n]` of an answer.
    func openCitation(_ n: Int, in entry: Entry) {
        guard let s = entry.sources.first(where: { $0.index == n }) else { return }
        Task { await AppState.shared.open(messageId: s.messageId) }
    }

    // MARK: Loops

    func loadLoops() async {
        loops = (try? await APIClient.shared.aiLoops()) ?? loops
        if selectedLoop == nil || !loops.contains(where: { $0.id == selectedLoop }) {
            selectedLoop = loops.first { $0.lastRun != nil }?.id ?? loops.first?.id
        }
        await loadRuns()
    }

    func loadRuns() async {
        guard let id = selectedLoop else {
            runs = []
            return
        }
        loadingRuns = true
        defer { loadingRuns = false }
        let r = (try? await APIClient.shared.aiLoopRuns(id)) ?? []
        if selectedLoop == id { runs = r.sorted { $0.startedAt > $1.startedAt } }
    }

    func focus(loop id: String) {
        tab = .loops
        selectedLoop = id
        Task { await loadLoops() }
    }

    func run(_ id: String) {
        runningLoop = id
        Task {
            do {
                _ = try await APIClient.shared.runAILoop(id)
                AppState.shared.show(Toast(text: "已开始运行，完成后会通知你"))
            } catch {
                AppState.shared.show(Toast(text: error.localizedDescription, isError: true))
                runningLoop = nil
            }
        }
    }

    func loopFinished(_ id: String?) {
        if runningLoop == id { runningLoop = nil }
        Task { await loadLoops() }
    }
}

struct AssistantWindow: View {
    @EnvironmentObject private var state: AppState
    @ObservedObject private var model = AssistantModel.shared
    @ObservedObject private var ai = AIState.shared

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Picker("", selection: $model.tab) {
                    ForEach(AssistantModel.Tab.allCases) { t in Text(t.title).tag(t) }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 260)
                Spacer()
                if model.tab == .ask && !model.entries.isEmpty {
                    Button { model.clear() } label: { Label("新对话", systemImage: "square.and.pencil") }
                        .buttonStyle(.borderless)
                }
                if model.tab == .loops {
                    Button { AIState.openSettings(tab: "ai_features") } label: { Label("管理 Loop…", systemImage: "gearshape") }
                        .buttonStyle(.borderless)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            Divider()
            if !ai.isConfigured {
                AINotConfiguredView(message: "还没有设置 AI 模型，助手暂时无法回答")
                    .padding([.horizontal, .top], 12)
            }
            switch model.tab {
            case .ask:
                AssistantChatView(model: model, stream: model.stream)
            case .loops:
                AssistantLoopsView(model: model)
            }
        }
        .frame(minWidth: 620, minHeight: 480)
        .navigationTitle("AI 助手")
        .onChange(of: ai.loopEvents) { _, _ in model.loopFinished(ai.lastLoopId) }
        .task { await model.loadLoops() }
    }
}

// MARK: - Chat

private struct AssistantChatView: View {
    @ObservedObject var model: AssistantModel
    @ObservedObject var stream: AIStreamModel
    @FocusState private var inputFocused: Bool

    private let starters = ["最近有哪些需要我回复的邮件？", "这周收到了哪些账单和发票？", "总结一下今天的未读邮件", "有没有快到截止日期的事情？"]

    var body: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 14) {
                        if model.entries.isEmpty && !stream.running && stream.error == nil {
                            welcome
                        }
                        ForEach(model.entries) { e in
                            bubble(e).id(e.id)
                        }
                        if stream.running || stream.error != nil || stream.hasOutput {
                            liveBubble.id("live")
                        }
                        Color.clear.frame(height: 1).id("bottom")
                    }
                    .padding(16)
                }
                .onChange(of: model.entries.count) { _, _ in
                    withAnimation { proxy.scrollTo("bottom", anchor: .bottom) }
                }
                .onChange(of: stream.text) { _, _ in proxy.scrollTo("bottom", anchor: .bottom) }
            }
            Divider()
            if let ctx = model.context {
                HStack(spacing: 6) {
                    Image(systemName: "envelope").foregroundStyle(.secondary)
                    Text("正在讨论：\(ctx.subject.isEmpty ? "（无主题）" : ctx.subject)").font(.caption).lineLimit(1)
                    Button {
                        Task { await AppState.shared.open(messageId: ctx.messageId) }
                    } label: { Image(systemName: "arrow.up.forward.square") }
                    .buttonStyle(.borderless)
                    .help("打开邮件")
                    Spacer()
                    Button { model.context = nil } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.borderless)
                        .help("不再带上这封邮件，改为搜索整个邮箱")
                }
                .padding(.horizontal, 14)
                .padding(.top, 8)
            }
            HStack(alignment: .bottom, spacing: 8) {
                TextField(model.context == nil ? "问问你的邮箱，例如：上次张三说的报价是多少？" : "关于这封邮件，想问什么？",
                          text: $model.input, axis: .vertical)
                    .lineLimit(1...5)
                    .textFieldStyle(.roundedBorder)
                    .focused($inputFocused)
                    .onSubmit { model.send() }
                Button {
                    model.send()
                } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.title2)
                }
                .buttonStyle(.borderless)
                .disabled(model.input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || stream.running)
                .keyboardShortcut(.return, modifiers: .command)
            }
            .padding(12)
        }
        .onAppear { inputFocused = true }
    }

    private var welcome: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("问问邮箱", systemImage: "sparkles").font(.title3.weight(.semibold))
            Text("AI 会先在本地邮件里搜索相关内容，再回答你的问题。回答中的 [1] [2] 可以点击打开对应邮件。")
                .font(.callout).foregroundStyle(.secondary)
            FlowLayout(spacing: 8) {
                ForEach(starters, id: \.self) { q in
                    Button(q) { model.send(q) }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                }
            }
        }
        .padding(.vertical, 20)
    }

    @ViewBuilder
    private func bubble(_ e: AssistantModel.Entry) -> some View {
        if e.role == "user" {
            HStack {
                Spacer(minLength: 60)
                Text(e.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(Color.accentColor.opacity(0.15), in: RoundedRectangle(cornerRadius: 12))
            }
        } else {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "sparkles").foregroundStyle(.purple).padding(.top, 2)
                VStack(alignment: .leading, spacing: 8) {
                    MarkdownText(text: e.text, citations: !e.sources.isEmpty)
                        .environment(\.openURL, OpenURLAction { url in
                            if url.scheme == "mailboxai-cite", let n = Int(url.host ?? "") {
                                model.openCitation(n, in: e)
                                return .handled
                            }
                            return .systemAction
                        })
                    if !e.sources.isEmpty {
                        VStack(alignment: .leading, spacing: 3) {
                            Text("来源").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                            ForEach(e.sources) { s in
                                Button {
                                    model.openCitation(s.index, in: e)
                                } label: {
                                    HStack(spacing: 4) {
                                        Text("[\(s.index)]").monospacedDigit().foregroundStyle(Color.accentColor)
                                        Text(s.subject.isEmpty ? "（无主题）" : s.subject).lineLimit(1)
                                        if !s.from.isEmpty { Text("· \(s.from)").foregroundStyle(.secondary).lineLimit(1) }
                                        if let d = s.date { Text("· \(Fmt.relative(d))").foregroundStyle(.secondary) }
                                    }
                                    .font(.caption)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    HStack(spacing: 10) {
                        Button {
                            AIText.copy(e.text)
                        } label: { Label("复制", systemImage: "doc.on.doc") }
                        Button {
                            Speech.shared.speak(AIText.speakable(e.text), settings: AIState.shared.settings)
                        } label: { Label("朗读", systemImage: "speaker.wave.2") }
                        if let m = e.model { Text(m).foregroundStyle(.tertiary) }
                    }
                    .buttonStyle(.borderless)
                    .font(.caption)
                }
                .padding(12)
                .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 12))
                Spacer(minLength: 40)
            }
        }
    }

    private var liveBubble: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "sparkles").foregroundStyle(.purple).padding(.top, 2)
            VStack(alignment: .leading, spacing: 8) {
                if stream.hasOutput { MarkdownText(text: stream.text) }
                AIStreamStatus(stream: stream)
                if stream.running && stream.hasOutput { ProgressView().controlSize(.small) }
            }
            .padding(12)
            .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 12))
            Spacer(minLength: 40)
        }
    }
}

// MARK: - Loops

private struct AssistantLoopsView: View {
    @ObservedObject var model: AssistantModel

    var body: some View {
        HSplitView {
            List(selection: Binding(get: { model.selectedLoop }, set: { id in
                model.selectedLoop = id
                Task { await model.loadRuns() }
            })) {
                ForEach(model.loops) { l in
                    VStack(alignment: .leading, spacing: 2) {
                        HStack {
                            Text(l.name.isEmpty ? "未命名 Loop" : l.name).fontWeight(.medium)
                            if !l.enabled { Text("已停用").font(.caption2).foregroundStyle(.secondary) }
                        }
                        Text(l.schedule.summary).font(.caption).foregroundStyle(.secondary)
                        if let last = l.lastRun {
                            Text("上次运行 \(Fmt.relative(last))").font(.caption2).foregroundStyle(.tertiary)
                        }
                    }
                    .tag(l.id)
                }
            }
            .frame(minWidth: 200, maxWidth: 260)
            .overlay {
                if model.loops.isEmpty {
                    ContentUnavailableView {
                        Label("还没有 Loop", systemImage: "clock.arrow.circlepath")
                    } description: {
                        Text("在 设置 → AI 功能 中从模板新建，例如每日早报。")
                    } actions: {
                        Button("新建 Loop…") { AIState.openSettings(tab: "ai_features") }
                    }
                }
            }
            detail
                .frame(minWidth: 360, maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    @ViewBuilder
    private var detail: some View {
        if let id = model.selectedLoop, let loop = model.loops.first(where: { $0.id == id }) {
            VStack(alignment: .leading, spacing: 0) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(loop.name).font(.headline)
                        Text("\(loop.schedule.summary) · \(loop.query.isEmpty ? "收件箱最近邮件" : loop.query)")
                            .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    Spacer()
                    if model.runningLoop == loop.id { ProgressView().controlSize(.small) }
                    Button {
                        model.run(loop.id)
                    } label: {
                        Label("立即运行", systemImage: "play.circle")
                    }
                    .disabled(model.runningLoop == loop.id)
                }
                .padding(12)
                Divider()
                if model.runs.isEmpty {
                    ContentUnavailableView(model.loadingRuns ? "加载中…" : "还没有运行记录", systemImage: "clock",
                                           description: Text(loop.nextRun.map { "下次运行：\(Fmt.full($0))" } ?? ""))
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 12) {
                            ForEach(Array(model.runs.enumerated()), id: \.element.id) { idx, run in
                                AILoopRunView(run: run, expandedByDefault: idx == 0)
                            }
                        }
                        .padding(14)
                    }
                }
            }
        } else {
            ContentUnavailableView("选择一个 Loop", systemImage: "clock.arrow.circlepath")
        }
    }
}

/// One Loop run: header, Markdown output (collapsible for older runs).
struct AILoopRunView: View {
    let run: AILoopRun
    var expandedByDefault = true
    @State private var expanded: Bool? = nil

    private var isExpanded: Bool { expanded ?? expandedByDefault }

    private var statusColor: Color {
        if run.error != nil || run.status == "failed" || run.status == "error" { return .red }
        if run.status == "running" { return .orange }
        return .green
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                expanded = !isExpanded
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: isExpanded ? "chevron.down" : "chevron.right").font(.caption)
                    Text(Fmt.full(run.startedAt)).font(.subheadline.weight(.medium))
                    AIChip(text: run.statusTitle, color: statusColor)
                    Text("\(run.messageCount) 封邮件").font(.caption).foregroundStyle(.secondary)
                    Spacer()
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if isExpanded {
                if let err = run.error, !err.isEmpty {
                    Label(err, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.red).font(.callout)
                }
                if !run.output.isEmpty {
                    MarkdownText(text: run.output)
                    HStack(spacing: 12) {
                        Button { AIText.copy(run.output) } label: { Label("复制", systemImage: "doc.on.doc") }
                        Button {
                            Speech.shared.speak(AIText.speakable(run.output), settings: AIState.shared.settings)
                        } label: { Label("朗读", systemImage: "speaker.wave.2") }
                    }
                    .buttonStyle(.borderless)
                    .font(.caption)
                }
            }
        }
        .padding(12)
        .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 10))
    }
}
