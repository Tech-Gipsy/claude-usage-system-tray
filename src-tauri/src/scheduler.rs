use crate::kimi;
use crate::snapshot::{ApiSpend, KimiSnapshot, UsageSnapshot};
use crate::{api_spend, limits, local_stats, tray_icon};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

pub struct AppState {
    pub snapshot: Mutex<UsageSnapshot>,
    pub file_cache: Mutex<local_stats::FileCache>,
    pub claude_dir: PathBuf,
    /// Last tray render (mode with rounded pcts, tooltip); None = nothing rendered yet.
    pub last_tray: Mutex<Option<(TrayMode, String)>>,
    /// When the meter last asked the endpoint itself rather than the display
    /// app's cache; that fallback is throttled to [`OWN_FETCH_EVERY`].
    pub last_own_fetch: Mutex<Option<std::time::Instant>>,
    pub kimi_file_cache: Mutex<kimi::local::KimiFileCache>,
    /// When Kimi `/usages` was last requested; throttled to [`OWN_FETCH_EVERY`].
    pub last_kimi_fetch: Mutex<Option<std::time::Instant>>,
}

impl AppState {
    pub fn new() -> Self {
        let claude_dir = dirs::home_dir().unwrap_or_default().join(".claude");
        Self {
            snapshot: Mutex::new(UsageSnapshot::default()),
            file_cache: Mutex::new(local_stats::FileCache::default()),
            claude_dir,
            last_tray: Mutex::new(None),
            last_own_fetch: Mutex::new(None),
            kimi_file_cache: Mutex::new(kimi::local::KimiFileCache::default()),
            last_kimi_fetch: Mutex::new(None),
        }
    }
}

#[derive(PartialEq, Clone, Debug)]
pub enum TrayMode {
    Single(Option<f32>),
    /// (Claude weekly, Kimi weekly)
    Split(Option<f32>, Option<f32>),
}

impl TrayMode {
    /// Whole-number percentages: the change-detection key. Rendering uses the raw values.
    fn rounded(&self) -> TrayMode {
        let r = |p: &Option<f32>| p.map(|p| p.round());
        match self {
            TrayMode::Single(p) => TrayMode::Single(r(p)),
            TrayMode::Split(c, k) => TrayMode::Split(r(c), r(k)),
        }
    }
}

/// What the tray should show: raw percentages (what gets drawn, so ring colors
/// match the popup) and the tooltip. Compare via [`TrayMode::rounded`].
pub fn tray_render_key(snap: &UsageSnapshot) -> (TrayMode, String) {
    let subscribed = snap.kimi.as_ref().map(|k| k.has_subscription).unwrap_or(false);
    if subscribed {
        let c = snap.limits.as_ref().map(|l| l.weekly_pct);
        let k = snap
            .kimi
            .as_ref()
            .and_then(|k| k.limits.as_ref())
            .and_then(|l| l.weekly_pct);
        let fmt = |p: Option<f32>| p.map(|p| format!("{:.0}%", p)).unwrap_or_else(|| "—".into());
        let tooltip = format!("Claude weekly {} · Kimi weekly {}", fmt(c), fmt(k));
        return (TrayMode::Split(c, k), tooltip);
    }
    let pct = snap.limits.as_ref().map(|l| l.session_pct);
    let tooltip = match &snap.limits {
        Some(l) => format!("Session {:.0}% · Weekly {:.0}%", l.session_pct, l.weekly_pct),
        None => "Claude Usage Meter".to_string(),
    };
    (TrayMode::Single(pct), tooltip)
}

