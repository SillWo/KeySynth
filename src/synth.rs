use crate::events::{KeyEvent, Shared};
use crate::mapping::note_for_key;
use crate::settings::{Mode, Waveform};

const VOICE_COUNT: usize = 128;
const KEY_COUNT: usize = 512;

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    key_id: u16,
    phase_step: f32,
    waveform: Waveform,
    phase: f32,
    envelope: f32,
    attack_step: f32,
    releasing: bool,
    release_step: f32,
}

impl Voice {
    const SILENT: Self = Self {
        active: false,
        key_id: 0,
        phase_step: 0.0,
        waveform: Waveform::Sine,
        phase: 0.0,
        envelope: 0.0,
        attack_step: 1.0,
        releasing: false,
        release_step: 0.0,
    };
}

pub struct Synth {
    sample_rate: f32,
    seed: u64,
    voices: [Voice; VOICE_COUNT],
    pressed: [bool; KEY_COUNT],
    waveform: Waveform,
    mode: Mode,
    attack_ms: u32,
    release_ms: u32,
    enabled: bool,
    was_enabled: bool,
    target_volume: f32,
    volume: f32,
    volume_ramp_step: f32,
    transpose: i32,
    age_counter: u64,
    voice_age: [u64; VOICE_COUNT],
}

impl Synth {
    pub fn new(sample_rate: u32, seed: u64) -> Self {
        Self {
            sample_rate: sample_rate.max(1) as f32,
            seed,
            voices: [Voice::SILENT; VOICE_COUNT],
            pressed: [false; KEY_COUNT],
            waveform: Waveform::Sine,
            mode: Mode::WickiHayden,
            attack_ms: 10,
            release_ms: 50,
            enabled: true,
            was_enabled: true,
            target_volume: 0.25,
            volume: 0.25,
            volume_ramp_step: 1.0 / (sample_rate.max(1) as f32 * 0.005),
            transpose: 0,
            age_counter: 0,
            voice_age: [0; VOICE_COUNT],
        }
    }

    /// Updates key state without allocating or waiting on a lock. Repeated key
    /// down messages are ignored until the matching key up arrives.
    pub fn handle_event(&mut self, event: KeyEvent, shared: &Shared) {
        let key = event.key_id as usize;
        if key >= KEY_COUNT {
            return;
        }
        if event.down {
            if self.pressed[key] {
                return;
            }
            self.pressed[key] = true;
            if !self.enabled {
                return;
            }
            let Some(note) = note_for_key(event.key_id, self.mode, self.transpose, self.seed)
            else {
                return;
            };
            self.start_voice(event.key_id, note);
        } else {
            if !self.pressed[key] {
                return;
            }
            self.pressed[key] = false;
            self.release_key(event.key_id);
        }
        // Reading shared here also makes event handling respond safely if the UI
        // changed settings just before the audio thread drained this event.
        if shared.shutdown.load(std::sync::atomic::Ordering::Acquire) {
            self.reset();
        }
    }

    /// Call once at the start of an audio buffer to snapshot UI parameters.
    pub fn sync_parameters(&mut self, shared: &Shared) {
        use std::sync::atomic::Ordering::Acquire;
        self.was_enabled = self.enabled;
        self.enabled = shared.enabled.load(Acquire);
        self.target_volume = shared.volume.load(Acquire).min(100) as f32 / 100.0;
        self.transpose = shared.transpose.load(Acquire).clamp(-24, 24);
        self.waveform = Waveform::from_u32(shared.waveform.load(Acquire));
        self.mode = Mode::from_u32(shared.mode.load(Acquire));
        self.attack_ms = shared.attack_ms.load(Acquire).min(200);
        self.release_ms = shared.release_ms.load(Acquire).min(500);
        if self.was_enabled && !self.enabled {
            self.release_voices();
        }
    }

