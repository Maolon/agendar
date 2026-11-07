//! Linux backend: freedesktop notifications through `notify-rust`.
//!
//! Every D-Bus call happens on this crate's dedicated worker thread, because
//! `notify-rust`'s synchronous helpers run their own blocking reactor and must
//! never be called from a GUI or async-runtime thread. Each shown notification
//! keeps one blocking waiter thread (bounded by [`MAX_WAITERS`]) that reports
//! the action; the bounded event channel is the only thing a waiter touches.
//!
//! A missing daemon or a failed capability query reports
//! [`Capability::Unavailable`]; a daemon that does not advertise the `actions`
//! capability reports `Available { actions: false }` so the app can fall back
//! to the tray/main UI.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::thread;
use std::time::Duration;

use notify_rust::{Hint, Notification, NotificationResponse};

use crate::{
    Capability, Ingress, Notice, NoticeAction, NoticeEvent, NoticeId, NotifyError, Transport,
};

/// Concurrent blocking waiters, i.e. notifications whose action is awaited.
const MAX_WAITERS: usize = 4;
/// How long `start` waits for the worker's capability answer. The D-Bus query
/// cannot be cancelled, so the deadline only stops the caller waiting and never
/// spawns a retry.
const START_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a `show` waits for the worker before reporting a timeout.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

enum Command {
    Show {
        notice: Notice,
        reply: SyncSender<Result<(), NotifyError>>,
    },
    Shutdown,
}

pub(crate) struct Backend {
    commands: Mutex<Sender<Command>>,
    capability: Mutex<Capability>,
    withdrawn: Arc<Mutex<HashSet<NoticeId>>>,
    closed: AtomicBool,
}

fn build_notification(app_id: &str, notice: &Notice) -> Notification {
    let mut notification = Notification::new();
    notification
        .appname(app_id)
        .hint(Hint::DesktopEntry(app_id.to_owned()))
        .summary(&notice.title)
        .action("default", "Open")
        .action(NoticeAction::WIRE_OPEN, "Open");
    if !notice.body.is_empty() {
        notification.body(&notice.body);
    }
    for (index, label) in notice.choices.iter().enumerate() {
        if let Some(wire) = NoticeAction::WIRE_CHOOSE.get(index) {
            notification.action(wire, label);
        }
    }
    notification
}

fn response_action(response: &NotificationResponse) -> Option<NoticeAction> {
    match response {
        // A body click is the default action and must never answer.
        NotificationResponse::Default => Some(NoticeAction::Open),
        NotificationResponse::Action(action) => NoticeAction::parse(action),
        // Replies and daemon-side closes carry no user decision.
        NotificationResponse::Reply(_) | NotificationResponse::Closed(_) => None,
    }
}

/// Spawns the waiter for one shown notification. The handle must be owned by the
/// waiter because `wait_for_response` consumes it.
fn spawn_waiter(
    handle: notify_rust::NotificationHandle,
    id: NoticeId,
    ingress: Arc<Ingress>,
    withdrawn: Arc<Mutex<HashSet<NoticeId>>>,
    waiters: Arc<AtomicUsize>,
) {
    let thread_ingress = Arc::clone(&ingress);
    let thread_waiters = Arc::clone(&waiters);
    let spawned = thread::Builder::new()
        .name("agendar-notify-wait".to_owned())
        .spawn(move || {
            let waiter = Arc::clone(&thread_ingress);
            let withdrawal = Arc::clone(&withdrawn);
            let slots = Arc::clone(&thread_waiters);
            let waited_id = id.clone();
            let _ = handle.wait_for_response(move |response: &NotificationResponse| {
                let Some(action) = response_action(response) else {
                    return;
                };
                // Never block the daemon's callback thread: if the withdrawal
                // directory is busy, drop this callback and report a degraded
                // notifier instead of stalling the notification daemon.
                let is_withdrawn = match withdrawal.try_lock() {
                    Ok(withdrawn) => withdrawn.contains(&waited_id),
                    Err(TryLockError::Poisoned(poisoned)) => {
                        poisoned.into_inner().contains(&waited_id)
                    }
                    Err(TryLockError::WouldBlock) => {
                        waiter.mark_degraded();
                        return;
                    }
                };
                if !is_withdrawn {
                    waiter.emit(NoticeEvent {
                        id: waited_id.clone(),
                        action,
                    });
                }
            });
            slots.fetch_sub(1, Ordering::AcqRel);
        });
    if spawned.is_err() {
        waiters.fetch_sub(1, Ordering::AcqRel);
        // Without a waiter the notification cannot report an action; report a
        // degraded notifier instead of silently dropping user decisions.
        ingress.mark_degraded();
    }
}

