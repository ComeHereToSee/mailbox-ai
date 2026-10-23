import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct SettingsView: View {
    @EnvironmentObject private var state: AppState
    /// Also set by `mailboxai://settings?tab=…`.
    @AppStorage("settingsTab") private var tab = "general"

    var body: some View {
        TabView(selection: $tab) {
            GeneralSettings().tabItem { Label("通用", systemImage: "gearshape") }.tag("general")
            AccountsSettings().tabItem { Label("账户", systemImage: "person.crop.circle") }.tag("accounts")
            NotificationSettings().tabItem { Label("通知", systemImage: "bell.badge") }.tag("notifications")
            RulesSettings().tabItem { Label("规则", systemImage: "line.3.horizontal.decrease.circle") }.tag("rules")
            ComposeSettings().tabItem { Label("写信", systemImage: "square.and.pencil") }.tag("compose")
            ForwarderSettings().tabItem { Label("推送转发", systemImage: "iphone.radiowaves.left.and.right") }.tag("forwarders")
            PrivacySettings().tabItem { Label("隐私", systemImage: "hand.raised") }.tag("privacy")
            CryptoSettings().tabItem { Label("加密", systemImage: "lock.shield") }.tag("crypto")
            AdvancedSettings().tabItem { Label("高级", systemImage: "wrench.and.screwdriver") }.tag("advanced")
        }
        .environmentObject(state)
        .frame(width: 800, height: 560)
    }
}

/// Saves settings after a short pause so toggles feel instant.
@MainActor
private func persist(_ state: AppState) {
    Task { _ = try? await state.api.saveSettings(state.settings) }
}

struct GeneralSettings: View {
    @EnvironmentObject private var state: AppState
    @State private var loginItem = LoginItem.isEnabled
    @State private var isDefault = DefaultMailApp.isDefault
    @State private var loginError: String?

    var body: some View {
        Form {
            Section("启动") {
                Toggle("登录时在后台启动（持续收信和提醒）", isOn: Binding(get: { loginItem }, set: { v in
                    do {
                        try LoginItem.setEnabled(v)
                        loginItem = LoginItem.isEnabled
                        loginError = nil
                    } catch {
                        loginError = error.localizedDescription
                    }
                }))
                if let loginError { Text(loginError).foregroundStyle(.red).font(.caption) }
                HStack {
                    Text(isDefault ? "Mailbox 是默认邮件应用" : "默认邮件应用：其他应用")
                    Spacer()
                    if !isDefault {
                        Button("设为默认") {
                            Task {
                                try? await DefaultMailApp.makeDefault()
                                isDefault = DefaultMailApp.isDefault
                            }
                        }
                    }
                }
            }
            Section("阅读") {
                Toggle("按会话分组", isOn: $state.settings.conversationView)
                Toggle("打开邮件时标为已读", isOn: $state.settings.markReadOnOpen)
                Stepper("延迟 \(state.settings.markReadDelaySecs) 秒标为已读", value: $state.settings.markReadDelaySecs, in: 0...30)
                    .disabled(!state.settings.markReadOnOpen)
                Toggle("收件箱分类（主要 / 社交 / 推广 / 通知 / 论坛）", isOn: $state.settings.categoriesEnabled)
                Toggle("列表显示摘要", isOn: $state.settings.showSnippets)
                Picker("列表密度", selection: $state.settings.listDensity) {
                    Text("舒适").tag("comfortable")
                    Text("紧凑").tag("compact")
                }
            }
        }
        .formStyle(.grouped)
        .onChange(of: state.settings) { _, _ in persist(state) }
    }
}

struct AccountsSettings: View {
    @EnvironmentObject private var state: AppState
    @State private var selected: String?

    var body: some View {
        HSplitView {
            VStack(spacing: 0) {
                List(selection: $selected) {
                    ForEach(state.accounts) { a in
                        HStack {
                            Circle().fill(Color(hex: a.color)).frame(width: 10, height: 10)
                            VStack(alignment: .leading) {
                                Text(a.displayLabel)
                                Text(a.email).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            if let s = state.statuses[a.id], s.state == .error {
                                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                            }
                        }
                        .tag(a.id)
                    }
                    .onMove { from, to in
                        var ids = state.accounts.map(\.id)
                        ids.move(fromOffsets: from, toOffset: to)
                        Task {
                            try? await state.api.reorderAccounts(ids)
                            await state.reloadAll()
                        }
                    }
                }
                Divider()
                HStack {
                    Button { state.showAddAccount = true } label: { Image(systemName: "plus") }
                    Button {
                        guard let id = selected else { return }
                        let alert = NSAlert()
                        alert.messageText = "删除这个账户？"
                        alert.informativeText = "只会删除本机的缓存和登录信息，服务器上的邮件不受影响。"
                        alert.addButton(withTitle: "删除")
                        alert.addButton(withTitle: "取消")
                        if alert.runModal() == .alertFirstButtonReturn {
                            Task {
                                try? await state.api.deleteAccount(id)
                                selected = nil
                                await state.reloadAll()
                            }
                        }
                    } label: { Image(systemName: "minus") }
                    .disabled(selected == nil)
                    Spacer()
                }
                .buttonStyle(.borderless)
                .padding(6)
            }
            .frame(minWidth: 200, maxWidth: 240)
            Group {
                if let id = selected, let acc = state.accountsById[id] {
                    AccountEditor(account: acc).id(acc.id)
                } else {
                    ContentUnavailableView("选择一个账户", systemImage: "person.crop.circle")
                }
            }
            .frame(minWidth: 480)
        }
        .onAppear { if selected == nil { selected = state.accounts.first?.id } }
        .sheet(isPresented: $state.showAddAccount) { AddAccountView().environmentObject(state) }
    }
}

struct AccountEditor: View {
    @EnvironmentObject private var state: AppState
    @State var account: Account
    @State private var password = ""
    @State private var smtpPassword = ""
    @State private var saving = false
    @State private var message: String?
    @State private var identities: [Identity] = []

