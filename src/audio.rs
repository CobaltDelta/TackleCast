use std::sync::{
    atomic::{AtomicU32, AtomicUsize, Ordering},
    Arc,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use tracing::{error, info, warn};

use crate::devices;

const BUFFER_FRAMES: u32 = 256;
const RING_BUFFER_CAPACITY: usize = 48_000 * 2;

pub struct AudioPassthrough {
    input_stream: Option<Stream>,
    output_stream: Option<Stream>,
    volume_bits: Arc<AtomicU32>,
}

impl AudioPassthrough {
    pub fn new() -> Self {
        Self {
            input_stream: None,
            output_stream: None,
            volume_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        }
    }

    pub fn start(
        &mut self,
        video_device_name: &str,
        input_index: i32,
        output_index: i32,
        volume: f64,
    ) {
        self.stop();
        self.set_volume(volume);

        let host = preferred_audio_host();
        let inputs = devices::enumerate_audio_inputs();
        let auto_detected_input = devices::find_audio_input_for_video(video_device_name, &inputs);

        let resolved_input_index = if input_index >= 0 {
            if inputs.iter().any(|device| device.index == input_index) {
                Some(input_index)
            } else if let Some(index) = auto_detected_input {
                warn!(
                    "saved audio input index {} is no longer valid; using auto-detected capture input {}",
                    input_index, index
                );
                Some(index)
            } else {
                None
            }
        } else {
            if let Some(index) = auto_detected_input {
                info!(
                    "audio auto-detect matched input {} for video '{}'",
                    index, video_device_name
                );
                Some(index)
            } else {
                None
            }
        };

        let input_device = match resolve_input_device(&host, resolved_input_index) {
            Ok(device) => device,
            Err(error) => {
                warn!("audio input unavailable: {error}; falling back to default input");
                match host.default_input_device() {
                    Some(device) => device,
                    None => return,
                }
            }
        };

        let output_device = match resolve_output_device(
            &host,
            if output_index >= 0 { Some(output_index) } else { None },
        ) {
            Ok(device) => device,
            Err(error) => {
                warn!("audio output unavailable: {error}; falling back to default output");
                match host.default_output_device() {
                    Some(device) => device,
                    None => return,
                }
            }
        };

        let input_name = input_device
            .name()
            .unwrap_or_else(|_| "<unknown input>".to_string());
        let output_name = output_device
            .name()
            .unwrap_or_else(|_| "<unknown output>".to_string());

        let input_default = match input_device.default_input_config() {
            Ok(config) => config,
            Err(error) => {
                warn!("audio input config unavailable for '{}': {}", input_name, error);
                return;
            }
        };

        let output_supported = match output_device.supported_output_configs() {
            Ok(configs) => configs.collect::<Vec<_>>(),
            Err(error) => {
                warn!(
                    "audio output configs unavailable for '{}': {}",
                    output_name, error
                );
                return;
            }
        };

        let input_rate = input_default.sample_rate().0;
        let input_channels = input_default.channels();
        let channels = match choose_channel_count(input_channels, &output_supported) {
            Some(channels) => channels,
            None => {
                warn!(
                    "no compatible audio channel count between '{}' and '{}'",
                    input_name, output_name
                );
                return;
            }
        };

        let output_format = match choose_output_config(&output_supported, input_rate, channels) {
            Some(config) => config,
            None => {
                warn!(
                    "audio output '{}' does not support {} Hz / {} channels",
                    output_name, input_rate, channels
                );
                return;
            }
        };

        let stream_config = StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(input_rate),
            buffer_size: cpal::BufferSize::Fixed(BUFFER_FRAMES),
        };

        info!(
            "starting audio passthrough: input='{}' ({:?}), output='{}' ({:?}), {}ch @ {}Hz",
            input_name,
            input_default.sample_format(),
            output_name,
            output_format.sample_format(),
            channels,
            input_rate
        );

        let ring = Arc::new(AudioRingBuffer::new(RING_BUFFER_CAPACITY));
        let input_stream = match build_input_stream(
            &input_device,
            &stream_config,
            input_default.sample_format(),
            ring.clone(),
        ) {
            Ok(stream) => stream,
            Err(error) => {
                error!("failed to build input audio stream: {error}");
                return;
            }
        };

        let output_stream = match build_output_stream(
            &output_device,
            &stream_config,
            output_format.sample_format(),
            ring,
            self.volume_bits.clone(),
        ) {
            Ok(stream) => stream,
            Err(error) => {
                error!("failed to build output audio stream: {error}");
                return;
            }
        };

        if let Err(error) = input_stream.play() {
            error!("failed to start input audio stream: {error}");
            return;
        }
        if let Err(error) = output_stream.play() {
            error!("failed to start output audio stream: {error}");
            return;
        }

        self.input_stream = Some(input_stream);
        self.output_stream = Some(output_stream);
    }

    pub fn stop(&mut self) {
        self.input_stream.take();
        self.output_stream.take();
    }

    pub fn set_volume(&mut self, volume: f64) {
        let clamped = volume.clamp(0.0, 1.0) as f32;
        self.volume_bits
            .store(clamped.to_bits(), Ordering::Relaxed);
    }
}

