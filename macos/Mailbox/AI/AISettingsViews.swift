import AppKit
import SwiftUI

/// Data for Settings → AI 服务 (providers, presets, CLI status).
@MainActor
final class AIProvidersModel: ObservableObject {
    @Published var providers: [AIProvider] = []
    @Published var presets: [String: AIProviderPreset] = [:]
    @Published var cli = AICLIReport()
    @Published var cliLoading = false
    @Published var loaded = false
    @Published var error: String?

    private var saveTasks: [String: Task<Void, Never>] = [:]
    let api = APIClient.shared

    func load() async {
        async let p = try? api.aiProviders()
        async let pr = try? api.aiPresets()
        if let p = await p { providers = p }
        if let pr = await pr {
            var map: [String: AIProviderPreset] = [:]
            for preset in pr where map[preset.id] == nil { map[preset.id] = preset }
            presets = map
        }
        loaded = true
        AIState.shared.providers = providers
        await loadCLI()
    }

    func loadCLI() async {
        cliLoading = true
        defer { cliLoading = false }
        if let c = try? await api.aiCLIStatus() { cli = c }
    }

    func provider(_ id: String) -> AIProvider? { providers.first { $0.id == id } }

    func cliStatus(for p: AIProvider) -> AICLIStatus? {
        switch p.kind {
        case .claudeCode: return cli.claude
        case .codex: return cli.codex
        default: return nil
        }
    }

    /// Local edit; saved shortly after the last change.
    func update(_ p: AIProvider) {
        guard let i = providers.firstIndex(where: { $0.id == p.id }) else { return }
        providers[i] = p
        AIState.shared.providers = providers
        saveTasks[p.id]?.cancel()
        let id = p.id
        saveTasks[id] = Task {
            try? await Task.sleep(nanoseconds: 600_000_000)
            if Task.isCancelled { return }
            _ = await perform(id, apiKey: nil)
        }
    }

    /// Saves now (optionally with a key), dropping a pending debounced save. Returns false and sets `error` on failure.
    @discardableResult
    func save(_ id: String, apiKey: String?) async -> Bool {
        saveTasks[id]?.cancel()
        saveTasks[id] = nil
        return await perform(id, apiKey: apiKey)
    }

    private func perform(_ id: String, apiKey: String?) async -> Bool {
        guard let p = provider(id) else { return false }
        do {
            let saved = try await api.saveAIProvider(p, apiKey: apiKey)
            if let i = providers.firstIndex(where: { $0.id == id }) {
                if apiKey != nil {
                    providers[i] = saved
                } else {
                    // Keep in-flight edits; only take server-owned fields.
                    providers[i].hasKey = saved.hasKey
                    providers[i].custom = saved.custom
                }
            }
            error = nil
            AIState.shared.providers = providers
            return true
        } catch {
            self.error = error.localizedDescription
            return false
        }
    }

    func addCustom(kind: AIProviderKind) async -> String? {
        var p = AIProvider()
        p.id = UUID().uuidString.lowercased()
        p.kind = kind
        p.name = "自定义 \(kind.title)"
        p.enabled = true
        p.custom = true
        if kind == .azureOpenai { p.apiVersion = "2024-10-21" }
        do {
            let saved = try await api.saveAIProvider(p, apiKey: nil)
            providers.append(saved)
            AIState.shared.providers = providers
            return saved.id
        } catch {
            self.error = error.localizedDescription
            return nil
        }
    }

