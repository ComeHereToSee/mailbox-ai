import AppKit
import SwiftUI

/// Data for Settings → AI 功能 → MCP (docs/AI.md §8.2, §8.3).
@MainActor
final class AIMCPModel: ObservableObject {
    @Published var info: AIMCPInfo?
    @Published var infoError: String?
    @Published var servers: [AIMCPServer] = []
    @Published var serversLoaded = false
    @Published var serversError: String?
    @Published var tests: [String: AIMCPTestResult] = [:]
    @Published var testing: Set<String> = []
    @Published var regenerating = false

    let api = APIClient.shared

    func load() async {
        await loadInfo()
        await loadServers()
        await AIState.shared.refreshConfirmations()
    }

    func loadInfo() async {
        do {
            info = try await api.aiMCPInfo()
            infoError = nil
        } catch {
            infoError = error.localizedDescription
        }
    }

    func loadServers() async {
        do {
            servers = try await api.aiMCPServers()
            serversError = nil
        } catch {
            serversError = error.localizedDescription
        }
        serversLoaded = true
    }

    func regenerateToken() async {
        regenerating = true
        defer { regenerating = false }
        do {
            let token = try await api.regenerateAIMCPToken()
            if info == nil { info = AIMCPInfo() }
            info?.token = token
            AppState.shared.show(Toast(text: "已生成新令牌，请更新各客户端的配置"))
            await loadInfo()
        } catch {
            infoError = error.localizedDescription
        }
    }

    /// Saves and returns the stored server (looked up by name when the engine does not echo it).
    @discardableResult
    func save(_ s: AIMCPServer) async -> AIMCPServer? {
        do {
            let echoed = try await api.saveAIMCPServer(s)
            await loadServers()
            if let echoed { return echoed }
            if !s.id.isEmpty { return servers.first { $0.id == s.id } }
            return servers.last { $0.name == s.name }
        } catch {
            serversError = error.localizedDescription
            return nil
        }
    }

    func delete(_ s: AIMCPServer) async {
        do {
            try await api.deleteAIMCPServer(s.id)
            tests[s.id] = nil
            serversError = nil
        } catch {
            serversError = error.localizedDescription
        }
        await loadServers()
    }

    func test(_ id: String) async {
        guard !id.isEmpty else { return }
        testing.insert(id)
        defer { testing.remove(id) }
        do {
            tests[id] = try await api.testAIMCPServer(id)
        } catch {
            tests[id] = AIMCPTestResult(ok: false, error: error.localizedDescription)
        }
    }

    // MARK: Client snippets

    /// `<main app>/Contents/Library/LoginItems/MailboxAIHelper.app/Contents/MacOS/MailboxAIHelper`.
    var helperPath: String {
        if let s = info?.stdioCommand, !s.isEmpty { return s }
        if let url = Daemon.helperExecutableURL { return url.path }
        return Bundle.main.bundleURL
            .appendingPathComponent("Contents/Library/LoginItems/MailboxAIHelper.app/Contents/MacOS/\(Daemon.helperExecutableName)").path
    }

    var url: String {
        if let u = info?.url, !u.isEmpty { return u }
        if let base = api.baseURL { return base.appendingPathComponent("mcp").absoluteString }
        return "http://127.0.0.1:<端口>/mcp"
    }

    var token: String {
        if let t = info?.token, !t.isEmpty { return t }
        return "<令牌>"
    }

    var claudeSnippet: String {
        "claude mcp add --transport http mailbox-ai \(url) --header \"Authorization: Bearer \(token)\""
    }

    var codexSnippet: String {
        let path = helperPath.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
        return [
            "[mcp_servers.mailbox-ai]",
            "command = \"\(path)\"",
            "args = [\"--mcp\"]",
        ].joined(separator: "\n")
    }

    var jsonSnippet: String {
        [
            "{",
            "  \"mcpServers\": {",
            "    \"mailbox-ai\": {",
            "      \"type\": \"streamableHttp\",",
            "      \"url\": \"\(url)\",",
            "      \"headers\": {",
            "        \"Authorization\": \"Bearer \(token)\"",
            "      }",
            "    }",
            "  }",
            "}",
        ].joined(separator: "\n")
    }
}

