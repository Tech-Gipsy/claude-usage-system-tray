use crate::snapshot::{ApiSpend, UsageSnapshot};
use crate::{api_spend, limits, local_stats, tray_icon};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

pub struct AppState {
    pub snapshot: Mutex<UsageSnapshot>,
    pub file_cache: Mutex<local_stats::FileCache>,
    pub claude_dir: PathBuf,
    /// Last tray render: (rounded session pct, tooltip). -1 = nothing rendered yet.
    pub last_tray: Mutex<(i8, String)>,
}

impl AppState {
    pub fn new() -> Self {
        let claude_dir = dirs::home_dir().unwrap_or_default().join(".claude");
        Self {
            snapshot: Mutex::new(UsageSnapshot::default()),
            file_cache: Mutex::new(local_stats::FileCache::default()),
            claude_dir,
            last_tray: Mutex::new((-1, String::new())),
        }
    }
}

fn publish(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snap = state.snapshot.lock().unwrap().clone();

    // tray icon + tooltip — only re-render when the rounded pct or tooltip changed
    let pct = snap.limits.as_ref().map(|l| l.session_pct);
    let pct_key: i8 = pct.map(|p| p.round() as i8).unwrap_or(-1);
    let tooltip = match &snap.limits {
        Some(l) => format!("Session {:.0}% · Weekly {:.0}%", l.session_pct, l.weekly_pct),
        None => "Claude Usage Meter".to_string(),
    };
    {
        let mut last = state.last_tray.lock().unwrap();
        if (pct_key, &tooltip) != (last.0, &last.1) {
            let (rgba, w, h) = tray_icon::render_ring(pct);
            if let Some(tray) = app.tray_by_id("main") {
                let _ = tray.set_icon(Some(tauri::image::Image::new_owned(rgba, w, h)));
                let _ = tray.set_tooltip(Some(tooltip.clone()));
            }
            *last = (pct_key, tooltip);
        }
    }

    let _ = app.emit("snapshot", &snap);
}

pub async fn refresh_limits(app: &AppHandle) {
    let creds_path = app.state::<AppState>().claude_dir.join(".credentials.json");
    let result = limits::get_limits(limits::USAGE_BASE, limits::TOKEN_BASE, &creds_path).await;
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

pub async fn refresh_all(app: &AppHandle) {
    tokio::join!(refresh_limits(app), refresh_local(app), refresh_spend(app));
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
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_spend(&app).await;
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        }
    });
}
