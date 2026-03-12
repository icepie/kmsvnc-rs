use anyhow::{bail, Result};
use drm::control::framebuffer;

use super::card::Card;
use super::cpu_capture::ActiveOutput;

#[derive(Debug, Clone, Copy)]
pub struct DmabufPlane {
    pub offset: u32,
    pub pitch: u32,
    pub size: u32,
}

#[derive(Debug)]
pub struct DmabufFrame {
    pub width: u32,
    pub height: u32,
    pub drm_format: u32,
    pub modifier: u64,
    pub num_planes: u32,
    pub planes: [DmabufPlane; 4],
}

/// Placeholder for the future GPU-native capture path.
///
/// The current codebase still uses CPU BGRA frames end-to-end. This type is
/// introduced now to give the upcoming encoder path a stable integration point
/// without changing runtime behavior yet.
pub struct DmabufCapturer {
    _card: Card,
    _output: framebuffer::Handle,
}

impl DmabufCapturer {
    pub fn new(card: Card, output: &ActiveOutput) -> Self {
        Self {
            _card: card,
            _output: output.fb_handle,
        }
    }

    pub fn capture(&mut self) -> Result<DmabufFrame> {
        bail!("DMA-BUF capture path is not implemented yet")
    }
}
