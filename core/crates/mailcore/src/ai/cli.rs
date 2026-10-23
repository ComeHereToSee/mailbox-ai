//! Subscription command lines: the local `claude` (Claude Code) and `codex` (Codex) CLIs,
//! run as subprocesses in an empty working directory with the prompt on stdin.

use super::client::{Sink, split_system};
use super::model::*;
use crate::Engine;
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::mpsc;

/// A CLI call is stopped after this long.
const CLI_TIMEOUT: Duration = Duration::from_secs(300);

const CLAUDE_NOT_LOGGED_IN: &str = "Claude Code 未登录：请在终端运行 claude 并完成登录";
const CODEX_NOT_LOGGED_IN: &str = "Codex 未登录：请在终端运行 codex login";

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"))
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

/// Where npm, Homebrew and the installers usually put `claude` and `codex`.
pub fn candidate_dirs() -> Vec<PathBuf> {
    let h = home();
    let mut dirs = vec![
        h.join(".local/bin"),
        h.join(".claude/local"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        h.join(".npm-global/bin"),
        h.join(".bun/bin"),
        h.join(".volta/bin"),
        h.join(".cargo/bin"),
        h.join("Library/pnpm"),
    ];
    if let Some(nvm) = newest_nvm_bin(&h.join(".nvm/versions/node")) {
        dirs.push(nvm);
    }
    dirs
}

/// `~/.nvm/versions/node/<newest>/bin`.
fn newest_nvm_bin(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let version: Vec<u64> = name.trim_start_matches('v').split('.').map(|x| x.parse().unwrap_or(0)).collect();
        let bin = entry.path().join("bin");
        if !bin.is_dir() {
            continue;
        }
        let better = match &best {
            None => true,
            Some((b, _)) => version > *b,
        };
        if better {
            best = Some((version, bin));
        }
    }
    best.map(|(_, p)| p)
}

