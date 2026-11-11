//! The verdict on a delivery: accept, or send back with the owner's words
//! and comments pointing into the demos.
//!
//! The desktop's `review_delivery`, by the same steps: refresh, the checks
//! every host shares (`guard::check_review`), the comments checked against
//! the demos with their quotes read from the demo files
//! (`anchors::resolve_comments`), then one `review.result` by this host's
//! operator through the service call the desktop uses. A verdict that does
//! not pass is refused and nothing is written.
#![forbid(unsafe_code)]

use std::sync::PoisonError;

use agendar_core::anchors::{AnchorDenial, AnchorError, resolve_comments};
use agendar_core::guard::{ReviewDenial, check_review};
use agendar_core::model::{
    DiffSide, MAX_REVIEW_COMMENTS, ReviewAnchor, ReviewComment, ReviewVerdict,
    validate_review_comment,
};
use agendar_core::vault::{Clock, SystemClock};
use agendar_core::{AgendarError, ErrorCode};

use super::activity::review_state;
use super::shared::{event_id, revision};
use super::write::{WRITES, find_event, next_need};
use super::{
    CommentPlace, FfiError, NewComment, RefusalCode, ReviewReceipt, SendBackDetail, Session,
    Verdict,
};

impl Session {
    /// Accept a delivery or send it back, bound to the revision the caller
    /// read. A delivery that changed since (a verdict from any host is such a
    /// change), was superseded, or answers a decision that was answered anew
    /// is `Refused(StaleRevision)`; one this device has not read is
    /// `Refused(NotSynced)`. A send-back whose reason or comments do not pass
    /// is `SendBackRefused`, with what is wrong and the comment at fault.
    pub fn review(
        &self,
        delivery_id: String,
        expected_revision: String,
        verdict: Verdict,
    ) -> Result<ReviewReceipt, FfiError> {
        let id = event_id("/delivery_id", &delivery_id)?;
        let expected = revision(&expected_revision)?;
        let (verdict, reason, mut comments) = match verdict {
            Verdict::Accept => (ReviewVerdict::Accept, None, Vec::new()),
            Verdict::SendBack { reason, comments } => {
                // As the desktop: a blank reason is no reason.
                let reason = reason
                    .map(|reason| reason.trim().to_owned())
                    .filter(|reason| !reason.is_empty());
                let comments = new_comments(comments)?;
                if reason.is_none() && comments.is_empty() {
                    return Err(send_back_refused(
                        SendBackDetail::SendBackRequiresReason,
                        None,
                        "Say what to change, or comment on a demo, before sending a delivery back.",
                    ));
                }
                (ReviewVerdict::SendBack, reason, comments)
            }
        };
        let _writes = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
        let fresh = self.write_refresh()?;
        let checked = check_review(&fresh.state, &id, &expected).map_err(denied)?;
        self.refuse_while_waiting(&fresh, checked.slug)?;
        resolve_comments(
            self.service.vault(),
            checked.slug,
            &checked.delivery.demos,
            &mut comments,
        )
        .map_err(misplaced)?;
        let sends_back = verdict == ReviewVerdict::SendBack;
        let (review, _) = self
            .service
            .review_delivery_events(
                checked.slug,
                &checked.delivery.node,
                &id,
                verdict,
                reason,
                None,
                comments,
                checked.causes,
            )
            .map_err(|error| unwritten(sends_back, error))?;
        let after = self.service.refresh(SystemClock.now(), false)?;
        let delivery = after
            .state
            .projects
            .get(&review.project)
            .and_then(|project| project.deliveries.get(&id))
            .ok_or_else(|| {
                AgendarError::new(
                    ErrorCode::Index,
                    "The reviewed delivery is not in the index.",
                )
            })?;
        let written = find_event(&after, &review.id).unwrap_or(&review);
        Ok(ReviewReceipt {
            review_id: review.id.as_str().to_owned(),
            delivery_id,
            project: review.project.as_str().to_owned(),
            review: review_state(delivery.review),
            written_at: written.ts.to_string(),
            writer_file: self.writer_file(&after, written)?,
            next_need_id: next_need(&after, &id),
        })
    }
}

/// What the shared check said, as the contract spells it.
fn denied(denial: ReviewDenial) -> FfiError {
    let (code, message) = match denial {
        ReviewDenial::NotFound => (
            RefusalCode::NotSynced,
            "This device has not read this delivery yet. Try again when the Vault has synced.",
        ),
        ReviewDenial::Changed => (
            RefusalCode::StaleRevision,
            "This delivery changed after you opened it. Read it again before reviewing.",
        ),
        ReviewDenial::Superseded => (
            RefusalCode::StaleRevision,
            "A newer delivery replaced this one. Review the current delivery.",
        ),
        ReviewDenial::ReceiptNotCurrent => (
            RefusalCode::StaleRevision,
            "The answer this delivery was made for is no longer the current one.",
        ),
    };
    FfiError::refused(code, message)
}

