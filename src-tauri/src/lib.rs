pub mod api_spend;
mod http;
pub mod limits;
pub mod local_stats;
pub mod pricing;
pub mod scheduler;
pub mod snapshot;
pub mod tray_icon;

#[cfg(test)]
pub(crate) fn test_fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

use scheduler::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, MenuBuilder, MenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, PhysicalPosition};

/// Grace period for the cursor to travel from tray icon to popup before auto-hide.
const POPUP_HIDE_GRACE_MS: u64 = 350;

#[derive(Default)]
struct HoverState {
    pinned: AtomicBool,
    over_tray: AtomicBool,
    over_popup: AtomicBool,
}

/// Single owner of the "hide popup" action: clears hover/pin state and hides the window.
fn force_hide_popup(app: &AppHandle) {
    let hover = app.state::<Arc<HoverState>>();
    hover.over_popup.store(false, Ordering::Relaxed);
    hover.pinned.store(false, Ordering::Relaxed);
    if let Some(win) = app.get_webview_window("popup") {
        let _ = win.hide();
    }
}

fn show_popup(app: &AppHandle, tray_rect: Option<tauri::Rect>) {
    let Some(win) = app.get_webview_window("popup") else {
        return;
    };
    if let Some(rect) = tray_rect {
        let (rx, ry) = match rect.position {
            tauri::Position::Physical(p) => (p.x as f64, p.y as f64),
            tauri::Position::Logical(p) => (p.x, p.y),
        };
        let rw = match rect.size {
            tauri::Size::Physical(s) => s.width as f64,
            tauri::Size::Logical(s) => s.width,
        };
        if let Ok(size) = win.outer_size() {
            let x = (rx + rw / 2.0 - size.width as f64 / 2.0).max(8.0);
            let y = (ry - size.height as f64 - 8.0).max(8.0);
            let _ = win.set_position(PhysicalPosition::new(x, y));
        }
    }
    let _ = win.show();
}

fn maybe_hide_popup(app: &AppHandle) {
    let app = app.clone();
    // grace delay so the cursor can travel tray -> popup
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(POPUP_HIDE_GRACE_MS)).await;
        let hover = app.state::<Arc<HoverState>>();
        if !hover.pinned.load(Ordering::Relaxed)
            && !hover.over_tray.load(Ordering::Relaxed)
            && !hover.over_popup.load(Ordering::Relaxed)
        {
            // pinned was verified false above, so force_hide clearing it is a no-op
            force_hide_popup(&app);
        }
    });
}

// ---------- commands (frontend -> backend) ----------

#[tauri::command]
fn get_snapshot(state: tauri::State<AppState>) -> snapshot::UsageSnapshot {
    state.snapshot.lock().unwrap().clone()
}

#[tauri::command]
fn dismiss_popup(app: AppHandle, hover: tauri::State<Arc<HoverState>>) {
    hover.pinned.store(false, Ordering::Relaxed);
    force_hide_popup(&app);
}

#[tauri::command]
fn popup_hover(app: AppHandle, hover: tauri::State<Arc<HoverState>>, inside: bool) {
    hover.over_popup.store(inside, Ordering::Relaxed);
    if !inside {
        maybe_hide_popup(&app);
    }
}

#[tauri::command]
fn set_pinned(app: AppHandle, hover: tauri::State<Arc<HoverState>>, pinned: bool) {
    hover.pinned.store(pinned, Ordering::Relaxed);
    if !pinned {
        maybe_hide_popup(&app);
    }
}

#[tauri::command]
fn has_admin_key() -> bool {
    api_spend::load_admin_key().is_some()
}