// MARK: - Pane

struct AIMCPSettings: View {
    @StateObject private var model = AIMCPModel()
    @ObservedObject private var ai = AIState.shared
    @State private var showToken = false
    @State private var editing: AIMCPServer? = nil

    var body: some View {
        Form {
            serverSection
            if ai.settings.mcpEnabled {
                clientsSection
            }
            confirmSection
            externalSection
        }
        .formStyle(.grouped)
        .task { await model.load() }
        .onChange(of: ai.confirmEvents) { _, _ in
            Task { await ai.refreshConfirmations() }
        }
        .sheet(item: $editing) { s in
            AIMCPServerEditor(server: s) { saved in
                editing = nil
                guard let saved else { return }
                Task {
                    if let stored = await model.save(saved) { await model.test(stored.id) }
                }
            }
        }
    }

    // MARK: Built-in server

    private var serverSection: some View {
        Section {
            Toggle(isOn: Binding(get: { ai.settings.mcpEnabled }, set: { on in
                ai.settings.mcpEnabled = on
                Task {
                    await ai.saveSettingsNow()
                    await model.loadInfo()
                }
            })) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("启用 MCP 服务")
                    Text("让 Claude Code、Codex、Cherry Studio 等 MCP 客户端搜索、阅读和整理邮件。发送和删除邮件前会先请你确认。")
                        .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            }
            .toggleStyle(.switch)
            LabeledContent("地址") {
                HStack(spacing: 6) {
                    Text(model.url).font(.system(.callout, design: .monospaced)).textSelection(.enabled).lineLimit(1)
                    copyButton(model.url, what: "地址")
                }
            }
            LabeledContent("令牌") {
                HStack(spacing: 6) {
                    Text(showToken ? model.token : masked(model.token))
                        .font(.system(.callout, design: .monospaced))
                        .textSelection(.enabled)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Button {
                        showToken.toggle()
                    } label: {
                        Image(systemName: showToken ? "eye.slash" : "eye")
                    }
                    .buttonStyle(.borderless)
                    .help(showToken ? "隐藏" : "显示")
                    copyButton(model.token, what: "令牌")
                    if model.regenerating { ProgressView().controlSize(.small) }
                    Button("重新生成…") { confirmRegenerate() }
                        .disabled(model.regenerating)
                }
            }
            if let e = model.infoError {
                Text(e).font(.caption).foregroundStyle(.red)
            }
        } header: {
            Text("内置 MCP 服务")
        } footer: {
            Text("只监听本机（127.0.0.1）。令牌固定保存在本机，重新生成后旧令牌立即失效。")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private var clientsSection: some View {
        Section {
            AISnippetView(title: "Claude Code", note: "在终端运行：", snippet: model.claudeSnippet)
            AISnippetView(title: "Codex", note: "加入 ~/.codex/config.toml（通过本机 stdio 桥连接，不需要令牌）：", snippet: model.codexSnippet)
            AISnippetView(title: "Cherry Studio / 通用 JSON", note: "在 MCP 设置中「从 JSON 导入」，或填写 Streamable HTTP 地址和请求头：",
                          snippet: model.jsonSnippet)
        } header: {
            Text("客户端配置")
        }
    }

    private func masked(_ token: String) -> String {
        guard token.count > 8, !token.hasPrefix("<") else { return token }
        return String(repeating: "•", count: 12) + token.suffix(4)
    }

    private func copyButton(_ text: String, what: String) -> some View {
        Button {
            AIText.copy(text)
            AppState.shared.show(Toast(text: "已复制\(what)"))
        } label: {
            Image(systemName: "doc.on.doc")
        }
        .buttonStyle(.borderless)
        .help("复制")
    }

    private func confirmRegenerate() {
        let alert = NSAlert()
        alert.messageText = "重新生成 MCP 令牌？"
        alert.informativeText = "旧令牌会立即失效，使用 HTTP 方式连接的客户端（Claude Code、Cherry Studio 等）需要更新配置。"
        alert.alertStyle = .warning
        alert.addButton(withTitle: "重新生成")
        alert.addButton(withTitle: "取消")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        Task { await model.regenerateToken() }
    }

    // MARK: Pending confirmations

    private var confirmSection: some View {
        Section {
            if ai.confirmations.isEmpty {
                Text("没有等待确认的请求。MCP 客户端要发送或删除邮件时，会弹出窗口并发送通知请你确认，3 分钟内没有回应会自动拒绝。")
                    .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            ForEach(ai.confirmations) { c in
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: c.isDelete ? "trash" : "paperplane")
                        .foregroundStyle(c.isDelete ? Color.red : Color.accentColor)
                        .frame(width: 18)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(c.title.isEmpty ? (c.isDelete ? "删除邮件" : "发送邮件") : c.title).fontWeight(.medium)
                        Text(c.detail).font(.caption).foregroundStyle(.secondary).lineLimit(3)
                    }
                    Spacer()
                    Button("拒绝") { Task { await ai.decide(c, approve: false) } }
                    Button("允许") { Task { await ai.decide(c, approve: true) } }
                        .buttonStyle(.borderedProminent)
                        .tint(c.isDelete ? Color.red : Color.accentColor)
                }
            }
        } header: {
            HStack {
                Text("待确认")
                Spacer()
                Button {
                    Task { await ai.refreshConfirmations() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .help("刷新")
            }
        }
    }

    // MARK: External servers (§8.3)

    private var externalSection: some View {
        Section {
            if model.servers.isEmpty && model.serversLoaded {
                Text("还没有外部 MCP 服务器。添加后，在 AI 助手中打开「🔧 使用工具」，AI 就可以调用它们提供的工具（例如查询日程、读写笔记）。")
                    .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            ForEach(model.servers) { s in
                serverRow(s)
            }
            HStack {
                Button {
                    var s = AIMCPServer()
                    s.name = "my-tools"
                    editing = s
                } label: {
                    Label("添加服务器…", systemImage: "plus")
                }
                if let e = model.serversError { Text(e).font(.caption).foregroundStyle(.red).lineLimit(2) }
                Spacer()
            }
        } header: {
            Text("外部工具")
        } footer: {
            Text("本地命令（stdio）会在每次对话时启动、结束后关闭；远程服务器使用 Streamable HTTP。Claude Code 订阅也可以使用这些工具，Codex 暂不支持。")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private func serverRow(_ s: AIMCPServer) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Toggle("", isOn: Binding(get: { s.enabled }, set: { on in
                    var v = s
                    v.enabled = on
                    Task { await model.save(v) }
                }))
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.mini)
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 6) {
                        Text(s.name.isEmpty ? "未命名" : s.name).fontWeight(.medium)
                        AIChip(text: s.isHTTP ? "HTTP" : "stdio", color: s.isHTTP ? .blue : .purple)
                        if !s.enabled { Text("已停用").font(.caption2).foregroundStyle(.secondary) }
                    }
                    Text(s.isHTTP ? s.url : ([s.command] + s.args).joined(separator: " "))
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer()
                if model.testing.contains(s.id) { ProgressView().controlSize(.small) }
                Button("测试") { Task { await model.test(s.id) } }
                    .disabled(model.testing.contains(s.id) || s.id.isEmpty)
                Button("编辑") { editing = s }
                Button(role: .destructive) {
                    confirmDelete(s)
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .help("删除")
            }
            if let r = model.tests[s.id] {
                AIMCPTestView(result: r)
            }
        }
        .padding(.vertical, 2)
    }

