//! `POST /v1/ai/demo`: AI demo data on top of the demo accounts, so the UI (and CI
//! screenshots) show AI cards, the "待处理" list, calendar entries, a phishing warning, a
//! follow-up, MCP settings and usage charts without a real model.

use super::followup::{FollowUp, SPACE as FOLLOWUP_SPACE};
use super::insight::{SPACE as INSIGHT_SPACE, compute_remind_at_local};
use super::loops::{next_run_local, templates};
use super::model::{ExtractedEvent, Insight, LoopRun, LoopSchedule, McpServerConfig, ModelRef, SummaryCache, TokenUsage};
use super::{local_timestamp, usage};
use crate::Engine;
use crate::error::Result;
use crate::events::Event;
use crate::mime::build::{self, Outgoing};
use crate::model::{Account, Category, EmailAddress, Flags, FolderRole, Mailbox, MessageId, MessageSummary};
use crate::store::{ListQuery, NewMessage, new_id, now};
use chrono::Datelike;
use serde_json::{Value, json};

const DEMO_SUMMARY: &str = "**一句话**：王经理发来 Q4 产品规划评审会的纪要，请各位在周五前确认自己负责的事项。\n\n\
- 附件是评审会议纪要（会议纪要.pdf）\n\
- 移动端改版由张三负责\n\
- 支付接入由李四负责\n\n\
**待办**\n\
- 周五前确认自己负责的事项（全体）";

const DEMO_LOOP_OUTPUT: &str = "**今日概览**：收件箱里有几封未读邮件，其中 2 件需要你今天处理。\n\n\
### 需要处理\n\
1. **王经理 · Q4 产品规划评审会议纪要**：周五前确认各自负责的事项（移动端改版：张三；支付接入：李四）。\n\
2. **Emily Chen · Updated mockups for the inbox view**：新版侧边栏设计稿，等你反馈。\n\n\
### 可以稍后看\n\
- 李四：接口文档已更新到 v2.3，鉴权部分有改动。\n\
- GitHub：#42 IMAP IDLE 支持已合并。\n\
- 京东：双十一预售促销，可以忽略。";

/// Next occurrence of a weekday (1 = Monday) at a local time.
fn next_weekday_at(weekday: u8, time: &str) -> i64 {
    let s = LoopSchedule { kind: "weekly".into(), time: time.into(), weekday: Some(weekday), every_minutes: None };
    next_run_local(&s, now()).unwrap_or_else(|| now() + 3 * 86_400)
}

/// Next 11 月 9 日 18:00 (local).
fn next_nov_9() -> i64 {
    let year = chrono::Local::now().year();
    for y in [year, year + 1] {
        let ts = chrono::NaiveDate::from_ymd_opt(y, 11, 9)
            .and_then(|d| d.and_hms_opt(18, 0, 0))
            .and_then(|n| local_timestamp(&n));
        if let Some(ts) = ts {
            if ts > now() {
                return ts;
            }
        }
    }
    now() + 14 * 86_400
}

const PHISHING_SUBJECT: &str = "您的 Apple ID 已被锁定，请立即验证";
const PHISHING_HTML: &str = "<p>尊敬的用户：</p><p>我们检测到您的 Apple ID 在异地登录，账户已被临时锁定。请在 <b>24 小时内</b>点击下方链接验证身份，\
并重新输入密码和短信验证码，否则账户将被永久停用。</p><p><a href=\"http://apple-id-verify.demo/unlock\">立即验证</a></p><p>Apple 支持团队</p>";
const FOLLOWUP_SUBJECT: &str = "合作方案报价，请确认";
const FOLLOWUP_HTML: &str = "<p>赵经理您好，</p><p>附上我们的合作方案和报价，麻烦您看一下是否合适，有问题随时联系我。期待您的回复！</p><p>谢谢</p>";

/// The hike of 张三's message: Saturday 08:00 at the west hill gate.
fn hike_event() -> ExtractedEvent {
    let start = next_weekday_at(6, "08:00");
    ExtractedEvent {
        title: "西山爬山".into(),
        start,
        end: Some(start + 4 * 3600),
        all_day: false,
        location: Some("西山门口".into()),
        notes: "张三约周六早上 8 点在西山门口集合，记得带水".into(),
        source_text: "周六早上 8 点".into(),
    }
}

fn insight(needs_reply: bool, priority: &str, deadline: Option<i64>, deadline_text: Option<&str>, todos: &[&str], one_line: &str) -> Insight {
    Insight {
        needs_reply,
        priority: priority.to_string(),
        deadline,
        deadline_text: deadline_text.map(String::from),
        todos: todos.iter().map(|t| t.to_string()).collect(),
        one_line: one_line.to_string(),
        ..Default::default()
    }
}