    var body: some View {
        Form {
            Section("账户") {
                TextField("标签（通知中显示）", text: $account.label)
                TextField("显示名称", text: $account.name)
                ColorPicker("颜色", selection: Binding(get: { Color(hex: account.color) }, set: { c in
                    if let s = NSColor(c).usingColorSpace(.sRGB) {
                        account.color = String(format: "#%02X%02X%02X", Int(s.redComponent * 255), Int(s.greenComponent * 255), Int(s.blueComponent * 255))
                    }
                }), supportsOpacity: false)
                Toggle("启用同步", isOn: $account.enabled)
                LabeledContent("地址", value: account.email)
                if let s = state.statuses[account.id] {
                    LabeledContent("状态", value: statusText(s))
                    if let e = s.error { Text(e).font(.caption).foregroundStyle(.red).textSelection(.enabled) }
                }
            }
            if account.kind != .web {
                Section("登录") {
                    if account.config.auth == .oauth2 {
                        Button("重新登录（\(account.config.oauthProvider ?? "OAuth")）") { Task { await reauth() } }
                    } else {
                        SecureField("新密码 / 授权码（不修改请留空）", text: $password)
                        SecureField("SMTP 密码（不同时填写）", text: $smtpPassword)
                    }
                }
                Section(account.kind == .pop3 ? "POP3" : "IMAP") {
                    if account.kind == .pop3 {
                        ServerFields(server: Binding(get: { account.config.pop3 ?? ServerConfig() }, set: { account.config.pop3 = $0 }))
                        Toggle("在服务器上保留邮件", isOn: $account.config.popLeaveOnServer)
                        Stepper(value: Binding(get: { account.config.popDeleteAfterDays ?? 0 }, set: { account.config.popDeleteAfterDays = $0 == 0 ? nil : $0 }), in: 0...365) {
                            Text(account.config.popDeleteAfterDays.map { "\($0) 天后从服务器删除" } ?? "永不从服务器删除")
                        }
                        Toggle("本地永久删除时同时删除服务器上的邮件", isOn: $account.config.popDeleteOnLocalDelete)
                    } else {
                        ServerFields(server: Binding(get: { account.config.imap ?? ServerConfig() }, set: { account.config.imap = $0 }))
                        Toggle("使用 IDLE 实时推送", isOn: $account.config.useIdle)
                        Toggle("发送 IMAP ID（网易邮箱必须）", isOn: $account.config.sendId)
                    }
                    Picker("本地缓存", selection: $account.config.syncDays) {
                        Text("最近 30 天").tag(30)
                        Text("最近 90 天").tag(90)
                        Text("最近 1 年").tag(365)
                        Text("全部").tag(0)
                    }
                    Picker("检查频率", selection: $account.config.pollIntervalSecs) {
                        Text("1 分钟").tag(60)
                        Text("5 分钟").tag(300)
                        Text("15 分钟").tag(900)
                        Text("30 分钟").tag(1800)
                    }
                }
                Section("SMTP") {
                    ServerFields(server: Binding(get: { account.config.smtp ?? ServerConfig(port: 465) }, set: { account.config.smtp = $0 }))
                    Toggle("发送后保存副本到“已发送”", isOn: $account.config.saveSentCopy)
                }
                Section("发件身份与签名") {
                    ForEach($identities) { $i in
                        IdentityEditor(identity: $i)
                    }
                    Button("添加别名身份") {
                        identities.append(Identity(accountId: account.id, name: account.name, email: account.email))
                    }
                }
            } else {
                Section("网页模式") {
                    TextField("网址", text: Binding(get: { account.config.webUrl ?? "" }, set: { account.config.webUrl = $0 }))
                }
            }
            HStack {
                if let message { Text(message).font(.caption).foregroundStyle(.secondary) }
                Spacer()
                if saving { ProgressView().controlSize(.small) }
                Button("保存") { Task { await save() } }.buttonStyle(.borderedProminent)
            }
        }
        .formStyle(.grouped)
        .task { identities = (try? await state.api.identities(account: account.id)) ?? [] }
    }

