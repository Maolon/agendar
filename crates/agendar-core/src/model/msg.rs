//! Language-neutral display messages for text the core writes for humans.
//!
//! The core keeps its English `text` (agents and the CLI read it). Next to it, a
//! [`Msg`] names a key in the app's message catalog plus typed arguments, so the
//! app renders the same line in the human's chosen language without the core
//! knowing that language, and a language switch needs no new projection.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One catalog key with its arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Msg {
    pub key: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, MsgArg>,
}

/// A typed argument; the app formats each kind for the active language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum MsgArg {
    /// Shown as written: titles, names, questions.
    Text(String),
    /// A count. An argument named `n` also picks the `.one`/`.other` key.
    Count(u64),
    /// An age or duration in whole minutes.
    Minutes(u64),
    /// A nested message.
    Msg(Box<Msg>),
    /// Messages joined with the language's list separator.
    List(Vec<Msg>),
}

impl Msg {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            args: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn text(mut self, name: &str, value: impl Into<String>) -> Self {
        self.args
            .insert(name.to_owned(), MsgArg::Text(value.into()));
        self
    }

    #[must_use]
    pub fn count(mut self, name: &str, value: impl TryInto<u64>) -> Self {
        let value = value.try_into().unwrap_or(u64::MAX);
        self.args.insert(name.to_owned(), MsgArg::Count(value));
        self
    }

    #[must_use]
    pub fn minutes(mut self, name: &str, value: u64) -> Self {
        self.args.insert(name.to_owned(), MsgArg::Minutes(value));
        self
    }

    #[must_use]
    pub fn arg(mut self, name: &str, value: MsgArg) -> Self {
        self.args.insert(name.to_owned(), value);
        self
    }

    #[must_use]
    pub fn msg(mut self, name: &str, value: Self) -> Self {
        self.args
            .insert(name.to_owned(), MsgArg::Msg(Box::new(value)));
        self
    }

    #[must_use]
    pub fn list(mut self, name: &str, values: Vec<Self>) -> Self {
        self.args.insert(name.to_owned(), MsgArg::List(values));
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn serializes_typed_arguments_compactly() {
        let msg = Msg::new("need.reason.waiting")
            .count("n", 2_u32)
            .minutes("age", 38)
            .text("title", "Wire model")
            .msg("actor", Msg::new("actor.you"));
        assert_eq!(
            serde_json::to_value(&msg).expect("json"),
            json!({
                "key": "need.reason.waiting",
                "args": {
                    "actor": {"t": "msg", "v": {"key": "actor.you"}},
                    "age": {"t": "minutes", "v": 38},
                    "n": {"t": "count", "v": 2},
                    "title": {"t": "text", "v": "Wire model"}
                }
            })
        );
        let back: Msg =
            serde_json::from_value(serde_json::to_value(&msg).expect("json")).expect("msg");
        assert_eq!(back, msg);
    }
}