fn is_exec(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

/// Finds a CLI: the provider's `base_url` (a path), `MAILBOX_AI_CLI_DIR`, the usual install
/// directories, then the login shell's `PATH`.
pub async fn find_cli(name: &str, custom: &str) -> Option<PathBuf> {
    let custom = custom.trim();
    if !custom.is_empty() {
        let mut p = expand(custom);
        if p.is_dir() {
            p = p.join(name);
        }
        return if p.exists() { Some(p) } else { None };
    }
    if let Ok(dir) = std::env::var("MAILBOX_AI_CLI_DIR") {
        let p = expand(&dir).join(name);
        if is_exec(&p) {
            return Some(p);
        }
    }
    for d in candidate_dirs() {
        let p = d.join(name);
        if is_exec(&p) {
            return Some(p);
        }
    }
    login_shell_lookup(name).await
}

/// `/bin/zsh -lc 'command -v <name>'` (apps started from Finder have a minimal PATH).
async fn login_shell_lookup(name: &str) -> Option<PathBuf> {
    let shell = ["/bin/zsh", "/bin/bash"].into_iter().find(|s| Path::new(*s).exists())?;
    let mut cmd = Command::new(shell);
    cmd.arg("-lc")
        .arg(format!("command -v {name}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(5), cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let line = stdout.lines().map(str::trim).find(|l| l.starts_with('/'))?;
    let p = PathBuf::from(line);
    if is_exec(&p) { Some(p) } else { None }
}

/// PATH for the child: its own directory, the install directories, then the system PATH.
fn child_path(exe: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = exe.parent() {
        parts.push(d.to_string_lossy().to_string());
    }
    for d in candidate_dirs() {
        parts.push(d.to_string_lossy().to_string());
    }
    if let Ok(p) = std::env::var("PATH") {
        parts.extend(p.split(':').filter(|s| !s.is_empty()).map(String::from));
    }
    for d in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        parts.push(d.to_string());
    }
    let mut seen = std::collections::HashSet::new();
    parts.retain(|p| seen.insert(p.clone()));
    parts.join(":")
}

fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    super::truncate_plain(&lines[start..].join("\n"), 600)
}

fn not_found(name: &str, custom: &str) -> Error {
    if !custom.trim().is_empty() {
        return Error::Ai(format!("找不到 {}：请检查 设置 → AI 服务 中填写的路径", custom.trim()));
    }
    match name {
        "claude" => Error::Ai("未找到 claude：请先安装 Claude Code（npm i -g @anthropic-ai/claude-code）并在终端登录".into()),
        _ => Error::Ai("未找到 codex：请先安装 Codex（npm i -g @openai/codex）并在终端运行 codex login".into()),
    }
}

/// System prompt and a single prompt text (earlier turns are written out as a transcript).
fn flatten(req: &ChatRequest) -> Result<(Option<String>, String)> {
    let (system, msgs) = split_system(req);
    let prompt = match msgs.len() {
        0 => return Err(Error::Invalid("没有要发送给 AI 的内容".into())),
        1 => msgs[0].content.clone(),
        _ => {
            let mut s = String::from("下面是之前的对话记录，请接着回复最后一条用户消息（只输出你的回复）。\n\n");
            for m in &msgs {
                s.push_str(if m.role == "assistant" { "【助手】\n" } else { "【用户】\n" });
                s.push_str(&m.content);
                s.push_str("\n\n");
            }
            s
        }
    };
    Ok((system, prompt))
}

struct Spawned {
    child: Child,
    stdout: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr: tokio::task::JoinHandle<String>,
}

/// Starts the CLI in `<data_dir>/ai-workdir` (an empty directory) and writes the prompt to stdin.
fn spawn(e: &Engine, exe: &Path, args: &[String], input: &str) -> Result<Spawned> {
    let dir = e.paths.root.join("ai-workdir");
    std::fs::create_dir_all(&dir)?;
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .current_dir(&dir)
        .env("PATH", child_path(exe))
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|err| Error::Ai(format!("无法启动 {}：{err}", exe.display())))?;
    if let Some(mut stdin) = child.stdin.take() {
        let data = input.as_bytes().to_vec();
        // In the background: a long prompt can fill the pipe before the CLI starts reading.
        tokio::spawn(async move {
            let _ = stdin.write_all(&data).await;
            let _ = stdin.shutdown().await;
        });
    }
    let stdout = child.stdout.take().ok_or_else(|| Error::Ai("无法读取命令行输出".into()))?;
    let stderr_pipe = child.stderr.take();
    let stderr = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut s) = stderr_pipe {
            let _ = s.read_to_end(&mut buf).await;
        }
        let text = String::from_utf8_lossy(&buf).to_string();
        let skip = text.chars().count().saturating_sub(4000);
        text.chars().skip(skip).collect::<String>()
    });
    Ok(Spawned { child, stdout: BufReader::new(stdout).lines(), stderr })
}

fn model_arg(model: &str) -> Option<String> {
    let m = model.trim();
    if m.is_empty() || m == "default" { None } else { Some(m.to_string()) }
}

// ---- Claude Code --------------------------------------------------------------------------

pub async fn claude_chat(e: &Engine, p: &AiProvider, req: &ChatRequest, tx: Option<mpsc::Sender<String>>) -> Result<String> {
    let exe = find_cli("claude", &p.base_url).await.ok_or_else(|| not_found("claude", &p.base_url))?;
    let (system, prompt) = flatten(req)?;
    let mut sink = Sink::new(tx);
    match claude_once(e, &exe, &req.model.model, system.as_deref(), &prompt, true, &mut sink).await? {
        Some(text) => Ok(text),
        // An older CLI that does not know --include-partial-messages.
        None => match claude_once(e, &exe, &req.model.model, system.as_deref(), &prompt, false, &mut sink).await? {
            Some(text) => Ok(text),
            None => Err(Error::Ai("Claude Code 版本过旧，请更新后再试".into())),
        },
    }
}

