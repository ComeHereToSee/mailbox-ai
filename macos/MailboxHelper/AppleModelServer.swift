import Foundation
import Network
#if canImport(FoundationModels)
import FoundationModels
#endif

/// OpenAI-compatible endpoint on 127.0.0.1:47922 backed by Apple's on-device model (macOS 26+,
/// Apple Intelligence). The engine reaches it through the "Apple 端侧模型" provider preset like
/// any other OpenAI-compatible service. On older systems the server is not started.
final class AppleModelServer: @unchecked Sendable {
    static let shared = AppleModelServer()
    static let port: UInt16 = 47922
    static let modelId = "apple-on-device"
    /// The on-device context window is small; longer prompts are cut.
    static let maxPromptChars = 6000

    private var listener: NWListener?
    private let queue = DispatchQueue(label: "com.comeheretosee.mailbox.ai.apple-model")

    func start() {
        #if canImport(FoundationModels)
        guard #available(macOS 26.0, *) else { return }
        guard listener == nil, let port = NWEndpoint.Port(rawValue: Self.port) else { return }
        do {
            let params = NWParameters.tcp
            params.requiredLocalEndpoint = .hostPort(host: .ipv4(.loopback), port: port)
            params.allowLocalEndpointReuse = true
            let l = try NWListener(using: params)
            l.newConnectionHandler = { [weak self] conn in self?.accept(conn) }
            l.start(queue: queue)
            listener = l
        } catch {
            NSLog("Apple model server not started: \(error)")
        }
        #endif
    }

    // MARK: - HTTP

    private struct Request {
        var method: String
        var path: String
        var body: Data
    }

    private func accept(_ conn: NWConnection) {
        conn.start(queue: queue)
        receive(conn, buffer: Data())
    }

