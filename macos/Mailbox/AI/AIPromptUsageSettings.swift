import AppKit
import Charts
import SwiftUI

// MARK: - Prompt library (docs/AI.md §8.8)

/// Settings → AI 功能 → 提示词.
struct AIPromptsSettings: View {
    @ObservedObject private var ai = AIState.shared
    @State private var editing: AIPromptPreset? = nil
    @State private var error: String? = nil
    @State private var loaded = false

    private struct Scope: Identifiable {
        var id: String
        var title: String
        var hint: String
    }

    private static let scopes: [Scope] = [
        Scope(id: "compose", title: "写信", hint: "写信窗口 ✨ AI → 我的提示词：改写正文或选中的文字"),
        Scope(id: "reader", title: "阅读", hint: "阅读区 ✨ → 我的提示词：对这封邮件或整个会话执行，结果显示在 AI 卡片中"),
        Scope(id: "batch", title: "批量处理", hint: "AI 批量处理 → 自定义指令旁的「我的提示词」"),
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("常用的 AI 指令可以存成提示词，在写信、阅读和批量处理时一键使用。")
                .font(.callout).foregroundStyle(.secondary)
                .padding(.horizontal)
            List {
                ForEach(Self.scopes) { scope in
                    Section {
                        let items = ai.promptList(scope: scope.id)
                        if items.isEmpty {
                            Text("还没有\(scope.title)提示词").font(.callout).foregroundStyle(.secondary)
                        }
                        ForEach(items) { p in
                            row(p)
                        }
                    } header: {
                        VStack(alignment: .leading, spacing: 1) {
                            Text(scope.title)
                            Text(scope.hint).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            HStack {
                Menu("新建提示词") {
                    ForEach(Self.scopes) { scope in
                        Button(scope.title) {
                            var p = AIPromptPreset(scope: scope.id)
                            p.sort = (ai.promptList(scope: scope.id).map(\.sort).max() ?? 0) + 1
                            editing = p
                        }
                    }
                }
                .fixedSize()
                if let error { Text(error).font(.caption).foregroundStyle(.red) }
                Spacer()
            }
            .padding([.horizontal, .bottom])
        }
        .task {
            await ai.reloadPrompts()
            loaded = true
        }
        .sheet(item: $editing) { p in
            AIPromptEditor(prompt: p) { saved in
                editing = nil
                if let saved { Task { await save(saved) } }
            }
        }
    }

    private func row(_ p: AIPromptPreset) -> some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(p.name.isEmpty ? "未命名" : p.name).fontWeight(.medium)
                Text(p.prompt).font(.caption).foregroundStyle(.secondary).lineLimit(2)
            }
            Spacer()
            Button {
                move(p, by: -1)
            } label: {
                Image(systemName: "arrow.up")
            }
            .buttonStyle(.borderless)
            .help("上移")
            Button {
                move(p, by: 1)
            } label: {
                Image(systemName: "arrow.down")
            }
            .buttonStyle(.borderless)
            .help("下移")
            Button("编辑") { editing = p }
            Button(role: .destructive) {
                Task { await delete(p) }
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.borderless)
            .help("删除")
        }
        .padding(.vertical, 2)
    }

    /// Swaps positions with the neighbour and renumbers the scope.
    private func move(_ p: AIPromptPreset, by offset: Int) {
        var items = ai.promptList(scope: p.scope)
        guard let i = items.firstIndex(where: { $0.id == p.id }) else { return }
        let j = i + offset
        guard items.indices.contains(j) else { return }
        items.swapAt(i, j)
        let ordered = items
        Task {
            for (n, item) in ordered.enumerated() where item.sort != n + 1 {
                var updated = item
                updated.sort = n + 1
                _ = try? await APIClient.shared.saveAIPrompt(updated)
            }
            await ai.reloadPrompts()
        }
    }

