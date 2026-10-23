import AppKit
import SwiftUI

/// Settings → AI 功能.
struct AIFeaturesSettings: View {
    @ObservedObject private var ai = AIState.shared
    @AppStorage("aiFeaturesPane") private var pane = "general"

    var body: some View {
        VStack(spacing: 0) {
            Picker("", selection: $pane) {
                Text("常规").tag("general")
                Text("自动回复").tag("autoreply")
                Text("Loop 定时任务").tag("loops")
                Text("MCP").tag("mcp")
                Text("提示词").tag("prompts")
                Text("用量").tag("usage")
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 560)
            .padding(.top, 12)
            .padding(.bottom, 4)
            switch pane {
            case "autoreply": AIAutoReplySettings()
            case "loops", "loop": AILoopSettings()
            case "mcp": AIMCPSettings()
            case "prompts": AIPromptsSettings()
            case "usage": AIUsageSettings()
            default: AIGeneralSettings()
            }
        }
        .task { await ai.reload() }
        .onAppear { if pane == "loop" { pane = "loops" } }
    }
}

// MARK: - General

private struct AIGeneralSettings: View {
    @EnvironmentObject private var state: AppState
    @ObservedObject private var ai = AIState.shared
    @ObservedObject private var speech = Speech.shared
    @AppStorage("settingsTab") private var settingsTab = "general"
    @State private var voices: [Speech.Voice] = []