impl Engine {
    /// Seeds insights, a summary, a loop with a finished run and `claude_code/default` as
    /// the default model. Idempotent.
    pub fn ai_seed_demo(&self) -> Result<Value> {
        let all = ListQuery { mailbox: Mailbox::All, limit: 500, ..Default::default() };
        if self.db.list_messages(&all)?.is_empty() {
            self.seed_demo()?;
        }
        let msgs = self.db.list_messages(&all)?;
        let find = |needle: &str| -> Option<MessageSummary> { msgs.iter().find(|m| m.subject.contains(needle)).cloned() };

        // Providers + default model.
        let _ = self.ai_providers();
        let mut s = self.ai_settings();
        s.default_model = Some(ModelRef::new("claude_code", "default"));
        // The MCP page shows its URL and token in the screenshots.
        s.mcp_enabled = true;
        self.save_ai_settings(&s)?;

        // Smart reminders.
        let t = now();
        let items: Vec<(&str, Insight)> = vec![
            (
                "Q4 产品规划评审",
                insight(
                    true,
                    "high",
                    Some(next_weekday_at(5, "18:00")),
                    Some("周五前"),
                    &["确认自己在移动端改版 / 支付接入中负责的事项", "查看附件《会议纪要.pdf》"],
                    "Q4 规划评审纪要，周五前确认各自负责的事项",
                ),
            ),
            (
                "10 月账单",
                insight(false, "normal", Some(next_nov_9()), Some("11 月 9 日前"), &["还款 ¥3,280.50"], "支付宝 10 月账单 ¥3,280.50，11 月 9 日前还款"),
            ),
            (
                "Updated mockups",
                insight(true, "normal", None, None, &["查看新版侧边栏设计稿并回复 Emily"], "Emily 发来收件箱新设计稿，等你反馈"),
            ),
            (
                "周末爬山",
                insight(true, "low", Some(next_weekday_at(6, "08:00")), Some("周六早上 8 点"), &["回复张三是否参加周六爬山"], "张三约周六早上 8 点西山爬山"),
            ),
            (
                "接口文档更新",
                insight(false, "low", None, None, &["阅读接口文档 v2.3 第 4 节（鉴权改动）"], "李四：接口文档已更新到 v2.3"),
            ),
        ];
        let mut insights = 0;
        let mut events_id: Option<MessageId> = None;
        for (needle, mut ins) in items {
            let Some(m) = find(needle) else { continue };
            ins.message_id = m.id;
            ins.created_at = t;
            ins.remind_at = compute_remind_at_local(ins.deadline, t);
            // No notifications pop up while the demo is being looked at.
            ins.reminded = true;
            if needle == "周末爬山" {
                // A calendar entry (8.1).
                ins.events = vec![hike_event()];
                self.ai_store_events(m.id, &ins.events, "claude_code/default")?;
                events_id = Some(m.id);
            }
            self.db.kv_put(INSIGHT_SPACE, &m.id.to_string(), &ins, 0)?;
            self.events.emit(Event::AiInsight { message_id: m.id });
            insights += 1;
        }

        // A phishing message with its warning (8.7).
        let accounts = self.db.accounts()?;
        let work = accounts.iter().find(|a| a.email == "me@qq-demo.com").or_else(|| accounts.first()).cloned();
        let mut phishing_id: Option<MessageId> = None;
        let mut followup_id: Option<MessageId> = None;
        if let Some(acc) = &work {
            let id = self.ai_demo_message(
                acc,
                FolderRole::Inbox,
                "phishing",
                EmailAddress::new(Some("Apple 支持"), "security@apple-id-verify.demo"),
                EmailAddress::new(Some(acc.name.as_str()), &acc.email),
                PHISHING_SUBJECT,
                PHISHING_HTML,
                t - 6 * 3600,
                false,
            )?;
            let mut ins = insight(false, "high", None, None, &[], "冒充 Apple 的钓鱼邮件，不要点击链接");
            ins.message_id = id;
            ins.created_at = t;
            ins.reminded = true;
            ins.risk = Some("phishing".into());
            ins.risk_reason = "冒充 Apple 要求点击链接输入密码和验证码，发件域名不是 apple.com".into();
            self.db.kv_put(INSIGHT_SPACE, &id.to_string(), &ins, 0)?;
            self.events.emit(Event::AiInsight { message_id: id });
            insights += 1;
            phishing_id = Some(id);

            // Own mail of five days ago without an answer: a follow-up (8.7).
            let id = self.ai_demo_message(
                acc,
                FolderRole::Sent,
                "followup",
                EmailAddress::new(Some(acc.name.as_str()), &acc.email),
                EmailAddress::new(Some("赵六"), "zhaoliu@partner-demo.cn"),
                FOLLOWUP_SUBJECT,
                FOLLOWUP_HTML,
                t - 5 * 86_400,
                true,
            )?;
            let f = FollowUp {
                message_id: id,
                awaits_reply: true,
                reason: "询问对方是否接受报价，需要对方确认".into(),
                checked_at: t,
                done: false,
                reminded: true,
            };
            self.db.kv_put(FOLLOWUP_SPACE, &id.to_string(), &f, 0)?;
            followup_id = Some(id);
        }

        // An external MCP server (off) and seven days of usage (8.3, 8.4).
        if !self.ai_mcp_servers().iter().any(|c| c.id == "demo-filesystem") {
            let cfg = McpServerConfig {
                id: "demo-filesystem".into(),
                name: "文件系统".into(),
                enabled: false,
                transport: "stdio".into(),
                command: "npx".into(),
                args: vec!["-y".into(), "@modelcontextprotocol/server-filesystem".into(), "~/Documents".into()],
                ..Default::default()
            };
            self.ai_save_mcp_server(cfg)?;
        }
        self.ai_demo_usage();

        // A cached summary of the meeting notes.
        let mut summary_id = None;
        if let Some(m) = find("Q4 产品规划评审") {
            for thread in [true, false] {
                let count = if thread { self.ai_thread_ids(m.id)?.len() as i64 } else { 1 };
                let c = SummaryCache { text: DEMO_SUMMARY.into(), model: "claude_code/default".into(), created_at: t, thread_count: count };
                self.ai_store_summary(m.id, thread, &c)?;
            }
            summary_id = Some(m.id);
        }

        // A loop that already ran.
        let mut l = match self.ai_loops().into_iter().find(|l| l.name == "每日早报") {
            Some(l) => l,
            None => {
                let mut l = templates().into_iter().next().unwrap_or_default();
                l.id = new_id();
                l
            }
        };
        l.enabled = true;
        l.last_run = Some(t - 2 * 3600);
        l.next_run = next_run_local(&l.schedule, t);
        self.db.kv_put("ai_loop", &l.id, &l, 0)?;
        if self.ai_loop_runs(&l.id).is_empty() {
            let run = LoopRun {
                id: new_id(),
                loop_id: l.id.clone(),
                started_at: t - 2 * 3600,
                finished_at: Some(t - 2 * 3600 + 18),
                status: "done".into(),
                message_count: 5,
                output: DEMO_LOOP_OUTPUT.into(),
                error: None,
            };
            self.ai_store_run(&run)?;
        }

        Ok(json!({
            "ok": true,
            "insights": insights,
            "summary_message_id": summary_id,
            "loop_id": l.id,
            "default_model": s.default_model,
            "events_message_id": events_id,
            "phishing_message_id": phishing_id,
            "followup_message_id": followup_id,
        }))
    }