fn publish(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut snap = state.snapshot.lock().unwrap().clone();
    expire_stale_limits(&mut snap, chrono::Utc::now());

    // tray icon + tooltip — only re-render when the rounded pcts or tooltip changed
    let (mode, tooltip) = tray_render_key(&snap);
    let key = (mode.rounded(), tooltip);
    {
        let mut last = state.last_tray.lock().unwrap();
        if last.as_ref() != Some(&key) {
            let (rgba, w, h) = match &mode {
                TrayMode::Single(p) => tray_icon::render_ring(*p),
                TrayMode::Split(c, k) => tray_icon::render_split_ring(*c, *k),
            };
            if let Some(tray) = app.tray_by_id("main") {
                let _ = tray.set_icon(Some(tauri::image::Image::new_owned(rgba, w, h)));
                let _ = tray.set_tooltip(Some(key.1.clone()));
            }
            *last = Some(key);
        }
    }

    let _ = app.emit("snapshot", &snap);
}

/// The meter's own endpoint fetches, when the display app's cache is not
/// there to read, are spaced at least this far apart. The endpoint's budget
/// is per account, and a minute was what kept it tripped.
const OWN_FETCH_EVERY: std::time::Duration = std::time::Duration::from_secs(300);

pub async fn refresh_limits(app: &AppHandle) {
    // The display app's reading first: same endpoint, same account, and it
    // has already paid for the request. See shared_cache.rs.
    if let Some(path) = crate::shared_cache::display_cache_path() {
        if let Some(l) = crate::shared_cache::read_shared(&path, chrono::Utc::now().timestamp()) {
            app.state::<AppState>().snapshot.lock().unwrap().limits = Some(l);
            publish(app);
            return;
        }
    }
    // No fresh shared reading: ask the endpoint ourselves, but not more
    // often than OWN_FETCH_EVERY, whatever the loop's cadence.
    {
        let state = app.state::<AppState>();
        let mut last = state.last_own_fetch.lock().unwrap();
        if last.map(|t| t.elapsed() < OWN_FETCH_EVERY).unwrap_or(false) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }
    // macOS keeps the OAuth token in Claude Code's Keychain; elsewhere it's the file.
    #[cfg(target_os = "macos")]
    let store = limits::CredStore::Keychain;
    #[cfg(not(target_os = "macos"))]
    let store =
        limits::CredStore::File(app.state::<AppState>().claude_dir.join(".credentials.json"));
    let result = limits::get_limits(limits::USAGE_BASE, limits::TOKEN_BASE, &store).await;
    {
        let state = app.state::<AppState>();
        let mut snap = state.snapshot.lock().unwrap();
        match result {
            Ok(l) => snap.limits = Some(l),
            Err(_) => {
                // Keep last-good values, just mark them stale.
                if let Some(l) = snap.limits.as_mut() {
                    l.stale = true;
                }
            }
        }
    }
    publish(app);
}

pub async fn refresh_local(app: &AppHandle) {
    let (claude_dir, mut cache) = {
        let state = app.state::<AppState>();
        let claude_dir = state.claude_dir.clone();
        let cache = std::mem::take(&mut *state.file_cache.lock().unwrap());
        (claude_dir, cache)
    };
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let result = tokio::task::spawn_blocking(move || {
        let stats = local_stats::collect_local_stats(&claude_dir, &today, &mut cache);
        (stats, cache)
    })
    .await;
    if let Ok((stats, cache)) = result {
        let state = app.state::<AppState>();
        *state.file_cache.lock().unwrap() = cache;
        state.snapshot.lock().unwrap().local = Some(stats);
    }
    publish(app);
}

pub async fn refresh_spend(app: &AppHandle) {
    let Some(key) = api_spend::load_admin_key() else {
        app.state::<AppState>().snapshot.lock().unwrap().api_spend = None;
        publish(app);
        return;
    };
    let start = api_spend::month_start_utc();
    let end = chrono::Utc::now().to_rfc3339();
    match api_spend::fetch_month_to_date(api_spend::ADMIN_BASE, &key, &start, &end).await {
        Ok(usd) => {
            app.state::<AppState>().snapshot.lock().unwrap().api_spend = Some(ApiSpend {
                month_to_date_usd: usd,
                fetched_at: chrono::Utc::now().to_rfc3339(),
                stale: false,
            });
        }
        Err(_) => {
            let state = app.state::<AppState>();
            let mut snap = state.snapshot.lock().unwrap();
            if let Some(s) = snap.api_spend.as_mut() {
                s.stale = true;
            }
        }
    }
    publish(app);
}

