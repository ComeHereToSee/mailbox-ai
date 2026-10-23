import Foundation

// Mirrors core/crates/mailcore/src/ai/model.rs (docs/AI.md §2–§4). JSON keys are snake_case and
// converted by `JSON.decoder` / `JSON.encoder`. Decoding is lenient: missing or mistyped fields
// fall back to defaults so a newer / older engine never breaks the UI.

extension KeyedDecodingContainer {
    /// Decodes `key`, falling back when it is missing, null or of another type.
    func lenient<T: Decodable>(_ type: T.Type, _ key: Key, _ fallback: T) -> T {
        (try? decodeIfPresent(type, forKey: key)) ?? fallback
    }

    /// A string, or the display form of an `EmailAddress` object.
    func addressText(_ key: Key) -> String {
        if let s = try? decodeIfPresent(String.self, forKey: key) { return s }
        if let a = try? decodeIfPresent(EmailAddress.self, forKey: key) { return a.display }
        return ""
    }
}

enum AIProviderKind: String, LenientEnum, CaseIterable, Identifiable {
    case openai, anthropic, gemini, azureOpenai = "azure_openai", claudeCode = "claude_code", codex
    static var fallback: AIProviderKind { .openai }
    var id: String { rawValue }

    var isCLI: Bool { self == .claudeCode || self == .codex }

    var title: String {
        switch self {
        case .openai: return "OpenAI 兼容"
        case .anthropic: return "Anthropic"
        case .gemini: return "Google Gemini"
        case .azureOpenai: return "Azure OpenAI"
        case .claudeCode: return "Claude Code（命令行）"
        case .codex: return "Codex（命令行）"
        }
    }
}

struct AIProviderPreset: Codable, Hashable, Identifiable {
    var id: String = ""
    var kind: AIProviderKind = .openai
    var name: String = ""
    var baseUrl: String = ""
    var website: String = ""
    var keyUrl: String?
    var needsKey: Bool = true
    var color: String = ""
    var models: [String] = []
    var description: String = ""

    private enum CodingKeys: String, CodingKey { case id, kind, name, baseUrl, website, keyUrl, needsKey, color, models, description }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        kind = c.lenient(AIProviderKind.self, .kind, .openai)
        name = c.lenient(String.self, .name, "")
        baseUrl = c.lenient(String.self, .baseUrl, "")
        website = c.lenient(String.self, .website, "")
        keyUrl = try? c.decodeIfPresent(String.self, forKey: .keyUrl)
        needsKey = c.lenient(Bool.self, .needsKey, true)
        color = c.lenient(String.self, .color, "")
        models = c.lenient([String].self, .models, [])
        description = c.lenient(String.self, .description, "")
    }
}

struct AIModel: Codable, Hashable, Identifiable {
    var id: String = ""
    var name: String = ""
    var group: String = ""
    var enabled: Bool = true
    /// "vision" | "reasoning" | "tools" | "embedding" (§8.4); inferred by the engine, editable in the UI.
    var capabilities: [String] = []

    private enum CodingKeys: String, CodingKey { case id, name, group, enabled, capabilities }

    init(id: String, name: String = "", group: String = "", enabled: Bool = true, capabilities: [String] = []) {
        self.id = id
        self.name = name.isEmpty ? id : name
        self.group = group
        self.enabled = enabled
        self.capabilities = capabilities
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        name = c.lenient(String.self, .name, "")
        if name.isEmpty { name = id }
        group = c.lenient(String.self, .group, "")
        enabled = c.lenient(Bool.self, .enabled, true)
        capabilities = c.lenient([String].self, .capabilities, [])
    }

    static let capabilityOrder = ["vision", "reasoning", "tools", "embedding"]

    static func capabilityTitle(_ c: String) -> String {
        switch c {
        case "vision": return "看图"
        case "reasoning": return "推理"
        case "tools": return "工具"
        case "embedding": return "嵌入"
        default: return c
        }
    }

    static func capabilitySymbol(_ c: String) -> String {
        switch c {
        case "vision": return "eye"
        case "reasoning": return "brain"
        case "tools": return "wrench.and.screwdriver"
        case "embedding": return "point.3.connected.trianglepath.dotted"
        default: return "tag"
        }
    }

    func has(_ capability: String) -> Bool { capabilities.contains(capability) }

