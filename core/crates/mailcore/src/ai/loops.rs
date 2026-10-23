//! Loops: scheduled AI tasks over a search (kv spaces `ai_loop` and `ai_loop_run`).

use super::model::{AiLoop, LoopRun, LoopSchedule, ModelRef};
use super::{clean_output, fmt_time, local_tz, markdown_to_html, preview_text, prompts};
use crate::Engine;
use crate::error::{Error, Result};
use crate::events::Event;
use crate::mime::build::{self, Outgoing};
use crate::model::{Category, EmailAddress, FolderRole, Mailbox, MessageSummary};
use crate::store::{ListQuery, NewMessage, new_id, now};
use chrono::{Datelike, TimeZone};
use std::collections::{HashMap, HashSet};

const SPACE: &str = "ai_loop";
const RUNS: &str = "ai_loop_run";
const KEEP_RUNS: usize = 20;
pub const NO_MATCH: &str = "没有符合条件的邮件";

fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m) = (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?);
    if h < 24 && m < 60 { Some((h, m)) } else { None }
}

/// Next run strictly after `after` in time zone `tz`.
pub fn next_run<Z: TimeZone>(s: &LoopSchedule, after: i64, tz: &Z) -> Option<i64> {
    if s.kind == "interval" {
        let every = s.every_minutes.unwrap_or(60).max(30) as i64;
        return Some(after + every * 60);
    }
    let (h, m) = parse_hhmm(&s.time)?;
    let mut date = tz.timestamp_opt(after, 0).single()?.date_naive();
    for _ in 0..15 {
        let wd = date.weekday().number_from_monday();
        let ok = match s.kind.as_str() {
            "weekdays" => wd <= 5,
            "weekly" => wd == s.weekday.unwrap_or(1).clamp(1, 7) as u32,
            _ => true,
        };
        if ok {
            if let Some(naive) = date.and_hms_opt(h, m, 0) {
                if let Some(dt) = tz.from_local_datetime(&naive).earliest() {
                    let ts = dt.timestamp();
                    if ts > after {
                        return Some(ts);
                    }
                }
            }
        }
        date = date.succ_opt()?;
    }
    None
}

/// [`next_run`] in the system time zone.
pub fn next_run_local(s: &LoopSchedule, after: i64) -> Option<i64> {
    match local_tz() {
        Some(tz) => next_run(s, after, &tz),
        None => next_run(s, after, &chrono::Local),
    }
}

/// `a (b OR c) d` → `a b d`, `a c d` (the local search has no OR).
pub fn expand_or(q: &str) -> Vec<String> {
    let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if let (Some(open), Some(close)) = (q.find('('), q.find(')')) {
        if open < close {
            let before = &q[..open];
            let inner = &q[open + 1..close];
            let after = &q[close + 1..];
            let alternatives: Vec<&str> = inner.split(" OR ").map(str::trim).filter(|s| !s.is_empty()).collect();
            let rest = expand_or(after);
            let mut out = Vec::new();
            for a in &alternatives {
                for r in &rest {
                    out.push(norm(format!("{before} {a} {r}")));
                }
            }
            if out.is_empty() {
                out.push(norm(format!("{before} {after}")));
            }
            out.truncate(16);
            return out;
        }
    }
    if q.contains(" OR ") {
        return q.split(" OR ").map(|s| norm(s.to_string())).filter(|s| !s.is_empty()).take(16).collect();
    }
    vec![norm(q.to_string())]
}

