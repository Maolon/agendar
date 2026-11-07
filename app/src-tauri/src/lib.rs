#![forbid(unsafe_code)]
// The IPC error is a small owned payload (code/message/hint); boxing it in every
// `Result` would add an allocation and obscure the wire shape. The lint only
// measures the enum size, not a correctness or ownership issue.
#![allow(clippy::result_large_err)]

//! Agendar desktop shell (Tauri 2).
//!
//! The shell owns windows, the tray, native notification dispatch and the local
//! watcher/steward loops. Every Vault mutation goes through `agendar-core`'s
//! `Service` with this host's operator writer; the IPC wrappers never accept a
//! writer, host or caller role from JavaScript.

pub mod commands;
pub mod error;
pub mod guard;
pub mod host;
pub mod native;
pub mod notifications;
pub mod preferences;
pub mod projection;
pub mod setup_locations;
pub mod tray;
pub mod ui;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use tauri::menu::{MenuBuilder, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_positioner::{Position, WindowExt};

use agendar_core::runtime::LocalPaths;

use crate::host::{Host, Origin, SystemWallClock};
use crate::native::{NativeSink, TauriUi, action_of};

/// Launch the native app.
pub fn run() {
    let application = tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .register_uri_scheme_protocol("agendar-media", media_response)
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::get_project,
            commands::search,
            commands::prepare_answer,
            commands::answer_decision,
            commands::review_delivery,
            commands::resolve_rescue,
            commands::update_project,
            commands::update_feature,
            commands::set_policy,
            commands::set_rule,
            commands::read_report,
            commands::approve_check,
            commands::run_check,
            commands::set_preferences,
            commands::snooze_need,
            commands::dismiss_need,
            commands::configure_vault,
            commands::get_setup_locations,
            commands::get_local_config,
            commands::test_notice,
            commands::open_report,
            commands::get_bindings,
            commands::set_binding,
            commands::unlink_repo,
            commands::install_local,
            commands::link_repo,
            commands::show_main,
            commands::hide_popover,
            commands::window_chrome,
            commands::set_glass_regions,
            commands::open_preview,
        ])
        .setup(|app| {
            let data_root = resolve_data_root()?;
            let (sender, receiver) = mpsc::channel();
            let native_sink = Arc::new(NativeSink::new(app.handle().clone(), sender));
            let ui = Arc::new(TauriUi::new(app.handle().clone(), Arc::clone(&native_sink)));
            let sink: Arc<dyn crate::notifications::NoticeSink> = native_sink;
            let host = Arc::new(
                Host::new(&data_root, Arc::new(SystemWallClock), ui, Arc::clone(&sink)).map_err(
                    |error| {
                        std::io::Error::other(format!(
                            "Cannot open the Agendar data directory: {}",
                            error.message
                        ))
                    },
                )?,
            );
            app.manage(commands::HostHandle {
                host: Arc::clone(&host),
            });
            install_tray(app)?;
            #[cfg(target_os = "macos")]
            {
                let motion_host = Arc::clone(&host);
                tray::spawn_animator(app.handle().clone(), move || motion_host.motion());
            }
            spawn_watcher(Arc::clone(&host));
            spawn_steward(Arc::clone(&host));
            spawn_notifications(Arc::clone(&host), receiver);
            // One early refresh after the notification backend has had a chance
            // to start on the main thread; it also paints the first real state
            // even before the frontend subscribes.
            let early = Arc::clone(&host);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(1));
                early.watch_tick("time");
            });
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                // Closing the main window keeps the process in the menu bar
                // only: on macOS it also leaves the Dock and the app switcher.
                api.prevent_close();
                hide_main(window);
            }
            WindowEvent::Focused(false) if window.label() == "popover" => {
                let _ = window.hide();
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("cannot build the Agendar application");

    application.run(|app_handle, event| match event {
        tauri::RunEvent::Exit => {
            if let Some(state) = app_handle.try_state::<commands::HostHandle>() {
                state.host.cancel();
            }
        }
        // Launching Agendar again while it lives in the menu bar opens the window.
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => show_main(app_handle),
        _ => {}
    });
}

/// The verified `agendar-media` protocol. Only an opaque token path is accepted;
/// bytes come from a Vault blob referenced by a delivery or observation.
fn media_response(
    ctx: tauri::UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    let not_found = || {
        tauri::http::Response::builder()
            .status(tauri::http::StatusCode::NOT_FOUND)
            .header("Cache-Control", "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .body(Vec::new())
            .unwrap_or_else(|_| tauri::http::Response::new(Vec::new()))
    };
    if !matches!(
        request.method(),
        &tauri::http::Method::GET | &tauri::http::Method::HEAD
    ) || request.uri().query().is_some()
    {
        return not_found();
    }
    let Some(state) = ctx.app_handle().try_state::<commands::HostHandle>() else {
        return not_found();
    };
    let Ok((mime, bytes)) = state.host.media_response(request.uri().path()) else {
        return not_found();
    };
    tauri::http::Response::builder()
        .status(tauri::http::StatusCode::OK)
        .header("Content-Type", mime)
        .header("Content-Length", bytes.len().to_string())
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        // An SVG icon opened as a document still cannot run script.
        .header(
            "Content-Security-Policy",
            "default-src 'none'; style-src 'unsafe-inline'; sandbox",
        )
        .body(bytes)
        .unwrap_or_else(|_| not_found())
}

fn resolve_data_root() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    if let Some(value) = std::env::var_os("AGENDAR_DATA_DIR") {
        return Ok(std::path::PathBuf::from(value));
    }
    LocalPaths::default_data_root().map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Cannot resolve the Agendar data directory: {}",
                error.message()
            ),
        )
        .into()
    })
}

