//! Update detection for the app: the cached status in every snapshot, a
//! manual "Check now", the background check and the release page link.
//!
//! Only the local data directory is touched (`update-check.json`); nothing is
//! installed. When the person turned checks off, no request is made at all.

use std::path::Path;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use agendar_core::error::ErrorCode;
use agendar_core::update::{self, Fetcher};

use super::{Host, open_with_system};
use crate::error::{ApiResult, AppError};

impl Host {
    /// Whether the person allows update checks (default on).
    pub fn update_checks_enabled(&self) -> bool {
        self.lock().preferences.update_check
    }

    /// The cached update status (no network) plus the switch and the last
    /// failure, as the snapshot and Settings show it.
    pub(super) fn update_view(&self, enabled: bool) -> Value {
        let mut view = json!(update::cached_status(&self.data_root, false));
        if let Value::Object(object) = &mut view {
            object.insert("enabled".to_owned(), json!(enabled));
            object.insert(
                "checking".to_owned(),
                json!(self.update_checking.load(Ordering::Acquire)),
            );
            if let Some(error) = self
                .update_error
                .lock()
                .ok()
                .and_then(|error| error.clone())
            {
                object.insert("error".to_owned(), json!(error));
            }
        }
        view
    }

    /// "Check now": one request to the public releases API.
    pub fn check_updates(&self) -> ApiResult<Value> {
        let url = update::endpoint().map_err(AppError::from)?;
        self.check_updates_with(&update::HttpFetcher, &url)
    }

    /// One check through `fetcher`. Refused without a request while checks are
    /// off or another check is running.
    pub fn check_updates_with(&self, fetcher: &dyn Fetcher, url: &str) -> ApiResult<Value> {
        if !self.update_checks_enabled() {
            return Err(AppError::new(
                ErrorCode::Policy.as_str(),
                "Update checks are turned off.",
            ));
        }
        if self.update_checking.swap(true, Ordering::AcqRel) {
            return Err(AppError::new(
                ErrorCode::Busy.as_str(),
                "An update check is already running.",
            )
            .retryable(true));
        }
        self.emit_update_phase("started");
        let result = update::check(&self.data_root, fetcher, url, self.clock.now());
        self.update_checking.store(false, Ordering::Release);
        let failure = result
            .as_ref()
            .err()
            .map(|error| error.message().to_owned());
        if let Ok(mut slot) = self.update_error.lock() {
            *slot = failure;
        }
        // Finished either way: the event carries the new status or the error.
        self.emit_update_phase("finished");
        result.map_err(AppError::from)?;
        Ok(json!({ "update": self.update_view(true) }))
    }

    /// `agendar://update`: `{v, phase: started|finished, update}`.
    fn emit_update_phase(&self, phase: &str) {
        let enabled = self.update_checks_enabled();
        self.ui.emit_update(&json!({
            "v": 1,
            "phase": phase,
            "update": self.update_view(enabled),
        }));
    }

    /// The background loop's check: silent, and only when allowed.
    pub fn background_update_check(&self) {
        if self.update_checks_enabled() {
            let _ = self.check_updates();
        }
    }

    /// Open the latest release page (or the releases list) in the browser.
    /// The link comes from the cache and must be one of this project's
    /// release pages on github.com; nothing from the webview is opened.
    pub fn open_release(&self) -> ApiResult<Value> {
        let url = update::load_cache(&self.data_root)
            .latest
            .map(|release| release.url)
            .filter(|url| update::is_release_page(url))
            .unwrap_or_else(|| update::RELEASES_PAGE.to_owned());
        open_with_system(Path::new(&url)).map_err(|error| {
            AppError::new(
                ErrorCode::Io.as_str(),
                format!("Cannot open the release page: {error}."),
            )
        })?;
        Ok(json!({ "opened": true }))
    }
}
