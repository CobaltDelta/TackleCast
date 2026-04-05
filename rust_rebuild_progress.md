# TackleCast Rust Rebuild Progress

Last updated: 2026-04-05 (late)

## Current Status

The Rust rebuild is now well past the initial scaffold stage.

The app currently:
- compiles in debug and release
- opens the main window
- reads and writes the Python-compatible `tacklecast_settings.json`
- writes rotating logs
- enumerates real DirectShow and WASAPI devices
- renders synthetic test video with `cargo run -- --test`
- opens the real `ShadowCast 3` capture device on this machine
- decodes and displays real `2560x1440 @ 120 FPS` MJPEG video in Rust
- starts audio passthrough and can now route capture-card audio on this machine
- renders an in-app egui overlay pill for FPS, resolution, and status/error text
- opens an in-app settings menu with Escape and applies settings on close
- supports additional capture compatibility fallback paths beyond a single card profile

This is no longer just early setup work. The core video path is functioning end to end.

The rebuild is still incomplete overall because overlay/UI/menu polish,
distribution cleanup, and a final packaging pass are still pending.

## Important Environment Notes

- Rust toolchain is installed at `C:\Users\Aluca\.cargo\bin`
- FFmpeg dev/runtime files are installed at `C:\ffmpeg`
- LLVM/libclang is installed at `C:\Program Files\LLVM\bin`

For build commands in this environment, these variables were needed:

```powershell
$env:PATH="$env:USERPROFILE\.cargo\bin;C:\ffmpeg\bin;$env:PATH"
$env:FFMPEG_DIR='C:\ffmpeg'
$env:LIBCLANG_PATH='C:\Program Files\LLVM\bin'
```

## Important Dependency Note

The original plan suggested `ffmpeg-next = "7"`, but that failed against the installed
FFmpeg headers on this machine because `ffmpeg-sys-next` expected `libavcodec/avfft.h`,
which is not present in the installed FFmpeg include tree.

This was fixed by upgrading to:

```toml
ffmpeg-next = "8"
```

Do not downgrade this without re-checking FFmpeg header compatibility.

## Completed Plan Progress

### Step 1: Project Scaffold + Window

Status: complete enough to continue

Completed:
- Rust project initialized at repo root
- `winit` event loop created
- window opens with title `TackleCast`
- initial size `1280x720`
- resizable window
- `wgpu` initialized
- surface clear color matches plan background `#0a0a14`
- resize handling works
- app exits cleanly

Files:
- `Cargo.toml`
- `src/main.rs`
- `src/render.rs`

### Step 2: Settings

Status: complete

Completed:
- serde-compatible settings struct
- load/save of `tacklecast_settings.json`
- defaults match Python shape
- pretty JSON write
- effective FPS helper
- capture config helper
- tests for round-trip and Python JSON compatibility

Files:
- `src/settings.rs`

### Step 3: Logging

Status: complete enough for current phase

Completed:
- log file creation in `logs/`
- timestamped filenames
- pruning old logs down to 5 newest
- startup logging

Files:
- `src/logger.rs`

Notes:
- Current format is good enough to continue.
- Default log filtering now suppresses most non-actionable `wgpu`/backend noise unless overridden by `RUST_LOG`.

### Step 4: Device Enumeration

Status: complete enough for current phase

Completed:
- DirectShow video device enumeration by shelling out to FFmpeg
- WASAPI audio enumeration via `cpal`
- keyword-based audio auto-detect helper
- startup logging of discovered devices
- tests for FFmpeg line parsing and keyword matching

Files:
- `src/devices.rs`
- `src/main.rs`

Verified on this machine:
- video devices included `"ShadowCast 3"`
- audio inputs included `"HD (ShadowCast 3)"`

### Step 5: Video Capture and Decode

Status: complete enough for current phase

Completed:
- threaded capture API in `src/capture.rs`
- synthetic frame generator for test mode
- DirectShow input opening through FFmpeg
- MJPEG capture path for real `ShadowCast 3` video
- decoder loop using send/receive APIs
- frame copying that respects plane layout
- startup tolerance for bad MJPEG packets instead of aborting the thread
- rolling capture stats sent back to the main thread
- pixel-format attempt fallback sequence for DirectShow open (`requested -> mjpeg -> nv12 -> yuyv422 -> uyvy422 -> yuv420p -> auto`)
- software conversion path for unsupported decoder outputs using FFmpeg swscale to supported render formats (`NV12` / `YUVJ422P`)

Files:
- `src/capture.rs`
- `src/main.rs`

Verified on this machine:
- `cargo run` opened `ShadowCast 3`
- first decoded frame logged as `2560x1440 Yuvj422p`
- logs showed sustained decoding at about `120 FPS`

Notes:
- There is still room to tune DirectShow buffering and reduce occasional dropped-frame warnings.
- Broader card compatibility is now materially improved in code, but still needs on-hardware validation across more devices.

### Step 6: GPU Rendering of Video Frames

Status: substantially working

Completed:
- YUV frame upload path in `src/render.rs`
- shader-based YUV to RGB conversion
- support for both `NV12` and `Yuvj422p`
- aspect-ratio-correct rendering with letterboxing
- synthetic test path validated through the real renderer
- real decoded capture frames displayed in the window

Files:
- `src/render.rs`
- `src/main.rs`

