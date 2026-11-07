//! Safe native notification actions for Agendar.
//!
//! This crate is the sole place in the workspace allowed to contain authored
//! `unsafe` code: it isolates the platform notification FFI (Apple
//! UserNotifications, Windows WinRT toasts, Linux D-Bus) behind a safe, fully
//! owned-data API. Nothing here knows about the Vault, the operator decision
//! journal or the app's authorization rules; callers own those.
//!
//! The public surface is [`Notifier`]: `start` yields a notifier plus a
//! receiver of [`NoticeEvent`]s, [`Notifier::show`] displays a [`Notice`] with
//! an `Open` action plus up to [`MAX_CHOICES`] choice buttons (the answer
//! options of an easy question, labelled with their text), and
//! [`Notifier::withdraw`] removes it again.
//! Events travel through one bounded channel; a callback never blocks, and an
//! overflow drops the event and reports the notifier as degraded.
//!
//! Action identifiers are `open` and `choose-0`..`choose-2` on every backend
//! (the legacy `approve` still parses as the first choice). Windows toast
//! arguments use the grammar `v=1&notice=<32 hex>&action=<identifier>`,
//! which [`parse_arguments`] validates before any event is emitted.

#![deny(unsafe_op_in_unsafe_fn)]

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// Number of action events that may be buffered before new ones are dropped.
pub const EVENT_CAPACITY: usize = 16;
/// Maximum length, in characters, of a notice title.
pub const MAX_TITLE_CHARS: usize = 120;
/// Maximum length, in characters, of a notice body.
pub const MAX_BODY_CHARS: usize = 400;
/// Maximum length, in characters, of a choice button label.
pub const MAX_ACTION_LABEL_CHARS: usize = 40;
/// Choice buttons one notice may offer next to Open.
pub const MAX_CHOICES: usize = 3;
/// Maximum length, in characters, of the application identifier.
pub const MAX_APP_ID_CHARS: usize = 128;
/// Version prefix of the frozen Windows toast argument grammar.
pub const ARGUMENT_VERSION: &str = "v=1";

/// Stable per-notice routing handle: exactly 32 lowercase hexadecimal bytes.
///
/// The identifier is an opaque routing handle that the caller chooses; it is
/// not a token and carries no authority.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NoticeId(String);

impl NoticeId {
    /// Length of a notice identifier in bytes.
    pub const LEN: usize = 32;

    /// Validates and wraps an already formatted identifier.
    pub fn parse(raw: &str) -> Result<Self, NotifyError> {
        if raw.len() != Self::LEN {
            return Err(NotifyError::InvalidId);
        }
        if !raw
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(NotifyError::InvalidId);
        }
        Ok(Self(raw.to_owned()))
    }

    /// Validates and wraps an owned identifier string.
    pub fn new(raw: impl Into<String>) -> Result<Self, NotifyError> {
        Self::parse(&raw.into())
    }

    /// Borrows the identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NoticeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for NoticeId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// The action a user selected on a notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoticeAction {
    /// Open the originating card in the app UI. The notification body and its
    /// default action always map to this action.
    Open,
    /// Answer with the notice's choice at this index (`0..MAX_CHOICES`). Only
    /// offered when the notice carries that choice.
    Choose(u8),
}