    /// Same rules as the engine (docs/AI.md §8.4), used for models added by hand.
    static func inferCapabilities(_ id: String) -> [String] {
        let s = id.lowercased()
        func hasAny(_ parts: [String]) -> Bool { parts.contains { s.contains($0) } }
        if s.contains("embed") { return ["embedding"] }
        var caps: [String] = []
        if hasAny(["vision", "vl", "gpt-4o", "gpt-4.1", "gpt-5", "claude", "gemini", "glm-4v", "qwen-vl", "omni", "pixtral", "llava"]) {
            caps.append("vision")
        }
        if hasAny(["reason", "r1", "o1", "o3", "o4", "thinking", "qwq"]) { caps.append("reasoning") }
        if hasAny(["gpt", "claude", "gemini", "qwen", "deepseek-chat", "deepseek-v", "glm", "kimi", "moonshot", "mistral", "grok"]) {
            caps.append("tools")
        }
        return caps
    }

    /// Group used when the engine left `group` empty: the part before the first "-" or "/".
    var displayGroup: String {
        if !group.isEmpty { return group }
        if let i = id.firstIndex(where: { $0 == "-" || $0 == "/" }) { return String(id[..<i]) }
        return id
    }
}

struct AIProvider: Codable, Hashable, Identifiable {
    var id: String = ""
    var kind: AIProviderKind = .openai
    var name: String = ""
    var baseUrl: String = ""
    var apiVersion: String?
    var enabled: Bool = false
    var models: [AIModel] = []
    var hasKey: Bool = false
    var custom: Bool = false
    /// Number of keys stored (comma / newline separated, used in turn; output only, §8.4).
    var keyCount: Int = 0

    private enum CodingKeys: String, CodingKey { case id, kind, name, baseUrl, apiVersion, enabled, models, hasKey, custom, keyCount }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        kind = c.lenient(AIProviderKind.self, .kind, .openai)
        name = c.lenient(String.self, .name, "")
        baseUrl = c.lenient(String.self, .baseUrl, "")
        apiVersion = try? c.decodeIfPresent(String.self, forKey: .apiVersion)
        enabled = c.lenient(Bool.self, .enabled, false)
        models = c.lenient([AIModel].self, .models, [])
        hasKey = c.lenient(Bool.self, .hasKey, false)
        custom = c.lenient(Bool.self, .custom, false)
        keyCount = c.lenient(Int.self, .keyCount, hasKey ? 1 : 0)
    }

    var isCLI: Bool { kind.isCLI }
}

/// Body of `PUT /v1/ai/providers/{id}`: the provider plus an optional key
/// (nil = unchanged, "" = delete, otherwise save).
struct AIProviderInput: Encodable {
    var id: String
    var kind: AIProviderKind
    var name: String
    var baseUrl: String
    var apiVersion: String?
    var enabled: Bool
    var models: [AIModel]
    var hasKey: Bool
    var custom: Bool
    var apiKey: String?

    init(_ p: AIProvider, apiKey: String?) {
        id = p.id
        kind = p.kind
        name = p.name
        baseUrl = p.baseUrl
        apiVersion = p.apiVersion
        enabled = p.enabled
        models = p.models
        hasKey = p.hasKey
        custom = p.custom
        self.apiKey = apiKey
    }
}

struct AIModelRef: Codable, Hashable {
    var provider: String
    var model: String

    var key: String { "\(provider)/\(model)" }
}

struct AISettings: Codable, Hashable {
    var defaultModel: AIModelRef?
    var quickModel: AIModelRef?
    var language: String = "zh-CN"
    var tone: String = "professional"
    var signatureName: String = ""
    var smartReminders: Bool = true
    var smartScope: String = "primary"
    var autoSummary: Bool = false
    var allowEncrypted: Bool = false
    var maxBodyChars: Int = 12000
    var ttsVoice: String?
    var ttsRate: Double = 0.5
    // §8 additions
    /// Built-in MCP server (`/mcp`), off by default.
    var mcpEnabled: Bool = false
    /// Reads image attachments; nil = default model when it can see images.
    var visionModel: AIModelRef?
    /// Semantic re-ranking for "问问邮箱".
    var embeddingModel: AIModelRef?
    /// account id → "on" | "off" | "local_only" (missing = on).
    var accountPolicies: [String: String] = [:]
    /// "normal" | "annotate" | "important"
    var notifyMode: String = "normal"
    /// Remind about sent mail without a reply after this many days; 0 = off.
    var followupDays: Int = 3

