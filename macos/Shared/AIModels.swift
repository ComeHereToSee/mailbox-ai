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

    private enum CodingKeys: String, CodingKey { case id, name, group, enabled }

    init(id: String, name: String = "", group: String = "", enabled: Bool = true) {
        self.id = id
        self.name = name.isEmpty ? id : name
        self.group = group
        self.enabled = enabled
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        name = c.lenient(String.self, .name, "")
        if name.isEmpty { name = id }
        group = c.lenient(String.self, .group, "")
        enabled = c.lenient(Bool.self, .enabled, true)
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

    private enum CodingKeys: String, CodingKey { case id, kind, name, baseUrl, apiVersion, enabled, models, hasKey, custom }

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

    private enum CodingKeys: String, CodingKey {
        case defaultModel, quickModel, language, tone, signatureName, smartReminders, smartScope, autoSummary
        case allowEncrypted, maxBodyChars, ttsVoice, ttsRate
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
    }

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

    private enum CodingKeys: String, CodingKey { case delta, done, text, model, error, subject, cached, sources }

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

    private enum CodingKeys: String, CodingKey {
        case messageId, needsReply, priority, deadline, deadlineText, todos, oneLine, remindAt, done, createdAt
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
    }

    /// Worth surfacing in the reader / pending list.
    var isActionable: Bool { needsReply || deadline != nil || !todos.isEmpty || priority == "high" }
}

/// `GET /v1/ai/reminders` entry.
struct AIReminderItem: Decodable, Hashable, Identifiable {
    var message: MessageSummary
    var accountLabel: String
    var insight: AIInsight

    var id: Int64 { message.id }

    private enum CodingKeys: String, CodingKey { case message, accountLabel, insight }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        message = try c.decode(MessageSummary.self, forKey: .message)
        accountLabel = c.lenient(String.self, .accountLabel, "")
        insight = c.lenient(AIInsight.self, .insight, AIInsight())
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

    private enum CodingKeys: String, CodingKey {
        case id, name, enabled, accountId, fromContains, subjectContains, aiMatch, instruction, mode, maxPerDay
        case oncePerSenderHours, sentToday, day
    }

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
    }
}
