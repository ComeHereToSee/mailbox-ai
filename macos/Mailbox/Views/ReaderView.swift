import UniformTypeIdentifiers
import QuickLook
import SwiftUI

struct ReaderView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        if state.selection.count > 1 {
            MultiSelectionView(ids: Array(state.selection))
        } else if let id = state.selection.first {
            ThreadView(messageId: id).id(id)
        } else {
            ContentUnavailableView("未选择邮件", systemImage: "envelope.open", description: Text("在左侧选择一封邮件"))
        }
    }
}

struct MultiSelectionView: View {
    @EnvironmentObject private var state: AppState
    let ids: [Int64]

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "envelope.stack").font(.system(size: 48)).foregroundStyle(.secondary)
            Text("已选择 \(ids.count) 封邮件").font(.title3)
            HStack {
                Button("标为已读") { Task { await state.act(ids, .read) } }
                Button("标为未读") { Task { await state.act(ids, .unread) } }
                Button("加星标") { Task { await state.act(ids, .flag) } }
            }
            HStack {
                Button("归档") { Task { await state.act(ids, .archive) } }
                Button("垃圾邮件") { Task { await state.act(ids, .spam) } }
                Button("删除", role: .destructive) { Task { await state.act(ids, .trash) } }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct ThreadView: View {
    @EnvironmentObject private var state: AppState
    let messageId: Int64
    @State private var details: [MessageDetail] = []
    @State private var expanded: Set<Int64> = []
    @State private var error: String?
    @State private var loading = true
    @State private var showSnooze = false
    @State private var showMove = false
    @State private var showSource = false

    private var subject: String { details.last?.summary.subject ?? "" }
    private var current: MessageDetail? { details.first { $0.id == messageId } ?? details.last }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let error {
                    Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.red)
                }
                if !details.isEmpty {
                    header
                    ForEach(details) { d in
                        MessageCard(detail: d, expanded: expanded.contains(d.id) || details.count == 1, onToggle: {
                            if expanded.contains(d.id) { expanded.remove(d.id) } else { expanded.insert(d.id) }
                        }, onReload: { Task { await load() } })
                    }
                } else if loading {
                    ProgressView().frame(maxWidth: .infinity).padding(40)
                }
            }
            .padding(20)
            .frame(maxWidth: 980)
            .frame(maxWidth: .infinity)
        }
        .task(id: messageId) { await load() }
        .toolbar { toolbar }
        .sheet(isPresented: $showSnooze) {
            SnoozePicker { date in
                showSnooze = false
                if let date { Task { await state.act([messageId], .snooze(until: Int64(date.timeIntervalSince1970))) } }
            }
        }
        .sheet(isPresented: $showMove) {
            MoveToFolderSheet(messageIds: [messageId]) { showMove = false }.environmentObject(state)
        }
        .sheet(isPresented: $showSource) {
            SourceView(messageId: messageId)
        }
        .userActivity("com.comeheretosee.mailbox.ai.message") { activity in
            activity.title = subject
            activity.userInfo = ["message_id": messageId]
            activity.isEligibleForHandoff = false
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(subject.isEmpty ? "（无主题）" : subject)
                .font(.title2.weight(.semibold))
                .textSelection(.enabled)
            HStack(spacing: 6) {
                if let d = current {
                    if let acc = state.accountsById[d.summary.accountId] {
                        Tag(text: acc.displayLabel, color: Color(hex: acc.color))
                    }
                    Tag(text: d.folderRole.title, color: .gray)
                    if d.summary.category != .primary { Tag(text: d.summary.category.title, color: .gray) }
                    ForEach(d.summary.labels, id: \.self) { Tag(text: $0, color: .purple) }
                    if details.count > 1 { Text("\(details.count) 封邮件").font(.caption).foregroundStyle(.secondary) }
                }
            }
        }
        .padding(.bottom, 4)
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .primaryAction) {
            Button { state.reply(messageId, mode: .reply) } label: { Label("回复", systemImage: "arrowshape.turn.up.left") }
            Button { state.reply(messageId, mode: .replyAll) } label: { Label("全部回复", systemImage: "arrowshape.turn.up.left.2") }
            Button { state.reply(messageId, mode: .forward) } label: { Label("转发", systemImage: "arrowshape.turn.up.right") }
            Button { Task { await state.act([messageId], .archive) } } label: { Label("归档", systemImage: "archivebox") }
            Button { Task { await state.act([messageId], .trash) } } label: { Label("删除", systemImage: "trash") }
            Button {
                let flagged = current?.summary.flags.flagged ?? false
                Task {
                    await state.act([messageId], flagged ? .unflag : .flag)
                    await load()
                }
            } label: {
                Label("星标", systemImage: current?.summary.flags.flagged == true ? "star.fill" : "star")
            }
            Menu {
                moreMenu
            } label: {
                Label("更多", systemImage: "ellipsis.circle")
            }
        }
    }

    @ViewBuilder
    private var moreMenu: some View {
        Button("标为未读") { Task { await state.act([messageId], .unread) } }
        Button("稍后提醒…") { showSnooze = true }
        Button("移动到…") { showMove = true }
        Button(current?.summary.pinned == true ? "取消置顶" : "置顶") {
            Task { await state.act([messageId], current?.summary.pinned == true ? .unpin : .pin) }
        }
        Button("作为附件转发") { state.reply(messageId, mode: .forwardAsAttachment) }
        Divider()
        Button("标记为垃圾邮件") { Task { await state.act([messageId], .spam) } }
        Button("永久删除", role: .destructive) { Task { await state.act([messageId], .delete) } }
        Divider()
        if let d = current {
            Button("加入日历…") { CalendarService.shared.addEvent(from: d) }
            Button("创建提醒事项") { CalendarService.shared.addReminder(from: d) }
            Button("打印…") { PrintService.print(detail: d) }
            Button("导出 PDF…") { PrintService.exportPDF(detail: d) }
            Button("存储为 .eml…") { Task { await saveEml(d) } }
            Button("查看原始邮件") { showSource = true }
            Divider()
            ShareLink("分享…", item: SharingText.text(for: d), subject: Text(d.summary.subject))
            Button("屏蔽 \(d.summary.from.email)") {
                Task {
                    try? await state.api.block(d.summary.from.email)
                    await state.act([messageId], .trash)
                }
            }
        }
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            let t = try await state.api.thread(messageId)
            details = t
            error = nil
            var open = Set(t.filter { !$0.summary.flags.seen }.map(\.id))
            if let last = t.last { open.insert(last.id) }
            open.insert(messageId)
            expanded.formUnion(open)
            let unread = t.filter { !$0.summary.flags.seen }.map(\.id)
            if state.settings.markReadOnOpen, !unread.isEmpty {
                let delay = UInt64(max(0, state.settings.markReadDelaySecs)) * 1_000_000_000
                if delay > 0 { try? await Task.sleep(nanoseconds: delay) }
                if !Task.isCancelled, state.selection == [messageId] {
                    await state.act(unread, .read, expandThreads: false)
                    state.toast = nil
                }
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func saveEml(_ d: MessageDetail) async {
        guard let data = try? await state.api.rawMessage(d.id) else { return }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = (d.summary.subject.isEmpty ? "邮件" : d.summary.subject) + ".eml"
        if panel.runModal() == .OK, let url = panel.url {
            try? data.write(to: url)
        }
    }
}

struct MessageCard: View {
    @EnvironmentObject private var state: AppState
    let detail: MessageDetail
    let expanded: Bool
    let onToggle: () -> Void
    let onReload: () -> Void
    @State private var height: CGFloat = 80
    @State private var showRemote = false
    @State private var showDetails = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            headerRow
                .contentShape(Rectangle())
                .onTapGesture(perform: onToggle)
            if expanded {
                banners
                if detail.bodyLoaded, let html = detail.html {
                    MessageWebView(html: html, messageId: detail.id, allowRemote: detail.allowRemote || showRemote, height: $height)
                        .frame(height: height)
                        .frame(maxWidth: .infinity)
                        .background(Color.white, in: RoundedRectangle(cornerRadius: 8))
                } else if !detail.bodyLoaded {
                    Text(detail.warnings.first ?? "正文加载中…").foregroundStyle(.secondary)
                }
                if !detail.attachments.filter({ !$0.inline }).isEmpty {
                    AttachmentStrip(messageId: detail.id, attachments: detail.attachments.filter { !$0.inline })
                }
                if let invite = detail.invite {
                    InviteCard(messageId: detail.id, invite: invite)
                }
            } else {
                Text(detail.summary.snippet).font(.callout).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .padding(14)
        .background(.background, in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(.quaternary))
    }

    private var headerRow: some View {
        HStack(alignment: .top, spacing: 10) {
            Avatar(address: detail.summary.from, size: 38)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(detail.summary.from.display).fontWeight(.semibold).lineLimit(1).layoutPriority(1)
                    if detail.summary.isVip { Image(systemName: "crown.fill").foregroundStyle(.orange).font(.caption) }
                    Text(detail.summary.from.email).foregroundStyle(.secondary).font(.caption)
                        .lineLimit(1).truncationMode(.middle)
                        .textSelection(.enabled)
                }
                if expanded {
                    Button { showDetails.toggle() } label: {
                        Text(recipientsLine).font(.caption).foregroundStyle(.secondary).lineLimit(showDetails ? nil : 1)
                    }
                    .buttonStyle(.plain)
                }
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 4) {
                Text(Fmt.short(detail.summary.date)).font(.caption).foregroundStyle(.secondary)
                    .lineLimit(1).fixedSize()
                    .help(Fmt.full(detail.summary.date))
                HStack(spacing: 6) {
                    if detail.encrypted { Image(systemName: "lock.fill").help("加密邮件") }
                    if detail.signed { Image(systemName: "checkmark.seal").help("已签名") }
                    if detail.summary.hasAttachments { Image(systemName: "paperclip") }
                    if detail.summary.flags.flagged { Image(systemName: "star.fill").foregroundStyle(.orange) }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
    }

    private var recipientsLine: String {
        var parts: [String] = []
        if !detail.summary.to.isEmpty { parts.append("收件人：" + detail.summary.to.map { showDetails ? $0.full : $0.display }.joined(separator: "，")) }
        if !detail.cc.isEmpty { parts.append("抄送：" + detail.cc.map { showDetails ? $0.full : $0.display }.joined(separator: "，")) }
        if !detail.bcc.isEmpty { parts.append("密送：" + detail.bcc.map { showDetails ? $0.full : $0.display }.joined(separator: "，")) }
        if showDetails, !detail.replyTo.isEmpty { parts.append("回复至：" + detail.replyTo.map(\.full).joined(separator: "，")) }
        return parts.joined(separator: showDetails ? "\n" : "  ")
    }

    @ViewBuilder
    private var banners: some View {
        if let err = detail.decryptError {
            Banner(icon: "lock.trianglebadge.exclamationmark", color: .red, text: "无法解密：\(err)") {
                SettingsLink { Text("管理密钥") }
            }
        } else if detail.decrypted {
            Banner(icon: "lock.fill", color: .green, text: "这封邮件是加密的，已用你的私钥解密")
        } else if detail.encrypted {
            Banner(icon: "lock.fill", color: .orange, text: "这是一封加密邮件，导入对应的私钥后才能阅读") {
                SettingsLink { Text("管理密钥") }
            }
        }
        if let sig = detail.signature {
            Banner(icon: sig.valid ? "checkmark.seal.fill" : "xmark.seal.fill", color: sig.valid ? .green : .red,
                   text: (sig.kind == "smime" ? "S/MIME：" : "PGP：") + sig.message)
        }
        ForEach(detail.warnings, id: \.self) { w in
            Banner(icon: "exclamationmark.shield.fill", color: .red, text: w)
        }
        if detail.hasRemoteContent && !detail.allowRemote && !showRemote {
            Banner(icon: "photo", color: .blue, text: "为保护隐私，已阻止加载远程图片" + (detail.trackers > 0 ? "，并拦截了 \(detail.trackers) 个跟踪器" : "")) {
                Button("显示图片") { showRemote = true }
                Button("始终显示此发件人的图片") {
                    Task {
                        try? await state.api.trustImages(detail.id, trusted: true)
                        showRemote = true
                    }
                }
            }
        } else if detail.trackers > 0 {
            Banner(icon: "eye.slash", color: .gray, text: "已拦截 \(detail.trackers) 个邮件跟踪器")
        }
        if let to = detail.readReceiptTo {
            Banner(icon: "checkmark.message", color: .orange, text: "发件人请求已读回执（\(to)）") {
                Button("发送回执") { Task { try? await state.api.sendReadReceipt(detail.id) } }
            }
        }
        if detail.unsubscribe != nil {
            Banner(icon: "bell.slash", color: .gray, text: "这是一封订阅邮件") {
                Button("退订") { Task { await unsubscribe() } }
            }
        }
    }

    private func unsubscribe() async {
        do {
            let r = try await state.api.unsubscribe(detail.id)
            if let url = r.url.flatMap(URL.init(string:)), !r.done {
                NSWorkspace.shared.open(url)
            } else {
                state.show(Toast(text: "已退订"))
            }
        } catch {
            state.show(Toast(text: error.localizedDescription, isError: true))
        }
    }
}

struct Banner<Actions: View>: View {
    let icon: String
    let color: Color
    let text: String
    @ViewBuilder var actions: () -> Actions

    init(icon: String, color: Color, text: String, @ViewBuilder actions: @escaping () -> Actions) {
        self.icon = icon
        self.color = color
        self.text = text
        self.actions = actions
    }

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: icon).foregroundStyle(color)
            Text(text).font(.callout)
            Spacer()
            actions().buttonStyle(.link).font(.callout)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(color.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
    }
}

extension Banner where Actions == EmptyView {
    init(icon: String, color: Color, text: String) {
        self.init(icon: icon, color: color, text: text) { EmptyView() }
    }
}

struct AttachmentStrip: View {
    @EnvironmentObject private var state: AppState
    let messageId: Int64
    let attachments: [Attachment]
    @State private var previewURL: URL?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("\(attachments.count) 个附件").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("全部存储…") { Task { await saveAll() } }.buttonStyle(.link).font(.caption)
            }
            FlowLayout(spacing: 8) {
                ForEach(attachments) { a in
                    Button {
                        Task { previewURL = try? await state.api.attachmentFile(messageId, a.id) }
                    } label: {
                        HStack(spacing: 6) {
                            Image(nsImage: icon(for: a))
                                .resizable()
                                .frame(width: 24, height: 24)
                            VStack(alignment: .leading, spacing: 0) {
                                Text(a.filename).lineLimit(1).font(.callout)
                                Text(Fmt.size(a.size)).font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                        .padding(.horizontal, 8)
                        .padding(.vertical, 5)
                        .frame(maxWidth: 240, alignment: .leading)
                        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
                    }
                    .buttonStyle(.plain)
                    .contextMenu {
                        Button("快速查看") { Task { previewURL = try? await state.api.attachmentFile(messageId, a.id) } }
                        Button("打开") {
                            Task { if let url = try? await state.api.attachmentFile(messageId, a.id) { NSWorkspace.shared.open(url) } }
                        }
                        Button("存储为…") { Task { await save(a) } }
                    }
                    .onDrag { dragProvider(for: a) }
                }
            }
        }
        .quickLookPreview($previewURL)
    }

    private func icon(for a: Attachment) -> NSImage {
        let ext = (a.filename as NSString).pathExtension
        if let type = UTType(filenameExtension: ext) {
            return NSWorkspace.shared.icon(for: type)
        }
        return NSWorkspace.shared.icon(for: .data)
    }

    private func dragProvider(for a: Attachment) -> NSItemProvider {
        let provider = NSItemProvider()
        provider.suggestedName = a.filename
        let type = UTType(filenameExtension: (a.filename as NSString).pathExtension) ?? .data
        let api = state.api
        let mid = messageId
        provider.registerFileRepresentation(forTypeIdentifier: type.identifier, fileOptions: [], visibility: .all) { completion in
            Task {
                do {
                    let url = try await api.attachmentFile(mid, a.id)
                    completion(url, false, nil)
                } catch {
                    completion(nil, false, error)
                }
            }
            return nil
        }
        return provider
    }

    private func save(_ a: Attachment) async {
        guard let src = try? await state.api.attachmentFile(messageId, a.id) else { return }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = a.filename
        if panel.runModal() == .OK, let dst = panel.url {
            try? FileManager.default.removeItem(at: dst)
            try? FileManager.default.copyItem(at: src, to: dst)
        }
    }

    private func saveAll() async {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.prompt = "存储到此处"
        guard panel.runModal() == .OK, let dir = panel.url else { return }
        for a in attachments {
            if let src = try? await state.api.attachmentFile(messageId, a.id) {
                let dst = dir.appendingPathComponent(a.filename)
                try? FileManager.default.removeItem(at: dst)
                try? FileManager.default.copyItem(at: src, to: dst)
            }
        }
        NSWorkspace.shared.activateFileViewerSelecting([dir])
    }
}