/// Directory and config for a Kimi refresh tick; None (and the snapshot's
/// kimi cleared) when Kimi Code is not installed. Re-read every tick.
fn kimi_setup(app: &AppHandle) -> Option<(PathBuf, kimi::config::KimiConfig)> {
    let Some(dir) = kimi::kimi_dir() else {
        app.state::<AppState>().snapshot.lock().unwrap().kimi = None;
        publish(app);
        return None;
    };
    let Some(cfg) = kimi::config::read_config(&dir) else {
        // Config missing or mid-write: keep last-known data, mark it stale.
        if let Some(k) = app.state::<AppState>().snapshot.lock().unwrap().kimi.as_mut() {
            on_config_unreadable(k);
        }
        publish(app);
        return None;
    };
    Some((dir, cfg))
}

fn on_config_unreadable(k: &mut KimiSnapshot) {
    if let Some(l) = k.limits.as_mut() {
        l.stale = true;
    }
    if let Some(b) = k.balance.as_mut() {
        b.stale = true;
    }
}

/// Run `f` on the (get-or-created) Kimi snapshot; each refresher touches only its own fields.
fn with_kimi(app: &AppHandle, f: impl FnOnce(&mut KimiSnapshot)) {
    let state = app.state::<AppState>();
    let mut snap = state.snapshot.lock().unwrap();
    f(snap.kimi.get_or_insert_with(KimiSnapshot::default));
}

/// Stale Kimi percentages whose reset time has passed are certainly wrong
/// (the window rolled over); blank them so tray and popup show "unknown".
fn expire_stale_limits(snap: &mut UsageSnapshot, now: chrono::DateTime<chrono::Utc>) {
    let Some(l) = snap.kimi.as_mut().and_then(|k| k.limits.as_mut()) else { return };
    if !l.stale {
        return;
    }
    let passed = |r: &Option<String>| {
        r.as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .is_some_and(|t| t < now)
    };
    if passed(&l.five_hour_resets_at) {
        l.five_hour_pct = None;
    }
    if passed(&l.weekly_resets_at) {
        l.weekly_pct = None;
    }
}

/// The not-subscribed path: drop quota and forget the fetch throttle stamp so the
/// first tick after a subscription reappears fetches immediately.
fn on_not_subscribed(k: &mut KimiSnapshot, last_fetch: &mut Option<std::time::Instant>) {
    *last_fetch = None;
    k.has_subscription = false;
    k.limits = None;
    k.login_expired = false;
}

/// A token that is usable now: the first read if unexpired, else one re-read
/// (the CLI may have refreshed it), else None. Never refreshes anything itself.
fn usable_token(
    first: Option<kimi::config::KimiToken>,
    reread: impl FnOnce() -> Option<kimi::config::KimiToken>,
    now: i64,
) -> Option<kimi::config::KimiToken> {
    let first = first?;
    if first.expires_at >= now {
        return Some(first);
    }
    reread().filter(|t| t.expires_at >= now)
}

fn mark_kimi_failed(k: &mut KimiSnapshot, login_expired: bool) {
    if let Some(l) = k.limits.as_mut() {
        l.stale = true;
    }
    if login_expired {
        k.login_expired = true;
    }
}

