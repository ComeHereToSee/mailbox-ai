import Foundation

// Mirrors core/crates/mailcore/src/model.rs. JSON keys are snake_case and converted
// automatically by `JSON.decoder` / `JSON.encoder`; enum raw values stay snake_case.

/// String enums that decode unknown values to a fallback instead of failing.
protocol LenientEnum: RawRepresentable, Codable, Hashable where RawValue == String {
    static var fallback: Self { get }
}

extension LenientEnum {
    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? Self.fallback
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(rawValue)
    }
}

enum AccountKind: String, LenientEnum, CaseIterable {
    case imap, pop3, graph, jmap, web
    static var fallback: AccountKind { .imap }
}

enum ConnectionSecurity: String, LenientEnum, CaseIterable, Identifiable {
    case tls, startTls = "start_tls", none
    static var fallback: ConnectionSecurity { .tls }
    var id: String { rawValue }
    var title: String {
        switch self {
        case .tls: return "SSL/TLS"
        case .startTls: return "STARTTLS"
        case .none: return "无加密"
        }
    }
}

enum AuthKind: String, LenientEnum {
    case password, oauth2
    static var fallback: AuthKind { .password }
}

enum FolderRole: String, LenientEnum, CaseIterable {
    case inbox, sent, drafts, trash, junk, archive, all, flagged, important, outbox, custom
    static var fallback: FolderRole { .custom }

    var title: String {
        switch self {
        case .inbox: return "收件箱"
        case .sent: return "已发送"
        case .drafts: return "草稿箱"
        case .trash: return "已删除"
        case .junk: return "垃圾邮件"
        case .archive: return "归档"
        case .all: return "所有邮件"
        case .flagged: return "星标邮件"
        case .important: return "重要"
        case .outbox: return "发件箱"
        case .custom: return "文件夹"
        }
    }

    var symbol: String {
        switch self {
        case .inbox: return "tray"
        case .sent: return "paperplane"
        case .drafts: return "doc"
        case .trash: return "trash"
        case .junk: return "xmark.bin"
        case .archive: return "archivebox"
        case .all: return "tray.2"
        case .flagged: return "star"
        case .important: return "exclamationmark.circle"
        case .outbox: return "tray.and.arrow.up"
        case .custom: return "folder"
        }
    }
}

enum Category: String, LenientEnum, CaseIterable, Identifiable {
    case primary, social, promotions, updates, forums
    static var fallback: Category { .primary }
    var id: String { rawValue }
    var title: String {
        switch self {
        case .primary: return "主要"
        case .social: return "社交"
        case .promotions: return "推广"
        case .updates: return "通知"
        case .forums: return "论坛"
        }
    }
    var symbol: String {
        switch self {
        case .primary: return "person"
        case .social: return "person.2"
        case .promotions: return "tag"
        case .updates: return "info.circle"
        case .forums: return "bubble.left.and.bubble.right"
        }
    }
}

enum SyncState: String, LenientEnum {
    case idle, connecting, syncing, error, offline, disabled
    static var fallback: SyncState { .idle }
}

enum OutboxStatus: String, LenientEnum {
    case queued, sending, sent, failed, cancelled
    static var fallback: OutboxStatus { .queued }
}

enum ComposeMode: String, LenientEnum {
    case new, reply, replyAll = "reply_all", forward, forwardAsAttachment = "forward_as_attachment"
    static var fallback: ComposeMode { .new }
}

struct ServerConfig: Codable, Hashable {
    var host: String = ""
    var port: Int = 993
    var security: ConnectionSecurity = .tls
    var username: String?
    var acceptInvalidCerts: Bool = false
}

struct NotifyPrefs: Codable, Hashable {
    var enabled: Bool = true
    var vipOnly: Bool = false
    var categories: [Category] = [.primary, .updates]
    var sound: Bool = true
}

struct AccountConfig: Codable, Hashable {
    var imap: ServerConfig?
    var pop3: ServerConfig?
    var smtp: ServerConfig?
    var auth: AuthKind = .password
    var oauthProvider: String?
    var webUrl: String?
    var jmapUrl: String?
    var sendId: Bool = true
    var saveSentCopy: Bool = true
    var syncDays: Int = 90
    var pollIntervalSecs: Int = 300
    var useIdle: Bool = true
    var popLeaveOnServer: Bool = true
    var popDeleteAfterDays: Int?
    var popDeleteOnLocalDelete: Bool = false
    var notify: NotifyPrefs = NotifyPrefs()
}