    pub fn next_sample(&mut self) -> f32 {
        // A roughly 5 ms linear gain ramp prevents clicks when the UI moves volume.
        let ramp = self.volume_ramp_step;
        if self.volume < self.target_volume {
            self.volume = (self.volume + ramp).min(self.target_volume);
        } else if self.volume > self.target_volume {
            self.volume = (self.volume - ramp).max(self.target_volume);
        }

        let mut mix = 0.0;
        for voice in &mut self.voices {
            if !voice.active {
                continue;
            }
            if voice.releasing {
                voice.envelope = (voice.envelope - voice.release_step).max(0.0);
                if voice.envelope == 0.0 {
                    voice.active = false;
                    continue;
                }
            } else {
                voice.envelope = (voice.envelope + voice.attack_step).min(1.0);
            }
            let sample = match voice.waveform {
                Waveform::Sine => (voice.phase * std::f32::consts::TAU).sin(),
                Waveform::Triangle => triangle(voice.phase),
                Waveform::Sawtooth => poly_blep_saw(voice.phase, voice.phase_step),
                Waveform::Square => poly_blep_square(voice.phase, voice.phase_step),
            };
            mix += sample * voice.envelope;
            voice.phase += voice.phase_step;
            if voice.phase >= 1.0 {
                voice.phase -= 1.0;
            }
        }
        // Fixed headroom avoids level jumps as voices end; the bounded soft
        // saturation keeps large chords inside the audio range without clipping.
        let scaled = mix * self.volume * 0.25;
        scaled / (1.0 + scaled.abs())
    }

    pub fn is_active(&self) -> bool {
        self.voices.iter().any(|voice| voice.active)
    }

    /// Releases every sounding voice with the configured release envelope.
    /// Clears all voices and held-key state after queue overflow or device loss.
    pub fn reset(&mut self) {
        self.voices.fill(Voice::SILENT);
        self.pressed.fill(false);
        self.voice_age.fill(0);
    }

    fn start_voice(&mut self, key_id: u16, note: u8) {
        let slot = self
            .voices
            .iter()
            .position(|voice| !voice.active)
            .or_else(|| {
                self.voice_age
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, age)| *age)
                    .map(|(index, _)| index)
            })
            .unwrap_or(0);
        self.age_counter = self.age_counter.wrapping_add(1);
        self.voice_age[slot] = self.age_counter;
        self.voices[slot] = Voice {
            active: true,
            key_id,
            phase_step: midi_frequency(note) / self.sample_rate,
            waveform: self.waveform,
            phase: 0.0,
            envelope: 0.0,
            attack_step: if self.attack_ms == 0 {
                1.0
            } else {
                1.0 / (self.sample_rate * self.attack_ms as f32 / 1000.0)
            },
            releasing: false,
            release_step: 0.0,
        };
    }

    fn release_key(&mut self, key_id: u16) {
        for voice in &mut self.voices {
            if voice.active && voice.key_id == key_id {
                release_voice(voice, self.release_ms, self.sample_rate);
            }
        }
    }

    fn release_voices(&mut self) {
        for voice in &mut self.voices {
            if voice.active {
                release_voice(voice, self.release_ms, self.sample_rate);
            }
        }
    }
}

fn release_voice(voice: &mut Voice, release_ms: u32, sample_rate: f32) {
    voice.releasing = true;
    voice.release_step = if release_ms == 0 || voice.envelope <= f32::EPSILON {
        voice.envelope.max(1.0)
    } else {
        voice.envelope / (sample_rate * release_ms as f32 / 1000.0)
    };
}

fn midi_frequency(note: u8) -> f32 {
    440.0 * 2.0f32.powf((note as f32 - 69.0) / 12.0)
}

