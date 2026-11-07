//! macOS backend: Apple's UserNotifications framework.
//!
//! objc2 does not declare `UNUserNotificationCenter` as usable from any thread,
//! so this backend never moves it: the centre and its strongly retained
//! delegate live in the macOS main thread's thread-local storage, `start` must
//! be called there and `show`/`withdraw` are routed there by the app (the app
//! owns main-thread scheduling). The
//! notifier itself only keeps `Send + Sync` values, so no platform handle is
//! ever asserted `Send`/`Sync`.
//!
//! Outside an app bundle the centre has no permission identity; asking it would
//! abort the process, so this backend reports [`Capability::Unavailable`] and
//! keeps working (never crashing, never showing).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send};
use objc2_foundation::{NSArray, NSBundle, NSError, NSSet, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationAction, UNNotificationActionOptions, UNNotificationCategory,
    UNNotificationCategoryOptions, UNNotificationDefaultActionIdentifier,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSettings, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use crate::{
    Capability, Ingress, Notice, NoticeAction, NoticeEvent, NoticeId, NotifyError, Transport,
};

/// Category with a single Open action (body and default action also open).
const CATEGORY_OPEN: &str = "agendar.open";
/// Choice categories kept registered at once (each live notice needs its own).
const MAX_CATEGORIES: usize = 16;
/// Label of the Open action.
const OPEN_LABEL: &str = "Open";
const AUTH_UNKNOWN: u8 = 0;
const AUTH_GRANTED: u8 = 1;
const AUTH_DENIED: u8 = 2;
/// Notifications kept at once; showing one more retires the oldest.
const LIVE_LIMIT: usize = 8;
const NOT_BUNDLED: &str =
    "Agendar is not running from an app bundle, so UserNotifications has no identity";
const NOT_MAIN_THREAD: &str =
    "macOS notifications must be started on the native main thread before launch completes";
const MAIN_THREAD_ONLY: &str = "macOS notifications must be shown on the native main thread";

struct DelegateIvars {
    ingress: Arc<Ingress>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, the superclass is
    // thread-safe, the only ivar is `Arc<Ingress>` (Send + Sync) and the class
    // is not `MainThreadOnly`, so the auto traits make it Send + Sync; it is
    // nevertheless only ever retained on the main thread.
    #[unsafe(super(NSObject))]
    #[name = "AgendarNotifyDelegate"]
    #[ivars = DelegateIvars]
    struct NotifyDelegate;

    // SAFETY: `NotifyDelegate` inherits from NSObject and adds no requirement
    // of the NSObject protocol that the runtime would not expect.
    unsafe impl NSObjectProtocol for NotifyDelegate {}

    // SAFETY: implements the two optional delegate methods used here and
    // upholds their contracts: the borrowed `response`/`notification` objects
    // are decoded into owned values inside the callback and never cross a
    // thread, the completion handler is called exactly once even if decoding
    // panics, and the ivars keep the ingress alive for the delegate's life.
    unsafe impl UNUserNotificationCenterDelegate for NotifyDelegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive_response(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            // Decoding and the completion call are contained separately so the
            // handler still runs exactly once after a decode panic, and so no
            // panic can unwind into Objective-C from either step.
            contain_panic(&self.ivars().ingress, || self.decode(response));
            contain_panic(&self.ivars().ingress, || completion_handler.call(()));
        }

        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            contain_panic(&self.ivars().ingress, || {
                completion_handler.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
            });
        }
    }
);

/// Runs one step of a foreign callback behind the unwind barrier a
/// UserNotifications delegate requires.
///
/// A panic must never unwind into Objective-C: the step is contained here, a
/// contained panic marks the notifier degraded (the app then falls back to the
/// tray) and is reported to the caller as `false`.
fn contain_panic(ingress: &Arc<Ingress>, step: impl FnOnce()) -> bool {
    if catch_unwind(AssertUnwindSafe(step)).is_err() {
        ingress.mark_degraded();
        return false;
    }
    true
}