pub async fn refresh_kimi_quota(app: &AppHandle) {
    let Some((dir, cfg)) = kimi_setup(app) else { return };
    if !kimi::config::has_subscription(&dir, &cfg) {
        {
            let state = app.state::<AppState>();
            let mut last = state.last_kimi_fetch.lock().unwrap();
            with_kimi(app, |k| on_not_subscribed(k, &mut last));
        }
        publish(app);
        return;
    }
    with_kimi(app, |k| k.has_subscription = true);
    {
        let state = app.state::<AppState>();
        let mut last = state.last_kimi_fetch.lock().unwrap();
        if last.map(|t| t.elapsed() < OWN_FETCH_EVERY).unwrap_or(false) {
            drop(last);
            publish(app);
            return;
        }
        *last = Some(std::time::Instant::now());
    }
    let (Some(base), Some(path)) = (
        cfg.managed_base_url.clone(),
        kimi::config::credentials_path(&dir, &cfg),
    ) else {
        with_kimi(app, |k| mark_kimi_failed(k, false));
        publish(app);
        return;
    };
    // Expired token: re-read once (the CLI may have refreshed it); still expired
    // means skip the network call, keep last-known data, flag the login.
    let now = chrono::Utc::now().timestamp();
    let Some(token) =
        usable_token(kimi::config::read_token(&path), || kimi::config::read_token(&path), now)
    else {
        with_kimi(app, |k| mark_kimi_failed(k, true));
        publish(app);
        return;
    };
    let mut result = kimi::quota::fetch_usages(&base, &token.access_token).await;
    if matches!(result, Err(kimi::quota::QuotaError::Unauthorized)) {
        // The CLI may have refreshed the token meanwhile; retry once if so.
        if let Some(t2) = kimi::config::read_token(&path) {
            if t2.access_token != token.access_token {
                result = kimi::quota::fetch_usages(&base, &t2.access_token).await;
            }
        }
    }
    with_kimi(app, |k| match result {
        Ok(l) => {
            k.limits = Some(l);
            k.login_expired = false;
        }
        Err(kimi::quota::QuotaError::Unauthorized) => mark_kimi_failed(k, true),
        Err(_) => mark_kimi_failed(k, false),
    });
    publish(app);
}

pub async fn refresh_kimi_local(app: &AppHandle) {
    let Some((dir, _cfg)) = kimi_setup(app) else { return };
    let mut cache = std::mem::take(&mut *app.state::<AppState>().kimi_file_cache.lock().unwrap());
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let result = tokio::task::spawn_blocking(move || {
        let local = kimi::local::collect_kimi_local(&dir, &today, &mut cache);
        (local, cache)
    })
    .await;
    if let Ok((local, cache)) = result {
        *app.state::<AppState>().kimi_file_cache.lock().unwrap() = cache;
        with_kimi(app, |k| k.local = Some(local));
    }
    publish(app);
}

pub async fn refresh_kimi_balance(app: &AppHandle) {
    let Some((_dir, cfg)) = kimi_setup(app) else { return };
    let Some((base, key)) = kimi::balance::resolve_key(kimi::balance::load_moonshot_key(), &cfg)
    else {
        with_kimi(app, |k| k.balance = None);
        publish(app);
        return;
    };
    let result = kimi::balance::fetch_balance(&base, &key).await;
    with_kimi(app, |k| match result {
        Ok(b) => k.balance = Some(b),
        Err(_) => {
            if let Some(b) = k.balance.as_mut() {
                b.stale = true;
            }
        }
    });
    publish(app);
}

pub async fn refresh_all(app: &AppHandle) {
    tokio::join!(
        refresh_limits(app),
        refresh_local(app),
        refresh_spend(app),
        refresh_kimi_quota(app),
        refresh_kimi_local(app),
        refresh_kimi_balance(app)
    );
}

