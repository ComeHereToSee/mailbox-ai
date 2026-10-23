import AppKit
import SwiftUI

/// "✨ AI 批量处理…" for a multi-selection: choose an operation, watch progress,
/// review results (tick classify suggestions and apply), read / copy the summary.
struct AIBatchSheet: View {
    @EnvironmentObject private var state: AppState
    @ObservedObject private var ai = AIState.shared
    @Environment(\.dismiss) private var dismiss
    let ids: [Int64]

    @State private var op = "summarize"
    @State private var instruction = ""
    @State private var jobId: String? = nil
    @State private var job: AIBatchJob? = nil
    @State private var error: String? = nil
    @State private var notConfigured = false
    @State private var starting = false
    @State private var picked: Set<Int64> = []
    @State private var applying = false
    @State private var pollTask: Task<Void, Never>? = nil

    struct Op: Identifiable {
        var id: String
        var title: String
        var symbol: String
        var hint: String
    }

    static let ops: [Op] = [
        Op(id: "summarize", title: "逐封总结", symbol: "text.bubble", hint: "每封一句话，最后给出整体汇总"),
        Op(id: "classify", title: "智能整理", symbol: "tray.full", hint: "建议标签、分类以及归档 / 删除，确认后再执行"),
        Op(id: "todos", title: "提取待办", symbol: "checklist", hint: "列出每封邮件里的待办事项并汇总"),
        Op(id: "draft_replies", title: "生成回复草稿", symbol: "arrowshape.turn.up.left", hint: "为每封邮件写好回复，保存到草稿箱"),
        Op(id: "custom", title: "自定义指令", symbol: "wand.and.stars", hint: "对每封邮件执行你的指令"),
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 8) {
                Image(systemName: "sparkles").foregroundStyle(.purple).font(.title2)
                Text("AI 批量处理").font(.title3.weight(.semibold))
                Text("\(ids.count) 封邮件").foregroundStyle(.secondary)
                Spacer()
            }
            if jobId == nil {
                chooser
            } else {
                progress
                results
            }
            if notConfigured {
                AINotConfiguredView(message: error ?? "还没有设置 AI 模型")
            } else if let error {
                Label(error, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.red).font(.callout)
            }
            footer
        }
        .padding(20)
        .frame(width: 720, height: jobId == nil ? 460 : 600)
        .onChange(of: ai.jobProgress[jobId ?? ""]) { _, _ in
            Task { await refresh() }
        }
        .onDisappear { pollTask?.cancel() }
    }

    // MARK: Step 1

    private var chooser: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("选择操作").font(.headline)
            ForEach(Self.ops) { o in
                Button {
                    op = o.id
                } label: {
                    HStack(spacing: 10) {
                        Image(systemName: op == o.id ? "largecircle.fill.circle" : "circle")
                            .foregroundStyle(op == o.id ? Color.accentColor : Color.secondary)
                        Image(systemName: o.symbol).frame(width: 20)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(o.title).fontWeight(.medium)
                            Text(o.hint).font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                    }
                    .padding(.vertical, 4)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            if op == "custom" || op == "draft_replies" {
                Text(op == "custom" ? "指令" : "回复要求（可选）").font(.headline).padding(.top, 4)
                TextField(op == "custom" ? "例如：提取每封邮件里的金额和付款截止日" : "例如：礼貌地确认收到，稍后详细回复",
                          text: $instruction, axis: .vertical)
                    .lineLimit(2...4)
                    .textFieldStyle(.roundedBorder)
            }
            Spacer(minLength: 0)
        }
    }

    // MARK: Step 2

    @ViewBuilder
    private var progress: some View {
        let total = max(job?.total ?? ids.count, 1)
        let done = min(job?.done ?? 0, total)
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(opTitle).font(.headline)
                Spacer()
                Text(statusText).font(.callout).foregroundStyle(.secondary)
            }
            ProgressView(value: Double(done), total: Double(total))
            Text("\(done) / \(total)").font(.caption.monospacedDigit()).foregroundStyle(.secondary)
        }
    }

    private var opTitle: String {
        Self.ops.first(where: { $0.id == op })?.title ?? op
    }

    private var statusText: String {
        switch job?.status ?? "running" {
        case "running": return "处理中…"
        case "done": return "已完成"
        case "failed": return "失败"
        case "cancelled": return "已取消"
        default: return job?.status ?? ""
        }
    }

    // MARK: Step 3 & 4

    @ViewBuilder
    private var results: some View {
        let items = job?.items ?? []
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                if let summary = job?.summary, !summary.isEmpty {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Label("汇总", systemImage: "sparkles").font(.subheadline.weight(.semibold))
                            Spacer()
                            Button("复制") {
                                AIText.copy(summary)
                                state.show(Toast(text: "汇总已复制"))
                            }
                            .buttonStyle(.link)
                        }
                        MarkdownText(text: summary)
                    }
                    .padding(10)
                    .background(Color.purple.opacity(0.07), in: RoundedRectangle(cornerRadius: 10))
                }
                if op == "classify" && items.contains(where: { !$0.suggested.isEmpty }) {
                    HStack {
                        Text("勾选要执行的建议").font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Button("全选") { picked = Set(items.filter { !$0.suggested.isEmpty }.map(\.messageId)) }
                            .buttonStyle(.link).font(.caption)
                        Button("全不选") { picked = [] }
                            .buttonStyle(.link).font(.caption)
                    }
                }
                ForEach(items) { item in
                    row(item)
                    Divider()
                }
                if items.isEmpty && (job?.isRunning ?? true) {
                    HStack {
                        Spacer()
                        ProgressView().controlSize(.small)
                        Spacer()
                    }
                    .padding()
                }
            }
            .padding(.vertical, 4)
        }
        .frame(maxHeight: .infinity)
        .background(.background, in: RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
    }

    private func row(_ item: AIBatchItem) -> some View {
        HStack(alignment: .top, spacing: 8) {
            if op == "classify" {
                Toggle("", isOn: Binding(get: { picked.contains(item.messageId) }, set: { on in
                    if on { picked.insert(item.messageId) } else { picked.remove(item.messageId) }
                }))
                .labelsHidden()
                .toggleStyle(.checkbox)
                .disabled(item.suggested.isEmpty)
            }
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(item.from).fontWeight(.medium).lineLimit(1)
                    Text(item.subject.isEmpty ? "（无主题）" : item.subject).foregroundStyle(.secondary).lineLimit(1)
                    Spacer(minLength: 4)
                    Button {
                        Task { await state.open(messageId: item.messageId) }
                    } label: {
                        Image(systemName: "arrow.up.forward.square")
                    }
                    .buttonStyle(.borderless)
                    .help("打开邮件")
                }
                if let err = item.error {
                    Text(err).font(.callout).foregroundStyle(.red)
                } else if !item.output.isEmpty {
                    MarkdownText(text: item.output)
                        .font(.callout)
                }
                if !item.suggested.isEmpty {
                    FlowLayout(spacing: 4) {
                        ForEach(item.suggested) { s in
                            AIChip(text: s.title, color: s.action == "trash" ? .red : .blue)
                                .help(s.reason)
                        }
                    }
                }
                if let draftId = item.draftId {
                    Button {
                        Task {
                            if let d = try? await state.api.draft(draftId) { state.compose(ComposeRequest(draft: d)) }
                        }
                    } label: {
                        Label("打开回复草稿", systemImage: "doc.text")
                    }
                    .buttonStyle(.link)
                    .font(.caption)
                }
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
    }

    // MARK: Footer

    private var footer: some View {
        HStack {
            if starting || applying { ProgressView().controlSize(.small) }
            Spacer()
            if jobId == nil {
                Button("取消") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("开始处理") { Task { await start() } }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(starting || (op == "custom" && instruction.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty))
            } else {
                if job?.isRunning ?? true {
                    Button("停止") { Task { await cancelJob() } }
                }
                if op == "classify" {
                    Button("应用所选（\(picked.count)）") { Task { await apply() } }
                        .disabled(picked.isEmpty || applying || (job?.isRunning ?? true))
                }
                Button("完成") { dismiss() }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
            }
        }
    }

    // MARK: Actions

    private func start() async {
        starting = true
        error = nil
        notConfigured = false
        defer { starting = false }
        do {
            let trimmed = instruction.trimmingCharacters(in: .whitespacesAndNewlines)
            let id = try await state.api.startAIBatch(ids: ids, op: op, instruction: trimmed.isEmpty ? nil : trimmed)
            jobId = id
            poll(id)
        } catch let e as APIError {
            error = e.message
            notConfigured = e.code == "ai_not_configured"
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func poll(_ id: String) {
        pollTask?.cancel()
        pollTask = Task {
            while !Task.isCancelled {
                await refresh()
                if let j = job, !j.isRunning { break }
                try? await Task.sleep(nanoseconds: 1_500_000_000)
            }
        }
    }

    private func refresh() async {
        guard let id = jobId, let j = try? await state.api.aiJob(id) else { return }
        let firstDone = job?.isRunning != false && !j.isRunning
        job = j
        if firstDone && j.op == "classify" {
            picked = Set(j.items.filter { !$0.suggested.isEmpty }.map(\.messageId))
        }
        if firstDone && j.op == "draft_replies" {
            state.scheduleReload()
        }
    }

    private func cancelJob() async {
        guard let id = jobId else { return }
        try? await state.api.cancelAIJob(id)
        await refresh()
    }

    private func apply() async {
        guard let id = jobId else { return }
        applying = true
        defer { applying = false }
        do {
            try await state.api.applyAIJob(id, messageIds: Array(picked))
            state.show(Toast(text: "已对 \(picked.count) 封邮件执行建议"))
            state.scheduleReload()
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