fn install_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let open = MenuItem::with_id(app, "open", "Open Agendar", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = MenuBuilder::new(app).item(&open).item(&quit).build()?;
    // Start idle; the host switches the glyph once it has read the Vault.
    let mut builder = TrayIconBuilder::with_id(tray::TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip(tray::tooltip(0, 0));
    if let Some(icon) = tray::icon(0).or_else(|| app.default_window_icon().cloned()) {
        builder = builder.icon(icon).icon_as_template(tray::TEMPLATE);
    }
    let tray = builder
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => {
                if let Some(state) = app.try_state::<commands::HostHandle>() {
                    state.host.cancel();
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            let app = tray.app_handle();
            // Every event reaches the positioner before dispatch.
            tauri_plugin_positioner::on_tray_event(app, &event);
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_popover(app);
            }
        })
        .build(app)?;
    // The tray icon is removed when its handle drops, so keep it for the whole
    // process lifetime.
    app.manage(tray);
    Ok(())
}

/// Show and focus the main window. On macOS the app returns to the Dock first.
/// Call on the main thread.
pub(crate) fn show_main(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Hide the main window. On macOS the app then lives in the menu bar only: no
/// Dock icon and no app-switcher entry until the window opens again.
fn hide_main(window: &tauri::Window) {
    let _ = window.hide();
    #[cfg(target_os = "macos")]
    let _ = window
        .app_handle()
        .set_activation_policy(tauri::ActivationPolicy::Accessory);
}

fn toggle_popover(app: &tauri::AppHandle) {
    let Some(popover) = app.get_webview_window("popover") else {
        return;
    };
    if popover.is_visible().unwrap_or(false) {
        let _ = popover.hide();
        return;
    }
    if popover.move_window(Position::TrayCenter).is_err() {
        // Missing tray geometry on some Linux desktops: fall back to the main
        // window instead of opening unreachable controls.
        show_main(app);
        return;
    }
    clamp_to_work_area(&popover);
    let _ = popover.show();
    let _ = popover.set_focus();
}

fn clamp_to_work_area(window: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let area = monitor.work_area();
    let Ok(position) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let min_x = area.position.x;
    let min_y = area.position.y;
    let max_x = (area.position.x + area.size.width as i32 - size.width as i32).max(min_x);
    let max_y = (area.position.y + area.size.height as i32 - size.height as i32).max(min_y);
    let clamped_x = position.x.clamp(min_x, max_x);
    let clamped_y = position.y.clamp(min_y, max_y);
    if clamped_x != position.x || clamped_y != position.y {
        let _ = window.set_position(tauri::PhysicalPosition::new(clamped_x, clamped_y));
    }
}

fn spawn_watcher(host: Arc<Host>) {
    std::thread::spawn(move || {
        let mut current: Option<(std::path::PathBuf, agendar_core::runtime::VaultWatch)> = None;
        let mut next_reconcile = std::time::Instant::now() + Duration::from_secs(60);
        while !host.is_shutdown() {
            let Some(root) = host.current_vault_root() else {
                current = None;
                std::thread::sleep(Duration::from_secs(1));
                continue;
            };
            if current.as_ref().map(|(path, _)| path) != Some(&root) {
                current = host.start_watch(&root).map(|watch| (root.clone(), watch));
            }
            let Some((_, watch)) = current.as_ref() else {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            };
            // Short wakes let a Vault switch replace the watch without waiting
            // for the 60 s reconciliation interval.
            match watch.wait(Duration::from_secs(5)) {
                agendar_core::runtime::WaitOutcome::Changed => {
                    host.watch_tick("sync");
                    next_reconcile = std::time::Instant::now() + Duration::from_secs(60);
                }
                agendar_core::runtime::WaitOutcome::TimedOut => {
                    if std::time::Instant::now() >= next_reconcile {
                        host.watch_tick("time");
                        next_reconcile = std::time::Instant::now() + Duration::from_secs(60);
                    }
                }
                agendar_core::runtime::WaitOutcome::Closed => current = None,
            }
        }
    });
}

fn spawn_steward(host: Arc<Host>) {
    std::thread::spawn(move || {
        while !host.is_shutdown() {
            host.steward_tick();
            std::thread::sleep(Duration::from_secs(30));
        }
    });
}

fn spawn_notifications(host: Arc<Host>, receiver: mpsc::Receiver<agendar_notify::NoticeEvent>) {
    std::thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            if host.is_shutdown() {
                break;
            }
            host.handle_notice(event.id.as_str(), action_of(event.action), Origin::Live);
        }
    });
}

/// Emit the health event used by the shell when the local service degrades.
pub fn emit_health(app: &tauri::AppHandle, health: &str, diagnostics: serde_json::Value) {
    let _ = app.emit(
        "agendar://health",
        serde_json::json!({"v": 1, "health": health, "diagnostics": diagnostics}),
    );
}