/// Built-in templates (empty ids).
pub fn templates() -> Vec<AiLoop> {
    let t = |name: &str, kind: &str, time: &str, weekday: Option<u8>, query: &str, instruction: &str| AiLoop {
        name: name.to_string(),
        schedule: LoopSchedule { kind: kind.to_string(), time: time.to_string(), weekday, every_minutes: None },
        query: query.to_string(),
        instruction: instruction.to_string(),
        ..Default::default()
    };
    vec![
        t("每日早报", "daily", "08:00", None, "is:unread newer_than:1d", "按重要程度总结这些未读邮件，列出需要我今天处理的事项。"),
        t("待回复检查", "weekdays", "17:30", None, "newer_than:3d -in:sent", "找出需要我回复但我还没回复的邮件，给出建议回复要点。"),
        t(
            "每周账单汇总",
            "weekly",
            "09:00",
            Some(1),
            "newer_than:7d (账单 OR 发票 OR invoice OR receipt)",
            "汇总本周的账单 / 发票：商家、金额、到期日，最后给出合计。",
        ),
        t("订阅清理建议", "weekly", "20:00", Some(7), "category:promotions newer_than:7d", "统计本周营销邮件来自哪些发件人，建议哪些可以退订。"),
    ]
}

/// Starts due loops. Called once a minute from housekeeping.
pub(crate) fn schedule_due(e: &Engine, now: i64) {
    for mut l in e.ai_loops() {
        if !l.enabled {
            continue;
        }
        match l.next_run {
            None => {
                l.next_run = next_run_local(&l.schedule, now);
                let _ = e.db.kv_put(SPACE, &l.id, &l, 0);
            }
            Some(t) if t <= now => {
                let running = e.ai.running_loops.lock().contains(&l.id);
                if running {
                    continue;
                }
                l.next_run = next_run_local(&l.schedule, now);
                let _ = e.db.kv_put(SPACE, &l.id, &l, 0);
                if e.ai_loop_model(&l).is_err() {
                    tracing::debug!(loop_id = %l.id, "ai loop skipped: no model configured");
                    continue;
                }
                e.ai_spawn_loop_run(l);
            }
            Some(_) => {}
        }
    }
}

impl Engine {
    pub fn ai_loops(&self) -> Vec<AiLoop> {
        self.db.kv_list(SPACE).unwrap_or_default()
    }

    pub fn ai_loop(&self, id: &str) -> Option<AiLoop> {
        self.db.kv_get::<AiLoop>(SPACE, id).ok().flatten()
    }

    /// Saves a loop; a new one (empty id) gets an id. `next_run` is recomputed.
    pub fn ai_save_loop(&self, mut l: AiLoop) -> Result<AiLoop> {
        l.name = l.name.trim().to_string();
        if l.name.is_empty() {
            l.name = "未命名 Loop".into();
        }
        l.instruction = l.instruction.trim().to_string();
        if l.instruction.is_empty() {
            return Err(Error::Invalid("请填写要 AI 做什么".into()));
        }
        l.query = l.query.trim().to_string();
        match l.schedule.kind.as_str() {
            "daily" | "weekdays" | "weekly" => {
                if parse_hhmm(&l.schedule.time).is_none() {
                    return Err(Error::Invalid("时间格式应为 HH:MM，例如 08:30".into()));
                }
                if l.schedule.kind == "weekly" {
                    l.schedule.weekday = Some(l.schedule.weekday.unwrap_or(1).clamp(1, 7));
                }
            }
            "interval" => l.schedule.every_minutes = Some(l.schedule.every_minutes.unwrap_or(60).max(30)),
            other => return Err(Error::Invalid(format!("未知的频率 {other}"))),
        }
        if l.max_messages == 0 {
            l.max_messages = 30;
        }
        l.max_messages = l.max_messages.min(200);
        if l.id.trim().is_empty() {
            l.id = new_id();
        } else if let Some(old) = self.ai_loop(&l.id) {
            l.last_run = old.last_run;
        }
        l.next_run = if l.enabled { next_run_local(&l.schedule, now()) } else { None };
        self.db.kv_put(SPACE, &l.id, &l, 0)?;
        Ok(l)
    }

