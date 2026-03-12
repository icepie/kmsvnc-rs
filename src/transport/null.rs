use anyhow::Result;
use std::time::Instant;

use crate::encode::EncodedPacket;
use crate::transport::PacketSink;

/// Discards encoded packets while keeping lightweight counters for validation.
pub struct NullSink {
    packets: u64,
    bytes: u64,
    keyframes: u64,
    started_at: Instant,
    last_report_at: Instant,
    last_report_packets: u64,
    last_report_bytes: u64,
}

impl NullSink {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            packets: 0,
            bytes: 0,
            keyframes: 0,
            started_at: now,
            last_report_at: now,
            last_report_packets: 0,
            last_report_bytes: 0,
        }
    }
}

impl PacketSink for NullSink {
    fn submit(&mut self, packet: EncodedPacket) -> Result<()> {
        self.packets += 1;
        self.bytes += packet.data.len() as u64;
        if packet.keyframe {
            self.keyframes += 1;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_report_at);
        if self.packets == 1 || elapsed.as_secs_f32() >= 2.0 {
            let total_elapsed = now.duration_since(self.started_at).as_secs_f32().max(0.001);
            let delta_packets = self.packets - self.last_report_packets;
            let delta_bytes = self.bytes - self.last_report_bytes;
            let interval_secs = elapsed.as_secs_f32().max(0.001);
            tracing::info!(
                "NullSink stats: codec={:?} packets={} keyframes={} avg_fps={:.1} interval_fps={:.1} avg_kbps={:.1} interval_kbps={:.1} last_pts={} last_bytes={}",
                packet.codec,
                self.packets,
                self.keyframes,
                self.packets as f32 / total_elapsed,
                delta_packets as f32 / interval_secs,
                (self.bytes as f32 * 8.0 / 1000.0) / total_elapsed,
                (delta_bytes as f32 * 8.0 / 1000.0) / interval_secs,
                packet.pts,
                packet.data.len(),
            );
            self.last_report_at = now;
            self.last_report_packets = self.packets;
            self.last_report_bytes = self.bytes;
        }
        Ok(())
    }
}
