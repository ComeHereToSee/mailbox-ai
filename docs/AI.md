# Mailbox AI：AI 功能设计与接口

本文是 AI 版在无 AI 版（Mailbox）之上新增内容的**唯一契约**。
- Rust 核心：`core/crates/mailcore/src/ai/`
- Swift 界面：`macos/`

两边都以本文为准，JSON 一律 snake_case（Swift 端由 `convertFromSnakeCase` 转换）。

## 1. 总体原则

- **服务接入方式参考 Cherry Studio**：
  - 「服务商 → 模型」两级结构，内置常用服务商预设。
  - 每个服务商可以填 API Key 和 API 地址，可以从服务商拉取模型列表，也可以手动添加模型，并提供「检查连接」。
  - 不同功能可以分别指定模型：默认（写作 / 对话）用强模型，快速任务（摘要、分类、提醒）用便宜的模型。
- **订阅直连**：Claude Code、Codex 两个服务商不需要 API Key，直接调用本机已登录的 `claude` / `codex` 命令行，走用户自己的订阅。
- **隐私**：
  - 默认不把加密邮件交给 AI（`allow_encrypted = false`）。
  - API Key 存在加密的 secrets 文件中，键名为 `ai:key:<provider_id>`，不写入数据库，也不在接口里回传。
  - 每次发送给 AI 的正文最多 `max_body_chars` 字符。
- **流式**：生成类接口返回 NDJSON 流（`Content-Type: application/x-ndjson`），每行一个 JSON：
  - `{"delta":"…"}`：增量文本，可以有多行。
  - `{"done":true,"text":"完整文本","model":"provider/model", ...额外字段}`：最后一行，表示成功结束。
  - `{"error":"中文错误说明"}`：最后一行，表示失败结束。

  在流开始之前就发现的错误（参数无效、未配置模型等）直接返回普通 HTTP 错误 `{"error","code"}`，与现有接口一致。

## 2. 数据模型（`ai/model.rs`）

```rust
/// Which wire protocol a provider speaks.
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Openai,       // 任何 OpenAI 兼容接口（OpenAI、DeepSeek、通义、Kimi、智谱、硅基流动、豆包、OpenRouter、Ollama、LM Studio…）
    Anthropic,    // Messages API
    Gemini,       // Google Generative Language API
    AzureOpenai,
    ClaudeCode,   // 本机 claude CLI（订阅）
    Codex,        // 本机 codex CLI（订阅）
}

pub struct ProviderPreset {
    pub id: String,             // "openai" "anthropic" "deepseek" …
    pub kind: ProviderKind,
    pub name: String,           // 显示名，如 "DeepSeek 深度求索"
    pub base_url: String,       // 默认 API 地址（CLI 类为空）
    pub website: String,
    pub key_url: Option<String>,   // 「获取 API Key」链接
    pub needs_key: bool,
    pub color: String,          // "#4D6BFE"
    pub models: Vec<String>,    // 推荐模型 id（拉取失败时的兜底）
    pub description: String,
}

pub struct AiModel {
    pub id: String,             // 发给服务商的模型 id；CLI 类的 "default" = 不传 --model / -m
    pub name: String,           // 显示名，默认等于 id
    pub group: String,          // 分组（如 "deepseek"、"gpt-4o"），UI 分组用，可空
    pub enabled: bool,
}

pub struct AiProvider {
    pub id: String,             // 预设 id；自定义服务商用 uuid
    pub kind: ProviderKind,
    pub name: String,
    pub base_url: String,       // CLI 类：可选，填写时作为可执行文件路径
    pub api_version: Option<String>, // Azure 用
    pub enabled: bool,
    pub models: Vec<AiModel>,
    pub has_key: bool,          // 仅输出
    pub custom: bool,           // 自定义服务商（可删除）
}

/// PUT /v1/ai/providers/{id} 的请求体 = AiProvider 的字段 + 可选 api_key。
/// api_key: Some("") 删除密钥；None 不改；Some(k) 保存。
pub struct AiProviderInput { #[serde(flatten)] pub provider: AiProvider, pub api_key: Option<String> }

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ModelRef { pub provider: String, pub model: String }

pub struct AiSettings {
    pub default_model: Option<ModelRef>,   // 写作、对话、问问邮箱、自动回复
    pub quick_model: Option<ModelRef>,     // 摘要、翻译、分类、智能提醒、Loop；为空时用 default_model
    pub language: String,                  // AI 输出语言："auto"（跟随邮件）| "zh-CN" | "en"；默认 "zh-CN"
    pub tone: String,                      // 默认写作语气："professional" | "friendly" | "concise"；默认 "professional"
    pub signature_name: String,            // AI 写信时的落款称呼，可空
    pub smart_reminders: bool,             // 新邮件智能分析，默认 true
    pub smart_scope: String,               // "primary"（默认，只分析主要分类）| "all" | "vip"
    pub auto_summary: bool,                // 打开长邮件（> 1500 字）时自动摘要，默认 false
    pub allow_encrypted: bool,             // 默认 false
    pub max_body_chars: usize,             // 默认 12000
    pub tts_voice: Option<String>,         // Swift 用，核心只存取
    pub tts_rate: f32,                     // 0.0–1.0，默认 0.5
}
```

`AiSettings` 存在 kv（space `ai`，id `settings`）。服务商列表存在 kv（space `ai_provider`）。第一次读取时用全部预设生成列表：
- 预设默认 `enabled = false`，但 `claude_code`、`codex`、`ollama` 例外。
- 每个模型取预设推荐模型，`enabled = true`。

### 预设服务商（id → kind, base_url）

| id | 名称 | kind | base_url | needs_key |
|---|---|---|---|---|
| `claude_code` | Claude Code（订阅） | claude_code | "" | 否 |
| `codex` | Codex（ChatGPT 订阅） | codex | "" | 否 |
| `openai` | OpenAI | openai | https://api.openai.com/v1 | 是 |
| `anthropic` | Anthropic Claude | anthropic | https://api.anthropic.com | 是 |
| `gemini` | Google Gemini | gemini | https://generativelanguage.googleapis.com | 是 |
| `deepseek` | DeepSeek 深度求索 | openai | https://api.deepseek.com/v1 | 是 |
| `dashscope` | 阿里云百炼（通义千问） | openai | https://dashscope.aliyuncs.com/compatible-mode/v1 | 是 |
| `moonshot` | 月之暗面 Kimi | openai | https://api.moonshot.cn/v1 | 是 |
| `zhipu` | 智谱 GLM | openai | https://open.bigmodel.cn/api/paas/v4 | 是 |
| `siliconflow` | 硅基流动 | openai | https://api.siliconflow.cn/v1 | 是 |
| `doubao` | 火山引擎（豆包） | openai | https://ark.cn-beijing.volces.com/api/v3 | 是 |
| `hunyuan` | 腾讯混元 | openai | https://api.hunyuan.cloud.tencent.com/v1 | 是 |
| `qianfan` | 百度千帆 | openai | https://qianfan.baidubce.com/v2 | 是 |
| `minimax` | MiniMax | openai | https://api.minimaxi.com/v1 | 是 |
| `stepfun` | 阶跃星辰 | openai | https://api.stepfun.com/v1 | 是 |
| `openrouter` | OpenRouter | openai | https://openrouter.ai/api/v1 | 是 |
| `groq` | Groq | openai | https://api.groq.com/openai/v1 | 是 |
| `mistral` | Mistral | openai | https://api.mistral.ai/v1 | 是 |
| `xai` | xAI Grok | openai | https://api.x.ai/v1 | 是 |
| `azure_openai` | Azure OpenAI | azure_openai | "" | 是 |
| `ollama` | Ollama（本地） | openai | http://localhost:11434/v1 | 否 |
| `lmstudio` | LM Studio（本地） | openai | http://localhost:1234/v1 | 否 |