    private func confirmDelete(_ s: AIMCPServer) {
        let alert = NSAlert()
        alert.messageText = "删除「\(s.name)」？"
        alert.addButton(withTitle: "删除")
        alert.addButton(withTitle: "取消")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        Task { await model.delete(s) }
    }
}

/// A copyable configuration block.
private struct AISnippetView: View {
    let title: String
    let note: String
    let snippet: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(title).fontWeight(.medium)
                Spacer()
                Button {
                    AIText.copy(snippet)
                    AppState.shared.show(Toast(text: "已复制 \(title) 配置"))
                } label: {
                    Label("复制", systemImage: "doc.on.doc")
                }
                .buttonStyle(.borderless)
            }
            Text(note).font(.caption).foregroundStyle(.secondary)
            Text(snippet)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
                .padding(8)
                .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 6))
        }
        .padding(.vertical, 2)
    }
}

/// "可用 · N 个工具" with the tool list, or the error.
private struct AIMCPTestView: View {
    let result: AIMCPTestResult
    @State private var expanded = false

    var body: some View {
        if result.ok {
            VStack(alignment: .leading, spacing: 4) {
                Button {
                    expanded.toggle()
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                        Text("连接成功 · \(result.tools.count) 个工具").font(.caption)
                        if !result.tools.isEmpty {
                            Image(systemName: expanded ? "chevron.up" : "chevron.down").font(.caption2)
                        }
                    }
                }
                .buttonStyle(.plain)
                if expanded {
                    ForEach(result.tools) { t in
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text(t.name).font(.system(.caption, design: .monospaced)).fontWeight(.medium)
                            Text(t.description).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                        }
                    }
                }
            }
            .padding(.leading, 34)
        } else {
            Label(result.error ?? "连接失败", systemImage: "xmark.octagon.fill")
                .font(.caption)
                .foregroundStyle(.red)
                .textSelection(.enabled)
                .padding(.leading, 34)
        }
    }
}