    private enum CodingKeys: String, CodingKey {
        case defaultModel, quickModel, language, tone, signatureName, smartReminders, smartScope, autoSummary
        case allowEncrypted, maxBodyChars, ttsVoice, ttsRate
        case mcpEnabled, visionModel, embeddingModel, accountPolicies, notifyMode, followupDays
    }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        defaultModel = try? c.decodeIfPresent(AIModelRef.self, forKey: .defaultModel)
        quickModel = try? c.decodeIfPresent(AIModelRef.self, forKey: .quickModel)
        language = c.lenient(String.self, .language, "zh-CN")
        tone = c.lenient(String.self, .tone, "professional")
        signatureName = c.lenient(String.self, .signatureName, "")
        smartReminders = c.lenient(Bool.self, .smartReminders, true)
        smartScope = c.lenient(String.self, .smartScope, "primary")
        autoSummary = c.lenient(Bool.self, .autoSummary, false)
        allowEncrypted = c.lenient(Bool.self, .allowEncrypted, false)
        maxBodyChars = c.lenient(Int.self, .maxBodyChars, 12000)
        ttsVoice = try? c.decodeIfPresent(String.self, forKey: .ttsVoice)
        ttsRate = c.lenient(Double.self, .ttsRate, 0.5)
        mcpEnabled = c.lenient(Bool.self, .mcpEnabled, false)
        visionModel = try? c.decodeIfPresent(AIModelRef.self, forKey: .visionModel)
        embeddingModel = try? c.decodeIfPresent(AIModelRef.self, forKey: .embeddingModel)
        accountPolicies = c.lenient([String: String].self, .accountPolicies, [:])
        notifyMode = c.lenient(String.self, .notifyMode, "normal")
        followupDays = c.lenient(Int.self, .followupDays, 3)
    }

    /// "on" | "off" | "local_only" for an account.
    func policy(for accountId: String) -> String { accountPolicies[accountId] ?? "on" }

    var isConfigured: Bool { defaultModel != nil || quickModel != nil }
}

struct AICLIStatus: Codable, Hashable {
    var found: Bool = false
    var path: String?
    var version: String?
    var loggedIn: Bool?
    var hint: String = ""

    private enum CodingKeys: String, CodingKey { case found, path, version, loggedIn, hint }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        found = c.lenient(Bool.self, .found, false)
        path = try? c.decodeIfPresent(String.self, forKey: .path)
        version = try? c.decodeIfPresent(String.self, forKey: .version)
        loggedIn = try? c.decodeIfPresent(Bool.self, forKey: .loggedIn)
        hint = c.lenient(String.self, .hint, "")
    }
}

/// `GET /v1/ai/cli`.
struct AICLIReport: Codable, Hashable {
    var claude: AICLIStatus = AICLIStatus()
    var codex: AICLIStatus = AICLIStatus()

    private enum CodingKeys: String, CodingKey { case claude, codex }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        claude = c.lenient(AICLIStatus.self, .claude, AICLIStatus())
        codex = c.lenient(AICLIStatus.self, .codex, AICLIStatus())
    }
}

/// `POST /v1/ai/providers/{id}/test`.
struct AIProviderTest: Decodable, Hashable {
    var ok: Bool
    var latencyMs: Int?
    var reply: String?
    var error: String?

    private enum CodingKeys: String, CodingKey { case ok, latencyMs, reply, error }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        ok = c.lenient(Bool.self, .ok, false)
        latencyMs = try? c.decodeIfPresent(Int.self, forKey: .latencyMs)
        reply = try? c.decodeIfPresent(String.self, forKey: .reply)
        error = try? c.decodeIfPresent(String.self, forKey: .error)
    }
}

struct AIChatMessage: Codable, Hashable {
    var role: String
    var content: String
}

/// A source cited by `/v1/ai/ask` (`done.sources`).
struct AISource: Decodable, Hashable, Identifiable {
    var index: Int
    var messageId: Int64
    var subject: String
    var from: String
    var date: Int64?

    var id: Int { index }

    private enum CodingKeys: String, CodingKey { case index, messageId, subject, from, date }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        index = c.lenient(Int.self, .index, 0)
        messageId = c.lenient(Int64.self, .messageId, 0)
        subject = c.lenient(String.self, .subject, "")
        from = c.addressText(.from)
        date = try? c.decodeIfPresent(Int64.self, forKey: .date)
    }
}

/// One line of an NDJSON stream: `{"delta"}`, `{"done":true,"text",…}` or `{"error"}`.
struct AIStreamLine: Decodable {
    var delta: String?
    var done: Bool?
    var text: String?
    var model: String?
    var error: String?
    var subject: String?
    var cached: Bool?
    var sources: [AISource]?
    /// Tools called during a chat / ask with `tools: true` (§8.3).
    var toolCalls: [AIToolCall]?
    /// E.g. "Codex 暂不支持外部工具".
    var toolsNote: String?

