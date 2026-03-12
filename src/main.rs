mod config;
mod encode;
mod frame_diff;
mod input;
mod kms;
mod transport;
mod video;
mod vnc;

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

use config::{Config, EncodingMode, VideoCodecMode, VideoEncoderMode};
use encode::{software::SoftwareEncoder, vaapi::VaapiEncoder, VideoCodec, VideoEncoder};
use frame_diff::DirtyTiles;
use kms::capture;
use kms::dmabuf::DmabufCapturer;
use kms::fbdev::FbdevCapture;
use transport::{null::NullSink, PacketSink};
use video::VideoFrame;
use vnc::server::{self, EncodingPreference, InputEvent};

/// A boxed capture function: writes one frame into the provided frame object.
/// Returns `true` if a new frame was captured, `false` if unchanged.
/// `dirty_tiles` is provided for incremental tile-level capture.
type CaptureFn =
    Box<dyn FnMut(bool, &mut VideoFrame, Option<&DirtyTiles>) -> Result<bool> + Send>;
type VideoFrameSourceFn = Box<dyn FnMut() -> Result<VideoFrame> + Send>;

type EncoderBox = Box<dyn VideoEncoder + Send>;
type PacketSinkBox = Box<dyn PacketSink + Send>;

enum CaptureBackend {
    Drm {
        device_path: String,
        output_name: String,
    },
    Fbdev,
}

struct ExperimentalPipeline {
    encoder: EncoderBox,
    sink: PacketSinkBox,
    pts: u64,
    frame_source: Option<VideoFrameSourceFn>,
}

impl ExperimentalPipeline {
    fn process(&mut self, frame: &VideoFrame, force_keyframe: bool) -> Result<()> {
        let encode_frame = if let Some(source) = self.frame_source.as_mut() {
            source()?
        } else {
            clone_video_frame(frame)?
        };
        let packet = self.encoder.encode(&encode_frame, force_keyframe, self.pts)?;
        self.pts = self.pts.wrapping_add(1);
        self.sink.submit(packet)
    }
}

/// Try to set up DRM capture for a specific card path.
fn try_drm_capture(
    path: &str,
    output_name: Option<&str>,
) -> Result<(u32, u32, VideoFrame, CaptureFn, CaptureBackend)> {
    let (card, outputs) = capture::open_card_path(path)?;
    let output = select_output(&outputs, output_name)?;
    let width = output.width;
    let height = output.height;
    let output_name = output.connector_name.clone();
    tracing::info!("Output: {} ({}x{})", output_name, width, height);
    let mut capturer = capture::Capturer::new(card, output);
    let initial_data = capturer
        .capture(true)?
        .expect("first capture must produce a frame");
    let capture_fn: CaptureFn =
        Box::new(move |force, frame, dt| {
            let dst = frame.cpu_bgra_mut(width, height);
            capturer.capture_into(dst, force, dt)
        });
    Ok((
        width,
        height,
        VideoFrame::new_cpu_bgra(width, height, initial_data),
        capture_fn,
        CaptureBackend::Drm {
            device_path: path.to_string(),
            output_name,
        },
    ))
}

fn select_output<'a>(
    outputs: &'a [capture::ActiveOutput],
    output_name: Option<&str>,
) -> Result<&'a capture::ActiveOutput> {
    if let Some(name) = output_name {
        return outputs
            .iter()
            .find(|o| o.connector_name == name)
            .with_context(|| {
                let available = outputs
                    .iter()
                    .map(|o| o.connector_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Output {name} not found. Available outputs: {available}")
            });
    }
    outputs.first().context("No active outputs found")
}

/// Try to set up fbdev capture for a specific device path.
fn try_fbdev_capture(path: &str) -> Result<(u32, u32, VideoFrame, CaptureFn, CaptureBackend)> {
    let fbdev = FbdevCapture::open(path)?;
    let width = fbdev.width();
    let height = fbdev.height();
    let initial_data = fbdev.capture_frame()?;
    let capture_fn: CaptureFn = Box::new(move |_force, frame, _dt| {
        let dst = frame.cpu_bgra_mut(width, height);
        fbdev.capture_frame_into(dst)?;
        Ok(true)
    });
    Ok((
        width,
        height,
        VideoFrame::new_cpu_bgra(width, height, initial_data),
        capture_fn,
        CaptureBackend::Fbdev,
    ))
}