#[derive(Default)]
struct ClaudeRead {
    got_delta: bool,
    /// (is_error, result text) of the `result` line.
    result: Option<(bool, String)>,
    /// Non-JSON output, for error messages.
    other: String,
}

/// One run. `Ok(None)` asks to retry without `--include-partial-messages`.
async fn claude_once(
    e: &Engine,
    exe: &Path,
    model: &str,
    system: Option<&str>,
    prompt: &str,
    partial: bool,
    sink: &mut Sink,
) -> Result<Option<String>> {
    let mut args: Vec<String> = vec!["-p".into(), "--output-format".into(), "stream-json".into(), "--verbose".into()];
    if partial {
        args.push("--include-partial-messages".into());
    }
    args.push("--max-turns".into());
    args.push("1".into());
    if let Some(m) = model_arg(model) {
        args.push("--model".into());
        args.push(m);
    }
    if let Some(s) = system.filter(|s| !s.trim().is_empty()) {
        args.push("--append-system-prompt".into());
        args.push(s.to_string());
    }
    args.push("--disallowedTools".into());
    for t in ["Bash", "Edit", "Write", "Read", "Glob", "Grep", "WebFetch", "WebSearch", "NotebookEdit", "Task"] {
        args.push(t.to_string());
    }
    let mut sp = spawn(e, exe, &args, prompt)?;
    let mut st = ClaudeRead::default();
    let read = read_claude(&mut sp.stdout, sink, &mut st);
    let outcome = tokio::time::timeout(CLI_TIMEOUT, read).await;
    match outcome {
        Err(_) => {
            let _ = sp.child.kill().await;
            return Err(Error::Ai("Claude Code 超过 300 秒没有完成，已停止".into()));
        }
        Ok(r) => r?,
    }
    let status = sp.child.wait().await?;
    let stderr = sp.stderr.await.unwrap_or_default();
    let output = format!("{}\n{}", st.other, stderr);
    if partial && st.result.is_none() && sink.text.is_empty() && output.to_lowercase().contains("unknown option") {
        return Ok(None);
    }
    match st.result {
        Some((false, text)) => {
            if sink.text.trim().is_empty() && !text.trim().is_empty() {
                sink.push(&text).await;
            }
            if sink.text.trim().is_empty() {
                return Err(Error::Ai("Claude Code 没有返回内容".into()));
            }
            Ok(Some(sink.text.clone()))
        }
        Some((true, text)) => Err(claude_error(&text, &output)),
        None if status.success() && !sink.text.trim().is_empty() => Ok(Some(sink.text.clone())),
        None => Err(claude_error("", &output)),
    }
}

async fn read_claude(lines: &mut tokio::io::Lines<BufReader<ChildStdout>>, sink: &mut Sink, st: &mut ClaudeRead) -> Result<()> {
    while let Some(line) = lines.next_line().await? {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(t) else {
            if st.other.len() < 8000 {
                st.other.push_str(t);
                st.other.push('\n');
            }
            continue;
        };
        let kind = v["type"].as_str().unwrap_or("").to_string();
        match kind.as_str() {
            "stream_event" => {
                let ev = &v["event"];
                if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                    if let Some(text) = ev["delta"]["text"].as_str() {
                        st.got_delta = true;
                        sink.push(text).await;
                    }
                }
            }
            "assistant" => {
                // Only when partial messages did not stream the text already.
                if !st.got_delta {
                    for part in v["message"]["content"].as_array().into_iter().flatten() {
                        if part["type"] == "text" {
                            if let Some(text) = part["text"].as_str() {
                                sink.push(text).await;
                            }
                        }
                    }
                }
            }
            "result" => {
                let is_error = v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str().is_some_and(|s| s != "success");
                let text = v["result"].as_str().unwrap_or("").to_string();
                st.result = Some((is_error, text));
            }
            _ => {}
        }
    }
    Ok(())
}

fn looks_logged_out(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("login") || lower.contains("log in") || lower.contains("invalid api key") || lower.contains("not logged in")
}

