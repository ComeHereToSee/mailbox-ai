import Foundation

// Typed `/v1/ai/*` endpoints (docs/AI.md §4). Streaming endpoints go through `APIClient.ndjson`.

extension APIClient {
    // MARK: Providers & settings

    func aiPresets() async throws -> [AIProviderPreset] { try await get("/v1/ai/presets") }
    func aiProviders() async throws -> [AIProvider] { try await get("/v1/ai/providers") }
    func saveAIProvider(_ p: AIProvider, apiKey: String? = nil) async throws -> AIProvider {
        try await put("/v1/ai/providers/\(p.id)", AIProviderInput(p, apiKey: apiKey))
    }
    func deleteAIProvider(_ id: String) async throws { try await send("DELETE", "/v1/ai/providers/\(id)") }
    /// Lists the provider's models (not saved).
    func fetchAIModels(_ id: String) async throws -> [AIModel] { try await post("/v1/ai/providers/\(id)/models") }
    func testAIProvider(_ id: String, model: String) async throws -> AIProviderTest {
        struct B: Encodable { var model: String }
        return try await post("/v1/ai/providers/\(id)/test", B(model: model))
    }
    func aiCLIStatus() async throws -> AICLIReport { try await get("/v1/ai/cli") }
    func aiSettings() async throws -> AISettings { try await get("/v1/ai/settings") }
    func saveAISettings(_ s: AISettings) async throws -> AISettings { try await put("/v1/ai/settings", s) }

    // MARK: Reading

    /// Cached summary, or nil when there is none (404).
    func aiCachedSummary(_ id: Int64) async throws -> AISummaryCache? {
        do {
            return try await get("/v1/ai/messages/\(id)/summary", as: AISummaryCache.self)
        } catch let e as APIError where e.status == 404 {
            return nil
        }
    }
    func aiReplySuggestions(_ id: Int64) async throws -> [AIReplySuggestion] {
        struct R: Decodable { var suggestions: [AIReplySuggestion] }
        let r: R = try await post("/v1/ai/messages/\(id)/replies")
        return r.suggestions
    }
    /// Stored analysis, or nil when the message was not analysed (404).
    func aiInsight(_ id: Int64) async throws -> AIInsight? {
        do {
            return try await get("/v1/ai/messages/\(id)/insight", as: AIInsight.self)
        } catch let e as APIError where e.status == 404 {
            return nil
        }
    }
    func analyzeAIInsight(_ id: Int64) async throws -> AIInsight { try await post("/v1/ai/messages/\(id)/insight") }

    // MARK: Smart reminders

    func aiReminders() async throws -> [AIReminderItem] {
        let items: [Lossy<AIReminderItem>] = try await get("/v1/ai/reminders")
        return items.compactMap(\.value)
    }
    func setAIReminderDone(_ messageId: Int64, done: Bool) async throws {
        struct B: Encodable { var done: Bool }
        try await send("POST", "/v1/ai/reminders/\(messageId)/done", B(done: done))
    }

    // MARK: Batch

    func startAIBatch(ids: [Int64], op: String, instruction: String?) async throws -> String {
        struct B: Encodable { var ids: [Int64]; var op: String; var instruction: String? }
        struct R: Decodable { var jobId: String }
        let r: R = try await post("/v1/ai/batch", B(ids: ids, op: op, instruction: instruction))
        return r.jobId
    }
    func aiJob(_ id: String) async throws -> AIBatchJob { try await get("/v1/ai/jobs/\(id)") }
    func cancelAIJob(_ id: String) async throws { try await send("POST", "/v1/ai/jobs/\(id)/cancel", EmptyBody()) }
    func applyAIJob(_ id: String, messageIds: [Int64]) async throws {
        struct B: Encodable { var messageIds: [Int64] }
        try await send("POST", "/v1/ai/jobs/\(id)/apply", B(messageIds: messageIds))
    }

    // MARK: Loops

    func aiLoops() async throws -> [AILoop] { try await get("/v1/ai/loops") }
    func saveAILoop(_ l: AILoop) async throws -> AILoop { try await post("/v1/ai/loops", l) }
    func deleteAILoop(_ id: String) async throws { try await send("DELETE", "/v1/ai/loops/\(id)") }
    func runAILoop(_ id: String) async throws -> String {
        struct R: Decodable { var runId: String }
        let r: R = try await post("/v1/ai/loops/\(id)/run")
        return r.runId
    }
    func aiLoopRuns(_ id: String) async throws -> [AILoopRun] { try await get("/v1/ai/loops/\(id)/runs") }
    func aiLoopTemplates() async throws -> [AILoop] { try await get("/v1/ai/loop-templates") }

    // MARK: Auto-reply

    func aiAutoReplyRules() async throws -> [AIAutoReplyRule] { try await get("/v1/ai/autoreply") }
    func saveAIAutoReplyRule(_ r: AIAutoReplyRule) async throws -> AIAutoReplyRule { try await post("/v1/ai/autoreply", r) }
    func deleteAIAutoReplyRule(_ id: String) async throws { try await send("DELETE", "/v1/ai/autoreply/\(id)") }

