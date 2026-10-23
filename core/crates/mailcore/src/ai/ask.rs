//! "Ask the mailbox": the quick model turns the question into searches, the local search
//! finds the messages, the default model answers citing them as [1] [2].

use super::model::AskSource;
use super::{DeltaTx, StreamDone, clean_output, fmt_time, prompts};
use crate::Engine;
use crate::error::{Error, Result};
use crate::model::{Mailbox, MessageId, MessageSummary};
use crate::store::ListQuery;
use std::collections::{HashMap, HashSet};

const PER_QUERY: i64 = 8;
const MAX_SOURCES: usize = 16;

const STOP_WORDS: &[&str] = &[
    "什么", "怎么", "哪些", "哪个", "是否", "有没有", "一下", "我的", "邮件", "关于", "最近", "那个", "这个", "为什么", "多少",
    "什么时候", "告诉", "帮我", "请问", "的", "了", "吗", "呢", "是", "在", "有", "和", "与", "the", "a", "an", "is", "are", "was",
    "were", "what", "when", "who", "how", "did", "do", "does", "my", "me", "about", "any", "of", "to", "in", "on", "for", "email",
    "emails", "mail", "from", "with", "that", "this",
];

/// Keywords of a free-form question (CJK runs are split into bigrams).
pub fn keywords(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in question.split(|c: char| !c.is_alphanumeric()) {
        let tok = tok.trim().to_lowercase();
        if tok.is_empty() || STOP_WORDS.contains(&tok.as_str()) {
            continue;
        }
        let chars: Vec<char> = tok.chars().collect();
        let cjk = chars.iter().any(|c| (*c as u32) >= 0x2E80);
        if cjk && chars.len() > 4 {
            for w in chars.windows(2) {
                let s: String = w.iter().collect();
                let filler = w.iter().any(|c| "的了吗呢是在有和与我你他她它们么".contains(*c));
                if !filler && !STOP_WORDS.contains(&s.as_str()) && !out.contains(&s) {
                    out.push(s);
                }
            }
        } else if (chars.len() >= 2 || cjk) && !out.contains(&tok) {
            out.push(tok);
        }
    }
    out
}

impl Engine {
    /// Messages matching most keywords of the question (fallback when the rewrite found nothing).
    fn ai_keyword_search(&self, mailbox: &Mailbox, question: &str, limit: usize) -> Vec<MessageSummary> {
        let mut hits: HashMap<MessageId, (usize, MessageSummary)> = HashMap::new();
        for term in keywords(question).iter().take(12) {
            let q = format!("\"{}\"", term.replace('"', ""));
            let Ok(list) = self.db.list_messages(&ListQuery { mailbox: mailbox.clone(), search: Some(q), limit: 20, ..Default::default() })
            else {
                continue;
            };
            for m in list {
                let entry = hits.entry(m.id).or_insert((0, m));
                entry.0 += 1;
            }
        }
        let mut ranked: Vec<(usize, MessageSummary)> = hits.into_values().collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.date.cmp(&a.1.date)));
        ranked.into_iter().take(limit).map(|(_, m)| m).collect()
    }

    /// `POST /v1/ai/ask`; `done` carries `sources`.
    pub async fn ai_ask(&self, question: String, mailbox: Option<String>, tx: DeltaTx) -> Result<StreamDone> {
        let question = question.trim().to_string();
        if question.is_empty() {
            return Err(Error::Invalid("请输入问题".into()));
        }
        let mailbox = match mailbox.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
            Some(m) => Mailbox::parse(m).ok_or_else(|| Error::Invalid(format!("未知邮箱视图 {m}")))?,
            None => Mailbox::All,
        };
        let s = self.ai_settings();
        let answer_model = self.ai_model(false)?;

        // 1. Question → 1–3 searches (quick model).
        let mut queries: Vec<String> = Vec::new();
        if let Ok(quick) = self.ai_model(true) {
            let (system, user) = prompts::ask_queries(&question);
            match self.ai_json(&quick, system, user).await {
                Ok(v) => {
                    for q in v["queries"].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
                        let q = q.trim().to_string();
                        if !q.is_empty() && !queries.contains(&q) && queries.len() < 3 {
                            queries.push(q);
                        }
                    }
                }
                Err(err) => tracing::debug!(error = %err, "ask: query rewrite failed"),
            }
        }

        // 2. Local search, first 8 of each, without duplicates.
        let mut found: Vec<MessageSummary> = Vec::new();
        let mut seen: HashSet<MessageId> = HashSet::new();
        for q in &queries {
            let list = self
                .db
                .list_messages(&ListQuery { mailbox: mailbox.clone(), search: Some(q.clone()), limit: PER_QUERY, ..Default::default() })
                .unwrap_or_default();
            for m in list {
                if seen.insert(m.id) {
                    found.push(m);
                }
            }
        }
        // 3. Nothing: plain keywords.
        if found.is_empty() {
            found = self.ai_keyword_search(&mailbox, &question, PER_QUERY as usize);
        }
        found.truncate(MAX_SOURCES);
        let label = answer_model.label();
        if found.is_empty() {
            let text = "在本地邮件里没有找到和这个问题相关的邮件。可以换个说法，或者等邮件同步完成后再试。".to_string();
            if let Some(tx) = &tx {
                let _ = tx.send(text.clone()).await;
            }
            return Ok(StreamDone::new(text, label).with("sources", serde_json::json!([])));
        }

        // 4. Answer from the messages (default model).
        let mut context = String::new();
        let mut sources: Vec<AskSource> = Vec::new();
        for (i, m) in found.iter().enumerate() {
            let body = match self.ai_mail_text(m.id, 1200, true).await {
                Ok(t) => t.text,
                Err(_) => "（正文不可用，或是未允许交给 AI 的加密邮件）".to_string(),
            };
            let from = match &m.from.name {
                Some(n) if !n.is_empty() => format!("{n} <{}>", m.from.email),
                _ => m.from.email.clone(),
            };
            context.push_str(&format!(
                "[{}] 发件人：{}\n日期：{}\n主题：{}\n正文：{}\n\n",
                i + 1,
                from,
                fmt_time(m.date),
                if m.subject.is_empty() { "（无主题）" } else { m.subject.as_str() },
                body
            ));
            sources.push(AskSource { index: i + 1, message_id: m.id, subject: m.subject.clone(), from: m.from.display(), date: m.date });
        }
        let (system, user) = prompts::ask_answer(&s, &question, &context);
        let raw = self.ai_complete(&answer_model, system, user, false, tx).await?;
        Ok(StreamDone::new(clean_output(&raw), label).with("sources", serde_json::to_value(&sources)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_keywords() {
        assert_eq!(keywords("What did Emily say about the mockups?"), vec!["emily", "say", "mockups"]);
        let k = keywords("王经理上周发的会议纪要");
        assert!(k.contains(&"会议".to_string()) && k.contains(&"纪要".to_string()), "{k:?}");
        assert!(!k.iter().any(|w| w.contains('的')));
        assert_eq!(keywords("账单"), vec!["账单"]);
    }
}