    private func statusText(_ s: AccountStatus) -> String {
        let when = s.lastSync.map { "，上次同步 " + Fmt.relative($0) } ?? ""
        switch s.state {
        case .idle: return "正常" + when
        case .syncing: return "正在同步"
        case .connecting: return "正在连接"
        case .error: return "出错"
        case .offline: return "离线" + when
        case .disabled: return "已停用"
        }
    }

    private func save() async {
        saving = true
        defer { saving = false }
        do {
            account = try await state.api.updateAccount(account, password: password.isEmpty ? nil : password,
                                                        smtpPassword: smtpPassword.isEmpty ? nil : smtpPassword)
            for i in identities {
                _ = try await state.api.saveIdentity(i)
            }
            identities = (try? await state.api.identities(account: account.id)) ?? identities
            password = ""
            message = "已保存"
            await state.reloadAll()
        } catch {
            message = error.localizedDescription
        }
    }

    private func reauth() async {
        guard let op = account.config.oauthProvider else { return }
        do {
            let r = try await state.api.startOAuth(provider: op, loginHint: account.email)
            NSWorkspace.shared.open(r.url)
            for _ in 0..<600 {
                try await Task.sleep(nanoseconds: 1_000_000_000)
                if try await state.api.oauthStatus(r.state).done {
                    try await state.api.reauth(account.id, state: r.state)
                    message = "已重新授权"
                    return
                }
            }
        } catch {
            message = error.localizedDescription
        }
    }
}

struct IdentityEditor: View {
    @Binding var identity: Identity

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                TextField("名称", text: $identity.name)
                TextField("地址", text: $identity.email)
                Toggle("默认", isOn: $identity.isDefault).toggleStyle(.checkbox)
            }
            TextField("回复地址（可选）", text: Binding(get: { identity.replyTo ?? "" }, set: { identity.replyTo = $0.isEmpty ? nil : $0 }))
            Text("签名（支持 HTML）").font(.caption).foregroundStyle(.secondary)
            TextEditor(text: $identity.signatureHtml)
                .font(.system(.caption, design: .monospaced))
                .frame(height: 70)
                .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.quaternary))
        }
        .padding(.vertical, 4)
    }
}

struct NotificationSettings: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        Form {
            Section("免打扰") {
                Toggle("开启免打扰时段（VIP 来信仍会提醒）", isOn: $state.settings.quietHours.enabled)
                HStack {
                    TextField("开始", text: $state.settings.quietHours.start).frame(width: 80)
                    Text("至")
                    TextField("结束", text: $state.settings.quietHours.end).frame(width: 80)
                }
                .disabled(!state.settings.quietHours.enabled)
                if let until = state.settings.notificationsPausedUntil, until > Int64(Date().timeIntervalSince1970) {
                    HStack {
                        Text("通知已暂停至 \(Fmt.full(until))")
                        Button("恢复") {
                            Task { if let s = try? await state.api.pauseNotifications(minutes: 0) { state.settings = s } }
                        }
                    }
                }
            }
            Section("按账户") {
                ForEach(state.accounts.filter { $0.kind != .web }) { acc in
                    AccountNotifyRow(account: acc)
                }
            }
            Section {
                Text("通知由后台的 Mailbox Helper 发出，标题会注明来自哪个账户，例如“[QQ·工作] 张三”。可在 系统设置 → 通知 → Mailbox Helper 调整样式。")
                    .font(.caption).foregroundStyle(.secondary)
                Button("打开系统通知设置") {
                    if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension") { NSWorkspace.shared.open(url) }
                }
            }
        }
        .formStyle(.grouped)
        .onChange(of: state.settings) { _, _ in persist(state) }
    }
}

struct AccountNotifyRow: View {
    @EnvironmentObject private var state: AppState
    @State var account: Account

    var body: some View {
        DisclosureGroup {
            Toggle("仅 VIP 发件人", isOn: $account.config.notify.vipOnly)
            Toggle("声音", isOn: $account.config.notify.sound)
            HStack {
                Text("提醒的分类")
                ForEach(Category.allCases) { c in
                    Toggle(c.title, isOn: Binding(get: { account.config.notify.categories.contains(c) }, set: { on in
                        if on { account.config.notify.categories.append(c) } else { account.config.notify.categories.removeAll { $0 == c } }
                    }))
                    .toggleStyle(.checkbox)
                }
            }
        } label: {
            Toggle(isOn: $account.config.notify.enabled) {
                HStack {
                    Circle().fill(Color(hex: account.color)).frame(width: 8, height: 8)
                    Text(account.displayLabel)
                }
            }
        }
        .onChange(of: account) { _, a in
            Task { _ = try? await state.api.updateAccount(a) }
        }
    }
}