/// Set up capture with fallback chain: DRM (PRIME/dumb) -> fbdev.
fn setup_capture(config: &Config) -> Result<(u32, u32, VideoFrame, CaptureFn, CaptureBackend)> {
    if let Some(ref path) = config.device {
        // User specified a device — try as DRM first, then as fbdev
        match try_drm_capture(path, config.output.as_deref()) {
            Ok(result) => return Ok(result),
            Err(drm_err) => {
                tracing::debug!("DRM capture failed for {path}: {drm_err}");
                match try_fbdev_capture(path) {
                    Ok(result) => return Ok(result),
                    Err(fb_err) => {
                        bail!("Cannot use {path} as DRM ({drm_err:#}) or fbdev ({fb_err:#})");
                    }
                }
            }
        }
    }

    // Auto-detect: try all DRM cards first
    match capture::open_card_with_path() {
        Ok((device_path, card, outputs)) => {
            let output = select_output(&outputs, config.output.as_deref())?;
            let width = output.width;
            let height = output.height;
            let output_name = output.connector_name.clone();
            tracing::info!("Output: {} ({}x{})", output_name, width, height);
            let mut capturer = capture::Capturer::new(card, output);
            let initial_data = capturer
                .capture(true)?
                .expect("first capture must produce a frame");
            let capture_fn: CaptureFn = Box::new(move |force, frame, dt| {
                let dst = frame.cpu_bgra_mut(width, height);
                capturer.capture_into(dst, force, dt)
            });
            return Ok((
                width,
                height,
                VideoFrame::new_cpu_bgra(width, height, initial_data),
                capture_fn,
                CaptureBackend::Drm {
                    device_path,
                    output_name,
                },
            ));
        }
        Err(drm_err) => {
            tracing::debug!("DRM auto-detect failed: {drm_err}");
        }
    }

    // Fall back to fbdev
    let mut fb_entries: Vec<_> = fs::read_dir("/dev")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with("fb")))
        .collect();
    fb_entries.sort_by_key(|e| e.file_name());

    for entry in &fb_entries {
        let path = entry.path();
        let path_str = path.to_string_lossy();
        match try_fbdev_capture(&path_str) {
            Ok(result) => return Ok(result),
            Err(e) => {
                tracing::debug!("fbdev {path_str} failed: {e}");
            }
        }
    }

    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<binary>".into());
    bail!(
        "No usable capture device found. Tried all /dev/dri/card* (DRM) \
         and /dev/fb* (fbdev). Ensure a display is active and the process \
         has CAP_SYS_ADMIN (try: sudo setcap cap_sys_admin+ep {exe})"
    )
}

fn select_video_codec(mode: &VideoCodecMode) -> VideoCodec {
    match mode {
        VideoCodecMode::H264 => VideoCodec::H264,
        VideoCodecMode::Hevc => VideoCodec::Hevc,
        VideoCodecMode::Av1 => VideoCodec::Av1,
    }
}

fn setup_encoder(config: &Config) -> Option<EncoderBox> {
    let codec = select_video_codec(&config.video_codec);
    match config.video_encoder {
        VideoEncoderMode::None => None,
        VideoEncoderMode::Software => Some(Box::new(SoftwareEncoder::new(codec))),
        VideoEncoderMode::Vaapi => Some(Box::new(VaapiEncoder::new(codec))),
    }
}

fn setup_pipeline(config: &Config) -> Option<ExperimentalPipeline> {
    let encoder = setup_encoder(config)?;
    Some(ExperimentalPipeline {
        encoder,
        sink: Box::new(NullSink::new()),
        pts: 0,
        frame_source: None,
    })
}

