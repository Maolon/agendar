//! Claude Code adapter: pull via PostToolUse and stdin context injection.
//!
//! The hook shim (`agendar hook ...`) owns the process-side protocol; this
//! adapter only describes the capability and confirms that delivery is a pull,
//! never an execution or an acknowledgement.

use crate::error::Result;
use crate::model::InboxPacket;

use super::{Adapter, AdapterCapabilities, TransportResult, capabilities_for};

/// Claude Code transport capabilities.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeAdapter {
    pub running: bool,
}

impl Adapter for ClaudeAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        capabilities_for(crate::model::Harness::ClaudeCode, self.running)
    }

    fn deliver(&self, _packet: &InboxPacket) -> Result<TransportResult> {
        Ok(TransportResult::PullRequired)
    }
}
