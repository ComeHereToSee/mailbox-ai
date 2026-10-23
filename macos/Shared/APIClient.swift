import Foundation

struct APIError: LocalizedError {
    var status: Int
    var message: String
    var code: String

    var errorDescription: String? { message }
    var isAuth: Bool { code == "auth" || code == "oauth" }
}

/// Thin async client for the local engine API (see docs/API.md).
final class APIClient: @unchecked Sendable {
    static let shared = APIClient()

    private let lock = NSLock()
    private var info: DaemonInfo?
    private let session: URLSession = {
        let c = URLSessionConfiguration.ephemeral
        c.timeoutIntervalForRequest = 120
        c.connectionProxyDictionary = [:]
        return URLSession(configuration: c)
    }()

    var isConfigured: Bool { lock.withLock { info != nil } }

    var baseURL: URL? {
        lock.withLock { info.flatMap { URL(string: "http://127.0.0.1:\($0.port)") } }
    }

    var token: String? { lock.withLock { info?.token } }

    func configure(_ info: DaemonInfo) {
        lock.withLock { self.info = info }
    }

    /// Reads `daemon.json` written by the engine.
    @discardableResult
    func reloadInfo() -> Bool {
        guard let data = try? Data(contentsOf: Daemon.infoURL),
              let info = try? JSONDecoder().decode(DaemonInfo.self, from: data) else { return false }
        configure(info)
        return true
    }

    func url(_ path: String, query: [String: String?] = [:], withToken: Bool = false) -> URL? {
        guard let base = baseURL, var comps = URLComponents(url: base.appendingPathComponent(path), resolvingAgainstBaseURL: false) else {
            return nil
        }
        var items = query.compactMap { k, v in v.map { URLQueryItem(name: k, value: $0) } }
        if withToken, let t = token { items.append(URLQueryItem(name: "token", value: t)) }
        if !items.isEmpty { comps.queryItems = items }
        return comps.url
    }

