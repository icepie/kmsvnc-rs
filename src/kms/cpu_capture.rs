use std::ffi::c_void;
use std::ffi::CStr;
use std::fs;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::ptr;

use anyhow::{bail, Context, Result};
use drm::control::{connector, crtc, framebuffer, Device as ControlDevice};
use drm_ffi::drm_sys::{drm_gem_flink, drm_gem_open, DRM_IOCTL_BASE};
use drm_fourcc::{DrmFourcc, DrmModifier};
use rustix::ioctl::{self, ReadWriteOpcode, Updater};
use rustix::mm::{self, MapFlags, ProtFlags};

use super::card::Card;
use super::pixel_format;

use crate::frame_diff::DirtyTiles;

fn exe_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<binary>".into())
}

/// Active output: connector -> encoder -> CRTC chain.
pub struct ActiveOutput {
    pub connector_name: String,
    pub crtc_handle: crtc::Handle,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub fb_handle: framebuffer::Handle,
}

/// Open the first DRI card that has connected outputs.
pub fn open_card() -> Result<(Card, Vec<ActiveOutput>)> {
    let (_path, card, outputs) = open_card_with_path()?;
    Ok((card, outputs))
}

/// Open the first DRI card that has connected outputs and return its path.
pub fn open_card_with_path() -> Result<(String, Card, Vec<ActiveOutput>)> {
    let mut entries: Vec<_> = fs::read_dir("/dev/dri")?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("card"))
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let path = entry.path();
        let path_str = path.to_string_lossy();
        let card = match Card::open(&path_str) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("Cannot open {path_str}: {e}");
                continue;
            }
        };

        match probe_outputs(&card) {
            Ok(outputs) if !outputs.is_empty() => {
                tracing::info!(
                    "KMS: using {path_str} with {} active output(s)",
                    outputs.len()
                );
                return Ok((path_str.into_owned(), card, outputs));
            }
            Ok(_) => {
                tracing::debug!("{path_str}: no active outputs");
            }
            Err(e) => {
                tracing::debug!("{path_str}: probe failed: {e}");
            }
        }
    }

    bail!(
        "No DRI card with active outputs found. \
         Ensure /dev/dri/card* exists and the process has CAP_SYS_ADMIN \
         (try: sudo setcap cap_sys_admin+ep {})",
        exe_path()
    )
}

/// Open a specific DRI card by path.
pub fn open_card_path(path: &str) -> Result<(Card, Vec<ActiveOutput>)> {
    let card = Card::open(path).with_context(|| format!("Cannot open {path}"))?;
    let outputs = probe_outputs(&card)?;
    if outputs.is_empty() {
        bail!("{path}: no active outputs found");
    }
    tracing::info!("KMS: using {path} with {} active output(s)", outputs.len());
    Ok((card, outputs))
}

fn probe_outputs(card: &Card) -> Result<Vec<ActiveOutput>> {
    let res = card.resource_handles()?;
    let mut outputs = Vec::new();

    for &conn_h in res.connectors() {
        let conn = card.get_connector(conn_h, false)?;
        if conn.state() != connector::State::Connected {
            continue;
        }

        let enc_h = match conn.current_encoder() {
            Some(h) => h,
            None => continue,
        };
        let enc = card.get_encoder(enc_h)?;
        let crtc_h = match enc.crtc() {
            Some(h) => h,
            None => continue,
        };
        let crtc_info = card.get_crtc(crtc_h)?;
        let mode = match crtc_info.mode() {
            Some(m) => m,
            None => continue,
        };
        let fb_h = match crtc_info.framebuffer() {
            Some(h) => h,
            None => continue,
        };
        let (x, y) = crtc_info.position();

        let (w, h) = mode.size();
        outputs.push(ActiveOutput {
            connector_name: format!("{conn}"),
            crtc_handle: crtc_h,
            x,
            y,
            width: w as u32,
            height: h as u32,
            fb_handle: fb_h,
        });
    }

    Ok(outputs)
}

// ---------------------------------------------------------------------------
// Persistent DRM capturer with mmap cache
// ---------------------------------------------------------------------------

const MAX_CACHE_ENTRIES: usize = 16;

struct CachedBuffer {
    fb_key: u32,
    gem_handle: drm::buffer::Handle,
    ptr: *mut c_void,
    size: usize,
    format: DrmFourcc,
    pitch: u32,
    _prime_fd: Option<OwnedFd>,
}

struct VaapiCapture {
    gem_handle: drm::buffer::Handle,
    ctx: *mut VaapiContext,
    scratch: Vec<u8>,
}

