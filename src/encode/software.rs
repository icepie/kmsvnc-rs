use std::ffi::c_char;
use std::time::Instant;

use anyhow::{bail, Result};

use crate::encode::{EncodedPacket, VideoCodec, VideoEncoder};
use crate::video::VideoFrame;

pub struct SoftwareEncoder {
    codec: VideoCodec,
    inner: Option<X264Encoder>,
    frames_encoded: u64,
    started_at: Instant,
    target_width: Option<u32>,
    target_height: Option<u32>,
    scaled_bgra: Vec<u8>,
}

impl SoftwareEncoder {
    pub fn new(codec: VideoCodec, target_width: Option<u32>, target_height: Option<u32>) -> Self {
        Self {
            codec,
            inner: None,
            frames_encoded: 0,
            started_at: Instant::now(),
            target_width,
            target_height,
            scaled_bgra: Vec::new(),
        }
    }

    fn take_or_create_encoder(&mut self, width: u32, height: u32) -> Result<X264Encoder> {
        match self.inner.take() {
            Some(enc) if enc.width == width && enc.height == height => Ok(enc),
            Some(_enc) => X264Encoder::new(width, height),
            None => X264Encoder::new(width, height),
        }
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
        let (src_width, src_height, src_stride, data) = frame.as_bgra()?;
        let dst_width = self.target_width.unwrap_or(src_width);
        let dst_height = self.target_height.unwrap_or(src_height);
        let (width, height, stride) =
            self.prepare_bgra(data, src_width, src_height, src_stride, dst_width, dst_height);
        let mut enc = self.take_or_create_encoder(width, height)?;
        let bgra = if src_width == dst_width && src_height == dst_height {
            data
        } else {
            &self.scaled_bgra
        };
        let packet = enc.encode(bgra, stride, pts, force_keyframe)?;
        self.inner = Some(enc);
        self.frames_encoded += 1;
        if self.frames_encoded == 1 {
            tracing::info!(
                "Software H.264 encoder active: input={}x{} output={}x{} stride={} codec={:?}",
                src_width,
                src_height,
                width,
                height,
                stride,
                self.codec
            );
        } else if self.frames_encoded % 120 == 0 {
            let secs = self.started_at.elapsed().as_secs_f32().max(0.001);
            tracing::info!(
                "Software H.264 encoder progress: frames={} avg_fps={:.1} last_packet_bytes={} keyframe={}",
                self.frames_encoded,
                self.frames_encoded as f32 / secs,
                packet.data.len(),
                packet.keyframe,
            );
        }
        Ok(EncodedPacket {
            codec: VideoCodec::H264,
            data: packet.data,
            keyframe: packet.keyframe,
            pts,
        })
    }
}

impl SoftwareEncoder {
    fn prepare_bgra(
        &mut self,
        src: &[u8],
        src_width: u32,
        src_height: u32,
        src_stride: u32,
        dst_width: u32,
        dst_height: u32,
    ) -> (u32, u32, u32) {
        if src_width == dst_width && src_height == dst_height {
            return (src_width, src_height, src_stride);
        }

        let dst_stride = dst_width * 4;
        let needed = (dst_stride * dst_height) as usize;
        if self.scaled_bgra.len() != needed {
            self.scaled_bgra.resize(needed, 0);
        }

        for y in 0..dst_height {
            let src_y = (y as u64 * src_height as u64 / dst_height as u64) as u32;
            let src_row = &src[(src_y * src_stride) as usize..];
            let dst_row = &mut self.scaled_bgra[(y * dst_stride) as usize..][..dst_stride as usize];
            for x in 0..dst_width {
                let src_x = (x as u64 * src_width as u64 / dst_width as u64) as u32;
                let src_off = (src_x * 4) as usize;
                let dst_off = (x * 4) as usize;
                dst_row[dst_off..dst_off + 4].copy_from_slice(&src_row[src_off..src_off + 4]);
            }
        }

        (dst_width, dst_height, dst_stride)
    }
}

struct X264Encoder {
    raw: *mut X264EncoderContext,
    width: u32,
    height: u32,
}

// SAFETY: encoder context is owned and used by the single pipeline thread.
unsafe impl Send for X264Encoder {}

impl X264Encoder {
    fn new(width: u32, height: u32) -> Result<Self> {
        let raw = unsafe { kmsvnc_x264_open(width, height) };
        if raw.is_null() {
            bail!("x264 init failed: {}", x264_last_error());
        }
        Ok(Self { raw, width, height })
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
