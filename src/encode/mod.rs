pub mod software;
pub mod vaapi;

use anyhow::Result;

use crate::video::VideoFrame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone)]
pub struct EncodedPacket {
    pub codec: VideoCodec,
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub pts: u64,
}

pub trait VideoEncoder {
    fn codec(&self) -> VideoCodec;
    fn encode(&mut self, frame: &VideoFrame, force_keyframe: bool, pts: u64) -> Result<EncodedPacket>;
}
