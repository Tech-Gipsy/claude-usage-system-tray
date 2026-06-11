use crate::pricing::cost_usd;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

// ---------- stats-cache.json ----------

#[derive(Debug, Clone)]
pub struct CachedDay {
    pub date: String,
    pub tokens: u64,
}

#[derive(Deserialize)]
struct StatsCache {
    #[serde(rename = "dailyModelTokens", default)]
    daily_model_tokens: Vec<DailyModelTokens>,
}

#[derive(Deserialize)]
struct DailyModelTokens {
    date: String,
    #[serde(rename = "tokensByModel", default)]
    tokens_by_model: BTreeMap<String, u64>,
}

pub fn read_stats_cache(path: &Path) -> Option<Vec<CachedDay>> {
    let raw = fs::read_to_string(path).ok()?;
    let cache: StatsCache = serde_json::from_str(&raw).ok()?;
    let mut days: Vec<CachedDay> = cache
        .daily_model_tokens
        .into_iter()
        .map(|d| CachedDay { date: d.date, tokens: d.tokens_by_model.values().sum() })
        .collect();
    days.sort_by(|a, b| a.date.cmp(&b.date));
    Some(days)
}

// ---------- JSONL transcripts ----------

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub timestamp: String, // ISO 8601
    pub model: String,
    pub usage: Usage,
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

/// Parse one transcript file; return (dedupe-key, entry) pairs WITHOUT global dedup.
/// Skips malformed lines and non-assistant lines. Key is `"msg_id:requestId"`.
pub fn parse_jsonl_file(path: &Path) -> Vec<(String, Entry)> {
    let Ok(file) = fs::File::open(path) else { return vec![] };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(raw) = serde_json::from_str::<RawLine>(&line) else { continue };
        if raw.kind.as_deref() != Some("assistant") {
            continue;
        }
        let Some(msg) = raw.message else { continue };
        let (Some(id), Some(model), Some(usage)) = (msg.id, msg.model, msg.usage) else {
            continue;
        };
        let key = format!("{id}:{}", raw.request_id.unwrap_or_default());
        out.push((
            key,
            Entry {
                timestamp: raw.timestamp.unwrap_or_default(),
                model,
                usage,
            },
        ));
    }
    out
}

// ---------- per-file parse cache (Fix 2) ----------

struct CachedFile {
    mtime: std::time::SystemTime,
    len: u64,
    entries: Vec<(String, Entry)>, // (dedupe key, entry)
}

#[derive(Default)]
pub struct FileCache {
    map: std::collections::HashMap<PathBuf, CachedFile>,
    /// Parsed stats-cache.json guarded by (mtime, len) so it is only re-read on change.
    stats_cache: Option<(std::time::SystemTime, u64, Vec<CachedDay>)>,
}

/// Read stats-cache.json through the FileCache: re-parse only when mtime/len changed.
/// A missing/unstattable file clears the cached entry and yields an empty Vec.
fn read_stats_cache_cached(path: &Path, cache: &mut FileCache) -> Vec<CachedDay> {
    let Ok(meta) = fs::metadata(path) else {
        cache.stats_cache = None;
        return Vec::new();
    };
    let (mtime, len) = (meta.modified().unwrap_or(UNIX_EPOCH), meta.len());
    if let Some((m, l, days)) = &cache.stats_cache {
        if *m == mtime && *l == len {
            return days.clone();
        }
    }
    let days = read_stats_cache(path).unwrap_or_default();
    cache.stats_cache = Some((mtime, len, days.clone()));
    days
}

/// Entries from all recent transcripts, using the per-file cache (re-parse only when
/// mtime/len changed), then globally deduped by key.
pub fn load_recent_entries(claude_dir: &Path, days: u64, cache: &mut FileCache) -> Vec<Entry> {
    let mut keep = std::collections::HashSet::new();
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in recent_transcripts(claude_dir, days) {
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let (mtime, len) = (meta.modified().unwrap_or(UNIX_EPOCH), meta.len());
        let fresh = match cache.map.get(&path) {
            Some(c) if c.mtime == mtime && c.len == len => false,
            _ => true,
        };
        if fresh {
            let parsed = parse_jsonl_file(&path);
            cache.map.insert(path.clone(), CachedFile { mtime, len, entries: parsed });
        }
        keep.insert(path.clone());
        if let Some(c) = cache.map.get(&path) {
            for (key, e) in &c.entries {
                if seen.insert(key.clone()) {
                    entries.push(e.clone());
                }
            }
        }
    }
    // drop files that aged out of the window
    cache.map.retain(|p, _| keep.contains(p));
    entries
}

// ---------- aggregation ----------

#[derive(Debug, Default, Clone)]
pub struct DayAgg {
    pub tokens: u64,
    pub cost_usd: f64,
    /// Tokens for which a price is known (used to compute blended rate). (Fix 3)
    pub priced_tokens: u64,
}

