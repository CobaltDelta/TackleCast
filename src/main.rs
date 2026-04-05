mod audio;
mod capture;
mod devices;
#[cfg(feature = "gpu-decode")]
mod gpu_decode;
mod logger;
mod render;
mod settings;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;

use audio::AudioPassthrough;
use capture::{CaptureConfig, CaptureSource, CaptureStats, CaptureThread, PixelFormat};
use devices::AudioDevice;
use render::Renderer;
use settings::{get_capture_config, Settings};
use tracing::{error, info, warn};
use ui::{OverlayInfo, UiFrame, UiState};
use windows::core::HSTRING;
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Icon, Window, WindowAttributes};

const WINDOW_TITLE: &str = "TackleCast";
const INITIAL_WIDTH: u32 = 1280;
const INITIAL_HEIGHT: u32 = 720;
const APP_ID: &str = "tacklecast.tacklecast.v1";

#[derive(Clone, Copy, Debug)]
enum TestPatternMode {
    Alternate,
    Nv12,
    Yuvj422p,
}

#[derive(Debug)]
struct CliArgs {
    test_mode: bool,
    test_pattern: TestPatternMode,
}

impl CliArgs {
    fn parse() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let has_flag = |name: &str| args.iter().any(|arg| arg.eq_ignore_ascii_case(name));

        let test_pattern = if has_flag("--test-nv12") {
            TestPatternMode::Nv12
        } else if has_flag("--test-yuvj422p") || has_flag("--test-mjpeg") {
            TestPatternMode::Yuvj422p
        } else {
            TestPatternMode::Alternate
        };

        let test_mode = has_flag("--test")
            || has_flag("--test-alt")
            || has_flag("--test-nv12")
            || has_flag("--test-yuvj422p")
            || has_flag("--test-mjpeg");

        Self {
            test_mode,
            test_pattern,
        }
    }
}

fn main() {
    if let Err(error) = logger::init_logging() {
        eprintln!("failed to initialize logging: {error}");
    }
    if let Err(error) = ffmpeg_next::init() {
        eprintln!("failed to initialize ffmpeg: {error}");
    }

    info!("====== TackleCast starting ======");
    info!("platform: {}", std::env::consts::OS);
    info!("version: {}", env!("CARGO_PKG_VERSION"));
    info!(
        "build: {}",
        if cfg!(debug_assertions) { "debug" } else { "release" }
    );
    let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID)) };

    let settings = Settings::load();
    info!("loaded settings from {}", settings::settings_path().display());
    let video_devices = devices::enumerate_video_devices();
    let audio_inputs = devices::enumerate_audio_inputs();
    let audio_outputs = devices::enumerate_audio_outputs();
    info!("video devices: {:?}", video_devices);
    info!("audio inputs: {:?}", audio_inputs);
    info!("audio outputs: {:?}", audio_outputs);

    let event_loop = EventLoop::new().expect("failed to create event loop");
    let cli = CliArgs::parse();
    let mut app = App::new(
        settings,
        cli.test_mode,
        cli.test_pattern,
        video_devices,
        audio_inputs,
        audio_outputs,
    );
    event_loop.run_app(&mut app).expect("event loop error");
}

struct App {
    settings: Settings,
    test_mode: bool,
    test_pattern: TestPatternMode,
    video_devices: Vec<String>,
    audio_inputs: Vec<AudioDevice>,
    audio_outputs: Vec<AudioDevice>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    ui: Option<UiState>,
    capture: Option<CaptureThread>,
    audio: AudioPassthrough,
    latest_stats: Option<CaptureStats>,
    latest_error: Option<String>,
    is_fullscreen: bool,
    is_minimized: bool,
}

impl App {
    fn new(
        settings: Settings,
        test_mode: bool,
        test_pattern: TestPatternMode,
        video_devices: Vec<String>,
        audio_inputs: Vec<AudioDevice>,
        audio_outputs: Vec<AudioDevice>,
    ) -> Self {
        Self {
            settings,
            test_mode,
            test_pattern,
            video_devices,
            audio_inputs,
            audio_outputs,
            window: None,
            renderer: None,
            ui: None,
            capture: None,
            audio: AudioPassthrough::new(),
            latest_stats: None,
            latest_error: None,
            is_fullscreen: false,
            is_minimized: false,
        }
    }