struct InviteCard: View {
    @EnvironmentObject private var state: AppState
    let messageId: Int64
    let invite: CalendarInvite
    @State private var status: String?

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(spacing: 0) {
                Text(monthText).font(.caption2.weight(.bold)).foregroundStyle(.white).frame(maxWidth: .infinity).background(.red)
                Text(dayText).font(.title2.weight(.semibold)).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .frame(width: 52, height: 56)
            .background(.background, in: RoundedRectangle(cornerRadius: 8))
            .clipShape(RoundedRectangle(cornerRadius: 8))
            VStack(alignment: .leading, spacing: 4) {
                Text(invite.summary).font(.headline)
                Text(timeText).font(.callout).foregroundStyle(.secondary)
                if let loc = invite.location, !loc.isEmpty { Label(loc, systemImage: "mappin.and.ellipse").font(.callout) }
                if let org = invite.organizer { Text("组织者：\(org.display)").font(.caption).foregroundStyle(.secondary) }
                if invite.method.uppercased() == "REQUEST" {
                    HStack {
                        Button("接受") { respond("ACCEPTED") }
                        Button("暂定") { respond("TENTATIVE") }
                        Button("拒绝") { respond("DECLINED") }
                        Button("加入日历") { CalendarService.shared.addInvite(invite) }
                    }
                    .padding(.top, 4)
                    if let s = status ?? invite.myStatus.map(statusText) {
                        Text(s).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            Spacer()
        }
        .padding(12)
        .background(.blue.opacity(0.06), in: RoundedRectangle(cornerRadius: 10))
    }

    private var startDate: Date? { invite.start.map { Date(timeIntervalSince1970: TimeInterval($0)) } }

    private var monthText: String {
        guard let d = startDate else { return "" }
        let f = DateFormatter()
        f.dateFormat = "M月"
        if invite.allDay { f.timeZone = TimeZone(identifier: "UTC") }
        return f.string(from: d)
    }

    private var dayText: String {
        guard let d = startDate else { return "?" }
        let f = DateFormatter()
        f.dateFormat = "d"
        if invite.allDay { f.timeZone = TimeZone(identifier: "UTC") }
        return f.string(from: d)
    }

    private var timeText: String {
        guard let s = invite.start else { return "" }
        let start = Date(timeIntervalSince1970: TimeInterval(s))
        let f = DateFormatter()
        f.locale = Locale(identifier: "zh_CN")
        if invite.allDay {
            f.timeZone = TimeZone(identifier: "UTC")
            f.dateFormat = "yyyy年M月d日 EEEE（全天）"
            return f.string(from: start)
        }
        f.dateFormat = "yyyy年M月d日 EEEE HH:mm"
        var text = f.string(from: start)
        if let e = invite.end {
            let tf = DateFormatter()
            tf.dateFormat = "HH:mm"
            text += " – " + tf.string(from: Date(timeIntervalSince1970: TimeInterval(e)))
        }
        return text
    }

    private func statusText(_ s: String) -> String {
        switch s.uppercased() {
        case "ACCEPTED": return "你已接受"
        case "TENTATIVE": return "你已暂定"
        case "DECLINED": return "你已拒绝"
        default: return "等待你的回复"
        }
    }

    private func respond(_ partstat: String) {
        Task {
            do {
                try await state.api.rsvp(messageId, partstat: partstat)
                status = statusText(partstat) + "（回复已发送）"
                if partstat != "DECLINED" { CalendarService.shared.addInvite(invite, silent: true) }
            } catch {
                status = error.localizedDescription
            }
        }
    }
}

struct SourceView: View {
    @Environment(\.dismiss) private var dismiss
    let messageId: Int64
    @State private var text = "加载中…"

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                Text(text)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
            }
            Divider()
            HStack {
                Spacer()
                Button("关闭") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            .padding()
        }
        .frame(width: 760, height: 560)
        .task {
            if let data = try? await APIClient.shared.rawMessage(messageId) {
                text = String(data: data, encoding: .utf8) ?? String(decoding: data, as: UTF8.self)
            }
        }
    }
}

/// Simple wrapping layout for chips.
struct FlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0, y: CGFloat = 0, rowHeight: CGFloat = 0, maxX: CGFloat = 0
        for s in subviews {
            let size = s.sizeThatFits(.unspecified)
            if x > 0 && x + size.width > width {
                x = 0
                y += rowHeight + spacing
                rowHeight = 0
            }
            x += size.width + spacing
            maxX = max(maxX, x)
            rowHeight = max(rowHeight, size.height)
        }
        return CGSize(width: min(maxX, width), height: y + rowHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX, y = bounds.minY, rowHeight: CGFloat = 0
        for s in subviews {
            let size = s.sizeThatFits(.unspecified)
            if x > bounds.minX && x + size.width > bounds.maxX {
                x = bounds.minX
                y += rowHeight + spacing
                rowHeight = 0
            }
            s.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}