Verified on this machine:
- `cargo run -- --test` displays synthetic video
- `cargo run` displayed live `ShadowCast 3` video

### Step 7: Audio Passthrough

Status: partially implemented / working baseline

Completed:
- startup audio passthrough module in Rust
- capture-card audio auto-detect
- fallback handling for stale saved device indices
- ring-buffered input-to-output callback path
- volume control state
- support for the sample formats encountered on this machine

Files:
- `src/audio.rs`
- `src/main.rs`

Verified on this machine:
- audio now routes from `HD (ShadowCast 3)`
- output to `Headphones (USB Audio Device)` worked after device/format fixes

Notes:
- Audio is good enough to continue, but still deserves more polish and validation.

### Step 10: Window Icon + Platform Integration

Status: complete enough for current phase

Completed:
- window icon loaded from `assets/icon.ico`
- Windows app user model ID set
- F11 fullscreen toggle
- minimize/restore overlay behavior
- minimized-window redraw suppression to reduce unnecessary render churn

## In Progress / Polish Remaining

### Step 8: egui FPS Overlay

Status: baseline implemented / ready to build on

Completed:
- egui integrated with the existing `winit` + `wgpu` render path
- top-left overlay pill now renders inside the app instead of relying on the window title
- overlay shows status text while connecting or on capture errors
- normal runtime text shows `WIDTHxHEIGHT | XX.X FPS`
- overlay respects the saved `show_overlay` setting while still allowing status/error text

Files:
- `src/ui.rs`
- `src/render.rs`
- `src/main.rs`

Verified on this machine:
- `cargo build` succeeds after the egui integration
- `cargo test` passes
- `cargo run -- --test` was smoke-tested successfully after the overlay wiring

### Step 9: egui Pause Menu

Status: baseline implemented / needs polish

Completed:
- Escape now opens and closes a centered settings menu
- menu includes video, audio, and display sections
- settings are edited in a draft state and applied when the menu closes
- clicking the dimmed backdrop also closes the menu and applies changes
- capture restarts only when video-related settings change
- audio restarts only when device-routing settings change
- volume-only changes update the running audio path without a full restart
- F11 toggles fullscreen, and the menu also exposes a fullscreen button
- Exit TackleCast button now requests app shutdown from inside the menu

Files:
- `src/ui.rs`
- `src/main.rs`

Notes:
- This is a functional baseline rather than the final parity pass.
- Styling and some interaction polish still remain.
- FPS text in the window title was removed now that the in-app overlay is established.
- Menu visuals received a first polish pass (section typography, separator styling, and button theming/hover states).
- Menu now suppresses wheel/zoom input while open to reduce accidental control changes.
- Custom FPS editing switched to explicit +/- controls (no wheel-driven spinner behavior).
- Menu spacing/typography now scale more cleanly with window size.

### Step 11: Build Script + Distribution

Status: complete enough for current phase

Completed:
- `cargo build --release` succeeds
- manual test packaging is possible
- a `dist/TackleCast-Rust-Test` folder and zip were created for external testing
- repeatable Rust packaging script added: `scripts/package_rust_release.ps1`
- packaging docs added: `BUILD.md`
- package script verified on this machine (produced `dist/TackleCast-Rust`)

Not yet done:
- cleaned distribution layout
- optional deeper release automation/versioning workflow

### Step 12: Test Mode

Status: complete enough for current phase

Completed:
- `--test` launches synthetic video through the real render path
- added explicit test pattern mode flags (`--test-nv12`, `--test-yuvj422p`, `--test-mjpeg`, `--test-alt`)
- if no video device is discovered, capture now falls back to test pattern instead of failing silently

Not yet done:
- only optional extra CLI ergonomics/documentation polish

## Files Added or Significantly Changed

- `Cargo.toml`
- `src/main.rs`
- `src/render.rs`
- `src/settings.rs`
- `src/logger.rs`
- `src/devices.rs`
- `src/capture.rs`
- `src/audio.rs`
- `src/ui.rs`
- `tacklecast_settings.json`

## Current Build State

As of this note:

- `cargo test` passes
- `cargo build` succeeds
- `cargo build --release` succeeds
- `cargo clippy --all-targets --all-features -- -D warnings` succeeds
- `cargo run -- --test` displays synthetic video
- `cargo run` displays real `ShadowCast 3` video on this machine
- current tester package exists at `dist/TackleCast-Rust-Test.zip`
- packaging script currently produces `dist/TackleCast-Rust/` and `dist/TackleCast-Rust.zip`

## Recommended Next Steps

Resume with the next slice in this order:

1. Hardware validation pass across non-ShadowCast devices (confirm fallback/conversion behavior)
2. Continue Step 9 final parity polish based on live UX feedback
3. Optional release automation/versioned packaging workflow cleanup
4. Continue audio polish only as needed after broader hardware validation

## Notes for the Next Session

- The Python app is still present in the repo only as a reference. No cleanup/removal work has been done yet.
- The hardest core milestone is now complete: real Rust capture/decode/render is working on hardware.
- Audio device indices in `tacklecast_settings.json` are machine-specific. Sending builds to other testers may require updating or regenerating settings.
- Default runtime log noise is now lower; raise verbosity intentionally with `RUST_LOG` when debugging backend issues.
- The next biggest user-facing work is polishing the new menu and finishing platform behavior details.