    /// Custom providers are deleted; presets are reset to defaults (and their key removed).
    func remove(_ id: String) async {
        do {
            try await api.deleteAIProvider(id)
            if let p = try? await api.aiProviders() { providers = p }
            AIState.shared.providers = providers
            await AIState.shared.reload()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

// MARK: - Tab

/// Settings → AI 服务: Cherry-Studio-like provider list and detail.
struct AIProvidersSettings: View {
    @StateObject private var model = AIProvidersModel()
    @State private var selected: String? = nil

    var body: some View {
        HSplitView {
            VStack(spacing: 0) {
                List(selection: $selected) {
                    ForEach(model.providers) { p in
                        AIProviderRow(provider: p, color: color(for: p), enabled: Binding(get: {
                            model.provider(p.id)?.enabled ?? false
                        }, set: { on in
                            guard var v = model.provider(p.id) else { return }
                            v.enabled = on
                            model.update(v)
                        }))
                        .tag(p.id)
                    }
                }
                .overlay {
                    if !model.loaded {
                        ProgressView()
                    } else if model.providers.isEmpty {
                        Text(model.error ?? "无法读取服务商列表").font(.caption).foregroundStyle(.secondary).padding()
                    }
                }
                Divider()
                HStack {
                    Menu {
                        ForEach(AIProviderKind.allCases.filter { !$0.isCLI }) { k in
                            Button(k.title) {
                                Task {
                                    if let id = await model.addCustom(kind: k) { selected = id }
                                }
                            }
                        }
                    } label: {
                        Label("添加自定义服务商", systemImage: "plus")
                    }
                    .menuStyle(.borderlessButton)
                    .fixedSize()
                    Spacer()
                }
                .padding(8)
            }
            .frame(minWidth: 220, idealWidth: 240, maxWidth: 280)
            Group {
                if let id = selected, model.provider(id) != nil {
                    AIProviderDetail(model: model, id: id).id(id)
                } else {
                    ContentUnavailableView("选择一个服务商", systemImage: "sparkles",
                                           description: Text("Claude Code 和 Codex 直接使用你在本机登录的订阅，不需要 API Key。"))
                }
            }
            .frame(minWidth: 560, maxWidth: .infinity, maxHeight: .infinity)
        }
        .task {
            await model.load()
            if selected == nil { selected = model.providers.first(where: \.enabled)?.id ?? model.providers.first?.id }
        }
    }

    private func color(for p: AIProvider) -> String {
        if let c = model.presets[p.id]?.color, !c.isEmpty { return c }
        switch p.kind {
        case .claudeCode, .anthropic: return "#D97757"
        case .codex, .openai: return "#10A37F"
        case .gemini: return "#4285F4"
        case .azureOpenai: return "#0078D4"
        }
    }
}

/// Round colored badge with the provider's initial (or a terminal glyph for CLIs).
struct AIProviderIcon: View {
    let name: String
    let color: String
    var cli = false
    var size: CGFloat = 22

    var body: some View {
        Circle()
            .fill(Color(hex: color, fallback: .gray).gradient)
            .frame(width: size, height: size)
            .overlay {
                if cli {
                    Image(systemName: "terminal.fill").font(.system(size: size * 0.45, weight: .semibold)).foregroundStyle(.white)
                } else {
                    Text(String(name.trimmingCharacters(in: .whitespaces).prefix(1)).uppercased())
                        .font(.system(size: size * 0.48, weight: .bold))
                        .foregroundStyle(.white)
                }
            }
    }
}

struct AIProviderRow: View {
    let provider: AIProvider
    let color: String
    @Binding var enabled: Bool

    var body: some View {
        HStack(spacing: 8) {
            AIProviderIcon(name: provider.name, color: color, cli: provider.isCLI)
            VStack(alignment: .leading, spacing: 1) {
                Text(provider.name).lineLimit(1)
                if provider.isCLI {
                    Text("订阅 · 命令行").font(.caption2).foregroundStyle(.secondary)
                } else if provider.hasKey {
                    Text(provider.keyCount > 1 ? "已设置 \(provider.keyCount) 个密钥" : "已设置密钥").font(.caption2).foregroundStyle(.secondary)
                } else if provider.custom {
                    Text("自定义").font(.caption2).foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 4)
            Toggle("", isOn: $enabled)
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.mini)
        }
        .padding(.vertical, 2)
    }
}

// MARK: - Detail

struct AIProviderDetail: View {
    @ObservedObject var model: AIProvidersModel
    let id: String

    @State private var apiKey = ""
    @State private var showKey = false
    @State private var keyMessage: String? = nil
    @State private var testModel = ""
    @State private var testing = false
    @State private var testResult: AIProviderTest? = nil
    @State private var testError: String? = nil
    @State private var fetching = false
    @State private var fetched: [AIModel] = []
    @State private var showFetched = false
    @State private var fetchError: String? = nil
    @State private var newModel = ""

    private var p: AIProvider { model.provider(id) ?? AIProvider() }
    private var preset: AIProviderPreset? { model.presets[id] }

    private func edit(_ change: (inout AIProvider) -> Void) {
        var v = p
        change(&v)
        model.update(v)
    }

    private func text(_ keyPath: WritableKeyPath<AIProvider, String>) -> Binding<String> {
        Binding(get: { p[keyPath: keyPath] }, set: { v in edit { $0[keyPath: keyPath] = v } })
    }

    var body: some View {
        Form {
            Section { header }
            if p.isCLI {
                cliSection
            } else {
                keySection
                addressSection
            }
            testSection
            modelsSection
            if let e = model.error {
                Section { Text(e).foregroundStyle(.red).font(.callout) }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            if testModel.isEmpty { testModel = p.models.first(where: \.enabled)?.id ?? p.models.first?.id ?? "" }
        }
        .sheet(isPresented: $showFetched) {
            AIFetchedModelsSheet(models: fetched, existing: Set(p.models.map(\.id))) { chosen in
                showFetched = false
                guard !chosen.isEmpty else { return }
                edit { v in
                    for m in chosen where !v.models.contains(where: { $0.id == m.id }) {
                        var add = m
                        add.enabled = true
                        v.models.append(add)
                    }
                }
                if testModel.isEmpty { testModel = chosen.first?.id ?? "" }
            }
        }
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            AIProviderIcon(name: p.name, color: preset?.color ?? "", cli: p.isCLI, size: 40)
            VStack(alignment: .leading, spacing: 4) {
                if p.custom {
                    TextField("名称", text: text(\.name)).textFieldStyle(.roundedBorder).frame(maxWidth: 280)
                    Picker("接口类型", selection: Binding(get: { p.kind }, set: { k in edit { $0.kind = k } })) {
                        ForEach(AIProviderKind.allCases.filter { !$0.isCLI }) { Text($0.title).tag($0) }
                    }
                    .frame(maxWidth: 280)
                } else {
                    Text(p.name).font(.title3.weight(.semibold))
                    Text(p.kind.title).font(.caption).foregroundStyle(.secondary)
                }
                if let d = preset?.description, !d.isEmpty {
                    Text(d).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                HStack(spacing: 12) {
                    if let w = preset?.website, let url = URL(string: w), !w.isEmpty {
                        Link("官网", destination: url).font(.caption)
                    }
                    Button(p.custom ? "删除服务商…" : "恢复默认…") { confirmRemove() }
                        .buttonStyle(.link)
                        .font(.caption)
                }
            }
            Spacer()
            Toggle("启用", isOn: Binding(get: { p.enabled }, set: { on in edit { $0.enabled = on } }))
                .toggleStyle(.switch)
        }
    }

    private func confirmRemove() {
        let alert = NSAlert()
        alert.messageText = p.custom ? "删除「\(p.name)」？" : "把「\(p.name)」恢复为默认设置？"
        alert.informativeText = p.custom ? "服务商和保存的 API Key 都会被删除。" : "API 地址和模型列表恢复为预设值，保存的 API Key 会被删除。"
        alert.addButton(withTitle: p.custom ? "删除" : "恢复默认")
        alert.addButton(withTitle: "取消")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        Task { await model.remove(id) }
    }

    // MARK: Key / address

    /// Keys in the field (comma / newline separated; the engine uses them in turn, §8.4).
    private var typedKeyCount: Int {
        apiKey.split(whereSeparator: { $0 == "," || $0 == "，" || $0.isNewline })
            .filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
            .count
    }

    private var keySection: some View {
        Section {
            HStack(alignment: .top, spacing: 6) {
                Group {
                    if showKey {
                        TextField("API Key", text: $apiKey, prompt: Text(p.hasKey ? "已保存（输入新密钥以替换）" : "sk-…，多个 Key 用逗号或换行分隔"),
                                  axis: .vertical)
                            .lineLimit(1...4)
                    } else {
                        SecureField("API Key", text: $apiKey, prompt: Text(p.hasKey ? "已保存（输入新密钥以替换）" : "sk-…，多个 Key 用逗号分隔"))
                    }
                }
                .onSubmit { saveKey() }
                Button {
                    showKey.toggle()
                } label: {
                    Image(systemName: showKey ? "eye.slash" : "eye")
                }
                .buttonStyle(.borderless)
                .help(showKey ? "隐藏" : "显示")
                Button("保存") { saveKey() }
                    .disabled(apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            HStack(spacing: 10) {
                if p.hasKey {
                    Label(p.keyCount > 1 ? "已保存 \(p.keyCount) 个 Key，轮流使用，加密存储在本机" : "已保存，加密存储在本机",
                          systemImage: "checkmark.seal.fill").foregroundStyle(.green).font(.caption)
                    Button("删除密钥") {
                        Task {
                            if await model.save(id, apiKey: "") { keyMessage = "已删除密钥" }
                        }
                    }
                    .buttonStyle(.link)
                    .font(.caption)
                } else if preset?.needsKey == false {
                    Text("这个服务商通常不需要 API Key").font(.caption).foregroundStyle(.secondary)
                } else {
                    Text("还没有设置 API Key").font(.caption).foregroundStyle(.secondary)
                }
                if let m = keyMessage { Text(m).font(.caption).foregroundStyle(.secondary) }
                Spacer()
                if let s = preset?.keyUrl, let url = URL(string: s) {
                    Link("获取密钥", destination: url).font(.caption)
                }
            }
            if typedKeyCount > 1 {
                Text("将保存 \(typedKeyCount) 个 Key").font(.caption).foregroundStyle(.secondary)
            }
        } header: {
            Text("API 密钥")
        } footer: {
            Text("可以填多个 Key（英文逗号或换行分隔，点眼睛图标后可换行输入），每次请求轮流使用；遇到 401 / 403 / 429 时自动换下一个重试。")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private func saveKey() {
        // Kept as typed (commas / newlines separate several keys); only the outer whitespace goes.
        let key = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !key.isEmpty else { return }
        Task {
            if await model.save(id, apiKey: key) {
                apiKey = ""
                showKey = false
                keyMessage = "已保存"
            }
        }
    }

    private var addressSection: some View {
        Section("API 地址") {
            HStack {
                TextField("地址", text: text(\.baseUrl), prompt: Text(preset?.baseUrl.isEmpty == false ? (preset?.baseUrl ?? "") : "https://…"))
                if let def = preset?.baseUrl, !def.isEmpty, p.baseUrl != def {
                    Button("重置") { edit { $0.baseUrl = def } }
                }
            }
            Text(endpointHint).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            if p.kind == .azureOpenai {
                TextField("API 版本", text: Binding(get: { p.apiVersion ?? "" }, set: { v in
                    edit { $0.apiVersion = v.isEmpty ? nil : v }
                }), prompt: Text("2024-10-21"))
                Text("Azure 的模型 ID 填写部署名称（deployment）。").font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private var endpointHint: String {
        let base = (p.baseUrl.isEmpty ? (preset?.baseUrl ?? "") : p.baseUrl).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard !base.isEmpty else { return "填写服务商提供的 API 地址" }
        switch p.kind {
        case .openai: return "请求地址：\(base)/chat/completions"
        case .anthropic: return "请求地址：\(base)/v1/messages"
        case .gemini: return "请求地址：\(base)/v1beta/models/{模型}:streamGenerateContent"
        case .azureOpenai: return "请求地址：\(base)/openai/deployments/{部署}/chat/completions"
        case .claudeCode, .codex: return ""
        }
    }

    // MARK: CLI

    @ViewBuilder
    private var cliSection: some View {
        let isClaude = p.kind == .claudeCode
        let st = model.cliStatus(for: p) ?? AICLIStatus()
        let exe: String = isClaude ? "claude" : "codex"
        let foundText: String = (st.found ? "已找到 " : "未找到 ") + exe
        let plan: String = isClaude ? "Claude Pro / Max" : "ChatGPT Plus / Pro"
        let note: String = "使用你在本机终端登录的订阅（" + plan + "），不需要 API Key。邮件内容只会交给本机的 " + exe + " 命令处理。"
        Section("本机命令行") {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: st.found ? "checkmark.circle.fill" : "xmark.circle.fill")
                    .foregroundStyle(st.found ? Color.green : Color.red)
                    .font(.title3)
                VStack(alignment: .leading, spacing: 2) {
                    Text(foundText).fontWeight(.medium)
                    if !st.hint.isEmpty {
                        Text(st.hint).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer()
                if model.cliLoading { ProgressView().controlSize(.small) }
                Button("重新检测") { Task { await model.loadCLI() } }
            }
            if let path = st.path { LabeledContent("路径", value: path) }
            if let v = st.version { LabeledContent("版本", value: v) }
            LabeledContent("登录状态", value: loginText(st))
            TextField("自定义路径", text: text(\.baseUrl), prompt: Text("留空自动查找，例如 /opt/homebrew/bin/\(exe)"))
        }
        Section("说明") {
            Text(note)
                .font(.callout)
                .fixedSize(horizontal: false, vertical: true)
            if isClaude {
                AICommandRow(title: "安装", command: "npm i -g @anthropic-ai/claude-code")
                AICommandRow(title: "登录", command: "claude", note: "在 claude 中输入 /login 完成登录")
            } else {
                AICommandRow(title: "安装", command: "npm i -g @openai/codex", note: "或 brew install codex")
                AICommandRow(title: "登录", command: "codex login")
            }
        }
    }

    private func loginText(_ st: AICLIStatus) -> String {
        switch st.loggedIn {
        case .some(true): return "已登录"
        case .some(false): return "未登录"
        case .none: return st.found ? "首次使用时检查" : "—"
        }
    }

    // MARK: Test

    private var testSection: some View {
        Section("检查连接") {
            HStack {
                Picker("模型", selection: $testModel) {
                    if p.models.isEmpty { Text("没有模型").tag("") }
                    ForEach(p.models) { m in Text(m.name).tag(m.id) }
                }
                Button("检查") { test() }
                    .disabled(testModel.isEmpty || testing)
                if testing { ProgressView().controlSize(.small) }
            }
            if let r = testResult {
                if r.ok {
                    Label(successText(r), systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                } else {
                    Label(r.error ?? "连接失败", systemImage: "xmark.octagon.fill")
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                }
            }
            if let e = testError {
                Label(e, systemImage: "xmark.octagon.fill").foregroundStyle(.red).textSelection(.enabled)
            }
        }
    }

    private func successText(_ r: AIProviderTest) -> String {
        var s = "连接成功"
        if let ms = r.latencyMs { s += " · \(ms) ms" }
        if let reply = r.reply?.trimmingCharacters(in: .whitespacesAndNewlines), !reply.isEmpty {
            s += " · 回复：" + String(reply.prefix(40))
        }
        return s
    }

    private func test() {
        testing = true
        testResult = nil
        testError = nil
        let mid = testModel
        Task {
            await model.save(id, apiKey: nil)
            do {
                testResult = try await model.api.testAIProvider(id, model: mid)
            } catch {
                testError = error.localizedDescription
            }
            testing = false
        }
    }

    // MARK: Models

    private var groups: [String] {
        var seen: [String] = []
        for m in p.models where !seen.contains(m.displayGroup) { seen.append(m.displayGroup) }
        return seen
    }

    private func modelEnabled(_ mid: String) -> Binding<Bool> {
        Binding(get: { p.models.first { $0.id == mid }?.enabled ?? false }, set: { on in
            edit { v in
                if let i = v.models.firstIndex(where: { m in m.id == mid }) { v.models[i].enabled = on }
            }
        })
    }

    private func capability(_ mid: String, _ cap: String) -> Binding<Bool> {
        Binding(get: { p.models.first { $0.id == mid }?.capabilities.contains(cap) ?? false }, set: { on in
            edit { v in
                guard let i = v.models.firstIndex(where: { m in m.id == mid }) else { return }
                var caps = v.models[i].capabilities.filter { c in c != cap }
                if on { caps.append(cap) }
                let known = AIModel.capabilityOrder.filter { c in caps.contains(c) }
                let other = caps.filter { c in !AIModel.capabilityOrder.contains(c) }
                v.models[i].capabilities = known + other
            }
        })
    }

    private var modelsSection: some View {
        Section {
            if p.models.isEmpty {
                Text("还没有模型。点「拉取模型」从服务商获取，或在下面手动添加。").font(.callout).foregroundStyle(.secondary)
            }
            ForEach(groups, id: \.self) { g in
                if groups.count > 1 {
                    Text(g).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                }
                ForEach(p.models.filter { $0.displayGroup == g }) { m in
                    HStack {
                        Toggle(isOn: modelEnabled(m.id)) {
                            VStack(alignment: .leading, spacing: 0) {
                                Text(m.name)
                                if m.name != m.id { Text(m.id).font(.caption2).foregroundStyle(.secondary) }
                            }
                        }
                        .toggleStyle(.checkbox)
                        AICapabilityChips(capabilities: m.capabilities)
                        Spacer()
                        Menu {
                            ForEach(AIModel.capabilityOrder, id: \.self) { c in
                                Toggle(isOn: capability(m.id, c)) {
                                    Label(AIModel.capabilityTitle(c), systemImage: AIModel.capabilitySymbol(c))
                                }
                            }
                        } label: {
                            Image(systemName: "tag")
                        }
                        .menuStyle(.borderlessButton)
                        .menuIndicator(.hidden)
                        .fixedSize()
                        .help("设置模型能力（看图、推理、工具、嵌入）")
                        Button {
                            edit { v in v.models.removeAll { x in x.id == m.id } }
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .buttonStyle(.borderless)
                        .help("移除这个模型")
                    }
                }
            }
            HStack {
                TextField("添加模型", text: $newModel, prompt: Text(p.isCLI ? "例如 sonnet" : "模型 ID，例如 deepseek-chat"))
                    .onSubmit { addModel() }
                Button("添加") { addModel() }
                    .disabled(newModel.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            if let e = fetchError { Text(e).font(.caption).foregroundStyle(.red) }
        } header: {
            HStack {
                Text("模型")
                Spacer()
                if fetching { ProgressView().controlSize(.small) }
                Button {
                    fetchModels()
                } label: {
                    Label("拉取模型", systemImage: "arrow.down.circle")
                }
                .buttonStyle(.borderless)
                .disabled(fetching)
            }
        } footer: {
            Text("勾选的模型会出现在 AI 功能 的模型选择中。能力标签（看图、推理、工具、嵌入）按模型 ID 自动推断，可以点标签图标手动调整。")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private func addModel() {
        let mid = newModel.trimmingCharacters(in: .whitespaces)
        guard !mid.isEmpty else { return }
        newModel = ""
        edit { v in
            if !v.models.contains(where: { $0.id == mid }) {
                v.models.append(AIModel(id: mid, capabilities: AIModel.inferCapabilities(mid)))
            }
        }
        if testModel.isEmpty { testModel = mid }
    }

    private func fetchModels() {
        fetching = true
        fetchError = nil
        Task {
            await model.save(id, apiKey: nil)
            do {
                fetched = try await model.api.fetchAIModels(id)
                if fetched.isEmpty { fetchError = "服务商没有返回模型" } else { showFetched = true }
            } catch {
                fetchError = error.localizedDescription
            }
            fetching = false
        }
    }
}

/// Small capability tags of a model ("看图", "推理", "工具", "嵌入").
struct AICapabilityChips: View {
    let capabilities: [String]

    var body: some View {
        HStack(spacing: 3) {
            ForEach(AIModel.capabilityOrder.filter { capabilities.contains($0) }, id: \.self) { c in
                AIChip(text: AIModel.capabilityTitle(c), symbol: AIModel.capabilitySymbol(c), color: Self.color(c))
            }
        }
    }

    static func color(_ c: String) -> Color {
        switch c {
        case "vision": return .green
        case "reasoning": return .orange
        case "tools": return .blue
        case "embedding": return .pink
        default: return .gray
        }
    }
}

struct AICommandRow: View {
    let title: String
    let command: String
    var note: String?

    var body: some View {
        HStack(spacing: 8) {
            Text(title).foregroundStyle(.secondary).frame(width: 36, alignment: .leading)
            Text(command)
                .font(.system(.callout, design: .monospaced))
                .textSelection(.enabled)
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 5))
            Button {
                AIText.copy(command)
                AppState.shared.show(Toast(text: "已复制：\(command)"))
            } label: {
                Image(systemName: "doc.on.doc")
            }
            .buttonStyle(.borderless)
            .help("复制")
            if let note { Text(note).font(.caption).foregroundStyle(.secondary) }
            Spacer()
        }
    }
}

/// Multi-select list for "拉取模型".
struct AIFetchedModelsSheet: View {
    let models: [AIModel]
    let existing: Set<String>
    let onDone: ([AIModel]) -> Void
    @State private var picked: Set<String> = []
    @State private var filter = ""

    private var shown: [AIModel] {
        guard !filter.isEmpty else { return models }
        return models.filter { $0.id.localizedCaseInsensitiveContains(filter) || $0.name.localizedCaseInsensitiveContains(filter) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("选择要添加的模型").font(.headline)
            TextField("筛选", text: $filter).textFieldStyle(.roundedBorder)
            List {
                ForEach(shown) { m in
                    Toggle(isOn: Binding(get: { existing.contains(m.id) || picked.contains(m.id) }, set: { on in
                        if on { picked.insert(m.id) } else { picked.remove(m.id) }
                    })) {
                        HStack {
                            Text(m.name)
                            if m.name != m.id { Text(m.id).font(.caption).foregroundStyle(.secondary) }
                            AICapabilityChips(capabilities: m.capabilities)
                            Spacer()
                            if existing.contains(m.id) { Text("已添加").font(.caption).foregroundStyle(.secondary) }
                        }
                    }
                    .toggleStyle(.checkbox)
                    .disabled(existing.contains(m.id))
                }
            }
            .frame(height: 320)
            HStack {
                Button("全选") { picked = Set(shown.map(\.id)).subtracting(existing) }
                Button("全不选") { picked = [] }
                Spacer()
                Button("取消") { onDone([]) }.keyboardShortcut(.cancelAction)
                Button("添加所选（\(picked.count)）") {
                    onDone(models.filter { picked.contains($0.id) })
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(picked.isEmpty)
            }
        }
        .padding(20)
        .frame(width: 480)
    }
}
