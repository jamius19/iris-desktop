/*
* Author: Jamius Siam
* Since: 17/11/2026
*/

//! Mute or lower ("duck") other audio while listening, then put it back (the user's Handy fork, `audio.rs`
//! `MuteState`, `apply_mute`, `remove_mute`). [`DuckState`] snapshots the default output's mute state or volume before
//! changing it, so a restore puts back what was there: an output that was already muted stays muted. The output itself
//! is a [`SystemAudio`]; the Linux tools' output parsers live here so they're tested on every OS.

use crate::settings::{DUCK_AMOUNT_RANGE, Settings};

/// The default output device. Reads are None when they fail; writes fail silently.
pub trait SystemAudio {
    fn muted(&mut self) -> Option<bool>;
    fn set_muted(&mut self, muted: bool);
    /// 1.0 = 100%.
    fn volume(&mut self) -> Option<f32>;
    fn set_volume(&mut self, volume: f32);
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Duck {
    Mute,
    /// Lower by this percent.
    Lower(i32),
}

impl Duck {
    /// None when neither is on (or Lower is by 0%). Mute wins if a hand-edited file has both on.
    pub fn from_settings(settings: &Settings) -> Option<Duck> {
        if settings.mute_while_recording {
            Some(Duck::Mute)
        } else if settings.duck_while_recording && settings.duck_amount > 0 {
            Some(Duck::Lower(settings.duck_amount))
        } else {
            None
        }
    }
}

/// What [`DuckState::apply`] changed: `did_mute` while our mute is on, `prev_muted` the mute state before it,
/// `ducked_from` the volume before lowering it.
#[derive(Debug, Default)]
pub struct DuckState {
    did_mute: bool,
    prev_muted: Option<bool>,
    ducked_from: Option<f32>,
}

impl DuckState {
    pub fn is_active(&self) -> bool {
        self.did_mute || self.ducked_from.is_some()
    }

    /// A no-op while already active: a second snapshot would save our own change and strand the audio. Lowering is
    /// skipped when the volume can't be read, since it couldn't be put back.
    pub fn apply(&mut self, duck: Duck, audio: &mut dyn SystemAudio) {
        if self.is_active() {
            return;
        }
        match duck {
            Duck::Mute => {
                self.prev_muted = audio.muted();
                audio.set_muted(true);
                self.did_mute = true;
                log::debug!("Muted other audio (was muted: {:?})", self.prev_muted);
            }
            Duck::Lower(amount) => match audio.volume() {
                Some(volume) => {
                    let amount = amount.clamp(*DUCK_AMOUNT_RANGE.start(), *DUCK_AMOUNT_RANGE.end()) as f32 / 100.0;
                    audio.set_volume(volume * (1.0 - amount));
                    self.ducked_from = Some(volume);
                    log::debug!("Lowered other audio by {amount} from {volume}");
                }
                None => log::debug!("Not lowering other audio: the output volume couldn't be read"),
            },
        }
    }

    /// Undoes [`Self::apply`]: unmutes unless the output was already muted (an unknown prior state unmutes, so audio
    /// is never left muted by us), and puts the volume back. A no-op when nothing was applied.
    pub fn restore(&mut self, audio: &mut dyn SystemAudio) {
        if self.did_mute {
            if self.prev_muted != Some(true) {
                audio.set_muted(false);
            }
            self.did_mute = false;
            log::debug!("Restored other audio's mute state");
        }
        if let Some(volume) = self.ducked_from.take() {
            audio.set_volume(volume);
            log::debug!("Restored other audio's volume to {volume}");
        }
    }
}

/// The first `NN%` in pactl or amixer output, as a fraction (1.0 = 100%).
pub fn first_percent(output: &str) -> Option<f32> {
    output
        .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
        .find_map(|token| token.strip_suffix('%')?.parse::<f32>().ok())
        .map(|percent| percent / 100.0)
}

/// `wpctl get-volume`: "Volume: 0.40", plus " [MUTED]" when muted.
pub fn wpctl_volume(output: &str) -> Option<f32> {
    output.split_whitespace().nth(1)?.parse().ok()
}

pub fn wpctl_muted(output: &str) -> bool {
    output.contains("[MUTED]")
}

/// `pactl get-sink-mute`: "Mute: yes" or "Mute: no".
pub fn pactl_muted(output: &str) -> Option<bool> {
    let output = output.to_lowercase();
    if output.contains("yes") {
        Some(true)
    } else if output.contains("no") {
        Some(false)
    } else {
        None
    }
}

/// `amixer get Master`: "[off]" for a muted channel, "[on]" otherwise.
pub fn amixer_muted(output: &str) -> Option<bool> {
    if output.contains("[off]") {
        Some(true)
    } else if output.contains("[on]") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Values, to_settings};

    /// Records the output's state and every write.
    #[derive(Default)]
    struct FakeAudio {
        muted: Option<bool>,
        volume: Option<f32>,
        writes: Vec<String>,
    }

    impl SystemAudio for FakeAudio {
        fn muted(&mut self) -> Option<bool> {
            self.muted
        }

        fn set_muted(&mut self, muted: bool) {
            self.muted = Some(muted);
            self.writes.push(format!("mute {muted}"));
        }

        fn volume(&mut self) -> Option<f32> {
            self.volume
        }

        fn set_volume(&mut self, volume: f32) {
            self.volume = Some(volume);
            self.writes.push(format!("volume {volume}"));
        }
    }

