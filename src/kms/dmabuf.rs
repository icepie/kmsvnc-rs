use std::os::fd::{AsFd, OwnedFd};

use anyhow::{bail, Context, Result};
use drm::control::{crtc, framebuffer, Device as ControlDevice};
use drm_fourcc::DrmModifier;
use rustix::io::dup;

use super::card::Card;
use super::cpu_capture::ActiveOutput;

#[derive(Debug)]
pub struct DmabufObject {
    pub fd: OwnedFd,
    pub size: Option<u64>,
    pub modifier: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct DmabufPlane {
    pub object_index: usize,
    pub offset: u32,
    pub pitch: u32,
}

#[derive(Debug)]
pub struct DmabufFrame {
    pub width: u32,
    pub height: u32,
    pub drm_format: u32,
    pub src_x: u32,
    pub src_y: u32,
    pub fb_width: u32,
    pub fb_height: u32,
    pub drm_device_fd: OwnedFd,
    pub objects: Vec<DmabufObject>,
    pub planes: Vec<DmabufPlane>,
}

pub struct DmabufCapturer {
    card: Card,
    crtc_handle: crtc::Handle,
    default_fb: framebuffer::Handle,
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
}

impl DmabufCapturer {
    pub fn new(card: Card, output: &ActiveOutput) -> Self {
        Self {
            card,
            crtc_handle: output.crtc_handle,
            default_fb: output.fb_handle,
            src_x: output.x,
            src_y: output.y,
            width: output.width,
            height: output.height,
        }
    }

    pub fn capture(&mut self) -> Result<DmabufFrame> {
        let crtc_info = self
            .card
            .get_crtc(self.crtc_handle)
            .context("Failed to get CRTC for DMA-BUF capture")?;
        let fb_handle = crtc_info.framebuffer().unwrap_or(self.default_fb);
        let info = self
            .card
            .get_planar_framebuffer(fb_handle)
            .context("GET_FB2 failed for DMA-BUF capture")?;

        if info.buffers()[0].is_none() {
            bail!("Framebuffer does not expose any GEM buffer handles");
        }

        let modifier = info.modifier().unwrap_or(DrmModifier::Invalid);
        let modifier_raw = match modifier {
            DrmModifier::Invalid => 0,
            _ => modifier.into(),
        };

        let mut objects = Vec::new();
        let mut object_handles = Vec::new();
        let mut planes = Vec::new();

        for plane_index in 0..4 {
            let Some(handle) = info.buffers()[plane_index] else {
                continue;
            };

            let object_index =
                if let Some(existing) = object_handles.iter().position(|h| *h == handle) {
                    existing
                } else {
                    let fd = self
                        .card
                        .buffer_to_prime_fd(handle, drm::RDWR)
                        .with_context(|| format!("PRIME export failed for plane {plane_index}"))?;
                    object_handles.push(handle);
                    objects.push(DmabufObject {
                        fd,
                        size: None,
                        modifier: modifier_raw,
                    });
                    objects.len() - 1
                };

            planes.push(DmabufPlane {
                object_index,
                offset: info.offsets()[plane_index],
                pitch: info.pitches()[plane_index],
            });
        }

        if planes.is_empty() {
            bail!("Framebuffer exported no DMA-BUF planes");
        }

        Ok(DmabufFrame {
            width: self.width,
            height: self.height,
            drm_format: info.pixel_format() as u32,
            src_x: self.src_x,
            src_y: self.src_y,
            fb_width: info.size().0,
            fb_height: info.size().1,
            drm_device_fd: dup(self.card.as_fd()).context("dup DRM fd failed for DMA-BUF frame")?,
            objects,
            planes,
        })
    }
}