struct RulesSettings: View {
    @EnvironmentObject private var state: AppState
    @State private var rules: [Rule] = []
    @State private var editing: Rule?

    var body: some View {
        VStack(alignment: .leading) {
            List {
                ForEach(rules) { r in
                    HStack {
                        Toggle("", isOn: Binding(get: { r.enabled }, set: { v in
                            var nr = r
                            nr.enabled = v
                            Task { await save(nr) }
                        }))
                        .labelsHidden()
                        VStack(alignment: .leading) {
                            Text(r.name.isEmpty ? "未命名规则" : r.name)
                            Text(summary(r)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Spacer()
                        Button("编辑") { editing = r }
                        Button("对收件箱执行") {
                            Task {
                                let n = (try? await state.api.runRule(r.id)) ?? 0
                                state.show(Toast(text: "规则作用于 \(n) 封邮件"))
                            }
                        }
                        Button(role: .destructive) {
                            Task {
                                try? await state.api.deleteRule(r.id)
                                await load()
                            }
                        } label: { Image(systemName: "trash") }
                    }
                }
            }
            HStack {
                Button("新建规则") { editing = Rule(name: "新规则") }
                Spacer()
                Text("规则按顺序作用于新收到的邮件。").font(.caption).foregroundStyle(.secondary)
            }
            .padding(.horizontal)
            SieveBox().padding([.horizontal, .bottom])
        }
        .task { await load() }
        .sheet(item: $editing) { r in
            RuleEditor(rule: r) { saved in
                editing = nil
                if let saved { Task { await save(saved) } }
            }
            .environmentObject(state)
        }
    }

    private func load() async {
        rules = (try? await state.api.rules()) ?? []
    }

    private func save(_ r: Rule) async {
        _ = try? await state.api.saveRule(r)
        await load()
    }

    private func summary(_ r: Rule) -> String {
        let c = r.conditions.map { "\(RuleEditor.fieldName($0.field)) \(RuleEditor.opName($0.op)) \($0.value)" }.joined(separator: r.matchAll ? " 且 " : " 或 ")
        let a = r.actions.map { RuleEditor.actionName($0.kind) + ($0.value.isEmpty ? "" : " \($0.value)") }.joined(separator: "，")
        return "如果 \(c) → \(a)"
    }
}

/// Uploads rules as a server-side Sieve script (works even when the Mac is off).
struct SieveBox: View {
    @EnvironmentObject private var state: AppState
    @State private var accountId = ""
    @State private var status: APIClient.SieveStatus?
    @State private var busy = false
    @State private var message: String?
    @State private var showScript = false

    var body: some View {
        GroupBox("服务器端规则（Sieve）") {
            VStack(alignment: .leading, spacing: 8) {
                Text("支持 ManageSieve 的邮箱（Fastmail、mailbox.org、自建 Dovecot 等）可以把规则上传到服务器，Mac 关机时也会生效。移动、标记、转发会在服务器上执行；标签、分类、通知等只在本地生效。")
                    .font(.caption).foregroundStyle(.secondary)
                HStack {
                    Picker("账户", selection: $accountId) {
                        ForEach(state.accounts.filter { $0.kind == .imap }) { Text($0.displayLabel).tag($0.id) }
                    }
                    .frame(width: 260)
                    Button("检查") { Task { await check() } }
                    Button("上传规则") { Task { await upload() } }.disabled(status?.supported != true || busy)
                    Button("移除") { Task { await remove() } }.disabled(status?.supported != true || busy)
                    if busy { ProgressView().controlSize(.small) }
                }
                if let s = status {
                    if s.supported {
                        Text("服务器支持 Sieve。脚本：" + (s.scripts.isEmpty ? "无" : s.scripts.map { $0.name + ($0.active ? "（启用）" : "") }.joined(separator: "、")))
                            .font(.caption)
                        if let script = s.activeScript {
                            DisclosureGroup("当前启用的脚本", isExpanded: $showScript) {
                                ScrollView { Text(script).font(.system(.caption, design: .monospaced)).textSelection(.enabled) }
                                    .frame(height: 120)
                            }
                        }
                    } else {
                        Text("这个邮箱不支持 ManageSieve" + (s.error.map { "（\($0)）" } ?? "")).font(.caption).foregroundStyle(.secondary)
                    }
                }
                if let message { Text(message).font(.caption) }
            }
        }
        .onAppear { if accountId.isEmpty { accountId = state.accounts.first { $0.kind == .imap }?.id ?? "" } }
    }

    private func check() async {
        guard !accountId.isEmpty else { return }
        busy = true
        defer { busy = false }
        do {
            status = try await state.api.sieveStatus(accountId)
            message = nil
        } catch {
            message = error.localizedDescription
        }
    }

    private func upload() async {
        busy = true
        defer { busy = false }
        do {
            _ = try await state.api.sieveUpload(accountId)
            message = "已上传并启用"
            status = try? await state.api.sieveStatus(accountId)
        } catch {
            message = error.localizedDescription
        }
    }

    private func remove() async {
        busy = true
        defer { busy = false }
        do {
            try await state.api.sieveRemove(accountId)
            message = "已从服务器移除"
            status = try? await state.api.sieveStatus(accountId)
        } catch {
            message = error.localizedDescription
        }
    }
}

struct RuleEditor: View {
    @EnvironmentObject private var state: AppState
    @State var rule: Rule
    let onDone: (Rule?) -> Void

    static let fields = ["from", "to", "cc", "recipients", "subject", "body", "has_attachment", "list_id", "header:X-Mailer"]
    static let ops = ["contains", "not_contains", "equals", "starts_with", "ends_with", "regex", "exists"]
    static let actions = ["move", "archive", "trash", "mark_read", "flag", "label", "category", "forward", "vip", "no_notify", "notify", "stop"]

    static func fieldName(_ f: String) -> String {
        switch f {
        case "from": return "发件人"
        case "to": return "收件人"
        case "cc": return "抄送"
        case "recipients": return "收件人或抄送"
        case "subject": return "主题"
        case "body": return "正文"
        case "has_attachment": return "有附件"
        case "list_id": return "邮件列表"
        default: return f.hasPrefix("header:") ? "信头 \(f.dropFirst(7))" : f
        }
    }

    static func opName(_ o: String) -> String {
        ["contains": "包含", "not_contains": "不包含", "equals": "等于", "starts_with": "开头是", "ends_with": "结尾是", "regex": "匹配正则", "exists": "存在"][o] ?? o
    }

    static func actionName(_ a: String) -> String {
        ["move": "移动到", "archive": "归档", "trash": "删除", "mark_read": "标为已读", "flag": "加星标", "label": "加标签", "category": "设为分类",
         "forward": "转发给", "vip": "设为 VIP", "no_notify": "不提醒", "notify": "总是提醒", "stop": "停止后续规则"][a] ?? a
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            TextField("规则名称", text: $rule.name).textFieldStyle(.roundedBorder)
            Picker("账户", selection: Binding(get: { rule.accountId ?? "" }, set: { rule.accountId = $0.isEmpty ? nil : $0 })) {
                Text("所有账户").tag("")
                ForEach(state.accounts) { Text($0.displayLabel).tag($0.id) }
            }
            Picker("满足", selection: $rule.matchAll) {
                Text("全部条件").tag(true)
                Text("任一条件").tag(false)
            }
            .pickerStyle(.segmented).frame(width: 240)
            Text("条件").font(.headline)
            ForEach($rule.conditions) { $c in
                HStack {
                    Picker("", selection: $c.field) { ForEach(Self.fields, id: \.self) { Text(Self.fieldName($0)).tag($0) } }.frame(width: 140)
                    Picker("", selection: $c.op) { ForEach(Self.ops, id: \.self) { Text(Self.opName($0)).tag($0) } }.frame(width: 110)
                    TextField("值", text: $c.value)
                    Button { rule.conditions.removeAll { $0.id == c.id } } label: { Image(systemName: "minus.circle") }.buttonStyle(.borderless)
                }
                .labelsHidden()
            }
            Button("添加条件") { rule.conditions.append(RuleCondition()) }
            Text("动作").font(.headline)
            ForEach($rule.actions) { $a in
                HStack {
                    Picker("", selection: $a.kind) { ForEach(Self.actions, id: \.self) { Text(Self.actionName($0)).tag($0) } }.frame(width: 140)
                    if ["move", "label", "forward"].contains(a.kind) {
                        TextField(a.kind == "move" ? "文件夹名称" : a.kind == "forward" ? "转发地址" : "标签", text: $a.value)
                    } else if a.kind == "category" {
                        Picker("", selection: $a.value) { ForEach(Category.allCases) { Text($0.title).tag($0.rawValue) } }
                    }
                    Spacer()
                    Button { rule.actions.removeAll { $0.id == a.id } } label: { Image(systemName: "minus.circle") }.buttonStyle(.borderless)
                }
                .labelsHidden()
            }
            Button("添加动作") { rule.actions.append(RuleAction(kind: "mark_read")) }
            HStack {
                Spacer()
                Button("取消") { onDone(nil) }.keyboardShortcut(.cancelAction)
                Button("保存") { onDone(rule) }.keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
            }
        }
        .padding(20)
        .frame(width: 620)
    }
}

struct ComposeSettings: View {
    @EnvironmentObject private var state: AppState
    @State private var templates: [Template] = []
    @State private var editing: Template?

