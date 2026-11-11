//! Tauri-specific UI and notification implementations.
//!
//! All window/event work is scheduled on the native main thread with
//! `AppHandle::run_on_main_thread`; OS callback threads and the watcher never
//! touch the GUI directly. The native notification FFI itself stays inside
//! `agendar-notify`.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use agendar_notify::{
    Capability, Notice, NoticeAction as NativeAction, NoticeEvent, NoticeId, Notifier,
};

use crate::notifications::{Availability, NoticeSink, NoticeSpec};
use crate::ui::Ui;

/// Application identifier used by the notification centre identity.
pub const APP_ID: &str = "dev.agendar.app";

struct SinkInner {
    notifier: Option<Notifier>,
    error: Option<String>,
}

/// The native notification sink. `show` and `withdraw` are fire-and-forget on
/// the main thread; failures are reflected through `capability`/`reason` so the
/// registry retries instead of pretending a banner was delivered.
pub struct NativeSink {
    app: AppHandle,
    inner: Arc<Mutex<SinkInner>>,
    sender: Sender<NoticeEvent>,
}

impl NativeSink {
    pub fn new(app: AppHandle, sender: Sender<NoticeEvent>) -> Self {
        Self {
            app,
            inner: Arc::new(Mutex::new(SinkInner {
                notifier: None,
                error: None,
            })),
            sender,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SinkInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl NoticeSink for NativeSink {
    fn show(&self, notice: &NoticeSpec) -> Result<(), String> {
        let id = NoticeId::parse(&notice.id).map_err(|error| error.to_string())?;
        let built = Notice::new(id, notice.title.clone(), notice.body.clone())
            .and_then(|built| built.with_choices(notice.choices.iter().cloned()))
            .map_err(|error| error.to_string())?;
        if self.lock().notifier.is_none() {
            return Err("notification backend is not started".to_owned());
        }
        let post = {
            let inner = Arc::clone(&self.inner);
            move || {
                let mut state = inner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(notifier) = state.notifier.as_ref()
                    && let Err(error) = notifier.show(built)
                {
                    state.error = Some(error.to_string());
                }
            }
        };
        if notice.choices.is_empty() {
            return self
                .app
                .run_on_main_thread(post)
                .map_err(|error| error.to_string());
        }
        // macOS registers a notice's button category asynchronously: register
        // it now and post a moment later, or the buttons may be missing.
        let prepared = {
            let inner = Arc::clone(&self.inner);
            let notice = Notice::new(
                NoticeId::parse(&notice.id).map_err(|error| error.to_string())?,
                notice.title.clone(),
                notice.body.clone(),
            )
            .and_then(|built| built.with_choices(notice.choices.iter().cloned()))
            .map_err(|error| error.to_string())?;
            move || {
                let state = inner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(notifier) = state.notifier.as_ref() {
                    let _ = notifier.prepare(&notice);
                }
            }
        };
        self.app
            .run_on_main_thread(prepared)
            .map_err(|error| error.to_string())?;
        let app = self.app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(CATEGORY_SETTLE_MS));
            let _ = app.run_on_main_thread(post);
        });
        Ok(())
    }

    fn withdraw(&self, id: &str) -> Result<(), String> {
        let id = NoticeId::parse(id).map_err(|error| error.to_string())?;
        let inner = Arc::clone(&self.inner);
        self.app
            .run_on_main_thread(move || {
                let state = inner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(notifier) = state.notifier.as_ref() {
                    let _ = notifier.withdraw(&id);
                }
            })
            .map_err(|error| error.to_string())
    }

    fn capability(&self) -> Availability {
        let state = self.lock();
        let Some(notifier) = state.notifier.as_ref() else {
            return if state.error.is_some() {
                Availability::Unavailable
            } else {
                Availability::Unknown
            };
        };
        match notifier.capability() {
            Capability::Available { .. } => Availability::Available,
            Capability::Unavailable(_) => Availability::Unavailable,
        }
    }

    fn last_error(&self) -> Option<String> {
        self.lock().notifier.as_ref().and_then(Notifier::last_error)
    }

    fn refresh(&self) {
        let inner = Arc::clone(&self.inner);
        let _ = self.app.run_on_main_thread(move || {
            let state = inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(notifier) = state.notifier.as_ref() {
                notifier.refresh();
            }
        });
    }