/// Bucket entries by local date (timestamps are UTC; convert via chrono::Local).
/// Entries with unparseable RFC3339 timestamps are skipped. (Fix 4)
pub fn bucket_by_day(entries: &[Entry]) -> BTreeMap<String, DayAgg> {
    use chrono::{DateTime, Local};
    let mut days: BTreeMap<String, DayAgg> = BTreeMap::new();
    for e in entries {
        // Fix 4: skip entries whose timestamp fails RFC3339 parsing instead of falling back.
        let Ok(parsed_ts) = DateTime::parse_from_rfc3339(&e.timestamp) else { continue };
        let date = parsed_ts.with_timezone(&Local).format("%Y-%m-%d").to_string();
        let agg = days.entry(date).or_default();
        let u = &e.usage;
        let token_sum = u.input_tokens
            + u.output_tokens
            + u.cache_creation_input_tokens
            + u.cache_read_input_tokens;
        agg.tokens += token_sum;
        // Fix 3: only include tokens in priced_tokens when cost is known.
        if let Some(c) = cost_usd(
            &e.model,
            u.input_tokens,
            u.output_tokens,
            u.cache_creation_input_tokens,
            u.cache_read_input_tokens,
        ) {
            agg.priced_tokens += token_sum;
            agg.cost_usd += c;
        }
    }
    days
}

/// All transcript files under ~/.claude/projects modified within `days` days.
pub fn recent_transcripts(claude_dir: &Path, days: u64) -> Vec<PathBuf> {
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(days * 24 * 3600);
    let mut out = Vec::new();
    let mut stack = vec![claude_dir.join("projects")];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl")
                && entry.metadata().and_then(|m| m.modified()).is_ok_and(|m| m >= cutoff)
            {
                out.push(path);
            }
        }
    }
    out
}

use crate::snapshot::{DayStat, LocalStats};