impl NotifyDelegate {
    /// Copies the bounded identifiers out of the borrowed response and enqueues
    /// one event. Unknown identifiers or actions produce no event.
    fn decode(&self, response: &UNNotificationResponse) {
        let identifier = response.actionIdentifier().to_string();
        let Some(action) = action_from_identifier(&identifier) else {
            return;
        };
        let request = response.notification().request();
        let raw_id = request.identifier().to_string();
        let Ok(id) = NoticeId::parse(&raw_id) else {
            return;
        };
        self.ivars().ingress.emit(NoticeEvent { id, action });
    }
}

/// The runtime identifier macOS uses for a body/default click.
fn default_action_identifier() -> String {
    // SAFETY: `UNNotificationDefaultActionIdentifier` is an extern static the
    // framework initialises to a valid NSString for the process lifetime; it is
    // only read here, through `Display`, and never retained beyond the call.
    unsafe { UNNotificationDefaultActionIdentifier }.to_string()
}

fn action_from_identifier(identifier: &str) -> Option<NoticeAction> {
    if let Some(action) = NoticeAction::parse(identifier) {
        return Some(action);
    }
    if identifier == default_action_identifier() {
        // A body click is the default action; it must never answer.
        return Some(NoticeAction::Open);
    }
    None
}

/// A stable identifier for one set of choice labels (per process).
fn choice_category_id(choices: &[String]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    choices.hash(&mut hasher);
    format!("agendar.c.{:016x}", hasher.finish())
}

fn open_action() -> Retained<UNNotificationAction> {
    UNNotificationAction::actionWithIdentifier_title_options(
        &NSString::from_str(NoticeAction::WIRE_OPEN),
        &NSString::from_str(OPEN_LABEL),
        UNNotificationActionOptions::Foreground,
    )
}

fn category(
    id: &str,
    actions: &[Retained<UNNotificationAction>],
) -> Retained<UNNotificationCategory> {
    let refs: Vec<&UNNotificationAction> = actions.iter().map(|action| &**action).collect();
    UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(id),
        &NSArray::from_slice(&refs),
        &NSArray::<NSString>::from_slice(&[]),
        UNNotificationCategoryOptions::empty(),
    )
}

/// Open plus one button per choice, titled with the choice's label.
fn choice_category(id: &str, choices: &[String]) -> Retained<UNNotificationCategory> {
    let mut actions = vec![open_action()];
    for (index, label) in choices.iter().enumerate() {
        let Some(wire) = NoticeAction::WIRE_CHOOSE.get(index) else {
            break;
        };
        actions.push(UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str(wire),
            &NSString::from_str(label),
            // Answering is a deliberate second step, never a foreground shortcut.
            UNNotificationActionOptions::AuthenticationRequired,
        ));
    }
    category(id, &actions)
}

fn create_delegate(ingress: Arc<Ingress>) -> Retained<NotifyDelegate> {
    let allocated = NotifyDelegate::alloc().set_ivars(DelegateIvars { ingress });
    // SAFETY: `-init` on a freshly allocated NSObject subclass whose ivars are
    // already installed; the allocated handle is consumed exactly once and the
    // returned object owns a +1 reference.
    unsafe { msg_send![super(allocated), init] }
}

/// Registers the Open-only category and returns it; choice categories are
/// added on demand by [`Engine::ensure_category`].
fn install_categories(center: &UNUserNotificationCenter) -> Retained<UNNotificationCategory> {
    let open_only = category(CATEGORY_OPEN, &[open_action()]);
    center.setNotificationCategories(&NSSet::from_slice(&[&*open_only]));
    open_only
}

fn request_authorization(center: &UNUserNotificationCenter) -> Arc<AtomicU8> {
    let authorization = Arc::new(AtomicU8::new(AUTH_UNKNOWN));
    let state = Arc::clone(&authorization);
    let completion: RcBlock<dyn Fn(Bool, *mut NSError)> =
        RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            let granted = granted.as_bool();
            // Panics stay inside the foreign callback; on panic the state simply
            // stays "unknown" instead of unwinding across the ABI.
            let _ = catch_unwind(AssertUnwindSafe(|| {
                state.store(
                    if granted { AUTH_GRANTED } else { AUTH_DENIED },
                    Ordering::Release,
                );
            }));
        });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &completion,
    );
    authorization
}

