import AppKit
import SwiftUI

/// Writing-assistant operations of `POST /v1/ai/compose`.
enum AIComposeOp: String, CaseIterable, Identifiable {
    case polish, formal, friendly, concise, expand, proofread, translate, write, reply, custom
    var id: String { rawValue }

    var title: String {
        switch self {
        case .polish: return "润色"
        case .formal: return "更正式"
        case .friendly: return "更友好"
        case .concise: return "更简洁"
        case .expand: return "扩写"
        case .proofread: return "纠错"
        case .translate: return "翻译"
        case .write: return "帮我写"
        case .reply: return "一键回复"
        case .custom: return "自定义指令"
        }
    }

    var symbol: String {
        switch self {
        case .polish: return "sparkles"
        case .formal: return "briefcase"
        case .friendly: return "face.smiling"
        case .concise: return "scissors"
        case .expand: return "text.append"
        case .proofread: return "checkmark.circle"
        case .translate: return "globe"
        case .write: return "pencil.and.outline"
        case .reply: return "arrowshape.turn.up.left"
        case .custom: return "wand.and.stars"
        }
    }

    var needsInstruction: Bool { self == .write || self == .custom || self == .reply }

    static let rewrites: [AIComposeOp] = [.polish, .formal, .friendly, .concise, .expand, .proofread]
}

/// State of the composer's AI side panel.
@MainActor
final class ComposeAIModel: ObservableObject {
    let stream = AIStreamModel()

    @Published var visible = false
    @Published var op: AIComposeOp = .polish
    @Published var targetLang: String?
    @Published var instruction = ""
    @Published var quickReply = ""
    /// The rewrite acts on the selected text (otherwise on the whole body).
    @Published var usesSelection = false
    @Published var sourceText = ""
    /// Write / custom: waiting for the user to type an instruction.
    @Published var awaitingInstruction = false

    var title: String {
        if op == .translate { return targetLang == "en" ? "翻译成英文" : "翻译成中文" }
        return op.title
    }

    /// Opens the panel for `op`, capturing the selection (or the body) first.
    func begin(_ op: AIComposeOp, targetLang: String? = nil, instruction: String? = nil, compose: ComposeModel) async {
        stream.reset()
        let previous = self.op
        self.op = op
        self.targetLang = targetLang
        if let instruction {
            self.instruction = instruction
        } else if previous != op || !op.needsInstruction {
            self.instruction = ""
        }
        await capture(compose)
        visible = true
        if op.needsInstruction && self.instruction.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            awaitingInstruction = true
            return
        }
        awaitingInstruction = false
        if !op.needsInstruction && sourceText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            stream.reset()
            emptyBody = true
            return
        }
        emptyBody = false
        run(compose: compose)
    }

    /// Rewrite requested on an empty body.
    @Published var emptyBody = false

    private func capture(_ compose: ComposeModel) async {
        if compose.markdownMode {
            usesSelection = false
            sourceText = compose.markdownText
            return
        }
        let selected = await compose.editor.selectedText()
        if !selected.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && op != .write && op != .reply {
            usesSelection = true
            sourceText = selected
        } else {
            usesSelection = false
            sourceText = await compose.editor.bodyText()
        }
    }

    func run(compose: ComposeModel) {
        awaitingInstruction = false
        let trimmed = instruction.trimmingCharacters(in: .whitespacesAndNewlines)
        let body = AIComposeBody(
            op: op.rawValue,
            text: sourceText,
            instruction: trimmed.isEmpty ? nil : trimmed,
            targetLang: op == .translate ? (targetLang ?? "en") : nil,
            messageId: (op == .reply || compose.draft.mode == .reply || compose.draft.mode == .replyAll) ? compose.draft.sourceMessageId : nil
        )
        stream.start("/v1/ai/compose", body: body)
    }

    /// "✨ 一键回复": a short instruction becomes a full reply.
    func sendQuickReply(compose: ComposeModel) async {
        let q = quickReply.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else { return }
        await begin(.reply, instruction: q, compose: compose)
    }

    func replace(compose: ComposeModel) {
        let text = stream.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        if compose.markdownMode {
            compose.markdownText = text
        } else if usesSelection {
            compose.editor.replaceSelection(html: AIText.html(text))
        } else {
            compose.editor.replaceBody(html: AIText.html(text))
        }
        applySubject(compose)
        compose.markDirty()
        close()
    }

    func insertBelow(compose: ComposeModel) {
        let text = stream.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        if compose.markdownMode {
            compose.markdownText += (compose.markdownText.isEmpty ? "" : "\n\n") + text
        } else {
            compose.editor.insertBelow(html: AIText.html(text))
        }
        applySubject(compose)
        compose.markDirty()
        close()
    }

    private func applySubject(_ compose: ComposeModel) {
        if let s = stream.result?.subject?.trimmingCharacters(in: .whitespacesAndNewlines), !s.isEmpty,
           compose.draft.subject.trimmingCharacters(in: .whitespaces).isEmpty {
            compose.draft.subject = s
        }
    }

    func close() {
        stream.reset()
        visible = false
        awaitingInstruction = false
        emptyBody = false
        if op == .reply { quickReply = "" }
    }
}

// MARK: - Menu

/// "✨ AI" menu in the composer footer.
struct AIComposeMenu: View {
    @ObservedObject var ai: ComposeAIModel
    @ObservedObject var compose: ComposeModel