    private enum CodingKeys: String, CodingKey { case delta, done, text, model, error, subject, cached, sources, toolCalls, toolsNote }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        delta = try? c.decodeIfPresent(String.self, forKey: .delta)
        done = try? c.decodeIfPresent(Bool.self, forKey: .done)
        text = try? c.decodeIfPresent(String.self, forKey: .text)
        model = try? c.decodeIfPresent(String.self, forKey: .model)
        error = try? c.decodeIfPresent(String.self, forKey: .error)
        subject = try? c.decodeIfPresent(String.self, forKey: .subject)
        cached = try? c.decodeIfPresent(Bool.self, forKey: .cached)
        sources = try? c.decodeIfPresent([AISource].self, forKey: .sources)
        toolCalls = (try? c.decodeIfPresent([Lossy<AIToolCall>].self, forKey: .toolCalls))?.compactMap(\.value)
        toolsNote = try? c.decodeIfPresent(String.self, forKey: .toolsNote)
    }
}

/// `GET /v1/ai/messages/{id}/summary`.
struct AISummaryCache: Decodable, Hashable {
    var text: String
    var model: String
    var createdAt: Int64

    private enum CodingKeys: String, CodingKey { case text, model, createdAt }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        text = c.lenient(String.self, .text, "")
        model = c.lenient(String.self, .model, "")
        createdAt = c.lenient(Int64.self, .createdAt, 0)
    }
}

struct AIReplySuggestion: Codable, Hashable, Identifiable {
    var label: String
    var text: String

    var id: String { label + "\u{1}" + text }
}

/// Smart-reminder analysis of one message (`Insight`).
struct AIInsight: Codable, Hashable {
    var messageId: Int64 = 0
    var needsReply: Bool = false
    var priority: String = "normal"
    var deadline: Int64?
    var deadlineText: String?
    var todos: [String] = []
    var oneLine: String = ""
    var remindAt: Int64?
    var done: Bool = false
    var createdAt: Int64 = 0
    /// Calendar events found during the analysis (§8.1).
    var events: [AIExtractedEvent] = []
    /// "phishing" | "scam" | nil (§8.7).
    var risk: String?
    var riskReason: String = ""

    private enum CodingKeys: String, CodingKey {
        case messageId, needsReply, priority, deadline, deadlineText, todos, oneLine, remindAt, done, createdAt
        case events, risk, riskReason
    }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        messageId = c.lenient(Int64.self, .messageId, 0)
        needsReply = c.lenient(Bool.self, .needsReply, false)
        priority = c.lenient(String.self, .priority, "normal")
        deadline = try? c.decodeIfPresent(Int64.self, forKey: .deadline)
        deadlineText = try? c.decodeIfPresent(String.self, forKey: .deadlineText)
        todos = c.lenient([String].self, .todos, [])
        oneLine = c.lenient(String.self, .oneLine, "")
        remindAt = try? c.decodeIfPresent(Int64.self, forKey: .remindAt)
        done = c.lenient(Bool.self, .done, false)
        createdAt = c.lenient(Int64.self, .createdAt, 0)
        events = (c.lenient([Lossy<AIExtractedEvent>].self, .events, [])).compactMap(\.value)
        risk = (try? c.decodeIfPresent(String.self, forKey: .risk)).flatMap { $0.isEmpty ? nil : $0 }
        riskReason = c.lenient(String.self, .riskReason, "")
    }

    /// Worth surfacing in the reader / pending list.
    var isActionable: Bool { needsReply || deadline != nil || !todos.isEmpty || priority == "high" }

    /// "疑似钓鱼" / "疑似诈骗", or nil without a risk.
    var riskTitle: String? {
        guard let risk else { return nil }
        switch risk {
        case "phishing": return "疑似钓鱼"
        case "scam": return "疑似诈骗"
        default: return "存在风险"
        }
    }
}

/// `GET /v1/ai/reminders` entry.
struct AIReminderItem: Decodable, Hashable, Identifiable {
    var message: MessageSummary
    var accountLabel: String
    var insight: AIInsight
    /// "insight" | "follow_up" (a sent message still waiting for a reply, §8.7).
    var kind: String

    var id: Int64 { message.id }
    var isFollowUp: Bool { kind == "follow_up" }

    private enum CodingKeys: String, CodingKey { case message, accountLabel, insight, kind }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        message = try c.decode(MessageSummary.self, forKey: .message)
        accountLabel = c.lenient(String.self, .accountLabel, "")
        insight = c.lenient(AIInsight.self, .insight, AIInsight())
        kind = c.lenient(String.self, .kind, "insight")
    }
}

