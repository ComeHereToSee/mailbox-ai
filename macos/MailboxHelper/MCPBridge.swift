import Foundation

/// `MailboxAIHelper --mcp`: a stdio ↔ Streamable HTTP bridge for MCP clients such as Codex
/// (docs/AI.md §8.2). Runs without any GUI: every stdin line is one JSON-RPC message, POSTed to
/// `http://127.0.0.1:<port>/mcp` with the MCP token; each response body is written to stdout as one
/// line. Notifications (HTTP 202) print nothing. stdout carries JSON-RPC only — logs go to stderr.
///
/// When the engine is not running, the helper app is launched (`open -g -b …`) and the bridge waits
/// up to 20 seconds for `daemon.json` and the API.
enum MCPBridge {
    fileprivate struct Endpoint: Equatable {
        var port: Int
        var token: String
    }

    fileprivate final class State: @unchecked Sendable {
        let lock = NSLock()
        var endpoint: Endpoint?
        var lastLaunchFailure: Date?
    }

    fileprivate final class Response: @unchecked Sendable {
        var status = 0
        var data = Data()
        var error: URLError?
    }

    private static let state = State()
    private static let outputLock = NSLock()
    private static let launchWait: TimeInterval = 20

    private static let session: URLSession = {
        let c = URLSessionConfiguration.ephemeral
        // tools/call may wait up to 180 s for the user to confirm a send / delete.
        c.timeoutIntervalForRequest = 600
        c.timeoutIntervalForResource = 900
        c.connectionProxyDictionary = [:]
        c.httpMaximumConnectionsPerHost = 8
        return URLSession(configuration: c)
    }()

    // MARK: Main loop

    static func run() -> Never {
        signal(SIGPIPE, SIG_IGN)
        logLine("Mailbox AI MCP 桥（stdio）已启动")
        // Start the engine early so `initialize` does not wait for it.
        _ = endpoint(allowLaunch: true)
        let group = DispatchGroup()
        let queue = DispatchQueue(label: "com.comeheretosee.mailbox.ai.mcp-bridge", attributes: .concurrent)
        while let line = readLine(strippingNewline: true) {
            let message = line.trimmingCharacters(in: .whitespacesAndNewlines)
            if message.isEmpty { continue }
            group.enter()
            queue.async {
                MCPBridge.handle(message)
                group.leave()
            }
        }
        // stdin closed: finish the requests already in flight, then quit.
        group.wait()
        exit(0)
    }

    private static func handle(_ message: String) {
        let body = Data(message.utf8)
        let id = requestId(of: body)
        if let reply = forward(body, id: id) {
            emit(reply)
        }
    }

    /// Returns the line to print, or nil (notification / nothing to say).
    private static func forward(_ body: Data, id: Any?) -> Data? {
        var lastProblem = "无法连接 Mailbox AI：请打开 Mailbox AI 后重试"
        for attempt in 0..<3 {
            guard let ep = endpoint(allowLaunch: true) else { break }
            let r = http("POST", port: ep.port, path: "/mcp", token: ep.token, body: body, timeout: 600)
            if let err = r.error {
                if err.code == .cannotConnectToHost || err.code == .cannotFindHost {
                    // Engine stopped (or restarted on another port): the request never arrived, so retrying is safe.
                    invalidate(ep)
                    lastProblem = "Mailbox AI 没有运行（\(err.localizedDescription)）"
                    continue
                }
                return errorReply(id: id, message: "调用 Mailbox AI 失败：\(err.localizedDescription)")
            }
            if r.status == 202 { return nil }
            if r.status == 401 && attempt == 0 {
                // Token regenerated, or daemon.json is stale.
                invalidate(ep)
                lastProblem = "MCP 令牌无效"
                continue
            }
            if isJSONRPC(r.data) { return oneLine(r.data) }
            if let event = sseMessage(r.data) { return event }
            if (200..<300).contains(r.status) && r.data.isEmpty { return nil }
            let text = String(data: r.data.prefix(300), encoding: .utf8) ?? ""
            return errorReply(id: id, message: "Mailbox AI 返回错误（HTTP \(r.status)）" + (text.isEmpty ? "" : "：" + text))
        }
        logLine(lastProblem)
        return errorReply(id: id, message: lastProblem)
    }

    // MARK: Engine discovery

    /// The engine's port and MCP token, launching the helper app when nothing answers.
    private static func endpoint(allowLaunch: Bool) -> Endpoint? {
        state.lock.lock()
        defer { state.lock.unlock() }
        if let ep = state.endpoint { return ep }
        if let ep = discover() {
            state.endpoint = ep
            return ep
        }
        guard allowLaunch else { return nil }
        // Do not keep every request waiting 20 s when a launch just failed.
        if let failed = state.lastLaunchFailure, Date().timeIntervalSince(failed) < 30 { return nil }
        launchHelper()
        let deadline = Date().addingTimeInterval(launchWait)
        while Date() < deadline {
            Thread.sleep(forTimeInterval: 0.5)
            if let ep = discover() {
                state.endpoint = ep
                state.lastLaunchFailure = nil
                logLine("已连接 Mailbox AI（端口 \(ep.port)）")
                return ep
            }
        }
        state.lastLaunchFailure = Date()
        logLine("等待 Mailbox AI 启动超时（\(Int(launchWait)) 秒）")
        return nil
    }