预设表放在 `ai/presets.json`（含 website / key_url / color / models / description），用 `include_str!` 编入二进制。推荐模型写当前（2026 年）常见的 id，拿不准的宁可少写，用户可以「拉取模型」。
- Claude Code 的推荐模型：`default`、`sonnet`、`opus`、`haiku`（CLI 接受别名）。
- Codex 的推荐模型：`default`。

## 3. 调用层（`ai/client.rs`、`ai/cli.rs`）

```rust
pub struct ChatMessage { pub role: String /* "system" | "user" | "assistant" */, pub content: String }
pub struct ChatRequest { pub model: ModelRef, pub system: Option<String>, pub messages: Vec<ChatMessage>, pub temperature: Option<f32>, pub max_tokens: Option<u32>, pub json: bool }

impl Engine {
    /// Streams text deltas into `tx`; returns the full text.
    pub async fn ai_chat(&self, req: ChatRequest, tx: Option<tokio::sync::mpsc::Sender<String>>) -> Result<String>;
}
```

### HTTP 服务商
使用 `Inner.http`（reqwest），通过 `response.chunk()` 逐块读取 SSE，不要开启 reqwest 的 stream feature。

- **openai**
  - 请求：`POST {base}/chat/completions`，`Authorization: Bearer <key>`（没有 key 时不带此头），请求体 `{"model","messages","stream":true,"temperature"?,"max_tokens"?}`。
  - `json=true` 时加 `"response_format":{"type":"json_object"}`。如果服务商因此返回 400，就去掉这个字段重试一次。
  - 解析：`data: {...}` 中的 `choices[0].delta.content`；忽略 `reasoning_content`；遇到 `data: [DONE]` 结束。
- **anthropic**
  - 请求：`POST {base}/v1/messages`，头 `x-api-key`、`anthropic-version: 2023-06-01`，请求体 `{"model","system","messages","max_tokens": max_tokens.unwrap_or(4096),"stream":true}`。
  - 解析：`content_block_delta.delta.text`；`error` 事件转成错误。
- **gemini**
  - 请求：`POST {base}/v1beta/models/{model}:streamGenerateContent?alt=sse`，头 `x-goog-api-key`，请求体 `{"systemInstruction":{"parts":[{"text"}]},"contents":[{"role":"user"|"model","parts":[{"text"}]}]}`。
  - 解析：`candidates[0].content.parts[].text`。
- **azure_openai**：`POST {base}/openai/deployments/{model}/chat/completions?api-version={api_version|"2024-10-21"}`，头 `api-key`，其余同 openai。
- **HTTP 错误**：取响应 JSON 里的 `error.message`（或整段文本，截断到 300 字）作为错误信息。
  - 401 / 403 → 「API Key 无效或没有权限」。
  - 429 → 「请求太频繁或额度不足」。
- **超时**：连接 15 秒；两块数据之间超过 120 秒视为超时。

### 拉取模型列表（`POST /v1/ai/providers/{id}/models`）
- openai / azure：`GET {base}/models` → `data[].id`。
- anthropic：`GET {base}/v1/models` → `data[].id`。
- gemini：`GET {base}/v1beta/models` → `models[].name`（去掉 `models/` 前缀），只保留支持 `generateContent` 的。
- CLI 类：返回预设模型。
- `group` 字段：取 id 中第一个 `-` 或 `/` 之前的部分。

### 订阅 CLI（`ai/cli.rs`）

**查找可执行文件**，按以下顺序：
1. 服务商的 `base_url`：非空就当作路径使用。
2. 候选目录：
   - `~/.local/bin`
   - `~/.claude/local`
   - `/opt/homebrew/bin`
   - `/usr/local/bin`
   - `~/.npm-global/bin`
   - `~/.bun/bin`
   - `~/.volta/bin`
   - `~/.cargo/bin`
   - `~/Library/pnpm`
   - `~/.nvm/versions/node/*/bin`（取版本最新的一个）
3. 都没有时执行 `/bin/zsh -lc 'command -v <name>'`，拿登录 shell 的 PATH 来找。

**子进程环境**：
- `PATH` = 可执行文件所在目录 + 上述候选目录 + 系统 PATH。
- `cwd` = `<data_dir>/ai-workdir`（空目录），避免读到任何项目文件。
- stdin 写入提示词后关闭。
- 设置超时（默认 300 秒），超时后 kill。

**Claude Code（`claude`）**
- 参数：
  ```
  claude -p --output-format stream-json --verbose --include-partial-messages --max-turns 1 \
    [--model <m>] [--append-system-prompt <system>] \
    --disallowedTools Bash Edit Write Read Glob Grep WebFetch WebSearch NotebookEdit Task
  ```
  提示词从 stdin 传入。
- 逐行解析 JSON：
  - `{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":…}}}` → delta。
  - `{"type":"assistant","message":{"content":[{"type":"text","text":…}]}}`：只有前面没收到任何 delta 时，才把这段文本作为 delta 输出。
  - `{"type":"result","subtype":…,"is_error":…,"result":…}` → 结束。`is_error` 为真或 `subtype != "success"` 时报错，错误信息取 `result`，或 stderr 的最后几行。
- 如果 stderr 提示不认识 `--include-partial-messages`（输出含 "unknown option"），去掉这个参数重试一次。
- 如果输出提示未登录（含 "login" / "Invalid API key" / "/login"），报错「Claude Code 未登录：请在终端运行 claude 并完成登录」。

**Codex（`codex`）**
- 参数：
  ```
  codex exec --skip-git-repo-check --sandbox read-only --color never --json \
    [-m <m>] --output-last-message <tmpfile> -
  ```
  提示词从 stdin 传入。system 拼在提示词最前面：`<system>\n\n---\n\n<prompt>`。
- 逐行解析 JSONL，兼容两种格式：
  - `{"type":"item.completed","item":{"type":"agent_message","text":…}}`
  - `{"msg":{"type":"agent_message","message":…}}`

  这两种都作为 delta 输出（codex 不提供逐字流）。
- 进程结束后读取 `tmpfile` 作为最终文本；文件为空且退出码非 0 时报错。
- 未登录（输出含 "login" / "Not logged in"）→ 「Codex 未登录：请在终端运行 codex login」。

