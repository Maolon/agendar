//! Tauri IPC wrappers.
//!
//! Every wrapper validates the native caller first (window label + bundled
//! origin), then runs the blocking backend call off the GUI thread. No caller
//! identity, role, writer or host is ever read from JSON.

use std::sync::Arc;

use serde_json::Value;
use tauri::{State, WebviewWindow};

use agendar_core::error::ErrorCode;

use crate::error::{self, ApiResult, AppError};
use crate::host::Host;

/// Managed application state.
#[derive(Clone)]
pub struct HostHandle {
    pub host: Arc<Host>,
}

/// The frozen command matrix.
pub const MAIN_ONLY: &[&str] = &[
    "get_project",
    "search",
    "review_delivery",
    "resolve_rescue",
    "update_project",
    "update_feature",
    "set_policy",
    "set_rule",
    "read_report",
    "approve_check",
    "run_check",
    "set_preferences",
    "configure_vault",
    "get_setup_locations",
    "get_local_config",
    "test_notice",
    "open_report",
    "get_bindings",
    "set_binding",
    "install_local",
    "link_repo",
    "unlink_repo",
    "open_preview",
];

fn caller(window: &WebviewWindow, command: &str) -> ApiResult<String> {
    let label = window.label();
    if label != "main" && label != "popover" {
        return Err(AppError::forbidden("Unknown application webview."));
    }
    if label == "popover" && MAIN_ONLY.contains(&command) {
        return Err(AppError::forbidden(
            "The popover may only read the snapshot, prepare/answer an easy decision and manage windows.",
        ));
    }
    let url = window
        .url()
        .map_err(|error| AppError::forbidden(format!("Cannot read the webview URL: {error}.")))?;
    let allowed_origin = if cfg!(debug_assertions) {
        url.scheme() == "http" && url.host_str() == Some("localhost") && url.port() == Some(1420)
    } else {
        (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
            || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost"))
    };
    if !allowed_origin {
        return Err(AppError::forbidden("Unexpected webview origin."));
    }
    let path = url.path();
    if !(path.is_empty()
        || path == "/"
        || path.ends_with("/index.html")
        || path.ends_with("index.html"))
    {
        return Err(AppError::forbidden("Unexpected webview entry path."));
    }
    let popover_query = url
        .query_pairs()
        .any(|(key, value)| key == "surface" && value == "popover");
    if label == "popover" && !popover_query {
        return Err(AppError::forbidden(
            "The popover entry is missing its surface.",
        ));
    }
    if label == "main" && popover_query {
        return Err(AppError::forbidden("The main entry must not be a popover."));
    }
    Ok(label.to_owned())
}

async fn blocking<F>(call: F) -> Result<Value, ()>
where
    F: FnOnce() -> ApiResult<Value> + Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(call).await {
        Ok(result) => Ok(error::envelope(result)),
        Err(join_error) => Ok(error::err(AppError::io(format!(
            "The backend task did not complete: {join_error}."
        )))),
    }
}

pub fn unsupported_capability(command: &str) -> AppError {
    AppError::new(
        ErrorCode::Unsupported.as_str(),
        format!("Capability {command} is unavailable."),
    )
    .with_hint("This capability arrives in a later version.")
}

fn unsupported(command: &str) -> Value {
    error::err(unsupported_capability(command))
}

