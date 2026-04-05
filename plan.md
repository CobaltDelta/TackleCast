# TackleCast Rust Rewrite — Phase 1 Implementation Plan

## Overview

Rewrite TackleCast (a lightweight capture card viewer for Windows) from Python/PyQt6/mpv to Rust. The goal is to eliminate Python's GIL and GC overhead so software MJPEG decode can reach 120fps at 1440p on mid-range hardware (i7-12700 floor). The Python version tops out at ~40fps on that hardware due to CPU-bound MJPEG decoding through PyAV.

This plan covers **Phase 1: software decode only**. Hardware-accelerated decode (Phase 2) is a future follow-up if Phase 1 performance is insufficient.

The Rust version must achieve **full feature parity** with the current Python version described below.

---

## Current Python Architecture (reference)

```
tacklecast/
  __main__.py    — entry point, calls app.main()
  app.py         — MainWindow (PyQt6), pause menu, dark theme, video container, overlay positioning
  capture.py     — MpvCapture: wraps mpv for DirectShow capture via dshow, FPS polling, diagnostics
  audio.py       — AudioPassthrough: sounddevice stream (input -> output with volume), auto-detect input
  devices.py     — enumerate_video_devices (ffmpeg dshow), enumerate_audio_inputs/outputs (WASAPI filter)
  overlay.py     — OverlayWidget: frameless floating FPS/resolution display pill
  settings.py    — Settings dataclass, JSON load/save, resolution/FPS config, capture format selection
  logger.py      — Rotating file logger with auto-prune (keep 5)
launcher.py      — PyInstaller launcher (not needed for Rust)
build_dist.py    — PyInstaller build script (replaced by cargo build)
```

### Settings File Format (`tacklecast_settings.json`)

The Rust version MUST read and write this exact JSON format for compatibility:

```json
{
  "video_device": "ShadowCast 3",
  "audio_input": 15,
  "audio_output": 12,
  "resolution": "1440p",
  "fps_mode": "120",
  "custom_fps": 120,
  "volume": 1.0,
  "show_overlay": true
}
```

Fields:
- `video_device`: DirectShow device name string (e.g., "ShadowCast 3")
- `audio_input`: integer device index, -1 means "Default" (auto-detect)
- `audio_output`: integer device index, -1 means "Default"
- `resolution`: one of "720p", "1080p", "1440p", "4K"
- `fps_mode`: one of "60", "120", "custom"
- `custom_fps`: integer 30-240, only used when fps_mode is "custom"
- `volume`: float 0.0-1.0
- `show_overlay`: boolean

Settings file location:
- Dev mode: `tacklecast_settings.json` in the project root (next to `Cargo.toml`)
- Release build: `tacklecast_settings.json` next to the exe

### Resolution and Format Logic

```
Resolution map:
  "720p"  -> 1280x720
  "1080p" -> 1920x1080
  "1440p" -> 2560x1440
  "4K"    -> 3840x2160

Format selection (based on effective FPS):
  fps <= 60  -> pixel_format = "nv12",   decode_threads = 1
  fps > 60   -> pixel_format = "mjpeg",  decode_threads = 4
```

NV12 is raw uncompressed — no decode needed. MJPEG requires CPU decode but is the only format most capture cards support above 60fps.

### Key Behaviors to Replicate