**状态**：`GET /v1/ai/cli` 返回 `{"claude": CliStatus, "codex": CliStatus}`。
```rust
pub struct CliStatus { pub found: bool, pub path: Option<String>, pub version: Option<String>, pub logged_in: Option<bool>, pub hint: String }
```
- version 取 `<cli> --version` 的第一行。
- codex 的 `logged_in` 用 `codex login status` 判断（退出码 0 且输出含 "Logged in" → true）。
- claude 的 `logged_in` 为 None。
- hint 示例：
  - 「已找到 /opt/homebrew/bin/claude（2.1.3）」
  - 「未找到 claude：请先安装 Claude Code（npm i -g @anthropic-ai/claude-code）并在终端登录」

## 4. 功能与接口（全部以 `/v1/ai` 为前缀，需要 token）

### 服务商与设置
| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/presets` | `[ProviderPreset]` |
| GET | `/providers` | `[AiProvider]`（按预设顺序，自定义的在后） |
| PUT | `/providers/{id}` | `AiProviderInput` → `AiProvider`；id 不存在则新建自定义服务商（`custom=true`） |
| DELETE | `/providers/{id}` | 自定义：删除；预设：恢复为预设默认值，并删除 key |
| POST | `/providers/{id}/models` | 拉取模型列表 → `[AiModel]`（不自动保存） |
| POST | `/providers/{id}/test` | `{model}` → `{ok, latency_ms, reply?, error?}`（让模型回复「OK」） |
| GET | `/cli` | 见上 |
| GET / PUT | `/settings` | `AiSettings` |
| POST | `/chat` | 流。`{model?: ModelRef, system?, messages}`；model 缺省用 `default_model` |

没有可用模型时（未设置 default_model，或者它所在的服务商未启用），返回 400，code 为 `ai_not_configured`，错误信息「还没有设置 AI 模型：请在 设置 → AI 服务 中选择」。
- 只设置了 quick_model 时，用它兜底 default_model；反过来也一样。

### 写信助手
`POST /compose`（流），请求体：
```json
{"op":"polish|formal|friendly|concise|expand|proofread|translate|write|reply|custom",
 "text":"当前正文纯文本（可空）","instruction":"用户补充要求（custom / write / reply 用）",
 "target_lang":"en|zh-CN|ja|…（translate 用）","message_id":123}
```
- `polish` / `formal` / `friendly` / `concise` / `expand` / `proofread` / `translate`：改写 `text`。
  - 保留原意和事实。
  - 不加解释，不加「以下是…」，只输出改写后的正文。
  - 用段落分隔，不用 Markdown 标题。
- `write`：根据 `instruction` 写一封新邮件的正文。如果 instruction 里明显包含主题，最后一行允许输出 `主题：…`，但通常只要正文。
- `reply`：根据原邮件（`message_id`，取整个会话）和 `instruction` 写回复正文，语气用 `tone`，落款用 `signature_name`。
- `done` 行额外带 `"subject": Option<String>`。如果输出最后一行以「主题：」开头，就把它拆出来放进 subject，并从正文中去掉。

### 阅读：摘要、代读、翻译、建议回复
| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/messages/{id}/summary` | 流。`{thread: bool = true, force: bool = false}`；有缓存且非 force 时直接输出一条 delta（完整文本）加 done。done 额外带 `"cached": bool` |
| GET | `/messages/{id}/summary` | 缓存的摘要 `{text, model, created_at}`，没有则 404 |
| POST | `/messages/{id}/translate` | 流。`{target_lang}`，输出译文（纯文本段落） |
| POST | `/messages/{id}/replies` | 非流。→ `{"suggestions":[{"label":"好的，周五前确认","text":"完整回复正文…"}]}`，3 条，label 不超过 12 个字 |
| GET | `/messages/{id}/insight` | 智能分析结果 `Insight`，没有则 404 |
| POST | `/messages/{id}/insight` | 立即分析并返回 `Insight`（非流） |

摘要格式（Markdown）：
- 第一行「**一句话**：…」。
- 然后 2–5 条要点。
- 有待办或截止日期时加「**待办**」小节。
- 不复述签名和免责声明。

缓存在 kv：
- space `ai_summary`，id `"{message_id}:{thread}"`。
- 会话里出现新邮件后（thread_count 改变），旧缓存视为失效。

代读：Swift 端用 AVSpeechSynthesizer 朗读摘要或正文，核心不需要额外接口。

### 智能提醒（新邮件分析）
```rust
pub struct Insight {
    pub message_id: i64,
    pub needs_reply: bool,
    pub priority: String,          // "high" | "normal" | "low"
    pub deadline: Option<i64>,     // unix 秒，从正文推断（「周五前」→ 本周五 18:00 本地时间）
    pub deadline_text: Option<String>, // 原文中的说法
    pub todos: Vec<String>,
    pub one_line: String,          // ≤ 40 字的一句话摘要
    pub remind_at: Option<i64>,    // deadline 前 1 天 9:00；距离不到 1 天则为 deadline 前 2 小时；需要回复但没有截止时间时为 null
    pub done: bool,
    pub created_at: i64,
}
```
- **触发**：
  - `smart_reminders` 打开、新邮件进入收件箱、符合 `smart_scope` 时，在后台队列里分析，不阻塞同步。
  - 用 `quick_model`，`json=true`。
  - 提示词里要给出当前日期、星期和时区，以便换算相对日期。
- **存储**：kv space `ai_insight`，id 为 message_id。存好后发事件 `ai_insight {message_id}`。
- **到点提醒**：housekeeping 每分钟检查 `remind_at <= now && !done && !reminded` 的记录，发事件 `ai_reminder {message_id, title, body}`，同时标记 reminded。
  - title：`⏰ [账户标签] 发件人`。
  - body：`主题 · 截止 deadline_text`。
  - 助手 App 收到后弹系统通知，点击打开对应邮件。
- **接口**：

  | 方法 | 路径 | 说明 |
  |---|---|---|
  | GET | `/reminders` | `[{message: MessageSummary, account_label, insight}]`。条件：`!done`，且（needs_reply 或 deadline 或 todos 非空）。排序：有 deadline 的按 deadline 升序，其余按邮件时间倒序 |
  | POST | `/reminders/{message_id}/done` | `{done: bool}` |

- **同步回复状态**：用户回复了这封邮件（engine 设置 answered 标志）后，自动把 `needs_reply` 置为 false。

### 批量处理
| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/batch` | `{ids:[…], op:"summarize|classify|todos|draft_replies|custom", instruction?}` → `{job_id}` |
| GET | `/jobs/{id}` | `BatchJob` |
| POST | `/jobs/{id}/cancel` | |
| POST | `/jobs/{id}/apply` | `{message_ids:[…]}`：执行这些条目的 `suggested`（只对 classify 有效） |

```rust
pub struct BatchJob { pub id: String, pub op: String, pub status: String /* running|done|failed|cancelled */, pub total: usize, pub done: usize,
                      pub items: Vec<BatchItem>, pub summary: Option<String>, pub created_at: i64 }
pub struct BatchItem { pub message_id: i64, pub subject: String, pub from: String, pub output: String, pub error: Option<String>,
                       pub suggested: Vec<SuggestedAction>, pub draft_id: Option<String> }