    var body: some View {
        Menu {
            Section("改写（有选中文字时只改选区）") {
                ForEach(AIComposeOp.rewrites) { op in
                    Button { start(op) } label: { Label(op.title, systemImage: op.symbol) }
                }
                Button { start(.translate, lang: "en") } label: { Label("翻译成英文", systemImage: "globe") }
                Button { start(.translate, lang: "zh-CN") } label: { Label("翻译成中文", systemImage: "globe") }
            }
            Section {
                Button { start(.write) } label: { Label("帮我写…", systemImage: AIComposeOp.write.symbol) }
                if compose.draft.sourceMessageId != nil && (compose.draft.mode == .reply || compose.draft.mode == .replyAll) {
                    Button { start(.reply) } label: { Label("一键回复…", systemImage: AIComposeOp.reply.symbol) }
                }
                Button { start(.custom) } label: { Label("自定义指令…", systemImage: AIComposeOp.custom.symbol) }
            }
        } label: {
            Label("AI", systemImage: "sparkles")
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help("AI 写信助手")
        .disabled(!compose.loaded)
    }

    private func start(_ op: AIComposeOp, lang: String? = nil) {
        Task { await ai.begin(op, targetLang: lang, compose: compose) }
    }
}

// MARK: - Quick reply line

/// "✨ 一键回复" input shown for replies.
struct AIQuickReplyBar: View {
    @ObservedObject var ai: ComposeAIModel
    @ObservedObject var compose: ComposeModel

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "sparkles").foregroundStyle(.purple)
            Text("一键回复").foregroundStyle(.secondary)
            TextField("例如：同意，下周二可以", text: $ai.quickReply)
                .textFieldStyle(.plain)
                .onSubmit { send() }
            Button("生成") { send() }
                .buttonStyle(.borderless)
                .disabled(ai.quickReply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(Color.purple.opacity(0.05))
    }

    private func send() {
        Task { await ai.sendQuickReply(compose: compose) }
    }
}

// MARK: - Side panel

/// Streaming result panel: 替换 / 插入到下方 / 重新生成 / 放弃.
struct AIComposePanel: View {
    @ObservedObject var ai: ComposeAIModel
    @ObservedObject var stream: AIStreamModel
    @ObservedObject var compose: ComposeModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: ai.op.symbol).foregroundStyle(.purple)
                Text(ai.title).font(.headline)
                Spacer()
                Button { ai.close() } label: { Image(systemName: "xmark") }
                    .buttonStyle(.borderless)
                    .help("放弃")
            }
            Text(scopeText).font(.caption).foregroundStyle(.secondary)
            if ai.op.needsInstruction {
                VStack(alignment: .leading, spacing: 6) {
                    AIInstructionField(placeholder: placeholder, text: $ai.instruction, focusRequest: ai.awaitingInstruction) {
                        generate()
                    }
                    HStack {
                        Spacer()
                        // No ⌘↩ here: that shortcut sends the mail.
                        Button("生成") { generate() }
                            .disabled(ai.instruction.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || stream.running)
                    }
                }
            }
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    if ai.emptyBody {
                        Text("正文是空的。先写几句，或使用「帮我写…」。").font(.callout).foregroundStyle(.secondary)
                    }
                    if stream.hasOutput {
                        Text(stream.text)
                            .font(.body)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if let s = stream.result?.subject, !s.isEmpty {
                        Label("主题：\(s)", systemImage: "textformat").font(.caption).foregroundStyle(.secondary)
                    }
                    AIStreamStatus(stream: stream)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 2)
            }
            Divider()
            HStack(spacing: 8) {
                if stream.running {
                    ProgressView().controlSize(.small)
                    Button("停止") { stream.cancel() }
                }
                Spacer()
            }
            HStack(spacing: 8) {
                Button("替换") { ai.replace(compose: compose) }
                    .buttonStyle(.borderedProminent)
                    .disabled(!canApply)
                Button("插入到下方") { ai.insertBelow(compose: compose) }
                    .disabled(!canApply)
                Spacer(minLength: 0)
            }
            HStack(spacing: 8) {
                Button {
                    regenerate()
                } label: {
                    Label("重新生成", systemImage: "arrow.clockwise")
                }
                .disabled(stream.running || ai.awaitingInstruction || ai.emptyBody)
                Spacer(minLength: 0)
                Button("放弃", role: .cancel) { ai.close() }
            }
        }
        .padding(12)
        .frame(maxHeight: .infinity, alignment: .top)
        .background(.background)
    }

    private var canApply: Bool { !stream.running && stream.hasOutput && stream.error == nil }

    private var scopeText: String {
        switch ai.op {
        case .write: return "根据你的要求生成正文"
        case .reply: return "根据原邮件和你的要点生成回复"
        default:
            if compose.markdownMode { return "作用于：全文（Markdown）" }
            return ai.usesSelection ? "作用于：选中的文字" : "作用于：全文（不含签名和引用）"
        }
    }

    private var placeholder: String {
        switch ai.op {
        case .write: return "想写什么？例如：邀请团队周五下午参加季度复盘会"
        case .reply: return "回复要点，例如：同意，下周二可以"
        default: return "对正文做什么？例如：改成适合发给客户的语气"
        }
    }

    private func generate() {
        guard !ai.instruction.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, !stream.running else { return }
        ai.run(compose: compose)
    }

    private func regenerate() {
        ai.run(compose: compose)
    }
}

/// Multi-line instruction input that takes focus when the panel asks for an instruction.
private struct AIInstructionField: View {
    let placeholder: String
    @Binding var text: String
    let focusRequest: Bool
    let onSubmit: () -> Void
    @FocusState private var focused: Bool

    var body: some View {
        TextField(placeholder, text: $text, axis: .vertical)
            .lineLimit(2...5)
            .textFieldStyle(.roundedBorder)
            .focused($focused)
            .onSubmit { onSubmit() }
            .onAppear { if focusRequest { focused = true } }
            .onChange(of: focusRequest) { _, waiting in if waiting { focused = true } }
    }
}