struct AISuggestedAction: Codable, Hashable, Identifiable {
    var action: String = ""
    var value: String?
    var reason: String = ""

    var id: String { action + ":" + (value ?? "") }

    private enum CodingKeys: String, CodingKey { case action, value, reason }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        action = c.lenient(String.self, .action, "")
        value = try? c.decodeIfPresent(String.self, forKey: .value)
        reason = c.lenient(String.self, .reason, "")
    }

    var title: String {
        switch action {
        case "archive": return "归档"
        case "trash": return "删除"
        case "star": return "加星标"
        case "read": return "标为已读"
        case "label": return "标签：\(value ?? "")"
        case "move": return "移动到 \(value ?? "")"
        case "category": return "分类：\(Category(rawValue: value ?? "")?.title ?? (value ?? ""))"
        default: return action
        }
    }
}

struct AIBatchItem: Decodable, Hashable, Identifiable {
    var messageId: Int64
    var subject: String
    var from: String
    var output: String
    var error: String?
    var suggested: [AISuggestedAction]
    var draftId: String?

    var id: Int64 { messageId }

    private enum CodingKeys: String, CodingKey { case messageId, subject, from, output, error, suggested, draftId }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        messageId = c.lenient(Int64.self, .messageId, 0)
        subject = c.lenient(String.self, .subject, "")
        from = c.addressText(.from)
        output = c.lenient(String.self, .output, "")
        error = try? c.decodeIfPresent(String.self, forKey: .error)
        suggested = c.lenient([AISuggestedAction].self, .suggested, [])
        draftId = try? c.decodeIfPresent(String.self, forKey: .draftId)
    }
}

struct AIBatchJob: Decodable, Hashable, Identifiable {
    var id: String
    var op: String
    var status: String
    var total: Int
    var done: Int
    var items: [AIBatchItem]
    var summary: String?
    var createdAt: Int64

    private enum CodingKeys: String, CodingKey { case id, op, status, total, done, items, summary, createdAt }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        op = c.lenient(String.self, .op, "")
        status = c.lenient(String.self, .status, "running")
        total = c.lenient(Int.self, .total, 0)
        done = c.lenient(Int.self, .done, 0)
        items = c.lenient([AIBatchItem].self, .items, [])
        summary = try? c.decodeIfPresent(String.self, forKey: .summary)
        createdAt = c.lenient(Int64.self, .createdAt, 0)
    }

    var isRunning: Bool { status == "running" }
}

struct AILoopSchedule: Codable, Hashable {
    var kind: String = "daily"
    var time: String = "08:00"
    var weekday: Int?
    var everyMinutes: Int?

    private enum CodingKeys: String, CodingKey { case kind, time, weekday, everyMinutes }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = c.lenient(String.self, .kind, "daily")
        time = c.lenient(String.self, .time, "08:00")
        weekday = try? c.decodeIfPresent(Int.self, forKey: .weekday)
        everyMinutes = try? c.decodeIfPresent(Int.self, forKey: .everyMinutes)
    }

    static let weekdayNames = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"]

    var summary: String {
        switch kind {
        case "weekdays": return "工作日 \(time)"
        case "weekly":
            let w = weekday ?? 1
            let name = (1...7).contains(w) ? Self.weekdayNames[w - 1] : Self.weekdayNames[0]
            return "每\(name) \(time)"
        case "interval":
            let m = everyMinutes ?? 60
            return m % 60 == 0 ? "每 \(m / 60) 小时" : "每 \(m) 分钟"
        default: return "每天 \(time)"
        }
    }
}

struct AILoop: Codable, Hashable, Identifiable {
    var id: String = ""
    var name: String = ""
    var enabled: Bool = true
    var schedule: AILoopSchedule = AILoopSchedule()
    var query: String = ""
    var instruction: String = ""
    var maxMessages: Int = 30
    var notify: Bool = true
    var emailDigest: Bool = false
    var model: AIModelRef?
    var lastRun: Int64?
    var nextRun: Int64?

    private enum CodingKeys: String, CodingKey {
        case id, name, enabled, schedule, query, instruction, maxMessages, notify, emailDigest, model, lastRun, nextRun
    }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        name = c.lenient(String.self, .name, "")
        enabled = c.lenient(Bool.self, .enabled, true)
        schedule = c.lenient(AILoopSchedule.self, .schedule, AILoopSchedule())
        query = c.lenient(String.self, .query, "")
        instruction = c.lenient(String.self, .instruction, "")
        maxMessages = c.lenient(Int.self, .maxMessages, 30)
        notify = c.lenient(Bool.self, .notify, true)
        emailDigest = c.lenient(Bool.self, .emailDigest, false)
        model = try? c.decodeIfPresent(AIModelRef.self, forKey: .model)
        lastRun = try? c.decodeIfPresent(Int64.self, forKey: .lastRun)
        nextRun = try? c.decodeIfPresent(Int64.self, forKey: .nextRun)
    }
}

