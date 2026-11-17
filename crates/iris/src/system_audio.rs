/*
* Author: Jamius Siam
* Since: 17/11/2026
*/

//! The default output device for muting or lowering other audio (the user's Handy fork, `audio.rs`): on Windows its
//! `IAudioEndpointVolume`, looked up again on every call; on Linux the first of wpctl (PipeWire), pactl (PulseAudio)
//! and amixer (ALSA) that works, run with `LC_ALL=C` and a 2 s deadline. Failures are silent (debug log).

use iris_core::audio::ducking::SystemAudio;

/// Used from one thread: on Windows, [`Output::new`] joins that thread to COM's multithreaded apartment.
pub struct Output;

#[cfg(windows)]
mod imp {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eMultimedia, eRender};
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};

    use super::{Output, SystemAudio};

    impl Output {
        pub fn new() -> Self {
            // Already initialised (in either apartment) is fine: cpal tolerates the other mode on the same thread.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            Output
        }
    }

    /// The default render device's volume control, or None (logged).
    fn endpoint() -> Option<IAudioEndpointVolume> {
        let result = unsafe {
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .and_then(|devices| devices.GetDefaultAudioEndpoint(eRender, eMultimedia))
                .and_then(|device| device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None))
        };
        result.inspect_err(|e| log::debug!("No default output volume control: {e}")).ok()
    }

    impl SystemAudio for Output {
        fn muted(&mut self) -> Option<bool> {
            Some(unsafe { endpoint()?.GetMute() }.ok()?.as_bool())
        }

        fn set_muted(&mut self, muted: bool) {
            if let Some(endpoint) = endpoint()
                && let Err(e) = unsafe { endpoint.SetMute(muted, std::ptr::null()) }
            {
                log::debug!("Setting the output's mute failed: {e}");
            }
        }

        fn volume(&mut self) -> Option<f32> {
            unsafe { endpoint()?.GetMasterVolumeLevelScalar() }.ok()
        }

        fn set_volume(&mut self, volume: f32) {
            if let Some(endpoint) = endpoint()
                && let Err(e) = unsafe { endpoint.SetMasterVolumeLevelScalar(volume.clamp(0.0, 1.0), std::ptr::null()) }
            {
                log::debug!("Setting the output's volume failed: {e}");
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::process::Command;
    use std::time::Duration;

    use iris_core::audio::ducking::{amixer_muted, first_percent, pactl_muted, wpctl_muted, wpctl_volume};

    use super::{Output, SystemAudio};
    use crate::paste::run;

    const WPCTL_SINK: &str = "@DEFAULT_AUDIO_SINK@";
    const PACTL_SINK: &str = "@DEFAULT_SINK@";
    const TIMEOUT: Duration = Duration::from_secs(2);

    impl Output {
        pub fn new() -> Self {
            Output
        }
    }

    /// The tool's stdout when it ran and succeeded.
    fn tool(program: &str, args: &[&str]) -> Option<String> {
        let mut cmd = Command::new(program);
        cmd.args(args).env("LC_ALL", "C");
        match run(cmd, None, true, TIMEOUT) {
            Ok(out) if out.status.success() => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
            Ok(out) => {
                log::debug!("{program} {} failed: {}", args[0], out.status);
                None
            }
            Err(e) => {
                log::debug!("{e}");
                None
            }
        }
    }

    impl SystemAudio for Output {
        fn muted(&mut self) -> Option<bool> {
            tool("wpctl", &["get-volume", WPCTL_SINK])
                .map(|out| wpctl_muted(&out))
                .or_else(|| tool("pactl", &["get-sink-mute", PACTL_SINK]).and_then(|out| pactl_muted(&out)))
                .or_else(|| tool("amixer", &["get", "Master"]).and_then(|out| amixer_muted(&out)))
        }

        fn set_muted(&mut self, muted: bool) {
            let flag = if muted { "1" } else { "0" };
            let _ = tool("wpctl", &["set-mute", WPCTL_SINK, flag])
                .or_else(|| tool("pactl", &["set-sink-mute", PACTL_SINK, flag]))
                .or_else(|| tool("amixer", &["set", "Master", if muted { "mute" } else { "unmute" }]));
        }

        fn volume(&mut self) -> Option<f32> {
            tool("wpctl", &["get-volume", WPCTL_SINK])
                .and_then(|out| wpctl_volume(&out))
                .or_else(|| tool("pactl", &["get-sink-volume", PACTL_SINK]).and_then(|out| first_percent(&out)))
                .or_else(|| tool("amixer", &["get", "Master"]).and_then(|out| first_percent(&out)))
        }

        fn set_volume(&mut self, volume: f32) {
            let volume = volume.max(0.0);
            let percent = format!("{}%", (volume * 100.0).round() as u32);
            let _ = tool("wpctl", &["set-volume", WPCTL_SINK, &format!("{volume:.4}")])
                .or_else(|| tool("pactl", &["set-sink-volume", PACTL_SINK, &percent]))
                .or_else(|| tool("amixer", &["set", "Master", &percent]));
        }
    }
}