// MARK: - Server editor

struct AIMCPServerEditor: View {
    @State private var server: AIMCPServer
    @State private var argsText: String
    @State private var envText: String
    @State private var headersText: String
    let onDone: (AIMCPServer?) -> Void

    init(server: AIMCPServer, onDone: @escaping (AIMCPServer?) -> Void) {
        _server = State(initialValue: server)
        _argsText = State(initialValue: server.args.joined(separator: "\n"))
        _envText = State(initialValue: server.env.keys.sorted().map { "\($0)=\(server.env[$0] ?? "")" }.joined(separator: "\n"))
        _headersText = State(initialValue: server.headers.keys.sorted().map { "\($0): \(server.headers[$0] ?? "")" }.joined(separator: "\n"))
        self.onDone = onDone
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(server.id.isEmpty ? "添加 MCP 服务器" : "编辑 MCP 服务器").font(.headline)
            Form {
                TextField("名称", text: $server.name, prompt: Text("例如 calendar（工具名会以它为前缀）"))
                Picker("连接方式", selection: $server.transport) {
                    Text("本地命令（stdio）").tag("stdio")
                    Text("远程（Streamable HTTP）").tag("http")
                }
                .pickerStyle(.segmented)
                if server.isHTTP {
                    TextField("地址", text: $server.url, prompt: Text("https://example.com/mcp"))
                    editor("请求头（每行一个，名称: 值）", text: $headersText, height: 70)
                } else {
                    TextField("命令", text: $server.command, prompt: Text("例如 npx、uvx 或可执行文件的完整路径"))
                    editor("参数（每行一个）", text: $argsText, height: 70)
                    editor("环境变量（每行一个，KEY=VALUE）", text: $envText, height: 60)
                }
                Toggle("启用", isOn: $server.enabled)
            }
            .formStyle(.grouped)
            HStack {
                Text("保存后会自动测试并列出工具。").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("取消") { onDone(nil) }.keyboardShortcut(.cancelAction)
                Button("保存") { onDone(collected) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(!canSave)
            }
        }
        .padding(20)
        .frame(width: 560, height: 520)
    }

    private func editor(_ title: String, text: Binding<String>, height: CGFloat) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
            TextEditor(text: text)
                .font(.system(.body, design: .monospaced))
                .frame(height: height)
                .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.quaternary))
        }
    }

    private var canSave: Bool {
        guard !server.name.trimmingCharacters(in: .whitespaces).isEmpty else { return false }
        if server.isHTTP { return !server.url.trimmingCharacters(in: .whitespaces).isEmpty }
        return !server.command.trimmingCharacters(in: .whitespaces).isEmpty
    }

    private static func lines(_ text: String) -> [String] {
        text.components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    private static func pairs(_ text: String, separator: Character) -> [String: String] {
        var out: [String: String] = [:]
        for line in lines(text) {
            guard let i = line.firstIndex(of: separator) else { continue }
            let key = line[..<i].trimmingCharacters(in: .whitespaces)
            let value = line[line.index(after: i)...].trimmingCharacters(in: .whitespaces)
            if !key.isEmpty { out[key] = value }
        }
        return out
    }

    private var collected: AIMCPServer {
        var s = server
        s.name = s.name.trimmingCharacters(in: .whitespaces)
        s.command = s.command.trimmingCharacters(in: .whitespaces)
        s.url = s.url.trimmingCharacters(in: .whitespaces)
        s.args = Self.lines(argsText)
        s.env = Self.pairs(envText, separator: "=")
        s.headers = Self.pairs(headersText, separator: ":")
        return s
    }
}

