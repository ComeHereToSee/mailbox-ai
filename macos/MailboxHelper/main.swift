import AppKit

// `MailboxAIHelper --mcp`: stdio MCP bridge for MCP clients (docs/AI.md §8.2). No GUI, no engine:
// it talks to the running helper over the local API and never returns.
if CommandLine.arguments.contains("--mcp") {
    MCPBridge.run()
}

let app = NSApplication.shared
let delegate = HelperDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
