use anyhow::{bail, Result};

use crate::kms::dmabuf::DmabufFrame;

#[derive(Debug)]
pub enum VideoFrame {
    CpuBgra {
        width: u32,
        height: u32,
        stride: u32,
        data: Vec<u8>,
    },
    Dmabuf(DmabufFrame),
}

impl VideoFrame {
    pub fn new_cpu_bgra(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self::CpuBgra {
            width,
            height,
            stride: width * 4,
            data,
        }
    }

    pub fn cpu_bgra_mut(&mut self, width: u32, height: u32) -> &mut Vec<u8> {
        match self {
            Self::CpuBgra {
                width: frame_width,
                height: frame_height,
                stride,
                data,
            } => {
                *frame_width = width;
                *frame_height = height;
                *stride = width * 4;
                data
            }
            Self::Dmabuf(_) => {
                *self = Self::new_cpu_bgra(width, height, Vec::new());
                match self {
                    Self::CpuBgra { data, .. } => data,
                    Self::Dmabuf(_) => unreachable!(),
                }
            }
        }
    }

    pub fn as_bgra(&self) -> Result<(u32, u32, u32, &[u8])> {
        match self {
            Self::CpuBgra {
                width,
                height,
                stride,
                data,
            } => Ok((*width, *height, *stride, data.as_slice())),
            Self::Dmabuf(_) => bail!("Frame is DMA-BUF-backed, not CPU BGRA"),
        }
    }

    pub fn as_dmabuf(&self) -> Result<&DmabufFrame> {
        match self {
            Self::Dmabuf(frame) => Ok(frame),
            Self::CpuBgra { .. } => bail!("Frame is CPU BGRA, not DMA-BUF-backed"),
        }
    }

    pub fn validate_cpu_bgra(&self, expected_width: u32, expected_height: u32) -> Result<()> {
        let (width, height, stride, data) = self.as_bgra()?;
        if width != expected_width || height != expected_height {
            bail!(
                "Unexpected frame size {}x{} (expected {}x{})",
                width,
                height,
                expected_width,
                expected_height
            );
        }
        let expected_stride = width * 4;
        if stride != expected_stride {
            bail!("Unexpected BGRA stride {} (expected {})", stride, expected_stride);
        }
        let expected_len = (height * stride) as usize;
        if data.len() != expected_len {
            bail!("Unexpected BGRA buffer length {} (expected {})", data.len(), expected_len);
        }
        Ok(())
    }
}