pub struct Capturer {
    card: Card,
    crtc_handle: crtc::Handle,
    default_fb: framebuffer::Handle,
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
    use_fb2: Option<bool>,
    use_prime: Option<bool>,
    cache: Vec<CachedBuffer>,
    last_fb_key: Option<u32>,
    vaapi_cache: Vec<VaapiCapture>,
    allow_vaapi_readback: bool,
}

// SAFETY: The mmap pointers in CachedBuffer are read-only and their backing
// resources (prime fd or card fd) are kept alive by Capturer.
unsafe impl Send for Capturer {}
unsafe impl Send for VaapiCapture {}

impl Capturer {
    pub fn new(card: Card, output: &ActiveOutput) -> Self {
        Self::new_with_options(card, output, true)
    }

    pub fn new_with_options(card: Card, output: &ActiveOutput, allow_vaapi_readback: bool) -> Self {
        Self {
            crtc_handle: output.crtc_handle,
            default_fb: output.fb_handle,
            src_x: output.x,
            src_y: output.y,
            width: output.width,
            height: output.height,
            use_fb2: None,
            use_prime: None,
            cache: Vec::new(),
            last_fb_key: None,
            vaapi_cache: Vec::new(),
            allow_vaapi_readback,
            card,
        }
    }

    /// Capture a frame into a caller-provided buffer.
    /// Returns `true` if a new frame was captured, `false` if unchanged.
    ///
    /// When `dirty_tiles` is provided AND the buffer already contains a previous
    /// frame (same size), uses incremental tile-by-tile copy for direct-copy
    /// formats (XRGB8888/ARGB8888). Only changed tiles are copied and marked
    /// dirty, avoiding the full-frame memcpy + separate memcmp.
    pub fn capture_into(
        &mut self,
        dst: &mut Vec<u8>,
        force: bool,
        dirty_tiles: Option<&DirtyTiles>,
    ) -> Result<bool> {
        let crtc_info = self
            .card
            .get_crtc(self.crtc_handle)
            .context("Failed to get CRTC")?;
        let fb_handle = crtc_info.framebuffer().unwrap_or(self.default_fb);
        let fb_key = u32::from(fb_handle);

        // Skip capture if fb_handle hasn't changed (same page-flip buffer)
        if !force && self.last_fb_key == Some(fb_key) {
            return Ok(false);
        }
        self.last_fb_key = Some(fb_key);

        // Get the current GEM handle (the true identity of the buffer)
        let current_gem = self.get_gem_handle(fb_handle)?;

        if let Some(idx) = self
            .vaapi_cache
            .iter()
            .position(|v| v.gem_handle == current_gem)
        {
            return self.capture_via_vaapi(idx, dst, dirty_tiles);
        }

        // Cache lookup by GEM handle — supports double/triple buffering where
        // the same fb_handle maps to rotating GEM objects.
        if let Some(idx) = self.cache.iter().position(|e| e.gem_handle == current_gem) {
            // Update fb_key in case it changed (fb_handle recycling detection)
            self.cache[idx].fb_key = fb_key;
            let entry = &self.cache[idx];
            let raw =
                unsafe { std::slice::from_raw_parts(entry.ptr.cast::<u8>(), entry.size) };
            return self.convert_or_incremental(dst, raw, entry.format, entry.pitch, dirty_tiles);
        }

        // Cache miss — map the buffer
        let entry = self.map_buffer(fb_handle, current_gem, dst, dirty_tiles)?;
        let result = if let Some(ref entry) = entry {
            let raw = unsafe { std::slice::from_raw_parts(entry.ptr.cast::<u8>(), entry.size) };
            self.convert_full(dst, raw, entry.format, entry.pitch, dirty_tiles)
        } else {
            Ok(true)
        };

        // Evict oldest entry if cache is full
        if let Some(entry) = entry {
            if self.cache.len() >= MAX_CACHE_ENTRIES {
                let evicted = self.cache.remove(0);
                self.evict_entry(evicted);
            }
            self.cache.push(entry);
        }

        result
    }

    /// Try incremental copy if possible, otherwise fall back to full copy.
    fn convert_or_incremental(
        &self,
        dst: &mut Vec<u8>,
        raw: &[u8],
        format: DrmFourcc,
        pitch: u32,
        dirty_tiles: Option<&DirtyTiles>,
    ) -> Result<bool> {
        let expected_size = (self.width * self.height * 4) as usize;

        // Incremental path: direct-copy format + warm buffer + dirty_tiles available
        if let Some(dt) = dirty_tiles {
            if pixel_format::is_direct_copy(format) && dst.len() == expected_size {
                let changed = pixel_format::copy_rows_incremental(
                    dst, raw, self.width, self.height, pitch, dt,
                );
                return Ok(changed);
            }
        }

        // Full copy fallback
        self.convert_full(dst, raw, format, pitch, dirty_tiles)
    }

