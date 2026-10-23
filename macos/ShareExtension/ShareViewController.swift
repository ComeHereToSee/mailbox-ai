import AppKit
import UniformTypeIdentifiers

/// "Share → Mailbox": turns shared files, links and text into a new mail in the main app.
final class ShareViewController: NSViewController {
    private final class Collected: @unchecked Sendable {
        let lock = NSLock()
        var files: [String] = []
        var texts: [String] = []
        var links: [String] = []
        var subject: String?
    }

    override var nibName: NSNib.Name? { nil }

    override func loadView() {
        view = NSView(frame: NSRect(x: 0, y: 0, width: 320, height: 80))
        let label = NSTextField(labelWithString: "正在打开 Mailbox…")
        label.frame = NSRect(x: 20, y: 30, width: 280, height: 20)
        view.addSubview(label)
    }

    override func viewDidAppear() {
        super.viewDidAppear()
        collect()
    }

    private func collect() {
        let items = extensionContext?.inputItems as? [NSExtensionItem] ?? []
        let out = Collected()
        let group = DispatchGroup()
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("share-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)

        for item in items {
            if let title = item.attributedTitle?.string, !title.isEmpty { out.subject = title }
            if let text = item.attributedContentText?.string, !text.isEmpty { out.texts.append(text) }
            for provider in item.attachments ?? [] {
                if provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
                    group.enter()
                    provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier, options: nil) { data, _ in
                        defer { group.leave() }
                        var url: URL?
                        if let d = data as? Data { url = URL(dataRepresentation: d, relativeTo: nil) }
                        if let u = data as? URL { url = u }
                        guard let src = url else { return }
                        let dst = dir.appendingPathComponent(src.lastPathComponent)
                        if (try? FileManager.default.copyItem(at: src, to: dst)) != nil {
                            out.lock.withLock { out.files.append(dst.path) }
                        }
                    }
                } else if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
                    group.enter()
                    provider.loadItem(forTypeIdentifier: UTType.url.identifier, options: nil) { data, _ in
                        defer { group.leave() }
                        var link: String?
                        if let u = data as? URL { link = u.absoluteString }
                        if let d = data as? Data { link = URL(dataRepresentation: d, relativeTo: nil)?.absoluteString }
                        if let link { out.lock.withLock { out.links.append(link) } }
                    }
                } else if provider.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
                    group.enter()
                    provider.loadFileRepresentation(forTypeIdentifier: UTType.image.identifier) { url, _ in
                        defer { group.leave() }
                        guard let url else { return }
                        let dst = dir.appendingPathComponent(url.lastPathComponent)
                        if (try? FileManager.default.copyItem(at: url, to: dst)) != nil {
                            out.lock.withLock { out.files.append(dst.path) }
                        }
                    }
                } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
                    group.enter()
                    provider.loadItem(forTypeIdentifier: UTType.plainText.identifier, options: nil) { data, _ in
                        defer { group.leave() }
                        if let s = data as? String { out.lock.withLock { out.texts.append(s) } }
                    }
                }
            }
        }

        group.notify(queue: .main) { [weak self] in
            var comps = URLComponents()
            comps.scheme = "mailboxai"
            comps.host = "compose"
            var query: [URLQueryItem] = []
            if let s = out.subject { query.append(URLQueryItem(name: "subject", value: s)) }
            let body = (out.texts + out.links).joined(separator: "\n\n")
            if !body.isEmpty { query.append(URLQueryItem(name: "body", value: body)) }
            for f in out.files { query.append(URLQueryItem(name: "file", value: f)) }
            comps.queryItems = query
            if let url = comps.url { NSWorkspace.shared.open(url) }
            self?.extensionContext?.completeRequest(returningItems: nil, completionHandler: nil)
        }
    }
}
