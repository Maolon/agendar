#![forbid(unsafe_code)]

//! Contract v1 is declared and built in full. `NOT_IMPLEMENTED` stays the
//! code of a call a later contract declares before it is built.

use agendar_core::error::details;
use agendar_core::model::Sha256;
use agendar_ffi::{FfiError, NOT_IMPLEMENTED, RefusalCode, SendBackDetail};

#[test]
fn the_code_of_an_unbuilt_call_is_stable() {
    assert_eq!(NOT_IMPLEMENTED, "E_NOT_IMPLEMENTED");
}

#[test]
fn refusal_codes_have_stable_names() {
    let names: Vec<&str> = [
        RefusalCode::NotSynced,
        RefusalCode::StaleRevision,
        RefusalCode::AlreadyAnswered,
        RefusalCode::Withdrawn,
        RefusalCode::SurfaceNotAllowed,
        RefusalCode::RefreshTimeout,
    ]
    .into_iter()
    .map(RefusalCode::as_str)
    .collect();
    assert_eq!(
        names,
        [
            "not_synced",
            "stale_revision",
            "already_answered",
            "withdrawn",
            "surface_not_allowed",
            "refresh_timeout",
        ]
    );
}

/// The first thirteen are the core's detail codes, in its spelling.
#[test]
fn send_back_details_have_stable_names() {
    let names: Vec<&str> = SendBackDetail::ALL
        .into_iter()
        .map(SendBackDetail::as_str)
        .collect();
    assert_eq!(
        names,
        [
            "send_back_requires_reason",
            "too_many_comments",
            "blank_string",
            "length",
            "control_character",
            "demo_out_of_range",
            "line_out_of_range",
            "region_out_of_range",
            "no_such_demo",
            "anchor_kind_mismatch",
            "line_not_in_diff",
            "line_not_in_transcript",
            "demo_not_text",
            "demo_not_on_device",
        ]
    );
}

/// `FfiError.Core` as the UDL declares it: code, detail, field, retryable,
/// message.
#[test]
fn a_core_error_has_the_fields_the_udl_declares() {
    let error = FfiError::Core {
        code: "E_VALIDATION".to_owned(),
        detail: Some("snooze_too_long".to_owned()),
        field: Some("/until".to_owned()),
        retryable: false,
        message: "A snooze can last at most seven days.".to_owned(),
    };
    assert_eq!(
        error.to_string(),
        "E_VALIDATION (snooze_too_long): A snooze can last at most seven days."
    );
    let udl = include_str!("../src/agendar.udl");
    assert!(udl.contains(
        "Core(string code, string? detail, string? field, boolean retryable, string message);"
    ));
}

/// The values of `Core.detail` are the core's list, which a mobile client can
/// mirror as a closed set. Adding a code is a contract change that clients
/// follow at their pace (an unknown code maps to "other"); renaming or removing
/// one breaks them. Either way this pin moves only on purpose.
#[test]
fn the_details_of_a_core_error_are_pinned() {
    let all: Vec<&str> = details::all().collect();
    assert_eq!(all.len(), details::CORE.len() + details::HOST.len());
    // Representative codes a mobile client switches on.
    for code in [
        "vault_root_missing",
        "vault_missing",
        "vault_symlink",
        "vault_manifest_not_file",
        "vault_manifest_too_large",
        "vault_manifest_malformed",
        "vault_manifest_missing_version",
        "vault_manifest_version",
        "vault_manifest_shape",
        "vault_manifest_not_on_device",
        "vault_unreadable",
        "local_data_unreadable",
        "identity_missing_version",
        "identity_version",
        "identity_malformed",
        "config_version",
        "lock_timeout",
        "invalid_event_id",
        "invalid_revision",
        "invalid_option_id",
        "invalid_project",
        "invalid_time",
        "unknown_option",
        "own_answer_missing",
        "own_answer_too_long",
        "echo_mismatch",
        "control_character",
        "not_a_rescue",
        "handoff_not_offered",
        "snooze_too_long",
        "snooze_default_near",
        "source_waiting",
        "source_missing",
    ] {
        assert_eq!(details::listed(code), Some(code));
    }
    assert_eq!(
        (
            all.len(),
            Sha256::from_bytes(all.join("\n").as_bytes()).as_str()
        ),
        (
            111,
            "04306c7bfc8f367bcc56114d779a990437db8f2dcc07362830aa81dea5ee6d44"
        ),
        "the list of details changed: update mobile client bindings, then move this pin"
    );
}