    var body: some View {
        Form {
            Section("发送") {
                Picker("撤销发送时间", selection: $state.settings.undoSendSecs) {
                    Text("关闭").tag(0)
                    Text("5 秒").tag(5)
                    Text("10 秒").tag(10)
                    Text("20 秒").tag(20)
                    Text("30 秒").tag(30)
                }
                Picker("回复时光标位置", selection: $state.settings.replyPosition) {
                    Text("引用上方").tag("top")
                    Text("引用下方").tag("bottom")
                }
                TextField("自动密送给（逗号分隔）", text: $state.settings.autoBcc)
            }
            Section("模板与快捷短语") {
                ForEach(templates) { t in
                    HStack {
                        Text(t.name)
                        Spacer()
                        Button("编辑") { editing = t }
                        Button(role: .destructive) {
                            Task {
                                try? await state.api.deleteTemplate(t.id)
                                await load()
                            }
                        } label: { Image(systemName: "trash") }
                    }
                }
                Button("新建模板") { editing = Template(name: "新模板") }
            }
            Section {
                Text("签名在 设置 → 账户 → 发件身份与签名 中按身份设置。").font(.caption).foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .task { await load() }
        .onChange(of: state.settings) { _, _ in persist(state) }
        .sheet(item: $editing) { t in
            TemplateEditor(template: t) { saved in
                editing = nil
                if let saved {
                    Task {
                        _ = try? await state.api.saveTemplate(saved)
                        await load()
                    }
                }
            }
        }
    }

    private func load() async {
        templates = (try? await state.api.templates()) ?? []
        state.templates = templates
    }
}

struct TemplateEditor: View {
    @State var template: Template
    let onDone: (Template?) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            TextField("名称", text: $template.name).textFieldStyle(.roundedBorder)
            TextField("主题（可选）", text: $template.subject).textFieldStyle(.roundedBorder)
            Text("内容（HTML）").font(.caption).foregroundStyle(.secondary)
            TextEditor(text: $template.html).font(.system(.body, design: .monospaced)).frame(height: 200)
            HStack {
                Spacer()
                Button("取消") { onDone(nil) }.keyboardShortcut(.cancelAction)
                Button("保存") { onDone(template) }.keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
            }
        }
        .padding(20)
        .frame(width: 520)
    }
}

struct ForwarderSettings: View {
    @EnvironmentObject private var state: AppState
    @State private var items: [Forwarder] = []
    @State private var editing: Forwarder?

