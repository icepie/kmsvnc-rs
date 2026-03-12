use std::ffi::c_char;
use std::os::fd::AsRawFd;

use anyhow::{bail, Result};

use crate::encode::{EncodedPacket, VideoCodec, VideoEncoder};
use crate::kms::dmabuf::DmabufFrame;
use crate::video::VideoFrame;

pub struct VaapiEncoder {
    codec: VideoCodec,
    probe: Option<VaapiProbe>,
}

impl VaapiEncoder {
    pub fn new(codec: VideoCodec) -> Self {
        Self { codec, probe: None }
    }

    fn ensure_probe(&mut self, frame: &DmabufFrame) -> Result<()> {
        if self.probe.is_some() {
            return Ok(());
        }
        let probe = VaapiProbe::new(frame)?;
        if !probe.supports_h264() {
            bail!("VAAPI imported the DMA-BUF surface, but this driver does not expose an H.264 encode entrypoint");
        }
        self.probe = Some(probe);
        Ok(())
    }
}

impl VideoEncoder for VaapiEncoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn encode(
        &mut self,
        frame: &VideoFrame,
        _force_keyframe: bool,
        pts: u64,
    ) -> anyhow::Result<EncodedPacket> {
        if self.codec != VideoCodec::H264 {
            bail!("Only H.264 is supported by the experimental VAAPI path right now");
        }
        let dmabuf = frame.as_dmabuf()?;
        self.ensure_probe(dmabuf)?;
        bail!(
            "VAAPI DMA-BUF import and H.264 capability probe succeeded, but bitstream submission is not implemented yet (pts={pts})"
        )
    }
}

struct VaapiProbe {
    raw: *mut VaapiEncoderProbeContext,
}

// SAFETY: the VAAPI probe context is owned by the encoder and only accessed
// mutably from the single capture thread that owns the pipeline.
unsafe impl Send for VaapiProbe {}

impl VaapiProbe {
    fn new(frame: &DmabufFrame) -> Result<Self> {
        let num_objects = frame.objects.len();
        let num_planes = frame.planes.len();
        let mut object_fds = [0_i32; 4];
        let mut object_sizes = [0_u64; 4];
        let mut object_indices = [0_u32; 4];
        let mut offsets = [0_u32; 4];
        let mut pitches = [0_u32; 4];
        let modifier = frame.objects.first().map(|o| o.modifier).unwrap_or(0);

        if num_objects == 0 || num_objects > 4 {
            bail!("Unsupported DMA-BUF object count: {num_objects}");
        }
        if num_planes == 0 || num_planes > 4 {
            bail!("Unsupported DMA-BUF plane count: {num_planes}");
        }

        for (idx, object) in frame.objects.iter().enumerate() {
            object_fds[idx] = object.fd.as_raw_fd();
            object_sizes[idx] = object.size.unwrap_or(0);
        }
        for (idx, plane) in frame.planes.iter().enumerate() {
            object_indices[idx] = plane.object_index as u32;
            offsets[idx] = plane.offset;
            pitches[idx] = plane.pitch;
        }

        let raw = unsafe {
            kmsvnc_vaapi_encoder_open(
                frame.drm_device_fd.as_raw_fd(),
                frame.width,
                frame.height,
                frame.drm_format,
                modifier,
                object_fds.as_ptr(),
                object_sizes.as_ptr(),
                num_objects as u32,
                object_indices.as_ptr(),
                offsets.as_ptr(),
                pitches.as_ptr(),
                num_planes as u32,
            )
        };
        if raw.is_null() {
            bail!("VAAPI encoder probe failed: {}", vaapi_last_error());
        }
        Ok(Self { raw })
    }

    fn supports_h264(&self) -> bool {
        unsafe { kmsvnc_vaapi_encoder_supports_h264(self.raw) != 0 }
    }
}

impl Drop for VaapiProbe {
    fn drop(&mut self) {
        unsafe {
            kmsvnc_vaapi_encoder_close(self.raw);
        }
    }
}

#[repr(C)]
struct VaapiEncoderProbeContext {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn kmsvnc_vaapi_encoder_open(
        drm_fd: i32,
        width: u32,
        height: u32,
        drm_format: u32,
        modifier: u64,
        object_fds: *const i32,
        object_sizes: *const u64,
        num_objects: u32,
        object_indices: *const u32,
        offsets: *const u32,
        pitches: *const u32,
        num_planes: u32,
    ) -> *mut VaapiEncoderProbeContext;
    fn kmsvnc_vaapi_encoder_supports_h264(ctx: *const VaapiEncoderProbeContext) -> i32;
    fn kmsvnc_vaapi_encoder_close(ctx: *mut VaapiEncoderProbeContext);
    fn kmsvnc_vaapi_last_error() -> *const c_char;
}

fn vaapi_last_error() -> String {
    unsafe {
        let ptr = kmsvnc_vaapi_last_error();
        if ptr.is_null() {
            "unknown VAAPI error".into()
        } else {
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}