    private func makeRequest(_ method: String, _ path: String, query: [String: String?], body: Data?) throws -> URLRequest {
        guard let url = url(path, query: query), let token else {
            throw APIError(status: 0, message: "邮件服务未启动", code: "offline")
        }
        var req = URLRequest(url: url)
        req.httpMethod = method
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        if let body {
            req.httpBody = body
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        return req
    }

    func raw(_ method: String, _ path: String, query: [String: String?] = [:], body: (any Encodable)? = nil) async throws -> Data {
        let bodyData = try body.map { try JSON.encoder.encode($0) }
        var req = try makeRequest(method, path, query: query, body: bodyData)
        var attempt = 0
        while true {
            do {
                let (data, resp) = try await session.data(for: req)
                let status = (resp as? HTTPURLResponse)?.statusCode ?? 0
                if status == 401, attempt == 0, reloadInfo() {
                    // Engine restarted with a new token.
                    attempt += 1
                    req = try makeRequest(method, path, query: query, body: bodyData)
                    continue
                }
                guard (200..<300).contains(status) else {
                    struct E: Decodable { var error: String; var code: String? }
                    let e = try? JSONDecoder().decode(E.self, from: data)
                    throw APIError(status: status, message: e?.error ?? "请求失败（\(status)）", code: e?.code ?? "http")
                }
                return data
            } catch let e as URLError where attempt == 0 && (e.code == .cannotConnectToHost || e.code == .networkConnectionLost) {
                attempt += 1
                if reloadInfo() {
                    req = try makeRequest(method, path, query: query, body: bodyData)
                    continue
                }
                throw APIError(status: 0, message: "无法连接邮件服务", code: "offline")
            }
        }
    }

    func get<T: Decodable>(_ path: String, query: [String: String?] = [:], as type: T.Type = T.self) async throws -> T {
        try JSON.decoder.decode(T.self, from: try await raw("GET", path, query: query))
    }

    func post<T: Decodable>(_ path: String, _ body: (any Encodable)? = nil, query: [String: String?] = [:], as type: T.Type = T.self) async throws -> T {
        try JSON.decoder.decode(T.self, from: try await raw("POST", path, query: query, body: body ?? EmptyBody()))
    }

    func put<T: Decodable>(_ path: String, _ body: any Encodable, as type: T.Type = T.self) async throws -> T {
        try JSON.decoder.decode(T.self, from: try await raw("PUT", path, body: body))
    }

    func patch<T: Decodable>(_ path: String, _ body: any Encodable, as type: T.Type = T.self) async throws -> T {
        try JSON.decoder.decode(T.self, from: try await raw("PATCH", path, body: body))
    }

    @discardableResult
    func send(_ method: String, _ path: String, _ body: (any Encodable)? = nil) async throws -> Data {
        try await raw(method, path, body: body)
    }
}

// MARK: - NDJSON streaming (`/v1/ai/*`, docs/AI.md §1)

extension APIClient {
    /// POSTs `body` and yields each line of the `application/x-ndjson` response.
    /// Errors raised before the stream starts arrive as `APIError` (e.g. code `ai_not_configured`).
    func ndjson(_ path: String, body: any Encodable) -> AsyncThrowingStream<AIStreamLine, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    let bodyData = try JSON.encoder.encode(body)
                    var attempt = 0
                    while true {
                        var req = try self.makeRequest("POST", path, query: [:], body: bodyData)
                        // Subscription CLIs can take a while before the first token.
                        req.timeoutInterval = 600
                        req.setValue("application/x-ndjson", forHTTPHeaderField: "Accept")
                        let (bytes, resp) = try await self.session.bytes(for: req)
                        let status = (resp as? HTTPURLResponse)?.statusCode ?? 0
                        if status == 401, attempt == 0, self.reloadInfo() {
                            attempt += 1
                            continue
                        }
                        guard (200..<300).contains(status) else {
                            var data = Data()
                            for try await b in bytes { data.append(b) }
                            struct E: Decodable { var error: String; var code: String? }
                            let e = try? JSONDecoder().decode(E.self, from: data)
                            throw APIError(status: status, message: e?.error ?? "请求失败（\(status)）", code: e?.code ?? "http")
                        }
                        for try await line in bytes.lines {
                            let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
                            guard !trimmed.isEmpty, let data = trimmed.data(using: .utf8),
                                  let parsed = try? JSON.decoder.decode(AIStreamLine.self, from: data) else { continue }
                            continuation.yield(parsed)
                        }
                        break
                    }
                    continuation.finish()
                } catch let e as URLError where e.code == .cannotConnectToHost || e.code == .networkConnectionLost {
                    continuation.finish(throwing: APIError(status: 0, message: "无法连接邮件服务", code: "offline"))
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}

struct EmptyBody: Encodable {}

struct OK: Decodable {
    var ok: Bool?
}

// MARK: - Typed endpoints

extension APIClient {
    func status() async throws -> StatusSnapshot { try await get("/v1/status") }
    func accounts() async throws -> [Account] { try await get("/v1/accounts") }
    func presets() async throws -> [Provider] { try await get("/v1/presets") }
    func autoconfig(email: String) async throws -> Discovery {
        struct B: Encodable { var email: String }
        return try await post("/v1/autoconfig", B(email: email))
    }
    func testAccount(_ a: NewAccount) async throws -> TestReport { try await post("/v1/accounts/test", a) }
    func addAccount(_ a: NewAccount) async throws -> Account { try await post("/v1/accounts", a) }
    func updateAccount(_ a: Account, password: String? = nil, smtpPassword: String? = nil) async throws -> Account {
        struct B: Encodable { var account: Account; var password: String?; var smtpPassword: String? }
        return try await put("/v1/accounts/\(a.id)", B(account: a, password: password, smtpPassword: smtpPassword))
    }
    func deleteAccount(_ id: String) async throws { try await send("DELETE", "/v1/accounts/\(id)") }
    func syncAccount(_ id: String?) async throws {
        if let id { try await send("POST", "/v1/accounts/\(id)/sync", EmptyBody()) } else { try await send("POST", "/v1/sync", EmptyBody()) }
    }
    func reorderAccounts(_ ids: [String]) async throws {
        struct B: Encodable { var ids: [String] }
        try await send("POST", "/v1/accounts/order", B(ids: ids))
    }
    func reauth(_ id: String, state: String) async throws {
        struct B: Encodable { var oauthState: String }
        try await send("POST", "/v1/accounts/\(id)/reauth", B(oauthState: state))
    }

