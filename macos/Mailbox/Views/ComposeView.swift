import AppKit
import SwiftUI
import UniformTypeIdentifiers

@MainActor
final class ComposeModel: ObservableObject {
    @Published var draft = Draft()
    @Published var to: [EmailAddress] = [] { didSet { refreshCrypto() } }
    @Published var cc: [EmailAddress] = [] { didSet { refreshCrypto() } }
    @Published var bcc: [EmailAddress] = [] { didSet { refreshCrypto() } }
    @Published var showCc = false
    @Published var showBcc = false
    @Published var loaded = false
    @Published var busy = false
    @Published var error: String?
    @Published var markdownMode = false
    @Published var markdownText = ""
    @Published var savedAt: Date?
    @Published var showSchedule = false
    @Published var scheduleDate = Date().addingTimeInterval(3600)
    /// PGP / S/MIME keys known to the engine, and recipients that have none.
    @Published var keys: [KeyInfo] = []
    @Published var missingKeys: [String] = []
    private var encryptTouched = false
    private var autoEncrypt = false
    private var cryptoTask: Task<Void, Never>?

    let editor = RichEditorController()
    private var dirty = false
    private var autosave: Task<Void, Never>?
    private var finished = false
    private var everSaved = false

    var api: APIClient { .shared }

    func load(_ req: ComposeRequest, state: AppState) async {
        guard !loaded else { return }
        do {
            if let d = req.draft {
                draft = d
                everSaved = true
            } else if let r = req.replyTo {
                draft = try await api.replyDraft(r, mode: req.mode)
                if let body = req.body, !body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    // Prefilled reply (AI suggested reply): the text goes above the signature and quote.
                    draft.html = AIText.html(body) + draft.html
                    dirty = true
                }
            } else if let e = req.editServerDraft {
                draft = try await api.editDraft(e)
                everSaved = true
            } else {
                let accountId = req.accountId ?? state.accounts.first(where: { $0.enabled && $0.kind != .web })?.id ?? state.accounts.first?.id ?? ""
                let identity = state.identities.first { $0.accountId == accountId && $0.isDefault } ?? state.identities.first { $0.accountId == accountId }
                draft.accountId = accountId
                draft.identityId = identity?.id
                draft.subject = req.subject ?? ""
                let body = req.body.map { "<div>" + Self.escape($0).replacingOccurrences(of: "\n", with: "<br>") + "</div>" } ?? "<div><br></div>"
                let sig = identity?.signatureHtml ?? ""
                draft.html = body + "<div><br></div><div id=\"mailbox-signature\">\(sig)</div>"
                draft.to = req.to.map(Self.parseAddress)
                draft.cc = req.cc.map(Self.parseAddress)
                draft.bcc = req.bcc.map(Self.parseAddress)
                draft.attachments = req.attachments.map { path in
                    let url = URL(fileURLWithPath: path)
                    let size = (try? FileManager.default.attributesOfItem(atPath: path)[.size] as? NSNumber)?.int64Value
                    return DraftAttachment(path: path, filename: url.lastPathComponent, size: size)
                }
            }
        } catch {
            self.error = error.localizedDescription
        }
        to = draft.to
        cc = draft.cc
        bcc = draft.bcc
        showCc = !cc.isEmpty
        showBcc = !bcc.isEmpty
        if let md = draft.markdown, !md.isEmpty {
            markdownMode = true
            markdownText = md
        }
        editor.setHTML(draft.html)
        editor.onChange = { [weak self] in self?.markDirty() }
        loaded = true
        await loadCrypto(state: state, isNew: req.draft == nil && req.editServerDraft == nil)
    }

    // MARK: - Signing / encryption

    var cryptoAvailable: Bool { keys.contains { $0.hasSecret } }

    private func loadCrypto(state: AppState, isNew: Bool) async {
        keys = (try? await api.keys()) ?? []
        guard cryptoAvailable else { return }
        autoEncrypt = state.settings.cryptoAutoEncrypt
        let sender = (state.identities.first { $0.id == draft.identityId }?.email
            ?? state.accounts.first { $0.id == draft.accountId }?.email ?? "").lowercased()
        let ownKey = keys.contains { $0.hasSecret && $0.emails.contains(sender) }
        if isNew && ownKey && state.settings.cryptoSignDefault { draft.sign = true }
        refreshCrypto()
    }

    /// Re-checks recipient keys; with auto-encrypt on, the lock follows whether everyone has one
    /// until the user flips it by hand.
    func refreshCrypto() {
        guard loaded, cryptoAvailable else { return }
        let all = (to + cc + bcc).map(\.email)
        cryptoTask?.cancel()
        cryptoTask = Task {
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard !Task.isCancelled else { return }
            let r = (try? await api.checkKeys(all)) ?? (have: [], missing: all)
            guard !Task.isCancelled else { return }
            missingKeys = r.missing
            if autoEncrypt && !encryptTouched {
                draft.encrypt = !all.isEmpty && r.missing.isEmpty
            }
        }
    }

    func toggleEncrypt() {
        encryptTouched = true
        draft.encrypt.toggle()
    }

    nonisolated static func escape(_ s: String) -> String {
        s.replacingOccurrences(of: "&", with: "&amp;").replacingOccurrences(of: "<", with: "&lt;").replacingOccurrences(of: ">", with: "&gt;")
    }

    nonisolated static func parseAddress(_ s: String) -> EmailAddress {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        if let lt = t.lastIndex(of: "<"), let gt = t.lastIndex(of: ">"), lt < gt {
            let name = t[..<lt].trimmingCharacters(in: CharacterSet(charactersIn: " \""))
            let email = String(t[t.index(after: lt)..<gt])
            return EmailAddress(name: name.isEmpty ? nil : name, email: email)
        }
        return EmailAddress(name: nil, email: t)
    }

    func markDirty() {
        dirty = true
        autosave?.cancel()
        autosave = Task {
            try? await Task.sleep(nanoseconds: 4_000_000_000)
            if !Task.isCancelled { await save(sync: false) }
        }
    }

    private func collect() async {
        draft.to = to
        draft.cc = cc
        draft.bcc = bcc
        if markdownMode {
            draft.markdown = markdownText
        } else {
            draft.markdown = nil
            draft.html = await editor.html()
        }
    }

    func save(sync: Bool) async {
        guard loaded, !finished, !draft.accountId.isEmpty else { return }
        await collect()
        do {
            draft = try await api.saveDraft(draft, sync: sync)
            everSaved = true
            dirty = false
            savedAt = Date()
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Returns true when the window may close.
    func send(at date: Date?, state: AppState) async -> Bool {
        await collect()
        guard !draft.to.isEmpty || !draft.cc.isEmpty || !draft.bcc.isEmpty else {
            error = "请至少填写一个收件人"
            return false
        }
        if draft.subject.trimmingCharacters(in: .whitespaces).isEmpty {
            let alert = NSAlert()
            alert.messageText = "邮件没有主题，仍要发送吗？"
            alert.addButton(withTitle: "发送")
            alert.addButton(withTitle: "取消")
            if alert.runModal() != .alertFirstButtonReturn { return false }
        }
        let mentionsAttachment = ["附件", "attached", "attachment", "附上", "见附"].contains { (draft.markdown ?? draft.html).localizedCaseInsensitiveContains($0) }
        if mentionsAttachment && draft.attachments.isEmpty && draft.mode != .forward {
            let alert = NSAlert()
            alert.messageText = "正文提到了附件，但没有添加附件"
            alert.informativeText = "仍要发送吗？"
            alert.addButton(withTitle: "仍然发送")
            alert.addButton(withTitle: "返回添加")
            if alert.runModal() != .alertFirstButtonReturn { return false }
        }
        busy = true
        defer { busy = false }
        do {
            let item = try await api.sendDraft(draft, at: date.map { Int64($0.timeIntervalSince1970) })
            finished = true
            autosave?.cancel()
            state.didQueueSend(item, scheduled: date != nil)
            return true
        } catch {
            self.error = error.localizedDescription
            return false
        }
    }

    func discard() async {
        finished = true
        autosave?.cancel()
        if everSaved, !draft.id.isEmpty {
            try? await api.deleteDraft(draft.id)
        }
    }

    /// Window closing without sending: keep the draft (and upload it to the server).
    func closeWithoutSending() async {
        guard !finished else { return }
        if dirty || !everSaved {
            let html = markdownMode ? markdownText : await editor.html()
            let empty = to.isEmpty && cc.isEmpty && draft.subject.isEmpty && Self.plain(html).count < 3 && draft.attachments.isEmpty
            if empty && !everSaved { return }
        }
        await save(sync: true)
        finished = true
    }

    nonisolated static func plain(_ html: String) -> String {
        html.replacingOccurrences(of: "<[^>]+>", with: "", options: .regularExpression)
            .replacingOccurrences(of: "&nbsp;", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    func attach(_ urls: [URL]) {
        for url in urls where !draft.attachments.contains(where: { $0.path == url.path }) {
            let size = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? NSNumber)?.int64Value
            draft.attachments.append(DraftAttachment(path: url.path, filename: url.lastPathComponent, size: size))
        }
        markDirty()
    }

    func changeIdentity(_ identity: Identity, state: AppState) {
        draft.identityId = identity.id
        draft.accountId = identity.accountId
        editor.replaceSignature(identity.signatureHtml)
        markDirty()
    }

    func apply(template t: Template) {
        if draft.subject.isEmpty && !t.subject.isEmpty { draft.subject = t.subject }
        if markdownMode {
            markdownText += t.html
        } else {
            editor.insertHTML(t.html)
        }
        markDirty()
    }

    func toggleMarkdown() async {
        if markdownMode {
            markdownMode = false
            draft.markdown = nil
            let html = markdownText
                .components(separatedBy: "\n")
                .map { $0.isEmpty ? "<div><br></div>" : "<div>\(Self.escape($0))</div>" }
                .joined()
            editor.setHTML(html)
        } else {
            let html = await editor.html()
            markdownText = Self.plain(html.replacingOccurrences(of: "<br>", with: "\n").replacingOccurrences(of: "</div>", with: "\n"))
            markdownMode = true
        }
    }
}

struct ComposeWindow: View {
    @EnvironmentObject private var state: AppState
    @Environment(\.dismiss) private var dismiss
    let request: ComposeRequest
    @StateObject private var model = ComposeModel()
    @StateObject private var ai = ComposeAIModel()
    @State private var dropTargeted = false

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if showsQuickReply {
                AIQuickReplyBar(ai: ai, compose: model)
                Divider()
            }
            HStack(spacing: 0) {
                VStack(spacing: 0) {
                    if model.markdownMode {
                        TextEditor(text: $model.markdownText)
                            .font(.system(.body, design: .monospaced))
                            .padding(8)
                            .onChange(of: model.markdownText) { _, _ in model.markDirty() }
                    } else {
                        FormatBar(editor: model.editor)
                        Divider()
                        RichEditor(controller: model.editor)
                    }
                }
                if ai.visible {
                    Divider()
                    AIComposePanel(ai: ai, stream: ai.stream, compose: model)
                        .frame(width: 320)
                        .transition(.move(edge: .trailing))
                }
            }
            if !model.draft.attachments.isEmpty {
                Divider()
                attachmentsBar
            }
            Divider()
            footer
        }
        .frame(minWidth: 620, minHeight: 480)
        .navigationTitle(model.draft.subject.isEmpty ? "新邮件" : model.draft.subject)
        .task { await model.load(request, state: state) }
        .onDrop(of: [.fileURL], isTargeted: $dropTargeted) { providers in
            for p in providers {
                _ = p.loadObject(ofClass: URL.self) { url, _ in
                    if let url { Task { @MainActor in model.attach([url]) } }
                }
            }
            return true
        }
        .overlay {
            if dropTargeted {
                RoundedRectangle(cornerRadius: 12).strokeBorder(Color.accentColor, style: StrokeStyle(lineWidth: 3, dash: [8]))
                    .padding(6)
                    .allowsHitTesting(false)
            }
        }
        .onDisappear {
            ai.stream.cancel()
            Task { await model.closeWithoutSending() }
        }
        .alert("无法发送", isPresented: Binding(get: { model.error != nil }, set: { if !$0 { model.error = nil } })) {
            Button("好") {}
        } message: {
            Text(model.error ?? "")
        }
        .sheet(isPresented: $model.showSchedule) { scheduleSheet }
    }

    private var showsQuickReply: Bool {
        model.loaded && model.draft.sourceMessageId != nil && (model.draft.mode == .reply || model.draft.mode == .replyAll)
    }

    private var identities: [Identity] {
        let usable = Set(state.accounts.filter { $0.kind != .web }.map(\.id))
        return state.identities.filter { usable.contains($0.accountId) }
    }

    private var header: some View {
        VStack(spacing: 0) {
            HStack {
                Text("发件人").foregroundStyle(.secondary).frame(width: 56, alignment: .leading)
                Picker("", selection: Binding(get: { model.draft.identityId ?? "" }, set: { id in
                    if let i = identities.first(where: { $0.id == id }) { model.changeIdentity(i, state: state) }
                })) {
                    ForEach(identities) { i in
                        let label = state.accountsById[i.accountId]?.displayLabel ?? ""
                        Text("\(i.name.isEmpty ? i.email : i.name) <\(i.email)> · \(label)").tag(i.id)
                    }
                }
                .labelsHidden()
                Spacer()
                Button(model.showCc ? "隐藏抄送" : "抄送") { model.showCc.toggle() }.buttonStyle(.link)
                Button(model.showBcc ? "隐藏密送" : "密送") { model.showBcc.toggle() }.buttonStyle(.link)
            }
            .padding(.horizontal, 12).padding(.vertical, 6)
            Divider()
            RecipientField(label: "收件人", addresses: $model.to, onChange: model.markDirty)
            if model.showCc {
                Divider()
                RecipientField(label: "抄送", addresses: $model.cc, onChange: model.markDirty)
            }
            if model.showBcc {
                Divider()
                RecipientField(label: "密送", addresses: $model.bcc, onChange: model.markDirty)
            }
            Divider()
            HStack {
                Text("主题").foregroundStyle(.secondary).frame(width: 56, alignment: .leading)
                TextField("", text: $model.draft.subject)
                    .textFieldStyle(.plain)
                    .onChange(of: model.draft.subject) { _, _ in model.markDirty() }
                if model.draft.priorityHigh {
                    Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.red).help("重要")
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 8)
        }
    }

    private var attachmentsBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(model.draft.attachments) { a in
                    HStack(spacing: 4) {
                        Image(systemName: "doc")
                        Text(a.filename).lineLimit(1)
                        if let s = a.size { Text(Fmt.size(s)).foregroundStyle(.secondary).font(.caption) }
                        Button {
                            model.draft.attachments.removeAll { $0.id == a.id }
                            model.markDirty()
                        } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.borderless)
                    }
                    .padding(.horizontal, 8).padding(.vertical, 4)
                    .background(.quaternary.opacity(0.6), in: Capsule())
                }
                let total = model.draft.attachments.compactMap(\.size).reduce(0, +)
                if total > 20 * 1024 * 1024 {
                    Label("附件共 \(Fmt.size(total))，部分邮箱可能拒收", systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.orange).font(.caption)
                }
            }
            .padding(8)
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            Button { pickFiles() } label: { Label("附件", systemImage: "paperclip") }
            Menu {
                if state.templates.isEmpty {
                    Text("还没有模板（设置 → 模板）")
                }
                ForEach(state.templates) { t in
                    Button(t.name) { model.apply(template: t) }
                }
            } label: { Label("模板", systemImage: "text.badge.plus") }
            .menuStyle(.borderlessButton).fixedSize()
            AIComposeMenu(ai: ai, compose: model)
            Menu {
                Toggle("Markdown 模式", isOn: Binding(get: { model.markdownMode }, set: { _ in Task { await model.toggleMarkdown() } }))
                Toggle("纯文本", isOn: $model.draft.plainTextOnly)
                Toggle("请求已读回执", isOn: $model.draft.requestReadReceipt)
                Toggle("标为重要", isOn: $model.draft.priorityHigh)
                Divider()
                Button("保存草稿") { Task { await model.save(sync: true) } }
                Button("丢弃草稿", role: .destructive) {
                    Task {
                        await model.discard()
                        dismiss()
                    }
                }
            } label: { Image(systemName: "ellipsis.circle") }
            .menuStyle(.borderlessButton).fixedSize()
            if model.cryptoAvailable {
                Toggle(isOn: $model.draft.sign) { Image(systemName: "signature") }
                    .toggleStyle(.button)
                    .help(model.draft.sign ? "将进行数字签名" : "数字签名")
                Button { model.toggleEncrypt() } label: {
                    Image(systemName: model.draft.encrypt ? "lock.fill" : "lock.open")
                        .foregroundStyle(model.draft.encrypt ? (model.missingKeys.isEmpty ? Color.green : Color.red) : Color.secondary)
                }
                .buttonStyle(.borderless)
                .help(model.draft.encrypt ? "将加密发送" : "加密")
                if model.draft.encrypt && !model.missingKeys.isEmpty {
                    Text("缺少公钥：\(model.missingKeys.joined(separator: "、"))")
                        .font(.caption).foregroundStyle(.red).lineLimit(1).truncationMode(.tail)
                }
            }
            if let saved = model.savedAt {
                Text("已保存 \(Fmt.timeOnly.string(from: saved))").font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            if model.busy { ProgressView().controlSize(.small) }
            Menu {
                Button("1 小时后") { schedule(Date().addingTimeInterval(3600)) }
                Button("今晚 20:00") { schedule(Calendar.current.date(bySettingHour: 20, minute: 0, second: 0, of: Date()) ?? Date()) }
                Button("明天早上 8:00") {
                    let d = Calendar.current.date(byAdding: .day, value: 1, to: Date()) ?? Date()
                    schedule(Calendar.current.date(bySettingHour: 8, minute: 0, second: 0, of: d) ?? d)
                }
                Button("自定义时间…") { model.showSchedule = true }
            } label: {
                Label("发送", systemImage: "paperplane.fill")
            } primaryAction: {
                Task { if await model.send(at: nil, state: state) { dismiss() } }
            }
            .menuStyle(.button)
            .fixedSize()
            .keyboardShortcut(.return, modifiers: .command)
            .disabled(model.busy || !model.loaded)
        }
        .padding(10)
    }

    private var scheduleSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("定时发送").font(.headline)
            DatePicker("发送时间", selection: $model.scheduleDate, in: Date()...)
            HStack {
                Button("取消") { model.showSchedule = false }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("定时发送") {
                    model.showSchedule = false
                    schedule(model.scheduleDate)
                }
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 360)
    }

    private func schedule(_ date: Date) {
        Task { if await model.send(at: date, state: state) { dismiss() } }
    }

    private func pickFiles() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        if panel.runModal() == .OK { model.attach(panel.urls) }
    }
}

