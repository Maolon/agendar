//! The detail codes a host hands on as part of its contract.
//!
//! An [`AgendarError`](super::AgendarError) names the rule that failed in
//! `detail`, a short code such as `vault_manifest_malformed`. The core has a
//! few hundred of them and most belong to an agent's own request. The ones
//! listed here can reach a host that shows the owner's side (such as a mobile
//! client): a
//! host passes a detail on as a field only when [`listed`] knows it, so the
//! other end can mirror the list as a closed set. A code is never renamed or
//! reused once it is here; new ones are added.
//!
//! [`CORE`] are raised by this crate, each written at its check. [`HOST`]
//! are set by a host at its own boundary, where the core gives no detail (a
//! plain file error, an id that does not parse).

/// `vault.json` is still a placeholder of the sync app: its content is not
/// on this device. Retry when it has downloaded.
pub const VAULT_MANIFEST_NOT_ON_DEVICE: &str = "vault_manifest_not_on_device";
/// The Vault folder or `vault.json` could not be read, and the core cannot
/// tell why (no access, offline, a failing disk).
pub const VAULT_UNREADABLE: &str = "vault_unreadable";
/// A file or folder in this device's own data directory could not be read or
/// written, and the core cannot tell why.
pub const LOCAL_DATA_UNREADABLE: &str = "local_data_unreadable";
/// An event id (an ask, an answer, a delivery) that is not one.
pub const INVALID_EVENT_ID: &str = "invalid_event_id";
/// An `expected_revision` that is not a revision.
pub const INVALID_REVISION: &str = "invalid_revision";
/// An option id that is not one.
pub const INVALID_OPTION_ID: &str = "invalid_option_id";
/// A project slug that is not one.
pub const INVALID_PROJECT: &str = "invalid_project";
/// A time that is not an RFC 3339 UTC time.
pub const INVALID_TIME: &str = "invalid_time";
/// An id this device has not read.
pub const NOT_FOUND: &str = "not_found";
/// An own answer longer than the card allows.
pub const OWN_ANSWER_TOO_LONG: &str = "own_answer_too_long";
/// The echoed label is not the chosen option's.
pub const ECHO_MISMATCH: &str = "echo_mismatch";
/// The card's guard needs the chosen option's label echoed.
pub const ECHO_MISSING: &str = "echo_missing";
/// A hand-off on a rescue row that has nobody to hand the step to.
pub const HANDOFF_NOT_OFFERED: &str = "handoff_not_offered";

/// Details a host sets at its own boundary.
pub const HOST: &[&str] = &[
    VAULT_MANIFEST_NOT_ON_DEVICE,
    VAULT_UNREADABLE,
    LOCAL_DATA_UNREADABLE,
    INVALID_EVENT_ID,
    INVALID_REVISION,
    INVALID_OPTION_ID,
    INVALID_PROJECT,
    INVALID_TIME,
    NOT_FOUND,
    OWN_ANSWER_TOO_LONG,
    ECHO_MISMATCH,
    ECHO_MISSING,
    HANDOFF_NOT_OFFERED,
];

/// Details this crate raises, in the spelling of the check that raises them.
pub const CORE: &[&str] = &[
    // The Vault folder and its manifest (`vault/paths.rs`).
    "vault_root_missing",
    "vault_missing",
    "vault_symlink",
    "vault_manifest_not_file",
    "vault_manifest_too_large",
    "vault_manifest_malformed",
    "vault_manifest_missing_version",
    "vault_manifest_version",
    "vault_manifest_shape",
    "missing_directory",
    "vault_path",
    "vault_path_not_directory",
    "vault_escape",
    // This device's data directory (`runtime/paths.rs`, `runtime/local_state.rs`).
    "empty_path",
    "invalid_path",
    "not_a_directory",
    "vault_data_separation",
    "local_state_outside_data_root",
    "local_state_not_directory",
    "local_state_path",
    "local_state_symlink",
    "local_state_not_file",
    "local_state_too_large",
    "local_state_malformed",
    "local_state_alias",
    "local_state_serialize",
    "local_state_unsupported",
    "lock_timeout",
    // This device's identity, config and runtime journal.
    "missing_identity",
    "identity_missing_version",
    "identity_version",
    "identity_malformed",
    "config_missing_version",
    "config_version",
    "config_malformed",
    "runtime_missing_version",
    "runtime_version",
    "runtime_malformed",
    // The local index (`index/`).
    "index_alias",
    "index_digest",
    "index_id",
    "index_inside_vault",
    "index_leaf_unstable",
    "index_path",
    "index_slug",
    "index_symlink",
    "integrity_check",
    "foreign_key_check",
    "schema_incomplete",
    "schema_version",
    "schema_version_unsupported",
    "vault_id_mismatch",
    "cached_event_missing",
    "projection_decode",
    "policy_projection",
    // Reading writer files and demo files (`index/ingest.rs`, `vault/reader.rs`,
    // `vault/blobs.rs`).
    "source_waiting",
    "source_missing",
    "source_not_file",
    "source_shrunk",
    "invalid_source_path",
    "batch_limit",
    "blob_missing",
    "blob_pending_digest",
    "blob_pending_length",
    // Appending this device's events (`vault/writer.rs`).
    "vault_io",
    "pending_bytes_unreadable",
    "writer_journal_malformed",
    "writer_journal_version",
    "writer_journal_mismatch",
    "writer_sealed",
    "event_line_too_long",
    "id_exhausted",
    "seq_exhausted",
    // What the owner asks for: an answer, a rescue, a verdict, a snooze.
    "project_not_found",
    "decision_not_found",
    "decision_withdrawn",
    "answer_not_found",
    "unknown_option",
    "own_answer_missing",
    "own_answer_with_option",
    "done_when_count",
    "not_a_rescue",
    "rescue_without_node",
    "stale_rescue",
    "feature_not_found",
    "delivery_not_found",
    "delivery_superseded",
    "send_back_requires_reason",
    "too_many_comments",
    "snooze_too_long",
    "snooze_default_near",
    "need_not_waiting",
    // The text and place rules of an event the owner writes (`model/validate.rs`).
    "blank_string",
    "length",
    "control_character",
    "demo_out_of_range",
    "line_out_of_range",
    "region_out_of_range",
];

/// The listed code equal to `detail`, when the contract has it.
pub fn listed(detail: &str) -> Option<&'static str> {
    all().find(|code| *code == detail)
}

/// Every listed code: [`CORE`], then [`HOST`].
pub fn all() -> impl Iterator<Item = &'static str> {
    CORE.iter().chain(HOST).copied()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{CORE, HOST, all, listed};

    #[test]
    fn every_code_is_listed_once_in_one_spelling() {
        let mut seen = BTreeSet::new();
        for code in all() {
            assert!(seen.insert(code), "{code} is listed twice");
            assert!(
                code.starts_with(|first: char| first.is_ascii_lowercase())
                    && code
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{code} is not a snake_case code"
            );
            assert_eq!(listed(code), Some(code));
        }
        assert_eq!(seen.len(), CORE.len() + HOST.len());
        assert_eq!(listed("sqlite: disk I/O error"), None);
        assert_eq!(listed(""), None);
    }
}
