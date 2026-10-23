import AppKit
import SwiftUI

@main
struct MailboxApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var state = AppState.shared

    var body: some Scene {
        Window("Mailbox AI", id: "main") {
            MainView()
                .environmentObject(state)
                .frame(minWidth: 900, minHeight: 560)
        }
        .defaultSize(width: 1280, height: 800)
        .commands { MailboxCommands(state: state) }

        WindowGroup("写邮件", id: "compose", for: ComposeRequest.self) { $request in
            ComposeWindow(request: request ?? ComposeRequest())
                .environmentObject(state)
        }
        .defaultSize(width: 760, height: 640)

        SwiftUI.Settings {
            SettingsView()
                .environmentObject(state)
        }
    }
}

/// Lets non-view code open SwiftUI windows.
@MainActor
final class WindowRouter {
    static let shared = WindowRouter()
    var openWindow: OpenWindowAction?
    var openSettings: OpenSettingsAction?

    func compose(_ request: ComposeRequest) {
        if let open = openWindow {
            open(id: "compose", value: request)
        } else {
            AppState.shared.pendingCompose = request
        }
        NSApp.activate(ignoringOtherApps: true)
    }

    /// macOS 14+ ignores a programmatic `showSettingsWindow:`; the SwiftUI action is the supported way.
    func showSettings() {
        NSApp.activate(ignoringOtherApps: true)
        if let open = openSettings {
            open()
        } else {
            pendingSettings = true
        }
    }

    /// Set when a URL asked for Settings before the main window captured the action.
    var pendingSettings = false

    func showMain() {
        openWindow?(id: "main")
        NSApp.activate(ignoringOtherApps: true)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private let services = ServicesProvider()

    func applicationWillFinishLaunching(_ notification: Notification) {
        NSAppleEventManager.shared().setEventHandler(self, andSelector: #selector(handleURL(_:withReply:)),
                                                     forEventClass: AEEventClass(kInternetEventClass), andEventID: AEEventID(kAEGetURL))
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.servicesProvider = services
        NSUpdateDynamicServices()
        Task { @MainActor in await AppState.shared.bootstrap() }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag {
            Task { @MainActor in WindowRouter.shared.showMain() }
        }
        return true
    }

    func applicationDockMenu(_ sender: NSApplication) -> NSMenu? {
        let menu = NSMenu()
        let compose = NSMenuItem(title: "写新邮件", action: #selector(composeFromDock), keyEquivalent: "")
        compose.target = self
        menu.addItem(compose)
        let sync = NSMenuItem(title: "检查新邮件", action: #selector(syncFromDock), keyEquivalent: "")
        sync.target = self
        menu.addItem(sync)
        return menu
    }

    @objc private func composeFromDock() {
        Task { @MainActor in WindowRouter.shared.compose(ComposeRequest()) }
    }

    @objc private func syncFromDock() {
        Task { try? await APIClient.shared.syncAccount(nil) }
    }

    @objc private func handleURL(_ event: NSAppleEventDescriptor, withReply reply: NSAppleEventDescriptor) {
        guard let s = event.paramDescriptor(forKeyword: AEKeyword(keyDirectObject))?.stringValue, let url = URL(string: s) else { return }
        Task { @MainActor in await AppState.shared.handle(url: url) }
    }
}