    func identities(account: String? = nil) async throws -> [Identity] { try await get("/v1/identities", query: ["account": account]) }
    func saveIdentity(_ i: Identity) async throws -> Identity { try await post("/v1/identities", i) }
    func deleteIdentity(_ id: String) async throws { try await send("DELETE", "/v1/identities/\(id)") }

    func folders(account: String? = nil) async throws -> [Folder] { try await get("/v1/folders", query: ["account": account]) }
    func createFolder(account: String, name: String, parent: Int64?) async throws -> Folder {
        struct B: Encodable { var accountId: String; var name: String; var parentId: Int64? }
        return try await post("/v1/folders", B(accountId: account, name: name, parentId: parent))
    }
    func renameFolder(_ id: Int64, name: String) async throws -> Folder {
        struct B: Encodable { var name: String }
        return try await patch("/v1/folders/\(id)", B(name: name))
    }
    func setFolderSync(_ id: Int64, enabled: Bool) async throws -> Folder {
        struct B: Encodable { var syncEnabled: Bool }
        return try await patch("/v1/folders/\(id)", B(syncEnabled: enabled))
    }
    func deleteFolder(_ id: Int64) async throws { try await send("DELETE", "/v1/folders/\(id)") }
    func syncFolder(_ id: Int64) async throws { try await send("POST", "/v1/folders/\(id)/sync", EmptyBody()) }

    func messages(mailbox: String, query q: String? = nil, threads: Bool? = nil, cursor: MessagePage.Cursor? = nil, limit: Int = 100) async throws -> MessagePage {
        try await get("/v1/messages", query: [
            "mailbox": mailbox,
            "q": q,
            "threads": threads.map { $0 ? "true" : "false" },
            "before_date": cursor.map { String($0.beforeDate) },
            "before_id": cursor.map { String($0.beforeId) },
            "limit": String(limit),
        ])
    }
    func counts(_ mailboxes: [String]) async throws -> [String: [String: Int]] {
        try await get("/v1/counts", query: ["mailboxes": mailboxes.joined(separator: ",")])
    }
    func message(_ id: Int64, open: Bool = false) async throws -> MessageDetail {
        try await get("/v1/messages/\(id)", query: ["open": open ? "1" : nil])
    }
    func thread(_ id: Int64) async throws -> [MessageDetail] { try await get("/v1/messages/\(id)/thread") }
    func rawMessage(_ id: Int64) async throws -> Data { try await raw("GET", "/v1/messages/\(id)/raw") }
    func attachmentFile(_ id: Int64, _ aid: Int64) async throws -> URL {
        struct R: Decodable { var path: String }
        let r: R = try await post("/v1/messages/\(id)/attachments/\(aid)/file")
        return URL(fileURLWithPath: r.path)
    }