struct Account: Codable, Hashable, Identifiable {
    var id: String = ""
    var kind: AccountKind = .imap
    var email: String = ""
    var name: String = ""
    var label: String = ""
    var color: String = ""
    var provider: String?
    var config: AccountConfig = AccountConfig()
    var enabled: Bool = true
    var sortOrder: Int = 0
    var createdAt: Int64 = 0

    var displayLabel: String { label.isEmpty ? email : label }
}

struct AccountStatus: Codable, Hashable {
    var accountId: String
    var state: SyncState
    var error: String?
    var lastSync: Int64?
    var unread: Int
}

struct Folder: Codable, Hashable, Identifiable {
    var id: Int64
    var accountId: String
    var path: String
    var name: String
    var delimiter: String?
    var role: FolderRole
    var unread: Int
    var total: Int
    var selectable: Bool
    var local: Bool
    var lastSync: Int64?
}

struct EmailAddress: Codable, Hashable, Identifiable {
    var name: String?
    var email: String

    var id: String { email.lowercased() }
    var display: String {
        if let n = name, !n.isEmpty { return n }
        return email
    }
    var full: String {
        if let n = name, !n.isEmpty { return "\(n) <\(email)>" }
        return email
    }
}

struct Flags: Codable, Hashable {
    var seen: Bool = false
    var flagged: Bool = false
    var answered: Bool = false
    var forwarded: Bool = false
    var draft: Bool = false
}

struct MessageSummary: Codable, Hashable, Identifiable {
    var id: Int64
    var accountId: String
    var folderId: Int64
    var threadId: String
    var subject: String
    var from: EmailAddress
    var to: [EmailAddress]
    var date: Int64
    var snippet: String
    var sortDate: Int64
    var flags: Flags
    var hasAttachments: Bool
    var labels: [String]
    var category: Category
    var pinned: Bool
    var snoozedUntil: Int64?
    var isVip: Bool
    var threadCount: Int
    var threadUnread: Int
}

struct Attachment: Codable, Hashable, Identifiable {
    var id: Int64
    var filename: String
    var mimeType: String
    var size: Int64
    var contentId: String?
    var inline: Bool
}

struct Unsubscribe: Codable, Hashable {
    var http: String?
    var mailto: String?
    var oneClick: Bool
}

struct CalendarInvite: Codable, Hashable {
    var method: String
    var uid: String
    var summary: String
    var location: String?
    var description: String?
    var start: Int64?
    var end: Int64?
    var allDay: Bool
    var organizer: EmailAddress?
    var attendees: [EmailAddress]
    var myStatus: String?
    var raw: String
}

struct MessageDetail: Decodable, Identifiable, Hashable {
    var summary: MessageSummary
    var cc: [EmailAddress]
    var bcc: [EmailAddress]
    var replyTo: [EmailAddress]
    var messageId: String?
    var inReplyTo: String?
    var references: [String]
    var html: String?
    var text: String?
    var attachments: [Attachment]
    var hasRemoteContent: Bool
    var allowRemote: Bool
    var trackers: Int
    var unsubscribe: Unsubscribe?
    var invite: CalendarInvite?
    var readReceiptTo: String?
    var warnings: [String]
    var bodyLoaded: Bool
    var folderRole: FolderRole
    var encrypted: Bool
    var signed: Bool
    var decrypted: Bool
    var decryptError: String?
    var signature: SignatureInfo?

    var id: Int64 { summary.id }

    private enum Keys: String, CodingKey {
        case cc, bcc, replyTo, messageId, inReplyTo, references, html, text, attachments, hasRemoteContent, allowRemote
        case trackers, unsubscribe, invite, readReceiptTo, warnings, bodyLoaded, folderRole, encrypted, signed
        case decrypted, decryptError, signature
    }

