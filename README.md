# Camera Studio

A camera app for the Linux desktop (GNOME/Wayland), written in **Rust + GTK 4 + libadwaita + GStreamer** and
GPU-accelerated end to end: MJPEG decoding and video encoding with VA-API, effects and zoom in an OpenGL shader,
and zero-copy display.

Author: [Hồ Sỹ Thế](https://github.com/hsthe29) · Source: <https://github.com/hsthe29/camera-studio> · License: [MIT](LICENSE)

## Features

- **Photos**: JPEG with exact JFIF colours and EXIF data, **self-timer** (3/5/10 s), **burst** (3/5/10 shots)
- **Video**: MP4 (H.264 + AAC) or WebM (VP9 + Opus), encoded on the GPU; take photos while recording; files stay
  playable even if the app is killed
- **QR code / barcode scanning**: copy the content or open links
- **26 live GPU effects**: mono, noir, warm, cool, sepia, vintage, thermal, sketch, comic, night vision,
  kaleidoscope, fisheye, twirl, ripple… — switched instantly, even while recording; the effects strip shows a
  **live preview** of the current scene with each effect
- **Digital zoom** up to 8× (Ctrl + mouse wheel, pinch, +/− keys) with bicubic filtering on the GPU; the zoom
  button is highlighted while zoomed
- **Camera hardware controls** (V4L2): brightness, contrast, exposure, white balance, anti-flicker… — remembered
  and re-applied
- Camera and resolution selection, **hot-plug detection**, **automatic reconnection** when another app releases
  the camera
- 3×3 grid, mirrored viewfinder, fullscreen, FPS overlay (also shows whether decoding runs on the GPU or the CPU)
- Built-in **gallery**: cached thumbnails, photo viewer, **video player** (GPU decoding), move to trash, copy,
  open with another app
- **Video player for the desktop**: registered for MP4, WebM, MKV and MOV, so videos open from the file manager
  ("Open With…" or double-click); the camera stays off while you watch, and the other media of the folder can be
  browsed
- Settings live in `~/.config/camera-studio/settings.json`; photos go to `~/Pictures/Camera`, videos to
  `~/Videos/Camera`

## Installation

Supported: **Ubuntu 24.04 and newer** (other distros with GTK 4.14+, libadwaita 1.5+ and GStreamer 1.24+ should
work too). `install.sh` checks all of this and tells you what is missing.

### 1. Dependencies

```sh
# Build tools and libraries
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-gl gstreamer1.0-pulseaudio
# Rust 1.85 or newer
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Optional, GPU acceleration on Intel graphics
sudo apt install intel-media-va-driver-non-free vainfo
```

Also needed is the GStreamer plugin **`gtk4paintablesink`**, which shows the picture:

- Where your distro packages it: `sudo apt install gstreamer1.0-gtk4`.
- **Ubuntu 24.04 has no such package**; build it once from source:

  ```sh
  . "$HOME/.cargo/env" && cargo install cargo-c
  git clone https://gitlab.freedesktop.org/gstreamer/gst-plugins-rs.git && cd gst-plugins-rs
  git checkout 0.13.5     # matches GStreamer 1.24; use the newest 0.13.x tag if this one is missing
  cargo cinstall -p gst-plugin-gtk4 --prefix=$HOME/.local --libdir=$HOME/.local/lib/gstreamer-1.0
  echo 'export GST_PLUGIN_PATH=$HOME/.local/lib/gstreamer-1.0' >> ~/.profile   # then log out and in again
  gst-inspect-1.0 gtk4paintablesink                                            # must print the element
  ```

### 2. Build and install

```sh
git clone https://github.com/hsthe29/camera-studio.git && cd camera-studio
./install.sh            # release build, installed to ~/.local (no sudo), added to the app menu
camera-studio           # run
camera-studio -t        # add a test video source (no camera needed)
camera-studio clip.mp4  # play a video (or show a photo) without turning the camera on
./uninstall.sh          # remove (keeps photos, videos and settings; --purge also deletes settings and cache)
```

`PREFIX=/some/dir ./install.sh` installs somewhere else (pass the same `PREFIX` to `uninstall.sh`).

- **Interface by version:** with GTK 4.20 and libadwaita 1.8 (Ubuntu 26.04) `install.sh` builds the full
  interface. With older versions (Ubuntu 24.04) it builds with `--no-default-features`: the same app, but the
  mode switch, spinner, reset button and shortcuts dialog use simpler widgets (see `src/compat.rs`).
- `install.sh` makes Camera Studio the default player only for video formats that have no default application
  yet; `uninstall.sh` removes those defaults again.
- Without VA-API the app falls back to CPU decoding/encoding (jpegdec, openh264/vp8); hardware acceleration can
  also be turned off in Preferences → Performance.

### Troubleshooting

| Symptom | Fix |
|---|---|
| `Couldn't find any package by glob 'gstreamer1.0-gtk4'` | Not packaged on Ubuntu 24.04, build the plugin from source (above) |
| App opens but the picture stays black or is missing | `gtk4paintablesink` not found: run `gst-inspect-1.0 gtk4paintablesink`; if you built it yourself, set `GST_PLUGIN_PATH` in `~/.profile` and log in again |
| `Missing development files (…)` | Install the packages listed in step 1 |
| `GTK 4.14 … too old` | Upgrade to Ubuntu 24.04 or newer |
| `GPU: VA-API not ready` | `sudo apt install vainfo gstreamer1.0-plugins-bad intel-media-va-driver-non-free` (Intel graphics) |

Bug reports: <https://github.com/hsthe29/camera-studio/issues> — please attach the report from
*About → Troubleshooting* (it has copy and save buttons).

## Keyboard shortcuts

| Key | Action | Key | Action |
|---|---|---|---|
| Space | Take photo / start–stop recording | Esc | Cancel timer, close panels |
| P / V / Q | Photo / Video / QR mode | T / B | Cycle self-timer / burst count |
| + / − / 0 | Zoom in / out / reset | G / M | Grid / mirror |
| E / K | Effects / camera controls | C | Switch camera |
| F11 | Fullscreen | F12 | Show FPS |
| Ctrl+O | Gallery | Ctrl+, | Preferences |
| Ctrl + mouse wheel | Zoom | | |

## Architecture

```
v4l2src → vajpegdec (GPU) → DMABuf → glupload → tee "raw" → glshader [zoom + 26 effects, chosen by uniform] → tee
  raw ─ valve → glcolorscale 192 px → appsink            effect-strip previews (only while the strip is open)
  tee ├─ glsinkbin(gtk4paintablesink)                   preview: GL texture shared with GTK, no copy
      ├─ valve → gldownload → appsink                   photos (opened per shot) → jpegenc in a worker thread
      ├─ valve → glcolorscale ½ → zbar                  QR mode only
      └─ [recording branch, linked while recording]
             gldownload → vapostproc (GPU: RGBA→NV12, mirror) → vah264enc → mp4mux
             pulsesrc → voaacenc ↗                      (same pipeline clock → audio and video in sync)
```

- Changing the effect or the zoom only updates shader uniforms: no recompilation, no renegotiation.
- The camera resolution is **pinned at every step of the main path** (after vapostproc, after glcolorconvert,
  after the shader): vapostproc and all GL filters can rescale, and through the tees the small branches (QR ½,
  192 px previews) once dragged the whole stream down to a low resolution that was then stretched back up.
  `--probe` checks this automatically.
- H.264/VP9 on the GPU with the encoder's highest-quality preset (`target-usage=1`, VBR ≈17 Mbps at 1080p30).
- The first frame of a recording is always a keyframe (frames captured before the button press are dropped
  before the encoder).
- A write error (e.g. a full disk) only stops the recording, never the camera; if the camera is unplugged or
  fails while recording, the file is still finalised.
- The recording branch is linked and unlinked on the fly; stopping sends EOS into that branch only, so the
  viewfinder never stutters.
- MP4 is written fragmented while recording and finalised into a regular MP4 when it stops.
- If hardware decoding fails with a camera, that camera falls back to the CPU automatically.

## Performance (measured on the development machine)

Intel Core Ultra 5 225H / Arc 130T, 1080p30 MJPEG webcam, CPU as % of one core:

| Scenario | CPU | FPS |
|---|---|---|
| 1080p30 preview | ~17% | 30 |
| + effect (e.g. sketch) and zoom | ~21% | 30 |
| Recording 1080p30 + audio | ~39% | 30 |

About 240 MB of RAM (mostly GPU drivers: VA-API, OpenGL, Vulkan); the binary is 1.5 MB.
For comparison, doing the same decoding/encoding on the CPU costs ~73% for the preview and over 100% while
recording.

Re-measure with `camera-studio --bench /tmp/bench`.

## Testing

```sh
cargo test                                   # unit tests (camera modes, settings, storage, JPEG encoding, recording pipelines, V4L2 ABI…)
cargo clippy                                 # no warnings
camera-studio --self-test /tmp/selftest      # drives the real UI through every feature, saves screenshots + a log
camera-studio --probe /tmp/probe             # camera state every second + main-path resolution check
camera-studio --record-test /tmp/rec         # records 4 s from the real camera (first frame / keyframe check)
camera-studio --ui-tour /tmp/tour            # screenshots of every UI state, in a normal and a narrow window
CAMERA_STUDIO_RECORD_TO=/dev/full camera-studio --record-test /tmp/x   # a write error must not stop the camera
CAMERA_STUDIO_TEST_PATTERN=smpte camera-studio -t   # colour-bar test source for checking colours
```