    /// Inserts (once, by key) a local demo message into a folder of a demo account.
    #[allow(clippy::too_many_arguments)]
    fn ai_demo_message(
        &self,
        acc: &Account,
        role: FolderRole,
        key: &str,
        from: EmailAddress,
        to: EmailAddress,
        subject: &str,
        html: &str,
        date: i64,
        seen: bool,
    ) -> Result<MessageId> {
        let folder = match self.db.folder_by_role(&acc.id, role)? {
            Some(f) => f.id,
            None => self.db.ensure_local_folder(&acc.id, role, role.as_str())?,
        };
        let o = Outgoing {
            from,
            to: vec![to],
            subject: subject.to_string(),
            html: Some(html.to_string()),
            text: crate::html::html_to_text(html),
            message_id: build::new_message_id(&acc.email),
            date,
            ..Default::default()
        };
        let raw = build::build(&o, false)?;
        let h = crate::mime::parse_headers(&raw);
        let nm = NewMessage {
            account_id: acc.id.clone(),
            folder_id: folder,
            remote_id: Some(format!("demo-ai:{key}")),
            message_id: h.message_id.clone(),
            subject: h.subject.clone(),
            from: h.from.clone(),
            to: h.to.clone(),
            date,
            size: raw.len() as i64,
            flags: Flags { seen, ..Default::default() },
            category: Category::Primary,
            ..Default::default()
        };
        let (id, inserted) = self.db.insert_message(&nm)?;
        if inserted {
            crate::engine::detail::store_body(self, id, &raw)?;
            self.changed(Some(&acc.id));
        }
        Ok(id)
    }

    /// Seven days of plausible usage (replaces earlier demo rows of those days).
    fn ai_demo_usage(&self) {
        let dates = usage::last_days(7);
        let rows: [(&str, &str, &str, u64, u64, u64); 5] = [
            ("claude_code", "default", "compose", 4, 1800, 900),
            ("claude_code", "default", "summary", 6, 5200, 700),
            ("claude_code", "default", "insight", 9, 7400, 600),
            ("deepseek", "deepseek-chat", "ask", 2, 3100, 500),
            ("deepseek", "deepseek-chat", "loop", 1, 2600, 800),
        ];
        for (i, date) in dates.iter().enumerate() {
            let day = i as u64 + 1;
            for (provider, model, feature, calls, input, output) in rows {
                let key = usage::row_key(date, provider, model, feature);
                let _ = self.db.kv_delete(usage::SPACE, &key);
                let n = calls + (day * 7 + calls) % 4;
                let tokens = TokenUsage { input_tokens: input * n / calls, output_tokens: output * n / calls };
                self.ai_add_usage(date, provider, model, feature, n, tokens, 1200 * n, false);
            }
        }
    }
}
