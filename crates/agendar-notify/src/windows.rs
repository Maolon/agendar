//! Windows backend: WinRT toasts with in-process `Activated` handlers.
//!
//! A dedicated worker thread owns the apartment, the `ToastNotifier` and the
//! retained toasts, so no WinRT handle is ever moved between threads. The
//! worker calls `RoInitialize` when it starts and balances it with
//! `RoUninitialize` on the same thread after the toasts and handlers are gone.
//!
//! Only the in-process `Activated` handler is registered. The installed COM
//! activator/CLSID path and Notification Center cold activation are
//! deliberately absent; without installed identity the
//! notifier is reported unavailable, never silently banner-only.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{
    ToastActivatedEventArgs, ToastNotification, ToastNotificationManager, ToastNotifier,
};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::core::{HSTRING, IInspectable, Interface, Ref};

use crate::{Capability, Ingress, Notice, NoticeAction, NoticeId, NotifyError, Transport};

/// Toasts kept alive at once; beyond this the oldest toast is hidden.
const MAX_LIVE_TOASTS: usize = 4;
/// How long `start` waits for the worker's apartment/notifier initialisation.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a command waits for the worker before reporting a timeout.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// Label of the foreground open action.
const OPEN_LABEL: &str = "Open";

enum Command {
    Show {
        notice: Notice,
        reply: SyncSender<Result<(), NotifyError>>,
    },
    Withdraw {
        id: NoticeId,
        reply: SyncSender<Result<(), NotifyError>>,
    },
    Shutdown,
}

struct RetainedToast {
    id: NoticeId,
    toast: ToastNotification,
    token: i64,
    /// Keeps our handler alive beyond banner dismissal; the token is removed on
    /// withdrawal or shutdown.
    _handler: TypedEventHandler<ToastNotification, IInspectable>,
}

pub(crate) struct Backend {
    commands: Mutex<Sender<Command>>,
    capability: Mutex<Capability>,
    closed: AtomicBool,
}

fn backend_error(error: windows::core::Error) -> NotifyError {
    NotifyError::Backend(format!("{error}"))
}

