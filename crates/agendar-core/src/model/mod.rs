//! Pure protocol-v1 model: ids, envelope, bodies, cards, projections and
//! source-bound decoding.

pub mod cards;
pub mod event;
pub mod ids;
pub mod msg;
pub mod schema;
pub mod state;
pub mod validate;

pub use cards::*;
pub use event::*;
pub use ids::*;
pub use msg::{Msg, MsgArg};
pub use state::*;
pub use validate::{parse_event, validate_card_input};