#[tauri::command]
async fn set_admin_key(app: AppHandle, key: String) -> Result<(), String> {
    if key.trim().is_empty() {
        api_spend::clear_admin_key()?;
    } else {
        api_spend::save_admin_key(key.trim())?;
    }
    scheduler::refresh_spend(&app).await;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(AppState::new())
        .manage(Arc::new(HoverState::default()))
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            dismiss_popup,
            popup_hover,
            set_pinned,
            has_admin_key,
            set_admin_key
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // macOS: live in the menu bar only — no Dock icon, no app menu.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // right-click menu
            let refresh = MenuItem::with_id(app, "refresh", "Refresh now", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
            // Query authoritative autostart state so the checkbox reflects reality on first open.
            let initial_autostart = {
                use tauri_plugin_autostart::ManagerExt;
                app.autolaunch().is_enabled().unwrap_or(false)
            };
            let autostart = CheckMenuItem::with_id(
                app,
                "autostart",
                "Start at login",
                true,
                initial_autostart,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = MenuBuilder::new(app)
                .item(&refresh)
                .item(&settings)
                .item(&autostart)
                .separator()
                .item(&quit)
                .build()?;

            let (rgba, w, h) = tray_icon::render_ring(None);
            TrayIconBuilder::with_id("main")
                .icon(tauri::image::Image::new_owned(rgba, w, h))
                .tooltip("Claude Usage Meter")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event({
                    let autostart_item = autostart.clone();
                    move |app, event| match event.id.as_ref() {
                        "refresh" => {
                            let app = app.clone();
                            tauri::async_runtime::spawn(async move {
                                scheduler::refresh_all(&app).await;
                            });
                        }
                        "settings" => {
                            if let Some(win) = app.get_webview_window("settings") {
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }
                        "autostart" => {
                            use tauri_plugin_autostart::ManagerExt;
                            let mgr = app.autolaunch();
                            if mgr.is_enabled().unwrap_or(false) {
                                let _ = mgr.disable();
                            } else {
                                let _ = mgr.enable();
                            }
                            // Sync checkbox to authoritative state (click may auto-toggle visually).
                            let new_state = mgr.is_enabled().unwrap_or(false);
                            let _ = autostart_item.set_checked(new_state);
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    let app = tray.app_handle();
                    let hover = app.state::<Arc<HoverState>>();
                    match event {
                        TrayIconEvent::Enter { rect, .. } => {
                            hover.over_tray.store(true, Ordering::Relaxed);
                            show_popup(app, Some(rect));
                        }
                        TrayIconEvent::Leave { .. } => {
                            hover.over_tray.store(false, Ordering::Relaxed);
                            maybe_hide_popup(app);
                        }
                        TrayIconEvent::Click {
                            button: tauri::tray::MouseButton::Left,
                            button_state: tauri::tray::MouseButtonState::Up,
                            rect,
                            ..
                        } => {
                            let pinned = !hover.pinned.load(Ordering::Relaxed);
                            hover.pinned.store(pinned, Ordering::Relaxed);
                            if pinned {
                                show_popup(app, Some(rect));
                                if let Some(win) = app.get_webview_window("popup") {
                                    let _ = win.set_focus();
                                }
                            } else {
                                force_hide_popup(app);
                            }
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            // hide settings window on close instead of exiting
            if let Some(settings_win) = app.get_webview_window("settings") {
                let w = settings_win.clone();
                settings_win.on_window_event(move |e| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }
            // popup loses focus while pinned -> unpin and hide
            if let Some(popup) = app.get_webview_window("popup") {
                let h = handle.clone();
                popup.on_window_event(move |e| {
                    if let tauri::WindowEvent::Focused(false) = e {
                        let hover = h.state::<Arc<HoverState>>();
                        // On Windows, tray mousedown steals focus before the Click{Up} event
                        // fires, so skip the unpin here and let the tray click handler do it.
                        if hover.pinned.load(Ordering::Relaxed)
                            && !hover.over_tray.load(Ordering::Relaxed)
                        {
                            force_hide_popup(&h);
                        }
                    }
                });
            }

            scheduler::start(handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running claude-usage-meter");
}
