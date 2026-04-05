# TackleCast Build and Packaging (Windows)

## Prerequisites

1. **Rust toolchain** (`rustup`, `cargo`)
2. **Visual Studio C++ Build Tools** (MSVC toolchain)
3. **FFmpeg shared + dev build** (must include `bin/`, `lib/`, `include/`)
4. **LLVM/libclang** (required by `ffmpeg-sys-next` bindgen)
5. **CUDA Toolkit 13.2** (optional, only needed for GPU decode development)

### Environment Setup

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\ffmpeg\bin;$env:PATH"
$env:FFMPEG_DIR = 'C:\ffmpeg'
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
```

## Build

```powershell
cargo build              # debug build
cargo build --release    # release build (optimized, thin LTO)
cargo test               # run tests
cargo run -- --test      # test pattern mode (no capture card needed)
```

The release executable is at `target\release\tacklecast.exe`.

### Feature Flags

| Flag | Default | Description |
|---|---|---|
| `gpu-decode` | on | NVIDIA nvJPEG GPU decode + zero-copy pipeline |

To build without GPU decode support:

```powershell
cargo build --release --no-default-features
```

## Package for Distribution

Use the packaging script:

```powershell
.\scripts\package_rust_release.ps1              # build + package
.\scripts\package_rust_release.ps1 -Zip         # build + package + zip
```

Default output: `dist\TackleCast-Rust\`

### Package Contents

- `TackleCast.exe` - release binary
- `assets\icon.ico` - window icon
- `tacklecast_settings.json` - default settings (if present)
- `logs\` - empty directory for runtime logs
- FFmpeg runtime DLLs (avcodec, avformat, avdevice, avutil, swresample, swscale, avfilter)

### GPU Decode Bundle

For NVIDIA GPU decode support, also include alongside the exe:

- `nvjpeg64_13.dll`
- `cudart64_13.dll`

These can be copied from your CUDA Toolkit installation (`C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.2\bin`). Users without these DLLs will automatically fall back to software decode.

**Note:** End users need NVIDIA driver 570+ for CUDA 13 compatibility.

## Runtime Notes

- FFmpeg DLLs must be in the same directory as `TackleCast.exe`
- Settings are loaded from `tacklecast_settings.json` next to the exe
- Logs are written to `logs\` next to the exe
- The app forces the wgpu DX12 backend on Windows (required for zero-copy CUDA interop)
