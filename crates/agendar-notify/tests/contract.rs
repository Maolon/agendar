//! Contract tests for the safe notification API.
//!
//! Everything here runs against the in-memory fake transport: no notification
//! centre, no D-Bus and no WinRT apartment is touched. The tests pin the rules
//! the app relies on — identifier/argument validation, the bounded ingress and
//! its degraded state, ignore-after-shutdown, and the rule that an approve
//! action only exists when a notice asks for it.

use std::sync::mpsc::TryRecvError;

use agendar_notify::{
    Capability, EVENT_CAPACITY, FakeCall, MAX_ACTION_LABEL_CHARS, MAX_APP_ID_CHARS, MAX_BODY_CHARS,
    MAX_TITLE_CHARS, Notice, NoticeAction, NoticeEvent, NoticeId, Notifier, NotifyError,
    parse_arguments, start_fake,
};

const ID: &str = "0123456789abcdef0123456789abcdef";
const OTHER_ID: &str = "fedcba9876543210fedcba9876543210";
const APP_ID: &str = "dev.agendar.app";

fn id() -> NoticeId {
    NoticeId::parse(ID).expect("fixture id is valid")
}

fn other_id() -> NoticeId {
    NoticeId::parse(OTHER_ID).expect("fixture id is valid")
}

fn notice() -> Notice {
    Notice::new(id(), "Slice ready", "Open to review").expect("fixture notice is valid")
}

#[test]
fn notice_ids_are_thirty_two_lowercase_hex() {
    assert_eq!(id().as_str(), ID);
    assert_eq!(NoticeId::new(ID.to_owned()).unwrap(), id());

    let rejected = [
        String::new(),
        ID[..31].to_owned(),
        format!("{ID}0"),
        ID.to_uppercase(),
        "0123456789abcdef0123456789abcdeg".to_owned(),
        " 123456789abcdef0123456789abcdef".to_owned(),
        format!("{ID}&action=open"),
    ];
    for candidate in rejected {
        assert_eq!(
            NoticeId::parse(&candidate),
            Err(NotifyError::InvalidId),
            "accepted malformed id {candidate:?}"
        );
    }
}

#[test]
fn action_identifiers_are_only_open_and_the_choices() {
    assert_eq!(NoticeAction::parse("open"), Some(NoticeAction::Open));
    assert_eq!(
        NoticeAction::parse("choose-0"),
        Some(NoticeAction::Choose(0))
    );
    assert_eq!(
        NoticeAction::parse("choose-2"),
        Some(NoticeAction::Choose(2))
    );
    assert_eq!(
        NoticeAction::parse("approve"),
        Some(NoticeAction::Choose(0)),
        "the legacy approve id is the first choice"
    );
    assert_eq!(NoticeAction::Open.wire(), "open");
    assert_eq!(NoticeAction::Choose(1).wire(), "choose-1");

    for rejected in [
        "",
        "Open",
        "OPEN",
        "Approve",
        "default",
        "com.apple.UNNotificationDefaultActionIdentifier",
        "com.apple.UNNotificationDismissActionIdentifier",
        "reply",
        "open ",
        "approve-recommended",
        "approve%00",
        "choose-3",
        "choose-",
        "choose-01",
    ] {
        assert_eq!(
            NoticeAction::parse(rejected),
            None,
            "accepted malformed action {rejected:?}"
        );
    }
}