    static let kinds: [(String, String)] = [
        ("bark", "Bark（iPhone 推送）"), ("serverchan", "Server酱（推送到微信）"), ("pushplus", "PushPlus（微信）"),
        ("wecom", "企业微信群机器人"), ("feishu", "飞书机器人"), ("dingtalk", "钉钉机器人"), ("telegram", "Telegram Bot"),
        ("ntfy", "ntfy"), ("webhook", "自定义 Webhook"),
    ]

    var body: some View {
        VStack(alignment: .leading) {
            Text("新邮件提醒的同时推送到手机或聊天工具（只发送发件人、主题和摘要）。").font(.callout).foregroundStyle(.secondary).padding([.top, .horizontal])
            List {
                ForEach(items) { f in
                    HStack {
                        Toggle("", isOn: Binding(get: { f.enabled }, set: { v in
                            var nf = f
                            nf.enabled = v
                            Task { await save(nf) }
                        }))
                        .labelsHidden()
                        VStack(alignment: .leading) {
                            Text(f.name.isEmpty ? Self.kindName(f.kind) : f.name)
                            Text(Self.kindName(f.kind)).font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("测试") {
                            Task {
                                do {
                                    try await state.api.testForwarder(f)
                                    state.show(Toast(text: "测试推送已发送"))
                                } catch {
                                    state.show(Toast(text: error.localizedDescription, isError: true))
                                }
                            }
                        }
                        Button("编辑") { editing = f }
                        Button(role: .destructive) {
                            Task {
                                try? await state.api.deleteForwarder(f.id)
                                await load()
                            }
                        } label: { Image(systemName: "trash") }
                    }
                }
            }
            HStack {
                Menu("添加推送") {
                    ForEach(Self.kinds, id: \.0) { k in
                        Button(k.1) {
                            var f = Forwarder()
                            f.kind = k.0
                            f.name = k.1
                            editing = f
                        }
                    }
                }
                .fixedSize()
                Spacer()
            }
            .padding()
        }
        .task { await load() }
        .sheet(item: $editing) { f in
            ForwarderEditor(forwarder: f) { saved in
                editing = nil
                if let saved { Task { await save(saved) } }
            }
            .environmentObject(state)
        }
    }

    static func kindName(_ k: String) -> String { kinds.first { $0.0 == k }?.1 ?? k }

    private func load() async { items = (try? await state.api.forwarders()) ?? [] }

    private func save(_ f: Forwarder) async {
        _ = try? await state.api.saveForwarder(f)
        await load()
    }
}

struct ForwarderEditor: View {
    @EnvironmentObject private var state: AppState
    @State var forwarder: Forwarder
    let onDone: (Forwarder?) -> Void

