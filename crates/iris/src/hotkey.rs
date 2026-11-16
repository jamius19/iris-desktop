/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Global shortcuts, from Handy `shortcut/handy_keys.rs`: one thread owns the handy-keys `HotkeyManager` (Windows: the
//! low-level hook, which blocks the bound keys; Linux: evdev, read-only, needs only `/dev/input` read access) and,
//! while the settings UI records a shortcut, a `KeyboardListener`. Key auto-repeat doesn't repeat `Pressed`: the
//! manager tracks held hotkeys.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use handy_keys::{Error, Hotkey, HotkeyId, HotkeyManager, HotkeyState, Key, KeyEvent, KeyboardListener};

pub enum HotkeyEvent {
    Transcribe { pressed: bool },
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutStatus {
    Registered,
    /// The binding is already taken (handy-keys only knows its own bindings, e.g. transcribe = cancel).
    InUse,
    /// The keyboard can't be read (Linux: no `/dev/input` access).
    NoAccess,
    Invalid,
}

type OnDone = Box<dyn FnOnce(Option<String>) + Send>;

enum Command {
    Transcribe(String, Sender<ShortcutStatus>),
    Cancel(Option<String>),
    Record(OnDone),
    StopRecording,
}

/// Handle to the hotkey thread, which stops when the last clone is dropped. Callbacks run on that thread: hand the
/// work on, and don't call [Hotkeys::register_transcribe] from them (it waits for the thread).
#[derive(Clone)]
pub struct Hotkeys {
    commands: Sender<Command>,
}

impl Hotkeys {
    pub fn start(on_event: impl Fn(HotkeyEvent) + Send + 'static) -> Self {
        let (commands, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("hotkeys".into())
            .spawn(move || run(receiver, on_event))
            .expect("couldn't start the hotkey thread");
        Self { commands }
    }

    /// Registers the transcribe shortcut (handy-keys syntax), replacing the previous one.
    pub fn register_transcribe(&self, binding: &str) -> ShortcutStatus {
        let (response, status) = mpsc::channel();
        let _ = self.commands.send(Command::Transcribe(binding.to_owned(), response));
        status.recv().unwrap_or(ShortcutStatus::NoAccess)
    }

    /// `Some(..)` at session start, `None` at the end.
    pub fn set_cancel(&self, binding: Option<&str>) {
        let _ = self.commands.send(Command::Cancel(binding.map(str::to_owned)));
    }

    /// Shortcut capture for the settings UI: calls `on_done` once with Some(binding in handy-keys syntax) or None (Esc
    /// / failure). The transcribe shortcut is suspended meanwhile.
    pub fn record(&self, on_done: impl FnOnce(Option<String>) + Send + 'static) {
        let _ = self.commands.send(Command::Record(Box::new(on_done)));
    }

    pub fn stop_recording(&self) {
        let _ = self.commands.send(Command::StopRecording);
    }
}

/// Installs `packaging/linux/70-iris-input.rules` (embedded, fed on stdin) through pkexec, reloads udev and re-triggers
/// only the devices the rule covers (keyboards and uinput), for installs without the .deb (AppImage). The ACLs apply
/// at once, so registering again then works.
#[cfg(target_os = "linux")]
pub fn install_udev_rule() -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    const RULE: &str = include_str!("../../../packaging/linux/70-iris-input.rules");
    let mut child = Command::new("pkexec")
        .args([
            "/bin/sh",
            "-c",
            "install -m644 /dev/stdin /etc/udev/rules.d/70-iris-input.rules && udevadm control --reload \
             && udevadm trigger --action=change --subsystem-match=input --property-match=ID_INPUT_KEYBOARD=1 \
             && udevadm trigger --action=change --subsystem-match=misc --sysname-match=uinput",
        ])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run pkexec: {e}"))?;
    // Dropping stdin closes it, so `install` sees the end of the rule.
    let written = child.stdin.take().map(|mut stdin| stdin.write_all(RULE.as_bytes()));
    let status = child.wait().map_err(|e| format!("Failed to wait for pkexec: {e}"))?;
    if let Some(Err(e)) = written {
        return Err(format!("Failed to write the udev rule: {e}"));
    }
    if !status.success() {
        return Err(format!("Installing the udev rule failed ({status})"));
    }
    Ok(())
}

fn run(commands: Receiver<Command>, on_event: impl Fn(HotkeyEvent)) {
    let mut state = State::default();
    loop {
        if let Some(manager) = &state.manager {
            while let Some(event) = manager.try_recv() {
                let pressed = event.state == HotkeyState::Pressed;
                if state.transcribe == Some(event.id) {
                    on_event(HotkeyEvent::Transcribe { pressed });
                } else if state.cancel == Some(event.id) && pressed {
                    on_event(HotkeyEvent::Cancel);
                }
            }
        }
        if let Some(recording) = &mut state.recording {
            let mut done = None;
            while let Some(event) = recording.listener.try_recv() {
                done = recording.capture.on_key(&event);
                if done.is_some() {
                    break;
                }
            }
            if let Some(binding) = done {
                state.finish_recording(binding);
            }
        }
        match commands.recv_timeout(Duration::from_millis(10)) {
            Ok(Command::Transcribe(binding, response)) => {
                let _ = response.send(state.register_transcribe(binding));
            }
            Ok(Command::Cancel(binding)) => state.set_cancel(binding),
            Ok(Command::Record(on_done)) => state.record(on_done),
            Ok(Command::StopRecording) => state.finish_recording(None),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    state.finish_recording(None);
}

struct Recording {
    listener: KeyboardListener,
    capture: Capture,
    on_done: OnDone,
}

#[derive(Default)]
struct State {
    /// Created on the first registration and retried on each one until it works (Linux: after the udev rule).
    manager: Option<HotkeyManager>,
    /// Kept to re-register after recording.
    transcribe_binding: String,
    transcribe: Option<HotkeyId>,
    cancel: Option<HotkeyId>,
    recording: Option<Recording>,
}

impl State {
    fn register(&mut self, binding: &str) -> Result<HotkeyId, ShortcutStatus> {
        let fail = |e: Error| {
            log::warn!("Couldn't register the shortcut {binding:?}: {e}");
            status(&e)
        };
        let hotkey: Hotkey = binding.parse().map_err(fail)?;
        if self.manager.is_none() {
            self.manager = Some(new_manager().map_err(fail)?);
        }
        self.manager.as_ref().expect("set above").register(hotkey).map_err(fail)
    }

    fn unregister(&self, id: Option<HotkeyId>) {
        if let (Some(manager), Some(id)) = (&self.manager, id) {
            let _ = manager.unregister(id);
        }
    }

    fn register_transcribe(&mut self, binding: String) -> ShortcutStatus {
        let previous = self.transcribe.take();
        self.unregister(previous);
        let result = self.register(&binding);
        self.transcribe_binding = binding;
        match result {
            Ok(id) => {
                self.transcribe = Some(id);
                ShortcutStatus::Registered
            }
            Err(status) => status,
        }
    }

    fn set_cancel(&mut self, binding: Option<String>) {
        let previous = self.cancel.take();
        self.unregister(previous);
        if let Some(binding) = binding {
            self.cancel = self.register(&binding).ok();
        }
    }

    fn record(&mut self, on_done: OnDone) {
        self.finish_recording(None);
        match KeyboardListener::new() {
            Ok(listener) => {
                // Suspended so the recorded keys can't start a dictation (Handy's suspend_all_shortcuts).
                let transcribe = self.transcribe.take();
                self.unregister(transcribe);
                self.recording = Some(Recording { listener, capture: Capture::default(), on_done });
            }
            Err(e) => {
                log::warn!("Couldn't record a shortcut: {e}");
                on_done(None);
            }
        }
    }

    fn finish_recording(&mut self, binding: Option<String>) {
        let Some(Recording { listener, on_done, .. }) = self.recording.take() else {
            return;
        };
        drop(listener);
        if self.transcribe.is_none() && !self.transcribe_binding.is_empty() {
            self.transcribe = self.register(&self.transcribe_binding.clone()).ok();
        }
        on_done(binding);
    }
}

/// Windows blocks the bound keys from other apps, as Handy; Linux only reads, since blocking would grab the keyboards
/// and needs `/dev/uinput` (PLAN M2.9).
fn new_manager() -> handy_keys::Result<HotkeyManager> {
    #[cfg(windows)]
    return HotkeyManager::new_with_blocking();
    #[cfg(not(windows))]
    HotkeyManager::new()
}

fn status(error: &Error) -> ShortcutStatus {
    match error {
        Error::HotkeyAlreadyRegistered(_) => ShortcutStatus::InUse,
        Error::EmptyHotkey | Error::InvalidHotkeyFormat(_) | Error::UnknownKey(_) | Error::UnknownModifier(_) => {
            ShortcutStatus::Invalid
        }
        _ => ShortcutStatus::NoAccess,
    }
}

/// Builds the recorded binding from key events, as Handy's `useShortcutCapture`: a combination with a key is taken when
/// a key is released (never bare modifiers then), a modifier-only one when every modifier is released. Esc cancels.
#[derive(Default)]
struct Capture {
    keyed: Option<String>,
    modifier_only: Option<String>,
}

impl Capture {
    /// `Some(result)` when the capture ends: `Some(binding)`, or `None` for Esc.
    fn on_key(&mut self, event: &KeyEvent) -> Option<Option<String>> {
        if event.is_key_down && event.key == Some(Key::Escape) {
            return Some(None);
        }
        let hotkey = event.as_hotkey().ok().map(|hotkey| hotkey.to_handy_string());
        if event.is_key_down {
            if event.key.is_some() {
                self.keyed = hotkey.or(self.keyed.take());
            } else {
                self.modifier_only = hotkey.or(self.modifier_only.take());
            }
        } else if event.key.is_some() {
            return self.keyed.take().or(hotkey).map(Some);
        } else if event.modifiers.is_empty() && self.keyed.is_none() {
            return self.modifier_only.take().map(Some);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use handy_keys::Modifiers;

    use super::*;

    fn key(modifiers: Modifiers, key: Option<Key>, is_key_down: bool) -> KeyEvent {
        KeyEvent { modifiers, key, is_key_down, changed_modifier: None }
    }

    #[test]
    fn records_keyed_combination_on_key_release() {
        let mut capture = Capture::default();
        assert_eq!(capture.on_key(&key(Modifiers::CTRL_LEFT, None, true)), None);
        assert_eq!(capture.on_key(&key(Modifiers::CTRL_LEFT, Some(Key::Space), true)), None);

        let done = capture.on_key(&key(Modifiers::CTRL_LEFT, Some(Key::Space), false));

        assert_eq!(done, Some(Some("ctrl_left+space".into())));
    }

    #[test]
    fn keyed_combination_survives_modifier_released_first() {
        let mut capture = Capture::default();
        capture.on_key(&key(Modifiers::CTRL_LEFT | Modifiers::OPT_LEFT, Some(Key::Space), true));
        assert_eq!(capture.on_key(&key(Modifiers::CTRL_LEFT, None, false)), None);
        assert_eq!(capture.on_key(&key(Modifiers::empty(), None, false)), None);

        let done = capture.on_key(&key(Modifiers::empty(), Some(Key::Space), false));

        assert_eq!(done, Some(Some("ctrl_left+alt_left+space".into())));
    }

    #[test]
    fn records_modifier_only_combination_when_all_modifiers_are_released() {
        let mut capture = Capture::default();
        capture.on_key(&key(Modifiers::CTRL_LEFT, None, true));
        capture.on_key(&key(Modifiers::CTRL_LEFT | Modifiers::SHIFT_LEFT, None, true));
        assert_eq!(capture.on_key(&key(Modifiers::CTRL_LEFT, None, false)), None);

        let done = capture.on_key(&key(Modifiers::empty(), None, false));

        assert_eq!(done, Some(Some("ctrl_left+shift_left".into())));
    }

    #[test]
    fn escape_cancels_recording() {
        let mut capture = Capture::default();
        capture.on_key(&key(Modifiers::CTRL_LEFT, None, true));

        assert_eq!(capture.on_key(&key(Modifiers::empty(), Some(Key::Escape), true)), Some(None));
    }

    #[test]
    fn maps_registration_errors_to_status() {
        assert_eq!(status(&"ctrl+nokey".parse::<Hotkey>().unwrap_err()), ShortcutStatus::Invalid);
        assert_eq!(status(&"".parse::<Hotkey>().unwrap_err()), ShortcutStatus::Invalid);
        assert_eq!(status(&Error::HotkeyAlreadyRegistered("ctrl+space".into())), ShortcutStatus::InUse);
        assert_eq!(status(&Error::Platform("permission denied".into())), ShortcutStatus::NoAccess);
    }

    #[test]
    fn default_shortcuts_parse() {
        for binding in ["ctrl+space", "ctrl+alt+space", "escape"] {
            assert!(binding.parse::<Hotkey>().is_ok(), "{binding}");
        }
    }
}
