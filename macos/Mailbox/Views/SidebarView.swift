import SwiftUI

struct SidebarView: View {
    @EnvironmentObject private var state: AppState
    @ObservedObject private var web = WebMailStore.shared
    @ObservedObject private var ai = AIState.shared
    @State private var collapsed: Set<String> = []
    @State private var renaming: Folder?
    @State private var newFolderFor: String?
    @State private var folderName = ""

    private var selection: Binding<String?> {
        Binding(get: { state.selectedMailbox }, set: { if let v = $0 { state.select(mailbox: v) } })
    }

    var body: some View {
        List(selection: selection) {
            Section("收件箱") {
                row("unified:inbox", "全部收件箱", symbol: "tray.2.fill", badge: state.counts["unified:inbox"])
                ForEach(mailAccounts) { acc in
                    accountInboxRow(acc)
                }
            }
            if !webAccounts.isEmpty {
                Section("网页邮箱") {
                    ForEach(webAccounts) { acc in
                        Label(acc.displayLabel, systemImage: "globe")
                            .badge(web.unread[acc.id] ?? 0)
                            .tag("web:\(acc.id)")
                            .contextMenu {
                                Button("删除", role: .destructive) {
                                    Task {
                                        try? await state.api.deleteAccount(acc.id)
                                        await state.reloadAll()
                                    }
                                }
                            }
                    }
                }
            }
            Section("智能邮箱") {
                row(AIState.pendingMailbox, "待处理", symbol: "sparkles", badge: ai.reminders.count)
                    .help("AI 发现需要回复、有截止时间或待办的邮件")
                Button {
                    WindowRouter.shared.showAssistant()
                } label: {
                    Label("AI 助手", systemImage: "sparkles.rectangle.stack")
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("问问邮箱、查看 Loop 结果")
                row("unread", "未读", symbol: "envelope.badge", badge: state.counts["unread"])
                row("starred", "星标", symbol: "star", badge: state.counts["starred"])
                row("vip", "VIP", symbol: "crown", badge: state.counts["vip"])
                row("snoozed", "稍后提醒", symbol: "clock", badge: state.counts["snoozed"])
                row("pinned", "置顶", symbol: "pin")
                row("attachments", "有附件", symbol: "paperclip")
                row("unified:drafts", "草稿", symbol: "doc", badge: state.counts["unified:drafts"])
                row("unified:sent", "已发送", symbol: "paperplane")
                row("unified:archive", "归档", symbol: "archivebox")
                row("unified:junk", "垃圾邮件", symbol: "xmark.bin", badge: state.counts["unified:junk"])
                row("unified:trash", "已删除", symbol: "trash")
            }
            if state.settings.categoriesEnabled {
                Section("分类") {
                    ForEach(Category.allCases) { c in
                        row("category:\(c.rawValue)", c.title, symbol: c.symbol, badge: state.counts["category:\(c.rawValue)"])
                    }
                }
            }
            if !state.smartFolders.isEmpty {
                Section("智能文件夹") {
                    ForEach(state.smartFolders) { f in
                        row("smart:\(f.id)", f.name, symbol: f.icon.isEmpty ? "line.3.horizontal.decrease.circle" : f.icon)
                            .contextMenu {
                                Button("删除智能文件夹", role: .destructive) {
                                    Task {
                                        try? await state.api.deleteSmartFolder(f.id)
                                        await state.reloadAll()
                                    }
                                }
                            }
                    }
                }
            }
            ForEach(mailAccounts) { acc in
                Section(isExpanded: Binding(get: { !collapsed.contains(acc.id) }, set: { open in
                    if open { collapsed.remove(acc.id) } else { collapsed.insert(acc.id) }
                })) {
                    ForEach(folderRows(acc), id: \.folder.id) { item in
                        folderRow(item.folder, depth: item.depth)
                    }
                } header: {
                    HStack(spacing: 6) {
                        Circle().fill(Color(hex: acc.color)).frame(width: 8, height: 8)
                        Text(acc.displayLabel)
                    }
                    .contextMenu { accountMenu(acc) }
                }
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .bottom) { footer }
        .alert("重命名文件夹", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("名称", text: $folderName)
            Button("取消", role: .cancel) {}
            Button("重命名") {
                if let f = renaming {
                    Task {
                        _ = try? await state.api.renameFolder(f.id, name: folderName)
                        await state.reloadFolders()
                    }
                }
            }
        }
        .alert("新建文件夹", isPresented: Binding(get: { newFolderFor != nil }, set: { if !$0 { newFolderFor = nil } })) {
            TextField("名称", text: $folderName)
            Button("取消", role: .cancel) {}
            Button("创建") {
                if let acc = newFolderFor, !folderName.trimmingCharacters(in: .whitespaces).isEmpty {
                    Task {
                        _ = try? await state.api.createFolder(account: acc, name: folderName, parent: nil)
                        await state.reloadFolders()
                    }
                }
            }
        }
    }

    private var mailAccounts: [Account] { state.accounts.filter { $0.kind != .web } }
    private var webAccounts: [Account] { state.accounts.filter { $0.kind == .web } }

    private struct FolderItem {
        var folder: Folder
        var depth: Int
    }

    private func folderRows(_ acc: Account) -> [FolderItem] {
        let fs = (state.folders[acc.id] ?? []).filter { $0.selectable }
        let system = fs.filter { $0.role != .custom }.sorted { rank($0.role) < rank($1.role) }
        let custom = fs.filter { $0.role == .custom }.sorted { $0.path.localizedStandardCompare($1.path) == .orderedAscending }
        return system.map { FolderItem(folder: $0, depth: 0) } + custom.map { f in
            let d = f.delimiter ?? "/"
            return FolderItem(folder: f, depth: d.isEmpty ? 0 : max(0, f.path.components(separatedBy: d).count - 1))
        }
    }

    private func rank(_ r: FolderRole) -> Int {
        [FolderRole.inbox, .flagged, .important, .drafts, .sent, .outbox, .archive, .all, .junk, .trash].firstIndex(of: r) ?? 99
    }

    private func row(_ tag: String, _ title: String, symbol: String, badge: Int? = nil) -> some View {
        Label(title, systemImage: symbol)
            .badge(badge ?? 0)
            .tag(tag)
    }

    private func accountInboxRow(_ acc: Account) -> some View {
        let status = state.statuses[acc.id]
        return HStack(spacing: 6) {
            Image(systemName: "tray")
                .foregroundStyle(Color(hex: acc.color))
            Text(acc.displayLabel).lineLimit(1)
            Spacer(minLength: 4)
            if status?.state == .syncing || status?.state == .connecting {
                ProgressView().controlSize(.mini)
            } else if status?.state == .error || status?.state == .offline {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(status?.state == .error ? .red : .orange)
                    .help(status?.error ?? "")
            }
        }
        .badge(state.counts["account:\(acc.id):inbox"] ?? 0)
        .tag("account:\(acc.id):inbox")
        .contextMenu { accountMenu(acc) }
    }

    private func folderRow(_ f: Folder, depth: Int) -> some View {
        Label(f.role == .custom ? f.name : f.role.title, systemImage: f.role.symbol)
            .padding(.leading, CGFloat(depth) * 12)
            .badge(f.role == .trash || f.role == .sent ? 0 : f.unread)
            .tag("folder:\(f.id)")
            .contextMenu {
                Button("立即同步") { Task { try? await state.api.syncFolder(f.id) } }
                Button("全部标为已读") {
                    Task { try? await state.api.markAllRead(mailbox: "folder:\(f.id)"); state.scheduleReload() }
                }
                if f.role == .custom {
                    Divider()
                    Button("重命名…") {
                        folderName = f.name
                        renaming = f
                    }
                    Button("删除文件夹", role: .destructive) {
                        Task {
                            try? await state.api.deleteFolder(f.id)
                            await state.reloadFolders()
                        }
                    }
                }
            }
            .dropDestination(for: String.self) { items, _ in
                let ids = items.compactMap { Int64($0) }
                guard !ids.isEmpty else { return false }
                Task { await state.act(ids, .move(folderId: f.id)) }
                return true
            }
    }

    @ViewBuilder
    private func accountMenu(_ acc: Account) -> some View {
        Button("立即同步") { Task { try? await state.api.syncAccount(acc.id) } }
        Button("写邮件") { state.compose(ComposeRequest(accountId: acc.id)) }
        Button("新建文件夹…") {
            folderName = ""
            newFolderFor = acc.id
        }
        Divider()
        SettingsLink { Text("账户设置…") }
    }

    private var footer: some View {
        VStack(spacing: 0) {
            Divider()
            HStack {
                Button {
                    state.showAddAccount = true
                } label: {
                    Label("添加账户", systemImage: "plus")
                }
                .buttonStyle(.borderless)
                Spacer()
                syncSummary
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
        .background(.bar)
    }

    @ViewBuilder
    private var syncSummary: some View {
        let syncing = state.statuses.values.filter { $0.state == .syncing || $0.state == .connecting }
        let failing = state.statuses.values.filter { $0.state == .error }
        if !syncing.isEmpty {
            HStack(spacing: 4) {
                ProgressView().controlSize(.mini)
                Text("同步中").font(.caption).foregroundStyle(.secondary)
            }
        } else if !failing.isEmpty {
            Text("\(failing.count) 个账户出错").font(.caption).foregroundStyle(.red)
                .help(failing.compactMap(\.error).joined(separator: "\n"))
        } else {
            Button { state.sync() } label: { Image(systemName: "arrow.clockwise") }
                .buttonStyle(.borderless)
                .help("检查新邮件")
        }
    }
}