    private static func invalidate(_ ep: Endpoint) {
        state.lock.lock()
        if state.endpoint == ep { state.endpoint = nil }
        state.lock.unlock()
    }

    /// Reads daemon.json and asks `GET /v1/ai/mcp` (with the daemon token) for the MCP token.
    private static func discover() -> Endpoint? {
        guard let data = try? Data(contentsOf: Daemon.infoURL),
              let info = try? JSONDecoder().decode(DaemonInfo.self, from: data) else { return nil }
        let r = http("GET", port: info.port, path: "/v1/ai/mcp", token: info.token, body: nil, timeout: 5)
        if r.error != nil { return nil }
        if r.status == 200,
           let obj = try? JSONSerialization.jsonObject(with: r.data) as? [String: Any],
           let token = obj["token"] as? String, !token.isEmpty {
            return Endpoint(port: info.port, token: token)
        }
        if r.status == 401 || r.status == 0 { return nil }
        // Older engine without /v1/ai/mcp: /mcp also accepts the API token.
        return Endpoint(port: info.port, token: info.token)
    }

    private static func launchHelper() {
        logLine("Mailbox AI 没有运行，正在启动…")
        if runTool("/usr/bin/open", ["-g", "-b", Daemon.helperBundleId]) { return }
        // Not registered with Launch Services: open this very bundle.
        _ = runTool("/usr/bin/open", ["-g", Bundle.main.bundleURL.path])
    }

    /// Runs a tool with stdin / stdout detached from the JSON-RPC streams.
    private static func runTool(_ tool: String, _ args: [String]) -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = args
        p.standardInput = FileHandle.nullDevice
        p.standardOutput = FileHandle.standardError
        p.standardError = FileHandle.standardError
        do {
            try p.run()
            p.waitUntilExit()
            return p.terminationStatus == 0
        } catch {
            logLine("无法运行 \(tool)：\(error.localizedDescription)")
            return false
        }
    }

    // MARK: HTTP

    private static func http(_ method: String, port: Int, path: String, token: String, body: Data?, timeout: TimeInterval) -> Response {
        let result = Response()
        guard let url = URL(string: "http://127.0.0.1:\(port)\(path)") else {
            result.error = URLError(.badURL)
            return result
        }
        var req = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: timeout)
        req.httpMethod = method
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        req.setValue("application/json, text/event-stream", forHTTPHeaderField: "Accept")
        if let body {
            req.httpBody = body
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let done = DispatchSemaphore(value: 0)
        let task = session.dataTask(with: req) { data, response, error in
            result.status = (response as? HTTPURLResponse)?.statusCode ?? 0
            result.data = data ?? Data()
            if let error {
                result.error = (error as? URLError) ?? URLError(.unknown)
            }
            done.signal()
        }
        task.resume()
        done.wait()
        return result
    }

    // MARK: JSON-RPC helpers

    /// The request id (number, string or null), or nil for a notification / unparsable line.
    private static func requestId(of body: Data) -> Any? {
        guard let obj = try? JSONSerialization.jsonObject(with: body) as? [String: Any] else { return nil }
        return obj["id"]
    }

    private static func isJSONRPC(_ data: Data) -> Bool {
        guard !data.isEmpty, let obj = try? JSONSerialization.jsonObject(with: data) else { return false }
        if let dict = obj as? [String: Any] { return dict["jsonrpc"] != nil }
        if let list = obj as? [Any] { return list.contains { ($0 as? [String: Any])?["jsonrpc"] != nil } }
        return false
    }

    /// The JSON-RPC message inside a `text/event-stream` body (the server should answer JSON, but be tolerant).
    private static func sseMessage(_ data: Data) -> Data? {
        guard let text = String(data: data, encoding: .utf8), text.contains("data:") else { return nil }
        for raw in text.components(separatedBy: .newlines) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            guard line.hasPrefix("data:") else { continue }
            let payload = Data(line.dropFirst(5).trimmingCharacters(in: .whitespaces).utf8)
            if isJSONRPC(payload) { return oneLine(payload) }
        }
        return nil
    }

    /// The body as a single line (re-serialized only when it was pretty-printed).
    private static func oneLine(_ data: Data) -> Data {
        var bytes = data
        while let last = bytes.last, last == 0x0A || last == 0x0D || last == 0x20 || last == 0x09 { bytes.removeLast() }
        guard bytes.contains(0x0A) || bytes.contains(0x0D) else { return bytes }
        if let obj = try? JSONSerialization.jsonObject(with: bytes),
           let compact = try? JSONSerialization.data(withJSONObject: obj, options: []) {
            return compact
        }
        return bytes.filter { $0 != 0x0A && $0 != 0x0D }
    }

    /// A JSON-RPC error for request `id`; nil for notifications (they get no reply).
    private static func errorReply(id: Any?, message: String) -> Data? {
        guard let id else { return nil }
        let reply: [String: Any] = [
            "jsonrpc": "2.0",
            "id": id,
            "error": ["code": -32000, "message": message] as [String: Any],
        ]
        return try? JSONSerialization.data(withJSONObject: reply, options: [])
    }

    private static func emit(_ line: Data) {
        outputLock.lock()
        defer { outputLock.unlock() }
        var out = line
        out.append(0x0A)
        try? FileHandle.standardOutput.write(contentsOf: out)
    }

    private static func logLine(_ message: String) {
        let line = "[mailbox-ai mcp] " + message + "\n"
        try? FileHandle.standardError.write(contentsOf: Data(line.utf8))
    }
}