    /// Full pixel format conversion. Marks all tiles dirty.
    fn convert_full(
        &self,
        dst: &mut Vec<u8>,
        raw: &[u8],
        format: DrmFourcc,
        pitch: u32,
        dirty_tiles: Option<&DirtyTiles>,
    ) -> Result<bool> {
        pixel_format::convert_to_bgra_into(dst, raw, self.width, self.height, pitch, format)
            .map_err(|e| anyhow::anyhow!(e))?;
        if let Some(dt) = dirty_tiles {
            dt.set_all();
        }
        Ok(true)
    }

    /// Capture a frame. If `force` is true, always capture regardless of fb_handle.
    pub fn capture(&mut self, force: bool) -> Result<Option<Vec<u8>>> {
        let mut dst = Vec::new();
        if self.capture_into(&mut dst, force, None)? {
            Ok(Some(dst))
        } else {
            Ok(None)
        }
    }

    /// Get the current GEM handle for a framebuffer, to detect fb_handle recycling.
    fn get_gem_handle(&self, fb_handle: framebuffer::Handle) -> Result<drm::buffer::Handle> {
        match self.use_fb2 {
            Some(true) | None => {
                if let Ok(info) = self.card.get_planar_framebuffer(fb_handle) {
                    if let Some(gem) = info.buffers()[0] {
                        return Ok(gem);
                    }
                }
                if self.use_fb2 == Some(true) {
                    bail!("GET_FB2 failed for gem handle query");
                }
            }
            Some(false) => {}
        }
        let info = self
            .card
            .get_framebuffer(fb_handle)
            .context("GET_FB failed for gem handle query")?;
        info.buffer().context("No buffer handle from GET_FB")
    }

    fn capture_via_vaapi(
        &mut self,
        idx: usize,
        dst: &mut Vec<u8>,
        dirty_tiles: Option<&DirtyTiles>,
    ) -> Result<bool> {
        let required = (self.width * self.height * 4) as usize;
        let vaapi = self.vaapi_cache.get_mut(idx).context("VAAPI state missing")?;
        if vaapi.scratch.len() != required {
            vaapi.scratch.resize(required, 0);
        }
        let ok = unsafe {
            kmsvnc_vaapi_capture(vaapi.ctx, vaapi.scratch.as_mut_ptr(), vaapi.scratch.len())
        };
        if ok == 0 {
            bail!("VAAPI capture failed: {}", vaapi_last_error());
        }
        if let Some(dt) = dirty_tiles {
            if dst.len() == required {
                return Ok(pixel_format::copy_bgra_rows_incremental(
                    dst,
                    &vaapi.scratch,
                    self.width,
                    self.height,
                    dt,
                ));
            }
        }
        dst.clear();
        dst.extend_from_slice(&vaapi.scratch);
        if let Some(dt) = dirty_tiles {
            dt.set_all();
        }
        Ok(true)
    }

    fn map_buffer(
        &mut self,
        fb_handle: framebuffer::Handle,
        gem_handle: drm::buffer::Handle,
        dst: &mut Vec<u8>,
        dirty_tiles: Option<&DirtyTiles>,
    ) -> Result<Option<CachedBuffer>> {
        // Try FB2 first (gives pixel format), latch choice after first success/failure
        match self.use_fb2 {
            Some(true) | None => match self.map_fb2(fb_handle, gem_handle, dst, dirty_tiles) {
                Ok(entry) => {
                    self.use_fb2 = Some(true);
                    return Ok(entry);
                }
                Err(e) => {
                    if self.use_fb2 == Some(true) {
                        return Err(e);
                    }
                    tracing::debug!("GET_FB2 failed ({e}), trying GET_FB");
                }
            },
            Some(false) => {}
        }

        let entry = self.map_fb1(fb_handle)?;
        self.use_fb2 = Some(false);
        Ok(Some(entry))
    }

