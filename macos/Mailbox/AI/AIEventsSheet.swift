import SwiftUI

/// "提取日程到日历" (docs/AI.md §8.1): AI-extracted events as editable rows, written with EventKit.
struct AIEventsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var ai = AIState.shared
    let messageId: Int64
    var subject: String = ""

    struct Row: Identifiable {
        let id = UUID()
        var include = true
        var title: String
        var start: Date
        var end: Date
        var allDay: Bool
        var location: String
        var notes: String
        var sourceText: String

        init(_ e: AIExtractedEvent) {
            let begin = Date(timeIntervalSince1970: TimeInterval(e.start))
            let fallback = begin.addingTimeInterval(e.allDay ? 86400 : 3600)
            let finish = e.end.map { Date(timeIntervalSince1970: TimeInterval($0)) } ?? fallback
            title = e.title
            start = begin
            end = finish > begin ? finish : fallback
            allDay = e.allDay
            location = e.location ?? ""
            notes = e.notes
            sourceText = e.sourceText
        }

        var event: AIExtractedEvent {
            let s = Int64(start.timeIntervalSince1970)
            let e = Int64(end.timeIntervalSince1970)
            let loc = location.trimmingCharacters(in: .whitespaces)
            return AIExtractedEvent(title: title.trimmingCharacters(in: .whitespaces), start: s, end: e > s ? e : nil, allDay: allDay,
                                    location: loc.isEmpty ? nil : loc, notes: notes, sourceText: sourceText)
        }
    }

    @State private var rows: [Row] = []
    @State private var loading = false
    @State private var loaded = false
    @State private var error: String? = nil
    @State private var notConfigured = false
    @State private var adding = false

    private var chosen: [Row] { rows.filter(\.include) }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 8) {
                Image(systemName: "calendar.badge.plus").foregroundStyle(.purple).font(.title2)
                VStack(alignment: .leading, spacing: 1) {
                    Text("提取日程到日历").font(.title3.weight(.semibold))
                    if !subject.isEmpty { Text(subject).font(.caption).foregroundStyle(.secondary).lineLimit(1) }
                }
                Spacer()
                if loading { ProgressView().controlSize(.small) }
            }
            content
            if notConfigured {
                AINotConfiguredView(message: error ?? "还没有设置 AI 模型")
            } else if let error {
                Label(error, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.red).font(.callout)
            }
            HStack {
                Button {
                    Task { await extract(force: true) }
                } label: {
                    Label("重新提取", systemImage: "arrow.clockwise")
                }
                .disabled(loading)
                Spacer()
                if adding { ProgressView().controlSize(.small) }
                Button("取消") { dismiss() }.keyboardShortcut(.cancelAction)
                Button(chosen.count > 1 ? "加入日历（\(chosen.count)）" : "加入日历") { Task { await add() } }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(chosen.isEmpty || adding || loading)
            }
        }
        .padding(20)
        .frame(width: 640, height: 520)
        .task { await start() }
    }

    @ViewBuilder
    private var content: some View {
        if loading && rows.isEmpty {
            VStack(spacing: 10) {
                ProgressView()
                Text("AI 正在从邮件中查找日程…").foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if rows.isEmpty {
            ContentUnavailableView("没有发现日程", systemImage: "calendar",
                                   description: Text(loaded ? "这封邮件里没有找到明确的时间安排。可以点「重新提取」再试一次。" : ""))
                .frame(maxHeight: .infinity)
        } else {
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach($rows) { $row in
                        AIEventRowEditor(row: $row)
                    }
                }
                .padding(2)
            }
            .frame(maxHeight: .infinity)
        }
    }

    private func start() async {
        if let cached = ai.insights[messageId]?.events, !cached.isEmpty {
            rows = cached.map(Row.init)
            loaded = true
            return
        }
        await extract(force: false)
    }

    private func extract(force: Bool) async {
        loading = true
        error = nil
        notConfigured = false
        defer {
            loading = false
            loaded = true
        }
        do {
            let events = try await APIClient.shared.aiExtractEvents(messageId, force: force)
            rows = events.map(Row.init)
            if var i = ai.insights[messageId] {
                i.events = events
                ai.insights[messageId] = i
            }
        } catch let e as APIError {
            error = e.message
            notConfigured = e.code == "ai_not_configured"
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func add() async {
        adding = true
        defer { adding = false }
        let saved = await CalendarService.shared.addEvents(chosen.map(\.event), messageId: messageId)
        if saved > 0 { dismiss() }
    }
}

/// One editable event: include, title, all-day, start / end, location, the original wording.
private struct AIEventRowEditor: View {
    @Binding var row: AIEventsSheet.Row

    var body: some View {
        let components: DatePickerComponents = row.allDay ? [.date] : [.date, .hourAndMinute]
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Toggle("", isOn: $row.include)
                    .labelsHidden()
                    .toggleStyle(.checkbox)
                TextField("标题", text: $row.title)
                    .textFieldStyle(.roundedBorder)
                Toggle("全天", isOn: $row.allDay)
                    .toggleStyle(.checkbox)
                    .fixedSize()
            }
            HStack(spacing: 16) {
                DatePicker("开始", selection: $row.start, displayedComponents: components)
                    .fixedSize()
                DatePicker("结束", selection: $row.end, displayedComponents: components)
                    .fixedSize()
                Spacer(minLength: 0)
            }
            .padding(.leading, 26)
            TextField("地点（可选）", text: $row.location)
                .textFieldStyle(.roundedBorder)
                .padding(.leading, 26)
            if !row.sourceText.isEmpty || !row.notes.isEmpty {
                Text(detailText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, 26)
            }
        }
        .padding(10)
        .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 10))
        .opacity(row.include ? 1 : 0.55)
        .onChange(of: row.start) { old, new in
            // Keep the duration when the start moves.
            let duration = row.end.timeIntervalSince(old)
            row.end = new.addingTimeInterval(duration > 0 ? duration : (row.allDay ? 86400 : 3600))
        }
    }

    private var detailText: String {
        var parts: [String] = []
        if !row.sourceText.isEmpty { parts.append("原文：「\(row.sourceText)」") }
        if !row.notes.isEmpty { parts.append(row.notes) }
        return parts.joined(separator: "  ·  ")
    }
}
