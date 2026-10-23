//! AI usage statistics (kv space `ai_usage`): every call counted per local day, provider,
//! model and feature, with the tokens the provider reported (`GET /v1/ai/usage`).

use super::local_day;
use super::model::{TokenUsage, UsageRow};
use crate::Engine;
use serde::Serialize;

pub(crate) const SPACE: &str = "ai_usage";
/// Rows older than this are dropped by the daily cleanup.
const KEEP_DAYS: u64 = 400;

pub fn row_key(date: &str, provider: &str, model: &str, feature: &str) -> String {
    format!("{date}|{provider}|{model}|{feature}")
}

#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct DayUsage {
    pub date: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub errors: u64,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelUsage {
    pub provider: String,
    pub model: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureUsage {
    pub feature: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// `GET /v1/ai/usage`
#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageReport {
    /// Every day of the range, oldest first (days without calls are zero).
    pub days: Vec<DayUsage>,
    /// Most calls first.
    pub by_model: Vec<ModelUsage>,
    pub by_feature: Vec<FeatureUsage>,
}

/// The last `n` local dates ("2026-10-23"), oldest first.
pub fn last_days(n: u32) -> Vec<String> {
    let today = chrono::Local::now().date_naive();
    (0..n.max(1) as u64)
        .rev()
        .filter_map(|i| today.checked_sub_days(chrono::Days::new(i)))
        .map(|d| d.format("%Y-%m-%d").to_string())
        .collect()
}

/// Totals of the rows that fall on `dates`.
pub fn aggregate(rows: &[UsageRow], dates: &[String]) -> UsageReport {
    let mut days: Vec<DayUsage> = dates.iter().map(|d| DayUsage { date: d.clone(), ..Default::default() }).collect();
    let mut by_model: Vec<ModelUsage> = Vec::new();
    let mut by_feature: Vec<FeatureUsage> = Vec::new();
    for r in rows {
        let Some(day) = days.iter_mut().find(|d| d.date == r.date) else { continue };
        day.calls += r.calls;
        day.input_tokens += r.input_tokens;
        day.output_tokens += r.output_tokens;
        day.errors += r.errors;
        match by_model.iter_mut().find(|m| m.provider == r.provider && m.model == r.model) {
            Some(m) => {
                m.calls += r.calls;
                m.input_tokens += r.input_tokens;
                m.output_tokens += r.output_tokens;
            }
            None => by_model.push(ModelUsage {
                provider: r.provider.clone(),
                model: r.model.clone(),
                calls: r.calls,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
            }),
        }
        match by_feature.iter_mut().find(|f| f.feature == r.feature) {
            Some(f) => {
                f.calls += r.calls;
                f.input_tokens += r.input_tokens;
                f.output_tokens += r.output_tokens;
            }
            None => by_feature.push(FeatureUsage {
                feature: r.feature.clone(),
                calls: r.calls,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
            }),
        }
    }
    by_model.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.provider.cmp(&b.provider)).then_with(|| a.model.cmp(&b.model)));
    by_feature.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.feature.cmp(&b.feature)));
    UsageReport { days, by_model, by_feature }
}

/// Daily: drops rows older than about a year.
pub(crate) fn prune(e: &Engine) {
    let Some(cutoff) = chrono::Local::now().date_naive().checked_sub_days(chrono::Days::new(KEEP_DAYS)) else { return };
    let cutoff = cutoff.format("%Y-%m-%d").to_string();
    let rows: Vec<UsageRow> = e.db.kv_list(SPACE).unwrap_or_default();
    for r in rows {
        // "YYYY-MM-DD" compares like the date.
        if r.date < cutoff {
            let _ = e.db.kv_delete(SPACE, &row_key(&r.date, &r.provider, &r.model, &r.feature));
        }
    }
}

