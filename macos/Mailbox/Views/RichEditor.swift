import AppKit
import SwiftUI
import UniformTypeIdentifiers
import WebKit

/// Drives the contenteditable WKWebView used for composing HTML mail.
@MainActor
final class RichEditorController: NSObject, ObservableObject, WKScriptMessageHandler, WKNavigationDelegate {
    weak var webView: WKWebView?
    private var pendingHTML: String?
    /// Last known content, so a closing window can still save it.
    private(set) var lastHTML = ""
    private var ready = false
    var onChange: (() -> Void)?

    func attach(_ web: WKWebView) {
        webView = web
        // A fresh page: content is applied again once it reports "ready".
        ready = false
        pendingHTML = lastHTML
    }

    func setHTML(_ html: String) {
        lastHTML = html
        guard ready, let web = webView else {
            pendingHTML = html
            return
        }
        web.evaluateJavaScript("setContent(\(Self.jsString(html)))")
    }

    func html() async -> String {
        guard let web = webView, ready else { return lastHTML }
        if let s = (try? await web.evaluateJavaScript("getContent()")) as? String {
            lastHTML = s
        }
        return lastHTML
    }

    func exec(_ command: String, _ value: String? = nil) {
        let arg = value.map { Self.jsString($0) } ?? "null"
        webView?.evaluateJavaScript("exec('\(command)', \(arg))")
    }

    func insertHTML(_ html: String) {
        webView?.evaluateJavaScript("insertHTML(\(Self.jsString(html)))")
    }

    func replaceSignature(_ html: String) {
        webView?.evaluateJavaScript("replaceSignature(\(Self.jsString(html)))")
    }

    func focus() {
        webView?.evaluateJavaScript("document.getElementById('editor').focus()")
    }

    // MARK: AI helpers

    /// Plain text of the current selection inside the editor ("" when nothing is selected).
    /// Also remembers that range so `replaceSelection` / `insertBelow` act on it later.
    func selectedText() async -> String {
        guard let web = webView, ready else { return "" }
        return ((try? await web.evaluateJavaScript("aiSelectedText()")) as? String) ?? ""
    }

    /// Plain text of the body, without the signature (#mailbox-signature) and the quoted original (#mailbox-quote).
    func bodyText() async -> String {
        guard let web = webView, ready else { return ComposeModel.plain(lastHTML) }
        return ((try? await web.evaluateJavaScript("aiBodyText()")) as? String) ?? ""
    }

    /// Replaces the range captured by `selectedText()` (or the current selection).
    func replaceSelection(html: String) {
        webView?.evaluateJavaScript("aiReplaceSelection(\(Self.jsString(html)))")
    }

    /// Replaces the whole body, keeping the signature and the quoted original.
    func replaceBody(html: String) {
        webView?.evaluateJavaScript("aiReplaceBody(\(Self.jsString(html)))")
    }

    /// Inserts after the captured selection, or at the end of the body (above the signature).
    func insertBelow(html: String) {
        webView?.evaluateJavaScript("aiInsertBelow(\(Self.jsString(html)))")
    }

