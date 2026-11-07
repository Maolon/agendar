//! Generic pull adapter: packets are only readable through `agendar inbox`.

use crate::error::Result;
use crate::model::InboxPacket;

use super::{Adapter, AdapterCapabilities, TransportResult, capabilities_for};

/// The portable pull-only adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct GenericAdapter {
    pub running: bool,
}

impl Adapter for GenericAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        capabilities_for(crate::model::Harness::Generic, self.running)
    }

    fn deliver(&self, _packet: &InboxPacket) -> Result<TransportResult> {
        // A pull adapter never claims a push transport; the packet is available
        // to the next explicit inbox read.
        Ok(TransportResult::PullRequired)
    }
}
