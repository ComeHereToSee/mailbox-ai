//! "Ask the mailbox": the quick model turns the question into searches, the local search
//! finds the messages (re-ranked by an embedding model when one is set), the default model
//! answers citing them as [1] [2]. Messages of accounts whose AI policy keeps them from the
//! models are left out.

use super::client::{self, Keys};
use super::model::{AiProvider, AiSettings, AskSource, ChatMessage, ModelRef, TokenUsage};
use super::{DeltaTx, StreamDone, clean_output, fmt_time, policy, prompts, truncate_plain};
use crate::Engine;
use crate::error::{Error, Result};
use crate::model::{Mailbox, MessageId, MessageSummary};
use crate::store::ListQuery;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

const PER_QUERY: i64 = 8;
const MAX_SOURCES: usize = 16;
/// With an embedding model: full-text candidates, then the best by cosine similarity.
const CANDIDATES: usize = 30;
const RERANKED: usize = 8;

/// Cosine similarity (0 for empty or mismatched vectors).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na.sqrt() * nb.sqrt()) }
}

/// Indexes of `candidates` by similarity to `query`, best first.
pub fn rank_by_similarity(query: &[f32], candidates: &[Vec<f32>]) -> Vec<usize> {
    let mut scored: Vec<(usize, f32)> = candidates.iter().enumerate().map(|(i, v)| (i, cosine(query, v))).collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}

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

    /// The embedding model, when one is set and its provider can embed.
    fn ai_embedding_model(&self, s: &AiSettings) -> Option<(AiProvider, ModelRef)> {
        let m = s.embedding_model.clone().filter(|m| !m.provider.is_empty() && !m.model.is_empty())?;
        let p = self.ai_provider(&m.provider)?;
        if !p.enabled {
            return None;
        }
        if !client::supports_embeddings(&p) {
            tracing::info!(provider = %p.id, "嵌入模型的服务商不支持 embeddings 接口，已忽略这个设置");
            return None;
        }
        Some((p, m))
    }

    /// Embedding vectors (usage counted as "embedding").
    pub(crate) async fn ai_embed(&self, p: &AiProvider, model: &ModelRef, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        let keys = self.ai_key_list(&p.id);
        let start = self.ai_next_key(&p.id);
        let started = Instant::now();
        let r = client::embed(&self.ai.http, p, Keys::new(&keys, start), &model.model, inputs).await;
        let ms = started.elapsed().as_millis() as u64;
        match r {
            Ok((vectors, usage)) => {
                self.ai_record_usage(&p.id, &model.model, "embedding", usage, ms, true);
                Ok(vectors)
            }
            Err(err) => {
                self.ai_record_usage(&p.id, &model.model, "embedding", TokenUsage::default(), ms, false);
                Err(err)
            }
        }
    }

    /// The candidates most similar to the question ("question" vs "subject + snippet").
    /// Falls back to the full-text order when embedding fails.
    async fn ai_rerank(&self, p: &AiProvider, model: &ModelRef, question: &str, found: Vec<MessageSummary>) -> Vec<MessageSummary> {
        let mut inputs = vec![question.to_string()];
        for m in &found {
            inputs.push(truncate_plain(&format!("{}\n{}", m.subject, m.snippet), 1000));
        }
        match self.ai_embed(p, model, &inputs).await {
            Ok(vectors) if vectors.len() == inputs.len() => {
                let order = rank_by_similarity(&vectors[0], &vectors[1..]);
                let mut slots: Vec<Option<MessageSummary>> = found.into_iter().map(Some).collect();
                order.into_iter().take(RERANKED).filter_map(|i| slots.get_mut(i).and_then(Option::take)).collect()
            }
            Ok(vectors) => {
                tracing::warn!(got = vectors.len(), want = inputs.len(), "embedding answer has the wrong length");
                found.into_iter().take(RERANKED).collect()
            }
            Err(err) => {
                tracing::warn!(error = %err, "embedding failed, using full-text order");
                found.into_iter().take(RERANKED).collect()
            }
        }
    }

    /// `POST /v1/ai/ask`; `done` carries `sources`. With `tools` the answer may use the
    /// enabled external MCP servers (`done` also carries `tool_calls`).
    pub async fn ai_ask(&self, question: String, mailbox: Option<String>, tools: bool, tx: DeltaTx) -> Result<StreamDone> {
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
        let answer_local = self.ai_model_is_local(&answer_model);
        let embedding = self.ai_embedding_model(&s);
        let embed_local = embedding.as_ref().map(|(p, _)| policy::is_local_provider(p));
        let per_query = if embedding.is_some() { CANDIDATES as i64 } else { PER_QUERY };
        // Account policies: the answer model (and the embedding model) must be allowed.
        let allowed = |m: &MessageSummary| -> bool {
            let embed_ok = match embed_local {
                Some(local) => policy::allows(&s, &m.account_id, local),
                None => true,
            };
            embed_ok && policy::allows(&s, &m.account_id, answer_local)
        };

        // 1. Question → 1–3 searches (quick model).
        let mut queries: Vec<String> = Vec::new();
        if let Ok(quick) = self.ai_model(true) {
            let (system, user) = prompts::ask_queries(&question);
            match self.ai_json(&quick, "ask", system, user).await {
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

        // 2. Local search, first 8 of each (30 candidates with an embedding model), without
        //    duplicates.
        let mut found: Vec<MessageSummary> = Vec::new();
        let mut seen: HashSet<MessageId> = HashSet::new();
        for q in &queries {
            let list = self
                .db
                .list_messages(&ListQuery { mailbox: mailbox.clone(), search: Some(q.clone()), limit: per_query, ..Default::default() })
                .unwrap_or_default();
            for m in list {
                if seen.insert(m.id) && allowed(&m) {
                    found.push(m);
                }
            }
        }
        // 3. Nothing: plain keywords.
        if found.is_empty() {
            found = self.ai_keyword_search(&mailbox, &question, per_query as usize).into_iter().filter(|m| allowed(m)).collect();
        }
        match &embedding {
            Some((p, m)) if found.len() > 1 => {
                found.truncate(CANDIDATES);
                found = self.ai_rerank(p, m, &question, found).await;
            }
            _ => found.truncate(MAX_SOURCES),
        }
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
            let body = match self.ai_mail_text(m.id, 1200, true, answer_local).await {
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
        let sources = serde_json::to_value(&sources)?;
        if tools {
            let mut done = self.ai_tool_chat(answer_model, system, vec![ChatMessage::user(user)], "ask", tx).await?;
            done.text = clean_output(&done.text);
            return Ok(done.with("sources", sources));
        }
        let raw = self.ai_complete(&answer_model, "ask", system, user, false, tx).await?;
        Ok(StreamDone::new(clean_output(&raw), label).with("sources", sources))
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

    #[test]
    fn similarity_ranking() {
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        let q = [1.0, 0.0];
        let c = vec![vec![0.0, 1.0], vec![1.0, 0.1], vec![0.7, 0.7]];
        assert_eq!(rank_by_similarity(&q, &c), vec![1, 2, 0]);
    }
}
