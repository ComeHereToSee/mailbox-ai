import SwiftUI

struct MessageListView: View {
    @EnvironmentObject private var state: AppState
    @ObservedObject private var ai = AIState.shared
    @State private var searchTask: Task<Void, Never>?
    @State private var snoozeTarget: [Int64] = []
    @State private var moveTarget: [Int64] = []

    var body: some View {
        List(selection: $state.selection) {
            ForEach(state.messages) { m in
                MessageRow(message: m, account: state.account(for: m), showAccount: showsAccount, density: state.settings.listDensity,
                           showSnippet: state.settings.showSnippets,
                           insight: state.selectedMailbox == AIState.pendingMailbox ? ai.insightForList(m.id) : nil,
                           followUp: state.selectedMailbox == AIState.pendingMailbox && ai.isFollowUp(m.id))
                    .tag(m.id)
                    .contextMenu { contextMenu(for: m) }
                    .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                        Button { Task { await state.act([m.id], .archive) } } label: { Label("归档", systemImage: "archivebox") }
                            .tint(.green)
                        Button(role: .destructive) { Task { await state.act([m.id], .trash) } } label: { Label("删除", systemImage: "trash") }
                    }
                    .swipeActions(edge: .leading, allowsFullSwipe: true) {
                        Button { Task { await state.act([m.id], m.flags.seen ? .unread : .read) } } label: {
                            Label(m.flags.seen ? "未读" : "已读", systemImage: m.flags.seen ? "envelope.badge" : "envelope.open")
                        }
                        .tint(.blue)
                        Button { Task { await state.act([m.id], m.flags.flagged ? .unflag : .flag) } } label: {
                            Label("星标", systemImage: "star")
                        }
                        .tint(.orange)
                    }
                    .draggable(String(m.id))
                    .onAppear {
                        if m.id == state.messages.last?.id { Task { await state.loadMore() } }
                    }
            }
            if state.isSearching {
                HStack {
                    Spacer()
                    if state.searchingServer {
                        ProgressView().controlSize(.small)
                    } else {
                        Button("在服务器上搜索更多…") { Task { await state.searchServer() } }
                            .buttonStyle(.link)
                    }
                    Spacer()
                }
                .padding(.vertical, 6)
            }
        }
        .listStyle(.inset)
        .overlay {
            if state.messages.isEmpty && !state.loadingMessages {
                if state.isSearching {
                    VStack {
                        ContentUnavailableView.search(text: state.searchText)
                        Button("在服务器上搜索") { Task { await state.searchServer() } }
                    }
                } else {
                    ContentUnavailableView("没有邮件", systemImage: "tray", description: Text(emptyHint))
                }
            }
        }
        .searchable(text: $state.searchText, placement: .toolbar, prompt: "搜索（from: has:attachment before: …）")
        .onChange(of: state.searchText) { _, _ in
            searchTask?.cancel()
            searchTask = Task {
                try? await Task.sleep(nanoseconds: 350_000_000)
                if !Task.isCancelled { await state.reloadMessages() }
            }
        }
        .onChange(of: state.listFilter) { _, _ in Task { await state.reloadMessages() } }
        .navigationTitle(title)
        .navigationSubtitle(subtitle)
        .toolbar {
            ToolbarItemGroup {
                Menu {
                    Picker("显示", selection: $state.listFilter) {
                        ForEach(AppState.ListFilter.allCases) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.inline)
                    Divider()
                    Toggle("按会话分组", isOn: Binding(get: { state.settings.conversationView }, set: { v in
                        state.settings.conversationView = v
                        Task {
                            _ = try? await state.api.saveSettings(state.settings)
                            await state.reloadMessages()
                        }
                    }))
                    Button("全部标为已读") { Task { await state.markAllRead() } }
                    if state.isSearching {
                        Button("保存为智能文件夹") { saveSmartFolder() }
                    }
                } label: {
                    Label("筛选", systemImage: state.listFilter == .all ? "line.3.horizontal.decrease.circle" : "line.3.horizontal.decrease.circle.fill")
                }
                if state.selection.count > 1 {
                    Button { ai.batchIds = Array(state.selection) } label: { Label("AI 批量处理…", systemImage: "sparkles") }
                        .help("✨ AI 批量处理所选邮件")
                }
                Button { state.sync() } label: { Label("检查新邮件", systemImage: "arrow.clockwise") }
                Button { state.compose(ComposeRequest(accountId: currentAccountId)) } label: { Label("写邮件", systemImage: "square.and.pencil") }
            }
        }
        .onDeleteCommand { Task { await state.act(Array(state.selection), .trash) } }
        .onKeyPress(characters: .init(charactersIn: "ejk#rafscu!zx"), phases: .down) { press in
            handleKey(press.characters) ? .handled : .ignored
        }
        .sheet(isPresented: Binding(get: { !snoozeTarget.isEmpty }, set: { if !$0 { snoozeTarget = [] } })) {
            SnoozePicker { date in
                let ids = snoozeTarget
                snoozeTarget = []
                if let date { Task { await state.act(ids, .snooze(until: Int64(date.timeIntervalSince1970))) } }
            }
        }
        .sheet(isPresented: Binding(get: { !moveTarget.isEmpty }, set: { if !$0 { moveTarget = [] } })) {
            MoveToFolderSheet(messageIds: moveTarget) { moveTarget = [] }
                .environmentObject(state)
        }
    }

    private var showsAccount: Bool {
        !(state.selectedMailbox.hasPrefix("account:") || state.selectedMailbox.hasPrefix("folder:")) && state.accounts.count > 1
    }

    private var currentAccountId: String? {
        if state.selectedMailbox.hasPrefix("account:") {
            return state.selectedMailbox.split(separator: ":").dropFirst().first.map(String.init)
        }
        if state.selectedMailbox.hasPrefix("folder:"), let id = Int64(state.selectedMailbox.dropFirst(7)) {
            return state.folder(id)?.accountId
        }
        return nil
    }

    private var emptyHint: String {
        if state.selectedMailbox == AIState.pendingMailbox {
            return ai.isConfigured ? "AI 没有发现需要处理的邮件" : "在 设置 → AI 服务 中设置模型后，AI 会自动找出需要回复和有截止时间的邮件"
        }
        return state.accounts.isEmpty ? "添加一个邮箱账户开始使用" : "这里空空如也"
    }

    private var title: String {
        if state.isSearching { return "搜索结果" }
        let m = state.selectedMailbox
        switch m {
        case "unified:inbox": return "全部收件箱"
        case "unread": return "未读"
        case "starred": return "星标"
        case "vip": return "VIP"
        case "snoozed": return "稍后提醒"
        case "pinned": return "置顶"
        case "attachments": return "有附件"
        case "all": return "所有邮件"
        case AIState.pendingMailbox: return "待处理"
        default: break
        }
        if m.hasPrefix("unified:") { return FolderRole(rawValue: String(m.dropFirst(8)))?.title ?? "邮件" }
        if m.hasPrefix("category:") { return Category(rawValue: String(m.dropFirst(9)))?.title ?? "分类" }
        if m.hasPrefix("account:") {
            let id = m.split(separator: ":").dropFirst().first.map(String.init) ?? ""
            return state.accountsById[id]?.displayLabel ?? "收件箱"
        }
        if m.hasPrefix("folder:"), let id = Int64(m.dropFirst(7)), let f = state.folder(id) {
            let acc = state.accountsById[f.accountId]?.displayLabel ?? ""
            return (f.role == .custom ? f.name : f.role.title) + (acc.isEmpty ? "" : " · \(acc)")
        }
        if m.hasPrefix("smart:") {
            return state.smartFolders.first { "smart:\($0.id)" == m }?.name ?? "智能文件夹"
        }
        return "邮件"
    }

    private var subtitle: String {
        let unread = state.messages.filter { !$0.flags.seen }.count
        if state.loadingMessages && state.messages.isEmpty { return "正在加载…" }
        return unread > 0 ? "\(unread) 封未读" : "\(state.messages.count) 封邮件"
    }

    private func saveSmartFolder() {
        let q = state.effectiveQuery ?? ""
        Task {
            _ = try? await state.api.saveSmartFolder(SmartFolder(name: state.searchText, query: q))
            await state.reloadAll()
        }
    }

    private func targets(for m: MessageSummary) -> [Int64] {
        state.selection.contains(m.id) ? Array(state.selection) : [m.id]
    }

    @ViewBuilder
    private func contextMenu(for m: MessageSummary) -> some View {
        let ids = targets(for: m)
        if ids.count > 1 {
            Button("✨ AI 批量处理…") { ai.batchIds = ids }
            Divider()
        }
        if state.selectedMailbox == AIState.pendingMailbox {
            Button(ai.isFollowUp(m.id) ? "✨ 不再跟进" : "✨ 标为已处理") { Task { await ai.setDone(m.id, done: true) } }
            Divider()
        }
        Button("回复") { state.reply(m.id, mode: .reply) }
        Button("全部回复") { state.reply(m.id, mode: .replyAll) }
        Button("转发") { state.reply(m.id, mode: .forward) }
        Divider()
        Button(m.flags.seen ? "标为未读" : "标为已读") { Task { await state.act(ids, m.flags.seen ? .unread : .read) } }
        Button(m.flags.flagged ? "取消星标" : "加星标") { Task { await state.act(ids, m.flags.flagged ? .unflag : .flag) } }
        Button(m.pinned ? "取消置顶" : "置顶") { Task { await state.act(ids, m.pinned ? .unpin : .pin) } }
        Button("稍后提醒…") { snoozeTarget = ids }
        if m.snoozedUntil != nil {
            Button("取消稍后提醒") { Task { await state.act(ids, .unsnooze) } }
        }
        Divider()
        Button("归档") { Task { await state.act(ids, .archive) } }
        Button("移动到…") { moveTarget = ids }
        Menu("分类") {
            ForEach(Category.allCases) { c in
                Button(c.title) { Task { await state.act(ids, .category(c)) } }
            }
        }
        Button("标记为垃圾邮件") { Task { await state.act(ids, .spam) } }
        Button("删除", role: .destructive) { Task { await state.act(ids, .trash) } }
        Divider()
        Button(m.isVip ? "取消 VIP" : "设为 VIP 发件人") {
            Task {
                try? await state.api.setVip(m.from.email, !m.isVip)
                state.scheduleReload()
            }
        }
        Button("屏蔽发件人 \(m.from.email)") {
            Task {
                try? await state.api.block(m.from.email)
                await state.act([m.id], .trash)
            }
        }
    }

    /// Gmail-style single-key shortcuts.
    private func handleKey(_ chars: String) -> Bool {
        let ids = Array(state.selection)
        switch chars {
        case "j", "k":
            guard !state.messages.isEmpty else { return true }
            let idx = state.messages.firstIndex { state.selection.contains($0.id) } ?? -1
            let next = chars == "j" ? min(idx + 1, state.messages.count - 1) : max(idx - 1, 0)
            state.selection = [state.messages[next].id]
            return true
        case "e":
            Task { await state.act(ids, .archive) }
        case "#":
            Task { await state.act(ids, .trash) }
        case "!":
            Task { await state.act(ids, .spam) }
        case "s":
            let allFlagged = state.messages.filter { ids.contains($0.id) }.allSatisfy(\.flags.flagged)
            Task { await state.act(ids, allFlagged ? .unflag : .flag) }
        case "u":
            Task { await state.act(ids, .unread) }
        case "z":
            Task { await state.undoLast() }
        case "x":
            return false
        case "r", "a", "f":
            guard let id = ids.first else { return true }
            state.reply(id, mode: chars == "r" ? .reply : chars == "a" ? .replyAll : .forward)
        case "c":
            state.compose()
        default:
            return false
        }
        return true
    }
}