impl NoticeAction {
    /// Wire identifier for [`NoticeAction::Open`].
    pub const WIRE_OPEN: &'static str = "open";
    /// Wire identifiers of the choice buttons, by index.
    pub const WIRE_CHOOSE: [&'static str; MAX_CHOICES] = ["choose-0", "choose-1", "choose-2"];
    /// Legacy identifier of the single approve button; parses as `Choose(0)`.
    pub const WIRE_APPROVE: &'static str = "approve";

    /// Parses a wire action identifier. Anything but the known values,
    /// including the platform default/reply/dismiss identifiers, is rejected.
    pub fn parse(raw: &str) -> Option<Self> {
        if raw == Self::WIRE_OPEN {
            return Some(Self::Open);
        }
        if raw == Self::WIRE_APPROVE {
            return Some(Self::Choose(0));
        }
        Self::WIRE_CHOOSE
            .iter()
            .position(|wire| *wire == raw)
            .and_then(|index| u8::try_from(index).ok())
            .map(Self::Choose)
    }

    /// The wire identifier for this action.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Open => Self::WIRE_OPEN,
            Self::Choose(index) => Self::WIRE_CHOOSE
                .get(usize::from(index))
                .copied()
                .unwrap_or(Self::WIRE_OPEN),
        }
    }

    /// Builds the frozen toast argument string for this action.
    pub fn arguments(self, id: &NoticeId) -> String {
        format!(
            "{}&notice={}&action={}",
            ARGUMENT_VERSION,
            id.as_str(),
            self.wire()
        )
    }
}

impl fmt::Display for NoticeAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire())
    }
}

/// An event reported by the platform after a user interacted with a notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeEvent {
    /// The notice the user acted on.
    pub id: NoticeId,
    /// The action the user selected.
    pub action: NoticeAction,
}

/// Parses the frozen Windows toast argument grammar
/// `v=1&notice=<32 lowercase hex>&action=open|choose-N|approve`.
///
/// The grammar is strict: exactly three parameters, in that order, with no
/// percent-encoding, no duplicate keys and no user-supplied fields. A
/// malformed payload is rejected without producing an event, so an unknown
/// payload can never be mistaken for an action.
pub fn parse_arguments(raw: &str) -> Result<NoticeEvent, NotifyError> {
    let mut parts = raw.split('&');

    let version = parts.next().ok_or(NotifyError::InvalidArguments)?;
    if version != ARGUMENT_VERSION {
        return Err(NotifyError::InvalidArguments);
    }

    let notice = parts.next().ok_or(NotifyError::InvalidArguments)?;
    let notice = notice
        .strip_prefix("notice=")
        .ok_or(NotifyError::InvalidArguments)?;
    let id = NoticeId::parse(notice).map_err(|_| NotifyError::InvalidArguments)?;

    let action = parts.next().ok_or(NotifyError::InvalidArguments)?;
    let action = action
        .strip_prefix("action=")
        .ok_or(NotifyError::InvalidArguments)?;
    let action = NoticeAction::parse(action).ok_or(NotifyError::InvalidArguments)?;

    if parts.next().is_some() {
        return Err(NotifyError::InvalidArguments);
    }

    Ok(NoticeEvent { id, action })
}

/// A notification to display.
///
/// The fields are public so the app can build one without a builder dance, but
/// [`Notifier::show`] validates them again: the platform payloads must stay
/// bounded and free of control characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// Routing handle for this notification.
    pub id: NoticeId,
    /// Notification title, 1..=[`MAX_TITLE_CHARS`] characters.
    pub title: String,
    /// Notification body, 0..=[`MAX_BODY_CHARS`] characters.
    pub body: String,
    /// Labels of the choice buttons, in order; empty offers only Open.
    pub choices: Vec<String>,
}

