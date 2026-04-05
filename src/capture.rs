use std::f32::consts::PI;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use ffmpeg_next as ffmpeg;
use ffmpeg::{
    codec, device, format, media, threading, Dictionary,
    software::scaling::{flag::Flags as ScaleFlags, Context as ScaleContext},
    util::frame::video::Video,
};
use tracing::{info, warn};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Nv12,
    Yuvj422p,
}

#[derive(Debug, Clone)]
pub struct CaptureFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub y_data: Vec<u8>,
    pub u_data: Vec<u8>,
    pub v_data: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureStats {
    pub fps: f32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub source: CaptureSource,
}

#[derive(Debug, Clone)]
pub enum CaptureSource {
    TestPattern {
        alternate_formats: bool,
        force_format: Option<PixelFormat>,
    },
    DirectShow {
        device_name: String,
        pixel_format: String,
        decode_threads: usize,
    },
}

pub struct CaptureThread {
    frame_rx: Receiver<CaptureFrame>,
    stats_rx: Receiver<CaptureStats>,
    error_rx: Receiver<String>,
    stop_flag: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

impl CaptureThread {
    pub fn start(config: CaptureConfig) -> Self {
        let (frame_tx, frame_rx) = unbounded();
        let (stats_tx, stats_rx) = unbounded();
        let (error_tx, error_rx) = unbounded();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let thread_stop_flag = stop_flag.clone();

        let join_handle = thread::spawn(move || match config.source.clone() {
            CaptureSource::TestPattern {
                alternate_formats,
                force_format,
            } => run_test_pattern(
                config.width,
                config.height,
                config.fps.max(1),
                alternate_formats,
                force_format,
                thread_stop_flag,
                frame_tx,
                stats_tx,
            ),
            CaptureSource::DirectShow {
                device_name,
                pixel_format,
                decode_threads,
            } => run_directshow_capture(
                device_name,
                config.width,
                config.height,
                config.fps.max(1),
                pixel_format,
                decode_threads,
                thread_stop_flag,
                frame_tx,
                stats_tx,
                error_tx,
            ),
        });

        Self {
            frame_rx,
            stats_rx,
            error_rx,
            stop_flag,
            join_handle: Some(join_handle),
        }
    }

    pub fn latest_frame(&self) -> Option<CaptureFrame> {
        self.frame_rx.try_iter().last()
    }

    pub fn latest_stats(&self) -> Option<CaptureStats> {
        self.stats_rx.try_iter().last()
    }