fn setup_pipeline_frame_source(
    config: &Config,
    backend: &CaptureBackend,
) -> Result<Option<VideoFrameSourceFn>> {
    if !matches!(config.video_encoder, VideoEncoderMode::Vaapi) {
        return Ok(None);
    }
    let CaptureBackend::Drm {
        device_path,
        output_name,
    } = backend
    else {
        tracing::warn!("VAAPI video pipeline requires a DRM capture backend");
        return Ok(None);
    };

    let (card, outputs) = capture::open_card_path(device_path)?;
    let output = outputs
        .iter()
        .find(|o| o.connector_name == *output_name)
        .with_context(|| format!("Output {output_name} disappeared before pipeline setup"))?;
    let mut capturer = DmabufCapturer::new(card, output);
    Ok(Some(Box::new(move || Ok(VideoFrame::Dmabuf(capturer.capture()?)))))
}

fn clone_video_frame(frame: &VideoFrame) -> Result<VideoFrame> {
    match frame {
        VideoFrame::CpuBgra {
            width,
            height,
            stride,
            data,
        } => Ok(VideoFrame::CpuBgra {
            width: *width,
            height: *height,
            stride: *stride,
            data: data.clone(),
        }),
        VideoFrame::Dmabuf(_) => bail!("DMA-BUF frame cloning is not supported"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::parse();

    check_permissions();

    let (width, height, initial_data, capture_fn, backend) = setup_capture(&config)?;
    let mut pipeline = setup_pipeline(&config);
    if let Some(ref mut pipeline) = pipeline {
        pipeline.frame_source = setup_pipeline_frame_source(&config, &backend)?;
    }

    // Shared dirty tile accumulator between capture thread and VNC server
    let dirty_tiles = Arc::new(DirtyTiles::new(width, height));

    // Frame channel: latest full BGRA buffer
    let (frame_tx, frame_rx) = watch::channel(Arc::new(initial_data));

    // Capture request channel: VNC clients signal when they need a frame
    let (capture_req_tx, capture_req_rx) = std_mpsc::channel::<()>();

    // Input event channel
    let (input_tx, mut input_rx) = mpsc::channel::<InputEvent>(256);

    // Shutdown flag for the capture loop
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_capture = shutdown.clone();

    let fps = config.fps;
    let dirty_tiles_capture = dirty_tiles.clone();

    // Spawn capture loop (on-demand, driven by client requests)
    let capture_handle = tokio::task::spawn_blocking(move || {
        capture_loop(
            capture_fn,
            frame_tx,
            capture_req_rx,
            shutdown_capture,
            fps,
            dirty_tiles_capture,
            pipeline,
        )
    });

    // Spawn input handler
    let input_handle = tokio::spawn(async move { input_loop(&mut input_rx, width, height).await });

    // Share password across client tasks
    let password = Arc::new(config.password);
    let encoding_pref = match config.encoding {
        EncodingMode::Auto => EncodingPreference::Auto,
        EncodingMode::Raw => EncodingPreference::Raw,
        EncodingMode::Zlib => EncodingPreference::Zlib,
    };

    // VNC server listen loop
    let addr = format!("{}:{}", config.listen, config.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;
    tracing::info!("VNC server listening on {addr}");

    // Graceful shutdown on Ctrl+C
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Shutting down...");
        let _ = shutdown_tx.send(()).await;
    });

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (stream, peer) = accept?;
                tracing::info!("VNC client connected: {peer}");
                let frame_rx = frame_rx.clone();
                let capture_req_tx = capture_req_tx.clone();
                let input_tx = input_tx.clone();
                let password = password.clone();
                let dirty_tiles = dirty_tiles.clone();
                let encoding_pref = encoding_pref;
                let w = width as u16;
                let h = height as u16;
                tokio::spawn(async move {
                    if let Err(e) = server::handle_client(stream, w, h, frame_rx, capture_req_tx, input_tx, password.as_deref(), dirty_tiles, encoding_pref).await {
                        tracing::info!("Client {peer} disconnected: {e}");
                    }
                });
            }
            _ = shutdown_rx.recv() => {
                break;
            }
        }
    }

    // Signal capture loop to stop and wait for it
    shutdown.store(true, Ordering::Relaxed);
    drop(input_tx);
    input_handle.abort();
    let _ = capture_handle.await;

    Ok(())
}

