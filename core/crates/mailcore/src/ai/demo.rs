//! `POST /v1/ai/demo`: AI demo data on top of the demo accounts, so the UI (and CI
//! screenshots) show AI cards and the "待处理" list without a real model.

use super::insight::{SPACE as INSIGHT_SPACE, compute_remind_at_local};
use super::loops::{next_run_local, templates};
use super::model::{Insight, LoopRun, LoopSchedule, ModelRef, SummaryCache};
use super::local_timestamp;
use crate::Engine;
use crate::error::Result;
use crate::events::Event;
use crate::model::{Mailbox, MessageSummary};
use crate::store::{ListQuery, new_id, now};
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
        for (needle, mut ins) in items {
            let Some(m) = find(needle) else { continue };
            ins.message_id = m.id;
            ins.created_at = t;
            ins.remind_at = compute_remind_at_local(ins.deadline, t);
            // No notifications pop up while the demo is being looked at.
            ins.reminded = true;
            self.db.kv_put(INSIGHT_SPACE, &m.id.to_string(), &ins, 0)?;
            self.events.emit(Event::AiInsight { message_id: m.id });
            insights += 1;
        }

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
        }))
    }
}