pub struct SuggestedAction { pub action: String /* archive|trash|star|read|label|move|category */, pub value: Option<String>, pub reason: String }
```
- **执行**：每封邮件单独调用 `quick_model`，并发 3 个。每完成一封发事件 `ai_job {job_id, done, total, status}`。
- **各 op 的输出**：
  - `summarize`：每封一句话；全部完成后再调用一次模型，生成整体 `summary`（Markdown）。
  - `classify`：
    - 输出 JSON `{"label":"…","category":"primary|social|promotions|updates|forums","action":"keep|archive|trash","reason":"…"}`。
    - 转成 suggested：label → `label`；category 与当前分类不同 → `category`；action 为 archive / trash → 对应动作。
    - 用户确认后通过 apply 执行，复用 `Engine::act`：label 用 `Action::Label { add: vec![label], remove: vec![] }`，category 用 `Action::Category { category }`，archive / trash / star / read 用对应的 `Action`。
  - `todos`：输出待办列表；`summary` 汇总全部待办。
  - `draft_replies`：为每封生成回复草稿并保存为本地草稿（`save_draft` + `sync_draft`），写入 `draft_id`。
  - `custom`：对每封执行 `instruction`；`summary` 做总体汇总。
- **存储**：任务存在内存里（`Inner` 中的 `Mutex<HashMap>`），保留最近 20 个。

### Loop（定时 AI 任务）
```rust
pub struct AiLoop {
    pub id: String, pub name: String, pub enabled: bool,
    pub schedule: LoopSchedule,
    pub query: String,             // 搜索语法，例如 "is:unread newer_than:1d"；空表示 unified:inbox 的最近邮件
    pub instruction: String,       // 对这批邮件做什么
    pub max_messages: usize,       // 默认 30
    pub notify: bool,              // 完成后发系统通知（默认 true）
    pub email_digest: bool,        // 同时作为一封邮件投递到本地收件箱（默认 false）
    pub model: Option<ModelRef>,   // 默认 quick_model
    pub last_run: Option<i64>, pub next_run: Option<i64>,
}
pub struct LoopSchedule { pub kind: String /* "daily" | "weekdays" | "weekly" | "interval" */, pub time: String /* "08:30" */,
                          pub weekday: Option<u8> /* 1=周一…7，weekly 用 */, pub every_minutes: Option<u32> /* interval 用，≥ 30 */ }
pub struct LoopRun { pub id: String, pub loop_id: String, pub started_at: i64, pub finished_at: Option<i64>, pub status: String,
                     pub message_count: usize, pub output: String /* Markdown */, pub error: Option<String> }
```
| 方法 | 路径 | 说明 |
|---|---|---|
| GET / POST | `/loops` | 列表 / 保存（id 为空则新建并计算 next_run） |
| DELETE | `/loops/{id}` | |
| POST | `/loops/{id}/run` | 立即运行 → `{run_id}`（后台运行） |
| GET | `/loops/{id}/runs` | 最近 20 次运行 |
| GET | `/loop-templates` | 内置模板 `[AiLoop]`（id 为空） |

- **调度**：housekeeping 每分钟检查到期的 Loop，在后台运行，然后更新 last_run / next_run。时间按本地时区计算，用 chrono-tz 和系统时区。
- **运行**：
  - 按 query 搜索最近 `max_messages` 封邮件，把「发件人 / 主题 / 时间 / 摘要片段（snippet）」列表连同 instruction 交给模型。
  - 邮件不超过 10 封时附带正文前 1500 字。
  - 输出 Markdown。没有匹配邮件时不调用模型，output 为「没有符合条件的邮件」。
- **完成**：发事件 `ai_loop {loop_id, run_id, name, status, preview}`，preview 为输出前 120 字的纯文本。
  - `notify` 时，助手 App 弹通知：标题 `✨ name`，正文 preview。
- **email_digest**：
  - 把输出转成 HTML，作为一封来自「Mailbox AI <ai@mailbox.local>」的邮件插入第一个账户的收件箱。
  - 只存在本地：`folder_local`，不上传服务器；分类 primary。
- **内置模板**：
  1. **每日早报**：每天 08:00；`is:unread newer_than:1d`；「按重要程度总结这些未读邮件，列出需要我今天处理的事项。」
  2. **待回复检查**：工作日 17:30；`newer_than:3d -in:sent`；「找出需要我回复但我还没回复的邮件，给出建议回复要点。」
  3. **每周账单汇总**：每周一 09:00；`newer_than:7d (账单 OR 发票 OR invoice OR receipt)`；「汇总本周的账单 / 发票：商家、金额、到期日，最后给出合计。」
  4. **订阅清理建议**：每周日 20:00；`category:promotions newer_than:7d`；「统计本周营销邮件来自哪些发件人，建议哪些可以退订。」

### 自动回复
```rust
pub struct AutoReplyRule {
    pub id: String, pub name: String, pub enabled: bool,
    pub account_id: Option<String>,
    pub from_contains: String, pub subject_contains: String,   // 空表示不限
    pub ai_match: String,          // 可空。不为空时让 quick_model 判断「这封邮件是否属于：<ai_match>」→ {"match":bool}
    pub instruction: String,       // 如何回复
    pub mode: String,              // "draft"（默认，生成草稿并通知）| "send"（直接发送，走正常发件箱和撤销发送）
    pub max_per_day: u32,          // 默认 20
    pub once_per_sender_hours: u32,// 同一发件人多少小时内只回一次，默认 24
    pub sent_today: u32, pub day: String, // 内部计数
}
```
| 方法 | 路径 | 说明 |
|---|---|---|
| GET / POST | `/autoreply` | 列表 / 保存 |
| DELETE | `/autoreply/{id}` | |

- **触发**：新邮件进入收件箱时（与智能提醒同一个后台队列）依次检查启用的规则，第一条匹配的规则生效。
- **不回复的邮件**：
  - 自己发的邮件。
  - 带 `List-Unsubscribe` 或 `Precedence: bulk/list/junk` 的邮件。
  - 带 `Auto-Submitted` 头且值不是 `no` 的邮件。
  - 发件人地址含 noreply / no-reply / mailer-daemon 的邮件。
- **生成**：用 `default_model`，与 `/compose` 的 reply 相同。
  - 回复草稿用 `Engine::reply_draft(message_id, ComposeMode::Reply)` 得到引用格式，再把 AI 正文放在最前面。
- **mode=send**：加信头 `Auto-Submitted: auto-replied`，避免回复循环。
- **完成**：发事件 `ai_autoreply {message_id, rule_name, draft_id?, sent, subject}`。助手 App 弹通知「✨ 已为 发件人 生成回复草稿」或「已自动回复 发件人」。

### 问问邮箱
`POST /ask`（流），请求体 `{question, mailbox?: String}`。
- **检索**：
  1. 用 `quick_model` 把问题改写成 1–3 组搜索词（JSON `{"queries":["…"]}`，可以用搜索语法）。
  2. 用现有本地搜索各取前 8 封，去重。
  3. 结果为空时退回关键词直接搜索。
- **回答**：把这些邮件的「编号、发件人、日期、主题、正文前 1200 字」交给 `default_model` 回答。要求：
  - 引用时用 `[1]` `[2]` 标注。
  - 不知道就说不知道。
- **done**：done 行带 `"sources":[{"index":1,"message_id":…,"subject":…,"from":…,"date":…}]`。

## 5. 事件（加到 `events::Event`，WebSocket 原样下发）
```
ai_insight    { message_id }
ai_reminder   { message_id, title, body }
ai_job        { job_id, done, total, status }
ai_loop       { loop_id, run_id, name, status, preview }
ai_autoreply  { message_id, rule_name, draft_id, sent, subject }
```

## 6. 测试（只在 GitHub Actions 上运行）
`tests/ai.rs` 不依赖外部服务：
- 在测试里用 axum 起一个假的 OpenAI 兼容服务器和一个假的 Anthropic 服务器，返回 SSE 流。
- 用 shell 脚本伪造 `claude` / `codex` 可执行文件，输出示例 JSON 行和 last-message 文件。
- 通过 `MAILBOX_AI_CLI_DIR` 环境变量，或服务商的 `base_url` 指定这些伪造的可执行文件。

覆盖范围：
- 流式解析（含分块边界）。
- 未配置模型时的错误。
- compose / summary 缓存。
- 用演示数据生成 insight JSON 和 reminders。
- batch classify 与 apply。
- Loop 的运行和 next_run 计算（纯函数单测）。
- 自动回复生成草稿，以及跳过 noreply 和 bulk 邮件。
- ask 返回的 sources。

## 7. Swift 界面（`macos/`）

### 设置 → AI 服务（tab id `ai`，图标 `sparkles`）
- **布局**：仿 Cherry Studio，左右分栏。
  - 左侧：服务商列表（圆形色块图标、名称、启用开关），底部有「添加自定义服务商」。
  - 右侧：所选服务商的配置：
    - API Key（SecureField + 显示/隐藏，「获取密钥」链接）。
    - API 地址（显示预设值，可以修改，可以重置）。
    - Azure 的 api_version。
    - 「检查连接」按钮：先选模型再测试，显示耗时或错误。
    - 模型列表：按 group 分组，每个模型有启用开关；「拉取模型」按钮弹出多选列表；可以手动添加模型。
- **Claude Code / Codex**：
  - 显示 `GET /v1/ai/cli` 的状态（是否找到、路径、版本、是否已登录）和自定义路径输入框。
  - 显示说明「使用你在本机终端登录的订阅，不需要 API Key」，以及安装 / 登录命令（可以复制）。

### 设置 → AI 功能（tab id `ai_features`，图标 `wand.and.stars`）
- 默认模型、快速模型：Picker，列出所有已启用服务商下已启用的模型，格式为「服务商 · 模型」。
- 输出语言、写作语气、落款称呼。
- 智能提醒开关和范围；打开长邮件时自动摘要；允许处理加密邮件。
- 朗读：系统语音选择（中文语音优先）、语速，以及「试听」按钮。
- 自动回复规则列表和编辑器，其中「直接发送」要二次确认。
- Loop 列表：
  - 新建时可以从模板选择。
  - 编辑器包括：名称、频率、时间、搜索条件、指令、通知、投递到收件箱、模型。
  - 列表上可以立即运行，可以查看最近运行结果（Markdown 渲染）。

### 阅读区
- **工具栏「✨」菜单**：总结这封 / 总结整个会话、朗读摘要、朗读全文、翻译成中文 / 英文、建议回复、分析待办（insight）、问问 AI（打开助手面板，并带上这封邮件作为上下文）。
- **会话顶部的 AI 卡片**（可折叠）：流式显示摘要，有「朗读」「复制」「重新生成」三个按钮。
- **insight 标签**：在邮件头部下方显示一行小标签：「需回复」「截止：周五」「高优先级」。
- **建议回复 chips**：显示在正文下方。点击后打开回复窗口，正文预填 AI 文本。
- **翻译**：在卡片中显示译文。

### 写信窗口
- **工具栏「✨ AI」菜单**：
  - 改写：润色、更正式、更友好、更简洁、扩写、纠错、翻译成英文 / 中文。
  - 「帮我写…」：输入要求后生成。
  - 「自定义指令…」。
- **作用范围**：有选中文本时只改写选区（RichEditorController 需要新增 `selectedText()` 和 `replaceSelection(html:)`）；否则改写全文。签名区 `#mailbox-signature` 不参与改写。
- **结果面板**：生成过程在侧边弹出的面板中流式显示，有「替换」「插入到下方」「重新生成」「放弃」。
- **回复 / 全部回复时**：额外显示一行「✨ 一键回复」输入框，例如输入「同意，下周二可以」后生成完整回复。