    private func save(_ p: AIPromptPreset) async {
        do {
            try await APIClient.shared.saveAIPrompt(p)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        await ai.reloadPrompts()
    }

    private func delete(_ p: AIPromptPreset) async {
        do {
            try await APIClient.shared.deleteAIPrompt(p.id)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        await ai.reloadPrompts()
    }
}

private struct AIPromptEditor: View {
    @State var prompt: AIPromptPreset
    let onDone: (AIPromptPreset?) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(prompt.id.isEmpty ? "新建提示词" : "编辑提示词").font(.headline)
            Form {
                TextField("名称", text: $prompt.name, prompt: Text("例如：改成适合发给客户的语气"))
                Picker("用在", selection: $prompt.scope) {
                    Text("写信").tag("compose")
                    Text("阅读").tag("reader")
                    Text("批量处理").tag("batch")
                }
                .pickerStyle(.segmented)
                VStack(alignment: .leading, spacing: 4) {
                    Text("提示词")
                    TextEditor(text: $prompt.prompt)
                        .font(.body)
                        .frame(height: 140)
                        .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.quaternary))
                    Text(example).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            }
            .formStyle(.grouped)
            HStack {
                Spacer()
                Button("取消") { onDone(nil) }.keyboardShortcut(.cancelAction)
                Button("保存") { onDone(prompt) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(prompt.name.trimmingCharacters(in: .whitespaces).isEmpty
                        || prompt.prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 520, height: 440)
    }

    private var example: String {
        switch prompt.scope {
        case "reader": return "例如：提取所有金额和日期；用三句话向老板汇报这封邮件。"
        case "batch": return "例如：提取订单号和金额，输出表格。"
        default: return "例如：改成适合发给客户的语气；压缩到 100 字以内。"
        }
    }
}

// MARK: - Usage (docs/AI.md §8.4)

/// Settings → AI 功能 → 用量: daily bar chart plus per-model and per-feature tables.
struct AIUsageSettings: View {
    @ObservedObject private var ai = AIState.shared
    @AppStorage("aiUsageDays") private var days = 30
    @AppStorage("aiUsageMetric") private var metric = "tokens"
    @State private var report: AIUsageReport? = nil
    @State private var loading = false
    @State private var error: String? = nil

    private struct Point: Identifiable {
        var id: String
        var date: Date
        var series: String
        var value: Int
    }

    private static let dayParser: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                HStack {
                    Picker("", selection: $days) {
                        Text("7 天").tag(7)
                        Text("30 天").tag(30)
                        Text("90 天").tag(90)
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .frame(width: 220)
                    Spacer()
                    if loading { ProgressView().controlSize(.small) }
                    Button {
                        Task { await load() }
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .buttonStyle(.borderless)
                    .help("刷新")
                }
                if let error {
                    Label(error, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.red).font(.callout)
                }
                totals
                chartSection
                byModelTable
                byFeatureTable
                Text("用量按本地日期统计，每次 AI 调用记录一条。部分服务商（如 Codex）不返回 token 数，记为 0。")
                    .font(.caption).foregroundStyle(.secondary)
            }
            .padding(20)
        }
        .task(id: days) { await load() }
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            report = try await APIClient.shared.aiUsage(days: days)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    // MARK: Totals

    private var totals: some View {
        let d = report?.days ?? []
        let calls = d.reduce(0) { $0 + $1.calls }
        let input = d.reduce(0) { $0 + $1.inputTokens }
        let output = d.reduce(0) { $0 + $1.outputTokens }
        let errors = d.reduce(0) { $0 + $1.errors }
        return HStack(spacing: 12) {
            tile("调用次数", calls.formatted(), symbol: "bolt.horizontal")
            tile("输入 token", Self.compact(input), symbol: "arrow.down.doc")
            tile("输出 token", Self.compact(output), symbol: "arrow.up.doc")
            tile("失败", errors.formatted(), symbol: "exclamationmark.triangle", color: errors > 0 ? .red : .secondary)
        }
    }

    private func tile(_ title: String, _ value: String, symbol: String, color: Color = .purple) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(title, systemImage: symbol).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.title2.weight(.semibold).monospacedDigit()).foregroundStyle(color)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 10))
    }

    static func compact(_ n: Int) -> String {
        if n >= 100_000_000 { return String(format: "%.1f 亿", Double(n) / 100_000_000) }
        if n >= 10_000 { return String(format: "%.1f 万", Double(n) / 10_000) }
        return n.formatted()
    }

    // MARK: Chart

    private var points: [Point] {
        var out: [Point] = []
        for d in report?.days ?? [] {
            guard let date = Self.dayParser.date(from: d.date) else { continue }
            if metric == "calls" {
                out.append(Point(id: d.date + "-ok", date: date, series: "成功", value: max(0, d.calls - d.errors)))
                out.append(Point(id: d.date + "-err", date: date, series: "失败", value: d.errors))
            } else {
                out.append(Point(id: d.date + "-in", date: date, series: "输入", value: d.inputTokens))
                out.append(Point(id: d.date + "-out", date: date, series: "输出", value: d.outputTokens))
            }
        }
        return out
    }

    private var styleScale: KeyValuePairs<String, Color> {
        if metric == "calls" { return ["成功": Color.purple, "失败": Color.red] }
        return ["输入": Color.blue, "输出": Color.purple]
    }

    private var chartSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("每日用量").font(.headline)
                Spacer()
                Picker("", selection: $metric) {
                    Text("Token").tag("tokens")
                    Text("调用次数").tag("calls")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 180)
            }
            if points.isEmpty {
                Text(loading ? "加载中…" : "这段时间还没有 AI 调用").font(.callout).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 160)
            } else {
                Chart(points) { p in
                    BarMark(
                        x: .value("日期", p.date, unit: .day),
                        y: .value("数量", p.value)
                    )
                    .foregroundStyle(by: .value("类型", p.series))
                }
                .chartForegroundStyleScale(styleScale)
                .frame(height: 220)
            }
        }
        .padding(12)
        .background(.quaternary.opacity(0.2), in: RoundedRectangle(cornerRadius: 10))
    }

    // MARK: Tables

    private func headerCell(_ title: String) -> some View {
        Text(title).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
    }

    private func providerName(_ id: String) -> String {
        ai.providers.first { $0.id == id }?.name ?? id
    }

    private var byModelTable: some View {
        let rows = (report?.byModel ?? []).sorted { a, b in
            let ta = a.inputTokens + a.outputTokens
            let tb = b.inputTokens + b.outputTokens
            return ta != tb ? ta > tb : a.calls > b.calls
        }
        return VStack(alignment: .leading, spacing: 8) {
            Text("按模型").font(.headline)
            if rows.isEmpty {
                Text("暂无数据").font(.callout).foregroundStyle(.secondary)
            } else {
                Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 6) {
                    GridRow {
                        headerCell("服务商 · 模型")
                        headerCell("调用").gridColumnAlignment(.trailing)
                        headerCell("输入 token").gridColumnAlignment(.trailing)
                        headerCell("输出 token").gridColumnAlignment(.trailing)
                    }
                    Divider()
                    ForEach(rows) { r in
                        GridRow {
                            Text("\(providerName(r.provider)) · \(r.model)").lineLimit(1)
                            Text(r.calls.formatted()).monospacedDigit()
                            Text(r.inputTokens.formatted()).monospacedDigit()
                            Text(r.outputTokens.formatted()).monospacedDigit()
                        }
                    }
                }
                .font(.callout)
            }
        }
    }

    private var byFeatureTable: some View {
        let rows = (report?.byFeature ?? []).sorted { $0.calls > $1.calls }
        return VStack(alignment: .leading, spacing: 8) {
            Text("按功能").font(.headline)
            if rows.isEmpty {
                Text("暂无数据").font(.callout).foregroundStyle(.secondary)
            } else {
                Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 6) {
                    GridRow {
                        headerCell("功能")
                        headerCell("调用").gridColumnAlignment(.trailing)
                        headerCell("输入 token").gridColumnAlignment(.trailing)
                        headerCell("输出 token").gridColumnAlignment(.trailing)
                    }
                    Divider()
                    ForEach(rows) { r in
                        GridRow {
                            Text(AIUsageReport.featureTitle(r.feature)).lineLimit(1)
                            Text(r.calls.formatted()).monospacedDigit()
                            Text(r.inputTokens.formatted()).monospacedDigit()
                            Text(r.outputTokens.formatted()).monospacedDigit()
                        }
                    }
                }
                .font(.callout)
            }
        }
    }
}