impl Notice {
    /// Builds a notice with the Open action only.
    pub fn new(
        id: NoticeId,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, NotifyError> {
        let notice = Self {
            id,
            title: title.into(),
            body: body.into(),
            choices: Vec::new(),
        };
        notice.validate()?;
        Ok(notice)
    }

    /// Adds choice buttons, one per label (at most [`MAX_CHOICES`]).
    pub fn with_choices(
        mut self,
        labels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, NotifyError> {
        self.choices = labels.into_iter().map(Into::into).collect();
        self.validate()?;
        Ok(self)
    }

    /// Re-validates the notice bounds and character content.
    pub fn validate(&self) -> Result<(), NotifyError> {
        if self.title.is_empty() {
            return Err(NotifyError::InvalidNotice("title must not be empty"));
        }
        if self.title.chars().any(char::is_control) {
            return Err(NotifyError::InvalidNotice(
                "title contains control characters",
            ));
        }
        if self.title.chars().count() > MAX_TITLE_CHARS {
            return Err(NotifyError::InvalidNotice("title is too long"));
        }
        if self
            .body
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
        {
            return Err(NotifyError::InvalidNotice(
                "body contains control characters",
            ));
        }
        if self.body.chars().count() > MAX_BODY_CHARS {
            return Err(NotifyError::InvalidNotice("body is too long"));
        }
        if self.choices.len() > MAX_CHOICES {
            return Err(NotifyError::InvalidNotice("too many choices"));
        }
        for label in &self.choices {
            if label.is_empty() {
                return Err(NotifyError::InvalidNotice("choice label must not be empty"));
            }
            if label.chars().any(char::is_control) {
                return Err(NotifyError::InvalidNotice(
                    "choice label contains control characters",
                ));
            }
            if label.chars().count() > MAX_ACTION_LABEL_CHARS {
                return Err(NotifyError::InvalidNotice("choice label is too long"));
            }
        }
        Ok(())
    }

    /// Whether this notice offers any choice button.
    pub fn offers_choices(&self) -> bool {
        !self.choices.is_empty()
    }

    /// Whether `action` is one this notice actually offers.
    pub fn offers(&self, action: NoticeAction) -> bool {
        match action {
            NoticeAction::Open => true,
            NoticeAction::Choose(index) => usize::from(index) < self.choices.len(),
        }
    }
}

/// Whether this build/platform can turn a click into an action event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capability {
    /// Notifications can be shown. `actions` says whether the platform can
    /// deliver the Open/choice buttons back to this process.
    Available {
        /// Whether notification actions are delivered to the app.
        actions: bool,
    },
    /// Notifications cannot be shown here; the string explains why and is safe
    /// to surface to a person.
    Unavailable(String),
}

impl Capability {
    /// Whether notifications can be shown at all.
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    /// The unavailability reason, if any.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available { .. } => None,
            Self::Unavailable(reason) => Some(reason),
        }
    }
}

/// Errors produced by this crate. All values are owned; no platform error type
/// crosses the public API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifyError {
    /// A notice identifier was not 32 lowercase hexadecimal characters.
    InvalidId,
    /// A toast argument payload did not match the frozen grammar.
    InvalidArguments,
    /// A notice field was empty, too long or contained control characters.
    InvalidNotice(&'static str),
    /// The application identifier was empty, too long or contained control
    /// characters.
    InvalidAppId,
    /// The platform cannot show notifications, or the requested action.
    Unavailable(String),
    /// The notifier is shutting down or already shut down.
    Shutdown,
    /// The backend refuses more simultaneously live notifications.
    LiveLimit,
    /// The platform reported a failure for a valid request.
    Backend(String),
}

impl fmt::Display for NotifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId => f.write_str("notice id must be 32 lowercase hex characters"),
            Self::InvalidArguments => f.write_str("notification arguments are malformed"),
            Self::InvalidNotice(reason) => write!(f, "invalid notification: {reason}"),
            Self::InvalidAppId => f.write_str("application identifier is malformed"),
            Self::Unavailable(reason) => write!(f, "notifications unavailable: {reason}"),
            Self::Shutdown => f.write_str("notifier is shut down"),
            Self::LiveLimit => f.write_str("too many notifications are still live"),
            Self::Backend(reason) => write!(f, "notification backend failed: {reason}"),
        }
    }
}

impl std::error::Error for NotifyError {}

fn validate_app_id(app_id: &str) -> Result<(), NotifyError> {
    if app_id.is_empty()
        || app_id.chars().count() > MAX_APP_ID_CHARS
        || app_id.chars().any(char::is_control)
    {
        return Err(NotifyError::InvalidAppId);
    }
    Ok(())
}

