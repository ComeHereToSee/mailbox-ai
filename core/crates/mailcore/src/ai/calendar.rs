//! Calendar extraction (`POST /v1/ai/messages/{id}/events`): meetings, appointments and
//! other dated plans found in a message, ready for EventKit. Cached in kv space
//! `ai_events` (id = message id); the smart analysis fills the same cache.

use super::insight::{bool_of, text_of};
use super::model::ExtractedEvent;
use super::{local_tz, prompts, truncate_plain};
use crate::Engine;
use crate::error::{Error, Result};
use crate::model::MessageId;
use crate::store::now;
use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const SPACE: &str = "ai_events";
const MAX_EVENTS: usize = 10;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct EventsCache {
    pub message_id: MessageId,
    pub events: Vec<ExtractedEvent>,
    /// `provider/model`
    pub model: String,
    pub created_at: i64,
}

/// A time written by the model: RFC 3339, "YYYY-MM-DD HH:MM[:SS]" (local) or a date
/// (00:00 local). Unix seconds when it is a number.
pub fn parse_event_time<Z: TimeZone>(v: &Value, tz: &Z) -> Option<i64> {
    let s = match v {
        Value::Number(n) => return n.as_i64().filter(|t| *t > 1_000_000_000),
        Value::String(s) => s.trim(),
        _ => return None,
    };
    if s.is_empty() || s.eq_ignore_ascii_case("null") {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    let norm = s.replace('T', " ").replace('/', "-");
    for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(&norm, f) {
            return tz.from_local_datetime(&n).earliest().map(|d| d.timestamp());
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(&norm, "%Y-%m-%d") {
        return tz.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).earliest().map(|d| d.timestamp());
    }
    None
}

/// Whether a time string is only a date ("2026-10-24").
fn date_only(v: &Value) -> bool {
    v.as_str().is_some_and(|s| chrono::NaiveDate::parse_from_str(s.trim().replace('/', "-").as_str(), "%Y-%m-%d").is_ok())
}

/// Events of the model's JSON: `{"events":[…]}` or the array itself. Entries without a
/// title or a valid start are dropped.
pub fn parse_events<Z: TimeZone>(v: &Value, tz: &Z) -> Vec<ExtractedEvent> {
    let items: Vec<Value> = match v {
        Value::Array(list) => list.clone(),
        _ => v["events"].as_array().cloned().unwrap_or_default(),
    };
    let mut out = Vec::new();
    for it in &items {
        let Some(title) = text_of(&it["title"]) else { continue };
        let Some(start) = parse_event_time(&it["start"], tz) else { continue };
        let all_day = bool_of(&it["all_day"]) || date_only(&it["start"]);
        let end = parse_event_time(&it["end"], tz).filter(|e| *e > start);
        out.push(ExtractedEvent {
            title: truncate_plain(&title, 60),
            start,
            end,
            all_day,
            location: text_of(&it["location"]),
            notes: truncate_plain(&text_of(&it["notes"]).unwrap_or_default(), 200),
            source_text: truncate_plain(&text_of(&it["source_text"]).unwrap_or_default(), 100),
        });
        if out.len() >= MAX_EVENTS {
            break;
        }
    }
    out
}

/// [`parse_events`] in the system time zone.
pub fn parse_events_local(v: &Value) -> Vec<ExtractedEvent> {
    match local_tz() {
        Some(tz) => parse_events(v, &tz),
        None => parse_events(v, &chrono::Local),
    }
}

/// Daily: drops cached events of deleted messages.
pub(crate) fn prune(e: &Engine) {
    let list: Vec<EventsCache> = e.db.kv_list(SPACE).unwrap_or_default();
    for c in list {
        if matches!(e.db.message(c.message_id), Err(Error::NotFound(_))) {
            let _ = e.db.kv_delete(SPACE, &c.message_id.to_string());
        }
    }
}

impl Engine {
    pub fn ai_cached_events(&self, id: MessageId) -> Option<EventsCache> {
        self.db.kv_get::<EventsCache>(SPACE, &id.to_string()).ok().flatten()
    }

    pub(crate) fn ai_store_events(&self, id: MessageId, events: &[ExtractedEvent], model: &str) -> Result<()> {
        let c = EventsCache { message_id: id, events: events.to_vec(), model: model.to_string(), created_at: now() };
        self.db.kv_put(SPACE, &id.to_string(), &c, 0)
    }

    /// Calendar entries of a message (quick model, JSON). Served from the cache unless
    /// `force`. Returns (events, cached).
    pub async fn ai_extract_events(&self, id: MessageId, force: bool) -> Result<(Vec<ExtractedEvent>, bool)> {
        self.db.message(id)?;
        if !force {
            if let Some(c) = self.ai_cached_events(id) {
                return Ok((c.events, true));
            }
        }
        let s = self.ai_settings();
        let model = self.ai_model(true)?;
        let local = self.ai_model_is_local(&model);
        let mail = self.ai_mail_text(id, s.max_body_chars, true, local).await?;
        let (system, user) = prompts::events(&s, &mail);
        let v = self.ai_json(&model, "events", system, user).await?;
        let events = parse_events_local(&v);
        self.ai_store_events(id, &events, &model.label())?;
        // Keep the insight (if any) in step with the fresh extraction.
        if let Some(mut ins) = self.ai_insight(id) {
            ins.events = events.clone();
            let _ = self.ai_put_insight(&ins);
        }
        Ok((events, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Asia::Shanghai;
    use serde_json::json;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Shanghai.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp()
    }

    #[test]
    fn parses_event_json() {
        let v = json!({"events": [
            {"title": "西山爬山", "start": "2026-10-24 08:00", "end": "2026-10-24 12:00", "all_day": false,
             "location": "西山门口", "notes": "记得带水", "source_text": "周六早上 8 点"},
            {"title": "团建", "start": "2026-10-30", "end": null, "all_day": "false", "location": "null", "notes": "", "source_text": "下周五"},
            {"title": "", "start": "2026-10-24 08:00"},
            {"title": "没有时间", "start": "下周"},
            {"title": "结束早于开始", "start": "2026-10-24T09:30:00+08:00", "end": "2026-10-24 09:00"}
        ]});
        let ev = parse_events(&v, &Shanghai);
        assert_eq!(ev.len(), 3, "{ev:?}");
        assert_eq!(ev[0].title, "西山爬山");
        assert_eq!(ev[0].start, at(2026, 10, 24, 8, 0));
        assert_eq!(ev[0].end, Some(at(2026, 10, 24, 12, 0)));
        assert!(!ev[0].all_day);
        assert_eq!(ev[0].location.as_deref(), Some("西山门口"));
        assert_eq!(ev[0].source_text, "周六早上 8 点");
        // A bare date is an all-day event at local midnight.
        assert_eq!(ev[1].start, at(2026, 10, 30, 0, 0));
        assert!(ev[1].all_day);
        assert_eq!(ev[1].location, None);
        assert_eq!(ev[1].end, None);
        assert_eq!(ev[2].start, at(2026, 10, 24, 9, 30));
        assert_eq!(ev[2].end, None);
        // The array alone, unix seconds, nothing.
        let ev = parse_events(&json!([{"title": "会", "start": 4_102_444_800i64}]), &Shanghai);
        assert_eq!(ev[0].start, 4_102_444_800);
        assert!(parse_events(&json!({"events": []}), &Shanghai).is_empty());
        assert!(parse_events(&Value::Null, &Shanghai).is_empty());
    }
}
