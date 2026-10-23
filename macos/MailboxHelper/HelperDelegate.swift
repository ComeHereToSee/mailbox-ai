import AppKit
import UserNotifications

/// Background helper: hosts the mail engine, shows notifications and the menu bar extra.
final class HelperDelegate: NSObject, NSApplicationDelegate {
    private let notifications = NotificationController()
    private let statusItem = StatusItemController()
    private var refreshWork: DispatchWorkItem?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let me = ProcessInfo.processInfo.processIdentifier
        let others = NSRunningApplication.runningApplications(withBundleIdentifier: Daemon.helperBundleId)
            .filter { $0.processIdentifier != me }
        if !others.isEmpty {
            NSApp.terminate(nil)
            return
        }
        notifications.setup()
        statusItem.setup()
        EngineHost.shared.onEvent = { [weak self] data in
            let event = MailEvent.decode(data)
            DispatchQueue.main.async { self?.handle(event) }
        }
        // Apple's on-device model as a local OpenAI-compatible endpoint (macOS 26+ only).
        AppleModelServer.shared.start()
        if !EngineHost.shared.start() {
            statusItem.showError(EngineHost.shared.lastError ?? "邮件引擎启动失败")
            return
        }
        scheduleMenuRefresh()
    }

    func applicationWillTerminate(_ notification: Notification) {
        EngineHost.shared.stop()
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls {
            Daemon.openMainApp(url: url)
        }
    }

    private func handle(_ event: MailEvent) {
        switch event {
        case let .newMail(_, label, _, message, notify, sound):
            if notify {
                notifications.post(message: message, accountLabel: label, sound: sound)
            }
            scheduleMenuRefresh()
        case let .unreadCounts(total, _):
            statusItem.unread = total
            scheduleMenuRefresh()
        case .messagesChanged, .accountsChanged, .settingsChanged, .snoozed:
            scheduleMenuRefresh()
        case let .accountStatus(status):
            statusItem.updateStatus(status)
        case let .outbox(_, status, error):
            if status == .failed, let error {
                notifications.postSendFailure(error)
            }
        case let .aiInsight(messageId):
            scheduleMenuRefresh()
            annotateIfNeeded(messageId)
        case let .aiNotify(messageId, title, subtitle, body):
            // notify_mode "important": the engine held back the new-mail notification until the analysis finished.
            notifications.postMail(messageId: messageId, title: title, subtitle: subtitle, body: body)
            scheduleMenuRefresh()
        case let .aiConfirm(id, kind, title, detail):
            // Also answerable from here when the main app is closed; the first decision wins.
            notifications.postConfirm(id: id, kind: kind, title: title, detail: detail)
        case let .aiReminder(messageId, title, body):
            notifications.postAI(identifier: "ai-reminder-\(messageId)", title: title.isEmpty ? "⏰ 待处理邮件" : title, body: body,
                                 url: "mailboxai://message/\(messageId)", timeSensitive: true)
            scheduleMenuRefresh()
        case let .aiLoop(loopId, runId, name, status, preview):
            postLoop(loopId: loopId, runId: runId, name: name, status: status, preview: preview)
        case let .aiAutoreply(messageId, ruleName, draftId, sent, subject):
            postAutoReply(messageId: messageId, ruleName: ruleName, draftId: draftId, sent: sent, subject: subject)
        default:
            break
        }
    }

    /// notify_mode "annotate": add "✨ one_line" to the mail notification if it is still in Notification Center.
    private func annotateIfNeeded(_ messageId: Int64) {
        let notifications = self.notifications
        Task {
            guard let settings = try? await APIClient.shared.aiSettings(), settings.notifyMode == "annotate",
                  let insight = try? await APIClient.shared.aiInsight(messageId), !insight.oneLine.isEmpty else { return }
            await notifications.annotate(messageId: messageId, oneLine: insight.oneLine)
        }
    }

    /// `ai_loop` carries no `notify` flag: look the Loop up and respect its setting (default: notify).
    private func postLoop(loopId: String, runId: String, name: String, status: String, preview: String) {
        let notifications = self.notifications
        Task {
            let loops = (try? await APIClient.shared.aiLoops()) ?? []
            let notify = loops.first { $0.id == loopId }?.notify ?? true
            guard notify else { return }
            let failed = status == "failed" || status == "error"
            var body = preview.isEmpty ? "已完成" : preview
            if failed { body = preview.isEmpty ? "运行失败" : "运行失败：" + preview }
            await MainActor.run {
                notifications.postAI(identifier: "ai-loop-\(runId.isEmpty ? UUID().uuidString : runId)", title: "✨ \(name)", body: body,
                                     url: "mailboxai://assistant?loop=\(loopId)")
            }
        }
    }

    /// "✨ 已为 发件人 生成回复草稿" / "已自动回复 发件人".
    private func postAutoReply(messageId: Int64, ruleName: String, draftId: String?, sent: Bool, subject: String) {
        let notifications = self.notifications
        Task {
            let sender = (try? await APIClient.shared.message(messageId))?.summary.from.display ?? "发件人"
            let title = sent ? "✨ 已自动回复 \(sender)" : "✨ 已为 \(sender) 生成回复草稿"
            let url: String
            if !sent, let draftId, !draftId.isEmpty {
                url = "mailboxai://draft/\(draftId)"
            } else {
                url = "mailboxai://message/\(messageId)"
            }
            var parts: [String] = []
            if !subject.isEmpty { parts.append(subject) }
            if !ruleName.isEmpty { parts.append("规则：" + ruleName) }
            let body = parts.isEmpty ? "点按查看" : parts.joined(separator: " · ")
            await MainActor.run {
                notifications.postAI(identifier: "ai-autoreply-\(messageId)", title: title, body: body, url: url)
            }
        }
    }

    private func scheduleMenuRefresh() {
        refreshWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            Task { await self?.statusItem.refresh() }
        }
        refreshWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4, execute: work)
    }
}