/// Ingress shared by the notifier and every platform callback.
///
/// A callback only bounds, copies and enqueues: `try_send` never blocks, so a
/// full queue drops the event and marks the notifier degraded instead of
/// stalling the OS callback thread.
pub(crate) struct Ingress {
    sender: SyncSender<NoticeEvent>,
    closed: AtomicBool,
    degraded: AtomicBool,
}

impl Ingress {
    fn new(sender: SyncSender<NoticeEvent>) -> Self {
        Self {
            sender,
            closed: AtomicBool::new(false),
            degraded: AtomicBool::new(false),
        }
    }

    /// Emits one event, dropping it when the queue is full or the notifier is
    /// closed. Late callbacks after shutdown are ignored here.
    pub(crate) fn emit(&self, event: NoticeEvent) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        match self.sender.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.mark_degraded(),
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Records that the platform could not report actions reliably any more.
    pub(crate) fn mark_degraded(&self) {
        self.degraded.store(true, Ordering::Release);
    }

    fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Acquire)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

/// Platform backend contract. Implementors are owned by the notifier, hold
/// their platform handles on the thread that owns them and never leak an
/// unsafe obligation to the caller.
pub(crate) trait Transport: Send + Sync {
    /// Current capability, recomputed because authorization can change it.
    fn capability(&self) -> Capability;

    /// Shows one validated notice.
    fn show(&self, notice: &Notice) -> Result<(), NotifyError>;

    /// Withdraws one notice; unknown identifiers are a no-op success.
    fn withdraw(&self, id: &NoticeId) -> Result<(), NotifyError>;

    /// Bounded, idempotent teardown: no waiting on the OS, no authority left.
    fn shutdown(&self);

    /// Get ready to show `notice` (macOS registers its button category, which
    /// takes effect asynchronously). Showing without it still works, but the
    /// buttons may be missing.
    fn prepare(&self, _notice: &Notice) {}

    /// Why the platform refused the last notice, when it says so after `show`
    /// returned (macOS reports this asynchronously).
    fn last_error(&self) -> Option<String> {
        None
    }

    /// Re-read the platform permission (it can change while the app runs).
    fn refresh(&self) {}
}

#[cfg(not(any(target_os = "macos", windows, all(unix, not(target_os = "macos")))))]
struct UnavailableTransport {
    reason: String,
}

#[cfg(not(any(target_os = "macos", windows, all(unix, not(target_os = "macos")))))]
impl Transport for UnavailableTransport {
    fn capability(&self) -> Capability {
        Capability::Unavailable(self.reason.clone())
    }

    fn show(&self, _notice: &Notice) -> Result<(), NotifyError> {
        Err(NotifyError::Unavailable(self.reason.clone()))
    }

    fn withdraw(&self, _id: &NoticeId) -> Result<(), NotifyError> {
        Err(NotifyError::Unavailable(self.reason.clone()))
    }

    fn shutdown(&self) {}
}

