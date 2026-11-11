//! Anchored send-back comments: parse them from the review request. The
//! check against the delivery and the quoted line (always re-read from the
//! demo blob; the frontend's quote is never trusted) are the core's
//! `anchors::resolve_comments`, shared with the mobile facade.

use serde_json::Value;

use agendar_core::anchors::{AnchorDenial, AnchorError};
use agendar_core::model::{MAX_REVIEW_COMMENTS, ReviewComment, validate_review_comment};

use crate::error::{ApiResult, AppError};

/// The optional `comments` array of a review request, each checked on its own.
/// A line anchor may omit `quote`: the host fills it in from the blob.
pub fn parse_comments(value: Option<&Value>) -> ApiResult<Vec<ReviewComment>> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(AppError::validation(
            "/comments",
            "comments must be an array.",
        ));
    };
    if items.len() > MAX_REVIEW_COMMENTS {
        return Err(AppError::validation(
            "/comments",
            format!("A review carries at most {MAX_REVIEW_COMMENTS} comments."),
        ));
    }
    let mut comments = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let field = format!("/comments/{index}");
        let mut item = item.clone();
        if let Some(anchor) = item.get_mut("anchor").and_then(Value::as_object_mut) {
            let line_kind = matches!(
                anchor.get("kind").and_then(Value::as_str),
                Some("diff_line" | "transcript_line")
            );
            if line_kind && !anchor.contains_key("quote") {
                anchor.insert("quote".to_owned(), Value::String(String::new()));
            }
        }
        let comment: ReviewComment = serde_json::from_value(item)
            .map_err(|error| AppError::validation(&field, format!("Invalid comment: {error}.")))?;
        validate_review_comment(&comment, &field).map_err(AppError::from)?;
        comments.push(comment);
    }
    Ok(comments)
}

/// The core's refusal of one comment, as this host has always reported it.
pub fn anchor_error(error: AnchorError) -> AppError {
    let field = error.field();
    let message = error.message().to_owned();
    match error.denial {
        AnchorDenial::Blob(error) => AppError::from(error),
        _ => AppError::validation(field, message),
    }
}