    pub fn ai_delete_loop(&self, id: &str) -> Result<()> {
        if !self.db.kv_delete(SPACE, id)? {
            return Err(Error::NotFound("Loop 不存在".into()));
        }
        for r in self.db.kv_list::<LoopRun>(RUNS)? {
            if r.loop_id == id {
                self.db.kv_delete(RUNS, &r.id)?;
            }
        }
        Ok(())
    }

    /// The most recent runs (newest first, at most 20).
    pub fn ai_loop_runs(&self, id: &str) -> Vec<LoopRun> {
        let mut runs: Vec<LoopRun> = self.db.kv_list::<LoopRun>(RUNS).unwrap_or_default().into_iter().filter(|r| r.loop_id == id).collect();
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs.truncate(KEEP_RUNS);
        runs
    }

    fn ai_prune_runs(&self, id: &str) {
        let mut runs: Vec<LoopRun> = self.db.kv_list::<LoopRun>(RUNS).unwrap_or_default().into_iter().filter(|r| r.loop_id == id).collect();
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        for r in runs.iter().skip(KEEP_RUNS) {
            let _ = self.db.kv_delete(RUNS, &r.id);
        }
    }

    pub(crate) fn ai_store_run(&self, run: &LoopRun) -> Result<()> {
        self.db.kv_put(RUNS, &run.id, run, 0)
    }

    /// The loop's model (if its provider is usable) or the quick model.
    fn ai_loop_model(&self, l: &AiLoop) -> Result<ModelRef> {
        if let Some(m) = &l.model {
            if let Some(p) = self.ai_provider(&m.provider) {
                if p.enabled && !m.model.is_empty() {
                    return Ok(m.clone());
                }
            }
        }
        self.ai_model(true)
    }

    /// `POST /v1/ai/loops/{id}/run`: starts a run in the background → run id.
    pub fn ai_run_loop_now(&self, id: &str) -> Result<String> {
        let l = self.ai_loop(id).ok_or_else(|| Error::NotFound("Loop 不存在".into()))?;
        if self.ai.running_loops.lock().contains(&l.id) {
            return Err(Error::Invalid("这个 Loop 正在运行".into()));
        }
        self.ai_loop_model(&l)?;
        Ok(self.ai_spawn_loop_run(l))
    }

    fn ai_spawn_loop_run(&self, l: AiLoop) -> String {
        let run = LoopRun {
            id: new_id(),
            loop_id: l.id.clone(),
            started_at: now(),
            status: "running".into(),
            ..Default::default()
        };
        if let Err(err) = self.ai_store_run(&run) {
            tracing::warn!(error = %err, "could not store loop run");
        }
        self.ai.running_loops.lock().insert(l.id.clone());
        let run_id = run.id.clone();
        let e = self.clone();
        tokio::spawn(async move { e.ai_execute_loop(l, run).await });
        run_id
    }

    async fn ai_execute_loop(&self, l: AiLoop, mut run: LoopRun) {
        let result = self.ai_loop_output(&l).await;
        run.finished_at = Some(now());
        match result {
            Ok((count, output)) => {
                run.status = "done".into();
                run.message_count = count;
                run.output = output;
            }
            Err(err) => {
                run.status = "failed".into();
                run.error = Some(err.to_string());
            }
        }
        if let Err(err) = self.ai_store_run(&run) {
            tracing::warn!(error = %err, "could not store loop run");
        }
        self.ai_prune_runs(&l.id);
        // The stored loop may have been edited meanwhile: only touch the bookkeeping.
        if let Some(mut cur) = self.ai_loop(&l.id) {
            cur.last_run = Some(run.started_at);
            if cur.enabled {
                cur.next_run = next_run_local(&cur.schedule, now());
            }
            let _ = self.db.kv_put(SPACE, &cur.id, &cur, 0);
        }
        self.ai.running_loops.lock().remove(&l.id);
        if run.status == "done" && l.email_digest && run.message_count > 0 {
            if let Err(err) = self.ai_deliver_digest(&l, &run) {
                tracing::warn!(error = %err, "could not deliver loop digest");
            }
        }
        let source = if run.status == "done" { run.output.clone() } else { run.error.clone().unwrap_or_default() };
        self.events.emit(Event::AiLoop {
            loop_id: l.id.clone(),
            run_id: run.id.clone(),
            name: l.name.clone(),
            status: run.status.clone(),
            preview: preview_text(&source, 120),
        });
    }