fn build_input_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    ring: Arc<AudioRingBuffer>,
) -> Result<Stream, cpal::BuildStreamError> {
    match sample_format {
        SampleFormat::I8 => build_input_stream_typed::<i8>(device, config, ring),
        SampleFormat::U8 => build_input_stream_typed::<u8>(device, config, ring),
        SampleFormat::F32 => build_input_stream_typed::<f32>(device, config, ring),
        SampleFormat::I16 => build_input_stream_typed::<i16>(device, config, ring),
        SampleFormat::U16 => build_input_stream_typed::<u16>(device, config, ring),
        other => Err(cpal::BuildStreamError::StreamConfigNotSupported).inspect_err(|_error| {
            warn!("unsupported input sample format: {:?}", other);
        }),
    }
}

fn build_output_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    ring: Arc<AudioRingBuffer>,
    volume_bits: Arc<AtomicU32>,
) -> Result<Stream, cpal::BuildStreamError> {
    match sample_format {
        SampleFormat::I8 => build_output_stream_typed::<i8>(device, config, ring, volume_bits),
        SampleFormat::U8 => build_output_stream_typed::<u8>(device, config, ring, volume_bits),
        SampleFormat::F32 => build_output_stream_typed::<f32>(device, config, ring, volume_bits),
        SampleFormat::I16 => build_output_stream_typed::<i16>(device, config, ring, volume_bits),
        SampleFormat::U16 => build_output_stream_typed::<u16>(device, config, ring, volume_bits),
        other => Err(cpal::BuildStreamError::StreamConfigNotSupported).inspect_err(|_error| {
            warn!("unsupported output sample format: {:?}", other);
        }),
    }
}

fn build_input_stream_typed<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    ring: Arc<AudioRingBuffer>,
) -> Result<Stream, cpal::BuildStreamError>
where
    T: Sample + SizedSample,
    f32: FromSample<T>,
{
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut converted = [0.0f32; 2048];
            let mut offset = 0;
            while offset < data.len() {
                let chunk_len = (data.len() - offset).min(converted.len());
                for index in 0..chunk_len {
                    converted[index] = f32::from_sample(data[offset + index]);
                }
                ring.push_samples(&converted[..chunk_len]);
                offset += chunk_len;
            }
        },
        move |error| {
            error!("audio input stream error: {error}");
        },
        None,
    )
}

fn build_output_stream_typed<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    ring: Arc<AudioRingBuffer>,
    volume_bits: Arc<AtomicU32>,
) -> Result<Stream, cpal::BuildStreamError>
where
    T: Sample + SizedSample + FromSample<f32>,
{
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let mut scratch = [0.0f32; 2048];
            let mut offset = 0;
            while offset < data.len() {
                let chunk_len = (data.len() - offset).min(scratch.len());
                let filled = ring.pop_samples(&mut scratch[..chunk_len]);
                let volume = f32::from_bits(volume_bits.load(Ordering::Relaxed));
                for index in 0..chunk_len {
                    let sample = if index < filled {
                        scratch[index] * volume
                    } else {
                        0.0
                    };
                    data[offset + index] = T::from_sample(sample);
                }
                offset += chunk_len;
            }
        },
        move |error| {
            error!("audio output stream error: {error}");
        },
        None,
    )
}

fn choose_channel_count(
    input_channels: u16,
    output_configs: &[cpal::SupportedStreamConfigRange],
) -> Option<u16> {
    let input_channels = input_channels.min(2);
    (1..=input_channels)
        .rev()
        .find(|channels| output_configs.iter().any(|config| config.channels() >= *channels))
}

fn choose_output_config(
    output_configs: &[cpal::SupportedStreamConfigRange],
    sample_rate: u32,
    channels: u16,
) -> Option<cpal::SupportedStreamConfigRange> {
    let mut candidates: Vec<_> = output_configs
        .iter()
        .filter(|config| {
            config.channels() >= channels
                && config.min_sample_rate().0 <= sample_rate
                && config.max_sample_rate().0 >= sample_rate
        })
        .cloned()
        .collect();

    candidates.sort_by_key(|config| {
        (
            output_format_rank(config.sample_format()),
            config.channels() != channels,
        )
    });

    candidates.into_iter().next()
}

