import AppKit
import UserNotifications

/// System notifications for new mail: "[账户] 发件人", subject, snippet, with quick actions.
final class NotificationController: NSObject, UNUserNotificationCenterDelegate {
    private let center = UNUserNotificationCenter.current()
    private static let category = "NEW_MAIL"
    /// MCP send / delete confirmations (docs/AI.md §8.2).
    private static let confirmCategory = "AI_CONFIRM"

    func setup() {
        center.delegate = self
        center.requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in }
        let read = UNNotificationAction(identifier: "MARK_READ", title: "标为已读", options: [])
        let archive = UNNotificationAction(identifier: "ARCHIVE", title: "归档", options: [])
        let trash = UNNotificationAction(identifier: "TRASH", title: "删除", options: [.destructive])
        let reply = UNTextInputNotificationAction(identifier: "REPLY", title: "回复", options: [],
                                                  textInputButtonTitle: "发送", textInputPlaceholder: "输入回复内容")
        let cat = UNNotificationCategory(identifier: Self.category, actions: [reply, read, archive, trash], intentIdentifiers: [],
                                         options: [.customDismissAction])
        let allow = UNNotificationAction(identifier: "AI_ALLOW", title: "允许", options: [.authenticationRequired])
        let deny = UNNotificationAction(identifier: "AI_DENY", title: "拒绝", options: [])
        let confirm = UNNotificationCategory(identifier: Self.confirmCategory, actions: [allow, deny], intentIdentifiers: [],
                                             options: [.customDismissAction])
        center.setNotificationCategories([cat, confirm])
    }

    func post(message: MessageSummary, accountLabel: String, sound: Bool) {
        let content = UNMutableNotificationContent()
        content.title = "[\(accountLabel)] \(message.from.display)"
        content.subtitle = message.subject.isEmpty ? "（无主题）" : message.subject
        content.body = message.snippet
        content.threadIdentifier = message.accountId
        content.categoryIdentifier = Self.category
        content.userInfo = ["message_id": message.id, "account_id": message.accountId]
        if sound { content.sound = .default }
        if message.isVip { content.interruptionLevel = .timeSensitive }
        let req = UNNotificationRequest(identifier: "mail-\(message.id)", content: content, trigger: nil)
        center.add(req)
    }

    /// Smart notifications (`ai_notify`): a new-mail notification (same identifier, category and quick actions).
    func postMail(messageId: Int64, title: String, subtitle: String, body: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.subtitle = subtitle.isEmpty ? "（无主题）" : subtitle
        content.body = body
        content.categoryIdentifier = Self.category
        content.userInfo = ["message_id": messageId]
        content.sound = .default
        center.add(UNNotificationRequest(identifier: "mail-\(messageId)", content: content, trigger: nil))
    }

    /// notify_mode "annotate": re-posts the still-delivered `mail-<id>` notification silently with "✨ one_line" first.
    func annotate(messageId: Int64, oneLine: String) async {
        let line = oneLine.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !line.isEmpty else { return }
        let identifier = "mail-\(messageId)"
        let delivered = await center.deliveredNotifications()
        guard let note = delivered.first(where: { $0.request.identifier == identifier }),
              let content = note.request.content.mutableCopy() as? UNMutableNotificationContent,
              !content.body.hasPrefix("✨") else { return }
        content.body = "✨ " + line + (content.body.isEmpty ? "" : "\n" + content.body)
        content.sound = nil
        content.interruptionLevel = .passive
        try? await center.add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil))
    }

    /// An MCP client wants to send / delete mail: 允许 / 拒绝 answer `POST /v1/ai/confirm/{id}`.
    func postConfirm(id: String, kind: String, title: String, detail: String) {
        let content = UNMutableNotificationContent()
        content.title = title.isEmpty ? (kind == "delete" ? "MCP 请求删除邮件" : "MCP 请求发送邮件") : title
        content.subtitle = "需要你的确认（3 分钟内有效）"
        content.body = String(detail.prefix(400))
        content.categoryIdentifier = Self.confirmCategory
        content.threadIdentifier = "ai-confirm"
        content.userInfo = ["confirm_id": id]
        content.sound = .default
        content.interruptionLevel = .timeSensitive
        center.add(UNNotificationRequest(identifier: "ai-confirm-\(id)", content: content, trigger: nil))
    }

    /// AI notifications (reminders, Loop results, auto-replies); a click opens `url` in the main app.
    func postAI(identifier: String, title: String, subtitle: String? = nil, body: String, url: String, timeSensitive: Bool = false) {
        let content = UNMutableNotificationContent()
        content.title = title
        if let subtitle, !subtitle.isEmpty { content.subtitle = subtitle }
        content.body = body
        content.threadIdentifier = "ai"
        content.userInfo = ["url": url]
        content.sound = .default
        if timeSensitive { content.interruptionLevel = .timeSensitive }
        center.add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil))
    }

    func postSendFailure(_ error: String) {
        let content = UNMutableNotificationContent()
        content.title = "邮件发送失败"
        content.body = error
        content.sound = .default
        center.add(UNNotificationRequest(identifier: "send-failed-\(UUID().uuidString)", content: content, trigger: nil))
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        if notification.request.content.interruptionLevel == .passive {
            completionHandler([.list])
        } else {
            completionHandler([.banner, .list, .sound])
        }
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let info = response.notification.request.content.userInfo
        if let confirmId = info["confirm_id"] as? String {
            let action = response.actionIdentifier
            Task {
                switch action {
                case "AI_ALLOW", "AI_DENY":
                    // A 404 means the main app (or a timeout) decided first.
                    try? await APIClient.shared.answerAIConfirmation(confirmId, approve: action == "AI_ALLOW")
                case UNNotificationDismissActionIdentifier:
                    break
                default:
                    if let url = URL(string: "mailboxai://confirm/\(confirmId)") {
                        await MainActor.run { Daemon.openMainApp(url: url) }
                    }
                }
                completionHandler()
            }
            return
        }
        if let link = info["url"] as? String, let url = URL(string: link) {
            if response.actionIdentifier != UNNotificationDismissActionIdentifier {
                DispatchQueue.main.async { Daemon.openMainApp(url: url) }
            }
            completionHandler()
            return
        }
        guard let id = (info["message_id"] as? NSNumber)?.int64Value ?? (info["message_id"] as? Int64) else {
            completionHandler()
            return
        }
        let identifier = response.notification.request.identifier
        let action = response.actionIdentifier
        let replyText = (response as? UNTextInputNotificationResponse)?.userText
        Task {
            switch action {
            case "MARK_READ":
                _ = try? await APIClient.shared.act([id], .read)
            case "ARCHIVE":
                _ = try? await APIClient.shared.act([id], .archive)
            case "TRASH":
                _ = try? await APIClient.shared.act([id], .trash)
            case "REPLY":
                if let text = replyText, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    await Self.quickReply(to: id, text: text)
                }
            case UNNotificationDismissActionIdentifier:
                break
            default:
                if let url = URL(string: "mailboxai://message/\(id)") {
                    await MainActor.run { Daemon.openMainApp(url: url) }
                }
            }
            center.removeDeliveredNotifications(withIdentifiers: [identifier])
            completionHandler()
        }
    }

    private static func quickReply(to id: Int64, text: String) async {
        do {
            var draft = try await APIClient.shared.replyDraft(id, mode: .reply)
            let escaped = text
                .replacingOccurrences(of: "&", with: "&amp;")
                .replacingOccurrences(of: "<", with: "&lt;")
                .replacingOccurrences(of: ">", with: "&gt;")
                .replacingOccurrences(of: "\n", with: "<br>")
            draft.html = "<div>\(escaped)</div>" + draft.html
            _ = try await APIClient.shared.sendDraft(draft)
            _ = try? await APIClient.shared.act([id], .read)
        } catch {
            let content = UNMutableNotificationContent()
            content.title = "快速回复失败"
            content.body = error.localizedDescription
            try? await UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
        }
    }
}
