use crate::pricing::kimi_cost_usd;
use crate::snapshot::{DayStat, KimiLocal, LocalStats};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq)]
pub struct KimiRecord {
    pub time_ms: i64,
    pub model: String,
    /// inputOther + inputCacheCreation
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
}

#[derive(Default)]
pub struct KimiFileCache {
    map: HashMap<PathBuf, (u64, SystemTime, Vec<KimiRecord>)>,
    /// Number of actual file parses (test observability).
    pub(crate) parse_count: usize,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(rename = "inputOther", default)]
    input_other: u64,
    #[serde(default)]
    output: u64,
    #[serde(rename = "inputCacheRead", default)]
    input_cache_read: u64,
    #[serde(rename = "inputCacheCreation", default)]
    input_cache_creation: u64,
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "usageScope")]
    scope: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
    time: Option<i64>,
}

/// Parse one wire.jsonl; only per-turn usage records count (session records are aggregates).
/// Malformed or truncated lines are skipped.
pub fn parse_wire_file(path: &Path) -> Vec<KimiRecord> {
    use std::io::{BufRead, BufReader};
    let Ok(file) = std::fs::File::open(path) else { return vec![] };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(raw) = serde_json::from_str::<RawLine>(&line) else { continue };
        if raw.kind.as_deref() != Some("usage.record") || raw.scope.as_deref() != Some("turn") {
            continue;
        }
        let (Some(u), Some(time_ms)) = (raw.usage, raw.time) else { continue };
        out.push(KimiRecord {
            time_ms,
            model: raw.model.unwrap_or_default(),
            input: u.input_other + u.input_cache_creation,
            output: u.output,
            cache_read: u.input_cache_read,
        });
    }
    out
}

/// `sessions/**/wire.jsonl` files modified within `days` days.
fn recent_wire_files(kimi_dir: &Path, days: u64) -> Vec<PathBuf> {
    let cutoff = SystemTime::now() - std::time::Duration::from_secs(days * 24 * 3600);
    let mut out = Vec::new();
    let mut stack = vec![kimi_dir.join("sessions")];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "wire.jsonl")
                && entry.metadata().and_then(|m| m.modified()).is_ok_and(|m| m >= cutoff)
            {
                out.push(path);
            }
        }
    }
    out
}

#[derive(Default, Clone)]
struct Agg {
    tokens: u64,
    cost: f64,
    unpriced: u64,
}

pub fn collect_kimi_local(kimi_dir: &Path, today: &str, cache: &mut KimiFileCache) -> KimiLocal {
    use chrono::{Local, TimeZone};
    let mut keep = std::collections::HashSet::new();
    let mut by_day: std::collections::BTreeMap<String, Agg> = Default::default();
    for path in recent_wire_files(kimi_dir, 31) {
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let (mtime, len) = (meta.modified().unwrap_or(UNIX_EPOCH), meta.len());
        let fresh = !matches!(cache.map.get(&path), Some((l, m, _)) if *l == len && *m == mtime);
        if fresh {
            let recs = parse_wire_file(&path);
            cache.parse_count += 1;
            cache.map.insert(path.clone(), (len, mtime, recs));
        }
        keep.insert(path.clone());
        for r in &cache.map[&path].2 {
            let Some(dt) = Local.timestamp_millis_opt(r.time_ms).single() else { continue };
            let agg = by_day.entry(dt.format("%Y-%m-%d").to_string()).or_default();
            let tokens = r.input + r.output + r.cache_read;
            agg.tokens += tokens;
            match kimi_cost_usd(&r.model, r.input, r.output, r.cache_read) {
                Some(c) => agg.cost += c,
                None => agg.unpriced += tokens,
            }
        }
    }
    cache.map.retain(|p, _| keep.contains(p));

    let today_date = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .unwrap_or_else(|_| Local::now().date_naive());
    let fmt = |i: i64| (today_date - chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
    let days = (0..7)
        .rev()
        .map(|i| {
            let date = fmt(i);
            let a = by_day.get(&date).cloned().unwrap_or_default();
            DayStat { date, tokens: a.tokens, cost_usd: Some(a.cost) }
        })
        .collect();
    let (mut t30, mut c30, mut u30) = (0u64, 0f64, 0u64);
    for i in 0..30 {
        if let Some(a) = by_day.get(&fmt(i)) {
            t30 += a.tokens;
            c30 += a.cost;
            u30 += a.unpriced;
        }
    }
    let t = by_day.get(&today_date.format("%Y-%m-%d").to_string()).cloned().unwrap_or_default();
    KimiLocal {
        stats: LocalStats {
            today_tokens: t.tokens,
            today_cost_usd: t.cost,
            days,
            total_30d_tokens: t30,
            total_30d_cost_usd: c30,
            cost_is_estimate: false,
            fetched_at: chrono::Utc::now().to_rfc3339(),
            stale: false,
        },
        unpriced_today_tokens: t.unpriced,
        unpriced_30d_tokens: u30,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture as fixture;

    fn main_wire() -> PathBuf {
        fixture("kimi/sessions/wd_x/session_a/agents/main/wire.jsonl")
    }

    #[test]
    fn only_turn_records_count() {
        let r = parse_wire_file(&main_wire());
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|x| x.input < 1_000_000));
    }

    #[test]
    fn truncated_last_line_is_skipped() {
        assert_eq!(parse_wire_file(&main_wire()).len(), 2);
    }

    #[test]
    fn input_includes_cache_creation() {
        let r = parse_wire_file(&main_wire());
        assert_eq!(r[1].input, 550);
        assert_eq!(r[1].cache_read, 1000);
        assert_eq!(r[1].output, 100);
    }

    /// Copy a fixture, replacing every `"time":0` with now-ms, mtime = now (Windows copy keeps mtime).
    fn copy_now(src: PathBuf, dst: PathBuf, now_ms: i64) {
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        let s = std::fs::read_to_string(src).unwrap().replace("\"time\":0", &format!("\"time\":{now_ms}"));
        std::fs::write(&dst, s).unwrap();
    }

    fn setup() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let now = chrono::Local::now().timestamp_millis();
        for a in ["main", "agent-0"] {
            let rel = format!("sessions/wd_x/session_a/agents/{a}/wire.jsonl");
            copy_now(fixture(&format!("kimi/{rel}")), tmp.path().join(&rel), now);
        }
        tmp
    }

    #[test]
    fn collect_buckets_and_splits_priced_vs_unpriced() {
        let tmp = setup();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut cache = KimiFileCache::default();
        let k = collect_kimi_local(tmp.path(), &today, &mut cache);
        // main: 4200 + 1650; agent-0: 970
        assert_eq!(k.stats.today_tokens, 4200 + 1650 + 970);
        assert_eq!(k.unpriced_today_tokens, 970);
        assert_eq!(k.unpriced_30d_tokens, 970);
        assert_eq!(k.stats.total_30d_tokens, 6820);
        assert!(k.stats.today_cost_usd > 0.0);
        assert!(!k.stats.cost_is_estimate);
        assert_eq!(k.stats.days.len(), 7);
        assert_eq!(k.stats.days.last().unwrap().date, today);
        assert_eq!(k.stats.days.last().unwrap().tokens, 6820);
        assert_eq!(k.stats.days[0].tokens, 0);
    }

    #[test]
    fn cache_skips_unchanged_files() {
        let tmp = setup();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut cache = KimiFileCache::default();
        collect_kimi_local(tmp.path(), &today, &mut cache);
        assert_eq!(cache.parse_count, 2);
        collect_kimi_local(tmp.path(), &today, &mut cache);
        assert_eq!(cache.parse_count, 2);
    }
}