fn poly_blep(phase: f32, step: f32) -> f32 {
    if step <= 0.0 {
        return 0.0;
    }
    if phase < step {
        let t = phase / step;
        t + t - t * t - 1.0
    } else if phase > 1.0 - step {
        let t = (phase - 1.0) / step;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

fn poly_blep_saw(phase: f32, step: f32) -> f32 {
    2.0 * phase - 1.0 - poly_blep(phase, step)
}

fn poly_blep_square(phase: f32, step: f32) -> f32 {
    let mut value = if phase < 0.5 { 1.0 } else { -1.0 };
    value += poly_blep(phase, step);
    let shifted = (phase + 0.5).fract();
    value -= poly_blep(shifted, step);
    value
}

fn triangle(phase: f32) -> f32 {
    // Analytic triangle with continuous phase and no per-sample state.
    let x = 2.0 * phase - 1.0;
    1.0 - 2.0 * x.abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn note_lifecycle_tracks_repeats_and_release() {
        let shared = Shared::new(&Settings::default());
        let mut synth = Synth::new(48_000, 7);
        let down = KeyEvent {
            key_id: 0x2c,
            down: true,
        };
        synth.handle_event(down, &shared);
        assert!(synth.is_active());
        let initial = synth.voices.iter().filter(|voice| voice.active).count();
        synth.handle_event(down, &shared);
        assert_eq!(
            synth.voices.iter().filter(|voice| voice.active).count(),
            initial
        );
        for _ in 0..100 {
            let _ = synth.next_sample();
        }
        synth.handle_event(
            KeyEvent {
                down: false,
                ..down
            },
            &shared,
        );
        for _ in 0..3_000 {
            let _ = synth.next_sample();
        }
        assert!(!synth.is_active());
    }

    #[test]
    fn disabled_tracks_key_state_without_starting_notes() {
        let mut settings = Settings {
            enabled: false,
            ..Settings::default()
        };
        let shared = Shared::new(&settings);
        let mut synth = Synth::new(48_000, 1);
        synth.sync_parameters(&shared);
        synth.handle_event(
            KeyEvent {
                key_id: 0x2c,
                down: true,
            },
            &shared,
        );
        assert!(!synth.is_active());
        synth.handle_event(
            KeyEvent {
                key_id: 0x2c,
                down: false,
            },
            &shared,
        );
        settings.enabled = true;
        shared.apply(&settings);
        synth.sync_parameters(&shared);
        synth.handle_event(
            KeyEvent {
                key_id: 0x2c,
                down: true,
            },
            &shared,
        );
        assert!(synth.is_active());
    }

    #[test]
    fn overflow_reset_clears_stuck_keys_and_voices() {
        let shared = Shared::new(&Settings::default());
        let mut synth = Synth::new(48_000, 5);
        synth.handle_event(
            KeyEvent {
                key_id: 0x2c,
                down: true,
            },
            &shared,
        );
        shared
            .overflow
            .store(true, std::sync::atomic::Ordering::Release);
        synth.reset();
        assert!(!synth.is_active());
        assert!(!synth.pressed[0x2c]);
    }

    #[test]
    fn oscillators_stay_in_audio_range() {
        for phase in (0..1000).map(|x| x as f32 / 1000.0) {
            assert!((-1.1..=1.1).contains(&poly_blep_saw(phase, 0.02)));
            assert!((-1.1..=1.1).contains(&poly_blep_square(phase, 0.02)));
            assert!((-1.0..=1.0).contains(&triangle(phase)));
        }
    }

    #[test]
    fn midi_pitch_reference_and_octaves_are_correct() {
        assert!((midi_frequency(69) - 440.0).abs() < 0.001);
        assert!((midi_frequency(60) - 261.6256).abs() < 0.01);
        assert!((midi_frequency(81) - 880.0).abs() < 0.002);
    }

    #[test]
    fn three_notes_sound_together_and_existing_notes_keep_their_pitch() {
        let shared = Shared::new(&Settings::default());
        let mut synth = Synth::new(48_000, 71);
        for key_id in [0x2c, 0x2d, 0x2e] {
            synth.handle_event(KeyEvent { key_id, down: true }, &shared);
        }
        assert_eq!(synth.voices.iter().filter(|voice| voice.active).count(), 3);
        let first = synth
            .voices
            .iter()
            .find(|voice| voice.active && voice.key_id == 0x2c)
            .unwrap();
        let first_step = first.phase_step;

        let transposed = Settings {
            transpose: 12,
            ..Settings::default()
        };
        shared.apply(&transposed);
        synth.sync_parameters(&shared);
        assert_eq!(
            synth
                .voices
                .iter()
                .find(|voice| voice.active && voice.key_id == 0x2c)
                .unwrap()
                .phase_step,
            first_step
        );
        synth.handle_event(
            KeyEvent {
                key_id: 0x2f,
                down: true,
            },
            &shared,
        );
        let new_voice = synth
            .voices
            .iter()
            .find(|voice| voice.active && voice.key_id == 0x2f)
            .unwrap();
        assert_eq!(
            new_voice.phase_step,
            midi_frequency(12 + 59) / synth.sample_rate
        );
    }

    #[test]
    fn release_during_attack_uses_full_configured_release_time() {
        let settings = Settings {
            attack_ms: 100,
            release_ms: 50,
            ..Settings::default()
        };
        let shared = Shared::new(&settings);
        let mut synth = Synth::new(48_000, 7);
        synth.sync_parameters(&shared);
        let event = KeyEvent {
            key_id: 0x2c,
            down: true,
        };
        synth.handle_event(event, &shared);
        for _ in 0..480 {
            let _ = synth.next_sample();
        }
        let voice = synth.voices.iter().find(|voice| voice.active).unwrap();
        assert!(voice.envelope > 0.0 && voice.envelope < 1.0);
        synth.handle_event(
            KeyEvent {
                down: false,
                ..event
            },
            &shared,
        );
        let mut samples = 0;
        while synth.is_active() && samples < 2_402 {
            let _ = synth.next_sample();
            samples += 1;
        }
        assert!((2_399..=2_401).contains(&samples));
    }
}
