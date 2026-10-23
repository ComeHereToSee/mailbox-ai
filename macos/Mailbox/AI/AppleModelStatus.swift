import SwiftUI
#if canImport(FoundationModels)
import FoundationModels
#endif

/// Availability of Apple's on-device model, shown on the "Apple 端侧模型" provider page.
/// The model itself is served by the helper app on 127.0.0.1:47922 (AppleModelServer).
struct AppleModelStatusSection: View {
    var body: some View {
        Section("Apple 端侧模型") {
            let (ok, text) = Self.status()
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: ok ? "checkmark.circle.fill" : "exclamationmark.circle.fill")
                    .foregroundStyle(ok ? .green : .orange)
                Text(text).font(.callout)
            }
            Text("由 Mailbox AI 助手在本机提供（127.0.0.1:47922），不需要 API Key，邮件内容不会离开这台 Mac。端侧模型的上下文较短，适合摘要、分类和短邮件改写，较长的邮件会被截断。")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    static func status() -> (Bool, String) {
        #if canImport(FoundationModels)
        if #available(macOS 26.0, *) {
            switch SystemLanguageModel.default.availability {
            case .available:
                return (true, "可用")
            case .unavailable(.deviceNotEligible):
                return (false, "这台 Mac 不支持 Apple Intelligence")
            case .unavailable(.appleIntelligenceNotEnabled):
                return (false, "请先在 系统设置 → Apple Intelligence 与 Siri 中打开 Apple Intelligence")
            case .unavailable(.modelNotReady):
                return (false, "模型还在下载或准备中，请稍后再试")
            case .unavailable:
                return (false, "暂不可用")
            }
        }
        return (false, "需要 macOS 26 及以上")
        #else
        return (false, "此版本在编译时没有包含 Apple 端侧模型支持（需要用 Xcode 26 编译）")
        #endif
    }
}