#[test]
fn toast_arguments_are_the_frozen_grammar() {
    assert_eq!(
        NoticeAction::Open.arguments(&id()),
        format!("v=1&notice={ID}&action=open")
    );
    assert_eq!(
        NoticeAction::Choose(1).arguments(&id()),
        format!("v=1&notice={ID}&action=choose-1")
    );

    for action in [
        NoticeAction::Open,
        NoticeAction::Choose(0),
        NoticeAction::Choose(2),
    ] {
        assert_eq!(
            parse_arguments(&action.arguments(&id())),
            Ok(NoticeEvent { id: id(), action })
        );
    }

    let malformed = [
        String::new(),
        format!("v=2&notice={ID}&action=open"),
        format!("v=1&notice={ID}"),
        format!("v=1&notice={ID}&action=open&extra=1"),
        format!("v=1&action=open&notice={ID}"),
        format!("v=1&notice={}&action=open", ID.to_uppercase()),
        format!("v=1&notice={}&action=Open", ID),
        format!("v=1&notice={}&action=default", ID),
        format!("v=1&notice={}&action=", ID),
        format!("v=1&notice={}&action=open&", ID),
        format!("v=1&notice={}{ID}&action=open", 'a'),
        "v=1&notice=&action=open".to_owned(),
        format!("1&notice={ID}&action=open"),
        format!("v=1&notice={ID}&action=approve%20"),
        format!("v=1&notice={ID}&action=approve&action=open"),
    ];
    for candidate in malformed {
        assert_eq!(
            parse_arguments(&candidate),
            Err(NotifyError::InvalidArguments),
            "accepted malformed arguments {candidate:?}"
        );
    }
}

#[test]
fn notices_are_bounded_and_validated_again_on_show() {
    assert!(Notice::new(id(), "", "body").is_err());
    assert!(Notice::new(id(), "title\n", "body").is_err());
    assert!(Notice::new(id(), "t".repeat(MAX_TITLE_CHARS + 1), "body").is_err());
    assert!(Notice::new(id(), "title", "b".repeat(MAX_BODY_CHARS + 1)).is_err());
    assert!(Notice::new(id(), "title", "body\u{0}").is_err());
    assert!(Notice::new(id(), "title", "line\nbreak\ttab").is_ok());
    assert!(notice().with_choices([""]).is_err());
    assert!(
        notice()
            .with_choices(["a".repeat(MAX_ACTION_LABEL_CHARS + 1)])
            .is_err()
    );
    assert!(
        notice().with_choices(["A", "B", "C", "D"]).is_err(),
        "at most three"
    );
    assert!(notice().with_choices(["JSON lines", "SQLite"]).is_ok());

    // The fields are public, so `show` must not trust the caller.
    let (notifier, _receiver, backend) = start_fake(APP_ID).unwrap();
    let mut unchecked = notice();
    unchecked.title = "bad\ntitle".to_owned();
    assert!(matches!(
        notifier.show(unchecked),
        Err(NotifyError::InvalidNotice(_))
    ));
    assert!(backend.calls().is_empty());
}

#[test]
fn delivered_actions_become_events() {
    let (notifier, receiver, backend) = start_fake(APP_ID).unwrap();
    assert_eq!(
        notifier.capability(),
        Capability::Available { actions: true }
    );

    backend.deliver(&id(), NoticeAction::Open);
    backend.deliver(&other_id(), NoticeAction::Choose(0));

    assert_eq!(
        receiver.try_recv(),
        Ok(NoticeEvent {
            id: id(),
            action: NoticeAction::Open
        })
    );
    assert_eq!(
        receiver.try_recv(),
        Ok(NoticeEvent {
            id: other_id(),
            action: NoticeAction::Choose(0)
        })
    );
    assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
    assert!(!notifier.is_degraded());
}

#[test]
fn full_queue_drops_events_and_marks_the_notifier_degraded() {
    let (notifier, receiver, backend) = start_fake(APP_ID).unwrap();
    for _ in 0..=EVENT_CAPACITY {
        backend.deliver(&id(), NoticeAction::Open);
    }

    let mut delivered = 0;
    while receiver.try_recv().is_ok() {
        delivered += 1;
    }
    assert_eq!(
        delivered, EVENT_CAPACITY,
        "the bounded queue must keep exactly its capacity, never block a callback"
    );
    assert!(notifier.is_degraded());
    assert!(
        !notifier.capability().is_available(),
        "a degraded notifier cannot promise action delivery"
    );
    assert!(matches!(
        notifier.show(notice()),
        Err(NotifyError::Unavailable(_))
    ));
}

