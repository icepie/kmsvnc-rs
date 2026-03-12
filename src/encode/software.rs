use std::ffi::c_char;

use anyhow::{bail, Context, Result};

use crate::encode::{EncodedPacket, VideoCodec, VideoEncoder};
use crate::video::VideoFrame;

pub struct SoftwareEncoder {
    codec: VideoCodec,
    inner: Option<X264Encoder>,
}

impl SoftwareEncoder {
    pub fn new(codec: VideoCodec) -> Self {
        Self { codec, inner: None }
    }

    fn ensure_encoder(&mut self, width: u32, height: u32) -> Result<&mut X264Encoder> {
        if self.inner.is_none() {
            self.inner = Some(X264Encoder::new(width, height)?);
        }
        self.inner.as_mut().context("x264 encoder missing after init")
    }
}

impl VideoEncoder for SoftwareEncoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn encode(
        &mut self,
        frame: &VideoFrame,
        force_keyframe: bool,
        pts: u64,
    ) -> anyhow::Result<EncodedPacket> {
        if self.codec != VideoCodec::H264 {
            bail!("Software encoder currently only supports H.264");
        }
        let (width, height, stride, data) = frame.as_bgra()?;
        let enc = self.ensure_encoder(width, height)?;
        let packet = enc.encode(data, stride, pts, force_keyframe)?;
        Ok(EncodedPacket {
            codec: VideoCodec::H264,
            data: packet.data,
            keyframe: packet.keyframe,
            pts,
        })
    }
}

struct X264Encoder {
    raw: *mut X264EncoderContext,
}

// SAFETY: encoder context is owned and used by the single pipeline thread.
unsafe impl Send for X264Encoder {}

impl X264Encoder {
    fn new(width: u32, height: u32) -> Result<Self> {
        let raw = unsafe { kmsvnc_x264_open(width, height) };
        if raw.is_null() {
            bail!("x264 init failed: {}", x264_last_error());
        }
        Ok(Self { raw })
    }

    fn encode(&mut self, bgra: &[u8], stride: u32, pts: u64, force_keyframe: bool) -> Result<X264Packet> {
        let mut out_data = std::ptr::null_mut();
        let mut out_len = 0usize;
        let mut out_keyframe = 0i32;
        let ok = unsafe {
            kmsvnc_x264_encode(
                self.raw,
                bgra.as_ptr(),
                stride,
                pts,
                if force_keyframe { 1 } else { 0 },
                &mut out_data,
                &mut out_len,
                &mut out_keyframe,
            )
        };
        if ok == 0 {
            bail!("x264 encode failed: {}", x264_last_error());
        }
        if out_data.is_null() || out_len == 0 {
            return Ok(X264Packet {
                data: Vec::new(),
                keyframe: false,
            });
        }
        let data = unsafe { std::slice::from_raw_parts(out_data, out_len).to_vec() };
        unsafe {
            kmsvnc_x264_free_packet(out_data);
        }
        Ok(X264Packet {
            data,
            keyframe: out_keyframe != 0,
        })
    }
}

impl Drop for X264Encoder {
    fn drop(&mut self) {
        unsafe {
            kmsvnc_x264_close(self.raw);
        }
    }
}

struct X264Packet {
    data: Vec<u8>,
    keyframe: bool,
}

#[repr(C)]
struct X264EncoderContext {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn kmsvnc_x264_open(width: u32, height: u32) -> *mut X264EncoderContext;
    fn kmsvnc_x264_encode(
        ctx: *mut X264EncoderContext,
        bgra: *const u8,
        stride: u32,
        pts: u64,
        force_keyframe: i32,
        out_data: *mut *mut u8,
        out_len: *mut usize,
        out_keyframe: *mut i32,
    ) -> i32;
    fn kmsvnc_x264_free_packet(data: *mut u8);
    fn kmsvnc_x264_close(ctx: *mut X264EncoderContext);
    fn kmsvnc_x264_last_error() -> *const c_char;
}

fn x264_last_error() -> String {
    unsafe {
        let ptr = kmsvnc_x264_last_error();
        if ptr.is_null() {
            "unknown x264 error".into()
        } else {
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}