    fn reason(&self) -> Option<String> {
        let state = self.lock();
        if let Some(error) = &state.error {
            return Some(error.clone());
        }
        match state.notifier.as_ref().map(Notifier::capability) {
            Some(Capability::Unavailable(reason)) => Some(reason),
            _ => None,
        }
    }

    fn start(&self) {
        let inner = Arc::clone(&self.inner);
        let sender = self.sender.clone();
        let _ = self.app.run_on_main_thread(move || {
            let mut state = inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.notifier.is_some() {
                return;
            }
            match Notifier::start(APP_ID) {
                Ok((notifier, receiver)) => {
                    state.notifier = Some(notifier);
                    state.error = None;
                    forward(receiver, sender);
                }
                Err(error) => state.error = Some(error.to_string()),
            }
        });
    }

    fn shutdown(&self) {
        let inner = Arc::clone(&self.inner);
        let _ = self.app.run_on_main_thread(move || {
            let mut state = inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(notifier) = state.notifier.take() {
                notifier.shutdown();
            }
        });
    }
}

fn forward(receiver: Receiver<NoticeEvent>, sender: Sender<NoticeEvent>) {
    std::thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            if sender.send(event).is_err() {
                break;
            }
        }
    });
}

/// Map the notification crate's action into the app's routing decision.
pub fn action_of(action: NativeAction) -> crate::host::NoticeAction {
    match action {
        NativeAction::Open => crate::host::NoticeAction::Open,
        NativeAction::Choose(index) => crate::host::NoticeAction::Choose(usize::from(index)),
    }
}

/// How long a new button category gets to register before its notice posts.
const CATEGORY_SETTLE_MS: u64 = 400;

/// Real UI effects for the Tauri windows.
pub struct TauriUi {
    app: AppHandle,
    sink: Arc<NativeSink>,
    /// Last attention count shown in the tray, so unchanged publishes are free.
    attention: Mutex<Option<(usize, usize, usize, crate::preferences::Lang)>>,
}

impl TauriUi {
    pub fn new(app: AppHandle, sink: Arc<NativeSink>) -> Self {
        Self {
            app,
            sink,
            attention: Mutex::new(None),
        }
    }
}

impl Ui for TauriUi {
    fn show_main(&self) {
        let app = self.app.clone();
        let _ = self.app.run_on_main_thread(move || crate::show_main(&app));
    }

    fn hide_popover(&self) {
        let app = self.app.clone();
        let _ = self.app.run_on_main_thread(move || {
            if let Some(window) = app.get_webview_window("popover") {
                let _ = window.hide();
            }
        });
    }

    fn focus(&self, focus: &Value) {
        let payload = focus.clone();
        let app = self.app.clone();
        let _ = self.app.run_on_main_thread(move || {
            if let Some(popover) = app.get_webview_window("popover") {
                let _ = popover.hide();
            }
            crate::show_main(&app);
            let _ = app.emit_to("main", "agendar://notification-focus", payload);
        });
    }

    fn open_settings(&self, tab: &str) {
        let payload = serde_json::json!({ "view": "settings", "tab": tab });
        let app = self.app.clone();
        let _ = self.app.run_on_main_thread(move || {
            if let Some(popover) = app.get_webview_window("popover") {
                let _ = popover.hide();
            }
            crate::show_main(&app);
            let _ = app.emit_to("main", "agendar://navigate", payload);
        });
    }

    fn emit_changed(&self, event: &Value) {
        let _ = self.app.emit("agendar://changed", event);
    }

    fn emit_unblock(&self, event: &Value) {
        let _ = self.app.emit("agendar://unblock", event);
    }

    fn emit_health(&self, event: &Value) {
        let _ = self.app.emit("agendar://health", event);
    }

    fn emit_update(&self, event: &Value) {
        let _ = self.app.emit("agendar://update", event);
    }

    fn emit_upload(&self, event: &Value) {
        let _ = self.app.emit("agendar://upload", event);
    }

    fn start_notifications(&self) {
        self.sink.start();
    }

    fn attention(&self, needs: usize, working: usize, projects: usize) {
        {
            let mut last = self
                .attention
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            // A language switch alone also refreshes the tooltip.
            let seen = (needs, working, projects, crate::preferences::active_lang());
            if *last == Some(seen) {
                return;
            }
            *last = Some(seen);
        }
        let app = self.app.clone();
        let _ = self
            .app
            .run_on_main_thread(move || crate::tray::apply(&app, needs, working, projects));
    }
}
