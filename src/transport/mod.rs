pub mod null;
pub mod tcp;

use anyhow::Result;

use crate::encode::EncodedPacket;

pub trait PacketSink {
    fn submit(&mut self, packet: EncodedPacket) -> Result<()>;
}