    pub fn latest_error(&self) -> Option<String> {
        self.error_rx.try_iter().last()
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

impl Drop for CaptureThread {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn run_test_pattern(
    width: u32,
    height: u32,
    fps: u32,
    alternate_formats: bool,
    force_format: Option<PixelFormat>,
    stop_flag: Arc<AtomicBool>,
    frame_tx: Sender<CaptureFrame>,
    stats_tx: Sender<CaptureStats>,
) {
    let frame_interval = Duration::from_secs_f64(1.0 / fps as f64);
    let mut frame_index = 0_u64;
    let mut last_stats_at = Instant::now();
    let mut stats_frame_counter = 0_u32;

    while !stop_flag.load(Ordering::Relaxed) {
        let loop_started = Instant::now();
        let format = force_format.unwrap_or_else(|| {
            if alternate_formats && ((frame_index / fps as u64) % 6) >= 3 {
                PixelFormat::Yuvj422p
            } else {
                PixelFormat::Nv12
            }
        });

        let frame = generate_test_frame(width, height, frame_index, format);
        if frame_tx.send(frame).is_err() {
            break;
        }

        frame_index += 1;
        stats_frame_counter += 1;

        let elapsed = last_stats_at.elapsed();
        if elapsed >= Duration::from_millis(300) {
            let fps = stats_frame_counter as f32 / elapsed.as_secs_f32();
            let _ = stats_tx.send(CaptureStats { fps, width, height });
            last_stats_at = Instant::now();
            stats_frame_counter = 0;
        }

        let spent = loop_started.elapsed();
        if spent < frame_interval {
            thread::sleep(frame_interval - spent);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_directshow_capture(
    device_name: String,
    requested_width: u32,
    requested_height: u32,
    requested_fps: u32,
    requested_pixel_format: String,
    decode_threads: usize,
    stop_flag: Arc<AtomicBool>,
    frame_tx: Sender<CaptureFrame>,
    stats_tx: Sender<CaptureStats>,
    error_tx: Sender<String>,
) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
    ffmpeg::log::set_level(ffmpeg::log::Level::Error);

    let attempt_formats = pixel_format_attempts(&requested_pixel_format);
    let mut last_error: Option<String> = None;
    for format_attempt in attempt_formats {
        if stop_flag.load(Ordering::Relaxed) {
            return;
        }

        let format_label = format_attempt.as_deref().unwrap_or("auto");
        info!(
            "capture attempt: device='{}' format='{}' {}x{} @ {}fps",
            device_name, format_label, requested_width, requested_height, requested_fps
        );

        match run_directshow_capture_inner(
            &device_name,
            requested_width,
            requested_height,
            requested_fps,
            format_attempt.as_deref(),
            decode_threads,
            stop_flag.clone(),
            frame_tx.clone(),
            stats_tx.clone(),
        ) {
            Ok(()) => return,
            Err(error) => {
                warn!(
                    "capture attempt failed: device='{}' format='{}' error={}",
                    device_name, format_label, error
                );
                last_error = Some(error);
            }
        }
    }

    let _ = error_tx.send(
        last_error.unwrap_or_else(|| format!("all capture attempts failed for '{device_name}'")),
    );
}

#[allow(clippy::too_many_arguments)]
fn run_directshow_capture_inner(
    device_name: &str,
    requested_width: u32,
    requested_height: u32,
    requested_fps: u32,
    requested_pixel_format: Option<&str>,
    decode_threads: usize,
    stop_flag: Arc<AtomicBool>,
    frame_tx: Sender<CaptureFrame>,
    stats_tx: Sender<CaptureStats>,
) -> Result<(), String> {
    let dshow_format = find_dshow_format()
        .ok_or_else(|| "DirectShow input format was not found in FFmpeg".to_string())?;
    let mut options = Dictionary::new();
    options.set("video_size", &format!("{requested_width}x{requested_height}"));
    options.set("framerate", &requested_fps.to_string());
    options.set("rtbufsize", "16M");
    options.set("probesize", "5000000");
    options.set("analyzeduration", "1000000");
    if let Some(format_name) = requested_pixel_format {
        if format_name.eq_ignore_ascii_case("mjpeg") {
            options.set("vcodec", "mjpeg");
        } else {
            options.set("pixel_format", format_name);
        }
    }

    let url = format!("video={device_name}");
    let mut input = format::open_with(&url, &dshow_format, options)
        .map_err(|error| format!("failed to open DirectShow input for '{device_name}': {error}"))?
        .input();

    let input_stream = input
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| format!("no video stream found for '{device_name}'"))?;
    let stream_index = input_stream.index();
    let parameters = input_stream.parameters();

    let mut decoder_context = codec::context::Context::from_parameters(parameters)
        .map_err(|error| format!("failed to create decoder context: {error}"))?;
    decoder_context.set_threading(threading::Config {
        kind: threading::Type::Frame,
        count: decode_threads.max(1),
    });

    let mut decoder = decoder_context
        .decoder()
        .video()
        .map_err(|error| format!("failed to open video decoder: {error}"))?;

    let mut decoded = Video::empty();
    let mut last_stats_at = Instant::now();
    let mut stats_frame_counter = 0_u32;
    let mut total_frames = 0_u64;
    let mut logged_first_frame = false;
    let mut packet_errors = 0_u32;
    let mut scaler: Option<ScaleContext> = None;
    let mut scaled_frame = Video::empty();

    info!(
        "capture thread opened DirectShow stream for '{}' at {}x{} @ {}fps using {}",
        device_name,
        requested_width,
        requested_height,
        requested_fps,
        requested_pixel_format.unwrap_or("auto")
    );

    for (stream, packet) in input.packets() {
        if stop_flag.load(Ordering::Relaxed) {
            return Ok(());
        }

        if stream.index() != stream_index {
            continue;
        }

        if let Err(error) = decoder.send_packet(&packet) {
            packet_errors += 1;
            if packet_errors <= 10 || packet_errors.is_multiple_of(50) {
                warn!(
                    "capture packet decode submission failed for '{}': {} (count={})",
                    device_name, error, packet_errors
                );
            }
            continue;
        }

        while decoder.receive_frame(&mut decoded).is_ok() {
            let frame = capture_frame_from_video(
                &decoded,
                &mut scaler,
                &mut scaled_frame,
                requested_fps > 60,
            )
                .map_err(|error| format!("failed to convert decoded frame: {error}"))?;
            let width = frame.width;
            let height = frame.height;
            total_frames += 1;

            if !logged_first_frame {
                logged_first_frame = true;
                info!(
                    "first decoded frame from '{}' => {}x{} {:?}",
                    device_name, width, height, frame.format
                );
            }
            if total_frames.is_multiple_of(120) {
                info!(
                    "decoded {} frames from '{}' (latest {}x{}, packet errors={})",
                    total_frames, device_name, width, height, packet_errors
                );
            }

            if frame_tx.send(frame).is_err() {
                return Ok(());
            }

            stats_frame_counter += 1;
            let elapsed = last_stats_at.elapsed();
            if elapsed >= Duration::from_millis(300) {
                let fps = stats_frame_counter as f32 / elapsed.as_secs_f32();
                let _ = stats_tx.send(CaptureStats { fps, width, height });
                last_stats_at = Instant::now();
                stats_frame_counter = 0;
            }
        }
    }

    Ok(())
}

fn capture_frame_from_video(
    frame: &Video,
    scaler: &mut Option<ScaleContext>,
    scaled_frame: &mut Video,
    prefer_yuvj422p: bool,
) -> Result<CaptureFrame, String> {
    let width = frame.width();
    let height = frame.height();

    let format = match frame.format() {
        format::Pixel::NV12 => PixelFormat::Nv12,
        format::Pixel::YUVJ422P => PixelFormat::Yuvj422p,
        other => {
            let target = if prefer_yuvj422p {
                format::Pixel::YUVJ422P
            } else {
                format::Pixel::NV12
            };

            if scaler
                .as_ref()
                .map(|ctx| {
                    ctx.input().format != other
                        || ctx.input().width != width
                        || ctx.input().height != height
                        || ctx.output().format != target
                        || ctx.output().width != width
                        || ctx.output().height != height
                })
                .unwrap_or(true)
            {
                *scaler = Some(
                    ScaleContext::get(other, width, height, target, width, height, ScaleFlags::BILINEAR)
                        .map_err(|error| {
                            format!(
                                "unsupported pixel format {other:?} and failed to initialize converter to {target:?}: {error}"
                            )
                        })?,
                );
                *scaled_frame = Video::empty();
                warn!("converting decoder output from {other:?} to {target:?} for compatibility");
            }

            let Some(scale_ctx) = scaler.as_mut() else {
                return Err("scaler context unavailable".to_string());
            };

            scale_ctx
                .run(frame, scaled_frame)
                .map_err(|error| format!("failed to convert frame via swscale: {error}"))?;
            return extract_supported_frame(scaled_frame);
        }
    };

    extract_supported_frame_with_format(frame, format, width, height)
}

fn extract_supported_frame(frame: &Video) -> Result<CaptureFrame, String> {
    let width = frame.width();
    let height = frame.height();
    let format = match frame.format() {
        format::Pixel::NV12 => PixelFormat::Nv12,
        format::Pixel::YUVJ422P => PixelFormat::Yuvj422p,
        other => return Err(format!("unsupported converted pixel format: {other:?}")),
    };
    extract_supported_frame_with_format(frame, format, width, height)
}

fn extract_supported_frame_with_format(
    frame: &Video,
    format: PixelFormat,
    width: u32,
    height: u32,
) -> Result<CaptureFrame, String> {
    let y_data = copy_plane(frame, 0, width as usize, height as usize);
    let u_data = match format {
        PixelFormat::Nv12 => copy_plane(frame, 1, width as usize, (height / 2) as usize),
        PixelFormat::Yuvj422p => copy_plane(frame, 1, (width / 2) as usize, height as usize),
    };
    let v_data = match format {
        PixelFormat::Nv12 => Vec::new(),
        PixelFormat::Yuvj422p => copy_plane(frame, 2, (width / 2) as usize, height as usize),
    };

    Ok(CaptureFrame {
        width,
        height,
        format,
        y_data,
        u_data,
        v_data,
    })
}

fn pixel_format_attempts(requested_pixel_format: &str) -> Vec<Option<String>> {
    let mut attempts = Vec::new();
    let mut push_unique = |value: Option<&str>| {
        if attempts.iter().any(|existing: &Option<String>| existing.as_deref() == value) {
            return;
        }
        attempts.push(value.map(str::to_string));
    };

    push_unique(Some(requested_pixel_format));
    push_unique(Some("mjpeg"));
    push_unique(Some("nv12"));
    push_unique(Some("yuyv422"));
    push_unique(Some("uyvy422"));
    push_unique(Some("yuv420p"));
    push_unique(None);
    attempts
}

fn copy_plane(frame: &Video, plane: usize, row_bytes: usize, rows: usize) -> Vec<u8> {
    let stride = frame.stride(plane);
    let source = frame.data(plane);
    let mut output = vec![0_u8; row_bytes * rows];

    for row in 0..rows {
        let src_start = row * stride;
        let dst_start = row * row_bytes;
        output[dst_start..dst_start + row_bytes]
            .copy_from_slice(&source[src_start..src_start + row_bytes]);
    }

    output
}

fn find_dshow_format() -> Option<ffmpeg::Format> {
    device::input::video().find(|format| match format {
        ffmpeg::Format::Input(input) => input.name() == "dshow",
        ffmpeg::Format::Output(_) => false,
    })
}

fn generate_test_frame(width: u32, height: u32, frame_index: u64, format: PixelFormat) -> CaptureFrame {
    let y_len = (width * height) as usize;
    let chroma_width = (width / 2) as usize;
    let chroma_height_420 = (height / 2) as usize;
    let chroma_height_422 = height as usize;
    let mut y_data = vec![0_u8; y_len];

    for y in 0..height as usize {
        for x in 0..width as usize {
            let xf = x as f32 / width as f32;
            let yf = y as f32 / height as f32;
            let phase = (frame_index as f32 * 0.04) + xf * PI * 2.0;
            let wave = ((phase.sin() * 0.5) + 0.5) * 80.0;
            let sweep = (((yf * 255.0) + frame_index as f32 * 2.0) as i32).rem_euclid(256) as u8;
            let bars = (((x * 8) / width as usize) * 28) as u8;
            y_data[y * width as usize + x] = sweep
                .saturating_div(2)
                .saturating_add(bars / 2)
                .saturating_add(wave as u8 / 2);
        }
    }

    match format {
        PixelFormat::Nv12 => {
            let mut uv_data = vec![0_u8; chroma_width * chroma_height_420 * 2];
            for y in 0..chroma_height_420 {
                for x in 0..chroma_width {
                    let index = (y * chroma_width + x) * 2;
                    let x_phase = ((x as f32 / chroma_width as f32) * PI * 2.0
                        + frame_index as f32 * 0.03)
                        .sin();
                    let y_phase = ((y as f32 / chroma_height_420 as f32) * PI * 2.0
                        + frame_index as f32 * 0.05)
                        .cos();
                    uv_data[index] = ((x_phase * 0.5 + 0.5) * 255.0) as u8;
                    uv_data[index + 1] = ((y_phase * 0.5 + 0.5) * 255.0) as u8;
                }
            }

            CaptureFrame {
                width,
                height,
                format,
                y_data,
                u_data: uv_data,
                v_data: Vec::new(),
            }
        }
        PixelFormat::Yuvj422p => {
            let mut u_data = vec![0_u8; chroma_width * chroma_height_422];
            let mut v_data = vec![0_u8; chroma_width * chroma_height_422];

            for y in 0..chroma_height_422 {
                for x in 0..chroma_width {
                    let index = y * chroma_width + x;
                    let x_phase = ((x as f32 / chroma_width as f32) * PI * 2.0
                        + frame_index as f32 * 0.06)
                        .sin();
                    let y_phase = ((y as f32 / chroma_height_422 as f32) * PI * 2.0
                        + frame_index as f32 * 0.02)
                        .cos();
                    u_data[index] = ((x_phase * 0.5 + 0.5) * 255.0) as u8;
                    v_data[index] = ((y_phase * 0.5 + 0.5) * 255.0) as u8;
                }
            }

            CaptureFrame {
                width,
                height,
                format,
                y_data,
                u_data,
                v_data,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_expected_plane_sizes_for_nv12() {
        let frame = generate_test_frame(1280, 720, 0, PixelFormat::Nv12);
        assert_eq!(frame.y_data.len(), 1280 * 720);
        assert_eq!(frame.u_data.len(), 1280 * 720 / 2);
        assert!(frame.v_data.is_empty());
    }

    #[test]
    fn generates_expected_plane_sizes_for_yuvj422p() {
        let frame = generate_test_frame(1280, 720, 0, PixelFormat::Yuvj422p);
        assert_eq!(frame.y_data.len(), 1280 * 720);
        assert_eq!(frame.u_data.len(), 640 * 720);
        assert_eq!(frame.v_data.len(), 640 * 720);
    }

    #[test]
    fn pixel_format_attempts_include_fallbacks_without_duplicates() {
        let attempts = pixel_format_attempts("nv12");
        let labels: Vec<_> = attempts.iter().map(|s| s.as_deref().unwrap_or("auto")).collect();
        assert!(labels.contains(&"nv12"));
        assert!(labels.contains(&"mjpeg"));
        assert!(labels.contains(&"auto"));
        let nv12_count = labels.iter().filter(|v| **v == "nv12").count();
        assert_eq!(nv12_count, 1);
    }
}
