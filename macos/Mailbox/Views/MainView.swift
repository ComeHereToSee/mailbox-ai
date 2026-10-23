import CoreSpotlight
import SwiftUI

struct MainView: View {
    @EnvironmentObject private var state: AppState
    @ObservedObject private var ai = AIState.shared
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings
    @State private var columns = NavigationSplitViewVisibility.all

    var body: some View {
        Group {
            switch state.connection {
            case .connecting:
                VStack(spacing: 12) {
                    ProgressView()
                    Text("正在启动邮件服务…").foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .failed(let message):
                ContentUnavailableView {
                    Label("邮件服务未运行", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(message)
                } actions: {
                    Button("重试") { state.retryConnection() }
                }
            case .ready:
                split
            }
        }
        .overlay(alignment: .bottom) { ToastView().padding(.bottom, 16) }
        .overlay { if state.locked { LockView() } }
        .onAppear {
            WindowRouter.shared.openWindow = openWindow
            WindowRouter.shared.openSettings = openSettings
            if WindowRouter.shared.pendingSettings {
                WindowRouter.shared.pendingSettings = false
                openSettings()
            }
            if WindowRouter.shared.pendingAssistant {
                WindowRouter.shared.pendingAssistant = false
                openWindow(id: "assistant")
            }
        }
        .onContinueUserActivity(CSSearchableItemActionType) { activity in
            if let id = SpotlightIndexer.messageId(from: activity) {
                Task { await state.open(messageId: id) }
            }
        }
        .onChange(of: state.pendingCompose) { _, req in
            if let req {
                state.pendingCompose = nil
                openWindow(id: "compose", value: req)
            }
        }
        .sheet(isPresented: $state.showOnboarding) {
            OnboardingView().environmentObject(state)
        }
        .sheet(isPresented: $state.showAddAccount) {
            AddAccountView().environmentObject(state)
        }
        .sheet(isPresented: Binding(get: { !ai.batchIds.isEmpty }, set: { if !$0 { ai.batchIds = [] } })) {
            AIBatchSheet(ids: ai.batchIds).environmentObject(state)
        }
    }

    @ViewBuilder
    private var split: some View {
        if state.selectedMailbox.hasPrefix("web:") {
            NavigationSplitView {
                SidebarView()
                    .navigationSplitViewColumnWidth(min: 200, ideal: 240, max: 340)
            } detail: {
                WebAccountDetail(accountId: String(state.selectedMailbox.dropFirst(4)))
            }
        } else {
            threeColumns
        }
    }

    private var threeColumns: some View {
        NavigationSplitView(columnVisibility: $columns) {
            SidebarView()
                .navigationSplitViewColumnWidth(min: 200, ideal: 240, max: 340)
        } content: {
            MessageListView()
                .navigationSplitViewColumnWidth(min: 300, ideal: 390, max: 620)
        } detail: {
            ReaderView()
        }
    }
}

struct ToastView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        if let t = state.toast {
            HStack(spacing: 12) {
                Image(systemName: t.isError ? "exclamationmark.circle.fill" : "checkmark.circle.fill")
                    .foregroundStyle(t.isError ? .red : .green)
                Text(t.text).lineLimit(2)
                if t.undoToken != nil || t.outboxId != nil {
                    Button("撤销") { Task { await state.undoLast() } }
                        .buttonStyle(.borderless)
                        .keyboardShortcut("z", modifiers: .command)
                }
                Button {
                    state.toast = nil
                } label: {
                    Image(systemName: "xmark").font(.caption)
                }
                .buttonStyle(.borderless)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .background(.regularMaterial, in: Capsule())
            .shadow(radius: 8, y: 2)
            .transition(.move(edge: .bottom).combined(with: .opacity))
            .animation(.spring(duration: 0.25), value: t.id)
        }
    }
}
