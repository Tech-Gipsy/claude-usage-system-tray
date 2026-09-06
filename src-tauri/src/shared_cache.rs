//! The Claude Display companion app (identifier `ai.digitals.claudedisplay`)
//! fetches the same usage endpoint on a careful schedule and writes its last
//! good reading, whole, to `usage-cache.json` in its data directory. The
//! endpoint rate-limits per account, so two apps asking is one too many: while
//! that file is present and fresh the meter serves its numbers from it and
//! never touches the endpoint itself. When it is missing or old -- the display
//! app is not installed, or not running -- the meter fetches as it always did.
use crate::snapshot::Limits;
use std::path::{Path, PathBuf};

/// How old the display app's reading may be and still be served. Its poll
/// interval is 15 min with jitter; twice that covers one missed fetch.
pub const FRESH_S: i64 = 30 * 60;

/// Where the display app keeps `usage-cache.json`: Tauri's app-data directory
/// for its identifier, which is the config dir on Windows and macOS and the
/// data dir on Linux.
pub fn display_cache_path() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    let base = dirs::data_dir();
    #[cfg(not(target_os = "linux"))]
    let base = dirs::config_dir();
    base.map(|b| b.join("ai.digitals.claudedisplay").join("usage-cache.json"))
}

/// The display app's reading as [`Limits`], or `None` when the file is
/// absent, unreadable, corrupt, from the future, or older than [`FRESH_S`].
pub fn read_shared(path: &Path, now: i64) -> Option<Limits> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_shared(&text, now)
}

/// The file is `{ "fetched_at": <epoch s>, "usage": { "limits": [ { "id",
/// "percent", "resets_at": <epoch s>, ... } ] } }`. The session window is the
/// limit with id `session`; the weekly one is `weekly_all`, or failing that
/// the first id starting with `weekly` (a model-scoped week).
pub fn parse_shared(text: &str, now: i64) -> Option<Limits> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let fetched_at = v.get("fetched_at")?.as_i64()?;
    if fetched_at > now + 60 || now - fetched_at > FRESH_S {
        return None;
    }
    let limits = v.get("usage")?.get("limits")?.as_array()?;
    let with_id = |pred: &dyn Fn(&str) -> bool| {
        limits
            .iter()
            .find(|l| l.get("id").and_then(|i| i.as_str()).map(pred).unwrap_or(false))
    };
    let session = with_id(&|id| id == "session")?;
    let weekly =
        with_id(&|id| id == "weekly_all").or_else(|| with_id(&|id| id.starts_with("weekly")))?;
    let pct = |l: &serde_json::Value| l.get("percent").and_then(|p| p.as_f64()).unwrap_or(0.0) as f32;
    let resets = |l: &serde_json::Value| l.get("resets_at").and_then(|r| r.as_i64()).and_then(iso);
    Some(Limits {
        session_pct: pct(session),
        session_resets_at: resets(session),
        weekly_pct: pct(weekly),
        weekly_resets_at: resets(weekly),
        fetched_at: iso(fetched_at)?,
        stale: false,
    })
}

fn iso(epoch: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0).map(|d| d.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_788_661_000;

    fn doc(fetched_at: i64, weekly_id: &str) -> String {
        format!(
            r#"{{"fetched_at": {fetched_at}, "usage": {{"limits": [
                {{"id": "session", "label": "5h", "percent": 4.0, "resets_at": 1788666599}},
                {{"id": "{weekly_id}", "label": "Week", "percent": 21.0, "resets_at": 1788778799}},
                {{"id": "weekly_scoped:Fable", "label": "Fable", "percent": 28.0, "resets_at": 1788778799}}
            ]}}}}"#
        )
    }

    #[test]
    fn maps_the_display_apps_reading() {
        let l = parse_shared(&doc(NOW - 120, "weekly_all"), NOW).expect("fresh");
        assert_eq!(l.session_pct, 4.0);
        assert_eq!(l.weekly_pct, 21.0);
        assert_eq!(l.session_resets_at.as_deref(), Some("2026-09-06T03:49:59+00:00"));
        assert_eq!(l.weekly_resets_at.as_deref(), Some("2026-09-07T10:59:59+00:00"));
        assert!(l.fetched_at.starts_with("2026-09-06T"));
        assert!(!l.stale);
    }

    #[test]
    fn a_model_scoped_week_stands_in_when_there_is_no_account_week() {
        let l = parse_shared(&doc(NOW - 120, "weekly_scoped:Opus"), NOW).expect("fresh");
        assert_eq!(l.weekly_pct, 21.0);
    }

    #[test]
    fn a_stale_future_or_broken_file_is_no_reading() {
        assert!(parse_shared(&doc(NOW - FRESH_S - 1, "weekly_all"), NOW).is_none());
        assert!(parse_shared(&doc(NOW + 120, "weekly_all"), NOW).is_none());
        assert!(parse_shared("{not json", NOW).is_none());
        assert!(parse_shared(r#"{"fetched_at": 1788661000, "usage": {"limits": []}}"#, NOW).is_none());
        assert!(parse_shared(&doc(NOW - FRESH_S + 5, "weekly_all"), NOW).is_some());
    }

    #[test]
    fn reads_from_disk_and_tolerates_absence() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("usage-cache.json");
        assert!(read_shared(&p, NOW).is_none());
        std::fs::write(&p, doc(NOW - 5, "weekly_all")).unwrap();
        assert_eq!(read_shared(&p, NOW).unwrap().session_pct, 4.0);
    }
}