struct AILoopRun: Decodable, Hashable, Identifiable {
    var id: String
    var loopId: String
    var startedAt: Int64
    var finishedAt: Int64?
    var status: String
    var messageCount: Int
    var output: String
    var error: String?

    private enum CodingKeys: String, CodingKey { case id, loopId, startedAt, finishedAt, status, messageCount, output, error }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        loopId = c.lenient(String.self, .loopId, "")
        startedAt = c.lenient(Int64.self, .startedAt, 0)
        finishedAt = try? c.decodeIfPresent(Int64.self, forKey: .finishedAt)
        status = c.lenient(String.self, .status, "")
        messageCount = c.lenient(Int.self, .messageCount, 0)
        output = c.lenient(String.self, .output, "")
        error = try? c.decodeIfPresent(String.self, forKey: .error)
    }

    var statusTitle: String {
        switch status {
        case "running": return "运行中"
        case "done", "ok", "success": return "完成"
        case "failed", "error": return "失败"
        case "cancelled": return "已取消"
        default: return status
        }
    }
}

struct AIAutoReplyRule: Codable, Hashable, Identifiable {
    var id: String = ""
    var name: String = ""
    var enabled: Bool = true
    var accountId: String?
    var fromContains: String = ""
    var subjectContains: String = ""
    var aiMatch: String = ""
    var instruction: String = ""
    var mode: String = "draft"
    var maxPerDay: Int = 20
    var oncePerSenderHours: Int = 24
    var sentToday: Int = 0
    var day: String = ""
    /// "ai" (default) | "vacation" (fixed `replyText` between `start` and `end`, §8.7).
    var kind: String = "ai"
    var replyText: String = ""
    var start: Int64?
    var end: Int64?

    private enum CodingKeys: String, CodingKey {
        case id, name, enabled, accountId, fromContains, subjectContains, aiMatch, instruction, mode, maxPerDay
        case oncePerSenderHours, sentToday, day, kind, replyText, start, end
    }

    var isVacation: Bool { kind == "vacation" }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        name = c.lenient(String.self, .name, "")
        enabled = c.lenient(Bool.self, .enabled, true)
        accountId = try? c.decodeIfPresent(String.self, forKey: .accountId)
        fromContains = c.lenient(String.self, .fromContains, "")
        subjectContains = c.lenient(String.self, .subjectContains, "")
        aiMatch = c.lenient(String.self, .aiMatch, "")
        instruction = c.lenient(String.self, .instruction, "")
        mode = c.lenient(String.self, .mode, "draft")
        maxPerDay = c.lenient(Int.self, .maxPerDay, 20)
        oncePerSenderHours = c.lenient(Int.self, .oncePerSenderHours, 24)
        sentToday = c.lenient(Int.self, .sentToday, 0)
        day = c.lenient(String.self, .day, "")
        kind = c.lenient(String.self, .kind, "ai")
        replyText = c.lenient(String.self, .replyText, "")
        start = try? c.decodeIfPresent(Int64.self, forKey: .start)
        end = try? c.decodeIfPresent(Int64.self, forKey: .end)
    }
}

// MARK: - §8 additions

/// A JSON array, or an object wrapping one (`{"items": […]}`); malformed elements are skipped.
struct AIList<T: Decodable>: Decodable {
    var items: [T] = []

    private struct AnyKey: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    init(from decoder: Decoder) throws {
        if let list = try? decoder.singleValueContainer().decode([Lossy<T>].self) {
            items = list.compactMap(\.value)
            return
        }
        let c = try decoder.container(keyedBy: AnyKey.self)
        for key in c.allKeys {
            if let list = try? c.decode([Lossy<T>].self, forKey: key) {
                items = list.compactMap(\.value)
                return
            }
        }
    }
}

/// An event found in a message (`ExtractedEvent`, §8.1). Times are unix seconds.
struct AIExtractedEvent: Codable, Hashable {
    var title: String = ""
    var start: Int64 = 0
    /// nil = one hour.
    var end: Int64?
    var allDay: Bool = false
    var location: String?
    var notes: String = ""
    /// How the message phrased it, e.g. "下周二下午 3 点".
    var sourceText: String = ""

    private enum CodingKeys: String, CodingKey { case title, start, end, allDay, location, notes, sourceText }

