use std::process::Command;

use cpal::traits::{DeviceTrait, HostTrait};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[allow(dead_code)]
const IGNORED_KEYWORDS: &[&str] = &["pro", "the", "and"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub index: i32,
    pub name: String,
}

pub fn enumerate_video_devices() -> Vec<String> {
    let ffmpeg = ffmpeg_path();
    let output = Command::new(ffmpeg)
        .args(["-f", "dshow", "-list_devices", "true", "-i", "dummy"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };

    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(parse_video_device_line)
        .collect()
}

pub fn enumerate_audio_inputs() -> Vec<AudioDevice> {
    enumerate_audio_devices(Direction::Input)
}

pub fn enumerate_audio_outputs() -> Vec<AudioDevice> {
    enumerate_audio_devices(Direction::Output)
}

#[allow(dead_code)]
pub fn find_audio_input_for_video(video_device_name: &str, inputs: &[AudioDevice]) -> Option<i32> {
    let keywords = keywords_for_device_name(video_device_name);
    if keywords.is_empty() {
        return None;
    }

    let mut best_index = None;
    let mut best_score = 0;
    for device in inputs {
        let haystack = device.name.to_ascii_lowercase();
        let score = keywords.iter().filter(|keyword| haystack.contains(keyword.as_str())).count();
        if score > best_score {
            best_score = score;
            best_index = Some(device.index);
        }
    }

    let threshold = if keywords.len() == 1 { 1 } else { 2 };
    (best_score >= threshold).then_some(best_index?).or(None)
}

fn enumerate_audio_devices(direction: Direction) -> Vec<AudioDevice> {
    let host = preferred_audio_host();
    let Ok(devices) = host.devices() else {
        return Vec::new();
    };

    devices
        .enumerate()
        .filter_map(|(index, device)| {
            let name = device.name().ok()?;
            let supported = match direction {
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
            };

            supported.then_some(AudioDevice {
                index: index as i32,
                name,
            })
        })
        .collect()
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

fn ffmpeg_path() -> String {
    std::env::var("FFMPEG")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("FFMPEG_DIR").ok().map(|dir| {
                format!(
                    "{}\\bin\\ffmpeg.exe",
                    dir.trim_end_matches(['\\', '/'])
                )
            })
        })
        .unwrap_or_else(|| "ffmpeg".to_string())
}

fn parse_video_device_line(line: &str) -> Option<String> {
    let marker = "(video)";
    let marker_index = line.find(marker)?;
    let prefix = &line[..marker_index];
    let start = prefix.rfind('"')?;
    let before_start = &prefix[..start];
    let open = before_start.rfind('"')?;
    Some(prefix[open + 1..start].to_string())
}

#[allow(dead_code)]
fn keywords_for_device_name(device_name: &str) -> Vec<String> {
    device_name
        .to_ascii_lowercase()
        .replace('-', " ")
        .split_whitespace()
        .filter(|word| word.len() >= 3 && !IGNORED_KEYWORDS.contains(word))
        .map(ToString::to_string)
        .collect()
}

#[derive(Clone, Copy)]
enum Direction {
    Input,
    Output,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffmpeg_device_line() {
        let line = r#"[dshow @ 000001]  "ShadowCast 3" (video)"#;
        assert_eq!(parse_video_device_line(line).as_deref(), Some("ShadowCast 3"));
    }

    #[test]
    fn auto_detect_matches_keyword_overlap() {
        let inputs = vec![
            AudioDevice {
                index: 2,
                name: "Microphone (USB Audio Device)".to_string(),
            },
            AudioDevice {
                index: 7,
                name: "ShadowCast Capture Audio".to_string(),
            },
        ];

        assert_eq!(
            find_audio_input_for_video("ShadowCast Capture", &inputs),
            Some(7)
        );
    }
}