    init(from decoder: Decoder) throws {
        summary = try MessageSummary(from: decoder)
        let c = try decoder.container(keyedBy: Keys.self)
        cc = try c.decodeIfPresent([EmailAddress].self, forKey: .cc) ?? []
        bcc = try c.decodeIfPresent([EmailAddress].self, forKey: .bcc) ?? []
        replyTo = try c.decodeIfPresent([EmailAddress].self, forKey: .replyTo) ?? []
        messageId = try c.decodeIfPresent(String.self, forKey: .messageId)
        inReplyTo = try c.decodeIfPresent(String.self, forKey: .inReplyTo)
        references = try c.decodeIfPresent([String].self, forKey: .references) ?? []
        html = try c.decodeIfPresent(String.self, forKey: .html)
        text = try c.decodeIfPresent(String.self, forKey: .text)
        attachments = try c.decodeIfPresent([Attachment].self, forKey: .attachments) ?? []
        hasRemoteContent = try c.decodeIfPresent(Bool.self, forKey: .hasRemoteContent) ?? false
        allowRemote = try c.decodeIfPresent(Bool.self, forKey: .allowRemote) ?? false
        trackers = try c.decodeIfPresent(Int.self, forKey: .trackers) ?? 0
        unsubscribe = try c.decodeIfPresent(Unsubscribe.self, forKey: .unsubscribe)
        invite = try c.decodeIfPresent(CalendarInvite.self, forKey: .invite)
        readReceiptTo = try c.decodeIfPresent(String.self, forKey: .readReceiptTo)
        warnings = try c.decodeIfPresent([String].self, forKey: .warnings) ?? []
        bodyLoaded = try c.decodeIfPresent(Bool.self, forKey: .bodyLoaded) ?? false
        folderRole = try c.decodeIfPresent(FolderRole.self, forKey: .folderRole) ?? .custom
        encrypted = try c.decodeIfPresent(Bool.self, forKey: .encrypted) ?? false
        signed = try c.decodeIfPresent(Bool.self, forKey: .signed) ?? false
        decrypted = try c.decodeIfPresent(Bool.self, forKey: .decrypted) ?? false
        decryptError = try c.decodeIfPresent(String.self, forKey: .decryptError)
        signature = try c.decodeIfPresent(SignatureInfo.self, forKey: .signature)
    }
}

struct SignatureInfo: Codable, Hashable {
    var kind: String = ""
    var valid: Bool = false
    var signer: String?
    var keyId: String?
    var message: String = ""
}

/// A PGP key or S/MIME certificate in the key store.
struct KeyInfo: Codable, Hashable, Identifiable {
    var id: String
    var kind: String
    var emails: [String] = []
    var names: [String] = []
    var hasSecret: Bool = false
    var created: Int64 = 0
    var expires: Int64?

    var kindLabel: String { kind == "smime" ? "S/MIME" : "PGP" }
    var shortId: String { String(id.suffix(16)) }
    var expired: Bool { expires.map { $0 < Int64(Date().timeIntervalSince1970) } ?? false }
}

struct KeyImportRequest: Encodable {
    var text: String?
    var dataBase64: String?
    var password: String?
    var passphrase: String?
}

struct Identity: Codable, Hashable, Identifiable {
    var id: String = ""
    var accountId: String = ""
    var name: String = ""
    var email: String = ""
    var replyTo: String?
    var signatureHtml: String = ""
    var isDefault: Bool = false
}

struct DraftAttachment: Codable, Hashable, Identifiable {
    var path: String?
    var messageId: Int64?
    var attachmentId: Int64?
    var filename: String = ""
    var mimeType: String?
    var size: Int64?
    var contentId: String?

    var id: String { path ?? "\(messageId ?? 0)-\(attachmentId ?? 0)-\(filename)" }
}

struct Draft: Codable, Hashable, Identifiable {
    var id: String = ""
    var accountId: String = ""
    var identityId: String?
    var to: [EmailAddress] = []
    var cc: [EmailAddress] = []
    var bcc: [EmailAddress] = []
    var subject: String = ""
    var html: String = ""
    var text: String?
    var markdown: String?
    var plainTextOnly: Bool = false
    var attachments: [DraftAttachment] = []
    var mode: ComposeMode = .new
    var sourceMessageId: Int64?
    var draftMessageId: Int64?
    var requestReadReceipt: Bool = false
    var priorityHigh: Bool = false
    var calendar: [String]?
    var sign: Bool = false
    var encrypt: Bool = false
    var updatedAt: Int64 = 0
}

struct OutboxItem: Codable, Hashable, Identifiable {
    var id: String
    var accountId: String
    var subject: String
    var to: [EmailAddress]
    var sendAt: Int64
    var status: OutboxStatus
    var attempts: Int
    var lastError: String?
    var createdAt: Int64
}

struct Contact: Codable, Hashable, Identifiable {
    var email: String
    var name: String?
    var useCount: Int = 0
    var lastUsed: Int64 = 0
    var vip: Bool = false
    var id: String { email }
    var address: EmailAddress { EmailAddress(name: name, email: email) }
}

