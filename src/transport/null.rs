use anyhow::Result;

use crate::encode::EncodedPacket;
use crate::transport::PacketSink;

/// Discards encoded packets while keeping lightweight counters for validation.
pub struct NullSink {
    packets: u64,
    bytes: u64,
}

impl NullSink {
    pub fn new() -> Self {
        Self { packets: 0, bytes: 0 }
    }
}

impl PacketSink for NullSink {
    fn submit(&mut self, packet: EncodedPacket) -> Result<()> {
        self.packets += 1;
        self.bytes += packet.data.len() as u64;
        if self.packets == 1 || self.packets % 120 == 0 {
            tracing::debug!(
                "NullSink received packet #{} (codec={:?}, keyframe={}, pts={}, bytes={}, total_bytes={})",
                self.packets,
                packet.codec,
                packet.keyframe,
                packet.pts,
                packet.data.len(),
                self.bytes
            );
        }
        Ok(())
    }
}