#[test]
fn callbacks_after_shutdown_are_ignored() {
    let (notifier, receiver, backend) = start_fake(APP_ID).unwrap();
    notifier.shutdown();

    backend.deliver(&id(), NoticeAction::Choose(0));
    backend.deliver_arguments(&NoticeAction::Open.arguments(&id()));
    assert_eq!(
        receiver.try_recv(),
        Err(TryRecvError::Empty),
        "callbacks after shutdown must not produce events"
    );
    assert!(backend.calls().contains(&FakeCall::Shutdown));
}

#[test]
fn choice_buttons_exist_only_when_the_notice_carries_them() {
    let (notifier, _receiver, backend) = start_fake(APP_ID).unwrap();
    notifier.show(notice()).unwrap();
    let with_choices = notice().with_choices(["JSON lines", "SQLite"]).unwrap();
    assert!(with_choices.offers(NoticeAction::Choose(1)));
    assert!(!with_choices.offers(NoticeAction::Choose(2)));
    notifier.show(with_choices.clone()).unwrap();

    let calls = backend.calls();
    assert_eq!(
        calls.first(),
        Some(&FakeCall::Show {
            notice: notice(),
            choices: Vec::new()
        })
    );
    assert_eq!(
        calls.get(1),
        Some(&FakeCall::Show {
            notice: with_choices,
            choices: vec!["JSON lines".to_owned(), "SQLite".to_owned()]
        })
    );
}

#[test]
fn withdraw_removes_the_notice_and_is_idempotent() {
    let (notifier, _receiver, backend) = start_fake(APP_ID).unwrap();
    notifier.show(notice()).unwrap();
    assert_eq!(backend.shown(), vec![notice()]);

    notifier.withdraw(&id()).unwrap();
    notifier.withdraw(&other_id()).unwrap();
    assert!(backend.shown().is_empty());
    assert_eq!(
        backend.calls().last(),
        Some(&FakeCall::Withdraw(other_id()))
    );
}

#[test]
fn malformed_os_payloads_never_become_events() {
    let (notifier, receiver, backend) = start_fake(APP_ID).unwrap();
    backend.deliver_arguments(&format!("v=1&notice={}&action=approve", ID.to_uppercase()));
    backend.deliver_arguments(&format!("v=1&notice={ID}&action=default"));
    backend.deliver_arguments("");
    assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));

    backend.deliver_arguments(&format!("v=1&notice={ID}&action=choose-1"));
    assert_eq!(
        receiver.try_recv(),
        Ok(NoticeEvent {
            id: id(),
            action: NoticeAction::Choose(1)
        })
    );
    assert!(!notifier.is_degraded());
}

#[test]
fn unavailable_capability_is_reported_and_blocks_show() {
    let (notifier, _receiver, backend) = start_fake(APP_ID).unwrap();
    backend.set_capability(Capability::Unavailable("no daemon".to_owned()));

    assert_eq!(
        notifier.capability(),
        Capability::Unavailable("no daemon".to_owned())
    );
    assert_eq!(
        notifier.show(notice()),
        Err(NotifyError::Unavailable("no daemon".to_owned()))
    );
    assert!(backend.calls().is_empty());
}

#[test]
fn invalid_app_id_is_rejected_before_any_backend_runs() {
    assert!(matches!(
        Notifier::start(""),
        Err(NotifyError::InvalidAppId)
    ));
    assert!(matches!(
        Notifier::start(&"a".repeat(MAX_APP_ID_CHARS + 1)),
        Err(NotifyError::InvalidAppId)
    ));
    assert!(matches!(
        Notifier::start("dev\nagendar"),
        Err(NotifyError::InvalidAppId)
    ));
    assert!(matches!(start_fake(""), Err(NotifyError::InvalidAppId)));
}

#[test]
fn notifier_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Notifier>();
    assert_send_sync::<NotifyError>();
    assert_send_sync::<NoticeEvent>();
}