thread_local! {
    /// Main-thread-only notification centre plus the retained delegate.
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}

struct Engine {
    center: Retained<UNUserNotificationCenter>,
    /// The centre's delegate property is weak; this +1 reference keeps the
    /// delegate alive while it is registered.
    _delegate: Retained<NotifyDelegate>,
    /// Shown notices, oldest first.
    live: VecDeque<NoticeId>,
    /// The Open-only category, always registered.
    base: Retained<UNNotificationCategory>,
    /// Choice categories registered so far, oldest first.
    categories: Vec<(String, Retained<UNNotificationCategory>)>,
}

impl Engine {
    /// The category for these choices, registered when new. Registration is
    /// asynchronous, so a caller posting right away may see no buttons: call
    /// [`Transport::prepare`] a moment before `show`.
    fn ensure_category(&mut self, choices: &[String]) -> String {
        if choices.is_empty() {
            return CATEGORY_OPEN.to_owned();
        }
        let id = choice_category_id(choices);
        if !self.categories.iter().any(|(known, _)| *known == id) {
            self.categories
                .push((id.clone(), choice_category(&id, choices)));
            while self.categories.len() > MAX_CATEGORIES {
                self.categories.remove(0);
            }
            let mut all: Vec<&UNNotificationCategory> = vec![&*self.base];
            all.extend(self.categories.iter().map(|(_, category)| &**category));
            self.center
                .setNotificationCategories(&NSSet::from_slice(&all));
        }
        id
    }

    fn remove(&self, id: &NoticeId) {
        let identifiers = NSArray::from_slice(&[&*NSString::from_str(id.as_str())]);
        self.center
            .removePendingNotificationRequestsWithIdentifiers(&identifiers);
        self.center
            .removeDeliveredNotificationsWithIdentifiers(&identifiers);
    }
}

pub(crate) struct Backend {
    authorization: Arc<AtomicU8>,
    unavailable: Option<String>,
    closed: AtomicBool,
    /// Why macOS refused the last notification, cleared by the next success.
    delivery_error: Arc<Mutex<Option<String>>>,
}

impl Backend {
    fn unavailable(reason: &str) -> Self {
        Self {
            authorization: Arc::new(AtomicU8::new(AUTH_UNKNOWN)),
            unavailable: Some(reason.to_owned()),
            closed: AtomicBool::new(false),
            delivery_error: Arc::new(Mutex::new(None)),
        }
    }

    /// Shared precondition for `show`/`withdraw`: usable, permitted, open and on
    /// the thread that owns the platform centre.
    fn check_usable(&self) -> Result<(), NotifyError> {
        if let Some(reason) = &self.unavailable {
            return Err(NotifyError::Unavailable(reason.clone()));
        }
        if self.authorization.load(Ordering::Acquire) == AUTH_DENIED {
            return Err(NotifyError::Unavailable(
                "notification permission was denied".to_owned(),
            ));
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(NotifyError::Shutdown);
        }
        if MainThreadMarker::new().is_none() {
            return Err(NotifyError::Backend(MAIN_THREAD_ONLY.to_owned()));
        }
        Ok(())
    }
}

/// Starts the macOS backend. Never touches the centre when unbundled or off the
/// main thread.
pub(crate) fn start(_app_id: &str, ingress: Arc<Ingress>) -> Box<dyn Transport> {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return Box::new(Backend::unavailable(NOT_BUNDLED));
    }
    if MainThreadMarker::new().is_none() {
        return Box::new(Backend::unavailable(NOT_MAIN_THREAD));
    }

    let center = UNUserNotificationCenter::currentNotificationCenter();
    let base = install_categories(&center);
    let authorization = request_authorization(&center);
    let delegate = create_delegate(Arc::clone(&ingress));
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let engine = Engine {
        center,
        _delegate: delegate,
        live: VecDeque::new(),
        base,
        categories: Vec::new(),
    };
    // A second start replaces the previous registration; remove it first so no
    // stale delegate stays attached to an old centre.
    let previous = ENGINE.with(|slot| slot.borrow_mut().replace(engine));
    if let Some(previous) = previous {
        previous.center.setDelegate(None);
    }

    Box::new(Backend {
        authorization,
        unavailable: None,
        closed: AtomicBool::new(false),
        delivery_error: Arc::new(Mutex::new(None)),
    })
}