    func act(_ ids: [Int64], _ action: MessageAction) async throws -> ActionResult {
        try await post("/v1/messages/actions", ActionBody(ids: ids, action: action))
    }
    func markAllRead(mailbox: String) async throws {
        struct B: Encodable { var mailbox: String }
        try await send("POST", "/v1/messages/mark-all-read", B(mailbox: mailbox))
    }
    func undo(_ token: String) async throws -> Bool {
        struct B: Encodable { var token: String }
        let r: OK = try await post("/v1/undo", B(token: token))
        return r.ok ?? false
    }
    func replyDraft(_ id: Int64, mode: ComposeMode) async throws -> Draft {
        try await get("/v1/messages/\(id)/reply", query: ["mode": mode.rawValue])
    }
    func editDraft(_ id: Int64) async throws -> Draft { try await get("/v1/messages/\(id)/edit-draft") }
    func rsvp(_ id: Int64, partstat: String) async throws {
        struct B: Encodable { var partstat: String }
        try await send("POST", "/v1/messages/\(id)/rsvp", B(partstat: partstat))
    }
    func unsubscribe(_ id: Int64) async throws -> (done: Bool, url: String?) {
        struct R: Decodable { var result: String; var method: String?; var url: String? }
        let r: R = try await post("/v1/messages/\(id)/unsubscribe")
        return (r.result == "done", r.url)
    }
    func sendReadReceipt(_ id: Int64) async throws { try await send("POST", "/v1/messages/\(id)/read-receipt", EmptyBody()) }
    func trustImages(_ id: Int64, trusted: Bool) async throws {
        struct B: Encodable { var trusted: Bool }
        try await send("POST", "/v1/messages/\(id)/trust-images", B(trusted: trusted))
    }
    func serverSearch(account: String, folder: Int64?, query q: String) async throws -> [MessageSummary] {
        struct B: Encodable { var accountId: String; var folderId: Int64?; var q: String }
        return try await post("/v1/search/server", B(accountId: account, folderId: folder, q: q))
    }

    func saveDraft(_ d: Draft, sync: Bool = false) async throws -> Draft {
        try await post("/v1/drafts", d, query: ["sync": sync ? "1" : nil])
    }
    func deleteDraft(_ id: String) async throws { try await send("DELETE", "/v1/drafts/\(id)") }
    func draft(_ id: String) async throws -> Draft { try await get("/v1/drafts/\(id)") }
    func sendDraft(_ d: Draft, at: Int64? = nil) async throws -> OutboxItem {
        struct B: Encodable { var draft: Draft; var sendAt: Int64? }
        return try await post("/v1/send", B(draft: d, sendAt: at))
    }
    func outbox() async throws -> [OutboxItem] { try await get("/v1/outbox") }
    func cancelSend(_ id: String) async throws -> Draft? {
        struct R: Decodable { var draft: Draft? }
        let r: R = try await post("/v1/outbox/\(id)/cancel")
        return r.draft
    }
    func retrySend(_ id: String) async throws { try await send("POST", "/v1/outbox/\(id)/retry", EmptyBody()) }
    func buildInvite(summary: String, start: Date, end: Date, allDay: Bool, location: String?, organizer: EmailAddress, attendees: [EmailAddress]) async throws -> String {
        struct B: Encodable {
            var summary: String; var start: Int64; var end: Int64; var allDay: Bool; var location: String?
            var organizer: EmailAddress; var attendees: [EmailAddress]
        }
        struct R: Decodable { var ics: String }
        let r: R = try await post("/v1/calendar/invite", B(summary: summary, start: Int64(start.timeIntervalSince1970), end: Int64(end.timeIntervalSince1970),
                                                          allDay: allDay, location: location, organizer: organizer, attendees: attendees))
        return r.ics
    }

    func contacts(_ q: String, limit: Int = 8) async throws -> [Contact] {
        try await get("/v1/contacts", query: ["q": q, "limit": String(limit)])
    }
    func setVip(_ email: String, _ vip: Bool) async throws {
        struct B: Encodable { var email: String; var vip: Bool }
        try await send("POST", "/v1/contacts/vip", B(email: email, vip: vip))
    }
    func vips() async throws -> [String] { try await get("/v1/vips") }
    func blocked() async throws -> [String] { try await get("/v1/blocked") }
    func block(_ pattern: String) async throws {
        struct B: Encodable { var pattern: String }
        try await send("POST", "/v1/blocked", B(pattern: pattern))
    }
    func unblock(_ pattern: String) async throws {
        struct B: Encodable { var pattern: String }
        try await send("DELETE", "/v1/blocked", B(pattern: pattern))
    }