fn worker(
    app_id: String,
    ingress: Arc<Ingress>,
    withdrawn: Arc<Mutex<HashSet<NoticeId>>>,
    waiters: Arc<AtomicUsize>,
    commands: Receiver<Command>,
    ready: SyncSender<Result<Capability, String>>,
) {
    let capability = match notify_rust::get_capabilities() {
        Ok(capabilities) => Capability::Available {
            actions: capabilities.iter().any(|name| name == "actions"),
        },
        Err(error) => {
            let _ = ready.send(Err(format!("notification daemon unavailable: {error}")));
            return;
        }
    };
    if ready.send(Ok(capability)).is_err() {
        return;
    }

    while let Ok(command) = commands.recv() {
        match command {
            Command::Show { notice, reply } => {
                // The worker is the only incrementer, so this check is exact.
                if waiters.load(Ordering::Acquire) >= MAX_WAITERS {
                    let _ = reply.send(Err(NotifyError::LiveLimit));
                    continue;
                }
                let notification = build_notification(&app_id, &notice);
                match notification.show() {
                    Ok(handle) => {
                        waiters.fetch_add(1, Ordering::AcqRel);
                        spawn_waiter(
                            handle,
                            notice.id,
                            Arc::clone(&ingress),
                            Arc::clone(&withdrawn),
                            Arc::clone(&waiters),
                        );
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(NotifyError::Backend(format!("{error}"))));
                    }
                }
            }
            Command::Shutdown => break,
        }
    }
}

pub(crate) fn start(app_id: &str, ingress: Arc<Ingress>) -> Box<dyn Transport> {
    let (command_sender, command_receiver) = mpsc::channel();
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let withdrawn = Arc::new(Mutex::new(HashSet::new()));
    let waiters = Arc::new(AtomicUsize::new(0));

    let worker_app_id = app_id.to_owned();
    let worker_ingress = Arc::clone(&ingress);
    let worker_withdrawn = Arc::clone(&withdrawn);
    let worker_waiters = Arc::clone(&waiters);
    let spawned = thread::Builder::new()
        .name("agendar-notify".to_owned())
        .spawn(move || {
            worker(
                worker_app_id,
                worker_ingress,
                worker_withdrawn,
                worker_waiters,
                command_receiver,
                ready_sender,
            )
        });

    let capability = match spawned {
        Err(error) => Capability::Unavailable(format!("notification worker failed: {error}")),
        Ok(_) => match ready_receiver.recv_timeout(START_TIMEOUT) {
            Ok(Ok(capability)) => capability,
            Ok(Err(reason)) => Capability::Unavailable(reason),
            Err(_) => Capability::Unavailable(
                "the notification daemon did not answer the capability query".to_owned(),
            ),
        },
    };

    Box::new(Backend {
        commands: Mutex::new(command_sender),
        capability: Mutex::new(capability),
        withdrawn,
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
                "the notification daemon did not answer in time".to_owned(),
            )),
        }
    }

    fn withdraw(&self, id: &NoticeId) -> Result<(), NotifyError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(NotifyError::Shutdown);
        }
        // The blocking waiter owns the notification handle, so a daemon-side
        // close is not attempted here; the notice is invalidated locally so a
        // late click is ignored.
        self.withdrawn
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.clone());
        Ok(())
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

    #[test]
    fn default_dbus_action_is_open_and_only_wire_names_are_actions() {
        assert_eq!(
            response_action(&NotificationResponse::Default),
            Some(NoticeAction::Open)
        );
        assert_eq!(
            response_action(&NotificationResponse::Action("approve".to_owned())),
            Some(NoticeAction::Choose(0)),
            "the legacy approve id is the first choice"
        );
        assert_eq!(
            response_action(&NotificationResponse::Action("choose-2".to_owned())),
            Some(NoticeAction::Choose(2))
        );
        assert_eq!(
            response_action(&NotificationResponse::Action("default".to_owned())),
            None
        );
        assert_eq!(
            response_action(&NotificationResponse::Action("bogus".to_owned())),
            None
        );
        assert_eq!(
            response_action(&NotificationResponse::Reply("text".to_owned())),
            None
        );
    }

    #[test]
    fn choice_actions_only_appear_with_choices() {
        let id = NoticeId::parse("0123456789abcdef0123456789abcdef").unwrap();
        let plain = crate::Notice::new(id, "title", "body").unwrap();
        let notification = build_notification("dev.agendar.app", &plain);
        assert_eq!(
            notification.actions,
            vec!["default", "Open", "open", "Open"],
            "only the default and open actions may be offered"
        );

        let with_choices = plain.with_choices(["JSON lines", "SQLite"]).unwrap();
        let notification = build_notification("dev.agendar.app", &with_choices);
        assert_eq!(
            notification.actions,
            vec![
                "default",
                "Open",
                "open",
                "Open",
                "choose-0",
                "JSON lines",
                "choose-1",
                "SQLite"
            ]
        );
    }
}