#[tauri::command]
pub async fn get_snapshot(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    _request: Value,
) -> Result<Value, ()> {
    match caller(&window, "get_snapshot") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.get_snapshot()).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn get_project(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "get_project") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.get_project(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn search(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "search") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.search(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn prepare_answer(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "prepare_answer") {
        Ok(surface) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.prepare_answer(&surface, &request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn answer_decision(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "answer_decision") {
        Ok(surface) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.answer_decision(&surface, &request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn set_preferences(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "set_preferences") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.set_preferences(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn snooze_need(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "snooze_need") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.snooze_need(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn dismiss_need(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "dismiss_need") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.dismiss_need(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn resolve_rescue(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "resolve_rescue") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.resolve_rescue(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn link_repo(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "link_repo") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.link_repo(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn configure_vault(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "configure_vault") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.configure_vault(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn unlink_repo(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "unlink_repo") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.unlink_repo(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn get_local_config(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    _request: Value,
) -> Result<Value, ()> {
    match caller(&window, "get_local_config") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.get_local_config()).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn open_report(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "open_report") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.open_report(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn get_bindings(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    _request: Value,
) -> Result<Value, ()> {
    match caller(&window, "get_bindings") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.bindings()).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn set_binding(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "set_binding") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.set_binding(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn test_notice(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    _request: Value,
) -> Result<Value, ()> {
    match caller(&window, "test_notice") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.test_notice()).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn get_setup_locations(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    _request: Value,
) -> Result<Value, ()> {
    match caller(&window, "get_setup_locations") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.get_setup_locations()).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn install_local(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "install_local") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.install_local(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn show_main(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "show_main") {
        Ok(_) => {
            // Optional target: the tray opens the main window on one item.
            let project = request
                .get("project")
                .and_then(Value::as_str)
                .and_then(|slug| agendar_core::model::ProjectSlug::parse(slug).ok());
            let item = request
                .get("item_id")
                .and_then(Value::as_str)
                .filter(|item| !item.is_empty() && item.len() <= 200);
            match (project, item) {
                (Some(project), Some(item)) => state.host.show_main_on(&project, item),
                _ => state.host.show_main(),
            }
            Ok(error::ok(serde_json::json!({})))
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn hide_popover(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    _request: Value,
) -> Result<Value, ()> {
    match caller(&window, "hide_popover") {
        Ok(_) => {
            state.host.hide_popover();
            Ok(error::ok(serde_json::json!({})))
        }
        Err(error) => Ok(error::err(error)),
    }
}

/// How the page should paint its chrome on this machine: whether the native
/// title bar is overlaid (macOS) and whether glass shows through.
#[tauri::command]
pub async fn window_chrome(window: WebviewWindow, _request: Value) -> Result<Value, ()> {
    match caller(&window, "window_chrome") {
        Ok(_) => Ok(error::ok(serde_json::json!({
            "overlay_titlebar": cfg!(target_os = "macos") && window.label() == "main",
            "material": agendar_chrome::material(),
        }))),
        Err(error) => Ok(error::err(error)),
    }
}

/// Place the native glass behind the page's transparent chrome regions.
#[tauri::command]
pub async fn set_glass_regions(window: WebviewWindow, request: Value) -> Result<Value, ()> {
    if let Err(error) = caller(&window, "set_glass_regions") {
        return Ok(error::err(error));
    }
    let regions: Vec<agendar_chrome::Region> =
        match request.get("regions").cloned().map(serde_json::from_value) {
            Some(Ok(regions)) => regions,
            Some(Err(error)) => {
                return Ok(error::err(AppError::validation(
                    "/regions",
                    format!("Invalid glass regions: {error}."),
                )));
            }
            None => {
                return Ok(error::err(AppError::validation(
                    "/regions",
                    "regions is required.",
                )));
            }
        };
    let tint = match request.get("tint").cloned().map(serde_json::from_value) {
        Some(Ok(tint)) => Some(tint),
        Some(Err(error)) => {
            return Ok(error::err(AppError::validation(
                "/tint",
                format!("Invalid glass tint: {error}."),
            )));
        }
        None => None,
    };
    match agendar_chrome::set_regions(&window, regions, tint) {
        Ok(()) => Ok(error::ok(serde_json::json!({}))),
        Err(error) => Ok(error::err(AppError::new(
            ErrorCode::Io.as_str(),
            format!("Cannot place the window glass: {error}."),
        ))),
    }
}

macro_rules! unsupported_command {
    ($name:ident) => {
        #[tauri::command]
        pub async fn $name(
            window: WebviewWindow,
            _state: State<'_, HostHandle>,
            _request: Value,
        ) -> Result<Value, ()> {
            match caller(&window, stringify!($name)) {
                Ok(_) => Ok(unsupported(stringify!($name))),
                Err(error) => Ok(error::err(error)),
            }
        }
    };
}

#[tauri::command]
pub async fn set_policy(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "set_policy") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.set_policy(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn update_project(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "update_project") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.update_project(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn update_feature(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "update_feature") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.update_feature(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

unsupported_command!(open_preview);

#[tauri::command]
pub async fn review_delivery(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "review_delivery") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.review_delivery(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn set_rule(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "set_rule") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.set_rule(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn read_report(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "read_report") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.read_report(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn approve_check(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "approve_check") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.approve_check(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

#[tauri::command]
pub async fn run_check(
    window: WebviewWindow,
    state: State<'_, HostHandle>,
    request: Value,
) -> Result<Value, ()> {
    match caller(&window, "run_check") {
        Ok(_) => {
            let host = Arc::clone(&state.host);
            blocking(move || host.run_check(&request)).await
        }
        Err(error) => Ok(error::err(error)),
    }
}

/// Every registered command, used by the build test.
pub const COMMANDS: &[&str] = &[
    "get_snapshot",
    "get_project",
    "search",
    "prepare_answer",
    "answer_decision",
    "review_delivery",
    "resolve_rescue",
    "update_project",
    "update_feature",
    "set_policy",
    "set_rule",
    "read_report",
    "approve_check",
    "run_check",
    "set_preferences",
    "snooze_need",
    "dismiss_need",
    "configure_vault",
    "get_setup_locations",
    "get_local_config",
    "test_notice",
    "open_report",
    "get_bindings",
    "set_binding",
    "install_local",
    "link_repo",
    "unlink_repo",
    "show_main",
    "hide_popover",
    "open_preview",
    "window_chrome",
    "set_glass_regions",
];