fn escape_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn escape_attribute(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Builds the toast XML: Open plus one button per choice the notice carries;
/// every argument uses the `v=1&notice=…&action=…` grammar.
fn toast_xml(notice: &Notice) -> String {
    let open_arguments = escape_attribute(&NoticeAction::Open.arguments(&notice.id));
    let mut actions = String::new();
    actions.push_str("<action content=\"");
    actions.push_str(&escape_attribute(OPEN_LABEL));
    actions.push_str("\" arguments=\"");
    actions.push_str(&open_arguments);
    actions.push_str("\" activationType=\"foreground\"/>");
    for (index, label) in notice.choices.iter().enumerate() {
        let Ok(index) = u8::try_from(index) else {
            break;
        };
        actions.push_str("<action content=\"");
        actions.push_str(&escape_attribute(label));
        actions.push_str("\" arguments=\"");
        actions.push_str(&escape_attribute(
            &NoticeAction::Choose(index).arguments(&notice.id),
        ));
        actions.push_str("\" activationType=\"foreground\"/>");
    }

    format!(
        "<toast launch=\"{open_arguments}\" activationType=\"foreground\">\
         <visual><binding template=\"ToastGeneric\">\
         <text>{title}</text><text>{body}</text>\
         </binding></visual>\
         <actions>{actions}</actions></toast>",
        title = escape_text(&notice.title),
        body = escape_text(&notice.body),
    )
}

fn show(
    notifier: &ToastNotifier,
    ingress: &Arc<Ingress>,
    live: &mut Vec<RetainedToast>,
    notice: &Notice,
) -> Result<(), NotifyError> {
    if live.iter().any(|retained| retained.id == notice.id) {
        return Err(NotifyError::Backend(
            "a notification with this id is already shown".to_owned(),
        ));
    }

    let document = XmlDocument::new().map_err(backend_error)?;
    document
        .LoadXml(&HSTRING::from(toast_xml(notice)))
        .map_err(backend_error)?;
    let toast = ToastNotification::CreateToastNotification(&document).map_err(backend_error)?;

    let captured = notice.id.clone();
    let ingress = Arc::clone(ingress);
    let handler = TypedEventHandler::new(
        move |_sender: Ref<'_, ToastNotification>, args: Ref<'_, IInspectable>| {
            // A panic must never unwind across the WinRT ABI; the notifier is
            // marked degraded instead.
            let decoded = catch_unwind(AssertUnwindSafe(|| {
                let Some(object) = args.as_ref() else {
                    return;
                };
                let Ok(event_args) = object.cast::<ToastActivatedEventArgs>() else {
                    return;
                };
                let Ok(arguments) = event_args.Arguments() else {
                    return;
                };
                let raw = arguments.to_string();
                let Ok(event) = crate::parse_arguments(&raw) else {
                    return;
                };
                if event.id == captured {
                    ingress.emit(event);
                }
            }));
            if decoded.is_err() {
                ingress.mark_degraded();
            }
            Ok(())
        },
    );
    let token = toast.Activated(&handler).map_err(backend_error)?;
    if let Err(error) = notifier.Show(&toast) {
        // Showing failed: drop the handler registration before the toast.
        let _ = toast.RemoveActivated(token);
        return Err(backend_error(error));
    }

    if live.len() >= MAX_LIVE_TOASTS {
        let oldest = live.remove(0);
        hide(notifier, oldest);
    }
    live.push(RetainedToast {
        id: notice.id.clone(),
        toast,
        token,
        _handler: handler,
    });
    Ok(())
}

fn hide(notifier: &ToastNotifier, retained: RetainedToast) {
    let _ = notifier.Hide(&retained.toast);
    let _ = retained.toast.RemoveActivated(retained.token);
}

fn withdraw(
    notifier: &ToastNotifier,
    live: &mut Vec<RetainedToast>,
    id: &NoticeId,
) -> Result<(), NotifyError> {
    if let Some(index) = live.iter().position(|retained| &retained.id == id) {
        let retained = live.remove(index);
        hide(notifier, retained);
    }
    Ok(())
}

fn worker(
    app_id: String,
    ingress: Arc<Ingress>,
    commands: Receiver<Command>,
    ready: SyncSender<Result<(), String>>,
) {
    // SAFETY: initialising the dedicated worker's MTA apartment. The matching
    // `RoUninitialize` runs on this same thread on every return path below.
    if let Err(error) = unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
        let _ = ready.send(Err(format!(
            "WinRT apartment initialisation failed (the thread may already be in STA mode): {error}"
        )));
        return;
    }

    let notifier = match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
        app_id.as_str(),
    )) {
        Ok(notifier) => notifier,
        Err(error) => {
            let _ = ready.send(Err(format!(
                "the application id has no installed toast identity: {error}"
            )));
            // SAFETY: balances the successful `RoInitialize` above on this thread.
            unsafe { RoUninitialize() };
            return;
        }
    };

    if ready.send(Ok(())).is_err() {
        // The caller is gone before the toasts can be shown; drop the WinRT
        // object first so no object outlives the apartment it belongs to.
        drop(notifier);
        // SAFETY: balances the successful `RoInitialize` above on this thread.
        unsafe { RoUninitialize() };
        return;
    }

    let mut live: Vec<RetainedToast> = Vec::new();
    while let Ok(command) = commands.recv() {
        match command {
            Command::Show { notice, reply } => {
                let result = show(&notifier, &ingress, &mut live, &notice);
                let _ = reply.send(result);
            }
            Command::Withdraw { id, reply } => {
                let result = withdraw(&notifier, &mut live, &id);
                let _ = reply.send(result);
            }
            Command::Shutdown => break,
        }
    }

    for retained in live.drain(..) {
        hide(&notifier, retained);
    }
    drop(notifier);
    // SAFETY: balances the successful `RoInitialize` above, after the handlers
    // and toasts owned by this apartment are gone and on the same thread.
    unsafe { RoUninitialize() };
}

pub(crate) fn start(app_id: &str, ingress: Arc<Ingress>) -> Box<dyn Transport> {
    let (command_sender, command_receiver) = mpsc::channel();
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);

    let worker_app_id = app_id.to_owned();
    let spawned = thread::Builder::new()
        .name("agendar-notify".to_owned())
        .spawn(move || worker(worker_app_id, ingress, command_receiver, ready_sender));

    let capability = match spawned {
        Err(error) => Capability::Unavailable(format!("notification worker failed: {error}")),
        Ok(_) => match ready_receiver.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => Capability::Available { actions: true },
            Ok(Err(reason)) => Capability::Unavailable(reason),
            Err(_) => Capability::Unavailable(
                "the toast subsystem did not finish initialising".to_owned(),
            ),
        },
    };

    Box::new(Backend {
        commands: Mutex::new(command_sender),
        capability: Mutex::new(capability),
        closed: AtomicBool::new(false),
    })
}