#[cfg(target_os = "macos")]
fn platform_start(app_id: &str, ingress: Arc<Ingress>) -> Box<dyn Transport> {
    macos::start(app_id, ingress)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_start(app_id: &str, ingress: Arc<Ingress>) -> Box<dyn Transport> {
    linux::start(app_id, ingress)
}

#[cfg(windows)]
fn platform_start(app_id: &str, ingress: Arc<Ingress>) -> Box<dyn Transport> {
    windows::start(app_id, ingress)
}

#[cfg(not(any(target_os = "macos", windows, all(unix, not(target_os = "macos")))))]
fn platform_start(_app_id: &str, _ingress: Arc<Ingress>) -> Box<dyn Transport> {
    Box::new(UnavailableTransport {
        reason: "no native notification backend for this platform".to_owned(),
    })
}

/// A running notification transport plus the event ingress behind it.
///
/// `Notifier` is `Send + Sync`: platform handles stay on the worker/thread that
/// owns them and callers only pass owned Rust values.
///
/// # Threading
///
/// Windows and Linux run their own worker threads, so every method may be
/// called from any thread. macOS's notification centre is main-thread affine
/// in the bindings this crate uses: `start` must run on the native main thread
/// and `show`/`withdraw` must be routed there as well (for example through the
/// app's `run_on_main_thread`). Off-main calls do not move platform handles;
/// they report the notifier unavailable (`start`) or return
/// [`NotifyError::Backend`] (`show`/`withdraw`).
pub struct Notifier {
    ingress: Arc<Ingress>,
    transport: Box<dyn Transport>,
}

impl Notifier {
    /// Starts the platform notification backend.
    ///
    /// Returns the notifier and the receiver of action events. A platform that
    /// cannot deliver notifications still returns `Ok`: query
    /// [`Notifier::capability`] and fall back to the tray/main UI. Dropping or
    /// shutting the notifier closes the ingress, so later callbacks are ignored.
    /// On macOS this must be called on the native main thread (see [`Notifier`]).
    pub fn start(app_id: &str) -> Result<(Self, Receiver<NoticeEvent>), NotifyError> {
        validate_app_id(app_id)?;
        let (sender, receiver) = mpsc::sync_channel(EVENT_CAPACITY);
        let ingress = Arc::new(Ingress::new(sender));
        let transport = platform_start(app_id, Arc::clone(&ingress));
        Ok((Self { ingress, transport }, receiver))
    }

    /// Current capability. A degraded notifier reports itself unavailable
    /// because action delivery can no longer be trusted.
    pub fn capability(&self) -> Capability {
        if self.ingress.is_degraded() {
            return Capability::Unavailable(
                "notification event queue overflowed; actions are no longer reported".to_owned(),
            );
        }
        self.transport.capability()
    }

    /// Get ready to show `notice` a moment later; see [`Notifier::show`]. On
    /// macOS call it on the main thread.
    pub fn prepare(&self, notice: &Notice) -> Result<(), NotifyError> {
        notice.validate()?;
        self.transport.prepare(notice);
        Ok(())
    }

    /// Why the platform refused the last notice, if it said so after `show`.
    pub fn last_error(&self) -> Option<String> {
        self.transport.last_error()
    }

    /// Re-read the platform permission. On macOS call it on the main thread.
    pub fn refresh(&self) {
        self.transport.refresh();
    }

    /// Whether events were dropped because the bounded queue was full.
    pub fn is_degraded(&self) -> bool {
        self.ingress.is_degraded()
    }

    /// Shows one notice, including the actions it is allowed to offer.
    ///
    /// On macOS this must be called on the native main thread (see [`Notifier`]).
    pub fn show(&self, notice: Notice) -> Result<(), NotifyError> {
        if self.ingress.is_closed() {
            return Err(NotifyError::Shutdown);
        }
        notice.validate()?;
        if let Capability::Unavailable(reason) = self.capability() {
            return Err(NotifyError::Unavailable(reason));
        }
        self.transport.show(&notice)
    }

    /// Withdraws one notice. Unknown identifiers are a no-op success.
    ///
    /// On macOS this must be called on the native main thread (see [`Notifier`]).
    pub fn withdraw(&self, id: &NoticeId) -> Result<(), NotifyError> {
        if self.ingress.is_closed() {
            return Err(NotifyError::Shutdown);
        }
        self.transport.withdraw(id)
    }

    /// Closes the ingress and tears the backend down within a bounded time.
    /// Events that arrive later, from any thread, are ignored.
    pub fn shutdown(self) {
        drop(self);
    }
}

impl Drop for Notifier {
    fn drop(&mut self) {
        self.ingress.close();
        self.transport.shutdown();
    }
}

/// The in-memory fake transport (`feature = "fake"`).
///
/// It records what a real backend would have been asked to do and lets tests
/// inject callbacks, so the bounded-channel, degraded and ignore-after-shutdown
/// rules can be exercised without an OS notification centre.
#[cfg(feature = "fake")]
mod fake {
    use std::sync::{Mutex, PoisonError};

    use super::*;

    /// What the notifier asked the backend to do.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum FakeCall {
        /// A notice was shown; `choices` mirrors the offered buttons.
        Show {
            /// The shown notice.
            notice: Notice,
            /// The choice labels offered.
            choices: Vec<String>,
        },
        /// A notice was withdrawn.
        Withdraw(NoticeId),
        /// The backend was shut down.
        Shutdown,
    }

    /// Test control over the fake transport.
    pub struct FakeBackend {
        ingress: Arc<Ingress>,
        capability: Mutex<Capability>,
        calls: Mutex<Vec<FakeCall>>,
        shown: Mutex<Vec<Notice>>,
    }

    impl FakeBackend {
        fn new(ingress: Arc<Ingress>) -> Self {
            Self {
                ingress,
                capability: Mutex::new(Capability::Available { actions: true }),
                calls: Mutex::new(Vec::new()),
                shown: Mutex::new(Vec::new()),
            }
        }

        /// Simulates the platform reporting an action for a notice.
        pub fn deliver(&self, id: &NoticeId, action: NoticeAction) {
            self.ingress.emit(NoticeEvent {
                id: id.clone(),
                action,
            });
        }

        /// Simulates a raw toast argument payload arriving from the OS.
        /// Malformed payloads produce no event.
        pub fn deliver_arguments(&self, raw: &str) {
            if let Ok(event) = crate::parse_arguments(raw) {
                self.ingress.emit(event);
            }
        }

        /// Replaces the reported capability.
        pub fn set_capability(&self, capability: Capability) {
            *self
                .capability
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = capability;
        }

        /// Everything the notifier asked the fake backend to do, in order.
        pub fn calls(&self) -> Vec<FakeCall> {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        /// The notices that are currently shown.
        pub fn shown(&self) -> Vec<Notice> {
            self.shown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    struct FakeTransport(Arc<FakeBackend>);

    impl Transport for FakeTransport {
        fn capability(&self) -> Capability {
            self.0
                .capability
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn show(&self, notice: &Notice) -> Result<(), NotifyError> {
            if let Capability::Unavailable(reason) = self.capability() {
                return Err(NotifyError::Unavailable(reason));
            }
            self.0
                .calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(FakeCall::Show {
                    notice: notice.clone(),
                    choices: notice.choices.clone(),
                });
            self.0
                .shown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(notice.clone());
            Ok(())
        }

        fn withdraw(&self, id: &NoticeId) -> Result<(), NotifyError> {
            if let Capability::Unavailable(reason) = self.capability() {
                return Err(NotifyError::Unavailable(reason));
            }
            self.0
                .calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(FakeCall::Withdraw(id.clone()));
            self.0
                .shown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|notice| &notice.id != id);
            Ok(())
        }

        fn shutdown(&self) {
            self.0
                .calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(FakeCall::Shutdown);
        }
    }

    /// Starts a notifier backed by the in-memory fake transport.
    pub fn start_fake(
        app_id: &str,
    ) -> Result<(Notifier, Receiver<NoticeEvent>, Arc<FakeBackend>), NotifyError> {
        validate_app_id(app_id)?;
        let (sender, receiver) = mpsc::sync_channel(EVENT_CAPACITY);
        let ingress = Arc::new(Ingress::new(sender));
        let backend = Arc::new(FakeBackend::new(Arc::clone(&ingress)));
        let notifier = Notifier {
            ingress,
            transport: Box::new(FakeTransport(Arc::clone(&backend))),
        };
        Ok((notifier, receiver, backend))
    }
}

#[cfg(feature = "fake")]
pub use fake::{FakeBackend, FakeCall, start_fake};