/// Token field with contact autocomplete.
struct RecipientField: View {
    let label: String
    @Binding var addresses: [EmailAddress]
    var onChange: () -> Void = {}
    @State private var text = ""
    @State private var suggestions: [Contact] = []
    @State private var highlighted = 0
    @State private var searchTask: Task<Void, Never>?
    @FocusState private var focused: Bool

    var body: some View {
        HStack(alignment: .top) {
            Text(label).foregroundStyle(.secondary).frame(width: 56, alignment: .leading).padding(.top, 3)
            FlowLayout(spacing: 4) {
                ForEach(addresses) { a in
                    HStack(spacing: 3) {
                        Text(a.display).lineLimit(1)
                        Button {
                            addresses.removeAll { $0.id == a.id }
                            onChange()
                        } label: { Image(systemName: "xmark").font(.system(size: 8, weight: .bold)) }
                        .buttonStyle(.borderless)
                    }
                    .help(a.full)
                    .padding(.horizontal, 7).padding(.vertical, 2)
                    .background(valid(a) ? Color.accentColor.opacity(0.15) : Color.red.opacity(0.2), in: Capsule())
                }
                TextField("", text: $text)
                    .textFieldStyle(.plain)
                    .frame(minWidth: 160)
                    .focused($focused)
                    .onSubmit { commit() }
                    .onChange(of: text) { _, new in
                        if new.hasSuffix(",") || new.hasSuffix(";") || new.hasSuffix("，") || new.hasSuffix("；") {
                            text = String(new.dropLast())
                            commit()
                        } else {
                            lookup(new)
                        }
                    }
                    .onChange(of: focused) { _, f in if !f { commit() } }
                    .onKeyPress(.downArrow) {
                        guard !suggestions.isEmpty else { return .ignored }
                        highlighted = min(highlighted + 1, suggestions.count - 1)
                        return .handled
                    }
                    .onKeyPress(.upArrow) {
                        guard !suggestions.isEmpty else { return .ignored }
                        highlighted = max(highlighted - 1, 0)
                        return .handled
                    }
                    .onKeyPress(.delete) {
                        if text.isEmpty, !addresses.isEmpty {
                            addresses.removeLast()
                            onChange()
                            return .handled
                        }
                        return .ignored
                    }
                    .popover(isPresented: Binding(get: { !suggestions.isEmpty && focused }, set: { if !$0 { suggestions = [] } }), arrowEdge: .bottom) {
                        VStack(alignment: .leading, spacing: 0) {
                            ForEach(Array(suggestions.enumerated()), id: \.element.id) { idx, c in
                                Button {
                                    add(c.address)
                                } label: {
                                    HStack {
                                        Avatar(address: c.address, size: 22)
                                        VStack(alignment: .leading) {
                                            Text(c.name ?? c.email)
                                            if c.name != nil { Text(c.email).font(.caption).foregroundStyle(.secondary) }
                                        }
                                        Spacer()
                                        if c.vip { Image(systemName: "crown.fill").foregroundStyle(.orange) }
                                    }
                                    .padding(.horizontal, 10).padding(.vertical, 5)
                                    .background(idx == highlighted ? Color.accentColor.opacity(0.2) : .clear)
                                    .contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        .frame(width: 320)
                        .padding(.vertical, 4)
                    }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, 12).padding(.vertical, 6)
    }

    private func valid(_ a: EmailAddress) -> Bool {
        a.email.range(of: #"^[^@\s]+@[^@\s]+\.[^@\s]+$"#, options: .regularExpression) != nil
    }

    private func lookup(_ q: String) {
        searchTask?.cancel()
        let query = q.trimmingCharacters(in: .whitespaces)
        guard query.count >= 1 else {
            suggestions = []
            return
        }
        searchTask = Task {
            try? await Task.sleep(nanoseconds: 150_000_000)
            var results = (try? await APIClient.shared.contacts(query)) ?? []
            let system = await ContactsService.shared.search(query)
            for c in system where !results.contains(where: { $0.email.caseInsensitiveCompare(c.email) == .orderedSame }) {
                results.append(c)
            }
            if !Task.isCancelled {
                suggestions = Array(results.prefix(8))
                highlighted = 0
            }
        }
    }

    private func add(_ a: EmailAddress) {
        if !addresses.contains(where: { $0.id == a.id }) {
            addresses.append(a)
            onChange()
        }
        text = ""
        suggestions = []
    }

    private func commit() {
        if !suggestions.isEmpty && focused, suggestions.indices.contains(highlighted), !text.contains("@") {
            add(suggestions[highlighted].address)
            return
        }
        let parts = text.split(whereSeparator: { ",;，；\n".contains($0) }).map(String.init)
        for p in parts where !p.trimmingCharacters(in: .whitespaces).isEmpty {
            add(ComposeModel.parseAddress(p))
        }
        text = ""
        suggestions = []
    }
}