### 邮件列表
- 多选后，右键菜单和工具栏出现「✨ AI 批量处理…」。
- 弹出的面板依次包括：
  1. 选择操作和指令。
  2. 进度条。
  3. 结果表格：可以勾选 classify 的建议，再点「应用所选」。
  4. 汇总区：Markdown 渲染，可以复制。

### 侧边栏
- 「智能邮箱」区新增「✨ 待处理」（数据来自 `GET /v1/ai/reminders`），每行显示截止时间和 one_line。
- 新增「✨ AI 助手」入口：打开助手窗口（`Window id "assistant"`）。
  - **问问邮箱**：对话形式，回答中的 `[n]` 可以点击打开对应邮件。
  - **Loop**：显示最近运行结果。

### 助手 App（菜单栏）
- 处理 `ai_reminder`、`ai_loop`、`ai_autoreply` 事件，弹出 UNUserNotification，并带上对应链接：
  - reminder 打开 `mailboxai://message/<id>`。
  - loop 打开 `mailboxai://assistant?loop=<id>`。
  - autoreply 打开草稿。
- 菜单中显示「✨ 待处理 N」。

### URL
- `mailboxai://assistant`、`mailboxai://assistant?loop=<id>`
- `mailboxai://settings?tab=ai`、`mailboxai://settings?tab=ai_features`

### 演示数据
`--demo` 时调用 `POST /v1/ai/demo`，由核心写入：
- 若干 insight。
- 一个已运行的 Loop（带运行结果）。
- 一个摘要缓存。
- 默认模型设为 `claude_code/default`。

这样 CI 截图时不需要真实的模型，也能看到 AI 卡片和待处理列表。

## 8. 补充功能（A1–A3 剩余部分）

本节是第二批功能的契约，规则与前文相同：JSON 用 snake_case，流式接口用 NDJSON，错误格式为 `{error, code}`。

### 8.1 日程提取（A2）
```rust
pub struct ExtractedEvent {
    pub title: String,
    pub start: i64,              // unix 秒
    pub end: Option<i64>,        // 缺省时由 UI 按 1 小时处理
    pub all_day: bool,
    pub location: Option<String>,
    pub notes: String,           // 一句话说明
    pub source_text: String,     // 原文中的说法，例如「下周二下午 3 点」
}
```
- **接口**：`POST /v1/ai/messages/{id}/events`（非流），返回 `{"events":[ExtractedEvent]}`。
  - 用 `quick_model`，`json=true`。提示词要给出当前日期、星期和时区。
  - 缓存在 kv `ai_events`（id 为 message_id）；请求体 `{force: bool}` 为 true 时重新提取。