impl Transport for Backend {
    fn prepare(&self, notice: &Notice) {
        if self.check_usable().is_err() {
            return;
        }
        ENGINE.with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut()
                && let Some(engine) = slot.as_mut()
            {
                engine.ensure_category(&notice.choices);
            }
        });
    }

    fn last_error(&self) -> Option<String> {
        self.delivery_error
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
    }

    /// Ask macOS for the current permission; the user may change it in System
    /// Settings while Agendar runs. Main thread only, like the centre.
    fn refresh(&self) {
        if self.unavailable.is_some() || MainThreadMarker::new().is_none() {
            return;
        }
        let state = Arc::clone(&self.authorization);
        ENGINE.with(|slot| {
            let Ok(slot) = slot.try_borrow() else {
                return;
            };
            let Some(engine) = slot.as_ref() else {
                return;
            };
            let completion: RcBlock<dyn Fn(NonNull<UNNotificationSettings>)> =
                RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        // SAFETY: UserNotifications passes a valid settings
                        // object that stays alive for this callback.
                        let status = unsafe { settings.as_ref() }.authorizationStatus();
                        let value = if status == UNAuthorizationStatus::Denied {
                            AUTH_DENIED
                        } else if status == UNAuthorizationStatus::NotDetermined {
                            AUTH_UNKNOWN
                        } else {
                            AUTH_GRANTED
                        };
                        state.store(value, Ordering::Release);
                    }));
                });
            engine
                .center
                .getNotificationSettingsWithCompletionHandler(&completion);
        });
    }

    fn capability(&self) -> Capability {
        if let Some(reason) = &self.unavailable {
            return Capability::Unavailable(reason.clone());
        }
        if self.authorization.load(Ordering::Acquire) == AUTH_DENIED {
            return Capability::Unavailable("notification permission was denied".to_owned());
        }
        Capability::Available { actions: true }
    }

    fn show(&self, notice: &Notice) -> Result<(), NotifyError> {
        self.check_usable()?;
        ENGINE.with(|slot| {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return Err(NotifyError::Backend(
                    "notification centre is busy".to_owned(),
                ));
            };
            let Some(engine) = slot.as_mut() else {
                return Err(NotifyError::Backend(
                    "notification centre is not initialised".to_owned(),
                ));
            };
            if engine.live.contains(&notice.id) {
                return Err(NotifyError::Backend(
                    "a notification with this id is already shown".to_owned(),
                ));
            }
            // Clicked or dismissed notices never report back, so the oldest
            // one gives way instead of blocking every later notice.
            while engine.live.len() >= LIVE_LIMIT {
                if let Some(oldest) = engine.live.pop_front() {
                    engine.remove(&oldest);
                }
            }

            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&notice.title));
            if !notice.body.is_empty() {
                content.setBody(&NSString::from_str(&notice.body));
            }
            let category = engine.ensure_category(&notice.choices);
            content.setCategoryIdentifier(&NSString::from_str(&category));
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(notice.id.as_str()),
                &content,
                None,
            );
            let errors = Arc::clone(&self.delivery_error);
            let completion: RcBlock<dyn Fn(*mut NSError)> =
                RcBlock::new(move |error: *mut NSError| {
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        // SAFETY: UserNotifications passes either null or an
                        // NSError that stays valid for this callback.
                        let message = unsafe { error.as_ref() }
                            .map(|error| error.localizedDescription().to_string());
                        if let Ok(mut slot) = errors.lock() {
                            *slot = message;
                        }
                    }));
                });
            engine
                .center
                .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
            engine.live.push_back(notice.id.clone());
            Ok(())
        })
    }

    fn withdraw(&self, id: &NoticeId) -> Result<(), NotifyError> {
        if let Some(reason) = &self.unavailable {
            return Err(NotifyError::Unavailable(reason.clone()));
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(NotifyError::Shutdown);
        }
        if MainThreadMarker::new().is_none() {
            return Err(NotifyError::Backend(MAIN_THREAD_ONLY.to_owned()));
        }
        ENGINE.with(|slot| {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return Err(NotifyError::Backend(
                    "notification centre is busy".to_owned(),
                ));
            };
            let Some(engine) = slot.as_mut() else {
                return Err(NotifyError::Backend(
                    "notification centre is not initialised".to_owned(),
                ));
            };
            let Some(index) = engine.live.iter().position(|live| live == id) else {
                return Ok(());
            };
            engine.live.remove(index);
            engine.remove(id);
            Ok(())
        })
    }

    fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        if MainThreadMarker::new().is_none() {
            // The centre and delegate stay on their owning (main) thread and are
            // released when it ends; late callbacks are already ignored because
            // the ingress is closed before this runs.
            return;
        }
        let engine =
            ENGINE.with(|slot| slot.try_borrow_mut().ok().and_then(|mut slot| slot.take()));
        let Some(engine) = engine else {
            return;
        };
        if !engine.live.is_empty() {
            let identifiers: Vec<Retained<NSString>> = engine
                .live
                .iter()
                .map(|id| NSString::from_str(id.as_str()))
                .collect();
            let identifiers = NSArray::from_retained_slice(&identifiers);
            engine
                .center
                .removePendingNotificationRequestsWithIdentifiers(&identifiers);
            engine
                .center
                .removeDeliveredNotificationsWithIdentifiers(&identifiers);
        }
        // Remove the registration before the retained delegate is released.
        engine.center.setDelegate(None);
        drop(engine);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn action_identifiers_map_to_open_and_the_choices_only() {
        assert_eq!(
            action_from_identifier(NoticeAction::WIRE_OPEN),
            Some(NoticeAction::Open)
        );
        assert_eq!(
            action_from_identifier(NoticeAction::WIRE_CHOOSE[1]),
            Some(NoticeAction::Choose(1))
        );
        assert_eq!(
            action_from_identifier(&default_action_identifier()),
            Some(NoticeAction::Open),
            "a body click is the default action and must never answer"
        );
        assert_eq!(
            action_from_identifier("com.apple.UNNotificationDismissActionIdentifier"),
            None,
            "dismissal must not produce an action"
        );
        assert_eq!(action_from_identifier("bogus"), None);
    }

    #[test]
    fn each_set_of_choice_labels_gets_its_own_category() {
        let id = NoticeId::parse(ID).unwrap();
        let notice = crate::Notice::new(id, "title", "body")
            .unwrap()
            .with_choices(["JSON lines", "SQLite"])
            .unwrap();
        let first = choice_category_id(&notice.choices);
        assert_eq!(first, choice_category_id(&notice.choices), "stable");
        assert_ne!(first, CATEGORY_OPEN);
        assert_ne!(
            first,
            choice_category_id(&["JSON lines".to_owned()]),
            "different labels, different buttons"
        );
    }

    #[test]
    fn a_panicking_callback_step_is_contained_and_marks_the_notifier_degraded() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let ingress = Arc::new(Ingress::new(sender));

        assert!(contain_panic(&ingress, || {}));
        assert!(!ingress.is_degraded(), "a clean step changes nothing");

        let mut completion_calls = 0;
        assert!(!contain_panic(&ingress, || {
            completion_calls += 1;
            panic!("decoding a foreign payload must never escape");
        }));
        assert_eq!(completion_calls, 1, "the step runs exactly once");
        assert!(
            ingress.is_degraded(),
            "a contained panic is reported instead of unwinding into Objective-C"
        );
    }
}
