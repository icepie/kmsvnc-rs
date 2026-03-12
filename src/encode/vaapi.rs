use anyhow::bail;

use crate::encode::{EncodedPacket, VideoCodec, VideoEncoder};
use crate::video::VideoFrame;

/// Placeholder VAAPI encoder.
///
/// The future implementation should accept GPU-native frames (DMA-BUF or
/// imported surfaces) and emit compressed video packets without forcing a CPU
/// readback path.
pub struct VaapiEncoder {
    codec: VideoCodec,
}

impl VaapiEncoder {
    pub fn new(codec: VideoCodec) -> Self {
        Self { codec }
    }
}

impl VideoEncoder for VaapiEncoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn encode(
        &mut self,
        _frame: &VideoFrame,
        _force_keyframe: bool,
        _pts: u64,
    ) -> anyhow::Result<EncodedPacket> {
        bail!("VAAPI encoder is not implemented yet")
    }
}