    static func jsString(_ s: String) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: [s])) ?? Data("[\"\"]".utf8)
        let arr = String(data: data, encoding: .utf8) ?? "[\"\"]"
        return String(arr.dropFirst().dropLast())
    }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        let body = message.body as? String
        if body == "ready" {
            ready = true
            if let html = pendingHTML {
                pendingHTML = nil
                setHTML(html)
            }
        } else {
            Task {
                _ = await self.html()
                self.onChange?()
            }
        }
    }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        // The editor never navigates: dropped files and clicked links must not replace it.
        if action.navigationType == .other, action.request.url?.scheme == "about" || action.request.url?.scheme == "mailbox-cid" {
            decisionHandler(.allow)
        } else {
            decisionHandler(.cancel)
        }
    }

    static let page = """
    <!doctype html><html><head><meta charset="utf-8"><meta name="color-scheme" content="light dark">
    <style>
    html, body { margin: 0; height: 100%; }
    body { font: 14px/1.6 -apple-system, "PingFang SC", sans-serif; }
    #editor { min-height: 100%; padding: 12px 16px; outline: none; box-sizing: border-box; word-wrap: break-word; }
    #editor img { max-width: 100%; }
    #editor blockquote { margin: 0 0 0 .8ex; border-left: 2px solid #ccc; padding-left: 1ex; color: #666; }
    #editor table { border-collapse: collapse; }
    #editor td, #editor th { border: 1px solid #bbb; padding: 4px 8px; min-width: 40px; }
    #editor pre { background: rgba(127,127,127,.12); padding: 8px; border-radius: 6px; white-space: pre-wrap; }
    #mailbox-signature { color: #666; }
    @media (prefers-color-scheme: dark) { body { color: #e8e8e8; background: #1e1e1e; } a { color: #6cb4ff; } }
    </style></head>
    <body><div id="editor" contenteditable="true"></div>
    <script>
    const ed = document.getElementById('editor');
    let timer = null;
    function changed() { clearTimeout(timer); timer = setTimeout(() => window.webkit.messageHandlers.editor.postMessage('change'), 300); }
    ed.addEventListener('input', changed);
    function setContent(html) { ed.innerHTML = html; }
    function getContent() { return ed.innerHTML; }
    function exec(cmd, val) { ed.focus(); document.execCommand(cmd, false, val); changed(); }
    function insertHTML(html) { ed.focus(); document.execCommand('insertHTML', false, html); changed(); }
    function replaceSignature(html) {
      let sig = document.getElementById('mailbox-signature');
      if (!sig) { sig = document.createElement('div'); sig.id = 'mailbox-signature'; const q = document.getElementById('mailbox-quote'); ed.insertBefore(sig, q); }
      sig.innerHTML = html; changed();
    }
    // AI: remember the last selection inside the editor; menus outside the web view keep it.
    let lastRange = null, aiRange = null;
    document.addEventListener('selectionchange', () => {
      const s = window.getSelection();
      if (s && s.rangeCount > 0) {
        const r = s.getRangeAt(0);
        if (ed.contains(r.commonAncestorContainer)) lastRange = r.cloneRange();
      }
    });
    function renderedText(node) {
      const box = document.createElement('div');
      box.style.cssText = 'position:absolute;left:-100000px;top:0;width:640px;white-space:normal;';
      box.appendChild(node);
      document.body.appendChild(box);
      const t = box.innerText;
      box.remove();
      return t;
    }
    function aiSelectedText() {
      const r = lastRange;
      if (!r || r.collapsed || !ed.contains(r.commonAncestorContainer)) { aiRange = null; return ''; }
      aiRange = r.cloneRange();
      return renderedText(r.cloneContents()).trim();
    }
    function aiBodyText() {
      const c = ed.cloneNode(true);
      c.removeAttribute('id');
      c.removeAttribute('contenteditable');
      c.querySelectorAll('#mailbox-signature, #mailbox-quote').forEach(n => n.remove());
      return renderedText(c).trim();
    }
    function aiFragment(html) {
      const t = document.createElement('div');
      t.innerHTML = html;
      const f = document.createDocumentFragment();
      while (t.firstChild) f.appendChild(t.firstChild);
      return f;
    }
    function aiAnchor() {
      // Body goes above the signature; with the reply below the quote, between quote and signature.
      const sig = document.getElementById('mailbox-signature');
      const quote = document.getElementById('mailbox-quote');
      if (quote && sig && (quote.compareDocumentPosition(sig) & Node.DOCUMENT_POSITION_FOLLOWING)) return sig;
      const first = [sig, quote].filter(Boolean)[0];
      if (!first) return null;
      let n = first;
      while (n.parentNode && n.parentNode !== ed) n = n.parentNode;
      return n.parentNode === ed ? n : null;
    }
    function aiKeep(n) {
      const sig = document.getElementById('mailbox-signature');
      const quote = document.getElementById('mailbox-quote');
      return [sig, quote].some(k => k && (n === k || n.contains(k)));
    }
    function aiReplaceSelection(html) {
      const r = aiRange || lastRange;
      if (!r || r.collapsed) { aiReplaceBody(html); return; }
      ed.focus();
      const s = window.getSelection();
      s.removeAllRanges();
      s.addRange(r);
      document.execCommand('insertHTML', false, html);
      aiRange = null;
      changed();
    }
    function aiReplaceBody(html) {
      const quote = document.getElementById('mailbox-quote');
      const sig = document.getElementById('mailbox-signature');
      const bottom = quote && sig && (quote.compareDocumentPosition(sig) & Node.DOCUMENT_POSITION_FOLLOWING);
      [...ed.childNodes].forEach(n => {
        if (aiKeep(n)) return;
        // With the reply below the quote, only the part between quote and signature is the body.
        if (bottom && (n.compareDocumentPosition(quote) & Node.DOCUMENT_POSITION_FOLLOWING)) return;
        n.remove();
      });
      const anchor = aiAnchor();
      const frag = aiFragment(html + (anchor ? '<div><br></div>' : ''));
      if (anchor) ed.insertBefore(frag, anchor); else ed.appendChild(frag);
      aiRange = null;
      changed();
    }
    function aiInsertBelow(html) {
      const r = aiRange;
      if (r && !r.collapsed) {
        ed.focus();
        const c = r.cloneRange();
        c.collapse(false);
        const s = window.getSelection();
        s.removeAllRanges();
        s.addRange(c);
        document.execCommand('insertHTML', false, html);
        aiRange = null;
        changed();
        return;
      }
      const anchor = aiAnchor();
      const frag = aiFragment(html);
      if (anchor) ed.insertBefore(frag, anchor); else ed.appendChild(frag);
      changed();
    }
    function readImages(files) {
      for (const f of files) {
        if (!f.type.startsWith('image/')) continue;
        const r = new FileReader();
        r.onload = () => insertHTML('<img src="' + r.result + '">');
        r.readAsDataURL(f);
      }
    }
    ed.addEventListener('paste', e => {
      const files = [...(e.clipboardData?.files || [])];
      if (files.some(f => f.type.startsWith('image/'))) { e.preventDefault(); readImages(files); }
    });
    document.addEventListener('dragover', e => e.preventDefault());
    document.addEventListener('drop', e => {
      const files = [...(e.dataTransfer?.files || [])];
      if (files.length) { e.preventDefault(); readImages(files); }
    });
    window.webkit.messageHandlers.editor.postMessage('ready');
    </script></body></html>
    """
}