    func rules() async throws -> [Rule] { try await get("/v1/rules") }
    func saveRule(_ r: Rule) async throws -> Rule { try await post("/v1/rules", r) }
    func deleteRule(_ id: String) async throws { try await send("DELETE", "/v1/rules/\(id)") }
    func runRule(_ id: String) async throws -> Int {
        struct R: Decodable { var affected: Int }
        let r: R = try await post("/v1/rules/\(id)/run")
        return r.affected
    }
    struct SieveStatus: Decodable {
        struct Script: Decodable, Hashable { var name: String; var active: Bool }
        var supported: Bool
        var scripts: [Script]
        var activeScript: String?
        var error: String?
    }
    func sieveStatus(_ account: String) async throws -> SieveStatus { try await get("/v1/accounts/\(account)/sieve") }
    func sieveUpload(_ account: String) async throws -> String {
        struct R: Decodable { var script: String }
        let r: R = try await post("/v1/accounts/\(account)/sieve")
        return r.script
    }
    func sieveRemove(_ account: String) async throws { try await send("DELETE", "/v1/accounts/\(account)/sieve") }
    func templates() async throws -> [Template] { try await get("/v1/templates") }
    func saveTemplate(_ t: Template) async throws -> Template { try await post("/v1/templates", t) }
    func deleteTemplate(_ id: String) async throws { try await send("DELETE", "/v1/templates/\(id)") }
    func smartFolders() async throws -> [SmartFolder] { try await get("/v1/smart-folders") }
    func saveSmartFolder(_ f: SmartFolder) async throws -> SmartFolder { try await post("/v1/smart-folders", f) }
    func deleteSmartFolder(_ id: String) async throws { try await send("DELETE", "/v1/smart-folders/\(id)") }
    func forwarders() async throws -> [Forwarder] { try await get("/v1/forwarders") }
    func saveForwarder(_ f: Forwarder) async throws -> Forwarder { try await post("/v1/forwarders", f) }
    func deleteForwarder(_ id: String) async throws { try await send("DELETE", "/v1/forwarders/\(id)") }
    func testForwarder(_ f: Forwarder) async throws { try await send("POST", "/v1/forwarders/test", f) }

    func settings() async throws -> Settings { try await get("/v1/settings") }
    func saveSettings(_ s: Settings) async throws -> Settings { try await put("/v1/settings", s) }
    func pauseNotifications(minutes: Int) async throws -> Settings {
        struct B: Encodable { var minutes: Int }
        return try await post("/v1/notifications/pause", B(minutes: minutes))
    }

    func startOAuth(provider: String, loginHint: String?) async throws -> (url: URL, state: String) {
        struct B: Encodable { var provider: String; var loginHint: String? }
        struct R: Decodable { var url: String; var state: String }
        let r: R = try await post("/v1/oauth/start", B(provider: provider, loginHint: loginHint))
        guard let u = URL(string: r.url) else { throw APIError(status: 0, message: "无效的登录地址", code: "oauth") }
        return (u, r.state)
    }
    func oauthStatus(_ state: String) async throws -> (done: Bool, email: String?) {
        struct R: Decodable { var pending: Bool; var done: Bool; var email: String? }
        let r: R = try await get("/v1/oauth/\(state)")
        return (r.done, r.email)
    }

    func menu(limit: Int = 8) async throws -> MenuSnapshot { try await get("/v1/menu", query: ["limit": String(limit)]) }
    func importMessages(folder: Int64, path: String) async throws -> Int {
        struct B: Encodable { var folderId: Int64; var path: String }
        struct R: Decodable { var imported: Int }
        let r: R = try await post("/v1/import", B(folderId: folder, path: path))
        return r.imported
    }
    func exportMessages(ids: [Int64]?, folder: Int64?, path: String, format: String) async throws -> Int {
        struct B: Encodable { var ids: [Int64]?; var folderId: Int64?; var path: String; var format: String }
        struct R: Decodable { var exported: Int }
        let r: R = try await post("/v1/export", B(ids: ids, folderId: folder, path: path, format: format))
        return r.exported
    }
    struct StorageReport: Decodable {
        var databaseBytes: Int64
        var messagesBytes: Int64
        var cacheBytes: Int64
        var messageCount: Int
    }
    func storage() async throws -> StorageReport { try await get("/v1/storage") }

