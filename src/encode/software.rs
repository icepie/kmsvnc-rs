use anyhow::bail;

use crate::encode::{EncodedPacket, VideoCodec, VideoEncoder};
use crate::video::VideoFrame;

/// Placeholder software encoder.
///
/// This exists to establish the module boundary and trait usage before
/// introducing a real CPU encoder implementation.
pub struct SoftwareEncoder {
    codec: VideoCodec,
}

impl SoftwareEncoder {
    pub fn new(codec: VideoCodec) -> Self {
        Self { codec }
    }
}

impl VideoEncoder for SoftwareEncoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn encode(
        &mut self,
        _frame: &VideoFrame,
        _force_keyframe: bool,
        _pts: u64,
    ) -> anyhow::Result<EncodedPacket> {
        bail!("software encoder is not implemented yet")
    }
}
