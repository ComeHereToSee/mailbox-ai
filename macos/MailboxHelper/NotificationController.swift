import AppKit
import UserNotifications

/// System notifications for new mail: "[账户] 发件人", subject, snippet, with quick actions.
final class NotificationController: NSObject, UNUserNotificationCenterDelegate {
    private let center = UNUserNotificationCenter.current()
    private static let category = "NEW_MAIL"

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
        center.setNotificationCategories([cat])
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

    func postSendFailure(_ error: String) {
        let content = UNMutableNotificationContent()
        content.title = "邮件发送失败"
        content.body = error
        content.sound = .default
        center.add(UNNotificationRequest(identifier: "send-failed-\(UUID().uuidString)", content: content, trigger: nil))
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .list, .sound])
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let info = response.notification.request.content.userInfo
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
