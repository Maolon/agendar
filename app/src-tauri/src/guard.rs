//! The backend rubber-stamp guard and the prepare-token store.
//!
//! A token is issued only by `prepare_answer`, lives 60 seconds and is bound to
//! the native caller, ask, chosen option and decision revision. `answer_decision`
//! rechecks the current card; a changed revision, receipt or lifecycle is a
//! stale answer with no write.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use agendar_core::model::{
    DecisionOption, DecisionState, EtaRange, EventId, OptionId, Reversibility, Sha256, UtcTime,
    canonical_json,
};

/// Prepare-token lifetime.
pub const TOKEN_TTL_S: i64 = 60;

/// The guard itself is shared with every other host.
pub use agendar_core::guard::{Guard, effective_risk, guard_for};

/// One issued prepare token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedToken {
    pub surface: String,
    pub ask: EventId,
    pub option: OptionId,
    pub revision: Sha256,
    pub receipt: Option<EventId>,
    pub label: String,
    pub risk: Reversibility,
    pub guard: Guard,
    /// The human picked a non-recommended option on an easy card from the main
    /// window. The plain-Enter shortcut is reserved for the
    /// recommendation; an explicit pointer/number choice of another option is
    /// still a valid answer, so this token is validated as an explicit choice
    /// instead of the one-key recommendation path.
    pub explicit_choice: bool,
    pub expires_at: UtcTime,
    pub used: bool,
    /// Notification route when this token was minted for a native action.
    pub notice_id: Option<String>,
    /// Stable request id for a notification action; none for UI preparation.
    pub request_id: Option<String>,
    /// The human's own words, bound to the token, when the option is `own`.
    pub own_answer: Option<String>,
}

impl PreparedToken {
    pub fn is_expired(&self, now: UtcTime) -> bool {
        self.expires_at <= now
    }
}

/// In-memory token store. Tokens are never persisted, never placed in OS
/// notification text and never renewed on click.
#[derive(Debug, Default)]
pub struct TokenStore {
    tokens: BTreeMap<String, PreparedToken>,
}

impl TokenStore {
    pub fn insert(&mut self, token: String, value: PreparedToken) {
        // Drop expired tokens opportunistically; the store is process-local.
        self.tokens.retain(|_, candidate| !candidate.used);
        self.tokens.insert(token, value);
    }

    pub fn get(&self, token: &str) -> Option<&PreparedToken> {
        self.tokens.get(token)
    }

    pub fn get_mut(&mut self, token: &str) -> Option<&mut PreparedToken> {
        self.tokens.get_mut(token)
    }

    /// Invalidate every outstanding token when the app quits or a window closes.
    pub fn clear(&mut self) {
        self.tokens.clear();
    }

    /// Remove tokens bound to one surface (a reopened window gets a new one).
    pub fn clear_surface(&mut self, surface: &str) {
        self.tokens.retain(|_, token| token.surface != surface);
    }
}

/// An opaque random 128-bit token rendered as 32 lowercase hex characters.
pub fn new_token() -> String {
    let first = ulid::Ulid::generate();
    let second = ulid::Ulid::generate();
    let digest = Sha256::from_bytes(format!("{first}{second}").as_bytes());
    digest.as_str()[..32].to_owned()
}

/// A canonical ULID for the idempotent answer request.
pub fn new_request_id() -> String {
    ulid::Ulid::generate().to_string()
}

/// A random 128-bit notification routing handle.
pub fn new_notice_id() -> String {
    new_token()
}

/// The receipt preview shown before a commit; it is explicitly unsaved.
pub fn receipt_preview(decision: &DecisionState, chosen: &DecisionOption) -> Value {
    let ruled_out: Vec<&DecisionOption> = decision
        .card
        .options
        .iter()
        .filter(|option| option.id != chosen.id)
        .collect();
    let mut value = json!({
        "preview": true,
        "ask": decision.id.as_str(),
        "node": decision.node.as_str(),
        "source": "human",
        "chose": chosen,
        "ruled_out": ruled_out,
        "done_when": decision.card.done_when,
    });
    if let Some(eta) = &chosen.eta
        && let Value::Object(object) = &mut value
    {
        object.insert("expected_eta".to_owned(), json!(eta));
    }
    value
}

/// One prepare result returned to the frontend.
pub fn prepare_data(
    token: String,
    expires_at: UtcTime,
    risk: Reversibility,
    label: String,
    receipt: Value,
    guard: Guard,
) -> Value {
    json!({
        "token": token,
        "expires_at": expires_at,
        "risk": risk,
        "label": label,
        "receipt": receipt,
        "guard": guard.as_str(),
    })
}

/// Payload digest for the app-side request journal (request id excluded).
pub fn payload_digest(value: &Value) -> Sha256 {
    Sha256::from_bytes(canonical_json(value).as_bytes())
}

/// Parses an optional ETA request field.
pub fn parse_eta(value: Option<&Value>) -> Result<Option<EtaRange>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| format!("expected_eta is not a valid ETA range: {error}")),
    }
}