fn claude_error(result: &str, output: &str) -> Error {
    if looks_logged_out(&format!("{result}\n{output}")) {
        return Error::Ai(CLAUDE_NOT_LOGGED_IN.into());
    }
    let msg = if result.trim().is_empty() { last_lines(output, 5) } else { super::truncate_plain(result.trim(), 600) };
    Error::Ai(format!("Claude Code 出错：{}", if msg.is_empty() { "没有输出任何内容".to_string() } else { msg }))
}

// ---- Codex --------------------------------------------------------------------------------

pub async fn codex_chat(e: &Engine, p: &AiProvider, req: &ChatRequest, tx: Option<mpsc::Sender<String>>) -> Result<String> {
    let exe = find_cli("codex", &p.base_url).await.ok_or_else(|| not_found("codex", &p.base_url))?;
    let (system, prompt) = flatten(req)?;
    let input = match system {
        Some(s) if !s.trim().is_empty() => format!("{s}\n\n---\n\n{prompt}"),
        _ => prompt,
    };
    let last_file = e.paths.tmp().join(format!("codex-{}.txt", uuid::Uuid::new_v4().simple()));
    let mut args: Vec<String> = ["exec", "--skip-git-repo-check", "--sandbox", "read-only", "--color", "never", "--json"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if let Some(m) = model_arg(&req.model.model) {
        args.push("-m".into());
        args.push(m);
    }
    args.push("--output-last-message".into());
    args.push(last_file.to_string_lossy().to_string());
    args.push("-".into());
    let mut sp = spawn(e, &exe, &args, &input)?;
    let mut sink = Sink::new(tx);
    let mut st = CodexRead::default();
    let read = read_codex(&mut sp.stdout, &mut sink, &mut st);
    let outcome = tokio::time::timeout(CLI_TIMEOUT, read).await;
    match outcome {
        Err(_) => {
            let _ = sp.child.kill().await;
            let _ = std::fs::remove_file(&last_file);
            return Err(Error::Ai("Codex 超过 300 秒没有完成，已停止".into()));
        }
        Ok(r) => {
            if let Err(err) = r {
                let _ = std::fs::remove_file(&last_file);
                return Err(err);
            }
        }
    }
    let status = sp.child.wait().await?;
    let stderr = sp.stderr.await.unwrap_or_default();
    let last = std::fs::read_to_string(&last_file).unwrap_or_default();
    let _ = std::fs::remove_file(&last_file);
    let last = last.trim().to_string();
    if !last.is_empty() {
        if sink.text.trim().is_empty() {
            sink.push(&last).await;
        }
        return Ok(last);
    }
    if status.success() && !sink.text.trim().is_empty() {
        return Ok(sink.text.clone());
    }
    let output = format!("{}\n{}\n{}", st.errors.join("\n"), st.other, stderr);
    if looks_logged_out(&output) {
        return Err(Error::Ai(CODEX_NOT_LOGGED_IN.into()));
    }
    let detail = if st.errors.is_empty() { last_lines(&output, 5) } else { super::truncate_plain(&st.errors.join("；"), 600) };
    Err(Error::Ai(format!("Codex 出错：{}", if detail.is_empty() { "没有返回内容".to_string() } else { detail })))
}

#[derive(Default)]
struct CodexRead {
    messages: usize,
    errors: Vec<String>,
    other: String,
}

async fn read_codex(lines: &mut tokio::io::Lines<BufReader<ChildStdout>>, sink: &mut Sink, st: &mut CodexRead) -> Result<()> {
    while let Some(line) = lines.next_line().await? {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(t) else {
            if st.other.len() < 8000 {
                st.other.push_str(t);
                st.other.push('\n');
            }
            continue;
        };
        // New format: {"type":"item.completed","item":{"type":"agent_message","text":…}}
        // Old format: {"msg":{"type":"agent_message","message":…}}
        let item = &v["item"];
        let text = if v["type"] == "item.completed" && (item["type"] == "agent_message" || item["item_type"] == "assistant_message") {
            item["text"].as_str().map(String::from)
        } else if v["msg"]["type"] == "agent_message" {
            v["msg"]["message"].as_str().map(String::from)
        } else {
            None
        };
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            if st.messages > 0 {
                sink.push("\n\n").await;
            }
            sink.push(&text).await;
            st.messages += 1;
            continue;
        }
        let error = if v["type"] == "error" {
            v["message"].as_str().map(String::from)
        } else if v["type"] == "turn.failed" {
            v["error"]["message"].as_str().map(String::from)
        } else if v["msg"]["type"] == "error" || v["msg"]["type"] == "stream_error" {
            v["msg"]["message"].as_str().map(String::from)
        } else {
            None
        };
        if let Some(err) = error {
            st.errors.push(err);
        }
    }
    Ok(())
}