/// `today` as local YYYY-MM-DD (injected for testability).
pub fn collect_local_stats(claude_dir: &Path, today: &str, cache: &mut FileCache) -> LocalStats {
    // 1. Accurate 7-day window from JSONL (mtime-bounded scan, per-file cached).
    let entries = load_recent_entries(claude_dir, 8, cache);
    let mut live = bucket_by_day(&entries);

    // Fix 1: bound "live" data to the 7-day window so stale entries in long-lived
    // session files don't inflate totals or suppress cache history.
    let today_date = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .unwrap_or_else(|_| chrono::Local::now().date_naive());
    let window_start = (today_date - chrono::Duration::days(6)).format("%Y-%m-%d").to_string();
    live.retain(|d, _| d.as_str() >= window_start.as_str());

    // 2. History from stats-cache (tokens only), via the mtime/len-guarded cache.
    let cached = read_stats_cache_cached(&claude_dir.join("stats-cache.json"), cache);
    let cached_by_date: std::collections::HashMap<&str, &CachedDay> =
        cached.iter().map(|c| (c.date.as_str(), c)).collect();

    // 3. Blended $/token from the live window (priced tokens only), used to estimate history cost.
    // Fix 3: use priced_tokens for the denominator to exclude unpriced tokens.
    let (live_tokens, live_priced_tokens, live_cost): (u64, u64, f64) = live
        .values()
        .fold((0, 0, 0.0), |(t, pt, c), d| (t + d.tokens, pt + d.priced_tokens, c + d.cost_usd));
    let blended = if live_priced_tokens > 0 { live_cost / live_priced_tokens as f64 } else { 0.0 };

    // 4. Last 7 local dates ending today; prefer live data, fall back to cache.
    let mut days = Vec::with_capacity(7);
    for i in (0..7).rev() {
        let date = (today_date - chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
        if let Some(d) = live.get(&date) {
            days.push(DayStat { date, tokens: d.tokens, cost_usd: Some(d.cost_usd) });
        } else if let Some(c) = cached_by_date.get(date.as_str()) {
            days.push(DayStat {
                date,
                tokens: c.tokens,
                cost_usd: if blended > 0.0 { Some(c.tokens as f64 * blended) } else { None },
            });
        } else {
            days.push(DayStat { date, tokens: 0, cost_usd: Some(0.0) });
        }
    }

    // 5. 30-day totals: cached days within window (excluding live-covered dates) + live.
    let cutoff = (today_date - chrono::Duration::days(29)).format("%Y-%m-%d").to_string();
    let cached_tokens: u64 = cached
        .iter()
        .filter(|c| c.date.as_str() >= cutoff.as_str() && !live.contains_key(&c.date))
        .map(|c| c.tokens)
        .sum();
    let total_tokens = cached_tokens + live_tokens;
    let total_cost = live_cost + cached_tokens as f64 * blended;

    let today_agg = live.get(today).cloned().unwrap_or_default();
    LocalStats {
        today_tokens: today_agg.tokens,
        today_cost_usd: today_agg.cost_usd,
        days,
        total_30d_tokens: total_tokens,
        total_30d_cost_usd: total_cost,
        cost_is_estimate: cached_tokens > 0,
        fetched_at: chrono::Utc::now().to_rfc3339(),
        stale: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture as fixture;

    /// Convert a UTC RFC3339 timestamp to the local YYYY-MM-DD date string,
    /// matching the same conversion used by bucket_by_day.
    fn local_date(ts: &str) -> String {
        chrono::DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string()
    }

    #[test]
    fn parses_stats_cache_daily_tokens() {
        let days = read_stats_cache(&fixture("stats-cache.json")).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].date, "2026-06-08");
        assert_eq!(days[0].tokens, 1_000_000);
        assert_eq!(days[1].tokens, 750_000);
    }

    #[test]
    fn jsonl_parse_dedupes_and_skips_bad_lines() {
        let pairs = parse_jsonl_file(&fixture("transcript.jsonl"));
        // parse_jsonl_file no longer dedupes — that is done globally in load_recent_entries.
        // The fixture has 3 assistant lines (line 3 is a dup of line 1), 1 bad JSON line,
        // and 1 user-type line; both bad/non-assistant are skipped, so we get 3 pairs.
        assert_eq!(pairs.len(), 3);
        let e = &pairs[0].1;
        assert_eq!(e.model, "claude-sonnet-4-6");
        assert_eq!(e.usage.input_tokens, 100);
        assert_eq!(e.usage.cache_read_input_tokens, 1000);
    }

    #[test]
    fn aggregates_entries_into_day_buckets() {
        // Use load_recent_entries via a temp dir to get globally-deduped entries.
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("projects/p1");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::copy(fixture("transcript.jsonl"), proj.join("t.jsonl")).unwrap();
        let mut cache = FileCache::default();
        let entries = load_recent_entries(tmp.path(), 8, &mut cache);
        // The two fixture timestamps (2026-06-10T08:00:00Z and 2026-06-10T09:00:00Z)
        // are assumed to fall on the same local date (true for UTC+5:30 dev and UTC CI).
        let expected_date = local_date("2026-06-10T08:00:00Z");
        let days = bucket_by_day(&entries);
        assert_eq!(days.len(), 1);
        let (date, agg) = days.iter().next().unwrap();
        assert_eq!(date, &expected_date);
        assert_eq!(agg.tokens, 1380);
        assert!(agg.cost_usd > 0.0);
    }

    #[test]
    fn collect_merges_live_window_with_cached_history() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("projects/p1");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::copy(fixture("transcript.jsonl"), proj.join("t.jsonl")).unwrap();
        std::fs::copy(fixture("stats-cache.json"), tmp.path().join("stats-cache.json")).unwrap();

        // Use local_date so the today arg matches the local bucket date for the fixture entries.
        // The two fixture timestamps are assumed to fall on the same local date.
        let today = local_date("2026-06-10T08:00:00Z");
        let mut cache = FileCache::default();
        let stats = collect_local_stats(tmp.path(), &today, &mut cache);
        assert_eq!(stats.days.last().unwrap().date, today);
        assert_eq!(stats.today_tokens, 1380);
        assert!(stats.today_cost_usd > 0.0);
        assert_eq!(stats.total_30d_tokens, 1_000_000 + 750_000 + 1380);
        assert!(stats.cost_is_estimate);
        assert_eq!(stats.days.len(), 7);
    }

    #[test]
    fn dedupes_across_files() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("projects/p1");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::copy(fixture("transcript.jsonl"), proj.join("a.jsonl")).unwrap();
        std::fs::copy(fixture("transcript.jsonl"), proj.join("b.jsonl")).unwrap();
        let mut cache = FileCache::default();
        let entries = load_recent_entries(tmp.path(), 8, &mut cache);
        assert_eq!(entries.len(), 2); // same content in both files counted once
    }

    #[test]
    fn file_cache_skips_unchanged_files() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("projects/p1");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::copy(fixture("transcript.jsonl"), proj.join("a.jsonl")).unwrap();
        let mut cache = FileCache::default();
        let first = load_recent_entries(tmp.path(), 8, &mut cache);
        let second = load_recent_entries(tmp.path(), 8, &mut cache);
        assert_eq!(first.len(), second.len());
        assert_eq!(cache.map.len(), 1);
    }

    #[test]
    fn old_entries_in_recent_files_do_not_pollute_totals() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("projects/p1");
        std::fs::create_dir_all(&proj).unwrap();
        // one entry from months ago inside a freshly-modified file
        let old_line = r#"{"type":"assistant","timestamp":"2026-01-01T08:00:00Z","requestId":"req_old","message":{"id":"msg_old","model":"claude-sonnet-4-6","usage":{"input_tokens":999999,"output_tokens":0}}}"#;
        std::fs::write(proj.join("old.jsonl"), old_line).unwrap();
        std::fs::copy(fixture("stats-cache.json"), tmp.path().join("stats-cache.json")).unwrap();
        let mut cache = FileCache::default();
        let stats = collect_local_stats(tmp.path(), "2026-06-10", &mut cache);
        // live old entry dropped; totals come from stats-cache only
        assert_eq!(stats.total_30d_tokens, 1_750_000);
        assert_eq!(stats.today_tokens, 0);
    }
}