    /// Searches the loop's messages: newest first, at most `limit`.
    fn ai_loop_messages(&self, query: &str, limit: usize) -> Result<Vec<MessageSummary>> {
        let q = query.trim();
        if q.is_empty() {
            return self.db.list_messages(&ListQuery {
                mailbox: Mailbox::Unified(FolderRole::Inbox),
                limit: limit as i64,
                ..Default::default()
            });
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for variant in expand_or(q) {
            let list = self.db.list_messages(&ListQuery {
                mailbox: Mailbox::All,
                search: Some(variant),
                limit: limit as i64,
                ..Default::default()
            })?;
            for m in list {
                if seen.insert(m.id) {
                    out.push(m);
                }
            }
        }
        out.sort_by(|a, b| b.date.cmp(&a.date));
        out.truncate(limit);
        Ok(out)
    }

    /// (message count, Markdown output). No messages: no model call.
    async fn ai_loop_output(&self, l: &AiLoop) -> Result<(usize, String)> {
        let s = self.ai_settings();
        let limit = l.max_messages.clamp(1, 200);
        let msgs = self.ai_loop_messages(&l.query, limit)?;
        if msgs.is_empty() {
            return Ok((0, NO_MATCH.to_string()));
        }
        let model = self.ai_loop_model(l)?;
        let labels: HashMap<String, String> = self.db.accounts()?.into_iter().map(|a| (a.id, a.label)).collect();
        let with_body = msgs.len() <= 10;
        let mut list = String::new();
        for (i, m) in msgs.iter().enumerate() {
            let from = match &m.from.name {
                Some(n) if !n.is_empty() => format!("{n} <{}>", m.from.email),
                _ => m.from.email.clone(),
            };
            let label = labels.get(&m.account_id).cloned().unwrap_or_default();
            list.push_str(&format!(
                "[{}] {}｜{}｜{}｜{}\n摘要：{}\n",
                i + 1,
                from,
                if m.subject.is_empty() { "（无主题）" } else { m.subject.as_str() },
                fmt_time(m.date),
                label,
                m.snippet
            ));
            if with_body {
                if let Ok(t) = self.ai_mail_text(m.id, 1500, true).await {
                    list.push_str(&format!("正文：{}\n", t.text));
                }
            }
            list.push('\n');
        }
        let (system, user) = prompts::loop_run(&s, l, &list, msgs.len());
        let text = self.ai_complete(&model, system, user, false, None).await?;
        Ok((msgs.len(), clean_output(&text)))
    }

    /// Delivers a run as a local-only message from "Mailbox AI" into the first account's inbox.
    fn ai_deliver_digest(&self, l: &AiLoop, run: &LoopRun) -> Result<()> {
        let Some(acc) = self.db.accounts()?.into_iter().next() else { return Ok(()) };
        let inbox = match self.db.folder_by_role(&acc.id, FolderRole::Inbox)? {
            Some(f) => f.id,
            None => self.db.ensure_local_folder(&acc.id, FolderRole::Inbox, "收件箱")?,
        };
        let date = now();
        let html = format!(
            "<div style=\"font-family:-apple-system,system-ui,sans-serif;line-height:1.5\">{}</div>",
            markdown_to_html(&run.output)
        );
        let o = Outgoing {
            from: EmailAddress::new(Some("Mailbox AI"), "ai@mailbox.local"),
            to: vec![EmailAddress::new(Some(acc.name.as_str()), &acc.email)],
            subject: format!("✨ {} · {}", l.name, fmt_time(date)),
            html: Some(html),
            text: run.output.clone(),
            message_id: build::new_message_id("ai@mailbox.local"),
            date,
            ..Default::default()
        };
        let raw = build::build(&o, false)?;
        let h = crate::mime::parse_headers(&raw);
        let nm = NewMessage {
            account_id: acc.id.clone(),
            folder_id: inbox,
            remote_id: Some(format!("ai-digest:{}", run.id)),
            message_id: h.message_id.clone(),
            subject: h.subject.clone(),
            from: h.from.clone(),
            to: h.to.clone(),
            date,
            size: raw.len() as i64,
            category: Category::Primary,
            body_text: Some(run.output.clone()),
            ..Default::default()
        };
        let (id, _) = self.db.insert_message(&nm)?;
        crate::engine::detail::store_body(self, id, &raw)?;
        self.changed(Some(&acc.id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Asia::Shanghai;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Shanghai.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp()
    }

    fn sched(kind: &str, time: &str, weekday: Option<u8>, every: Option<u32>) -> LoopSchedule {
        LoopSchedule { kind: kind.into(), time: time.into(), weekday, every_minutes: every }
    }

    #[test]
    fn next_run_schedules() {
        // Friday 2026-10-23 09:00.
        let now = at(2026, 10, 23, 9, 0);
        assert_eq!(next_run(&sched("daily", "08:30", None, None), now, &Shanghai), Some(at(2026, 10, 24, 8, 30)));
        assert_eq!(next_run(&sched("daily", "17:30", None, None), now, &Shanghai), Some(at(2026, 10, 23, 17, 30)));
        // Exactly at the time: the next day.
        assert_eq!(next_run(&sched("daily", "09:00", None, None), now, &Shanghai), Some(at(2026, 10, 24, 9, 0)));
        // Weekdays after Friday's run → Monday.
        assert_eq!(next_run(&sched("weekdays", "08:00", None, None), now, &Shanghai), Some(at(2026, 10, 26, 8, 0)));
        assert_eq!(next_run(&sched("weekdays", "17:30", None, None), now, &Shanghai), Some(at(2026, 10, 23, 17, 30)));
        // Weekly on Monday (1) and Sunday (7).
        assert_eq!(next_run(&sched("weekly", "09:00", Some(1), None), now, &Shanghai), Some(at(2026, 10, 26, 9, 0)));
        assert_eq!(next_run(&sched("weekly", "20:00", Some(7), None), now, &Shanghai), Some(at(2026, 10, 25, 20, 0)));
        assert_eq!(next_run(&sched("weekly", "10:00", Some(5), None), now, &Shanghai), Some(at(2026, 10, 23, 10, 0)));
        // Interval: at least 30 minutes.
        assert_eq!(next_run(&sched("interval", "", None, Some(10)), now, &Shanghai), Some(now + 30 * 60));
        assert_eq!(next_run(&sched("interval", "", None, Some(90)), now, &Shanghai), Some(now + 90 * 60));
        assert_eq!(next_run(&sched("daily", "25:00", None, None), now, &Shanghai), None);
    }

    #[test]
    fn or_groups_expand() {
        assert_eq!(
            expand_or("newer_than:7d (账单 OR 发票 OR invoice)"),
            vec!["newer_than:7d 账单", "newer_than:7d 发票", "newer_than:7d invoice"]
        );
        assert_eq!(expand_or("a OR b"), vec!["a", "b"]);
        assert_eq!(expand_or("is:unread  newer_than:1d"), vec!["is:unread newer_than:1d"]);
        assert_eq!(expand_or("(x OR y) z"), vec!["x z", "y z"]);
    }

    #[test]
    fn templates_are_valid() {
        let t = templates();
        assert_eq!(t.len(), 4);
        assert!(t.iter().all(|l| l.id.is_empty() && l.enabled && !l.instruction.is_empty()));
        assert_eq!(t[2].schedule.weekday, Some(1));
        assert_eq!(t[3].schedule.weekday, Some(7));
    }
}