    var body: some View {
        Form {
            Section {
                modelPicker("默认模型", hint: "写信、对话、问问邮箱、自动回复", selection: $ai.settings.defaultModel)
                modelPicker("快速模型", hint: "摘要、翻译、分类、智能提醒、Loop；留空时用默认模型", selection: $ai.settings.quickModel)
                if ai.modelOptions.isEmpty {
                    HStack {
                        Text("还没有可用的模型：先在 AI 服务 中启用一个服务商并勾选模型。").font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Button("前往 AI 服务") { settingsTab = "ai" }
                    }
                }
            } header: {
                Text("模型")
            }
            Section {
                capabilityPicker("看图模型", capability: "vision", empty: "默认（使用默认模型，需支持看图）",
                                 hint: "用于解读图片附件", selection: $ai.settings.visionModel)
                capabilityPicker("嵌入模型", capability: "embedding", empty: "不使用",
                                 hint: "问问邮箱先全文检索，再按语义相似度重排（支持 OpenAI 兼容接口和 Gemini）", selection: $ai.settings.embeddingModel)
            } header: {
                Text("专用模型")
            } footer: {
                Text("只列出带「看图」或「嵌入」能力的模型；可以在 AI 服务 的模型列表中调整能力标签。").font(.caption).foregroundStyle(.secondary)
            }
            Section("写作") {
                Picker("输出语言", selection: $ai.settings.language) {
                    Text("跟随邮件").tag("auto")
                    Text("简体中文").tag("zh-CN")
                    Text("English").tag("en")
                }
                Picker("默认语气", selection: $ai.settings.tone) {
                    Text("专业").tag("professional")
                    Text("友好").tag("friendly")
                    Text("简洁").tag("concise")
                }
                TextField("落款称呼", text: $ai.settings.signatureName, prompt: Text("例如：张三"))
            }
            Section {
                Toggle("新邮件智能分析（需回复、截止时间、待办）", isOn: $ai.settings.smartReminders)
                Picker("分析范围", selection: $ai.settings.smartScope) {
                    Text("主要分类").tag("primary")
                    Text("全部邮件").tag("all")
                    Text("仅 VIP").tag("vip")
                }
                .disabled(!ai.settings.smartReminders)
                VStack(alignment: .leading, spacing: 2) {
                    Picker("新邮件通知", selection: $ai.settings.notifyMode) {
                        Text("普通").tag("normal")
                        Text("附上 AI 一句话摘要").tag("annotate")
                        Text("只通知重要邮件").tag("important")
                    }
                    .disabled(!ai.settings.smartReminders)
                    Text(notifyHint).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                Picker("跟进提醒", selection: $ai.settings.followupDays) {
                    Text("关闭").tag(0)
                    ForEach([1, 2, 3, 5, 7, 14], id: \.self) { d in Text("发出 \(d) 天未收到回复时").tag(d) }
                    if ![0, 1, 2, 3, 5, 7, 14].contains(ai.settings.followupDays) {
                        Text("发出 \(ai.settings.followupDays) 天未收到回复时").tag(ai.settings.followupDays)
                    }
                }
                Toggle("打开长邮件时自动生成摘要", isOn: $ai.settings.autoSummary)
                Toggle("允许处理加密邮件", isOn: $ai.settings.allowEncrypted)
                Picker("每次发送给 AI 的正文上限", selection: $ai.settings.maxBodyChars) {
                    Text("4,000 字").tag(4000)
                    Text("8,000 字").tag(8000)
                    Text("12,000 字").tag(12000)
                    Text("24,000 字").tag(24000)
                    Text("48,000 字").tag(48000)
                    if ![4000, 8000, 12000, 24000, 48000].contains(ai.settings.maxBodyChars) {
                        Text("\(ai.settings.maxBodyChars) 字").tag(ai.settings.maxBodyChars)
                    }
                }
            } header: {
                Text("智能提醒与隐私")
            } footer: {
                Text("分析结果出现在侧边栏「待处理」中，到时间由菜单栏助手提醒。默认不把加密邮件交给 AI。AI 还会提示疑似钓鱼和诈骗的邮件。")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section {
                if mailAccounts.isEmpty {
                    Text("还没有邮箱账户").font(.callout).foregroundStyle(.secondary)
                }
                ForEach(mailAccounts) { acc in
                    Picker(selection: policyBinding(acc.id)) {
                        Text("启用").tag("on")
                        Text("关闭").tag("off")
                        Text("仅本地模型").tag("local_only")
                    } label: {
                        HStack(spacing: 6) {
                            Circle().fill(Color(hex: acc.color)).frame(width: 8, height: 8)
                            VStack(alignment: .leading, spacing: 0) {
                                Text(acc.displayLabel)
                                if acc.displayLabel != acc.email {
                                    Text(acc.email).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                }
            } header: {
                Text("账户 AI 策略")
            } footer: {
                Text("「关闭」的账户不会交给 AI（包括摘要、智能分析、自动回复、Loop、问问邮箱和 MCP）；「仅本地模型」只允许 Ollama、LM Studio 等本机模型处理。")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("朗读") {
                Picker("语音", selection: Binding(get: { ai.settings.ttsVoice ?? "" }, set: { ai.settings.ttsVoice = $0.isEmpty ? nil : $0 })) {
                    Text("自动（中文优先）").tag("")
                    ForEach(voices) { v in Text(v.label).tag(v.id) }
                }
                HStack {
                    Text("语速")
                    Slider(value: $ai.settings.ttsRate, in: 0...1)
                    Text(String(format: "%.2f", ai.settings.ttsRate)).font(.caption.monospacedDigit()).frame(width: 36)
                }
                HStack {
                    Spacer()
                    Button {
                        if speech.speaking {
                            speech.stop()
                        } else {
                            speech.speak("你好，这是 Mailbox AI 的朗读试听。Hello, this is a preview.", settings: ai.settings)
                        }
                    } label: {
                        Label(speech.speaking ? "停止" : "试听", systemImage: speech.speaking ? "stop.circle" : "speaker.wave.2")
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear { voices = Speech.voices }
        .onChange(of: ai.settings) { _, _ in ai.scheduleSaveSettings() }
    }

    private var mailAccounts: [Account] { state.accounts.filter { $0.kind != .web } }

    private func policyBinding(_ accountId: String) -> Binding<String> {
        Binding(get: { ai.settings.policy(for: accountId) }, set: { v in
            if v == "on" {
                ai.settings.accountPolicies.removeValue(forKey: accountId)
            } else {
                ai.settings.accountPolicies[accountId] = v
            }
        })
    }

    private var notifyHint: String {
        switch ai.settings.notifyMode {
        case "annotate": return "照常弹出通知，智能分析完成后在通知正文前加上「✨ 一句话摘要」。"
        case "important": return "先等智能分析：高优先级、需要回复、VIP 或疑似诈骗的邮件才弹出通知（不在分析范围内的邮件照常通知）。"
        default: return "新邮件到达时立即通知。"
        }
    }

    /// Picker over models tagged with `capability` (all models when none is tagged).
    private func capabilityPicker(_ title: String, capability: String, empty: String, hint: String,
                                  selection: Binding<AIModelRef?>) -> some View {
        let tagged = ai.capableModels(capability)
        let options = tagged.isEmpty ? ai.modelOptions : tagged
        return VStack(alignment: .leading, spacing: 2) {
            Picker(title, selection: selection) {
                Text(empty).tag(AIModelRef?.none)
                ForEach(options) { o in
                    Text(o.label).tag(AIModelRef?.some(o.ref))
                }
                if let cur = selection.wrappedValue, !options.contains(where: { $0.ref == cur }) {
                    Text(ai.label(for: cur) + "（不可用）").tag(AIModelRef?.some(cur))
                }
            }
            Text(tagged.isEmpty && !ai.modelOptions.isEmpty ? hint + "（还没有模型标记了这项能力）" : hint)
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private func modelPicker(_ title: String, hint: String, selection: Binding<AIModelRef?>) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Picker(title, selection: selection) {
                Text("未设置").tag(AIModelRef?.none)
                ForEach(ai.modelOptions) { o in
                    Text(o.label).tag(AIModelRef?.some(o.ref))
                }
                if let cur = selection.wrappedValue, !ai.modelOptions.contains(where: { $0.ref == cur }) {
                    Text(ai.label(for: cur) + "（不可用）").tag(AIModelRef?.some(cur))
                }
            }
            Text(hint).font(.caption).foregroundStyle(.secondary)
        }
    }
}

// MARK: - Auto-reply

private struct AIAutoReplySettings: View {
    @EnvironmentObject private var state: AppState
    @State private var rules: [AIAutoReplyRule] = []
    @State private var editing: AIAutoReplyRule? = nil
    @State private var error: String? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("新邮件进入收件箱时依次检查启用的规则，第一条匹配的规则生效。不会回复自己的邮件、群发 / 订阅邮件、自动回复和 noreply 地址。")
                .font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal)
            List {
                ForEach(rules) { r in
                    HStack {
                        Toggle("", isOn: Binding(get: { r.enabled }, set: { v in
                            var nr = r
                            nr.enabled = v
                            Task { await save(nr) }
                        }))
                        .labelsHidden()
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(r.name.isEmpty ? "未命名规则" : r.name)
                                if r.isVacation {
                                    AIChip(text: "休假回复", symbol: "sun.max", color: .teal)
                                } else {
                                    AIChip(text: r.mode == "send" ? "直接发送" : "生成草稿", color: r.mode == "send" ? .red : .blue)
                                }
                            }
                            Text(summary(r)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Spacer()
                        Text("今日 \(r.sentToday)/\(r.maxPerDay)").font(.caption).foregroundStyle(.secondary)
                        Button("编辑") { editing = r }
                        Button(role: .destructive) {
                            Task {
                                try? await APIClient.shared.deleteAIAutoReplyRule(r.id)
                                await load()
                            }
                        } label: { Image(systemName: "trash") }
                    }
                }
            }
            .overlay {
                if rules.isEmpty {
                    ContentUnavailableView("还没有自动回复规则", systemImage: "arrowshape.turn.up.left.2",
                                           description: Text("例如：发票类邮件自动回复「已收到，月底前处理」。"))
                }
            }
            HStack {
                Button("新建规则") {
                    var r = AIAutoReplyRule()
                    r.name = "新规则"
                    editing = r
                }
                if let error { Text(error).font(.caption).foregroundStyle(.red) }
                Spacer()
            }
            .padding([.horizontal, .bottom])
        }
        .task { await load() }
        .sheet(item: $editing) { r in
            AIAutoReplyEditor(rule: r) { saved in
                editing = nil
                if let saved { Task { await save(saved) } }
            }
            .environmentObject(state)
        }
    }

    private func load() async {
        do {
            rules = try await APIClient.shared.aiAutoReplyRules()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func save(_ r: AIAutoReplyRule) async {
        do {
            _ = try await APIClient.shared.saveAIAutoReplyRule(r)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        await load()
    }

    private func summary(_ r: AIAutoReplyRule) -> String {
        if r.isVacation {
            let start = r.start.map { Fmt.short($0) } ?? "现在"
            let end = r.end.map { Fmt.short($0) } ?? "不限"
            let text = r.replyText.replacingOccurrences(of: "\n", with: " ")
            return "\(start) – \(end) → \(text.isEmpty ? "（未填写回复内容）" : text)"
        }
        var parts: [String] = []
        if !r.fromContains.isEmpty { parts.append("发件人含「\(r.fromContains)」") }
        if !r.subjectContains.isEmpty { parts.append("主题含「\(r.subjectContains)」") }
        if !r.aiMatch.isEmpty { parts.append("AI 判断：\(r.aiMatch)") }
        let cond = parts.isEmpty ? "所有新邮件" : parts.joined(separator: " 且 ")
        return "\(cond) → \(r.instruction.isEmpty ? "按默认语气回复" : r.instruction)"
    }
}

private struct AIAutoReplyEditor: View {
    @EnvironmentObject private var state: AppState
    @State var rule: AIAutoReplyRule
    let onDone: (AIAutoReplyRule?) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text(rule.id.isEmpty ? "新建自动回复规则" : "编辑自动回复规则").font(.headline)
                Spacer()
                Picker("", selection: Binding(get: { rule.kind }, set: { setKind($0) })) {
                    Text("AI 回复").tag("ai")
                    Text("休假回复").tag("vacation")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 200)
            }
            Form {
                TextField("名称", text: $rule.name)
                Picker("账户", selection: Binding(get: { rule.accountId ?? "" }, set: { rule.accountId = $0.isEmpty ? nil : $0 })) {
                    Text("所有账户").tag("")
                    ForEach(state.accounts.filter { $0.kind != .web }) { Text($0.displayLabel).tag($0.id) }
                }
                TextField("发件人包含", text: $rule.fromContains, prompt: Text("留空表示不限"))
                TextField("主题包含", text: $rule.subjectContains, prompt: Text("留空表示不限"))
                if rule.isVacation {
                    vacationFields
                } else {
                    TextField("AI 判断（可选）", text: $rule.aiMatch, prompt: Text("例如：客户询问报价或交期"))
                    VStack(alignment: .leading, spacing: 4) {
                        Text("如何回复")
                        TextEditor(text: $rule.instruction)
                            .font(.body)
                            .frame(height: 80)
                            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.quaternary))
                        Text("例如：感谢来信，说明我在休假，下周一回复；紧急事项请联系李四。").font(.caption).foregroundStyle(.secondary)
                    }
                    Picker("方式", selection: Binding(get: { rule.mode }, set: { setMode($0) })) {
                        Text("生成草稿并通知我").tag("draft")
                        Text("直接发送").tag("send")
                    }
                    .pickerStyle(.radioGroup)
                }
                Stepper("每天最多 \(rule.maxPerDay) 封", value: $rule.maxPerDay, in: 1...200)
                if !rule.isVacation {
                    Stepper("同一发件人 \(rule.oncePerSenderHours) 小时内只回复一次", value: $rule.oncePerSenderHours, in: 1...720)
                }
                Toggle("启用", isOn: $rule.enabled)
            }
            .formStyle(.grouped)
            HStack {
                Spacer()
                Button("取消") { onDone(nil) }.keyboardShortcut(.cancelAction)
                Button("保存") { onDone(finalized) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(!canSave)
            }
        }
        .padding(20)
        .frame(width: 560, height: 620)
    }

    /// Vacation rules always carry both dates (the pickers show defaults until touched).
    private var finalized: AIAutoReplyRule {
        var r = rule
        if r.isVacation {
            let now = Int64(Date().timeIntervalSince1970)
            if r.start == nil { r.start = now }
            if r.end == nil { r.end = (r.start ?? now) + 7 * 86400 }
        }
        return r
    }

    private var canSave: Bool {
        guard !rule.name.trimmingCharacters(in: .whitespaces).isEmpty else { return false }
        if rule.isVacation {
            guard !rule.replyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return false }
            if let s = rule.start, let e = rule.end, e <= s { return false }
        }
        return true
    }

    /// Vacation replies: fixed text, always sent directly, once per sender for the whole period.
    @ViewBuilder
    private var vacationFields: some View {
        DatePicker("开始", selection: dateBinding(\.start, fallback: Date()))
        DatePicker("结束", selection: dateBinding(\.end, fallback: Date().addingTimeInterval(7 * 86400)))
        if let s = rule.start, let e = rule.end, e <= s {
            Text("结束时间要晚于开始时间").font(.caption).foregroundStyle(.red)
        }
        VStack(alignment: .leading, spacing: 4) {
            Text("回复内容")
            TextEditor(text: $rule.replyText)
                .font(.body)
                .frame(height: 110)
                .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.quaternary))
            Text("例如：您好，我 10 月 1 日至 10 月 7 日休假，期间无法及时回复邮件，回来后会尽快处理。紧急事项请联系李四（lisi@example.com）。")
                .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
        }
        Label("休假回复不调用 AI：主题为「自动回复：原主题」，在这段时间内直接发送（带 Auto-Submitted 信头），同一发件人只回复一次。",
              systemImage: "info.circle")
            .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
    }

    private func dateBinding(_ key: WritableKeyPath<AIAutoReplyRule, Int64?>, fallback: Date) -> Binding<Date> {
        Binding(get: {
            rule[keyPath: key].map { Date(timeIntervalSince1970: TimeInterval($0)) } ?? fallback
        }, set: { d in
            rule[keyPath: key] = Int64(d.timeIntervalSince1970)
        })
    }

    private func setKind(_ kind: String) {
        rule.kind = kind
        guard kind == "vacation" else { return }
        let cal = Calendar.current
        if rule.start == nil {
            rule.start = Int64(cal.startOfDay(for: Date()).timeIntervalSince1970)
        }
        if rule.end == nil {
            let start = Date(timeIntervalSince1970: TimeInterval(rule.start ?? Int64(Date().timeIntervalSince1970)))
            let end = cal.date(byAdding: .day, value: 7, to: start) ?? start.addingTimeInterval(7 * 86400)
            rule.end = Int64(end.timeIntervalSince1970)
        }
        if rule.name.isEmpty || rule.name == "新规则" { rule.name = "休假回复" }
    }

    /// "直接发送" asks for confirmation.
    private func setMode(_ mode: String) {
        guard mode == "send", rule.mode != "send" else {
            rule.mode = mode
            return
        }
        let alert = NSAlert()
        alert.messageText = "让 AI 直接发送回复？"
        alert.informativeText = "符合条件的邮件会由 AI 写好回复后直接发出（经过发件箱，可在撤销时间内撤回），不会再让你确认。建议先用「生成草稿」试运行一段时间。"
        alert.alertStyle = .warning
        alert.addButton(withTitle: "直接发送")
        alert.addButton(withTitle: "取消")
        if alert.runModal() == .alertFirstButtonReturn { rule.mode = "send" }
    }
}

// MARK: - Loops

private struct AILoopSettings: View {
    @ObservedObject private var ai = AIState.shared
    @State private var loops: [AILoop] = []
    @State private var templates: [AILoop] = []
    @State private var editing: AILoop? = nil
    @State private var showingRuns: AILoop? = nil
    @State private var running: Set<String> = []
    @State private var error: String? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Loop 会按计划自动搜索邮件并交给 AI 处理，例如每天早上总结未读邮件。结果可以弹出通知，或作为一封邮件放进收件箱。")
                .font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal)
            List {
                ForEach(loops) { l in
                    HStack(spacing: 10) {
                        Toggle("", isOn: Binding(get: { l.enabled }, set: { v in
                            var nl = l
                            nl.enabled = v
                            Task { await save(nl) }
                        }))
                        .labelsHidden()
                        VStack(alignment: .leading, spacing: 2) {
                            Text(l.name.isEmpty ? "未命名 Loop" : l.name)
                            Text(subtitle(l)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Spacer()
                        if running.contains(l.id) { ProgressView().controlSize(.small) }
                        Button {
                            run(l)
                        } label: {
                            Image(systemName: "play.circle")
                        }
                        .buttonStyle(.borderless)
                        .help("立即运行")
                        .disabled(running.contains(l.id))
                        Button("运行记录") { showingRuns = l }
                        Button("编辑") { editing = l }
                        Button(role: .destructive) {
                            Task {
                                try? await APIClient.shared.deleteAILoop(l.id)
                                await load()
                            }
                        } label: { Image(systemName: "trash") }
                    }
                }
            }
            .overlay {
                if loops.isEmpty {
                    ContentUnavailableView("还没有 Loop", systemImage: "clock.arrow.circlepath",
                                           description: Text("点「新建 Loop」从模板开始，例如每日早报。"))
                }
            }
            HStack {
                Menu("新建 Loop") {
                    Button("空白 Loop") {
                        var l = AILoop()
                        l.name = "新 Loop"
                        editing = l
                    }
                    if !templates.isEmpty {
                        Divider()
                        ForEach(templates.indices, id: \.self) { i in
                            Button(templates[i].name) {
                                var l = templates[i]
                                l.id = ""
                                editing = l
                            }
                        }
                    }
                }
                .fixedSize()
                Button("打开 AI 助手") { WindowRouter.shared.showAssistant() }
                if let error { Text(error).font(.caption).foregroundStyle(.red) }
                Spacer()
            }
            .padding([.horizontal, .bottom])
        }
        .task {
            await load()
            templates = (try? await APIClient.shared.aiLoopTemplates()) ?? []
        }
        .onChange(of: ai.loopEvents) { _, _ in
            if let id = ai.lastLoopId { running.remove(id) }
            Task { await load() }
        }
        .sheet(item: $editing) { l in
            AILoopEditor(loop: l) { saved in
                editing = nil
                if let saved { Task { await save(saved) } }
            }
        }
        .sheet(item: $showingRuns) { l in
            AILoopRunsSheet(loop: l)
        }
    }

    private func subtitle(_ l: AILoop) -> String {
        var s = l.schedule.summary
        if let last = l.lastRun { s += " · 上次 \(Fmt.relative(last))" }
        if let next = l.nextRun, l.enabled { s += " · 下次 \(Fmt.short(next))" }
        return s
    }

    private func load() async {
        do {
            loops = try await APIClient.shared.aiLoops()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func save(_ l: AILoop) async {
        do {
            _ = try await APIClient.shared.saveAILoop(l)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        await load()
    }

    private func run(_ l: AILoop) {
        running.insert(l.id)
        Task {
            do {
                _ = try await APIClient.shared.runAILoop(l.id)
                AppState.shared.show(Toast(text: "「\(l.name)」已开始运行"))
            } catch {
                running.remove(l.id)
                self.error = error.localizedDescription
            }
        }
    }
}

private struct AILoopEditor: View {
    @ObservedObject private var ai = AIState.shared
    @State var loop: AILoop
    let onDone: (AILoop?) -> Void

    private var timeBinding: Binding<Date> {
        Binding(get: {
            let parts = loop.schedule.time.split(separator: ":").compactMap { Int($0) }
            let h = parts.first ?? 8
            let m = parts.count > 1 ? parts[1] : 0
            return Calendar.current.date(bySettingHour: min(max(h, 0), 23), minute: min(max(m, 0), 59), second: 0, of: Date()) ?? Date()
        }, set: { d in
            let c = Calendar.current.dateComponents([.hour, .minute], from: d)
            loop.schedule.time = String(format: "%02d:%02d", c.hour ?? 8, c.minute ?? 0)
        })
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(loop.id.isEmpty ? "新建 Loop" : "编辑 Loop").font(.headline)
            Form {
                TextField("名称", text: $loop.name)
                Picker("频率", selection: Binding(get: { loop.schedule.kind }, set: { kind in
                    loop.schedule.kind = kind
                    if kind == "weekly" && loop.schedule.weekday == nil { loop.schedule.weekday = 1 }
                    if kind == "interval" && loop.schedule.everyMinutes == nil { loop.schedule.everyMinutes = 60 }
                })) {
                    Text("每天").tag("daily")
                    Text("工作日").tag("weekdays")
                    Text("每周").tag("weekly")
                    Text("按间隔").tag("interval")
                }
                if loop.schedule.kind == "weekly" {
                    Picker("星期", selection: Binding(get: { loop.schedule.weekday ?? 1 }, set: { loop.schedule.weekday = $0 })) {
                        ForEach(1...7, id: \.self) { d in Text(AILoopSchedule.weekdayNames[d - 1]).tag(d) }
                    }
                }
                if loop.schedule.kind == "interval" {
                    Stepper(value: Binding(get: { max(30, loop.schedule.everyMinutes ?? 60) }, set: { loop.schedule.everyMinutes = max(30, $0) }),
                            in: 30...1440, step: 30) {
                        Text("每 \(max(30, loop.schedule.everyMinutes ?? 60)) 分钟")
                    }
                } else {
                    DatePicker("时间", selection: timeBinding, displayedComponents: .hourAndMinute)
                }
                VStack(alignment: .leading, spacing: 4) {
                    TextField("搜索条件", text: $loop.query, prompt: Text("例如 is:unread newer_than:1d；留空为收件箱最近邮件"))
                    Text("支持搜索语法：from: subject: is:unread newer_than:3d category:promotions -in:sent").font(.caption).foregroundStyle(.secondary)
                }
                Stepper("最多 \(loop.maxMessages) 封邮件", value: $loop.maxMessages, in: 1...200)
                VStack(alignment: .leading, spacing: 4) {
                    Text("指令")
                    TextEditor(text: $loop.instruction)
                        .font(.body)
                        .frame(height: 90)
                        .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.quaternary))
                }
                Toggle("完成后发送系统通知", isOn: $loop.notify)
                Toggle("同时作为一封邮件投递到收件箱", isOn: $loop.emailDigest)
                Picker("模型", selection: $loop.model) {
                    Text("默认（快速模型）").tag(AIModelRef?.none)
                    ForEach(ai.modelOptions) { o in Text(o.label).tag(AIModelRef?.some(o.ref)) }
                    if let cur = loop.model, !ai.modelOptions.contains(where: { $0.ref == cur }) {
                        Text(ai.label(for: cur) + "（不可用）").tag(AIModelRef?.some(cur))
                    }
                }
                Toggle("启用", isOn: $loop.enabled)
            }
            .formStyle(.grouped)
            HStack {
                Spacer()
                Button("取消") { onDone(nil) }.keyboardShortcut(.cancelAction)
                Button("保存") { onDone(loop) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(loop.name.trimmingCharacters(in: .whitespaces).isEmpty || loop.instruction.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 580, height: 660)
    }
}

private struct AILoopRunsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var ai = AIState.shared
    let loop: AILoop
    @State private var runs: [AILoopRun] = []
    @State private var loading = true

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("「\(loop.name)」最近运行").font(.headline)
                Spacer()
                if loading { ProgressView().controlSize(.small) }
                Button {
                    Task { await load() }
                } label: { Image(systemName: "arrow.clockwise") }
                .buttonStyle(.borderless)
            }
            if runs.isEmpty && !loading {
                ContentUnavailableView("还没有运行记录", systemImage: "clock")
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 10) {
                        ForEach(Array(runs.enumerated()), id: \.element.id) { idx, r in
                            AILoopRunView(run: r, expandedByDefault: idx == 0)
                        }
                    }
                }
            }
            HStack {
                Spacer()
                Button("关闭") { dismiss() }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(20)
        .frame(width: 640, height: 560)
        .task { await load() }
        .onChange(of: ai.loopEvents) { _, _ in Task { await load() } }
    }

    private func load() async {
        loading = true
        defer { loading = false }
        runs = ((try? await APIClient.shared.aiLoopRuns(loop.id)) ?? []).sorted { $0.startedAt > $1.startedAt }
    }
}