    init(title: String, start: Int64, end: Int64?, allDay: Bool, location: String?, notes: String, sourceText: String) {
        self.title = title
        self.start = start
        self.end = end
        self.allDay = allDay
        self.location = location
        self.notes = notes
        self.sourceText = sourceText
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        title = c.lenient(String.self, .title, "")
        start = c.lenient(Int64.self, .start, 0)
        end = try? c.decodeIfPresent(Int64.self, forKey: .end)
        allDay = c.lenient(Bool.self, .allDay, false)
        location = try? c.decodeIfPresent(String.self, forKey: .location)
        notes = c.lenient(String.self, .notes, "")
        sourceText = c.lenient(String.self, .sourceText, "")
    }
}

/// A send / delete requested through MCP, waiting for the user (`Confirmation`, §8.2).
struct AIConfirmation: Decodable, Hashable, Identifiable {
    var id: String = ""
    /// "send" | "delete"
    var kind: String = "send"
    var title: String = ""
    var detail: String = ""
    var draftId: String?
    var messageIds: [Int64] = []
    var createdAt: Int64 = 0
    /// 0 when unknown (built from an `ai_confirm` event).
    var expiresAt: Int64 = 0

    private enum CodingKeys: String, CodingKey { case id, kind, title, detail, draftId, messageIds, createdAt, expiresAt }

    init(id: String, kind: String, title: String, detail: String) {
        self.id = id
        self.kind = kind
        self.title = title
        self.detail = detail
        createdAt = Int64(Date().timeIntervalSince1970)
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        kind = c.lenient(String.self, .kind, "send")
        title = c.lenient(String.self, .title, "")
        detail = c.lenient(String.self, .detail, "")
        draftId = try? c.decodeIfPresent(String.self, forKey: .draftId)
        messageIds = c.lenient([Int64].self, .messageIds, [])
        createdAt = c.lenient(Int64.self, .createdAt, 0)
        expiresAt = c.lenient(Int64.self, .expiresAt, 0)
    }

    var isDelete: Bool { kind == "delete" }
}

/// `GET /v1/ai/mcp`.
struct AIMCPInfo: Decodable, Hashable {
    var enabled: Bool = false
    var url: String = ""
    var token: String = ""
    /// Filled in by the Swift side (the core returns "").
    var stdioCommand: String = ""

    private enum CodingKeys: String, CodingKey { case enabled, url, token, stdioCommand }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = c.lenient(Bool.self, .enabled, false)
        url = c.lenient(String.self, .url, "")
        token = c.lenient(String.self, .token, "")
        stdioCommand = c.lenient(String.self, .stdioCommand, "")
    }
}

/// An external MCP server the assistant may call (`McpServerConfig`, §8.3).
struct AIMCPServer: Codable, Hashable, Identifiable {
    var id: String = ""
    var name: String = ""
    var enabled: Bool = true
    /// "stdio" | "http"
    var transport: String = "stdio"
    var command: String = ""
    var args: [String] = []
    var env: [String: String] = [:]
    var url: String = ""
    var headers: [String: String] = [:]

    private enum CodingKeys: String, CodingKey { case id, name, enabled, transport, command, args, env, url, headers }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        name = c.lenient(String.self, .name, "")
        enabled = c.lenient(Bool.self, .enabled, true)
        transport = c.lenient(String.self, .transport, "stdio")
        command = c.lenient(String.self, .command, "")
        args = c.lenient([String].self, .args, [])
        env = c.lenient([String: String].self, .env, [:])
        url = c.lenient(String.self, .url, "")
        headers = c.lenient([String: String].self, .headers, [:])
    }

    var isHTTP: Bool { transport == "http" }
}

struct AIMCPToolInfo: Decodable, Hashable, Identifiable {
    var name: String = ""
    var description: String = ""

    var id: String { name }

    private enum CodingKeys: String, CodingKey { case name, description }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = c.lenient(String.self, .name, "")
        description = c.lenient(String.self, .description, "")
    }
}

/// `POST /v1/ai/mcp-servers/{id}/test`.
struct AIMCPTestResult: Decodable, Hashable {
    var ok: Bool = false
    var tools: [AIMCPToolInfo] = []
    var error: String?

    private enum CodingKeys: String, CodingKey { case ok, tools, error }

    init(ok: Bool, error: String?) {
        self.ok = ok
        self.error = error
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        ok = c.lenient(Bool.self, .ok, false)
        tools = (c.lenient([Lossy<AIMCPToolInfo>].self, .tools, [])).compactMap(\.value)
        error = try? c.decodeIfPresent(String.self, forKey: .error)
    }
}