    fn map_fb2(
        &mut self,
        fb_handle: framebuffer::Handle,
        _gem_handle: drm::buffer::Handle,
        dst: &mut Vec<u8>,
        dirty_tiles: Option<&DirtyTiles>,
    ) -> Result<Option<CachedBuffer>> {
        let info = self
            .card
            .get_planar_framebuffer(fb_handle)
            .context("GET_FB2 failed")?;

        if let Some(modifier) = info.modifier() {
            if modifier != DrmModifier::Linear {
                bail!(
                    "Framebuffer has non-linear modifier ({modifier:?}); \
                     tiled buffers cannot be read via mmap"
                );
            }
        }

        let gem_handle = info.buffers()[0].context("No buffer handle in framebuffer")?;
        let pitch = info.pitches()[0];
        let format = info.pixel_format();
        tracing::debug!(
            "FB2: format={format:?}, pitch={pitch}, modifier={:?}",
            info.modifier()
        );

        match self.map_gem_cached(fb_handle, gem_handle, pitch, format) {
            Ok(entry) => Ok(Some(entry)),
            Err(cpu_err) => {
                if !self.allow_vaapi_readback {
                    return Err(cpu_err);
                }
                tracing::debug!("CPU mapping failed for FB2 ({cpu_err}), trying VAAPI");
                tracing::debug!(
                    "Creating VAAPI capture for fb={} gem={}",
                    u32::from(fb_handle),
                    u32::from(gem_handle)
                );
                let vaapi = VaapiCapture::new(
                    &self.card,
                    gem_handle,
                    self.src_x,
                    self.src_y,
                    self.width,
                    self.height,
                    info.size().0,
                    info.size().1,
                    format,
                    info.modifier().unwrap_or(DrmModifier::Invalid),
                    &info.pitches(),
                    &info.offsets(),
                    info.buffers().iter().take_while(|h| h.is_some()).count() as u32,
                )?;
                if self.vaapi_cache.len() >= MAX_CACHE_ENTRIES {
                    self.vaapi_cache.remove(0);
                }
                self.vaapi_cache.push(vaapi);
                self.capture_via_vaapi(self.vaapi_cache.len() - 1, dst, dirty_tiles)?;
                Ok(None)
            }
        }
    }

    fn map_fb1(&mut self, fb_handle: framebuffer::Handle) -> Result<CachedBuffer> {
        let info = self
            .card
            .get_framebuffer(fb_handle)
            .context("GET_FB failed")?;

        let gem_handle = info.buffer().with_context(|| {
            format!(
                "No buffer handle from GET_FB. \
                 CAP_SYS_ADMIN is required (try: sudo setcap cap_sys_admin+ep {})",
                exe_path()
            )
        })?;

        let pitch = info.pitch();
        let bpp = info.bpp();
        let depth = info.depth();

        let format = match (bpp, depth) {
            (32, 24) => DrmFourcc::Xrgb8888,
            (32, 32) => DrmFourcc::Argb8888,
            (16, 16) => DrmFourcc::Rgb565,
            _ => bail!("Unsupported framebuffer format: {bpp}bpp depth={depth}"),
        };

        self.map_gem_cached(fb_handle, gem_handle, pitch, format)
    }

    fn map_gem_cached(
        &mut self,
        fb_handle: framebuffer::Handle,
        gem_handle: drm::buffer::Handle,
        pitch: u32,
        format: DrmFourcc,
    ) -> Result<CachedBuffer> {
        let size = (self.height as usize) * (pitch as usize);
        let fb_key = u32::from(fb_handle);

        // Try PRIME first, latch choice after first success/failure
        match self.use_prime {
            Some(true) | None => {
                match self.map_prime_cached(fb_key, gem_handle, size, format, pitch) {
                    Ok(entry) => {
                        self.use_prime = Some(true);
                        return Ok(entry);
                    }
                    Err(e) => {
                        if self.use_prime == Some(true) {
                            return Err(e);
                        }
                        tracing::debug!("PRIME mmap failed ({e}), trying dumb buffer mmap");
                    }
                }
            }
            Some(false) => {}
        }

        let entry = self.map_dumb_cached(fb_key, gem_handle, size, format, pitch)?;
        self.use_prime = Some(false);
        Ok(entry)
    }

    fn map_prime_cached(
        &self,
        fb_key: u32,
        gem_handle: drm::buffer::Handle,
        size: usize,
        format: DrmFourcc,
        pitch: u32,
    ) -> Result<CachedBuffer> {
        let prime_fd: OwnedFd = self
            .card
            .buffer_to_prime_fd(gem_handle, drm::RDWR)
            .context("PRIME export failed")?;

        let ptr = unsafe {
            mm::mmap(
                ptr::null_mut(),
                size,
                ProtFlags::READ,
                MapFlags::SHARED,
                &prime_fd,
                0,
            )
            .context("PRIME mmap failed")?
        };

        Ok(CachedBuffer {
            fb_key,
            gem_handle,
            ptr,
            size,
            format,
            pitch,
            _prime_fd: Some(prime_fd),
        })
    }