struct RuleCondition: Codable, Hashable, Identifiable {
    var field: String = "from"
    var op: String = "contains"
    var value: String = ""
    var uid = UUID()
    var id: UUID { uid }
    private enum CodingKeys: String, CodingKey { case field, op, value }
}

struct RuleAction: Codable, Hashable, Identifiable {
    var kind: String = "move"
    var value: String = ""
    var uid = UUID()
    var id: UUID { uid }
    private enum CodingKeys: String, CodingKey { case kind, value }
}

struct Rule: Codable, Hashable, Identifiable {
    var id: String = ""
    var name: String = ""
    var enabled: Bool = true
    var accountId: String?
    var matchAll: Bool = true
    var conditions: [RuleCondition] = [RuleCondition()]
    var actions: [RuleAction] = [RuleAction()]
    var sortOrder: Int = 0
}

struct Template: Codable, Hashable, Identifiable {
    var id: String = ""
    var name: String = ""
    var subject: String = ""
    var html: String = ""
    var shortcut: String = ""
}

struct SmartFolder: Codable, Hashable, Identifiable {
    var id: String = ""
    var name: String = ""
    var query: String = ""
    var icon: String = ""
}

struct Forwarder: Codable, Hashable, Identifiable {
    var id: String = ""
    var kind: String = "bark"
    var name: String = ""
    var enabled: Bool = true
    var config: [String: String] = [:]
    var accounts: [String] = []
    var vipOnly: Bool = false

    private enum CodingKeys: String, CodingKey { case id, kind, name, enabled, config, accounts, vipOnly }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? "bark"
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        enabled = try c.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        accounts = try c.decodeIfPresent([String].self, forKey: .accounts) ?? []
        vipOnly = try c.decodeIfPresent(Bool.self, forKey: .vipOnly) ?? false
        // Config values may be any JSON type; keep them as strings for editing.
        let raw = try c.decodeIfPresent([String: JSONValue].self, forKey: .config) ?? [:]
        config = raw.mapValues { $0.stringValue }
    }
}

/// Minimal JSON value used where the schema is open.
enum JSONValue: Codable, Hashable {
    case string(String), number(Double), bool(Bool), null, array([JSONValue]), object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let n = try? c.decode(Double.self) { self = .number(n) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else if let a = try? c.decode([JSONValue].self) { self = .array(a) }
        else { self = .object(try c.decode([String: JSONValue].self)) }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .string(let s): try c.encode(s)
        case .number(let n): try c.encode(n)
        case .bool(let b): try c.encode(b)
        case .null: try c.encodeNil()
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }

    var stringValue: String {
        switch self {
        case .string(let s): return s
        case .number(let n): return n == n.rounded() ? String(Int64(n)) : String(n)
        case .bool(let b): return b ? "true" : "false"
        case .null: return ""
        case .array, .object:
            let data = (try? JSONEncoder().encode(self)) ?? Data()
            return String(data: data, encoding: .utf8) ?? ""
        }
    }
}

struct QuietHours: Codable, Hashable {
    var enabled: Bool = false
    var start: String = "22:00"
    var end: String = "08:00"
}

struct OAuthClients: Codable, Hashable {
    var googleClientId: String = ""
    var googleClientSecret: String = ""
    var microsoftClientId: String = ""
}

struct Settings: Codable, Hashable {
    var undoSendSecs: Int = 5
    var loadRemoteImages: String = "never"
    var conversationView: Bool = true
    var quietHours: QuietHours = QuietHours()
    var notificationsPausedUntil: Int64?
    var markReadOnOpen: Bool = true
    var markReadDelaySecs: Int = 0
    var confirmDelete: Bool = false
    var categoriesEnabled: Bool = true
    var oauth: OAuthClients = OAuthClients()
    var language: String = "zh-Hans"
    var listDensity: String = "comfortable"
    var showSnippets: Bool = true
    var autoBcc: String = ""
    var replyPosition: String = "top"
    var defaultFont: String = ""
    var defaultFontSize: Int = 14
    var blockTrackers: Bool = true
    var spotlightIndex: Bool = true
    var appLock: Bool = false
    var cryptoPrefer: String = "pgp"
    var cryptoSignDefault: Bool = false
    var cryptoAutoEncrypt: Bool = true
}

