//! UI side effects behind a trait so backend tests never need a window.

use serde_json::Value;

/// Window/event operations the host may request. The Tauri implementation
/// schedules them on the native main thread; tests record them.
pub trait Ui: Send + Sync {
    fn show_main(&self);
    fn hide_popover(&self);
    /// Show and focus the main window on one exact need, then publish the
    /// process-navigation event with that target.
    fn focus(&self, focus: &Value);
    /// Show the main window on one Settings tab (the tray's Update button
    /// opens About), via `agendar://navigate`.
    fn open_settings(&self, tab: &str);
    fn emit_changed(&self, event: &Value);
    fn emit_unblock(&self, event: &Value);
    fn emit_health(&self, event: &Value);
    /// An update check started or finished (`agendar://update`), so the
    /// sidebar shows "checking" at once instead of on the next snapshot.
    fn emit_update(&self, _event: &Value) {}
    /// This host's own writes started or stopped waiting for their upload
    /// (`agendar://upload`): `{v, upload: {state, since} | null}`.
    fn emit_upload(&self, _event: &Value) {}
    /// Ask the platform notification backend to start if consent allows it.
    fn start_notifications(&self) {}
    /// How many items currently need the human (idle glyph at zero, "needs
    /// you" glyph otherwise), how many projects have an agent at work (a
    /// number beside the menu bar icon, hidden at zero) and how many projects
    /// are on the board at all (a hollow glyph at zero).
    fn attention(&self, _needs: usize, _working: usize, _projects: usize) {}
}