    #[test]
    fn mute_snapshots_and_restores() {
        let mut audio = FakeAudio { muted: Some(false), ..Default::default() };
        let mut state = DuckState::default();
        state.apply(Duck::Mute, &mut audio);
        assert!(state.is_active());
        assert_eq!(Some(true), audio.muted);

        state.restore(&mut audio);
        assert!(!state.is_active());
        assert_eq!(vec!["mute true", "mute false"], audio.writes);
    }

    #[test]
    fn already_muted_stays_muted() {
        let mut audio = FakeAudio { muted: Some(true), ..Default::default() };
        let mut state = DuckState::default();
        state.apply(Duck::Mute, &mut audio);
        state.restore(&mut audio);
        assert_eq!(Some(true), audio.muted);
        assert_eq!(vec!["mute true"], audio.writes);
    }

    #[test]
    fn unknown_prior_state_is_unmuted() {
        let mut audio = FakeAudio::default();
        let mut state = DuckState::default();
        state.apply(Duck::Mute, &mut audio);
        state.restore(&mut audio);
        assert_eq!(Some(false), audio.muted);
    }

    #[test]
    fn second_apply_does_not_snapshot_again() {
        let mut audio = FakeAudio { muted: Some(false), volume: Some(0.8), ..Default::default() };
        let mut state = DuckState::default();
        state.apply(Duck::Mute, &mut audio);
        // Would save our own mute as the prior state.
        state.apply(Duck::Mute, &mut audio);
        state.apply(Duck::Lower(50), &mut audio);
        state.restore(&mut audio);
        assert_eq!(Some(false), audio.muted);
        assert_eq!(vec!["mute true", "mute false"], audio.writes);
    }

    #[test]
    fn restore_without_apply_is_a_no_op() {
        let mut audio = FakeAudio { muted: Some(true), volume: Some(0.5), ..Default::default() };
        DuckState::default().restore(&mut audio);
        assert!(audio.writes.is_empty());
    }

    #[test]
    fn lower_scales_the_volume_and_restores_it() {
        let mut audio = FakeAudio { volume: Some(0.8), ..Default::default() };
        let mut state = DuckState::default();
        state.apply(Duck::Lower(25), &mut audio);
        assert!(state.is_active());
        assert!((audio.volume.unwrap() - 0.6).abs() < 1e-6);

        state.restore(&mut audio);
        assert!(!state.is_active());
        assert_eq!(Some(0.8), audio.volume);
    }

    #[test]
    fn lower_amount_is_clamped() {
        let mut audio = FakeAudio { volume: Some(0.8), ..Default::default() };
        DuckState::default().apply(Duck::Lower(150), &mut audio);
        assert_eq!(Some(0.0), audio.volume);
    }

    #[test]
    fn unreadable_volume_skips_lowering() {
        let mut audio = FakeAudio::default();
        let mut state = DuckState::default();
        state.apply(Duck::Lower(50), &mut audio);
        assert!(!state.is_active());
        state.restore(&mut audio);
        assert!(audio.writes.is_empty());
    }

    #[test]
    fn duck_from_settings() {
        let mut settings = to_settings(&Values::new(), &Default::default()).unwrap();
        assert_eq!(None, Duck::from_settings(&settings));

        settings.duck_while_recording = true;
        assert_eq!(Some(Duck::Lower(50)), Duck::from_settings(&settings));
        settings.duck_amount = 0;
        assert_eq!(None, Duck::from_settings(&settings));

        settings.duck_amount = 30;
        settings.mute_while_recording = true;
        assert_eq!(Some(Duck::Mute), Duck::from_settings(&settings));
    }

    #[test]
    fn parses_wpctl() {
        assert_eq!(Some(0.4), wpctl_volume("Volume: 0.40\n"));
        assert_eq!(Some(0.4), wpctl_volume("Volume: 0.40 [MUTED]\n"));
        assert!(wpctl_muted("Volume: 0.40 [MUTED]\n"));
        assert!(!wpctl_muted("Volume: 0.40\n"));
    }

    #[test]
    fn parses_pactl() {
        let volume = "Volume: front-left: 26214 /  40% / -23.88 dB,   front-right: 26214 /  40% / -23.88 dB\n";
        assert_eq!(Some(0.4), first_percent(volume));
        assert_eq!(Some(true), pactl_muted("Mute: yes\n"));
        assert_eq!(Some(false), pactl_muted("Mute: no\n"));
    }

    #[test]
    fn parses_amixer() {
        let on = "Simple mixer control 'Master',0\n  Front Left: Playback 26214 [40%] [on]\n";
        assert_eq!(Some(0.4), first_percent(on));
        assert_eq!(Some(false), amixer_muted(on));
        assert_eq!(Some(true), amixer_muted("  Mono: Playback 0 [0%] [-65.25dB] [off]\n"));
    }

    #[test]
    fn garbage_is_unknown() {
        for garbage in ["", "Failed to connect to PulseAudio", "Volume: loud"] {
            assert_eq!(None, first_percent(garbage));
            assert_eq!(None, wpctl_volume(garbage));
            assert!(!wpctl_muted(garbage));
            assert_eq!(None, amixer_muted(garbage));
        }
        assert_eq!(None, pactl_muted(""));
        assert_eq!(None, pactl_muted("Volume: loud"));
    }
}
