//! How the index decodes one Vault line, and which rejected lines a newer
//! version could still read.
//!
//! A line this version cannot decode for a reason a newer version could cure
//! (an unknown kind, an unknown field, an unknown enum value, a newer event
//! version) is kept as a *placeholder* row in `raw_events`: its canonical JSON,
//! `status = 'quarantined'`, `reason = 'undecodable'`, with its source
//! occurrences. It is never folded. The index records which decoder last
//! committed (`decoder_version`, `decoder_generation` in `meta`); when that
//! differs from the running binary, the next commit decodes the stored
//! placeholders again (see `ingest::redecode_stored`). Every other rejection
//! (corrupt JSON, a failed semantic rule, an oversized line) stays an
//! `invalid_line` diagnostic exactly as before.
//!
//! The placeholders are the cheap path, not the guarantee: the list of
//! curable rejections need not be complete. On the same version change the
//! sources that have an `invalid_line` diagnostic are read again from the
//! start, in the same ingest call (`ingest::reread_after`).

use std::cell::RefCell;

use rusqlite::Connection;

use crate::error::{AgendarError, Result};
use crate::model::{Event, EventId, Sha256, SourcePath, canonical_json, parse_event};

/// The decoder of this binary. Wire changes ship in a release, and every
/// release has its own version, so a changed version is the signal to read
/// the placeholders again.
pub const DECODER_VERSION: &str = env!("CARGO_PKG_VERSION");

const MAX_SEQ: u64 = 9_007_199_254_740_991;

/// Meta key: the decoder version of the last commit.
pub const META_DECODER_VERSION: &str = "decoder_version";
/// Meta key: the generation that commit wrote. A binary that predates the
/// stamp bumps `generation` without it, which shows up as a mismatch.
pub const META_DECODER_GENERATION: &str = "decoder_generation";

/// The decoder version in effect on this thread.
pub fn decoder_version() -> String {
    OLDER.with(|older| {
        older
            .borrow()
            .as_ref()
            .map_or_else(|| DECODER_VERSION.to_owned(), |older| older.version.clone())
    })
}

/// A line kept for a newer version: the envelope columns `raw_events` needs
/// and the canonical JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placeholder {
    pub id: EventId,
    pub digest: Sha256,
    pub writer: String,
    pub seq: u64,
    pub ts: String,
    pub project: String,
    pub kind: String,
    pub json: String,
}

/// What one line decodes to.
pub(crate) enum LineOutcome {
    Event(Box<Event>),
    Deferred(Box<Placeholder>),
    Rejected(AgendarError),
}

/// Decode one complete line (with its terminator) read from `source`.
pub(crate) fn decode_line_bytes(bytes: &[u8], source: &SourcePath) -> LineOutcome {
    match decode_event(bytes, source) {
        Ok(event) => LineOutcome::Event(Box::new(event)),
        Err(error) if curable(&error) => match placeholder(bytes) {
            Some(placeholder) => LineOutcome::Deferred(Box::new(placeholder)),
            None => LineOutcome::Rejected(error),
        },
        Err(error) => LineOutcome::Rejected(error),
    }
}

/// `parse_event`, made stricter on this thread while a test simulates an
/// older reader.
pub(crate) fn decode_event(bytes: &[u8], source: &SourcePath) -> Result<Event> {
    let event = parse_event(bytes, source)?;
    OLDER.with(|older| match older.borrow().as_ref() {
        Some(older) => older.check(&event, bytes).map(|()| event),
        None => Ok(event),
    })
}

/// A rejection a newer decoder could cure: the line is well-formed JSON and
/// only names something this version does not know.
fn curable(error: &AgendarError) -> bool {
    match error.detail() {
        Some(
            "unknown_kind"
            | "unknown_field"
            | "unsupported_version"
            | "object_shape_required"
            | "unknown_glyph"
            | "unknown_icon",
        ) => true,
        Some("invalid_body" | "invalid_refs") => {
            let message = error.message();
            message.contains("unknown field `") || message.contains("unknown variant `")
        }
        _ => false,
    }
}

/// The envelope columns of a curable line, or `None` when they are missing.
fn placeholder(bytes: &[u8]) -> Option<Placeholder> {
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let text = text.strip_suffix('\r').unwrap_or(text);
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let object = value.as_object()?;
    let field = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let id = EventId::parse(object.get("id")?.as_str()?).ok()?;
    let seq = object
        .get("seq")?
        .as_u64()
        .filter(|seq| (1..=MAX_SEQ).contains(seq))?;
    let json = canonical_json(&value);
    Some(Placeholder {
        id,
        digest: Sha256::from_bytes(json.as_bytes()),
        writer: field("writer")?,
        seq,
        ts: field("ts")?,
        project: field("project")?,
        kind: field("kind")?,
        json,
    })
}

/// The envelope id of a line, best effort (`None` unless the line is JSON
/// with a valid `id`).
pub(crate) fn envelope_id(bytes: &[u8]) -> Option<EventId> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
    EventId::parse(value.get("id")?.as_str()?).ok()
}

