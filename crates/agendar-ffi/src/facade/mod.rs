//! The hand-written facade: owned, typed records over one `Service`.
//!
//! One file per group of calls, so each can change without touching the
//! others: `read` (refresh, Now, projects) with `activity` (the project page's
//! feed, numbers and deliveries), `card`, `write` (answer, rescue, receipt),
//! `delivery` with `demo` (read one delivery) and `review` (its verdict),
//! `snooze`, `open` (the constructor and whose fault a failed open is), and
//! `spike` (the early probe calls). `types` and `review_types` hold
//! the contract v1 records and enums; they mirror `agendar.udl` field for
//! field.
#![forbid(unsafe_code)]

use std::fmt;

use agendar_core::AgendarError;
use agendar_core::error::details;
use agendar_core::service::Service;

mod activity;
mod card;
mod delivery;
mod demo;
mod open;
mod read;
mod review;
mod review_types;
mod shared;
mod snooze;
mod spike;
mod types;
mod write;

pub use crate::profile::ColdSplit;
pub use demo::{LINE_MAX_CHARS, TEXT_MAX_BYTES, TEXT_MAX_LINES};
pub use review_types::*;
pub use spike::{Need, ProjectSummary, SnapshotSummary, VaultHealth};
pub use types::*;

/// The code of a call that a contract declares before it is built.
pub const NOT_IMPLEMENTED: &str = "E_NOT_IMPLEMENTED";

/// Why the core refused a request without writing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefusalCode {
    /// This device has not read enough of the Vault to judge the request.
    NotSynced,
    /// The card or row changed since the caller read it.
    StaleRevision,
    /// The question already has an answer.
    AlreadyAnswered,
    /// The question was withdrawn or replaced by a newer one.
    Withdrawn,
    /// The guard does not allow an answer from this surface.
    SurfaceNotAllowed,
    /// The bounded refresh before the check did not finish in time.
    RefreshTimeout,
}

impl RefusalCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotSynced => "not_synced",
            Self::StaleRevision => "stale_revision",
            Self::AlreadyAnswered => "already_answered",
            Self::Withdrawn => "withdrawn",
            Self::SurfaceNotAllowed => "surface_not_allowed",
            Self::RefreshTimeout => "refresh_timeout",
        }
    }
}

/// What is wrong with the input of a send-back (`FfiError::SendBackRefused`).
/// `as_str` is the core's own detail code for the same refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SendBackDetail {
    /// Neither a reason nor a comment.
    SendBackRequiresReason,
    TooManyComments,
    /// A text that is blank: the comment's, or the file of a diff line.
    BlankString,
    /// A text that is too long: the reason's when no comment is named.
    Length,
    ControlCharacter,
    DemoOutOfRange,
    LineOutOfRange,
    RegionOutOfRange,
    NoSuchDemo,
    AnchorKindMismatch,
    LineNotInDiff,
    LineNotInTranscript,
    DemoNotText,
    /// The demo file a line comment points into is not on this device yet.
    DemoNotOnDevice,
}

impl SendBackDetail {
    pub const ALL: [Self; 14] = [
        Self::SendBackRequiresReason,
        Self::TooManyComments,
        Self::BlankString,
        Self::Length,
        Self::ControlCharacter,
        Self::DemoOutOfRange,
        Self::LineOutOfRange,
        Self::RegionOutOfRange,
        Self::NoSuchDemo,
        Self::AnchorKindMismatch,
        Self::LineNotInDiff,
        Self::LineNotInTranscript,
        Self::DemoNotText,
        Self::DemoNotOnDevice,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SendBackRequiresReason => "send_back_requires_reason",
            Self::TooManyComments => "too_many_comments",
            Self::BlankString => "blank_string",
            Self::Length => "length",
            Self::ControlCharacter => "control_character",
            Self::DemoOutOfRange => "demo_out_of_range",
            Self::LineOutOfRange => "line_out_of_range",
            Self::RegionOutOfRange => "region_out_of_range",
            Self::NoSuchDemo => "no_such_demo",
            Self::AnchorKindMismatch => "anchor_kind_mismatch",
            Self::LineNotInDiff => "line_not_in_diff",
            Self::LineNotInTranscript => "line_not_in_transcript",
            Self::DemoNotText => "demo_not_text",
            Self::DemoNotOnDevice => "demo_not_on_device",
        }
    }

    /// The detail a core validation error carries in its `detail` field.
    pub(crate) fn of(error: &AgendarError) -> Option<Self> {
        let code = error.detail()?;
        Self::ALL.into_iter().find(|detail| detail.as_str() == code)
    }
}