1. **Window**: Dark themed (background #0a0a14), starts at 1280x720, resizable
2. **Video rendering**: Video fills the entire window, black letterbox if aspect ratio doesn't match
3. **FPS overlay**: Semi-transparent pill in top-left corner showing "WIDTHxHEIGHT | XX.X FPS". Black rounded-rect background (#000000 at 70% opacity), white text. Red text (#E94560) for status messages like "Connecting..." or errors. Hidden when `show_overlay` is false (but status messages always show).
4. **Pause menu** (Escape key toggles):
   - Dims the video behind it (semi-transparent black overlay)
   - Centered card with rounded corners, dark background (rgba(12,12,28,240))
   - Sections: VIDEO (device dropdown, resolution dropdown, FPS mode dropdown + custom spinbox), AUDIO (input dropdown, output dropdown, volume slider with percentage label), DISPLAY (fullscreen toggle button, show FPS overlay checkbox)
   - Exit TackleCast button (red themed)
   - "Press Escape to close" hint at bottom
   - Menu scales based on window width (460px at 1280px window, scales 0.8x-1.4x)
   - Changes are applied when menu CLOSES, not while editing (snapshot/apply/revert pattern)
   - Clicking the dim overlay behind the menu also closes it (applying changes)
5. **Fullscreen**: F11 toggles, also available from pause menu button
6. **Audio passthrough**: Captures audio from input device, plays to output device in real-time with volume control. Uses WASAPI devices only. Low latency (256 sample blocksize). Auto-detects capture card audio input by matching keywords from video device name.
7. **Device enumeration**: Video devices via ffmpeg's DirectShow listing. Audio via WASAPI host API filtering.
8. **Logging**: Timestamped log files in `logs/` directory, rotating (keep 5 newest), includes capture diagnostics
9. **Keyboard**: Escape = toggle pause menu, F11 = toggle fullscreen
10. **Window icon**: loads from `assets/icon.ico`
11. **Minimize behavior**: FPS overlay hides when window is minimized, shows when restored
12. **No scroll on dropdowns/spinboxes**: Mouse wheel events are ignored on combo boxes and spinboxes (prevents accidental changes)

### Color Palette (exact values)

```
Background:           #0a0a14
Panel background:     #16213e
Border:               #0f3460
Accent (red):         #e94560
Text primary:         #e0e0e0
Text secondary:       #8899aa
Text hint:            #445566
Menu background:      rgba(12, 12, 28, 240)
Menu border:          #1a2a50
Dim overlay:          rgba(0, 0, 0, 120)
FPS pill background:  rgba(0, 0, 0, 180)
Exit button bg:       #3a1020
Slider groove:        #0f3460
Slider handle/fill:   #e94560
```

---

## Rust Architecture

```
tacklecast-rs/
  Cargo.toml
  src/
    main.rs          — entry point, winit event loop, orchestrates all modules
    capture.rs       — DirectShow capture via ffmpeg-next, MJPEG/NV12 decode, YUV frame output
    audio.rs         — WASAPI audio passthrough via cpal (input -> output with volume)
    render.rs        — wgpu renderer: YUV textures -> RGB via WGSL shader, aspect-ratio-correct display
    ui.rs            — egui overlay: FPS pill, pause menu with all controls, dark theme
    devices.rs       — enumerate DirectShow video devices, enumerate WASAPI audio devices
    settings.rs      — serde JSON settings, compatible with Python version's format
    logger.rs        — file logging with rotation (tracing + tracing-appender or similar)
  assets/
    icon.ico         — window icon (copy from existing)
```

### Crate Dependencies (`Cargo.toml`)

```toml
[package]
name = "tacklecast"
version = "0.1.0"
edition = "2021"

[dependencies]
# Windowing
winit = "0.30"

# GPU rendering
wgpu = "24"

# UI overlay and menu
egui = "0.31"
egui-wgpu = "0.31"
egui-winit = "0.31"

# Video capture and decode
ffmpeg-next = "7"

# Audio passthrough
cpal = "0.15"

# Settings persistence
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["fmt", "env-filter"] }
tracing-appender = "0.2"

# Windows APIs for device enumeration and app ID
windows = { version = "0.58", features = [
    "Win32_Media_Audio",
    "Win32_System_Com",
    "Win32_Devices_FunctionDiscovery",
    "Win32_UI_Shell",
] }

# Image loading for window icon
image = "0.25"

# Time utilities
instant = "0.1"

[profile.release]
opt-level = 3
lto = "thin"
```

> **Note on crate versions**: The versions above are approximate targets. Use the latest compatible versions available at build time. If a version doesn't exist or has breaking API changes, adjust accordingly — the important thing is the crate choice, not the exact version.

> **Note on `ffmpeg-next`**: This crate requires FFmpeg development libraries (headers + shared libs) to be available at build time. The recommended approach for Windows:
> 1. Download prebuilt FFmpeg shared+dev from https://github.com/BtbN/FFmpeg-Builds/releases (e.g., `ffmpeg-n7.1-latest-win64-gpl-shared-7.1.zip`)
> 2. Set environment variable `FFMPEG_DIR` to the extracted folder path (the folder containing `bin/`, `lib/`, `include/`)
> 3. Add `%FFMPEG_DIR%/bin` to PATH so the DLLs are found at runtime
>
> Alternative: use vcpkg (`vcpkg install ffmpeg:x64-windows`) and set `VCPKG_ROOT`.

---

## Implementation Steps

Each step below is a self-contained unit of work. Complete them in order. Each step should compile and run (or at minimum compile) before moving to the next.

### Step 1: Project Scaffold + Window

**Goal**: Cargo project that opens a blank dark window with wgpu rendering.

**Files**: `Cargo.toml`, `src/main.rs`, `src/render.rs`

**Details**:
1. `cargo init` the project in a `tacklecast-rs/` subdirectory of this repo
2. Set up `winit` event loop with a single window:
   - Title: "TackleCast"
   - Initial size: 1280x720
   - Resizable: yes
   - Background: #0a0a14
3. Initialize `wgpu` surface and device:
   - Request `wgpu::PowerPreference::HighPerformance`
   - Surface format: `Bgra8Unorm` preferred
4. Render loop: clear to #0a0a14 every frame
5. Handle window resize (reconfigure surface)
6. Handle close event (exit cleanly)

**Validation**: Window opens, shows dark background, resizes, closes cleanly.

### Step 2: Settings

**Goal**: Load and save settings in the same JSON format as the Python version.

**Files**: `src/settings.rs`

**Details**:
1. Define `Settings` struct with serde Serialize/Deserialize:
   ```rust
   pub struct Settings {
       pub video_device: String,      // default: ""
       pub audio_input: i32,          // default: -1
       pub audio_output: i32,         // default: -1
       pub resolution: String,        // default: "1080p"
       pub fps_mode: String,          // default: "60"
       pub custom_fps: u32,           // default: 120
       pub volume: f64,               // default: 1.0
       pub show_overlay: bool,        // default: true
   }
   ```
2. `Settings::load()` — reads from `tacklecast_settings.json` next to the exe. Falls back to defaults if file missing or malformed. Use `serde_json::from_str` with `#[serde(default)]` on all fields.
3. `Settings::save()` — writes JSON with pretty-print (2-space indent) to the same path.
4. Helper: `get_capture_config(resolution, fps) -> (width, height, fps, pixel_format, threads)` matching the Python logic:
   - Resolution map: "720p"->(1280,720), "1080p"->(1920,1080), "1440p"->(2560,1440), "4K"->(3840,2160)
   - fps <= 60: nv12, 1 thread; fps > 60: mjpeg, 4 threads
5. Helper: `get_fps(&self) -> u32` — returns 60, 120, or custom_fps based on fps_mode.

**Validation**: Unit test that serializes default settings, deserializes, round-trips correctly. Test that it reads the existing `tacklecast_settings.json` from the Python version.

### Step 3: Logging

**Goal**: File-based logging matching the Python version's behavior.

**Files**: `src/logger.rs`

**Details**:
1. Use `tracing` + `tracing-subscriber` + `tracing-appender`
2. Log to `logs/tacklecast_YYYYMMDD_HHMMSS.log`
3. Format: `YYYY-MM-DD HH:MM:SS [LEVEL] message`
4. On startup, prune old logs — keep only the 5 newest `tacklecast_*.log` files in the `logs/` directory
5. Log startup info: platform, version, build type
6. Export an `init_logging()` function called from main

**Validation**: Run the app, check that a log file is created in `logs/` with correct format.

### Step 4: Device Enumeration

**Goal**: List DirectShow video devices and WASAPI audio devices.

**Files**: `src/devices.rs`

**Details**:

**Video devices** — enumerate via ffmpeg. Two approaches (pick whichever is simpler):
- Option A: Use `ffmpeg-next`'s `format::input_with_dictionary` to query dshow device list
- Option B: Shell out to ffmpeg binary (`ffmpeg -f dshow -list_devices true -i dummy`) and parse stderr, same as the Python version does

The Python version uses Option B. Either approach is fine. The output should be a `Vec<String>` of device names (e.g., `["ShadowCast 3", "OBS Virtual Camera"]`).

**Audio devices** — enumerate WASAPI devices via `cpal`:
1. Get the list of all audio devices from `cpal::default_host()`
2. For input devices: collect `(index, name)` pairs
3. For output devices: collect `(index, name)` pairs
4. The "index" here refers to the position in cpal's device list — we store these indices in settings

**Audio auto-detect** — match capture card audio input by name keywords:
1. Take the video device name, split into words (3+ chars, skip "pro", "the", "and")
2. Score each input device by how many keywords match
3. Return the best match if score >= 2, else None

**Validation**: Print device lists to log/stdout on startup.

### Step 5: Video Capture and Decode

**Goal**: Capture video from a DirectShow device via ffmpeg-next, decode MJPEG/NV12 frames, output raw YUV plane data.

**Files**: `src/capture.rs`

**Details**:
1. Open DirectShow input using `ffmpeg_next::format::input_with_dictionary`:
   - Format: "dshow"
   - URL: `video={device_name}`
   - Options dictionary:
     - `video_size`: "{width}x{height}"
     - `framerate`: "{fps}"
     - `rtbufsize`: "1M"
     - `vcodec`: "mjpeg" (if MJPEG) or `pixel_format`: "{format}" (if NV12)
2. Get the video stream, open a decoder:
   - For MJPEG: set `thread_count` to the value from capture config (4)
   - For NV12: thread_count 1 (no decoding needed)
3. Run a capture loop in a **separate thread** (`std::thread::spawn`):
   - Read packets from the input
   - Send to decoder
   - Receive decoded frames
   - Each frame is `ffmpeg_next::frame::Video` with format `yuvj422p` (MJPEG) or `nv12`
   - Extract YUV plane data as byte slices: `frame.data(0)` for Y, `frame.data(1)` for U, `frame.data(2)` for V
   - **Important**: respect `frame.stride(n)` — the stride may be larger than the width. Copy row-by-row if stride != width.
   - Send frame data to the render thread via a channel (`std::sync::mpsc` or `crossbeam-channel`)
4. Frame data struct sent through channel:
   ```rust
   pub struct CaptureFrame {
       pub width: u32,
       pub height: u32,
       pub format: PixelFormat,  // Nv12 or Yuvj422p
       pub y_data: Vec<u8>,
       pub u_data: Vec<u8>,
       pub v_data: Vec<u8>,
   }
   ```
   - For yuvj422p (MJPEG): Y is w*h, U is (w/2)*h, V is (w/2)*h (4:2:2 — full height!)
   - For NV12: Y is w*h, UV interleaved is w*(h/2) (stored in u_data, v_data is empty)
5. FPS measurement: track frame count and elapsed time, compute rolling FPS over ~0.3s intervals (same as Python's poll_stats logic)
6. Provide `CaptureThread::start(config)` and `CaptureThread::stop()` methods
7. Error reporting: send errors through a separate channel or callback

**Critical detail**: MJPEG decodes to `yuvj422p`, NOT `yuv420p`. The U and V planes are full height (w/2 * h), not half height. Getting this wrong causes greyscale or corrupt output. The Python version learned this the hard way.

**Validation**: Start capture, log frame dimensions and format, verify frames are being produced at the expected rate.

### Step 6: GPU Rendering (wgpu + WGSL shader)

**Goal**: Display YUV frames from the capture thread as full-window RGB video with correct aspect ratio.

**Files**: `src/render.rs` (expand from Step 1)

**Details**:
1. Create GPU textures for YUV planes:
   - Y texture: `R8Unorm`, dimensions `width x height`
   - U texture: `R8Unorm`, dimensions depend on format:
     - yuvj422p: `(width/2) x height`
     - NV12: `(width/2) x (height/2)` with RG8Unorm (UV interleaved)
   - V texture: `R8Unorm`, dimensions same as U (only for yuvj422p)
2. Each frame: upload plane data to textures via `queue.write_texture()`
3. WGSL fragment shader for YUV -> RGB conversion:
   ```wgsl
   // For yuvj422p (full-range, BT.601):
   // R = Y + 1.402 * (V - 0.5)
   // G = Y - 0.344136 * (U - 0.5) - 0.714136 * (V - 0.5)
   // B = Y + 1.772 * (U - 0.5)
   
   // For NV12 (limited-range, BT.601):
   // First scale Y from [16,235] to [0,1] and UV from [16,240] to [-0.5,0.5]
   // Then apply same matrix
   ```
4. Vertex shader: full-screen quad (two triangles) with UV coordinates
5. Aspect-ratio-correct rendering:
   - Compare video aspect ratio to window aspect ratio
   - Letterbox (black bars) on sides or top/bottom as needed
   - Clear color remains #0a0a14 for letterbox areas
6. Bind group layout: 3 textures + sampler
7. On window resize: reconfigure surface, recalculate aspect ratio
8. On new frame received from channel: upload textures, request redraw

**Validation**: Capture frames display correctly — colors are accurate (no green/pink tint), aspect ratio preserved, letterboxing works.

### Step 7: Audio Passthrough

**Goal**: Low-latency audio capture-to-playback with volume control.

**Files**: `src/audio.rs`

**Details**:
1. Use `cpal` to open input and output streams
2. Input device: by index from settings, or default input if -1
3. Output device: by index from settings, or default output if -1
4. Audio auto-detect: if input is -1 (Default), use the keyword-matching logic from `devices.rs` to find the capture card's audio
5. Stream config:
   - Sample rate: use input device's default sample rate
   - Channels: `min(input_channels, output_channels, 2)`
   - Sample format: f32
   - Buffer size: 256 frames (low latency)
6. Passthrough: ring buffer or channel between input callback and output callback
   - Input callback writes samples to buffer
   - Output callback reads samples from buffer, multiplies by volume
   - If buffer underrun: output silence
7. `AudioPassthrough::start(input_device, output_device, volume)` — stops any existing stream first
8. `AudioPassthrough::stop()`
9. `AudioPassthrough::set_volume(volume: f64)` — clamp to 0.0-1.0, use `AtomicU32` or similar for lock-free sharing with audio thread

**Validation**: Audio from capture card plays through speakers with no crackling, volume adjustment works.

### Step 8: egui UI — FPS Overlay

**Goal**: Render the FPS/resolution overlay pill using egui.

**Files**: `src/ui.rs`

**Details**:
1. Integrate egui with wgpu and winit using `egui-winit` and `egui-wgpu`
2. FPS overlay pill (top-left corner):
   - Background: rounded rect, #000000 at 70% opacity (rgba(0,0,0,180))
   - Text: "WIDTHxHEIGHT | XX.X FPS" in bold
   - Text color: #e0e0e0 (normal) or #e94560 (status/error messages)
   - Position: 8px from top-left, with 8px internal padding
   - Font: system default, ~14px equivalent, bold
   - Only visible when `show_overlay` is true (but status messages always show)
3. egui renders on top of the wgpu video output each frame
4. Overlay should not consume mouse events (click-through)

**Validation**: FPS pill displays correctly over video, updates in real-time, can be toggled.

### Step 9: egui UI — Pause Menu

**Goal**: Full settings pause menu matching the Python version's layout and behavior.

**Files**: `src/ui.rs` (expand)

**Details**:
1. Menu toggle: Escape key opens/closes the menu
2. When menu opens:
   - Snapshot current settings (for revert-on-cancel if we add that later)
   - Dim overlay: full-window semi-transparent black (rgba(0,0,0,120)) behind the menu
   - Menu panel: centered, dark background (rgba(12,12,28,240)), rounded corners (12px), border (#1a2a50)
3. Menu layout (top to bottom):
   - **Title**: "Settings" — centered, bold, #e0e0e0
   - **VIDEO section header**: "VIDEO" — #e94560, bold, uppercase, letter-spaced
     - "Video Device" label (#8899aa) + dropdown
     - Row: "Resolution" label + dropdown | "Frame Rate" label + dropdown (60 FPS / 120 FPS / Custom)
     - Custom FPS spinbox (visible only when "Custom" selected, range 30-240)
     - Warning text (italic, #e94560):
       - 120 FPS: "A fast CPU is required for 120 FPS. Performance may vary by hardware."
       - Custom: "Custom FPS is experimental and is not guaranteed to work with all devices."
   - Separator line (#1a2a50)
   - **AUDIO section header**: "AUDIO" — same style as VIDEO
     - "Audio Input" label + dropdown (includes "Default" option)
     - "Audio Output" label + dropdown (includes "Default" option)
     - "Volume" label + slider (0-100) + percentage label
   - Separator line
   - **DISPLAY section header**: "DISPLAY"
     - Row: "Enter Fullscreen" / "Exit Fullscreen" button | "Show FPS Overlay" checkbox
   - Separator line
   - **"Exit TackleCast"** button — red theme (bg #3a1020, border #e94560, hover: bg #e94560 text white)
   - "Press Escape to close" hint — #445566
4. Dropdown/combo styling: bg #16213e, text #e0e0e0, border #0f3460, selection highlight #e94560
5. Slider styling: groove #0f3460, handle/fill #e94560
6. Button styling: bg #16213e, text #e0e0e0, border #0f3460, hover: border #e94560
7. Menu width scales: 460px at 1280px window width, scale factor = clamp(window_width / 1280, 0.8, 1.4)
8. **Apply on close**: When menu closes (Escape or clicking dim overlay), read all current values, compare to saved settings, save, and restart capture/audio only if relevant settings changed (same logic as Python version)
9. Clicking the dim overlay area (outside the menu card) closes the menu

**Validation**: Menu opens/closes, all controls functional, settings persist, capture/audio restart when changed.

### Step 10: Window Icon + Platform Integration

**Goal**: Window icon, taskbar identity, fullscreen, minimize behavior.

**Files**: `src/main.rs` (expand)

**Details**:
1. Load `assets/icon.ico` and set as window icon using `winit`'s `set_window_icon`
   - Use the `image` crate to decode the ICO file
2. Set Windows app user model ID: `tacklecast.tacklecast.v1` (for taskbar grouping)
   - Use `windows` crate to call `SetCurrentProcessExplicitAppUserModelID`
3. F11 toggles fullscreen (winit `set_fullscreen` with `Fullscreen::Borderless(None)`)
4. When window is minimized: pause the FPS overlay rendering (or skip egui overlay)
5. When window is restored: resume overlay rendering

**Validation**: Icon appears in title bar and taskbar, fullscreen works, minimize/restore behavior correct.

### Step 11: Build Script + Distribution

**Goal**: Single exe build, README for building.

**Files**: `build.rs` (if needed for FFmpeg), update `Cargo.toml`

**Details**:
1. `cargo build --release` should produce a single exe in `target/release/tacklecast.exe`
2. The exe will need FFmpeg DLLs alongside it at runtime (avcodec, avformat, avutil, avdevice, swresample, swscale)
   - Document this in a BUILD.md
3. Copy `assets/icon.ico` to be accessible at runtime — either embed it via `include_bytes!` or use a build script to copy it
4. Add a Windows manifest for DPI awareness if needed
5. `[profile.release]` in Cargo.toml: `opt-level = 3`, `lto = "thin"` for performance

**Validation**: `cargo build --release` succeeds, exe runs, all features work.

### Step 12: Test Mode (Fake Capture)

**Goal**: A test/demo mode that works without a real capture card, for validating the rendering pipeline.

**Files**: `src/capture.rs` (add test pattern generator)

**Details**:
1. If no video device is found (or a `--test` CLI flag is passed), generate synthetic frames:
   - Color bars or gradient pattern in YUV format
   - Cycle through colors slowly so it's visually obvious the render pipeline works
   - Generate at the configured FPS
2. This allows Codex to verify the build works even without capture hardware
3. The test pattern should exercise both yuvj422p and NV12 code paths

**Validation**: `tacklecast.exe --test` shows a color bar pattern at the configured resolution and FPS.

---

## File-by-File Specification Summary

| File | Purpose | Key crates |
|------|---------|------------|
| `Cargo.toml` | Dependencies and build config | — |
| `src/main.rs` | Entry point, winit event loop, wires everything together | winit, wgpu |
| `src/settings.rs` | JSON settings load/save, resolution/FPS config | serde, serde_json |
| `src/logger.rs` | File logging with rotation | tracing, tracing-appender |
| `src/devices.rs` | DirectShow video + WASAPI audio device enumeration | ffmpeg-next or std::process, cpal |
| `src/capture.rs` | DirectShow capture, MJPEG/NV12 decode, YUV frame output | ffmpeg-next |
| `src/render.rs` | wgpu setup, YUV texture upload, WGSL shader, aspect ratio | wgpu |
| `src/audio.rs` | WASAPI audio passthrough with volume | cpal |
| `src/ui.rs` | egui FPS overlay + pause menu | egui, egui-wgpu, egui-winit |

---

## Critical Gotchas

1. **MJPEG decodes to yuvj422p (4:2:2)**, not yuv420p (4:2:0). UV planes are `(width/2) x height` (full height). Getting this wrong = greyscale or green/pink artifacts.

2. **Stride != width** in FFmpeg frames. Always use `frame.stride(plane)` and copy row-by-row when stride > width. Uploading stride-padded data directly will cause diagonal tearing.

3. **wgpu texture uploads** require `bytes_per_row` to be aligned to `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT` (256 bytes). Pad rows if needed.

4. **cpal audio callbacks** run on a real-time thread. No allocations, no locks, no panics in callbacks. Use a lock-free ring buffer for passthrough.

5. **DirectShow device names** contain spaces and special characters. The ffmpeg URL format is `video=Device Name` (no quotes in the URL string itself, quotes are an ffmpeg CLI thing).

6. **Settings apply on menu close**, not on each widget change. Snapshot state when menu opens, diff when it closes.

7. **egui consumes keyboard events** — make sure Escape and F11 are handled at the winit level before egui gets them, or check egui's response to see if it consumed the event.

8. **NV12 UV plane is interleaved** (UVUVUV...), not planar. If using separate U/V textures, you need to deinterleave. Alternatively, use a single RG8 texture for the UV plane.

9. **Volume is applied in the audio callback**, not by changing device volume. Multiply samples by volume factor.

10. **FFmpeg initialization**: Call `ffmpeg_next::init()` once at startup before any other ffmpeg operations.

---

## Build Prerequisites

Before building, the following must be installed:

1. **Rust toolchain**: Install from https://rustup.rs
   - Verify: `rustc --version`, `cargo --version`
   
2. **Visual Studio Build Tools**: MSVC C++ toolchain required for Rust on Windows
   - Install "Desktop development with C++" workload from https://visualstudio.microsoft.com/visual-cpp-build-tools/
   - Verify: `where cl`

3. **FFmpeg development libraries**: Required by the `ffmpeg-next` crate
   - Download prebuilt from https://github.com/BtbN/FFmpeg-Builds/releases
   - Get the `shared` + `dev` variant (e.g., `ffmpeg-n7.1-latest-win64-gpl-shared-7.1.zip`)
   - Extract and set `FFMPEG_DIR` environment variable to the extracted path
   - Add `%FFMPEG_DIR%/bin` to PATH
   - Verify: `where avcodec-61.dll` (or similar version number)

---

## Testing Checklist

After each step, verify:

- [ ] `cargo build` succeeds with no errors
- [ ] `cargo clippy` has no warnings
- [ ] The exe runs without panicking

After all steps complete:

- [ ] Window opens with dark background
- [ ] Video from capture card displays correctly (colors accurate, no tearing)
- [ ] FPS overlay shows correct resolution and frame rate
- [ ] Audio passthrough works with no crackling
- [ ] Pause menu opens/closes with Escape
- [ ] All settings controls work and persist across restarts
- [ ] Fullscreen toggle works (F11 and menu button)
- [ ] Window icon appears correctly
- [ ] `--test` mode shows synthetic frames without capture hardware
- [ ] Settings file is compatible with the Python version (same JSON format)
- [ ] Log files are created in `logs/` directory
- [ ] Build produces a reasonable exe size (~10-20MB + FFmpeg DLLs)