- **与智能分析合并**：`Insight` 新增 `events: Vec<ExtractedEvent>`（serde default）。智能分析时顺带提取日程，同时写入 `ai_events` 缓存。
- **UI**：
  - 阅读区 ✨ 菜单增加「提取日程到日历」，打开一个表格：每行可以修改标题和时间，点「加入日历」后用现有的 `CalendarService`（EventKit）写入日历。
  - insight 标签增加「📅 日程 N」，点击打开同一个表格。

### 8.2 内置邮箱 MCP 服务（A3）
让 Claude Code、Codex、Cherry Studio 等 MCP 客户端直接操作邮箱。

- **开关与令牌**：
  - `AiSettings.mcp_enabled: bool`，默认 false。关闭时 `/mcp` 返回 HTTP 403，JSON-RPC 错误信息为「MCP 未启用：请在 设置 → AI 功能 → MCP 中开启」。
  - MCP 使用单独的固定令牌：存在 secrets 中，键名 `ai:mcp_token`。第一次读取时自动生成（32 字节随机数，hex 编码），可以重新生成。
  - `/mcp` 接受 `Authorization: Bearer <mcp_token>`，也接受原有的每次启动随机生成的 API token。
- **传输**：Streamable HTTP，`POST /mcp`（与现有本地 API 在同一端口），只用 JSON 响应（`Content-Type: application/json`），不用 SSE。
  - 请求是单个 JSON-RPC 2.0 对象。
  - 通知类消息（没有 id）返回 HTTP 202，无响应体。
  - `GET /mcp` 返回 405。
- **支持的方法**：
  - `initialize`：返回 `{protocolVersion, capabilities:{tools:{}}, serverInfo:{name:"mailbox-ai", version}, instructions}`。protocolVersion 回显客户端请求的版本（如果在支持列表 `["2025-06-18","2025-03-26","2024-11-05"]` 中），否则返回 `2025-06-18`。instructions 用中文简要说明可用工具和「发送 / 删除需要用户在 Mailbox AI 中确认」。
  - `ping`
  - `tools/list`
  - `tools/call`：返回 `{content:[{type:"text",text}], isError}`。text 为易读的中文和 JSON 混排，列表用简洁的 JSON。
- **工具**（name 与 inputSchema 都要写全，description 用中文）：

  | 工具 | 参数 | 说明 |
  |---|---|---|
  | `list_accounts` | — | 账户列表：id、email、label |
  | `search_emails` | `query`（搜索语法）, `mailbox?`, `limit?`（≤ 50，默认 20） | 返回 id、from、subject、date、snippet、unread、account |
  | `list_unread` | `limit?` | 统一收件箱中的未读邮件 |
  | `read_email` | `id` | 信头加正文纯文本（最多 20000 字）。遵守 `allow_encrypted` 和账户 AI 策略（见 8.6） |
  | `read_thread` | `id` | 整个会话 |
  | `get_pending` | — | `/v1/ai/reminders` 的内容 |
  | `draft_email` | `to[]`, `cc[]?`, `subject`, `body`, `account?` | 生成本地草稿并同步到服务器 → `{draft_id}`，不需要确认 |
  | `reply_email` | `id`, `body`, `reply_all?` | 生成回复草稿 → `{draft_id}` |
  | `send_email` | `draft_id`，或者 `to[]` / `cc[]?` / `bcc[]?` / `subject` / `body` / `account?` | **需要用户确认**，见下文 |
  | `mark_emails` | `ids[]`, `action`（read / unread / star / unstar / archive） | 不需要确认 |
  | `delete_emails` | `ids[]` | **需要用户确认**，确认后移到废纸篓 |

  body 是纯文本，转成 HTML 后发送：转义后按空行分段落，单个换行变成 `<br>`。
- **用户确认**：
  ```rust
  pub struct Confirmation { pub id: String, pub kind: String /* "send" | "delete" */, pub title: String, pub detail: String,
                            pub draft_id: Option<String>, pub message_ids: Vec<i64>, pub created_at: i64, pub expires_at: i64 }
  ```
  1. 工具调用先创建确认请求（send 先存为草稿），发事件 `ai_confirm {id, kind, title, detail}`，然后在 HTTP 请求里等待用户决定，最长 180 秒。
  2. 用户通过 `POST /v1/ai/confirm/{id}`（`{approve: bool}`）做出决定。
  3. 结果：
     - **同意**：send 走正常发件箱，带撤销发送延迟，返回「已发送（N 秒内可在 Mailbox AI 中撤销）」；delete 执行移到废纸篓。
     - **拒绝或超时**：返回 `isError: false`，文本为「用户拒绝了…（已保留草稿 draft_id）」或「等待确认超时…」。
  4. `GET /v1/ai/confirm` 列出待确认的请求。
- **其他接口**：
  - `GET /v1/ai/mcp` → `{enabled, url:"http://127.0.0.1:<port>/mcp", token, stdio_command}`。
    - `stdio_command` 由 Swift 端补全，核心返回空字符串。
    - 这里的 port 是实际监听的端口。
  - `POST /v1/ai/mcp/token`：重新生成令牌 → `{token}`。
- **stdio 桥（Swift）**：`MailboxAIHelper --mcp` 不启动 GUI。
  - 读取 `daemon.json` 中的端口，从 `GET /v1/ai/mcp`（用 daemon token）取得 mcp token。
  - 从 stdin 逐行读取 JSON-RPC，转发给 `POST /mcp`，把响应写到 stdout，一行一个。通知类消息不输出。
  - 如果引擎没有运行：先 `open -g -b com.comeheretosee.mailbox.ai.helper` 启动助手 App，等 daemon.json 可用后再转发。
- **设置 → AI 功能 → MCP**：
  - 「启用 MCP 服务」开关、URL、令牌（可复制、可重新生成）。
  - 一键复制的配置片段：
    - Claude Code：`claude mcp add --transport http mailbox-ai <url> --header "Authorization: Bearer <token>"`
    - Codex：`~/.codex/config.toml` 的 stdio 配置（`[mcp_servers.mailbox-ai] command = "<helper 路径>" args = ["--mcp"]`）。
    - Cherry Studio / 通用 JSON：streamableHttp 的 url 加 headers。
  - 待确认列表。
- **确认界面**：
  - **主 App**：收到 `ai_confirm` 事件时弹出模态窗口，显示标题、详情（收件人、主题、正文前 500 字，或要删除的邮件列表），按钮「允许」「拒绝」。
  - **助手 App**：同时发一条通知，带「允许」「拒绝」两个操作（category `AI_CONFIRM`）。主 App 没开时也能处理。
  - 任意一方做出决定后，另一方忽略即可；重复提交返回 404，可以忽略。

### 8.3 外部 MCP 工具（A3，应用调用外部工具）
```rust
pub struct McpServerConfig { pub id: String, pub name: String, pub enabled: bool,
    pub transport: String /* "stdio" | "http" */, pub command: String, pub args: Vec<String>,
    pub env: BTreeMap<String,String>, pub url: String, pub headers: BTreeMap<String,String> }
```
- **接口**：
  - `GET` / `POST /v1/ai/mcp-servers`：列表 / 保存。
  - `DELETE /v1/ai/mcp-servers/{id}`
  - `POST /v1/ai/mcp-servers/{id}/test` → `{ok, tools:[{name, description}], error?}`