    private var fields: [(String, String)] {
        switch forwarder.kind {
        case "bark": return [("key", "Device Key"), ("server", "服务器（默认 https://api.day.app）"), ("sound", "铃声（可选）")]
        case "serverchan": return [("sendkey", "SendKey")]
        case "pushplus": return [("token", "Token"), ("topic", "群组编码（可选）")]
        case "wecom": return [("url", "Webhook 地址")]
        case "feishu": return [("url", "Webhook 地址"), ("secret", "签名密钥（可选）"), ("keyword", "关键词（可选）")]
        case "dingtalk": return [("url", "Webhook 地址"), ("secret", "加签密钥（可选）"), ("keyword", "关键词（可选）")]
        case "telegram": return [("token", "Bot Token"), ("chat_id", "Chat ID"), ("api", "API 地址（可选）")]
        case "ntfy": return [("topic", "Topic"), ("server", "服务器（默认 https://ntfy.sh）"), ("token", "Access Token（可选）")]
        default: return [("url", "URL"), ("method", "方法（POST / GET）")]
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(ForwarderSettings.kindName(forwarder.kind)).font(.headline)
            TextField("名称", text: $forwarder.name).textFieldStyle(.roundedBorder)
            ForEach(fields, id: \.0) { f in
                TextField(f.1, text: Binding(get: { forwarder.config[f.0] ?? "" }, set: { forwarder.config[f.0] = $0 }))
                    .textFieldStyle(.roundedBorder)
            }
            Toggle("仅推送 VIP 来信", isOn: $forwarder.vipOnly)
            Text("推送的账户").font(.caption).foregroundStyle(.secondary)
            ForEach(state.accounts) { acc in
                Toggle(acc.displayLabel, isOn: Binding(get: { forwarder.accounts.isEmpty || forwarder.accounts.contains(acc.id) }, set: { on in
                    var list = forwarder.accounts.isEmpty ? state.accounts.map(\.id) : forwarder.accounts
                    if on { list.append(acc.id) } else { list.removeAll { $0 == acc.id } }
                    forwarder.accounts = Set(list).count == state.accounts.count ? [] : Array(Set(list))
                }))
                .toggleStyle(.checkbox)
            }
            HStack {
                Spacer()
                Button("取消") { onDone(nil) }.keyboardShortcut(.cancelAction)
                Button("保存") { onDone(forwarder) }.keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
            }
        }
        .padding(20)
        .frame(width: 480)
    }
}

struct PrivacySettings: View {
    @EnvironmentObject private var state: AppState
    @State private var blocked: [String] = []
    @State private var vips: [String] = []
    @State private var newBlock = ""

    var body: some View {
        Form {
            Section("远程内容") {
                Picker("加载远程图片", selection: $state.settings.loadRemoteImages) {
                    Text("从不（推荐）").tag("never")
                    Text("仅联系人").tag("contacts")
                    Text("总是").tag("always")
                }
                Toggle("拦截邮件跟踪像素", isOn: $state.settings.blockTrackers)
            }
            Section("应用") {
                Toggle("启动时需要 Touch ID / 密码解锁", isOn: $state.settings.appLock)
                Toggle("允许 Spotlight 搜索邮件", isOn: $state.settings.spotlightIndex)
                    .onChange(of: state.settings.spotlightIndex) { _, on in if !on { SpotlightIndexer.clear() } }
            }
            Section("屏蔽的发件人") {
                ForEach(blocked, id: \.self) { b in
                    HStack {
                        Text(b)
                        Spacer()
                        Button("移除") { Task { try? await state.api.unblock(b); await load() } }
                    }
                }
                HStack {
                    TextField("地址或 @域名", text: $newBlock)
                    Button("屏蔽") {
                        Task {
                            try? await state.api.block(newBlock)
                            newBlock = ""
                            await load()
                        }
                    }
                    .disabled(newBlock.isEmpty)
                }
            }
            Section("VIP 发件人") {
                ForEach(vips, id: \.self) { v in
                    HStack {
                        Text(v)
                        Spacer()
                        Button("移除") { Task { try? await state.api.setVip(v, false); await load() } }
                    }
                }
                if vips.isEmpty { Text("在邮件上右键 → 设为 VIP 发件人").foregroundStyle(.secondary).font(.caption) }
            }
        }
        .formStyle(.grouped)
        .task { await load() }
        .onChange(of: state.settings) { _, _ in persist(state) }
    }

    private func load() async {
        blocked = (try? await state.api.blocked()) ?? []
        vips = (try? await state.api.vips()) ?? []
    }
}

struct AdvancedSettings: View {
    @EnvironmentObject private var state: AppState
    @State private var importFolder: Int64?
    @State private var message: String?
    @State private var storage: APIClient.StorageReport?
    @State private var cleaning = false

