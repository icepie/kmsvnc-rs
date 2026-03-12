# kmsvnc

A lightweight VNC server for Linux that captures the screen directly from the KMS/DRM framebuffer and forwards input via virtual uinput devices. Works independently of any display server — it operates the same whether X11, Wayland, or nothing at all is running.

## Features

- **KMS/DRM screen capture** — reads the GPU framebuffer directly, works without a display server
- **Dumb buffer fallback** — works with simpledrm, vkms, and other drivers that lack PRIME export
- **Linux fbdev fallback** — captures from `/dev/fb*` when DRM is unavailable entirely
- **Minimal RFB protocol** — standard VNC clients (TigerVNC, Remmina, KRDC, etc.) connect out of the box
- **Virtual touch input** — VNC pointer events are translated to Linux multitouch events via uinput
- **Virtual keyboard** — VNC key events are mapped from X11 keysyms to Linux input codes
- **Incremental updates** — 64px tile-based dirty rectangle detection to reduce bandwidth
- **Pixel format negotiation** — respects client `SetPixelFormat` requests (any bpp/endianness/shifts)
- **Multiple DRM formats** — XRGB8888, ARGB8888, XBGR8888, ABGR8888, RGB565
- **VNC authentication** — optional password-based authentication (RFB Security Type 2, DES challenge-response)

## Installation

```bash
cargo install kmsvnc
```

To build from source, see [BUILDING.md](BUILDING.md).

## Usage

```bash
# Run as root (simplest)
sudo $(which kmsvnc)

# Or grant capabilities
sudo setcap cap_sys_admin+ep $(which kmsvnc)
sudo usermod -aG input $USER  # for /dev/uinput access (re-login required)
kmsvnc
```

Then connect any VNC client to `localhost:5900`.

### Options

```
--device <path>      Capture device path: /dev/dri/card*, /dev/fb* (default: auto-detect)
--output <name>      DRM connector/output name to capture, e.g. DP-1
--encoding <mode>    VNC encoding: raw, zlib, or auto (default: raw)
--video-encoder <e>  Experimental video pipeline: none, software, vaapi
--video-codec <c>    Experimental codec preference: h264, hevc, av1
--video-stream-listen Experimental Annex B TCP listen address (default: 0.0.0.0)
--video-stream-port  Experimental Annex B TCP port, 0 disables output
--video-fps          Experimental video stream FPS limit
--video-width        Experimental video stream output width
--video-height       Experimental video stream output height
--video-capture-scale Use experimental VAAPI-scaled capture for software video
--port <port>        VNC listen port (default: 5900)
--fps <fps>          Capture frame rate (default: 30)
--listen <addr>      Listen address (default: 0.0.0.0)
--password <pass>    Require VNC password authentication (default: no auth)
```

### Logging

Control log verbosity with the `RUST_LOG` environment variable:

```bash
RUST_LOG=info sudo $(which kmsvnc)    # default useful output
RUST_LOG=debug sudo $(which kmsvnc)   # detailed diagnostics
```

### Experimental H.264 Stream

When using software H.264 encoding, you can expose an Annex B stream over TCP:

```bash
sudo ./target/release/kmsvnc \
  --device /dev/dri/card1 \
  --output DP-1 \
  --video-encoder software \
  --video-codec h264 \
  --video-width 1280 \
  --video-height 720 \
  --video-fps 10 \
  --video-stream-port 6000
```

By default, software video encoding keeps using the existing stable capture path.
Only add `--video-capture-scale` if you specifically want to test the experimental
VAAPI-scaled capture source.

Then connect with:

```bash
ffplay -fflags nobuffer -flags low_delay -f h264 tcp://127.0.0.1:6000?listen=0
```

## Limitations

- Raw encoding only (no compression — best used on LAN)
- No encryption (VNC authentication uses DES challenge-response but traffic is unencrypted — use SSH tunneling for security)
- Defaults to the first connected display output unless `--output` is set
- Clipboard forwarding not implemented

## Troubleshooting

See [TROUBLESHOOTING.md](TROUBLESHOOTING.md).