/// Adaptive capture mode: switches between on-demand and polling based on request frequency.
enum CaptureMode {
    /// Wait for explicit capture requests; always force-capture to ensure fresh frames.
    OnDemand,
    /// Actively poll at the given interval; skip unchanged frames to save CPU.
    Polling { interval: Duration },
}

fn capture_loop(
    mut capture_fn: CaptureFn,
    frame_tx: watch::Sender<Arc<VideoFrame>>,
    capture_req_rx: std_mpsc::Receiver<()>,
    shutdown: Arc<AtomicBool>,
    fps: u32,
    dirty_tiles: Arc<DirtyTiles>,
    mut pipeline: Option<ExperimentalPipeline>,
) {
    let poll_interval = Duration::from_millis(1000 / fps.max(1) as u64);
    let mut mode = CaptureMode::OnDemand;
    let mut last_request_time: Option<Instant> = None;
    let mut fast_request_count = 0u32;

    // Buffer pool: try to reuse the Vec from the previous Arc
    let mut reuse_frame: Option<VideoFrame> = None;

    // Idle backoff: reduce capture rate when screen content is unchanged.
    // Consecutive unchanged captures increase idle_streak; any change resets it.
    let mut idle_streak = 0u32;

    loop {
        let timeout = match mode {
            CaptureMode::OnDemand => Duration::from_millis(100),
            CaptureMode::Polling { interval } => {
                // Exponential backoff when idle: double interval every 5 unchanged
                // captures, up to 4x the base interval.
                let shift = (idle_streak / 5).min(2);
                interval * (1 << shift)
            }
        };

        match capture_req_rx.recv_timeout(timeout) {
            Ok(()) => {
                // Check request interval to detect high-frequency clients
                let now = Instant::now();
                if let Some(last) = last_request_time {
                    if now.duration_since(last) < Duration::from_millis(100) {
                        fast_request_count += 1;
                        if fast_request_count >= 3 {
                            if matches!(mode, CaptureMode::OnDemand) {
                                tracing::debug!("Switching to polling mode ({}fps)", fps);
                            }
                            mode = CaptureMode::Polling {
                                interval: poll_interval,
                            };
                        }
                    } else {
                        fast_request_count = 0;
                    }
                }
                last_request_time = Some(now);

                // Drain any additional queued requests (coalesce)
                while capture_req_rx.try_recv().is_ok() {}

                match mode {
                    CaptureMode::OnDemand => {
                        // On-demand: capture immediately on each client request
                        do_capture(
                            &mut capture_fn,
                            &frame_tx,
                            false,
                            &mut reuse_frame,
                            &dirty_tiles,
                            pipeline.as_mut(),
                        );
                    }
                    CaptureMode::Polling { .. } => {
                        // Polling: timer drives captures — don't capture here.
                        // The VNC server will get the response on the next timer tick.
                        // This prevents double-captures (timer + request) which
                        // effectively doubled the capture rate.
                    }
                }
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                match mode {
                    CaptureMode::Polling { .. } => {
                        // Check if we should switch back to on-demand
                        if let Some(last) = last_request_time {
                            if Instant::now().duration_since(last) > Duration::from_millis(500) {
                                tracing::debug!("Switching to on-demand mode");
                                mode = CaptureMode::OnDemand;
                                fast_request_count = 0;
                                idle_streak = 0;
                            } else {
                                // Timer-driven capture with idle backoff
                                let changed = do_capture(
                                    &mut capture_fn,
                                    &frame_tx,
                                    false,
                                    &mut reuse_frame,
                                    &dirty_tiles,
                                    pipeline.as_mut(),
                                );
                                if changed {
                                    idle_streak = 0;
                                } else {
                                    idle_streak = idle_streak.saturating_add(1);
                                }
                            }
                        }
                    }
                    CaptureMode::OnDemand => {
                        // Just check for shutdown
                        if shutdown.load(Ordering::Relaxed) {
                            tracing::debug!("Capture loop shutting down");
                            break;
                        }
                    }
                }
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                tracing::debug!("Capture request channel closed");
                break;
            }
        }
    }
}