struct MessageRow: View {
    let message: MessageSummary
    let account: Account?
    let showAccount: Bool
    let density: String
    let showSnippet: Bool
    /// Shown in the "✨ 待处理" list: deadline and AI one-liner.
    var insight: AIInsight? = nil
    /// A sent message still waiting for a reply (pending list, §8.7).
    var followUp = false

    private var unread: Bool { !message.flags.seen || message.threadUnread > 0 }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            ZStack(alignment: .topLeading) {
                Avatar(address: message.from, size: density == "compact" ? 26 : 34)
                if unread {
                    Circle().fill(Color.accentColor).frame(width: 9, height: 9).offset(x: -4, y: -2)
                }
            }
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    if message.isVip {
                        Image(systemName: "crown.fill").font(.caption2).foregroundStyle(.orange)
                    }
                    Text(message.from.display)
                        .fontWeight(unread ? .semibold : .regular)
                        .lineLimit(1)
                    if message.threadCount > 1 {
                        Text("\(message.threadCount)")
                            .font(.caption2.monospacedDigit())
                            .padding(.horizontal, 5)
                            .background(.quaternary, in: Capsule())
                    }
                    Spacer(minLength: 4)
                    if message.pinned { Image(systemName: "pin.fill").font(.caption2).foregroundStyle(.secondary) }
                    if message.snoozedUntil != nil { Image(systemName: "clock").font(.caption2).foregroundStyle(.secondary) }
                    if message.hasAttachments { Image(systemName: "paperclip").font(.caption2).foregroundStyle(.secondary) }
                    if message.flags.flagged { Image(systemName: "star.fill").font(.caption2).foregroundStyle(.orange) }
                    if message.flags.answered { Image(systemName: "arrowshape.turn.up.left").font(.caption2).foregroundStyle(.secondary) }
                    Text(Fmt.relative(message.date))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Text(message.subject.isEmpty ? "（无主题）" : message.subject)
                    .font(.callout)
                    .fontWeight(unread ? .medium : .regular)
                    .lineLimit(1)
                if let insight {
                    HStack(spacing: 4) {
                        if let risk = insight.riskTitle {
                            AIChip(text: risk, symbol: "exclamationmark.shield.fill", color: .red)
                                .help(insight.riskReason)
                        }
                        if followUp {
                            AIChip(text: "待跟进", symbol: "arrow.uturn.forward", color: .teal)
                        } else if let d = insight.deadline {
                            AIChip(text: "截止 " + AIFmt.deadline(d), symbol: "clock", color: AIFmt.isOverdueSoon(d) ? .red : .blue)
                        } else if insight.needsReply {
                            AIChip(text: "需回复", symbol: "arrowshape.turn.up.left", color: .orange)
                        }
                        if insight.priority == "high" {
                            AIChip(text: "高优先级", color: .red)
                        }
                        Text(insight.oneLine.isEmpty ? (insight.todos.first ?? "") : insight.oneLine)
                            .font(.caption)
                            .foregroundStyle(.purple)
                            .lineLimit(1)
                    }
                }
                if showSnippet && density != "compact" && !message.snippet.isEmpty && insight == nil {
                    Text(message.snippet)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
                if showAccount || !message.labels.isEmpty || message.category != .primary {
                    HStack(spacing: 4) {
                        if showAccount, let account {
                            Tag(text: account.displayLabel, color: Color(hex: account.color))
                        }
                        if message.category != .primary {
                            Tag(text: message.category.title, color: .gray)
                        }
                        ForEach(message.labels.prefix(3), id: \.self) { l in
                            Tag(text: l, color: .purple)
                        }
                    }
                }
            }
        }
        .padding(.vertical, density == "compact" ? 2 : 5)
        .contentShape(Rectangle())
    }
}

struct Tag: View {
    let text: String
    let color: Color

    var body: some View {
        Text(text)
            .font(.system(size: 10))
            .lineLimit(1)
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .foregroundStyle(color)
            .background(color.opacity(0.12), in: RoundedRectangle(cornerRadius: 4))
    }
}

struct Avatar: View {
    let address: EmailAddress
    var size: CGFloat = 34

    private var initials: String {
        let name = address.display.trimmingCharacters(in: .whitespaces)
        guard let first = name.first else { return "?" }
        if first.isASCII {
            let parts = name.split(separator: " ")
            let letters = parts.prefix(2).compactMap(\.first)
            return String(letters).uppercased()
        }
        // Chinese names: the given name reads better than the family name.
        return String(name.suffix(name.count >= 3 ? 2 : 1))
    }

    var body: some View {
        Circle()
            .fill(Color.forString(address.email).gradient)
            .frame(width: size, height: size)
            .overlay(
                Text(initials)
                    .font(.system(size: size * 0.38, weight: .semibold))
                    .foregroundStyle(.white)
                    .minimumScaleFactor(0.5)
            )
    }
}
