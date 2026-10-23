import AppKit

/// Menu bar extra: unread count and a mini inbox.
final class StatusItemController: NSObject, NSMenuDelegate {
    private var item: NSStatusItem?
    private var snapshot: MenuSnapshot?
    private var statuses: [String: AccountStatus] = [:]
    private var engineError: String?

    var unread: Int = 0 {
        didSet { updateButton() }
    }

    func setup() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        let image = NSImage(systemSymbolName: "envelope", accessibilityDescription: "Mailbox AI")
        image?.isTemplate = true
        item.button?.image = image
        item.button?.imagePosition = .imageLeading
        let menu = NSMenu()
        menu.delegate = self
        menu.autoenablesItems = false
        item.menu = menu
        self.item = item
        updateButton()
    }

    func showError(_ message: String) {
        engineError = message
        item?.button?.image = NSImage(systemSymbolName: "envelope.badge.shield.half.filled", accessibilityDescription: "Mailbox AI")
        item?.button?.toolTip = message
    }

    func updateStatus(_ status: AccountStatus) {
        statuses[status.accountId] = status
        let failing = statuses.values.contains { $0.state == .error }
        let name = failing ? "envelope.badge.shield.half.filled" : (unread > 0 ? "envelope.badge" : "envelope")
        let image = NSImage(systemSymbolName: name, accessibilityDescription: "Mailbox AI")
        image?.isTemplate = true
        item?.button?.image = image
    }

    private func updateButton() {
        item?.button?.title = unread > 0 ? " \(unread)" : ""
        item?.button?.toolTip = unread > 0 ? "Mailbox AI：\(unread) 封未读" : "Mailbox AI"
    }

    @MainActor
    func refresh() async {
        guard let snap = try? await APIClient.shared.menu(limit: 10) else { return }
        snapshot = snap
        unread = snap.unread
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        if let err = engineError {
            menu.addItem(disabled("邮件引擎未运行：\(err)"))
            menu.addItem(.separator())
        }
        let snap = snapshot
        let header = (snap?.unread ?? 0) > 0 ? "\(snap?.unread ?? 0) 封未读邮件" : "没有未读邮件"
        menu.addItem(disabled(header))
        for entry in snap?.items ?? [] {
            let m = entry.message
            let title = "[\(entry.accountLabel)] \(m.from.display) — \(m.subject.isEmpty ? "（无主题）" : m.subject)"
            let mi = NSMenuItem(title: String(title.prefix(80)), action: #selector(openMessage(_:)), keyEquivalent: "")
            mi.target = self
            mi.representedObject = NSNumber(value: m.id)
            mi.toolTip = m.snippet
            mi.image = colorDot(entry.accountColor)
            menu.addItem(mi)
        }
        menu.addItem(.separator())
        menu.addItem(action("写新邮件", #selector(compose), "n"))
        menu.addItem(action("检查新邮件", #selector(syncNow), "r"))
        if (snap?.unread ?? 0) > 0 {
            menu.addItem(action("全部标为已读", #selector(markAllRead), ""))
        }
        let pause = NSMenuItem(title: "通知", action: nil, keyEquivalent: "")
        let sub = NSMenu()
        if let until = snap?.settings.notificationsPausedUntil, until > Int64(Date().timeIntervalSince1970) {
            sub.addItem(disabled("已暂停至 \(Fmt.timeOnly.string(from: Date(timeIntervalSince1970: TimeInterval(until))))"))
            sub.addItem(action("恢复通知", #selector(resumeNotifications), ""))
        } else {
            for (title, minutes) in [("暂停 30 分钟", 30), ("暂停 1 小时", 60), ("暂停 4 小时", 240), ("暂停到明天早上", minutesUntilTomorrowMorning())] {
                let mi = action(title, #selector(pauseNotifications(_:)), "")
                mi.representedObject = NSNumber(value: minutes)
                sub.addItem(mi)
            }
        }
        pause.submenu = sub
        menu.addItem(pause)
        menu.addItem(.separator())
        menu.addItem(action("打开 Mailbox AI", #selector(openApp), "o"))
        menu.addItem(.separator())
        menu.addItem(action("退出（停止收信）", #selector(quit), "q"))
    }

    private func minutesUntilTomorrowMorning() -> Int {
        let cal = Calendar.current
        let tomorrow = cal.date(byAdding: .day, value: 1, to: Date()) ?? Date()
        let eight = cal.date(bySettingHour: 8, minute: 0, second: 0, of: tomorrow) ?? tomorrow
        return max(1, Int(eight.timeIntervalSinceNow / 60))
    }

    private func disabled(_ title: String) -> NSMenuItem {
        let mi = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        mi.isEnabled = false
        return mi
    }

    private func action(_ title: String, _ sel: Selector, _ key: String) -> NSMenuItem {
        let mi = NSMenuItem(title: title, action: sel, keyEquivalent: key)
        mi.target = self
        return mi
    }

    private func colorDot(_ hex: String) -> NSImage {
        let img = NSImage(size: NSSize(width: 10, height: 10))
        img.lockFocus()
        (NSColor(hex: hex) ?? .systemBlue).setFill()
        NSBezierPath(ovalIn: NSRect(x: 1, y: 1, width: 8, height: 8)).fill()
        img.unlockFocus()
        return img
    }

    @objc private func openMessage(_ sender: NSMenuItem) {
        guard let id = (sender.representedObject as? NSNumber)?.int64Value, let url = URL(string: "mailboxai://message/\(id)") else { return }
        Daemon.openMainApp(url: url)
    }

    @objc private func compose() {
        if let url = URL(string: "mailboxai://compose") { Daemon.openMainApp(url: url) }
    }

    @objc private func openApp() {
        Daemon.openMainApp()
    }

    @objc private func syncNow() {
        Task { try? await APIClient.shared.syncAccount(nil) }
    }

    @objc private func markAllRead() {
        Task {
            try? await APIClient.shared.markAllRead(mailbox: "unified:inbox")
            await refresh()
        }
    }

    @objc private func pauseNotifications(_ sender: NSMenuItem) {
        let minutes = (sender.representedObject as? NSNumber)?.intValue ?? 60
        Task {
            _ = try? await APIClient.shared.pauseNotifications(minutes: minutes)
            await refresh()
        }
    }

    @objc private func resumeNotifications() {
        Task {
            _ = try? await APIClient.shared.pauseNotifications(minutes: 0)
            await refresh()
        }
    }

    @objc private func quit() {
        NSApp.terminate(nil)
    }
}