struct Provider: Codable, Hashable, Identifiable {
    var id: String = ""
    var name: String = ""
    var label: String = ""
    var color: String = ""
    var domains: [String] = []
    var imap: ServerConfig?
    var pop3: ServerConfig?
    var smtp: ServerConfig?
    var auth: AuthKind = .password
    var oauthProvider: String?
    var passwordLabel: String = "密码"
    var helpUrl: String?
    var webUrl: String?
    var helpSteps: [String] = []
    var sendId: Bool = true
    var saveSentCopy: Bool = true
    var requiresBridge: Bool = false
    var notes: String = ""

    private enum CodingKeys: String, CodingKey {
        case id, name, label, color, domains, imap, pop3, smtp, auth, oauthProvider, passwordLabel, helpUrl, webUrl
        case helpSteps, sendId, saveSentCopy, requiresBridge, notes
    }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        label = try c.decodeIfPresent(String.self, forKey: .label) ?? ""
        color = try c.decodeIfPresent(String.self, forKey: .color) ?? ""
        domains = try c.decodeIfPresent([String].self, forKey: .domains) ?? []
        imap = try c.decodeIfPresent(ServerConfig.self, forKey: .imap)
        pop3 = try c.decodeIfPresent(ServerConfig.self, forKey: .pop3)
        smtp = try c.decodeIfPresent(ServerConfig.self, forKey: .smtp)
        auth = try c.decodeIfPresent(AuthKind.self, forKey: .auth) ?? .password
        oauthProvider = try c.decodeIfPresent(String.self, forKey: .oauthProvider)
        passwordLabel = try c.decodeIfPresent(String.self, forKey: .passwordLabel) ?? "密码"
        helpUrl = try c.decodeIfPresent(String.self, forKey: .helpUrl)
        webUrl = try c.decodeIfPresent(String.self, forKey: .webUrl)
        helpSteps = try c.decodeIfPresent([String].self, forKey: .helpSteps) ?? []
        sendId = try c.decodeIfPresent(Bool.self, forKey: .sendId) ?? true
        saveSentCopy = try c.decodeIfPresent(Bool.self, forKey: .saveSentCopy) ?? true
        requiresBridge = try c.decodeIfPresent(Bool.self, forKey: .requiresBridge) ?? false
        notes = try c.decodeIfPresent(String.self, forKey: .notes) ?? ""
    }
}

struct Discovery: Codable, Hashable {
    var email: String
    var provider: Provider?
    var imap: [ServerConfig]
    var pop3: [ServerConfig]
    var smtp: [ServerConfig]
    var source: String
}

struct NewAccount: Codable {
    var kind: AccountKind = .imap
    var email: String = ""
    var name: String = ""
    var label: String = ""
    var color: String = ""
    var provider: String?
    var config: AccountConfig = AccountConfig()
    var password: String?
    var smtpPassword: String?
    var oauthState: String?
    var skipTest: Bool = false
}

struct ProtocolTest: Codable, Hashable {
    var ok: Bool
    var error: String?
    var code: String?
    var skipped: Bool
}

struct TestReport: Codable, Hashable {
    var ok: Bool
    var incoming: ProtocolTest
    var smtp: ProtocolTest
}

struct ActionResult: Codable {
    var affected: Int
    var undoToken: String?
}

struct MessagePage: Decodable {
    struct Cursor: Decodable, Hashable {
        var beforeDate: Int64
        var beforeId: Int64
    }
    var messages: [MessageSummary]
    var cursor: Cursor?
}

struct MenuItem: Decodable, Identifiable, Hashable {
    var message: MessageSummary
    var accountLabel: String
    var accountColor: String
    var id: Int64 { message.id }
}

struct MenuSnapshot: Decodable {
    var unread: Int
    var items: [MenuItem]
    var settings: Settings
}

struct StatusSnapshot: Decodable {
    var version: String
    var accounts: [AccountStatus]
    var unreadTotal: Int
    var outbox: Int
}

struct DaemonInfo: Codable {
    var port: Int
    var token: String
    var pid: Int
    var version: String
}

/// Events pushed over `/v1/events` and through the helper's FFI callback.
enum MailEvent {
    case hello
    case newMail(accountId: String, accountLabel: String, accountColor: String, message: MessageSummary, notify: Bool, sound: Bool)
    case messagesChanged(accountId: String?)
    case foldersChanged(accountId: String)
    case accountStatus(AccountStatus)
    case accountsChanged
    case outbox(id: String, status: OutboxStatus, error: String?)
    case oauth(state: String, ok: Bool, email: String?, error: String?)
    case unreadCounts(total: Int, accounts: [String: Int])
    case settingsChanged
    case snoozed(messageId: Int64)
    case resync
    // AI (docs/AI.md §5)
    case aiInsight(messageId: Int64)
    case aiReminder(messageId: Int64, title: String, body: String)
    case aiJob(jobId: String, done: Int, total: Int, status: String)
    case aiLoop(loopId: String, runId: String, name: String, status: String, preview: String)
    case aiAutoreply(messageId: Int64, ruleName: String, draftId: String?, sent: Bool, subject: String)
    case unknown