/// Why the last commit's decoding may be out of date. On either change the
/// placeholders are decoded again and the files with rejected lines are read
/// again: `curable` need not be complete for an upgrade to catch up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Redecode {
    /// This decoder made the last commit.
    Current,
    /// Another stamped decoder made it.
    Stored,
    /// A binary without the stamp committed (or none ever did).
    Legacy,
}

pub(crate) fn redecode_due(connection: &Connection) -> Result<Redecode> {
    let version = crate::index::meta(connection, META_DECODER_VERSION)?;
    let stamped = crate::index::meta(connection, META_DECODER_GENERATION)?;
    let generation = crate::index::meta(connection, "generation")?;
    Ok(match version {
        None => Redecode::Legacy,
        Some(_) if stamped != generation => Redecode::Legacy,
        Some(version) if version != decoder_version() => Redecode::Stored,
        Some(_) => Redecode::Current,
    })
}

/// The stored source cursor, unless the stored decoding is out of date: then
/// the index is not current for this binary and the caller must commit.
pub(crate) fn current_cursor(connection: &Connection) -> Result<Option<Sha256>> {
    if redecode_due(connection)? != Redecode::Current {
        return Ok(None);
    }
    Ok(crate::index::meta(connection, "source_digest")?
        .and_then(|value| Sha256::parse(&value).ok()))
}

thread_local! {
    static OLDER: RefCell<Option<OlderReader>> = const { RefCell::new(None) };
}

/// Test support: an older reader, simulated on the calling thread. It reports
/// `version` as its decoder version and rejects, the way the real decoder
/// rejects what it does not know, every event of `unknown_kinds` and every
/// event whose body carries one of `unknown_body_fields` (`(kind, field)`).
/// Events of `rejected_kinds` fail a rule instead: a rejection that is not
/// curable, so the line is not kept. It can only make decoding stricter.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct OlderReader {
    pub version: String,
    pub unknown_kinds: Vec<String>,
    pub unknown_body_fields: Vec<(String, String)>,
    pub rejected_kinds: Vec<String>,
}

impl OlderReader {
    fn check(&self, event: &Event, bytes: &[u8]) -> Result<()> {
        let kind = event.kind();
        if self.unknown_kinds.iter().any(|known| known == kind) {
            return Err(AgendarError::validation_at(
                "/kind",
                "unknown_kind",
                format!("Unknown event kind `{kind}`."),
            ));
        }
        if self.rejected_kinds.iter().any(|rejected| rejected == kind) {
            return Err(AgendarError::validation_at(
                "/body",
                "older_rule",
                format!("{kind} fails a rule of the older version."),
            ));
        }
        let value = serde_json::from_slice::<serde_json::Value>(bytes).unwrap_or_default();
        for (field_kind, field) in &self.unknown_body_fields {
            if field_kind == kind && value["body"].get(field).is_some() {
                return Err(AgendarError::validation_at(
                    "/body",
                    "invalid_body",
                    format!("Invalid {kind} body: unknown field `{field}`."),
                ));
            }
        }
        Ok(())
    }
}

/// Restores the real decoder on this thread when dropped.
#[doc(hidden)]
pub struct OlderReaderGuard(());

impl Drop for OlderReaderGuard {
    fn drop(&mut self) {
        OLDER.with(|older| older.borrow_mut().take());
    }
}

/// Simulate `reader` on this thread until the guard drops.
#[doc(hidden)]
pub fn simulate_older_reader(reader: OlderReader) -> OlderReaderGuard {
    OLDER.with(|older| *older.borrow_mut() = Some(reader));
    OlderReaderGuard(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_curable_line_without_an_envelope_is_not_kept() {
        assert!(placeholder(b"{\"kind\":\"x\"}\n").is_none());
        let kept = placeholder(
            b"{\"v\":1,\"id\":\"00000000000000000000000001\",\"seq\":3,\"writer\":\"w\",\"ts\":\"t\",\"project\":\"p\",\"kind\":\"k\"}\r\n",
        )
        .expect("an envelope is kept");
        assert_eq!(kept.seq, 3);
        assert_eq!(kept.digest, Sha256::from_bytes(kept.json.as_bytes()));
    }

    #[test]
    fn unknown_kind_and_field_are_curable() {
        let unknown_kind = AgendarError::validation_at("/kind", "unknown_kind", "x");
        assert!(curable(&unknown_kind));
        let field = AgendarError::validation_at(
            "/body",
            "invalid_body",
            "Invalid plan.feature body: unknown field `owner`, expected one of `title`",
        );
        assert!(curable(&field));
        let variant = AgendarError::validation_at(
            "/body",
            "invalid_body",
            "Invalid work.progress body: unknown variant `parked`, expected `planned`",
        );
        assert!(curable(&variant));
        let semantic = AgendarError::validation_at("/body", "invalid_body", "title too long");
        assert!(!curable(&semantic));
        let json = AgendarError::validation_at("/", "invalid_json", "unknown field `x`");
        assert!(!curable(&json));
    }
}