    var body: some View {
        Form {
            Section("OAuth 客户端（Gmail / Outlook 浏览器登录）") {
                TextField("Google 客户端 ID", text: $state.settings.oauth.googleClientId)
                SecureField("Google 客户端密钥", text: $state.settings.oauth.googleClientSecret)
                TextField("Microsoft 应用程序（客户端）ID", text: $state.settings.oauth.microsoftClientId)
                Text("按照 docs/guides 中的教程注册。Microsoft 重定向 URI：http://localhost/oauth/callback；Google 选择“桌面应用”类型。")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("导入 / 导出") {
                Picker("导入到文件夹", selection: $importFolder) {
                    Text("选择…").tag(Int64?.none)
                    ForEach(state.accounts) { acc in
                        ForEach(state.folders[acc.id] ?? []) { f in
                            Text("\(acc.displayLabel) / \(f.role == .custom ? f.name : f.role.title)").tag(Int64?.some(f.id))
                        }
                    }
                }
                HStack {
                    Button("导入 .mbox / .eml…") { importFiles() }.disabled(importFolder == nil)
                    Button("导出此文件夹为 .mbox…") { exportFolder() }.disabled(importFolder == nil)
                }
                if let message { Text(message).font(.caption).foregroundStyle(.secondary) }
            }
            Section("存储") {
                if let s = storage {
                    LabeledContent("本地邮件", value: "\(s.messageCount) 封")
                    LabeledContent("邮件内容", value: Fmt.size(s.messagesBytes))
                    LabeledContent("索引数据库", value: Fmt.size(s.databaseBytes))
                    LabeledContent("附件缓存", value: Fmt.size(s.cacheBytes))
                }
                HStack {
                    Button("清理缓存") {
                        cleaning = true
                        Task {
                            let freed = (try? await state.api.cleanup()) ?? 0
                            storage = try? await state.api.storage()
                            message = "已释放 \(Fmt.size(freed))"
                            cleaning = false
                        }
                    }
                    .disabled(cleaning)
                    Text("超出同步范围的旧邮件只保留在服务器上，需要时可用服务器搜索找回。").font(.caption).foregroundStyle(.secondary)
                }
            }
            Section("数据") {
                LabeledContent("数据位置", value: Daemon.dataDir.path)
                HStack {
                    Button("在访达中显示") { NSWorkspace.shared.activateFileViewerSelecting([Daemon.dataDir]) }
                    Button("打开日志") { NSWorkspace.shared.open(Daemon.dataDir.appendingPathComponent("logs/mailcore.log")) }
                }
                Button("添加演示数据") { Task { try? await state.api.seedDemo(); await state.reloadAll() } }
            }
        }
        .formStyle(.grouped)
        .onChange(of: state.settings) { _, _ in persist(state) }
        .task { storage = try? await state.api.storage() }
    }

    private func importFiles() {
        guard let folder = importFolder else { return }
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task {
            do {
                let n = try await state.api.importMessages(folder: folder, path: url.path)
                message = "已导入 \(n) 封邮件"
            } catch {
                message = error.localizedDescription
            }
        }
    }

    private func exportFolder() {
        guard let folder = importFolder else { return }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "Mailbox-导出.mbox"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task {
            do {
                let n = try await state.api.exportMessages(ids: nil, folder: folder, path: url.path, format: "mbox")
                message = "已导出 \(n) 封邮件"
            } catch {
                message = error.localizedDescription
            }
        }
    }
}

struct OnboardingView: View {
    @EnvironmentObject private var state: AppState
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 18) {
            Image(systemName: "envelope.open.fill").font(.system(size: 56)).foregroundStyle(.blue.gradient)
            Text("欢迎使用 Mailbox").font(.largeTitle.weight(.semibold))
            Text("一个轻量的聚合邮箱：Gmail、Outlook、QQ、163、126、iCloud、ProtonMail… 都在一个收件箱里。")
                .multilineTextAlignment(.center).foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 8) {
                Label("后台轻服务实时收信，通知注明来自哪个邮箱", systemImage: "bell.badge")
                Label("本地全文搜索，支持中文和 Gmail 风格语法", systemImage: "magnifyingglass")
                Label("撤销发送、定时发送、稍后提醒、规则、模板", systemImage: "clock.arrow.circlepath")
                Label("隐私优先：默认拦截远程图片和跟踪像素", systemImage: "hand.raised")
            }
            .padding()
            HStack {
                Button("先看看演示数据") {
                    Task {
                        try? await state.api.seedDemo()
                        await state.reloadAll()
                        dismiss()
                    }
                }
                Button("添加邮箱账户") {
                    dismiss()
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { state.showAddAccount = true }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(36)
        .frame(width: 560)
    }
}
