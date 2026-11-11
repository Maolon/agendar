//! Read one delivery for review: what the desktop's delivery page shows
//! (`app/src/views/Delivery.tsx`), from the same reduced state. The revision
//! is the one every host binds a verdict to (`guard::delivery_revision`).
#![forbid(unsafe_code)]

use agendar_core::guard::{current_receipt, delivery_revision, find_delivery};
use agendar_core::model::{
    CheckResult as CoreResult, DeliveryState, DiffSide, Provenance as CoreProvenance, ReviewAnchor,
};

use super::activity::{review_need, review_state};
use super::demo::demo;
use super::shared::{event_id, not_found, step_title};
use super::{
    CheckResult, CommentPlace, Delivery, DeliveryCriterion, Disclosure, FfiError, Provenance,
    ReviewRound, RoundComment, Session,
};

impl Session {
    /// One delivery by its id (the `source` of a review need). The state comes
    /// from the cached index; the text of transcripts and diffs is read from
    /// the demo files in the Vault.
    pub fn delivery(&self, delivery_id: String) -> Result<Delivery, FfiError> {
        let id = event_id("/delivery_id", &delivery_id)?;
        let fresh = self.cached()?;
        let (slug, project, delivery) =
            find_delivery(&fresh.state, &id).ok_or_else(|| not_found("Delivery", &delivery_id))?;
        let need = review_need(project, delivery);
        let receipt = delivery
            .receipt
            .as_ref()
            .and_then(|receipt| project.receipts.get(receipt));
        Ok(Delivery {
            delivery_id: delivery.id.as_str().to_owned(),
            project: slug.as_str().to_owned(),
            project_title: project.summary.title.clone(),
            node: delivery.node.as_str().to_owned(),
            step_title: step_title(project, &delivery.node),
            name: delivery.name.clone(),
            revision: delivery_revision(slug, delivery, current_receipt(project, delivery))
                .as_str()
                .to_owned(),
            review: review_state(delivery.review),
            superseded: delivery.superseded_by.is_some(),
            need_id: need.map(|need| need.id.clone()),
            since: need.map(|need| need.since.to_string()),
            asked: receipt.map(|receipt| receipt.chose.label.clone()),
            demos: delivery
                .demos
                .iter()
                .map(|item| demo(self.service.vault(), slug, item))
                .collect(),
            criteria: delivery.criteria.iter().map(criterion).collect(),
            disclosure: Disclosure {
                proven: delivery.disclosure.proven.clone(),
                not_proven_yet: delivery.disclosure.not_proven_yet.clone(),
                faked_for_now: delivery.disclosure.faked_for_now.clone(),
            },
            unblocks: need.map_or_else(Vec::new, |need| {
                need.unblocks
                    .iter()
                    .map(|node| step_title(project, node))
                    .collect()
            }),
            answers: delivery
                .supersedes
                .as_ref()
                .and_then(|earlier| project.deliveries.get(earlier))
                .and_then(|earlier| round(earlier, Some(delivery))),
            sent_back: round(
                delivery,
                delivery
                    .superseded_by
                    .as_ref()
                    .and_then(|later| project.deliveries.get(later)),
            ),
        })
    }
}

const fn result(value: CoreResult) -> CheckResult {
    match value {
        CoreResult::Passed => CheckResult::Passed,
        CoreResult::Failed => CheckResult::Failed,
        CoreResult::NotProven => CheckResult::NotProven,
    }
}

fn criterion(entry: &agendar_core::model::ProjectedCriterion) -> DeliveryCriterion {
    DeliveryCriterion {
        id: entry.criterion.id.as_str().to_owned(),
        text: entry.criterion.text.clone(),
        provenance: match entry.provenance {
            CoreProvenance::Seen => Provenance::Seen,
            CoreProvenance::AgentSays => Provenance::AgentSays,
            CoreProvenance::NotProvenYet => Provenance::NotProvenYet,
        },
        agent_result: result(entry.agent_result),
        seen_result: entry.seen_result.map(result),
        conflict: entry.conflict,
    }
}

/// The send-back `sent` received, each comment beside the reply of the
/// redelivery that answers it, when there is one (`redeliveryOf` and
/// `sinceRows` in `app/src/state/redelivery.ts`): a reply without a comment
/// number answers the reason, or is an overall note. `None` for a delivery
/// that was not sent back.
fn round(sent: &DeliveryState, redelivery: Option<&DeliveryState>) -> Option<ReviewRound> {
    let note = sent.sent_back.as_ref()?;
    let reply_to = |number: Option<usize>| {
        redelivery?
            .replies
            .iter()
            .find(|reply| reply.comment.map(usize::from) == number)
            .map(|reply| reply.text.clone())
    };
    Some(ReviewRound {
        delivery_id: sent.id.as_str().to_owned(),
        name: sent.name.clone(),
        sent_back_at: note.at.to_string(),
        reason: note
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(str::to_owned),
        reply: reply_to(None),
        comments: note
            .comments
            .iter()
            .enumerate()
            .map(|(index, comment)| RoundComment {
                n: u32::try_from(index + 1).unwrap_or(u32::MAX),
                place: place(&comment.anchor),
                text: comment.text.clone(),
                reply: reply_to(Some(index + 1)),
            })
            .collect(),
    })
}

fn place(anchor: &ReviewAnchor) -> CommentPlace {
    match anchor {
        ReviewAnchor::DiffLine {
            demo,
            file,
            side,
            line,
            quote,
        } => CommentPlace::DiffLine {
            demo: *demo,
            file: file.clone(),
            old_side: *side == DiffSide::Old,
            line: *line,
            quote: quote.clone(),
        },
        ReviewAnchor::TranscriptLine { demo, line, quote } => CommentPlace::TranscriptLine {
            demo: *demo,
            line: *line,
            quote: quote.clone(),
        },
        ReviewAnchor::Region { demo, x, y, w, h } => CommentPlace::Region {
            demo: *demo,
            x: *x,
            y: *y,
            w: *w,
            h: *h,
        },
    }
}