- **客户端（`ai/mcp_client.rs`）**：
  - **stdio**：启动子进程（PATH 处理同 cli.rs），JSON-RPC 按行收发，先 `initialize` 再发 `notifications/initialized`。
  - **http**：POST JSON-RPC。响应可能是 JSON，也可能是 `text/event-stream`（取 data 中带相同 id 的那条）；保留 `Mcp-Session-Id` 头。
  - 超时：单次调用 60 秒。
  - 每次对话结束后关闭 stdio 子进程，不做常驻。
- **使用位置**：AI 助手的对话。`POST /v1/ai/chat` 和 `/ask` 请求体新增 `tools: bool`（默认 false），为 true 且有启用的服务器时启用工具：
  - **openai / azure / anthropic / gemini**：
    - 把各服务器的工具转换为对应格式：
      - OpenAI：`tools:[{type:"function",function:{name,description,parameters}}]`
      - Anthropic：`tools:[{name,description,input_schema}]`
      - Gemini：`tools:[{functionDeclarations:[…]}]`
    - 工具名统一为 `<server>__<tool>`，只保留 `[A-Za-z0-9_-]`，最长 64 个字符。
    - 以非流式方式循环调用：模型发起工具调用 → 执行 → 把结果作为 tool 消息回传，最多 8 轮。
    - 最后的文本作为一条 delta 输出，再输出 done。
  - **claude_code**：把启用的服务器写成临时 `--mcp-config` JSON，追加参数 `--allowedTools mcp__<server>`（每个服务器一个），`--max-turns` 改为 8。内置的读写文件和执行命令类工具仍然禁用。
  - **codex**：不支持，忽略 tools 并在 done 里给出 `"tools_note":"Codex 暂不支持外部工具"`。
  - done 行额外带 `"tool_calls":[{"server","tool","ok":bool}]`。
- **UI**：
  - 设置 → AI 功能 → MCP：在「外部工具」小节中增删改服务器，可以「测试」并列出工具。
  - AI 助手输入框旁有「🔧 使用工具」开关；回答下方显示调用过的工具。

### 8.4 服务商与模型（A1 细项）
- **多 Key 轮流**：
  - `api_key` 可以填多个，用英文逗号或换行分隔，原样存在 secrets 中。
  - 每次请求轮流取下一个（每个服务商一个计数器）。
  - 遇到 401 / 403 / 429 时换下一个 Key 重试一次。
  - `AiProvider` 新增 `key_count: usize`（仅输出）。
- **模型能力**：`AiModel` 新增 `capabilities: Vec<String>`，取值 `"vision" | "reasoning" | "tools" | "embedding"`，serde default 为空。
  - 拉取模型和读取预设时按 id 自动推断（不区分大小写）：
    - embedding：含 `embed`。
    - vision：含 `vision`、`vl`、`gpt-4o`、`gpt-4.1`、`gpt-5`、`claude`、`gemini`、`glm-4v`、`qwen-vl`、`omni`、`pixtral`、`llava`。
    - reasoning：含 `reason`、`r1`、`o1`、`o3`、`o4`、`thinking`、`qwq`。
    - tools：非 embedding 的 gpt、claude、gemini、qwen、deepseek-chat、deepseek-v、glm、kimi、moonshot、mistral、grok。
  - UI 中每个模型显示能力小标签，可以手动开关。
- **专用模型**：`AiSettings` 新增 `vision_model: Option<ModelRef>`、`embedding_model: Option<ModelRef>`。
  - 看图：用于解读图片附件（8.5）。为空时，如果 default_model 带 vision 能力就用它，否则报错「需要支持看图的模型：请在 AI 功能中设置看图模型」。
  - 嵌入：用于问问邮箱的语义重排。设置后，`/ask` 先用全文检索取 30 封候选，再按「问题 / 主题 + 摘要片段」的余弦相似度取前 8 封。
    - 支持 openai（`POST {base}/embeddings`）和 gemini（`:batchEmbedContents`）。
    - 其他服务商不支持时，忽略设置，并在日志中提示。
- **用量**：
  - 每次 AI 调用记录一条：日期（本地时间）、服务商、模型、功能（compose / summary / insight / batch / loop / autoreply / ask / chat / events / explain / mcp / embedding / test）、输入和输出 token、耗时、成功与否。
  - 按「日期 + 服务商 + 模型 + 功能」聚合后存进 kv `ai_usage`。
  - 各服务商的 token 来源：
    - **openai**：请求加 `stream_options:{include_usage:true}`。服务商因此返回 400 时，去掉这个字段重试，并记住该服务商不支持。
    - **anthropic**：`message_start` 和 `message_delta` 中的 usage。
    - **gemini**：`usageMetadata`。
    - **claude**：result 行的 `usage`。
    - **codex**：能取到就取，取不到记 0。
  - `GET /v1/ai/usage?days=30` → `{"days":[{date,calls,input_tokens,output_tokens,errors}], "by_model":[{provider,model,calls,input_tokens,output_tokens}], "by_feature":[{feature,calls,input_tokens,output_tokens}]}`
  - UI：AI 功能 → 用量，用 Swift Charts 画每日柱状图，下面附按模型和按功能的表格。
- **生成主题**：`/compose` 新增 op `subject`，根据正文 `text` 生成一行主题（不超过 30 字），done 中的 `subject` 与 `text` 相同。
  - UI：写信窗口 ✨ 菜单增加「生成主题」，结果直接填入主题栏；主题栏不为空时先确认是否覆盖。