    // MARK: PGP / S/MIME keys
    func keys() async throws -> [KeyInfo] { try await get("/v1/keys") }
    func importKey(_ req: KeyImportRequest) async throws -> KeyInfo { try await post("/v1/keys/import", req) }
    func generateKey(name: String, email: String, passphrase: String?) async throws -> KeyInfo {
        struct B: Encodable { var name: String; var email: String; var passphrase: String? }
        return try await post("/v1/keys/generate", B(name: name, email: email, passphrase: passphrase))
    }
    func exportKey(_ id: String, secret: Bool = false) async throws -> String {
        struct R: Decodable { var text: String }
        let r: R = try await get("/v1/keys/\(id)/export", query: ["secret": secret ? "true" : "false"])
        return r.text
    }
    func deleteKey(_ id: String) async throws { try await send("DELETE", "/v1/keys/\(id)") }
    func setKeyPassphrase(_ id: String, _ passphrase: String?) async throws {
        struct B: Encodable { var passphrase: String? }
        try await send("POST", "/v1/keys/\(id)/passphrase", B(passphrase: passphrase))
    }
    /// (recipients with an encryption key, recipients without).
    func checkKeys(_ recipients: [String]) async throws -> (have: [String], missing: [String]) {
        struct B: Encodable { var recipients: [String] }
        struct R: Decodable { var have: [String]; var missing: [String] }
        let r: R = try await post("/v1/keys/check", B(recipients: recipients))
        return (r.have, r.missing)
    }
    func cleanup() async throws -> Int64 {
        struct R: Decodable { var freed: Int64 }
        let r: R = try await post("/v1/cleanup")
        return r.freed
    }
    func seedDemo() async throws { try await send("POST", "/v1/demo", EmptyBody()) }
}

/// Message actions as understood by `/v1/messages/actions`.
enum MessageAction: Equatable {
    case read, unread, flag, unflag, archive, trash, delete, spam, notSpam, moveToInbox, pin, unpin, unsnooze
    case move(folderId: Int64)
    case label(add: [String], remove: [String])
    case snooze(until: Int64)
    case category(Category)

    var name: String {
        switch self {
        case .read: return "read"
        case .unread: return "unread"
        case .flag: return "flag"
        case .unflag: return "unflag"
        case .archive: return "archive"
        case .trash: return "trash"
        case .delete: return "delete"
        case .spam: return "spam"
        case .notSpam: return "not_spam"
        case .moveToInbox: return "move_to_inbox"
        case .pin: return "pin"
        case .unpin: return "unpin"
        case .unsnooze: return "unsnooze"
        case .move: return "move"
        case .label: return "label"
        case .snooze: return "snooze"
        case .category: return "category"
        }
    }

    /// Short description for the undo toast.
    var pastTense: String {
        switch self {
        case .read: return "已标为已读"
        case .unread: return "已标为未读"
        case .flag: return "已加星标"
        case .unflag: return "已取消星标"
        case .archive: return "已归档"
        case .trash: return "已移到废纸篓"
        case .delete: return "已永久删除"
        case .spam: return "已标记为垃圾邮件"
        case .notSpam, .moveToInbox: return "已移回收件箱"
        case .pin: return "已置顶"
        case .unpin: return "已取消置顶"
        case .unsnooze: return "已取消稍后提醒"
        case .move: return "已移动"
        case .label: return "已更新标签"
        case .snooze: return "已设置稍后提醒"
        case .category: return "已更改分类"
        }
    }
}

struct ActionBody: Encodable {
    var ids: [Int64]
    var action: MessageAction

    private struct Key: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init(_ s: String) { stringValue = s }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        try c.encode(ids, forKey: Key("ids"))
        try c.encode(action.name, forKey: Key("action"))
        switch action {
        case .move(let f): try c.encode(f, forKey: Key("folder_id"))
        case .label(let add, let remove):
            try c.encode(add, forKey: Key("add"))
            try c.encode(remove, forKey: Key("remove"))
        case .snooze(let until): try c.encode(until, forKey: Key("until"))
        case .category(let cat): try c.encode(cat.rawValue, forKey: Key("category"))
        default: break
        }
    }
}