// MARK: - Confirmation window

/// "需要你的确认": an MCP client wants to send or delete mail (docs/AI.md §8.2).
/// Shows the oldest pending request; closes itself when nothing is left (also when the helper's
/// notification answered first).
struct AIConfirmWindow: View {
    @ObservedObject private var ai = AIState.shared
    @Environment(\.dismissWindow) private var dismissWindow
    @State private var busy = false

    var body: some View {
        Group {
            if let c = ai.confirmations.first {
                content(c)
            } else {
                VStack(spacing: 10) {
                    ProgressView()
                    Text("正在读取待确认的请求…").foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 180)
                .padding(20)
            }
        }
        .frame(width: 540)
        .background { AIFloatingWindow() }
        .task {
            // Drop requests answered elsewhere or expired; close when none is left.
            while !Task.isCancelled {
                await ai.refreshConfirmations()
                if ai.confirmations.isEmpty {
                    dismissWindow(id: "ai-confirm")
                    break
                }
                try? await Task.sleep(nanoseconds: 3_000_000_000)
            }
        }
        .onChange(of: ai.confirmations.isEmpty) { _, empty in
            if empty { dismissWindow(id: "ai-confirm") }
        }
    }

    private func content(_ c: AIConfirmation) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: c.isDelete ? "trash.circle.fill" : "paperplane.circle.fill")
                    .font(.system(size: 38))
                    .foregroundStyle(c.isDelete ? Color.red : Color.accentColor)
                VStack(alignment: .leading, spacing: 4) {
                    Text(c.title.isEmpty ? (c.isDelete ? "删除邮件" : "发送邮件") : c.title)
                        .font(.headline)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(c.isDelete ? "MCP 客户端请求把下面的邮件移到废纸篓。" : "MCP 客户端请求发送下面这封邮件。")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }
            ScrollView {
                Text(c.detail.isEmpty ? "（没有详情）" : c.detail)
                    .font(.callout)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
            }
            .frame(minHeight: 120, maxHeight: 280)
            .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 8))
            HStack(spacing: 10) {
                if c.expiresAt > 0 {
                    TimelineView(.periodic(from: Date(), by: 1)) { context in
                        Text(remaining(c.expiresAt, now: context.date))
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                    }
                }
                if ai.confirmations.count > 1 {
                    Text("还有 \(ai.confirmations.count - 1) 个请求等待确认").font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                if busy { ProgressView().controlSize(.small) }
                Button("拒绝") { decide(c, approve: false) }
                    .keyboardShortcut(.cancelAction)
                    .disabled(busy)
                Button("允许") { decide(c, approve: true) }
                    .buttonStyle(.borderedProminent)
                    .tint(c.isDelete ? Color.red : Color.accentColor)
                    .disabled(busy)
            }
        }
        .padding(20)
    }

    private func remaining(_ expiresAt: Int64, now: Date) -> String {
        let left = max(0, Int(TimeInterval(expiresAt) - now.timeIntervalSince1970))
        return left > 0 ? "\(left) 秒后自动拒绝" : "已超时"
    }

    private func decide(_ c: AIConfirmation, approve: Bool) {
        busy = true
        Task {
            await ai.decide(c, approve: approve)
            busy = false
        }
    }
}

/// Keeps the confirmation window above other windows.
private struct AIFloatingWindow: NSViewRepresentable {
    final class Probe: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            window?.level = .floating
        }
    }

    func makeNSView(context: Context) -> NSView { Probe() }
    func updateNSView(_ nsView: NSView, context: Context) {}
}