### 8.5 解读附件（A1）
- **接口**：`POST /v1/ai/messages/{id}/attachments/{aid}/explain`（流），请求体 `{text?: String, question?: String}`。
  - **图片**（image/*，≤ 10 MB）：用看图模型，把图片以 base64 内联发送：
    - OpenAI：content parts，`image_url` 为 data URL。
    - Anthropic：`{type:"image",source:{type:"base64",media_type,data}}`。
    - Gemini：`inlineData`。
    - 不支持看图的 CLI 服务商报错。
  - **PDF**：使用请求体中的 `text`（Swift 端用 PDFKit 提取，最多 max_body_chars）。没有 text 时返回 400。
  - **text/*、.csv、.md、.json**：核心自己解码。
  - 其他类型返回 400「暂不支持解读这种附件」。
- **提示词**：默认「概括这份附件的内容和关键信息，列出需要我注意或处理的事项」；有 question 时回答这个问题。
- **UI**：附件条右键菜单和 ✨ 菜单增加「AI 解读附件」，结果显示在 AI 卡片中。

### 8.6 账户级策略（隐私）
- `AiSettings.account_policies: BTreeMap<String /* account_id */, String /* "on" | "off" | "local_only" */>`，缺省为 on。
- **执行位置**：在加载邮件正文给 AI 的统一入口检查。这个检查对摘要、翻译、建议回复、智能分析、批量、Loop、自动回复、问问邮箱、MCP 的 `read_email` 和解读附件全部生效。
  - **off**：拒绝，code `ai_refused`，信息「这个账户已关闭 AI」。后台任务（智能分析、自动回复、Loop）直接跳过，不报错。
  - **local_only**：只有当所用模型的服务商是本地的才允许。本地指：服务商 id 为 `ollama` 或 `lmstudio`，或者 base_url 的主机是 localhost / 127.0.0.1 / ::1。否则拒绝，信息「这个账户只允许使用本地模型」。后台任务跳过。
  - 问问邮箱和 Loop：候选邮件中属于 off 账户的直接剔除；属于 local_only 账户的，在模型不是本地时剔除。
- **UI**：AI 功能 → 常规 中列出每个账户，用 Picker 选「启用 / 关闭 / 仅本地模型」。

### 8.7 提醒与安全（A2 细项）
- **通知方式**：`AiSettings.notify_mode: String`，可选 `"normal"`（默认，不变）、`"annotate"`、`"important"`。
  - **annotate**：智能分析完成后，发事件 `ai_insight {message_id}`（已有）。助手 App 收到后，如果这封邮件的通知还在，就用同一个 identifier `mail-<id>` 重发一条无声通知，正文前加「✨ one_line」。
  - **important**：符合 smart_scope 的新邮件先不弹通知（NewMail 事件中 `notify=false`），等智能分析完成后再决定：
    - priority 为 high、或 needs_reply、或发件人是 VIP、或 risk 不为空时，发事件 `ai_notify {message_id, title, subtitle, body}`，由助手 App 弹出。
      - title 与普通新邮件通知格式相同：`[账户标签] 发件人`。
      - subtitle 为主题，body 为「✨ one_line」。
    - 分析失败、模型未配置或 90 秒内没有结果时，按普通新邮件通知弹出（同样通过 `ai_notify` 事件）。
    - 不在分析范围内的邮件照常通知。
- **钓鱼和诈骗识别**：`Insight` 新增 `risk: Option<String>`（`"phishing"`、`"scam"`，没有风险时为 null）和 `risk_reason: String`。智能分析的提示词要求判断风险：冒充、索要密码 / 验证码 / 转账、可疑链接等。
  - 阅读区：有 risk 时显示红色横幅「AI 提示：疑似钓鱼 / 诈骗：理由」。
  - 待处理列表：显示红色标签。
- **跟进提醒**：`AiSettings.followup_days: u32`，默认 3，0 表示关闭。
  - **检查**：housekeeping 每小时检查一次自己最近 14 天发出的邮件（Sent 文件夹），同时满足以下条件时，用 `quick_model` 判断这封邮件是否在等对方回复（JSON `{"awaits_reply":bool,"reason":…}`），结果缓存在 kv `ai_followup`：
    - 发出时间超过 followup_days 天。
    - 同一会话里此后没有来自其他人的邮件。
  - **提醒**：判断为 true 的邮件加入提醒列表。`GET /v1/ai/reminders` 的每一项新增 `kind: "insight" | "follow_up"`。follow_up 项中：
    - `message` 是那封已发邮件。
    - insight 的 one_line 为「N 天没收到 收件人 的回复」。
    - 第一次加入时发 `ai_reminder` 事件。
  - **完成**：可以标记为已处理；会话里收到新回复后自动移除。
- **休假自动回复**：
  - `AutoReplyRule` 新增字段：
    - `kind: String`：`"ai"`（默认）或 `"vacation"`。
    - `reply_text: String`
    - `start: Option<i64>`
    - `end: Option<i64>`
  - kind 为 vacation 时：
    - 不调用 AI，回复内容就是 reply_text，主题用「自动回复：原主题」。
    - 只在 start 到 end 期间生效，并且总是直接发送（加 `Auto-Submitted: auto-replied` 信头）。
    - 同一发件人在整个休假期间只回复一次。
    - 跳过规则与 AI 自动回复相同。
  - UI：自动回复规则编辑器顶部可以切换「AI 回复 / 休假回复」。
- **批量退订**：`/batch` 的 op 新增 `unsubscribe`，不调用 AI。
  - 对每封带 List-Unsubscribe 的邮件调用现有的退订逻辑，每封的 output 为「已退订」或「需要在浏览器中打开：url」。
  - summary 为汇总。
  - UI 中把退订放进批量操作选项。

### 8.8 提示词库
```rust
pub struct PromptPreset { pub id: String, pub name: String, pub scope: String /* "compose" | "reader" | "batch" */, pub prompt: String, pub sort: i64 }
```
- **接口**：
  - `GET` / `POST /v1/ai/prompts`：列表 / 保存。
  - `DELETE /v1/ai/prompts/{id}`
  - 第一次读取时生成默认提示词：
    - compose：「改成适合发给客户的语气」「压缩到 100 字以内」。
    - reader：「提取所有金额和日期」「用三句话向老板汇报这封邮件」。
    - batch：「提取订单号和金额，输出表格」。
- **阅读区自定义提示**：`POST /v1/ai/messages/{id}/prompt`（流），请求体 `{prompt, thread: bool}`，对邮件或会话执行该提示，结果显示在 AI 卡片中。
- **UI**：
  - 写信窗口 ✨ 菜单：「我的提示词」子菜单（compose 类）→ 执行 `/compose` 的 custom op。
  - 阅读区 ✨ 菜单：「我的提示词」（reader 类）。
  - 批量处理：自定义指令旁边可以选 batch 类提示词。
  - 设置 → AI 功能：增加「提示词」分区，用来管理提示词。

### 8.9 新增事件
```
ai_confirm   { id, kind, title, detail }
ai_notify    { message_id, title, subtitle, body }
```

### 8.10 演示数据（`POST /v1/ai/demo` 补充）
- **日程**：给一封邮件写入 events（例如张三「周末爬山？」→「周六 08:00 西山爬山」）。
- **风险**：插入一封演示钓鱼邮件，并写入带 `risk:"phishing"` 的 insight。
  - 发件人「Apple 支持 <security@apple-id-verify.demo>」，主题「您的 Apple ID 已被锁定，请立即验证」。
  - 这封邮件插入演示账户的收件箱（可以复用 seed_demo 的插入逻辑）。
- **跟进**：插入一封 5 天前自己发出、没有回复的邮件，并写入对应的 follow_up 提醒。
- **其他**：
  - 写入一个演示 MCP 外部服务器配置，设为未启用。
  - 写入 7 天的演示用量。
  - `mcp_enabled` 设为 true，以便截图展示 MCP 页面。
- **测试**：tests/ai.rs 补充：
  - MCP 服务端：initialize、tools/list、search_emails、send_email 确认的同意和超时两种情况（超时时间可以用环境变量缩短）。
  - 外部 MCP 客户端：用 shell 脚本伪造 stdio 服务器，用 axum 伪造 http 服务器，并配合一个支持 tools 的假 OpenAI 服务器跑完整的工具循环。
  - 其他：多 Key 轮流、账户策略、日程提取解析、跟进提醒判定、休假回复、用量聚合、模型能力推断。