/// Perform a capture and send the result if a new frame was obtained.
/// Returns `true` if the frame content actually changed.
fn do_capture(
    capture_fn: &mut CaptureFn,
    frame_tx: &watch::Sender<Arc<VideoFrame>>,
    force: bool,
    reuse_frame: &mut Option<VideoFrame>,
    dirty_tiles: &DirtyTiles,
    pipeline: Option<&mut ExperimentalPipeline>,
) -> bool {
    let mut frame = reuse_frame.take().unwrap_or_else(|| VideoFrame::new_cpu_bgra(0, 0, Vec::new()));

    match capture_fn(force, &mut frame, Some(dirty_tiles)) {
        Ok(true) => {
            if let Some(p) = pipeline {
                if let Err(e) = p.process(&frame, force) {
                    tracing::warn!("Experimental video pipeline failed: {e}");
                }
            }
            let new_arc = Arc::new(frame);
            let old_arc = frame_tx.send_replace(new_arc);
            if let Ok(old_frame) = Arc::try_unwrap(old_arc) {
                *reuse_frame = Some(old_frame);
            }
            true
        }
        Ok(false) => {
            // Frame unchanged — notify VNC server to unblock changed().await
            // (no dirty tiles set, so server sends empty FramebufferUpdate)
            frame_tx.send_modify(|_| {});
            *reuse_frame = Some(frame);
            false
        }
        Err(e) => {
            tracing::warn!("Capture failed: {e}");
            *reuse_frame = Some(frame);
            false
        }
    }
}

async fn input_loop(input_rx: &mut mpsc::Receiver<InputEvent>, width: u32, height: u32) {
    let mut mouse = match input::mouse::VirtualMouse::new(width, height) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!("Failed to create virtual mouse: {e}");
            tracing::warn!("Pointer input will be disabled");
            None
        }
    };

    let keyboard = match input::keyboard::VirtualKeyboard::new() {
        Ok(k) => Some(k),
        Err(e) => {
            tracing::warn!("Failed to create virtual keyboard: {e}");
            tracing::warn!("Keyboard input will be disabled");
            None
        }
    };

    while let Some(event) = input_rx.recv().await {
        match event {
            InputEvent::Pointer { button_mask, x, y } => {
                if let Some(ref mut m) = mouse {
                    if let Err(e) = m.handle_pointer(button_mask, x, y) {
                        tracing::warn!("Pointer event error: {e}");
                    }
                }
            }
            InputEvent::Key { down, keysym } => {
                if let Some(ref k) = keyboard {
                    if let Err(e) = k.handle_key(down, keysym) {
                        tracing::warn!("Key event error: {e}");
                    }
                }
            }
        }
    }
}

/// Check for required capabilities and permissions, warn early on problems.
fn check_permissions() {
    if !has_cap_sys_admin() {
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<binary>".into());
        tracing::warn!(
            "Process lacks CAP_SYS_ADMIN — DRM framebuffer access will likely fail. \
             Run as root or: sudo setcap cap_sys_admin+ep {exe}"
        );
    }

    match std::fs::metadata("/dev/uinput") {
        Ok(meta) => {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/uinput")
            {
                Ok(_) => {}
                Err(_) => {
                    tracing::warn!(
                        "/dev/uinput is not writable — input forwarding will be disabled. \
                         Fix: sudo usermod -aG input $USER (then re-login), \
                         or: sudo chmod 0660 /dev/uinput"
                    );
                }
            }
            let _ = meta;
        }
        Err(_) => {
            tracing::warn!(
                "/dev/uinput does not exist — input forwarding will be disabled. \
                 Fix: sudo modprobe uinput"
            );
        }
    }
}

/// Check whether the current process has CAP_SYS_ADMIN in its effective set.
fn has_cap_sys_admin() -> bool {
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return false,
    };
    for line in status.lines() {
        if let Some(hex) = line.strip_prefix("CapEff:\t") {
            let caps = match u64::from_str_radix(hex.trim(), 16) {
                Ok(v) => v,
                Err(_) => return false,
            };
            return (caps & (1 << 21)) != 0;
        }
    }
    false
}