fn output_format_rank(sample_format: SampleFormat) -> u8 {
    match sample_format {
        SampleFormat::F32 => 0,
        SampleFormat::I16 => 1,
        SampleFormat::U16 => 2,
        SampleFormat::I8 => 3,
        SampleFormat::U8 => 4,
        _ => 10,
    }
}

fn resolve_input_device(host: &cpal::Host, index: Option<i32>) -> Result<cpal::Device, String> {
    if let Some(index) = index {
        resolve_device_by_index(host, index, Direction::Input)
            .ok_or_else(|| format!("input device index {} was not found", index))
    } else {
        host.default_input_device()
            .ok_or_else(|| "no default input device found".to_string())
    }
}

fn resolve_output_device(host: &cpal::Host, index: Option<i32>) -> Result<cpal::Device, String> {
    if let Some(index) = index {
        resolve_device_by_index(host, index, Direction::Output)
            .ok_or_else(|| format!("output device index {} was not found", index))
    } else {
        host.default_output_device()
            .ok_or_else(|| "no default output device found".to_string())
    }
}

fn resolve_device_by_index(
    host: &cpal::Host,
    index: i32,
    direction: Direction,
) -> Option<cpal::Device> {
    let Ok(devices) = host.devices() else {
        return None;
    };

    devices
        .enumerate()
        .find_map(|(device_index, device)| {
            (device_index as i32 == index && device_supports_direction(&device, direction))
                .then_some(device)
        })
}

fn device_supports_direction(device: &cpal::Device, direction: Direction) -> bool {
    match direction {
        Direction::Input => device
            .supported_input_configs()
            .ok()
            .and_then(|mut configs| configs.next())
            .is_some(),
        Direction::Output => device
            .supported_output_configs()
            .ok()
            .and_then(|mut configs| configs.next())
            .is_some(),
    }
}

fn preferred_audio_host() -> cpal::Host {
    #[cfg(target_os = "windows")]
    {
        if let Ok(host) = cpal::host_from_id(cpal::HostId::Wasapi) {
            return host;
        }
    }

    cpal::default_host()
}

#[derive(Clone, Copy)]
enum Direction {
    Input,
    Output,
}

struct AudioRingBuffer {
    data: Box<[AtomicU32]>,
    capacity: usize,
    read_index: AtomicUsize,
    write_index: AtomicUsize,
}

impl AudioRingBuffer {
    fn new(capacity: usize) -> Self {
        let mut values = Vec::with_capacity(capacity);
        values.resize_with(capacity, || AtomicU32::new(0));
        Self {
            data: values.into_boxed_slice(),
            capacity,
            read_index: AtomicUsize::new(0),
            write_index: AtomicUsize::new(0),
        }
    }

    fn push_samples(&self, samples: &[f32]) -> usize {
        let mut written = 0;
        let mut write = self.write_index.load(Ordering::Relaxed);
        let read = self.read_index.load(Ordering::Acquire);

        while written < samples.len() && write.wrapping_sub(read) < self.capacity {
            let slot = write % self.capacity;
            self.data[slot].store(samples[written].to_bits(), Ordering::Relaxed);
            write = write.wrapping_add(1);
            written += 1;
        }

        if written > 0 {
            self.write_index.store(write, Ordering::Release);
        }

        written
    }

    fn pop_samples(&self, output: &mut [f32]) -> usize {
        let mut read = self.read_index.load(Ordering::Relaxed);
        let write = self.write_index.load(Ordering::Acquire);
        let available = write.wrapping_sub(read);
        let count = available.min(output.len());

        for sample in output.iter_mut().take(count) {
            let slot = read % self.capacity;
            *sample = f32::from_bits(self.data[slot].load(Ordering::Relaxed));
            read = read.wrapping_add(1);
        }

        if count > 0 {
            self.read_index.store(read, Ordering::Release);
        }

        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_round_trips_samples_in_order() {
        let ring = AudioRingBuffer::new(8);
        assert_eq!(ring.push_samples(&[0.1, 0.2, 0.3]), 3);

        let mut out = [0.0; 4];
        let count = ring.pop_samples(&mut out);
        assert_eq!(count, 3);
        assert!((out[0] - 0.1).abs() < 0.0001);
        assert!((out[1] - 0.2).abs() < 0.0001);
        assert!((out[2] - 0.3).abs() < 0.0001);
    }

    #[test]
    fn ring_buffer_limits_to_capacity() {
        let ring = AudioRingBuffer::new(2);
        assert_eq!(ring.push_samples(&[0.1, 0.2, 0.3]), 2);
    }
}