pub fn start(app: AppHandle) {
    let a = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_limits(&a).await;
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });
    let a = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_local(&a).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
    let a = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_spend(&a).await;
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        }
    });
    let a = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_kimi_quota(&a).await;
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });
    let a = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_kimi_local(&a).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_kimi_balance(&app).await;
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{KimiLimits, KimiSnapshot, Limits};

    fn claude() -> Limits {
        Limits {
            session_pct: 22.0,
            session_resets_at: None,
            weekly_pct: 42.0,
            weekly_resets_at: None,
            fetched_at: "t".into(),
            stale: false,
        }
    }

    fn klimits(weekly: f32) -> KimiLimits {
        KimiLimits {
            five_hour_pct: Some(10.0),
            five_hour_resets_at: None,
            weekly_pct: Some(weekly),
            weekly_resets_at: None,
            fetched_at: "t".into(),
            stale: false,
        }
    }

    fn snap(kimi: Option<KimiSnapshot>) -> UsageSnapshot {
        UsageSnapshot { limits: Some(claude()), kimi, ..Default::default() }
    }

    #[test]
    fn render_uses_raw_pct_but_key_is_rounded() {
        let mut a = snap(None);
        a.limits.as_mut().unwrap().session_pct = 59.6;
        let mut b = a.clone();
        b.limits.as_mut().unwrap().session_pct = 59.7;
        let (ma, ta) = tray_render_key(&a);
        let (mb, tb) = tray_render_key(&b);
        assert_eq!(ma, TrayMode::Single(Some(59.6)));
        assert_eq!(ma.rounded(), mb.rounded());
        assert_eq!(ma.rounded(), TrayMode::Single(Some(60.0)));
        assert_eq!(ta, tb);
        let k = KimiSnapshot {
            has_subscription: true,
            limits: Some(klimits(84.6)),
            ..Default::default()
        };
        let (m, _) = tray_render_key(&snap(Some(k)));
        assert_eq!(m, TrayMode::Split(Some(42.0), Some(84.6)));
    }

    #[test]
    fn unreadable_config_keeps_data_and_marks_stale() {
        let mut k = KimiSnapshot {
            has_subscription: true,
            limits: Some(klimits(50.0)),
            balance: Some(crate::snapshot::KimiBalance {
                available: 1.0,
                currency: "USD".into(),
                fetched_at: "t".into(),
                stale: false,
            }),
            ..Default::default()
        };
        on_config_unreadable(&mut k);
        assert!(k.has_subscription);
        assert!(k.limits.as_ref().unwrap().stale);
        assert!(k.balance.as_ref().unwrap().stale);
        assert_eq!(k.limits.as_ref().unwrap().weekly_pct, Some(50.0));
    }

    #[test]
    fn no_kimi_keeps_session_ring() {
        let (m, t) = tray_render_key(&snap(None));
        assert_eq!(m, TrayMode::Single(Some(22.0)));
        assert_eq!(t, "Session 22% · Weekly 42%");
    }

    #[test]
    fn key_only_kimi_keeps_session_ring() {
        let k = KimiSnapshot { has_subscription: false, ..Default::default() };
        let (m, _) = tray_render_key(&snap(Some(k)));
        assert_eq!(m, TrayMode::Single(Some(22.0)));
    }

    #[test]
    fn subscription_uses_split_weekly() {
        let k = KimiSnapshot {
            has_subscription: true,
            limits: Some(klimits(78.0)),
            ..Default::default()
        };
        let (m, t) = tray_render_key(&snap(Some(k)));
        assert_eq!(m, TrayMode::Split(Some(42.0), Some(78.0)));
        assert_eq!(t, "Claude weekly 42% · Kimi weekly 78%");
    }

    #[test]
    fn subscription_without_quota_yet_is_split_with_gray_half() {
        let k = KimiSnapshot { has_subscription: true, ..Default::default() };
        let (m, t) = tray_render_key(&snap(Some(k)));
        assert_eq!(m, TrayMode::Split(Some(42.0), None));
        assert!(t.contains("Kimi weekly —"));
    }

    fn kl(pct5: Option<f32>, r5: Option<&str>, pctw: Option<f32>, rw: Option<&str>, stale: bool) -> KimiLimits {
        KimiLimits {
            five_hour_pct: pct5,
            five_hour_resets_at: r5.map(str::to_string),
            weekly_pct: pctw,
            weekly_resets_at: rw.map(str::to_string),
            fetched_at: "t".into(),
            stale,
        }
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
    }

    #[test]
    fn stale_pct_past_reset_is_blanked_in_snapshot_and_tray() {
        let k = KimiSnapshot {
            has_subscription: true,
            limits: Some(kl(
                Some(80.0),
                Some("2026-11-08T18:00:00Z"),
                Some(60.0),
                Some("2026-11-12T00:00:00Z"),
                true,
            )),
            ..Default::default()
        };
        let mut s = snap(Some(k));
        expire_stale_limits(&mut s, at("2026-11-09T00:00:00Z"));
        let l = s.kimi.as_ref().unwrap().limits.as_ref().unwrap();
        assert_eq!(l.five_hour_pct, None); // reset passed
        assert_eq!(l.weekly_pct, Some(60.0)); // still ahead
        let mut s2 = s.clone();
        expire_stale_limits(&mut s2, at("2026-11-13T00:00:00Z"));
        assert_eq!(s2.kimi.as_ref().unwrap().limits.as_ref().unwrap().weekly_pct, None);
        let (m, t) = tray_render_key(&s2);
        assert_eq!(m, TrayMode::Split(Some(42.0), None));
        assert!(t.contains("Kimi weekly —"));
    }

    #[test]
    fn fresh_pct_past_reset_is_kept() {
        let k = KimiSnapshot {
            has_subscription: true,
            limits: Some(kl(Some(80.0), Some("2026-11-08T18:00:00Z"), None, None, false)),
            ..Default::default()
        };
        let mut s = snap(Some(k));
        expire_stale_limits(&mut s, at("2026-11-09T00:00:00Z"));
        assert_eq!(s.kimi.unwrap().limits.unwrap().five_hour_pct, Some(80.0));
    }

    #[test]
    fn stale_pct_without_or_with_junk_reset_is_kept() {
        let k = KimiSnapshot {
            limits: Some(kl(Some(80.0), None, Some(60.0), Some("garbage"), true)),
            ..Default::default()
        };
        let mut s = snap(Some(k));
        expire_stale_limits(&mut s, at("2030-01-01T00:00:00Z"));
        let l = s.kimi.unwrap().limits.unwrap();
        assert_eq!((l.five_hour_pct, l.weekly_pct), (Some(80.0), Some(60.0)));
    }

    #[test]
    fn not_subscribed_resets_fetch_throttle() {
        let mut k = KimiSnapshot {
            has_subscription: true,
            login_expired: true,
            limits: Some(klimits(50.0)),
            ..Default::default()
        };
        let mut last = Some(std::time::Instant::now());
        on_not_subscribed(&mut k, &mut last);
        assert!(!k.has_subscription && !k.login_expired && k.limits.is_none());
        assert!(last.is_none());
    }

    fn tok(name: &str, exp: i64) -> Option<kimi::config::KimiToken> {
        Some(kimi::config::KimiToken { access_token: name.into(), expires_at: exp })
    }

    #[test]
    fn unexpired_token_is_used_without_reread() {
        let t = usable_token(tok("a", 2000), || panic!("no reread"), 1000);
        assert_eq!(t.unwrap().access_token, "a");
    }

    #[test]
    fn expired_token_rereads_once_and_uses_fresh() {
        let t = usable_token(tok("a", 900), || tok("b", 5000), 1000);
        assert_eq!(t.unwrap().access_token, "b");
    }

    #[test]
    fn still_expired_after_reread_is_none() {
        assert!(usable_token(tok("a", 900), || tok("a", 900), 1000).is_none());
        assert!(usable_token(tok("a", 900), || None, 1000).is_none());
        assert!(usable_token(None, || panic!("no reread"), 1000).is_none());
    }
}