// ---- status -------------------------------------------------------------------------------

/// Output (stdout + stderr) of a short command, or None when it could not run in time.
async fn run_capture(exe: &Path, args: &[&str], secs: u64) -> Option<(bool, String)> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .env("PATH", child_path(exe))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(secs), cmd.output()).await.ok()?.ok()?;
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Some((out.status.success(), text))
}

pub async fn cli_status(name: &str, custom: &str) -> CliStatus {
    let Some(path) = find_cli(name, custom).await else {
        return CliStatus { found: false, path: None, version: None, logged_in: None, hint: not_found(name, custom).to_string() };
    };
    let version = run_capture(&path, &["--version"], 10)
        .await
        .and_then(|(_, out)| out.lines().map(str::trim).find(|l| !l.is_empty()).map(String::from));
    let logged_in = if name == "codex" {
        run_capture(&path, &["login", "status"], 10).await.map(|(ok, out)| ok && out.contains("Logged in"))
    } else {
        None
    };
    let shown = path.display().to_string();
    let hint = match (&version, logged_in) {
        (_, Some(false)) => format!("已找到 {shown}，但还没有登录：请在终端运行 codex login"),
        (Some(v), _) => format!("已找到 {shown}（{v}）"),
        (None, _) => format!("已找到 {shown}"),
    };
    CliStatus { found: true, path: Some(shown), version, logged_in, hint }
}

/// `GET /v1/ai/cli` → `{"claude": CliStatus, "codex": CliStatus}`.
pub async fn status(e: &Engine) -> Value {
    let providers = e.ai_providers();
    let custom = |id: &str| providers.iter().find(|p| p.id == id).map(|p| p.base_url.clone()).unwrap_or_default();
    let claude_path = custom("claude_code");
    let codex_path = custom("codex");
    let claude = cli_status("claude", &claude_path).await;
    let codex = cli_status("codex", &codex_path).await;
    json!({ "claude": claude, "codex": codex })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_for_multi_turn_chats() {
        let req = ChatRequest {
            system: Some("S".into()),
            messages: vec![ChatMessage::user("你好"), ChatMessage::assistant("你好！"), ChatMessage::user("再见")],
            ..Default::default()
        };
        let (system, prompt) = flatten(&req).unwrap();
        assert_eq!(system.as_deref(), Some("S"));
        assert!(prompt.contains("【用户】\n你好") && prompt.contains("【助手】\n你好！") && prompt.ends_with("再见\n\n"));
        let single = ChatRequest { messages: vec![ChatMessage::user("只有一句")], ..Default::default() };
        assert_eq!(flatten(&single).unwrap().1, "只有一句");
        assert!(flatten(&ChatRequest::default()).is_err());
    }

    #[test]
    fn model_argument() {
        assert_eq!(model_arg("default"), None);
        assert_eq!(model_arg(" "), None);
        assert_eq!(model_arg("sonnet").as_deref(), Some("sonnet"));
        assert!(looks_logged_out("Invalid API key · Please run /login"));
        assert!(!looks_logged_out("rate limited"));
    }
}