    private struct Envelope: Decodable {
        var type: String
        var accountId: String?
        var accountLabel: String?
        var accountColor: String?
        var message: MessageSummary?
        var notify: Bool?
        var sound: Bool?
        var id: String?
        var error: String?
        var state: String?
        var ok: Bool?
        var email: String?
        var total: Int?
        var accounts: [[JSONValue]]?
        var messageId: Int64?
    }

    private struct StatusEnvelope: Decodable {
        var status: AccountStatus
    }

    private struct OutboxEnvelope: Decodable {
        var id: String
        var status: OutboxStatus
        var error: String?
    }

    private struct AIEnvelope: Decodable {
        var messageId: Int64?
        var title: String?
        var body: String?
        var jobId: String?
        var done: Int?
        var total: Int?
        var status: String?
        var loopId: String?
        var runId: String?
        var name: String?
        var preview: String?
        var ruleName: String?
        var draftId: String?
        var sent: Bool?
        var subject: String?
    }

    private static func decodeAI(_ type: String, _ data: Data) -> MailEvent {
        guard let e = try? JSON.decoder.decode(AIEnvelope.self, from: data) else { return .unknown }
        switch type {
        case "ai_insight":
            return .aiInsight(messageId: e.messageId ?? 0)
        case "ai_reminder":
            return .aiReminder(messageId: e.messageId ?? 0, title: e.title ?? "", body: e.body ?? "")
        case "ai_job":
            return .aiJob(jobId: e.jobId ?? "", done: e.done ?? 0, total: e.total ?? 0, status: e.status ?? "")
        case "ai_loop":
            return .aiLoop(loopId: e.loopId ?? "", runId: e.runId ?? "", name: e.name ?? "", status: e.status ?? "", preview: e.preview ?? "")
        case "ai_autoreply":
            return .aiAutoreply(messageId: e.messageId ?? 0, ruleName: e.ruleName ?? "", draftId: e.draftId, sent: e.sent ?? false,
                                subject: e.subject ?? "")
        default:
            return .unknown
        }
    }

    static func decode(_ data: Data) -> MailEvent {
        guard let env = try? JSON.decoder.decode(Envelope.self, from: data) else { return .unknown }
        switch env.type {
        case "hello": return .hello
        case "new_mail":
            guard let m = env.message else { return .unknown }
            return .newMail(accountId: env.accountId ?? "", accountLabel: env.accountLabel ?? "", accountColor: env.accountColor ?? "",
                            message: m, notify: env.notify ?? false, sound: env.sound ?? true)
        case "messages_changed": return .messagesChanged(accountId: env.accountId)
        case "folders_changed": return .foldersChanged(accountId: env.accountId ?? "")
        case "account_status":
            guard let s = try? JSON.decoder.decode(StatusEnvelope.self, from: data) else { return .unknown }
            return .accountStatus(s.status)
        case "accounts_changed": return .accountsChanged
        case "outbox":
            guard let o = try? JSON.decoder.decode(OutboxEnvelope.self, from: data) else { return .unknown }
            return .outbox(id: o.id, status: o.status, error: o.error)
        case "o_auth": return .oauth(state: env.state ?? "", ok: env.ok ?? false, email: env.email, error: env.error)
        case "unread_counts":
            var map: [String: Int] = [:]
            for pair in env.accounts ?? [] where pair.count == 2 {
                if case .string(let id) = pair[0], case .number(let n) = pair[1] { map[id] = Int(n) }
            }
            return .unreadCounts(total: env.total ?? 0, accounts: map)
        case "settings_changed": return .settingsChanged
        case "snoozed": return .snoozed(messageId: env.messageId ?? 0)
        case "resync": return .resync
        case "ai_insight", "ai_reminder", "ai_job", "ai_loop", "ai_autoreply": return decodeAI(env.type, data)
        default: return .unknown
        }
    }
}

enum JSON {
    static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        return e
    }()
}