impl Engine {
    /// Counts one AI call (today, local time).
    pub(crate) fn ai_record_usage(&self, provider: &str, model: &str, feature: &str, usage: TokenUsage, duration_ms: u64, ok: bool) {
        let date = local_day();
        self.ai_add_usage(&date, provider, model, feature, 1, usage, duration_ms, !ok);
    }

    /// Adds calls to the row of (date, provider, model, feature).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ai_add_usage(
        &self,
        date: &str,
        provider: &str,
        model: &str,
        feature: &str,
        calls: u64,
        usage: TokenUsage,
        duration_ms: u64,
        error: bool,
    ) {
        let key = row_key(date, provider, model, feature);
        let _guard = self.ai.usage_lock.lock();
        let mut row: UsageRow = self.db.kv_get(SPACE, &key).ok().flatten().unwrap_or_else(|| UsageRow {
            date: date.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            feature: feature.to_string(),
            ..Default::default()
        });
        row.calls += calls;
        row.input_tokens += usage.input_tokens;
        row.output_tokens += usage.output_tokens;
        row.duration_ms += duration_ms;
        if error {
            row.errors += 1;
        }
        if let Err(err) = self.db.kv_put(SPACE, &key, &row, 0) {
            tracing::warn!(error = %err, "could not store ai usage");
        }
    }

    /// `GET /v1/ai/usage?days=30`
    pub fn ai_usage(&self, days: u32) -> UsageReport {
        let dates = last_days(days.clamp(1, 366));
        let rows: Vec<UsageRow> = self.db.kv_list(SPACE).unwrap_or_default();
        aggregate(&rows, &dates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(date: &str, provider: &str, model: &str, feature: &str, calls: u64, input: u64, output: u64, errors: u64) -> UsageRow {
        UsageRow {
            date: date.into(),
            provider: provider.into(),
            model: model.into(),
            feature: feature.into(),
            calls,
            input_tokens: input,
            output_tokens: output,
            errors,
            duration_ms: 0,
        }
    }

    #[test]
    fn aggregates_by_day_model_and_feature() {
        let rows = vec![
            row("2026-10-21", "deepseek", "deepseek-chat", "summary", 3, 300, 30, 0),
            row("2026-10-23", "deepseek", "deepseek-chat", "compose", 2, 200, 100, 1),
            row("2026-10-23", "claude_code", "default", "summary", 5, 50, 5, 0),
            row("2026-09-01", "deepseek", "deepseek-chat", "summary", 9, 9, 9, 9),
        ];
        let dates: Vec<String> = ["2026-10-21", "2026-10-22", "2026-10-23"].iter().map(|s| s.to_string()).collect();
        let r = aggregate(&rows, &dates);
        assert_eq!(r.days.len(), 3);
        assert_eq!(r.days[0], DayUsage { date: "2026-10-21".into(), calls: 3, input_tokens: 300, output_tokens: 30, errors: 0 });
        assert_eq!(r.days[1].calls, 0);
        assert_eq!(r.days[2], DayUsage { date: "2026-10-23".into(), calls: 7, input_tokens: 250, output_tokens: 105, errors: 1 });
        // Out of range rows are not counted anywhere.
        assert_eq!(r.by_model.len(), 2);
        assert_eq!((r.by_model[0].provider.as_str(), r.by_model[0].calls), ("claude_code", 5));
        assert_eq!((r.by_model[1].provider.as_str(), r.by_model[1].calls, r.by_model[1].input_tokens), ("deepseek", 5, 500));
        assert_eq!(r.by_feature[0], FeatureUsage { feature: "summary".into(), calls: 8, input_tokens: 350, output_tokens: 35 });
        assert_eq!(r.by_feature[1].feature, "compose");
        assert_eq!(row_key("2026-10-23", "a", "b", "c"), "2026-10-23|a|b|c");
        let d = last_days(7);
        assert_eq!(d.len(), 7);
        assert!(d[0] < d[6]);
        assert_eq!(d[6], local_day());
    }
}
