//! A review verdict on a delivery travels the answer road: it is a `review`
//! packet in the inbox, it wakes the idle session that made the delivery, and
//! `wait --delivery` returns it. These helpers read it from one converged
//! projection, so two hosts with the same events give the same packet.

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::{
    Event, EventId, EventPayload, InboxPacket, PacketKind, ProjectSlug, ProjectState, ReviewStatus,
    ReviewVerdict, SessionId,
};

/// Deliveries of this session that still wait for the human's verdict.
pub fn pending_reviews(state: &ProjectState, session: &SessionId) -> Vec<EventId> {
    let mut pending: Vec<EventId> = state
        .deliveries
        .values()
        .filter(|delivery| {
            delivery.review == ReviewStatus::Pending
                && delivery.superseded_by.is_none()
                && owns_step(state, &delivery.id, session)
        })
        .map(|delivery| delivery.id.clone())
        .collect();
    pending.sort();
    pending
}

/// Whether the session holds the delivery's step. The inbox addresses a
/// verdict to that session; a resumed session inherits it with the step.
pub fn owns_step(state: &ProjectState, delivery: &EventId, session: &SessionId) -> bool {
    state
        .deliveries
        .get(delivery)
        .and_then(|delivery| state.features.get(&delivery.node))
        .and_then(|feature| feature.session.as_ref())
        == Some(session)
}

/// Whether the delivery event was written by this session.
pub fn made_by(ordered: &[Event], delivery: &EventId, session: &SessionId) -> bool {
    ordered.iter().any(|event| {
        &event.id == delivery
            && matches!(&event.payload, EventPayload::Delivery(body) if &body.agent.session == session)
    })
}

/// The verdict packet for one delivery, once the human accepted it or sent it
/// back; `None` while it waits. Same id and body as the inbox packet.
pub fn review_packet(
    state: &ProjectState,
    ordered: &[Event],
    project: &ProjectSlug,
    delivery_id: &EventId,
) -> Result<Option<InboxPacket>> {
    let Some(delivery) = state.deliveries.get(delivery_id) else {
        return Err(AgendarError::new(
            ErrorCode::NotFound,
            format!("Delivery {} was not found.", delivery_id.as_str()),
        )
        .with_detail("delivery_not_found")
        .with_hint("Use the event_id `deliver` returned."));
    };
    if delivery.review_conflict {
        return Ok(None);
    }
    if delivery.review == ReviewStatus::Pending {
        if let Some(newer) = &delivery.superseded_by {
            return Err(AgendarError::new(
                ErrorCode::Withdrawn,
                format!(
                    "Delivery {} was replaced by {} before a verdict.",
                    delivery_id.as_str(),
                    newer.as_str()
                ),
            )
            .with_detail("delivery_superseded")
            .with_hint(format!("Wait for {} instead.", newer.as_str())));
        }
        return Ok(None);
    }
    let Some(review_id) = &delivery.review_event else {
        return Ok(None);
    };
    let Some(review) = ordered.iter().find(|event| &event.id == review_id) else {
        return Ok(None);
    };
    let EventPayload::ReviewResult(body) = &review.payload else {
        return Ok(None);
    };
    Ok(Some(InboxPacket {
        id: InboxPacket::compute_id(PacketKind::Review, project, &delivery.node, review_id, &[]),
        project: project.clone(),
        node: delivery.node.clone(),
        kind: PacketKind::Review,
        ask: None,
        answer: None,
        receipt: None,
        review: Some(body.clone()),
        delivery: Some(delivery.id.clone()),
        source: review_id.clone(),
        redo: Vec::new(),
        created_at: review.ts,
    }))
}

/// A sent-back verdict is work: it holds the session until it acts. An accept
/// is information only.
pub fn is_send_back(packet: &InboxPacket) -> bool {
    packet.kind == PacketKind::Review
        && packet
            .review
            .as_ref()
            .is_some_and(|review| review.verdict == ReviewVerdict::SendBack)
}
