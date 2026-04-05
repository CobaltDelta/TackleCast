# TackleCast Rust Build and Packaging (Windows)

This project now includes a native Rust build and a repeatable packaging script.

## Prerequisites

1. Rust toolchain (`rustup`, `cargo`)
2. Visual Studio C++ Build Tools (MSVC toolchain)
3. FFmpeg shared + dev build (must include `bin/`, `lib/`, `include/`)
4. LLVM/libclang (required by `ffmpeg-sys-next` bindgen in this environment)

Environment example:

```powershell
$env:PATH="$env:USERPROFILE\.cargo\bin;C:\ffmpeg\bin;$env:PATH"
$env:FFMPEG_DIR='C:\ffmpeg'
$env:LIBCLANG_PATH='C:\Program Files\LLVM\bin'
```

## Build

```powershell
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

Release executable:

`target\release\tacklecast.exe`

## Package for Distribution

Use the Rust packaging script:

```powershell
.\scripts\package_rust_release.ps1
```

Optional zip output:

```powershell
.\scripts\package_rust_release.ps1 -Zip
```

Default output folder:

`dist\TackleCast-Rust\`

Contents include:

- `TackleCast.exe`
- `assets\icon.ico`
- `tacklecast_settings.json` (if present in repo root)
- FFmpeg runtime DLLs copied from `%FFMPEG_DIR%\bin`
- `logs\` (empty folder created for runtime logs)
- `README.txt`

## Runtime Notes

- The app expects FFmpeg DLLs beside `TackleCast.exe`.
- Settings are loaded from `tacklecast_settings.json` next to the exe in release builds.
- Logs are written to `logs\` next to the exe.