    // MARK: §8.1 Calendar events

    /// Events found in a message (cached by the engine; `force` re-extracts).
    func aiExtractEvents(_ id: Int64, force: Bool = false) async throws -> [AIExtractedEvent] {
        struct B: Encodable { var force: Bool }
        struct R: Decodable {
            var events: [AIExtractedEvent]
            private enum CodingKeys: String, CodingKey { case events }
            init(from decoder: Decoder) throws {
                let c = try decoder.container(keyedBy: CodingKeys.self)
                events = ((try? c.decodeIfPresent([Lossy<AIExtractedEvent>].self, forKey: .events)) ?? []).compactMap(\.value)
            }
        }
        let r: R = try await post("/v1/ai/messages/\(id)/events", B(force: force))
        return r.events
    }

    // MARK: §8.2 Built-in MCP server

    func aiMCPInfo() async throws -> AIMCPInfo { try await get("/v1/ai/mcp") }
    /// Replaces the MCP token; returns the new one.
    func regenerateAIMCPToken() async throws -> String {
        struct R: Decodable { var token: String }
        let r: R = try await post("/v1/ai/mcp/token")
        return r.token
    }
    /// Pending send / delete requests from MCP clients.
    func aiConfirmations() async throws -> [AIConfirmation] {
        let r: AIList<AIConfirmation> = try await get("/v1/ai/confirm")
        return r.items
    }
    /// Allows or refuses a pending request (404 = already decided or expired).
    func answerAIConfirmation(_ id: String, approve: Bool) async throws {
        struct B: Encodable { var approve: Bool }
        try await send("POST", "/v1/ai/confirm/\(id)", B(approve: approve))
    }

    // MARK: §8.3 External MCP servers

    func aiMCPServers() async throws -> [AIMCPServer] {
        let r: AIList<AIMCPServer> = try await get("/v1/ai/mcp-servers")
        return r.items
    }
    /// Saves a server; returns it when the engine echoes it back (nil otherwise).
    @discardableResult
    func saveAIMCPServer(_ s: AIMCPServer) async throws -> AIMCPServer? {
        let data = try await send("POST", "/v1/ai/mcp-servers", s)
        guard let saved = try? JSON.decoder.decode(AIMCPServer.self, from: data), !saved.id.isEmpty else { return nil }
        return saved
    }
    func deleteAIMCPServer(_ id: String) async throws { try await send("DELETE", "/v1/ai/mcp-servers/\(id)") }
    func testAIMCPServer(_ id: String) async throws -> AIMCPTestResult { try await post("/v1/ai/mcp-servers/\(id)/test") }

    // MARK: §8.4 Usage

    func aiUsage(days: Int = 30) async throws -> AIUsageReport { try await get("/v1/ai/usage", query: ["days": String(days)]) }

    // MARK: §8.8 Prompt library

    func aiPrompts() async throws -> [AIPromptPreset] {
        let r: AIList<AIPromptPreset> = try await get("/v1/ai/prompts")
        return r.items
    }
    @discardableResult
    func saveAIPrompt(_ p: AIPromptPreset) async throws -> AIPromptPreset? {
        let data = try await send("POST", "/v1/ai/prompts", p)
        guard let saved = try? JSON.decoder.decode(AIPromptPreset.self, from: data), !saved.id.isEmpty else { return nil }
        return saved
    }
    func deleteAIPrompt(_ id: String) async throws { try await send("DELETE", "/v1/ai/prompts/\(id)") }

    // MARK: Demo

    /// Seeds insights, a finished Loop, a cached summary and `claude_code/default` (for `--demo`).
    func seedAIDemo() async throws { try await send("POST", "/v1/ai/demo", EmptyBody()) }
}

/// Decodes an array element, or nil when that element is malformed (the rest of the list still loads).
struct Lossy<T: Decodable>: Decodable {
    var value: T?

    init(from decoder: Decoder) throws {
        value = try? T(from: decoder)
    }
}

// MARK: - Request bodies for the streaming endpoints

struct AIChatBody: Encodable {
    var model: AIModelRef?
    var system: String?
    var messages: [AIChatMessage]
    /// Let the model call the enabled external MCP tools (§8.3).
    var tools: Bool = false
}

struct AIComposeBody: Encodable {
    var op: String
    var text: String
    var instruction: String?
    var targetLang: String?
    var messageId: Int64?
}

struct AISummaryBody: Encodable {
    var thread: Bool
    var force: Bool
}

struct AITranslateBody: Encodable {
    var targetLang: String
}

struct AIAskBody: Encodable {
    var question: String
    var mailbox: String?
    var tools: Bool = false
}

/// `POST /v1/ai/messages/{id}/attachments/{aid}/explain`: `text` carries PDF text extracted with PDFKit.
struct AIExplainBody: Encodable {
    var text: String?
    var question: String?
}

/// `POST /v1/ai/messages/{id}/prompt`: run a saved reader prompt on the message or its thread.
struct AIPromptBody: Encodable {
    var prompt: String
    var thread: Bool
}
