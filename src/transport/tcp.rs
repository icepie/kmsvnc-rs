use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::time::Instant;

use anyhow::{Context, Result};

use crate::encode::EncodedPacket;
use crate::transport::PacketSink;

pub struct TcpAnnexBSink {
    listener: TcpListener,
    client: Option<TcpStream>,
    waiting_for_keyframe: bool,
    packets: u64,
    bytes: u64,
    started_at: Instant,
}

impl TcpAnnexBSink {
    pub fn new(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .with_context(|| format!("Failed to bind experimental video stream to {addr}"))?;
        listener
            .set_nonblocking(true)
            .context("Failed to set video stream listener nonblocking")?;
        tracing::info!("Experimental Annex B stream listening on {addr}");
        Ok(Self {
            listener,
            client: None,
            waiting_for_keyframe: false,
            packets: 0,
            bytes: 0,
            started_at: Instant::now(),
        })
    }

    fn accept_client_if_needed(&mut self) {
        if self.client.is_some() {
            return;
        }
        match self.listener.accept() {
            Ok((stream, peer)) => {
                if let Err(e) = stream.set_nodelay(true) {
                    tracing::warn!("Failed to set TCP_NODELAY for video stream client {peer}: {e}");
                }
                tracing::info!("Experimental Annex B client connected: {peer}");
                self.client = Some(stream);
                self.waiting_for_keyframe = true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                tracing::warn!("Experimental Annex B accept failed: {e}");
            }
        }
    }
}

impl PacketSink for TcpAnnexBSink {
    fn submit(&mut self, packet: EncodedPacket) -> Result<()> {
        self.accept_client_if_needed();
        if packet.data.is_empty() {
            return Ok(());
        }

        if let Some(stream) = self.client.as_mut() {
            if self.waiting_for_keyframe {
                if !packet.keyframe {
                    return Ok(());
                }
                tracing::info!("Experimental Annex B stream starting from keyframe pts={}", packet.pts);
                self.waiting_for_keyframe = false;
            }

            if let Err(e) = stream.write_all(&packet.data) {
                tracing::warn!("Experimental Annex B client disconnected: {e}");
                self.client = None;
                self.waiting_for_keyframe = false;
                return Ok(());
            }

            self.packets += 1;
            self.bytes += packet.data.len() as u64;
            if self.packets == 1 || self.packets % 120 == 0 {
                let secs = self.started_at.elapsed().as_secs_f32().max(0.001);
                tracing::info!(
                    "Experimental Annex B stream: packets={} avg_fps={:.1} avg_kbps={:.1} last_bytes={} keyframe={}",
                    self.packets,
                    self.packets as f32 / secs,
                    (self.bytes as f32 * 8.0 / 1000.0) / secs,
                    packet.data.len(),
                    packet.keyframe,
                );
            }
        }

        Ok(())
    }
}