struct RichEditor: NSViewRepresentable {
    let controller: RichEditorController

    func makeNSView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.userContentController.add(controller, name: "editor")
        config.setURLSchemeHandler(CidSchemeHandler(), forURLScheme: "mailbox-cid")
        let web = WKWebView(frame: .zero, configuration: config)
        web.navigationDelegate = controller
        web.setValue(false, forKey: "drawsBackground")
        controller.attach(web)
        web.loadHTMLString(RichEditorController.page, baseURL: URL(string: "about:blank"))
        return web
    }

    func updateNSView(_ nsView: WKWebView, context: Context) {}
}

/// Formatting toolbar above the editor.
struct FormatBar: View {
    let editor: RichEditorController
    @State private var textColor = Color.primary
    @State private var showColors = false
    @State private var showLink = false
    @State private var linkURL = "https://"

    var body: some View {
        HStack(spacing: 2) {
            Menu {
                ForEach([("系统字体", "-apple-system"), ("苹方", "PingFang SC"), ("宋体", "Songti SC"), ("黑体", "Heiti SC"),
                         ("楷体", "Kaiti SC"), ("Helvetica", "Helvetica"), ("Times", "Times New Roman"), ("Courier", "Courier New")], id: \.1) { f in
                    Button(f.0) { editor.exec("fontName", f.1) }
                }
            } label: { Image(systemName: "textformat") }
            .menuStyle(.borderlessButton).frame(width: 36)
            Menu {
                Button("小") { editor.exec("fontSize", "2") }
                Button("正常") { editor.exec("fontSize", "3") }
                Button("大") { editor.exec("fontSize", "5") }
                Button("特大") { editor.exec("fontSize", "6") }
            } label: { Image(systemName: "textformat.size") }
            .menuStyle(.borderlessButton).frame(width: 36)
            Divider().frame(height: 16)
            tool("bold", "bold")
            tool("italic", "italic")
            tool("underline", "underline")
            tool("strikethrough", "strikethrough")
            Button { showColors = true } label: {
                Image(systemName: "paintbrush.pointed")
                    .overlay(alignment: .bottom) { Capsule().fill(textColor).frame(width: 14, height: 3).offset(y: 4) }
            }
            .help("文字颜色")
            .popover(isPresented: $showColors, arrowEdge: .bottom) {
                LazyVGrid(columns: Array(repeating: GridItem(.fixed(22), spacing: 6), count: 8), spacing: 6) {
                    ForEach(Self.palette, id: \.self) { hex in
                        Button {
                            textColor = Color(hex: hex)
                            editor.exec("foreColor", hex)
                            showColors = false
                        } label: {
                            RoundedRectangle(cornerRadius: 4).fill(Color(hex: hex)).frame(width: 22, height: 22)
                                .overlay(RoundedRectangle(cornerRadius: 4).stroke(Color.secondary.opacity(0.4)))
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(10)
            }
            Divider().frame(height: 16)
            tool("list.bullet", "insertUnorderedList")
            tool("list.number", "insertOrderedList")
            tool("decrease.indent", "outdent")
            tool("increase.indent", "indent")
            Button { editor.exec("formatBlock", "blockquote") } label: { Image(systemName: "text.quote") }.help("引用")
            Menu {
                Button("左对齐") { editor.exec("justifyLeft") }
                Button("居中") { editor.exec("justifyCenter") }
                Button("右对齐") { editor.exec("justifyRight") }
            } label: { Image(systemName: "text.alignleft") }
            .menuStyle(.borderlessButton).frame(width: 36)
            Divider().frame(height: 16)
            Button { showLink = true } label: { Image(systemName: "link") }.help("插入链接")
                .popover(isPresented: $showLink) {
                    HStack {
                        TextField("https://", text: $linkURL).frame(width: 260)
                        Button("插入") {
                            editor.exec("createLink", linkURL)
                            showLink = false
                        }
                    }
                    .padding()
                }
            Button { insertImage() } label: { Image(systemName: "photo") }.help("插入图片")
            Button {
                let row = "<tr>" + String(repeating: "<td>&nbsp;</td>", count: 3) + "</tr>"
                editor.insertHTML("<table>" + String(repeating: row, count: 3) + "</table><br>")
            } label: { Image(systemName: "tablecells") }.help("插入表格")
            Button { editor.exec("formatBlock", "pre") } label: { Image(systemName: "chevron.left.forwardslash.chevron.right") }.help("代码块")
            Button { editor.exec("insertHorizontalRule") } label: { Image(systemName: "minus") }.help("分隔线")
            Button { editor.exec("removeFormat") } label: { Image(systemName: "eraser") }.help("清除格式")
            Spacer()
        }
        .buttonStyle(.borderless)
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
    }

    static let palette = [
        "#000000", "#434343", "#666666", "#999999", "#B7B7B7", "#D9D9D9", "#EFEFEF", "#FFFFFF",
        "#C0392B", "#E67E22", "#F1C40F", "#27AE60", "#16A085", "#2980B9", "#8E44AD", "#D35400",
    ]

    private func tool(_ symbol: String, _ cmd: String) -> some View {
        Button { editor.exec(cmd) } label: { Image(systemName: symbol) }
    }

    private func insertImage() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.image]
        panel.allowsMultipleSelection = true
        guard panel.runModal() == .OK else { return }
        for url in panel.urls {
            guard let data = try? Data(contentsOf: url) else { continue }
            let ext = url.pathExtension.lowercased()
            let mime = ext == "png" ? "image/png" : ext == "gif" ? "image/gif" : "image/jpeg"
            editor.insertHTML("<img src=\"data:\(mime);base64,\(data.base64EncodedString())\">")
        }
    }
}
