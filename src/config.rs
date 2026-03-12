use clap::Parser;

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum EncodingMode {
    Auto,
    Raw,
    Zlib,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum VideoEncoderMode {
    None,
    Software,
    Vaapi,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum VideoCodecMode {
    H264,
    Hevc,
    Av1,
}

#[derive(Parser, Debug)]
#[command(
    name = "kmsvnc",
    about = "KMS-based VNC server with touch & keyboard input"
)]
pub struct Config {
    /// DRM device path (e.g. /dev/dri/card0). Auto-detects if not specified.
    #[arg(short, long)]
    pub device: Option<String>,

    /// DRM connector/output name to capture (e.g. DP-1, HDMI-A-1).
    #[arg(long)]
    pub output: Option<String>,

    /// VNC listen port
    #[arg(short, long, default_value_t = 5900)]
    pub port: u16,

    /// Maximum frames per second
    #[arg(short, long, default_value_t = 30)]
    pub fps: u32,

    /// VNC listen address
    #[arg(short, long, default_value = "0.0.0.0")]
    pub listen: String,

    /// VNC password for authentication (Type 2). No auth if omitted.
    #[arg(long)]
    pub password: Option<String>,

    /// Preferred VNC encoding: raw, zlib, or auto.
    #[arg(long, value_enum, default_value = "raw")]
    pub encoding: EncodingMode,

    /// Experimental video encoder pipeline (not active yet).
    #[arg(long, value_enum, default_value = "none")]
    pub video_encoder: VideoEncoderMode,

    /// Preferred codec for the experimental video pipeline.
    #[arg(long, value_enum, default_value = "h264")]
    pub video_codec: VideoCodecMode,
}