    private func receive(_ conn: NWConnection, buffer: Data) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            guard let self else { return }
            var buf = buffer
            if let data { buf.append(data) }
            if let req = Self.parse(buf) {
                self.handle(req, conn)
            } else if isComplete || error != nil || buf.count > 8_000_000 {
                conn.cancel()
            } else {
                self.receive(conn, buffer: buf)
            }
        }
    }

    /// A complete request (headers + Content-Length body), or nil while more bytes are needed.
    private static func parse(_ data: Data) -> Request? {
        let sep = Data("\r\n\r\n".utf8)
        guard let end = data.range(of: sep) else { return nil }
        let head = String(decoding: data[data.startIndex..<end.lowerBound], as: UTF8.self)
        let lines = head.components(separatedBy: "\r\n")
        let parts = (lines.first ?? "").split(separator: " ")
        guard parts.count >= 2 else { return Request(method: "", path: "", body: Data()) }
        var length = 0
        for line in lines.dropFirst() {
            let kv = line.split(separator: ":", maxSplits: 1)
            if kv.count == 2, kv[0].trimmingCharacters(in: .whitespaces).lowercased() == "content-length" {
                length = Int(kv[1].trimmingCharacters(in: .whitespaces)) ?? 0
            }
        }
        let body = data[end.upperBound...]
        guard body.count >= length else { return nil }
        return Request(method: String(parts[0]), path: String(parts[1]), body: Data(body.prefix(length)))
    }

    private func send(_ conn: NWConnection, status: String, contentType: String, body: Data) {
        var head = "HTTP/1.1 \(status)\r\nContent-Type: \(contentType)\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n"
        if body.isEmpty { head = head.replacingOccurrences(of: "Content-Type: \(contentType)\r\n", with: "") }
        var out = Data(head.utf8)
        out.append(body)
        conn.send(content: out, completion: .contentProcessed { _ in conn.cancel() })
    }

    private func sendJSON(_ conn: NWConnection, status: String = "200 OK", _ object: Any) {
        let data = (try? JSONSerialization.data(withJSONObject: object)) ?? Data("{}".utf8)
        send(conn, status: status, contentType: "application/json", body: data)
    }

    private func sendError(_ conn: NWConnection, status: String, _ message: String) {
        sendJSON(conn, status: status, ["error": ["message": message, "type": "apple_model"]])
    }

    private func handle(_ req: Request, _ conn: NWConnection) {
        let path = req.path.split(separator: "?").first.map(String.init) ?? req.path
        switch (req.method, path) {
        case ("GET", "/v1/models"), ("GET", "/models"):
            sendJSON(conn, ["object": "list", "data": [["id": Self.modelId, "object": "model", "owned_by": "apple"]]])
        case ("POST", "/v1/chat/completions"), ("POST", "/chat/completions"):
            chat(req.body, conn)
        default:
            sendError(conn, status: "404 Not Found", "不支持的接口：\(req.method) \(path)")
        }
    }

    // MARK: - Chat completions

    /// (system instructions, prompt) from OpenAI-style messages.
    private static func prompt(from messages: [[String: Any]]) -> (String, String) {
        func text(_ content: Any?) -> String {
            if let s = content as? String { return s }
            if let parts = content as? [[String: Any]] {
                return parts.compactMap { $0["text"] as? String }.joined(separator: "\n")
            }
            return ""
        }
        var system: [String] = []
        var turns: [(String, String)] = []
        for m in messages {
            let role = m["role"] as? String ?? "user"
            let t = text(m["content"])
            if role == "system" { system.append(t) } else { turns.append((role, t)) }
        }
        let prompt: String
        if turns.count == 1 {
            prompt = turns[0].1
        } else {
            prompt = turns.map { ($0.0 == "assistant" ? "助手：" : "用户：") + $0.1 }.joined(separator: "\n\n")
        }
        return (system.joined(separator: "\n\n"), String(prompt.prefix(maxPromptChars)))
    }

    private func chat(_ body: Data, _ conn: NWConnection) {
        guard let json = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any],
              let messages = json["messages"] as? [[String: Any]] else {
            sendError(conn, status: "400 Bad Request", "请求格式错误")
            return
        }
        let stream = json["stream"] as? Bool ?? false
        let (system, prompt) = Self.prompt(from: messages)
        #if canImport(FoundationModels)
        if #available(macOS 26.0, *) {
            if let reason = Self.unavailableReason() {
                sendError(conn, status: "503 Service Unavailable", reason)
                return
            }
            Task { await self.generate(system: system, prompt: prompt, stream: stream, conn: conn) }
            return
        }
        #endif
        sendError(conn, status: "503 Service Unavailable", "Apple 端侧模型需要 macOS 26 及以上")
    }

    #if canImport(FoundationModels)
    @available(macOS 26.0, *)
    static func unavailableReason() -> String? {
        switch SystemLanguageModel.default.availability {
        case .available:
            return nil
        case .unavailable(.deviceNotEligible):
            return "这台 Mac 不支持 Apple Intelligence"
        case .unavailable(.appleIntelligenceNotEnabled):
            return "请先在 系统设置 → Apple Intelligence 与 Siri 中打开 Apple Intelligence"
        case .unavailable(.modelNotReady):
            return "Apple 端侧模型还在下载或准备中，请稍后再试"
        case .unavailable:
            return "Apple 端侧模型暂不可用"
        }
    }

    @available(macOS 26.0, *)
    private func generate(system: String, prompt: String, stream: Bool, conn: NWConnection) async {
        let session = LanguageModelSession(instructions: system.isEmpty ? nil : system)
        let id = "chatcmpl-\(UUID().uuidString.prefix(12))"
        if !stream {
            do {
                let text = try await session.respond(to: prompt).content
                sendJSON(conn, [
                    "id": id, "object": "chat.completion", "model": Self.modelId,
                    "choices": [["index": 0, "message": ["role": "assistant", "content": text], "finish_reason": "stop"]],
                ])
            } catch {
                sendError(conn, status: "500 Internal Server Error", Self.describe(error))
            }
            return
        }
        var started = false
        var sent = ""
        do {
            for try await snapshot in session.streamResponse(to: prompt) {
                let full = snapshot.content
                guard full.count > sent.count, full.hasPrefix(sent) else {
                    sent = full.hasPrefix(sent) ? sent : full
                    continue
                }
                let delta = String(full.dropFirst(sent.count))
                sent = full
                if !started {
                    started = true
                    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
                    conn.send(content: Data(head.utf8), completion: .idempotent)
                }
                sendEvent(conn, id: id, delta: delta)
            }
        } catch {
            if !started {
                sendError(conn, status: "500 Internal Server Error", Self.describe(error))
                return
            }
        }
        if !started {
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
            conn.send(content: Data(head.utf8), completion: .idempotent)
        }
        let done = Data("data: [DONE]\n\n".utf8)
        conn.send(content: done, completion: .contentProcessed { _ in conn.cancel() })
    }

    private func sendEvent(_ conn: NWConnection, id: String, delta: String) {
        let chunk: [String: Any] = [
            "id": id, "object": "chat.completion.chunk", "model": Self.modelId,
            "choices": [["index": 0, "delta": ["content": delta]]],
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: chunk) else { return }
        var line = Data("data: ".utf8)
        line.append(data)
        line.append(Data("\n\n".utf8))
        conn.send(content: line, completion: .idempotent)
    }

    @available(macOS 26.0, *)
    private static func describe(_ error: Error) -> String {
        if let e = error as? LanguageModelSession.GenerationError {
            switch e {
            case .exceededContextWindowSize:
                return "内容太长，超出了 Apple 端侧模型的上下文长度"
            case .guardrailViolation:
                return "Apple 端侧模型拒绝处理这段内容（安全限制）"
            default:
                return "Apple 端侧模型出错：\(e.localizedDescription)"
            }
        }
        return "Apple 端侧模型出错：\(error.localizedDescription)"
    }
    #endif
}