    fn map_dumb_cached(
        &self,
        fb_key: u32,
        gem_handle: drm::buffer::Handle,
        size: usize,
        format: DrmFourcc,
        pitch: u32,
    ) -> Result<CachedBuffer> {
        let reopened = flink_open_gem(self.card.as_fd(), u32::from(gem_handle))?;
        let reopened_handle = drm::buffer::Handle::from(
            NonZeroU32::new(reopened.handle).context("DRM_IOCTL_GEM_OPEN returned handle 0")?,
        );
        let map_result = drm_ffi::mode::dumbbuffer::map(self.card.as_fd(), reopened.handle, 0, 0)
            .context("DRM_IOCTL_MODE_MAP_DUMB failed")?;

        let ptr = unsafe {
            mm::mmap(
                ptr::null_mut(),
                size,
                ProtFlags::READ,
                MapFlags::SHARED,
                self.card.as_fd(),
                map_result.offset,
            )
            .context("dumb buffer mmap failed")?
        };

        Ok(CachedBuffer {
            fb_key,
            gem_handle: reopened_handle,
            ptr,
            size,
            format,
            pitch,
            _prime_fd: None,
        })
    }

    fn evict_entry(&self, entry: CachedBuffer) {
        unsafe {
            let _ = mm::munmap(entry.ptr, entry.size);
        }
        let _ = self.card.close_buffer(entry.gem_handle);
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        for entry in self.cache.drain(..) {
            unsafe {
                let _ = mm::munmap(entry.ptr, entry.size);
            }
            let _ = self.card.close_buffer(entry.gem_handle);
        }
        self.vaapi_cache.clear();
    }
}

fn flink_open_gem(fd: std::os::fd::BorrowedFd<'_>, handle: u32) -> Result<drm_gem_open> {
    let mut flink = drm_gem_flink {
        handle,
        ..Default::default()
    };
    unsafe {
        ioctl::ioctl(fd, Updater::<ReadWriteOpcode<DRM_IOCTL_BASE, 0x0a, drm_gem_flink>, _>::new(&mut flink))
    }
    .context("DRM_IOCTL_GEM_FLINK failed")?;

    drm_ffi::gem::open(fd, flink.name).context("DRM_IOCTL_GEM_OPEN failed")
}

impl VaapiCapture {
    fn new(
        card: &Card,
        gem_handle: drm::buffer::Handle,
        src_x: u32,
        src_y: u32,
        width: u32,
        height: u32,
        fb_width: u32,
        fb_height: u32,
        format: DrmFourcc,
        modifier: DrmModifier,
        pitches: &[u32; 4],
        offsets: &[u32; 4],
        num_planes: u32,
    ) -> Result<Self> {
        let prime_fd = card
            .buffer_to_prime_fd(gem_handle, drm::RDWR)
            .context("PRIME export failed for VAAPI")?;
        let modifier = match modifier {
            DrmModifier::Invalid => 0,
            _ => modifier.into(),
        };
        let ctx = unsafe {
            kmsvnc_vaapi_open(
                card.as_fd().as_raw_fd(),
                prime_fd.as_raw_fd(),
                src_x,
                src_y,
                width,
                height,
                fb_width,
                fb_height,
                format as u32,
                modifier,
                pitches.as_ptr(),
                offsets.as_ptr(),
                num_planes,
            )
        };
        if ctx.is_null() {
            bail!("VAAPI init failed: {}", vaapi_last_error());
        }
        Ok(Self {
            gem_handle,
            ctx,
            scratch: Vec::new(),
        })
    }
}

impl Drop for VaapiCapture {
    fn drop(&mut self) {
        unsafe {
            kmsvnc_vaapi_close(self.ctx);
        }
    }
}

#[repr(C)]
struct VaapiContext {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn kmsvnc_vaapi_open(
        drm_fd: i32,
        prime_fd: i32,
        src_x: u32,
        src_y: u32,
        width: u32,
        height: u32,
        fb_width: u32,
        fb_height: u32,
        drm_format: u32,
        modifier: u64,
        pitches: *const u32,
        offsets: *const u32,
        num_planes: u32,
    ) -> *mut VaapiContext;
    fn kmsvnc_vaapi_capture(ctx: *mut VaapiContext, dst: *mut u8, dst_len: usize) -> i32;
    fn kmsvnc_vaapi_close(ctx: *mut VaapiContext);
    fn kmsvnc_vaapi_last_error() -> *const std::ffi::c_char;
}

fn vaapi_last_error() -> String {
    unsafe {
        let ptr = kmsvnc_vaapi_last_error();
        if ptr.is_null() {
            "unknown VAAPI error".into()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}