/// A typed core error for generated bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfiError {
    /// `code` is the stable public error code (`E_CONTEXT`, `E_NOT_FOUND`, ...).
    /// `detail` names the rule that failed, one of the codes the core lists
    /// in `agendar_core::error::details`; `field` is the JSON Pointer of the
    /// input or file at fault; `retryable` is true when the same call can
    /// succeed later without a change. `message` is an English sentence for
    /// logs.
    Core {
        code: String,
        detail: Option<String>,
        field: Option<String>,
        retryable: bool,
        message: String,
    },
    /// A checked request the core refused; nothing was written.
    Refused { code: RefusalCode, message: String },
    /// `review` refused the input of a send-back; nothing was written.
    /// `comment` is the 1-based number of the comment at fault, absent when
    /// the refusal is about the reason or the comments as a whole.
    SendBackRefused {
        detail: SendBackDetail,
        comment: Option<u32>,
        message: String,
    },
}

impl FfiError {
    /// A refusal with its sentence for the owner.
    #[allow(dead_code)] // Constructed by the write and read modules.
    pub(crate) fn refused(code: RefusalCode, message: impl Into<String>) -> Self {
        Self::Refused {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for FfiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core {
                code,
                detail,
                message,
                ..
            } => match detail {
                Some(detail) => write!(formatter, "{code} ({detail}): {message}"),
                None => write!(formatter, "{code}: {message}"),
            },
            Self::Refused { code, message } => {
                write!(formatter, "refused ({}): {message}", code.as_str())
            }
            Self::SendBackRefused {
                detail, message, ..
            } => write!(
                formatter,
                "send-back refused ({}): {message}",
                detail.as_str()
            ),
        }
    }
}

impl std::error::Error for FfiError {}

impl From<AgendarError> for FfiError {
    /// The core's error, field for field. Only a detail the contract lists
    /// (`details::listed`) is a `detail`: the other end mirrors that list as
    /// a closed set. One it does not list stays in the message for the log,
    /// in brackets as before.
    fn from(error: AgendarError) -> Self {
        let detail = error.detail().and_then(details::listed);
        let message = match (error.detail(), detail) {
            (Some(unlisted), None) => format!("{} [{unlisted}]", error.message()),
            _ => error.message().to_owned(),
        };
        Self::Core {
            code: error.code().as_str().to_owned(),
            detail: detail.map(str::to_owned),
            field: error.field().map(str::to_owned),
            retryable: error.retryable(),
            message,
        }
    }
}

/// The core version this library was built from.
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// One opened Vault plus this device's local data root.
pub struct Session {
    service: Service,
}

#[cfg(test)]
mod tests {
    use agendar_core::{AgendarError, ErrorCode};

    use super::FfiError;

    #[test]
    fn a_core_error_crosses_field_for_field() {
        let error = AgendarError::validation_at("/until", "snooze_too_long", "Too long.")
            .with_hint("Shorter.")
            .with_retryable(true);
        assert_eq!(
            FfiError::from(error),
            FfiError::Core {
                code: "E_VALIDATION".to_owned(),
                detail: Some("snooze_too_long".to_owned()),
                field: Some("/until".to_owned()),
                retryable: true,
                message: "Too long.".to_owned(),
            }
        );
        let bare = FfiError::from(AgendarError::new(ErrorCode::Io, "No."));
        assert_eq!(
            bare,
            FfiError::Core {
                code: "E_IO".to_owned(),
                detail: None,
                field: None,
                retryable: false,
                message: "No.".to_owned(),
            }
        );
    }

    /// The closed set: a detail the contract does not list never crosses as
    /// one, whatever the core put there.
    #[test]
    fn a_detail_the_contract_does_not_list_stays_in_the_message() {
        for unlisted in ["sqlite: disk I/O error", "check_spawn", "Not A Code"] {
            let error = AgendarError::new(ErrorCode::Index, "Cannot read.").with_detail(unlisted);
            let FfiError::Core {
                detail, message, ..
            } = FfiError::from(error)
            else {
                panic!("a core error");
            };
            assert_eq!(detail, None, "{unlisted}");
            assert_eq!(message, format!("Cannot read. [{unlisted}]"));
        }
    }
}
