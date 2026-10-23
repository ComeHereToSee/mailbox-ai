import AVFoundation
import SwiftUI

/// Read-aloud with AVSpeechSynthesizer (docs/AI.md §7 代读). Chinese voices are preferred.
@MainActor
final class Speech: NSObject, ObservableObject, AVSpeechSynthesizerDelegate {
    static let shared = Speech()

    @Published private(set) var speaking = false
    private let synth = AVSpeechSynthesizer()

    override init() {
        super.init()
        synth.delegate = self
    }

    struct Voice: Hashable, Identifiable {
        var id: String
        var name: String
        var language: String
        var enhanced: Bool
        var label: String { "\(name)（\(Self.languageName(language))\(enhanced ? "，增强" : "")）" }

        static func languageName(_ code: String) -> String {
            switch code {
            case "zh-CN": return "普通话"
            case "zh-TW": return "台湾国语"
            case "zh-HK": return "粤语"
            case "en-US": return "美式英语"
            case "en-GB": return "英式英语"
            case "ja-JP": return "日语"
            default: return code
            }
        }
    }

    /// System voices: Chinese first (zh-CN, then other zh), then English, then the rest.
    static var voices: [Voice] {
        let all = AVSpeechSynthesisVoice.speechVoices().map {
            Voice(id: $0.identifier, name: $0.name, language: $0.language, enhanced: $0.quality != .default)
        }
        func rank(_ v: Voice) -> Int {
            if v.language == "zh-CN" { return 0 }
            if v.language.hasPrefix("zh") { return 1 }
            if v.language.hasPrefix("en") { return 2 }
            return 3
        }
        return all.sorted { a, b in
            let ra = rank(a), rb = rank(b)
            if ra != rb { return ra < rb }
            if a.enhanced != b.enhanced { return a.enhanced }
            return a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
    }

    func speak(_ text: String, settings: AISettings) {
        speak(text, voice: settings.ttsVoice, rate: settings.ttsRate)
    }

    func speak(_ text: String, voice: String?, rate: Double) {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return }
        stop()
        let u = AVSpeechUtterance(string: t)
        u.voice = Self.resolveVoice(voice, for: t)
        let r = Float(max(0, min(1, rate)))
        u.rate = AVSpeechUtteranceMinimumSpeechRate + (AVSpeechUtteranceMaximumSpeechRate - AVSpeechUtteranceMinimumSpeechRate) * r
        speaking = true
        synth.speak(u)
    }

    func stop() {
        if synth.isSpeaking { synth.stopSpeaking(at: .immediate) }
        speaking = false
    }

    private static func resolveVoice(_ id: String?, for text: String) -> AVSpeechSynthesisVoice? {
        if let id, !id.isEmpty, let v = AVSpeechSynthesisVoice(identifier: id) { return v }
        let cjk = text.unicodeScalars.contains { $0.value >= 0x4E00 && $0.value <= 0x9FFF }
        if !cjk, let en = AVSpeechSynthesisVoice(language: "en-US") { return en }
        return AVSpeechSynthesisVoice(language: "zh-CN") ?? AVSpeechSynthesisVoice(language: "zh-TW")
    }

    nonisolated func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didFinish utterance: AVSpeechUtterance) {
        Task { @MainActor in self.speaking = self.synth.isSpeaking }
    }

    nonisolated func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didCancel utterance: AVSpeechUtterance) {
        Task { @MainActor in self.speaking = self.synth.isSpeaking }
    }
}