    fn create_window(event_loop: &ActiveEventLoop) -> Window {
        event_loop
            .create_window(
                WindowAttributes::default()
                    .with_title(WINDOW_TITLE)
                    .with_resizable(true)
                    .with_inner_size(PhysicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT))
                    .with_window_icon(load_window_icon()),
            )
            .expect("failed to create window")
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let window = Arc::new(Self::create_window(event_loop));
        let renderer = match pollster::block_on(Renderer::new(window.clone())) {
            Ok(renderer) => renderer,
            Err(error) => {
                error!("renderer initialization failed: {error}");
                event_loop.exit();
                return;
            }
        };

        let ui = UiState::new(&window, renderer.max_texture_side());

        self.renderer = Some(renderer);
        self.ui = Some(ui);
        self.window = Some(window);

        self.start_capture();
        self.start_audio();

        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = &self.window else {
            return;
        };

        if window.id() != window_id {
            return;
        }

        if let WindowEvent::KeyboardInput { event, .. } = &event {
            if event.state == ElementState::Pressed && !event.repeat {
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        if let Some(ui) = &mut self.ui {
                            if ui.is_menu_open() {
                                if let Some(settings) = ui.close_menu_and_apply() {
                                    self.apply_settings(settings);
                                }
                            } else {
                                ui.open_menu(&self.settings);
                            }
                        }
                        return;
                    }
                    Key::Named(NamedKey::F11) => {
                        self.toggle_fullscreen();
                        return;
                    }
                    _ => {}
                }
            }
        }

        if let Some(ui) = &mut self.ui {
            ui.on_window_event(window, &event);
        }

        match event {
            WindowEvent::CloseRequested => {
                if let Some(capture) = &mut self.capture {
                    capture.stop();
                }
                self.audio.stop();
                if let Err(error) = self.settings.save() {
                    error!("failed to save settings on exit: {error}");
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.is_minimized = size.width == 0 || size.height == 0;
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size);
                }
                if !self.is_minimized {
                    window.request_redraw();
                }
            }
            WindowEvent::Occluded(occluded) => {
                self.is_minimized = occluded;
                if !self.is_minimized {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if self.is_minimized {
                    return;
                }
                let overlay = self.overlay_info();
                if let (Some(renderer), Some(ui)) = (&mut self.renderer, &mut self.ui) {
                    let prepared_ui = ui.prepare(
                        window,
                        UiFrame {
                            overlay: &overlay,
                            settings: &self.settings,
                            video_devices: &self.video_devices,
                            audio_inputs: &self.audio_inputs,
                            audio_outputs: &self.audio_outputs,
                            is_fullscreen: self.is_fullscreen,
                        },
                    );
                    let toggle_fullscreen = prepared_ui.output.toggle_fullscreen;
                    let exit_requested = prepared_ui.output.exit_requested;
                    let apply_settings = prepared_ui.output.apply_settings.clone();
                    if let Err(error) = renderer.render(Some(prepared_ui)) {
                        error!("render error: {error}");
                    }
                    if toggle_fullscreen {
                        self.toggle_fullscreen();
                    }
                    if let Some(settings) = apply_settings {
                        self.apply_settings(settings);
                    }
                    if exit_requested {
                        if let Some(capture) = &mut self.capture {
                            capture.stop();
                        }
                        self.audio.stop();
                        if let Err(error) = self.settings.save() {
                            error!("failed to save settings on exit: {error}");
                        }
                        event_loop.exit();
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let (Some(capture), Some(renderer)) = (&self.capture, &mut self.renderer) {
            if let Some(frame) = capture.latest_frame() {
                self.latest_error = None;
                renderer.upload_frame(&frame);
            }

            if let Some(stats) = capture.latest_stats() {
                self.latest_stats = Some(stats);
            }

            if let Some(error_message) = capture.latest_error() {
                self.latest_error = Some(error_message.clone());
                warn!("capture error: {error_message}");
            }

            // If the capture thread fell back to a different resolution/fps,
            // update settings to reflect what's actually running.
            if let Some(negotiated) = capture.latest_negotiated() {
                info!(
                    "updating settings to match negotiated capture: {}x{} @ {}fps",
                    negotiated.width, negotiated.height, negotiated.fps
                );
                self.settings.apply_negotiated(
                    negotiated.width,
                    negotiated.height,
                    negotiated.fps,
                );
                if let Err(error) = self.settings.save() {
                    error!("failed to save negotiated settings: {error}");
                }
            }
        }

        if !self.is_minimized {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }
}

impl App {
    fn apply_settings(&mut self, new_settings: Settings) {
        let old_settings = self.settings.clone();
        self.settings = new_settings;

        if let Err(error) = self.settings.save() {
            error!("failed to save settings: {error}");
        }

        let video_changed = old_settings.video_device != self.settings.video_device
            || old_settings.resolution != self.settings.resolution
            || old_settings.fps_mode != self.settings.fps_mode
            || old_settings.custom_fps != self.settings.custom_fps;

        let audio_device_changed = old_settings.video_device != self.settings.video_device
            || old_settings.audio_input != self.settings.audio_input
            || old_settings.audio_output != self.settings.audio_output;

        if video_changed {
            if let Some(capture) = &mut self.capture {
                capture.stop();
            }
            self.start_capture();
        }

        if audio_device_changed {
            self.audio.stop();
            self.start_audio();
        } else if (old_settings.volume - self.settings.volume).abs() > f64::EPSILON {
            self.audio.set_volume(self.settings.volume);
        }
    }

    fn overlay_info(&self) -> OverlayInfo {
        let status_message = if let Some(error_message) = self.latest_error.as_ref() {
            Some(error_message.clone())
        } else if self.latest_stats.is_none() {
            Some("Connecting...".to_string())
        } else {
            None
        };

        OverlayInfo {
            width: self.latest_stats.map(|stats| stats.width),
            height: self.latest_stats.map(|stats| stats.height),
            fps: self.latest_stats.map(|stats| stats.fps),
            show_overlay: self.settings.show_overlay && !self.is_minimized,
            status_message,
            status_is_alert: self.latest_error.is_some() || self.latest_stats.is_none(),
        }
    }

    fn toggle_fullscreen(&mut self) {
        self.is_fullscreen = !self.is_fullscreen;
        if let Some(window) = &self.window {
            if self.is_fullscreen {
                window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
            } else {
                window.set_fullscreen(None);
            }
        }
    }

    fn start_capture(&mut self) {
        self.latest_stats = None;
        self.latest_error = None;
        let capture_config = get_capture_config(&self.settings.resolution, self.settings.get_fps());
        if self.test_mode {
            self.start_test_capture(
                capture_config.width,
                capture_config.height,
                capture_config.fps,
                "CLI test mode enabled",
            );
            return;
        }

        let video_device = if self.settings.video_device.is_empty() {
            self.video_devices.first().cloned().unwrap_or_default()
        } else {
            self.settings.video_device.clone()
        };

        if video_device.is_empty() {
            warn!("no video device configured or discovered");
            self.start_test_capture(
                capture_config.width,
                capture_config.height,
                capture_config.fps,
                "No video device discovered, using test pattern fallback",
            );
            return;
        }

        info!(
            "starting DirectShow capture: device='{}' {}x{} @ {}fps ({}, threads={})",
            video_device,
            capture_config.width,
            capture_config.height,
            capture_config.fps,
            capture_config.pixel_format,
            capture_config.decode_threads
        );
        self.capture = Some(CaptureThread::start(CaptureConfig {
            width: capture_config.width,
            height: capture_config.height,
            fps: capture_config.fps,
            source: CaptureSource::DirectShow {
                device_name: video_device,
                pixel_format: capture_config.pixel_format.to_string(),
                decode_threads: capture_config.decode_threads,
            },
        }));
    }

    fn start_test_capture(&mut self, width: u32, height: u32, fps: u32, reason: &str) {
        let (alternate_formats, force_format) = match self.test_pattern {
            TestPatternMode::Alternate => (true, None),
            TestPatternMode::Nv12 => (false, Some(PixelFormat::Nv12)),
            TestPatternMode::Yuvj422p => (false, Some(PixelFormat::Yuvj422p)),
        };

        info!("starting test-pattern capture: {}x{} @ {}fps ({reason})", width, height, fps);
        self.capture = Some(CaptureThread::start(CaptureConfig {
            width,
            height,
            fps,
            source: CaptureSource::TestPattern {
                alternate_formats,
                force_format,
            },
        }));
    }

    fn start_audio(&mut self) {
        if self.test_mode {
            return;
        }

        if self.settings.video_device.is_empty() && self.video_devices.is_empty() {
            info!("skipping audio passthrough because no video capture device is available");
            return;
        }

        let video_device = if self.settings.video_device.is_empty() {
            self.video_devices.first().cloned().unwrap_or_default()
        } else {
            self.settings.video_device.clone()
        };

        self.audio.start(
            &video_device,
            self.settings.audio_input,
            self.settings.audio_output,
            self.settings.volume,
        );
    }
}

#[allow(dead_code)]
fn project_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn load_window_icon() -> Option<Icon> {
    let icon_path = project_root().join("assets").join("icon.ico");
    let image = image::open(icon_path).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height).ok()
}