/// One tool call reported in a `done` line (`tool_calls`).
struct AIToolCall: Decodable, Hashable {
    var server: String = ""
    var tool: String = ""
    var ok: Bool = true

    private enum CodingKeys: String, CodingKey { case server, tool, ok }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        server = c.lenient(String.self, .server, "")
        tool = c.lenient(String.self, .tool, "")
        ok = c.lenient(Bool.self, .ok, true)
    }
}

/// `GET /v1/ai/usage` (§8.4).
struct AIUsageReport: Decodable, Hashable {
    struct Day: Decodable, Hashable, Identifiable {
        /// "2026-10-23" (local date).
        var date: String = ""
        var calls: Int = 0
        var inputTokens: Int = 0
        var outputTokens: Int = 0
        var errors: Int = 0

        var id: String { date }

        private enum CodingKeys: String, CodingKey { case date, calls, inputTokens, outputTokens, errors }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            date = c.lenient(String.self, .date, "")
            calls = c.lenient(Int.self, .calls, 0)
            inputTokens = c.lenient(Int.self, .inputTokens, 0)
            outputTokens = c.lenient(Int.self, .outputTokens, 0)
            errors = c.lenient(Int.self, .errors, 0)
        }
    }

    struct ByModel: Decodable, Hashable, Identifiable {
        var provider: String = ""
        var model: String = ""
        var calls: Int = 0
        var inputTokens: Int = 0
        var outputTokens: Int = 0

        var id: String { provider + "/" + model }

        private enum CodingKeys: String, CodingKey { case provider, model, calls, inputTokens, outputTokens }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            provider = c.lenient(String.self, .provider, "")
            model = c.lenient(String.self, .model, "")
            calls = c.lenient(Int.self, .calls, 0)
            inputTokens = c.lenient(Int.self, .inputTokens, 0)
            outputTokens = c.lenient(Int.self, .outputTokens, 0)
        }
    }

    struct ByFeature: Decodable, Hashable, Identifiable {
        var feature: String = ""
        var calls: Int = 0
        var inputTokens: Int = 0
        var outputTokens: Int = 0

        var id: String { feature }

        private enum CodingKeys: String, CodingKey { case feature, calls, inputTokens, outputTokens }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            feature = c.lenient(String.self, .feature, "")
            calls = c.lenient(Int.self, .calls, 0)
            inputTokens = c.lenient(Int.self, .inputTokens, 0)
            outputTokens = c.lenient(Int.self, .outputTokens, 0)
        }
    }

    var days: [Day] = []
    var byModel: [ByModel] = []
    var byFeature: [ByFeature] = []

    private enum CodingKeys: String, CodingKey { case days, byModel, byFeature }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        days = (c.lenient([Lossy<Day>].self, .days, [])).compactMap(\.value)
        byModel = (c.lenient([Lossy<ByModel>].self, .byModel, [])).compactMap(\.value)
        byFeature = (c.lenient([Lossy<ByFeature>].self, .byFeature, [])).compactMap(\.value)
    }

    static func featureTitle(_ f: String) -> String {
        switch f {
        case "compose": return "写信助手"
        case "summary": return "摘要"
        case "insight": return "智能分析"
        case "batch": return "批量处理"
        case "loop": return "Loop"
        case "autoreply": return "自动回复"
        case "ask": return "问问邮箱"
        case "chat": return "对话"
        case "events": return "日程提取"
        case "explain": return "解读附件"
        case "mcp": return "MCP"
        case "embedding": return "嵌入"
        case "test": return "检查连接"
        case "translate": return "翻译"
        case "replies": return "建议回复"
        case "prompt": return "我的提示词"
        default: return f
        }
    }
}

/// A saved prompt (`PromptPreset`, §8.8).
struct AIPromptPreset: Codable, Hashable, Identifiable {
    var id: String = ""
    var name: String = ""
    /// "compose" | "reader" | "batch"
    var scope: String = "compose"
    var prompt: String = ""
    var sort: Int = 0

    private enum CodingKeys: String, CodingKey { case id, name, scope, prompt, sort }

    init(scope: String) {
        self.scope = scope
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        name = c.lenient(String.self, .name, "")
        scope = c.lenient(String.self, .scope, "compose")
        prompt = c.lenient(String.self, .prompt, "")
        sort = c.lenient(Int.self, .sort, 0)
    }

    static func scopeTitle(_ scope: String) -> String {
        switch scope {
        case "compose": return "写信"
        case "reader": return "阅读"
        case "batch": return "批量处理"
        default: return scope
        }
    }
}