impl Backend {
    fn capability_snapshot(&self) -> Capability {
        self.capability
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn mark_unavailable(&self, reason: String) {
        *self
            .capability
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Capability::Unavailable(reason);
    }
}

impl Transport for Backend {
    fn capability(&self) -> Capability {
        self.capability_snapshot()
    }

    fn show(&self, notice: &Notice) -> Result<(), NotifyError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(NotifyError::Shutdown);
        }
        if let Capability::Unavailable(reason) = self.capability_snapshot() {
            return Err(NotifyError::Unavailable(reason));
        }
        let (reply, answer) = mpsc::sync_channel(1);
        {
            let commands = self.commands.lock().unwrap_or_else(PoisonError::into_inner);
            if commands
                .send(Command::Show {
                    notice: notice.clone(),
                    reply,
                })
                .is_err()
            {
                drop(commands);
                let reason = "the notification worker stopped".to_owned();
                self.mark_unavailable(reason.clone());
                return Err(NotifyError::Unavailable(reason));
            }
        }
        match answer.recv_timeout(COMMAND_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Err(NotifyError::Backend(
                "the toast subsystem did not answer in time".to_owned(),
            )),
        }
    }

    fn withdraw(&self, id: &NoticeId) -> Result<(), NotifyError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(NotifyError::Shutdown);
        }
        let (reply, answer) = mpsc::sync_channel(1);
        {
            let commands = self.commands.lock().unwrap_or_else(PoisonError::into_inner);
            if commands
                .send(Command::Withdraw {
                    id: id.clone(),
                    reply,
                })
                .is_err()
            {
                return Ok(());
            }
        }
        match answer.recv_timeout(COMMAND_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Ok(()),
        }
    }

    fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(commands) = self.commands.lock() {
            let _ = commands.send(Command::Shutdown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn notice() -> Notice {
        Notice::new(
            NoticeId::parse(ID).unwrap(),
            "Slice ready",
            "Open to review <the diff> & decide",
        )
        .unwrap()
    }

    #[test]
    fn choice_buttons_are_only_in_the_payload_when_asked_for() {
        let plain = toast_xml(&notice());
        assert_eq!(plain.matches("<action ").count(), 1);
        assert_eq!(plain.matches("action=choose").count(), 0);

        let with_choices = notice().with_choices(["JSON lines", "SQLite"]).unwrap();
        let xml = toast_xml(&with_choices);
        assert_eq!(xml.matches("<action ").count(), 3);
        assert_eq!(xml.matches("action=choose-0").count(), 1);
        assert_eq!(xml.matches("action=choose-1").count(), 1);
        assert!(xml.contains("content=\"JSON lines\""));
    }

    #[test]
    fn arguments_are_escaped_for_xml_and_round_trip() {
        let xml = toast_xml(&notice());
        assert!(!xml.contains("&notice="), "raw ampersands must be escaped");
        assert!(xml.contains(&format!("v=1&amp;notice={ID}&amp;action=open")));
        let payload = format!("v=1&notice={ID}&action=open");
        assert_eq!(
            crate::parse_arguments(&payload).unwrap().id.as_str(),
            ID,
            "the encoded arguments must be exactly the frozen grammar"
        );
        assert!(xml.contains("&lt;the diff&gt; &amp; decide"));
    }

    #[test]
    fn user_text_cannot_inject_markup() {
        let injected = Notice::new(
            NoticeId::parse(ID).unwrap(),
            "title",
            "<action content=\"Approve\" arguments=\"v=1&amp;notice=bad&amp;action=approve\"/>",
        )
        .unwrap();
        let xml = toast_xml(&injected);
        // Markup in user text must stay escaped text: the document contains
        // exactly one `<action>` element, that element is the open action, and
        // the only `action=approve` occurrence sits inside the escaped body.
        assert_eq!(xml.matches("<action ").count(), 1);
        assert!(xml.contains(&format!(
            "<action content=\"Open\" arguments=\"v=1&amp;notice={ID}&amp;action=open\" activationType=\"foreground\"/>"
        )));
        assert!(xml.contains("&lt;action content=\"Approve\""));
        assert_eq!(xml.matches("action=approve").count(), 1);
    }
}
