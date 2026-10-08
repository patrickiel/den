//! Notification sounds, as den makes them: short tones synthesized from a
//! few notes with a quick attack and an exponential decay, so the app ships
//! no audio files. Rendered to a WAV in memory and played by Windows'
//! PlaySound; elsewhere written once under den's data folder and played by
//! the system's player (`afplay` on macOS).

use std::{collections::HashMap, sync::Mutex};

use crate::pane::AlertKind;

#[derive(Clone, Copy)]
pub enum Wave {
    Sine,
    Triangle,
    Square,
}

/// Hz, start (s), time to fade out (s), wave, loudness 0..1.
type Note = (f32, f32, f32, Wave, f32);

/// den's sounds: id, name, notes.
pub const SOUNDS: &[(&str, &str, &[Note])] = &[
    ("chime", "Chime", &[(880., 0., 0.5, Wave::Sine, 1.), (1318.5, 0.12, 0.7, Wave::Sine, 1.)]),
    ("ping", "Ping", &[(1760., 0., 0.35, Wave::Sine, 0.7)]),
    ("pop", "Pop", &[(520., 0., 0.09, Wave::Triangle, 1.), (780., 0.07, 0.12, Wave::Triangle, 1.)]),
    ("bell", "Bell", &[(660., 0., 1.2, Wave::Sine, 1.), (1320., 0., 0.8, Wave::Sine, 0.35), (1980., 0., 0.4, Wave::Sine, 0.15)]),
    ("alert", "Alert", &[(988., 0., 0.14, Wave::Square, 0.35), (988., 0.2, 0.14, Wave::Square, 0.35)]),
    ("rise", "Rise", &[(523.3, 0., 0.25, Wave::Sine, 1.), (659.3, 0.1, 0.25, Wave::Sine, 1.), (784., 0.2, 0.45, Wave::Sine, 1.)]),
];

const RATE: u32 = 44_100;

/// The sound as 16-bit mono PCM in a WAV file, at `volume` (0..100).
fn render(notes: &[Note], volume: u32) -> Vec<u8> {
    let length = notes.iter().map(|&(_, at, dur, ..)| at + dur + 0.05).fold(0., f32::max);
    let samples = (length * RATE as f32) as usize;
    let master = volume.min(100) as f32 / 100. * 0.4;
    let mut pcm = vec![0f32; samples];
    for &(freq, at, dur, wave, gain) in notes {
        let attack = 0.008;
        for (i, out) in pcm.iter_mut().enumerate() {
            let t = i as f32 / RATE as f32 - at;
            if t < 0. || t > dur {
                continue;
            }
            // A linear rise, then an exponential fall to 0.0001 at `dur`.
            let envelope = if t < attack { gain * t / attack } else { gain * (0.0001f32 / gain).powf((t - attack) / (dur - attack).max(0.001)) };
            let phase = (t * freq).fract();
            let value = match wave {
                Wave::Sine => (phase * std::f32::consts::TAU).sin(),
                Wave::Triangle => 1. - 4. * (phase - 0.5).abs(),
                Wave::Square => {
                    if phase < 0.5 {
                        1.
                    } else {
                        -1.
                    }
                }
            };
            *out += value * envelope;
        }
    }
    let data: Vec<u8> = pcm.iter().flat_map(|s| (((s * master).clamp(-1., 1.) * i16::MAX as f32) as i16).to_le_bytes()).collect();
    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    wav
}

/// Rendered sounds, kept: PlaySound reads the memory while it plays.
static CACHE: Mutex<Option<HashMap<(String, u32), &'static [u8]>>> = Mutex::new(None);

#[cfg(windows)]
#[link(name = "winmm")]
unsafe extern "system" {
    fn PlaySoundW(sound: *const u8, module: *mut std::ffi::c_void, flags: u32) -> i32;
}

/// Play sound `name` (an id of `SOUNDS`; "none" is silent) at `volume`.
pub fn play_named(name: &str, volume: u32) {
    let Some((_, _, notes)) = SOUNDS.iter().find(|(id, ..)| *id == name) else { return };
    if volume == 0 {
        return;
    }
    let Ok(mut cache) = CACHE.lock() else { return };
    let wav: &'static [u8] = *cache
        .get_or_insert_with(HashMap::new)
        .entry((name.to_string(), volume))
        .or_insert_with(|| Box::leak(render(notes, volume).into_boxed_slice()));
    #[cfg(windows)]
    {
        const SND_ASYNC: u32 = 0x0001;
        const SND_NODEFAULT: u32 = 0x0002;
        const SND_MEMORY: u32 = 0x0004;
        // SAFETY: the WAV lives for the rest of the process (leaked above).
        unsafe {
            PlaySoundW(wav.as_ptr(), std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
        }
    }
    #[cfg(not(windows))]
    play_file(name, volume, wav);
}

/// Play the WAV from a file under den's data folder (written once per sound
/// and volume) with the system's player, which runs on its own.
#[cfg(not(windows))]
fn play_file(name: &str, volume: u32, wav: &[u8]) {
    let dir = crate::settings::data_dir().join("sounds");
    let path = dir.join(format!("{name}-{volume}.wav"));
    if !path.is_file() && (std::fs::create_dir_all(&dir).is_err() || std::fs::write(&path, wav).is_err()) {
        return;
    }
    let players: &[&str] = if cfg!(target_os = "macos") { &["afplay"] } else { &["paplay", "aplay"] };
    for player in players {
        let mut cmd = std::process::Command::new(player);
        cmd.arg(&path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        if cmd.spawn().is_ok() {
            return;
        }
    }
}

/// The sound Settings pick for `kind`.
pub fn play(kind: AlertKind, cx: &gpui_kit::App) {
    let settings = crate::settings::Settings::get(cx);
    let name = if kind == AlertKind::Done { &settings.notify_sound_done } else { &settings.notify_sound_input };
    play_named(name, settings.notify_volume);
}

#[cfg(test)]
mod tests {
    use super::{SOUNDS, render};

    #[test]
    fn renders_a_wav() {
        let (_, _, notes) = SOUNDS[0];
        let wav = render(notes, 60);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        // About 0.87 s of 16-bit samples, and not silent.
        assert!(wav.len() > 44 + 44_100);
        assert!(wav[44..].iter().any(|&b| b != 0));
    }
}