/// A refusal of the send-back's input. The message is the one this refusal
/// had as `Core("E_VALIDATION", ...)`: the sentence, then the detail code.
fn send_back_refused(detail: SendBackDetail, comment: Option<usize>, sentence: &str) -> FfiError {
    FfiError::SendBackRefused {
        detail,
        comment: comment.map(number),
        message: format!("{sentence} [{}]", detail.as_str()),
    }
}

/// The 1-based number a comment is read back with (`RoundComment.n`).
fn number(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

/// A refusal about comment `index`, its sentence led by the comment's number.
fn comment_refused(index: usize, detail: SendBackDetail, sentence: &str) -> FfiError {
    send_back_refused(
        detail,
        Some(index),
        &format!("Comment {}: {sentence}", index + 1),
    )
}

/// The core's refusal of the event at the append. The comments passed their
/// checks before, so a validation error on `/reason` is about the owner's
/// words (too long, a NUL).
fn unwritten(sends_back: bool, error: AgendarError) -> FfiError {
    let about_reason = sends_back
        && error.code() == ErrorCode::Validation
        && error
            .field()
            .is_some_and(|field| field.ends_with("/reason"));
    match SendBackDetail::of(&error) {
        Some(detail) if about_reason => send_back_refused(detail, None, error.message()),
        _ => error.into(),
    }
}

/// The comments of a send-back as the core writes them, each checked on its
/// own before anything is read. A line's quote is left empty here: the core
/// fills it from the demo file, whatever the caller sent.
fn new_comments(comments: Vec<NewComment>) -> Result<Vec<ReviewComment>, FfiError> {
    if comments.len() > MAX_REVIEW_COMMENTS {
        return Err(send_back_refused(
            SendBackDetail::TooManyComments,
            None,
            &format!("A review carries at most {MAX_REVIEW_COMMENTS} comments."),
        ));
    }
    comments
        .into_iter()
        .enumerate()
        .map(|(index, comment)| {
            let anchor = match comment.place {
                CommentPlace::DiffLine {
                    demo,
                    file,
                    old_side,
                    line,
                    ..
                } => ReviewAnchor::DiffLine {
                    demo,
                    file,
                    side: if old_side {
                        DiffSide::Old
                    } else {
                        DiffSide::New
                    },
                    line,
                    quote: String::new(),
                },
                CommentPlace::TranscriptLine { demo, line, .. } => ReviewAnchor::TranscriptLine {
                    demo,
                    line,
                    quote: String::new(),
                },
                CommentPlace::Region { demo, x, y, w, h } => {
                    ReviewAnchor::Region { demo, x, y, w, h }
                }
            };
            let comment = ReviewComment {
                anchor,
                text: comment.text,
            };
            validate_review_comment(&comment, &format!("/comments/{index}")).map_err(|error| {
                // The core names every refusal of one comment by a detail
                // code this contract lists; one it does not is passed on.
                match SendBackDetail::of(&error) {
                    Some(detail) => comment_refused(index, detail, error.message()),
                    None => error.into(),
                }
            })?;
            Ok(comment)
        })
        .collect()
}

/// A comment that does not fit the delivery, as the contract spells it.
fn misplaced(error: AnchorError) -> FfiError {
    let index = error.index;
    let detail = match &error.denial {
        AnchorDenial::NoDemo => SendBackDetail::NoSuchDemo,
        AnchorDenial::KindMismatch => SendBackDetail::AnchorKindMismatch,
        AnchorDenial::LineNotInDiff => SendBackDetail::LineNotInDiff,
        AnchorDenial::LineNotInTranscript => SendBackDetail::LineNotInTranscript,
        AnchorDenial::NotText => SendBackDetail::DemoNotText,
        // Missing, or still downloading: the quote cannot be read yet.
        AnchorDenial::Blob(blob) if blob.code() == ErrorCode::NotFound => {
            return FfiError::SendBackRefused {
                detail: SendBackDetail::DemoNotOnDevice,
                comment: Some(number(index)),
                message: format!(
                    "Comment {}: the demo it points into is not on this device yet, so its \
                     line cannot be quoted. Nothing was written; try again when the file has \
                     downloaded.",
                    index + 1
                ),
            };
        }
        // Unreadable for another reason: the core's own error.
        AnchorDenial::Blob(blob) => return blob.clone().into(),
    };
    comment_refused(index, detail, error.message())
}
