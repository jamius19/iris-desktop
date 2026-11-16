/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Global shortcuts, from Handy `shortcut/handy_keys.rs`: one thread owns the shortcuts and, while the settings UI
//! records a shortcut, a capture. Key events come from one handy-keys `KeyboardListener` (Windows: the low-level hook,
//! which blocks the bound keys; Linux: evdev, read-only, needs only `/dev/input` read access), forwarded by its own
//! thread into the hotkey thread's channel, so that thread sleeps until a command or a key event arrives (handy-keys
//! `HotkeyManager` can only be polled next to the commands). While recording, its events go to the capture instead of
//! the shortcuts: unlike Handy, no second listener (on Windows, one created per recording got no key events in the
//! app). [`Matcher`] is the manager's matching. Key auto-repeat doesn't repeat `Pressed`: the matcher tracks held
//! hotkeys.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, PoisonError};
use std::thread;

use handy_keys::{BlockingHotkeys, Error, Hotkey, Key, KeyEvent, KeyboardListener};

pub enum HotkeyEvent {
    Transcribe { pressed: bool },
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutStatus {
    Registered,
    /// The binding is already taken (only Iris's own bindings are known, e.g. transcribe = cancel).
    InUse,
    /// The keyboard can't be read (Linux: no `/dev/input` access).
    NoAccess,
    Invalid,
}

type OnDone = Box<dyn FnOnce(Option<String>) + Send>;
type OnKeys = Box<dyn Fn(String) + Send>;
type OnStatus = Box<dyn FnOnce(ShortcutStatus) + Send>;

enum Message {
    Transcribe(String, OnStatus),
    Cancel(Option<String>),
    Record(OnKeys, OnDone),
    StopRecording,
    /// A key event from the listener.
    Key(KeyEvent),
    /// The last [Hotkeys] clone was dropped.
    Stop,
}

/// Handle to the hotkey thread, which stops when the last clone is dropped. Callbacks run on that thread: hand the
/// work on.
#[derive(Clone)]
pub struct Hotkeys {
    messages: Sender<Message>,
    _stop: Arc<StopOnDrop>,
}

struct StopOnDrop(Sender<Message>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(Message::Stop);
    }
}

impl Hotkeys {
    pub fn start(on_event: impl Fn(HotkeyEvent) + Send + 'static) -> Self {
        let (messages, receiver) = mpsc::channel();
        let forwarders = messages.clone();
        thread::Builder::new()
            .name("hotkeys".into())
            .spawn(move || run(receiver, forwarders, on_event))
            .expect("couldn't start the hotkey thread");
        Self { messages: messages.clone(), _stop: Arc::new(StopOnDrop(messages)) }
    }

    /// Registers the transcribe shortcut (handy-keys syntax), replacing the previous one, on the hotkey thread (opening
    /// the keyboard can take a while); `on_status` then gets the result there. Requests are handled in order.
    pub fn register_transcribe(&self, binding: &str, on_status: impl FnOnce(ShortcutStatus) + Send + 'static) {
        let _ = self.messages.send(Message::Transcribe(binding.to_owned(), Box::new(on_status)));
    }

    /// `Some(..)` at session start, `None` at the end.
    pub fn set_cancel(&self, binding: Option<&str>) {
        let _ = self.messages.send(Message::Cancel(binding.map(str::to_owned)));
    }

    /// Shortcut capture for the settings UI: calls `on_keys` with the keys held so far whenever they change (handy-keys
    /// syntax), then `on_done` once with Some(binding) or None (Esc / failure). The transcribe shortcut is suspended
    /// meanwhile.
    pub fn record(
        &self,
        on_keys: impl Fn(String) + Send + 'static,
        on_done: impl FnOnce(Option<String>) + Send + 'static,
    ) {
        let _ = self.messages.send(Message::Record(Box::new(on_keys), Box::new(on_done)));
    }

    pub fn stop_recording(&self) {
        let _ = self.messages.send(Message::StopRecording);
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

/// Blocks on the channel: commands from [Hotkeys] and key events from the listener's forwarding thread.
fn run(messages: Receiver<Message>, forwarders: Sender<Message>, on_event: impl Fn(HotkeyEvent)) {
    let mut state = State::new(forwarders);
    while let Ok(message) = messages.recv() {
        match message {
            Message::Transcribe(binding, on_status) => on_status(state.register_transcribe(binding)),
            Message::Cancel(binding) => state.set_cancel(binding),
            Message::Record(on_keys, on_done) => state.record(on_keys, on_done),
            Message::StopRecording => {
                log::debug!("Shortcut recording: stop requested");
                state.finish_recording(None);
            }
            Message::Key(event) => match &mut state.recording {
                Some(recording) => {
                    log::debug!(
                        "Shortcut recording: {:?} {:?} {}",
                        event.modifiers,
                        event.key,
                        if event.is_key_down { "down" } else { "up" }
                    );
                    let shown = recording.capture.partial.clone();
                    let done = recording.capture.on_key(&event);
                    if done.is_none()
                        && recording.capture.partial != shown
                        && let Some(keys) = &recording.capture.partial
                    {
                        (recording.on_keys)(keys.clone());
                    }
                    if let Some(binding) = done {
                        state.finish_recording(binding);
                    }
                }
                None => {
                    for (slot, pressed) in state.matcher.process(&event) {
                        match slot {
                            Slot::Transcribe => on_event(HotkeyEvent::Transcribe { pressed }),
                            Slot::Cancel if pressed => on_event(HotkeyEvent::Cancel),
                            Slot::Cancel => {}
                        }
                    }
                }
            },
            Message::Stop => break,
        }
    }
    state.finish_recording(None);
    // The shortcut listener's thread ends at its next event; until then its hook blocks nothing.
    state.blocked().clear();
}

/// Forwards the listener's events until the hotkey thread is gone (noticed at the next event).
fn forward_keys(listener: KeyboardListener, messages: Sender<Message>) {
    while let Ok(event) = listener.recv() {
        if messages.send(Message::Key(event)).is_err() {
            break;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Slot {
    Transcribe,
    Cancel,
}

/// handy-keys `ManagerState::process_event` (MIT) for Iris's two shortcuts: a hotkey is pressed when its modifiers
/// match and its key goes down, and released when its key goes up or, on a modifier event, its modifiers stop matching.
#[derive(Default)]
struct Matcher {
    hotkeys: HashMap<Slot, Hotkey>,
    pressed: HashSet<Slot>,
}

impl Matcher {
    /// The (slot, pressed) changes `event` causes.
    fn process(&mut self, event: &KeyEvent) -> Vec<(Slot, bool)> {
        let mut changes = Vec::new();
        for (&slot, hotkey) in &self.hotkeys {
            if event.is_key_down {
                if hotkey.modifiers.matches(event.modifiers) && hotkey.key == event.key && self.pressed.insert(slot) {
                    changes.push((slot, true));
                }
            } else if self.pressed.contains(&slot)
                && ((event.key.is_some() && hotkey.key == event.key)
                    || (event.key.is_none() && !hotkey.modifiers.matches(event.modifiers)))
            {
                self.pressed.remove(&slot);
                changes.push((slot, false));
            }
        }
        changes
    }
}

struct Recording {
    capture: Capture,
    on_keys: OnKeys,
    on_done: OnDone,
}

struct State {
    /// For the forwarding thread.
    messages: Sender<Message>,
    /// The listener is running: started on the first registration or recording and retried on each one until it works
    /// (Linux: after the udev rule).
    listening: bool,
    /// The registered hotkeys, which the Windows listener blocks from other apps.
    blocking: BlockingHotkeys,
    matcher: Matcher,
    /// Kept to re-register after recording.
    transcribe_binding: String,
    recording: Option<Recording>,
}

impl State {
    fn new(messages: Sender<Message>) -> Self {
        Self {
            messages,
            listening: false,
            blocking: BlockingHotkeys::default(),
            matcher: Matcher::default(),
            transcribe_binding: String::new(),
            recording: None,
        }
    }

    fn blocked(&self) -> std::sync::MutexGuard<'_, HashSet<Hotkey>> {
        self.blocking.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn listen(&mut self) -> handy_keys::Result<()> {
        if !self.listening {
            let listener = new_listener(&self.blocking)?;
            let messages = self.messages.clone();
            thread::Builder::new()
                .name("hotkeys-listener".into())
                .spawn(move || forward_keys(listener, messages))
                .expect("couldn't start the hotkey listener thread");
            self.listening = true;
        }
        Ok(())
    }

    fn register(&mut self, slot: Slot, binding: &str) -> Result<(), ShortcutStatus> {
        let fail = |e: Error| {
            log::warn!("Couldn't register the shortcut {binding:?}: {e}");
            status(&e)
        };
        let hotkey: Hotkey = binding.parse().map_err(fail)?;
        self.listen().map_err(fail)?;
        if self.matcher.hotkeys.values().any(|registered| *registered == hotkey) {
            return Err(fail(Error::HotkeyAlreadyRegistered(hotkey.to_string())));
        }
        self.matcher.hotkeys.insert(slot, hotkey);
        self.blocked().insert(hotkey);
        Ok(())
    }

    fn unregister(&mut self, slot: Slot) {
        self.matcher.pressed.remove(&slot);
        if let Some(hotkey) = self.matcher.hotkeys.remove(&slot) {
            self.blocked().remove(&hotkey);
        }
    }

    fn register_transcribe(&mut self, binding: String) -> ShortcutStatus {
        self.unregister(Slot::Transcribe);
        let result = self.register(Slot::Transcribe, &binding);
        self.transcribe_binding = binding;
        match result {
            Ok(()) => ShortcutStatus::Registered,
            Err(status) => status,
        }
    }

    fn set_cancel(&mut self, binding: Option<String>) {
        self.unregister(Slot::Cancel);
        if let Some(binding) = binding {
            let _ = self.register(Slot::Cancel, &binding);
        }
    }

    fn record(&mut self, on_keys: OnKeys, on_done: OnDone) {
        self.finish_recording(None);
        if let Err(e) = self.listen() {
            log::warn!("Couldn't record a shortcut: {e}");
            on_done(None);
            return;
        }
        // Suspended so the recorded keys can't start a dictation (Handy's suspend_all_shortcuts); the cancel shortcut
        // gets no events meanwhile.
        self.unregister(Slot::Transcribe);
        self.recording = Some(Recording { capture: Capture::default(), on_keys, on_done });
        log::debug!("Shortcut recording started");
    }

    fn finish_recording(&mut self, binding: Option<String>) {
        let Some(Recording { on_done, .. }) = self.recording.take() else {
            return;
        };
        log::debug!("Shortcut recording ended: {binding:?}");
        if !self.matcher.hotkeys.contains_key(&Slot::Transcribe) && !self.transcribe_binding.is_empty() {
            let _ = self.register(Slot::Transcribe, &self.transcribe_binding.clone());
        }
        on_done(binding);
    }
}

/// Windows blocks the bound keys from other apps, as Handy; Linux only reads, since blocking would grab the keyboards
/// and needs `/dev/uinput`.
fn new_listener(blocking: &BlockingHotkeys) -> handy_keys::Result<KeyboardListener> {
    #[cfg(windows)]
    return KeyboardListener::new_with_blocking(blocking.clone());
    #[cfg(not(windows))]
    {
        let _ = blocking;
        KeyboardListener::new()
    }
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
    /// The keys held at the last key down, shown while recording (Handy's `partial`).
    partial: Option<String>,
}

impl Capture {
    /// `Some(result)` when the capture ends: `Some(binding)`, or `None` for Esc.
    fn on_key(&mut self, event: &KeyEvent) -> Option<Option<String>> {
        if event.is_key_down && event.key == Some(Key::Escape) {
            return Some(None);
        }
        let hotkey = event.as_hotkey().ok().map(|hotkey| hotkey.to_handy_string());
        if event.is_key_down {
            self.partial = hotkey.clone().or(self.partial.take());
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
    use std::time::Duration;

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
    fn partial_follows_the_keys_pressed() {
        let mut capture = Capture::default();
        capture.on_key(&key(Modifiers::CTRL_LEFT, None, true));
        assert_eq!(capture.partial.as_deref(), Some("ctrl_left"));
        capture.on_key(&key(Modifiers::CTRL_LEFT | Modifiers::SHIFT_LEFT, None, true));
        assert_eq!(capture.partial.as_deref(), Some("ctrl_left+shift_left"));
        capture.on_key(&key(Modifiers::CTRL_LEFT | Modifiers::SHIFT_LEFT, Some(Key::K), true));
        assert_eq!(capture.partial.as_deref(), Some("ctrl_left+shift_left+k"));
        capture.on_key(&key(Modifiers::CTRL_LEFT, None, false));
        assert_eq!(capture.partial.as_deref(), Some("ctrl_left+shift_left+k"));
    }

    /// The hotkey thread end to end, with key events as the recording listener forwards them.
    #[test]
    fn recording_reports_keys_then_the_binding() {
        let hotkeys = Hotkeys::start(|_| {});
        let (sender, results) = mpsc::channel();
        let done = sender.clone();
        hotkeys.record(
            move |keys| sender.send(format!("keys {keys}")).unwrap(),
            move |binding| done.send(format!("done {binding:?}")).unwrap(),
        );
        for event in [
            key(Modifiers::CTRL_LEFT, None, true),
            key(Modifiers::CTRL_LEFT | Modifiers::SHIFT_LEFT, None, true),
            key(Modifiers::CTRL_LEFT | Modifiers::SHIFT_LEFT, Some(Key::K), true),
            key(Modifiers::CTRL_LEFT | Modifiers::SHIFT_LEFT, Some(Key::K), false),
        ] {
            hotkeys.messages.send(Message::Key(event)).unwrap();
        }
        let got: Vec<String> = (0..4).map(|_| results.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert_eq!(
            got,
            [
                "keys ctrl_left",
                "keys ctrl_left+shift_left",
                "keys ctrl_left+shift_left+k",
                "done Some(\"ctrl_left+shift_left+k\")"
            ]
        );
    }

    #[cfg(windows)]
    fn tap_f24() {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn keybd_event(vk: u8, scan: u8, flags: u32, extra: usize);
        }
        // SAFETY: plain key injection; F24 types nothing.
        unsafe {
            keybd_event(0x87, 0, 0, 0);
            keybd_event(0x87, 0, 2, 0);
        }
    }

    /// The real hook: a plain listener sees an injected F24.
    #[cfg(windows)]
    #[test]
    #[ignore = "injects a real F24 key press"]
    fn real_listener_sees_injected_key() {
        let listener = KeyboardListener::new().unwrap();
        thread::sleep(Duration::from_millis(300));
        tap_f24();
        let event = listener.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(event.key, Some(Key::F24));
    }

    /// The real hook through the app's path: shortcut registered, then a recording.
    #[cfg(windows)]
    #[test]
    #[ignore = "injects a real F24 key press"]
    fn real_recording_sees_injected_key() {
        let hotkeys = Hotkeys::start(|_| {});
        let (status_tx, status) = mpsc::channel();
        hotkeys.register_transcribe("ctrl+space", move |s| status_tx.send(s).unwrap());
        assert_eq!(status.recv_timeout(Duration::from_secs(5)).unwrap(), ShortcutStatus::Registered);
        let (sender, results) = mpsc::channel();
        hotkeys.record(move |keys| sender.send(keys).unwrap(), |_| {});
        thread::sleep(Duration::from_millis(300));
        tap_f24();
        assert_eq!(results.recv_timeout(Duration::from_secs(2)).unwrap(), "f24");
        hotkeys.stop_recording();
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

    /// handy-keys `hotkey_press_generates_event`, `no_duplicate_press_events`, `hotkey_release_generates_event`.
    #[test]
    fn matcher_presses_once_and_releases_on_key_up() {
        let mut matcher = Matcher::default();
        matcher.hotkeys.insert(Slot::Transcribe, "ctrl+space".parse().unwrap());

        let down = key(Modifiers::CTRL_LEFT, Some(Key::Space), true);
        assert_eq!(matcher.process(&down), vec![(Slot::Transcribe, true)]);
        assert_eq!(matcher.process(&down), vec![]);
        assert_eq!(
            matcher.process(&key(Modifiers::CTRL_LEFT, Some(Key::Space), false)),
            vec![(Slot::Transcribe, false)]
        );
    }

    /// handy-keys `modifier_release_triggers_hotkey_release` and
    /// `modifier_only_hotkey_not_released_by_unrelated_modifier`.
    #[test]
    fn matcher_releases_when_modifiers_stop_matching() {
        let mut matcher = Matcher::default();
        matcher.hotkeys.insert(Slot::Cancel, "ctrl".parse().unwrap());

        assert_eq!(matcher.process(&key(Modifiers::CTRL_LEFT, None, true)), vec![(Slot::Cancel, true)]);
        assert_eq!(matcher.process(&key(Modifiers::CTRL_LEFT | Modifiers::SHIFT_LEFT, None, true)), vec![]);
        assert_eq!(matcher.process(&key(Modifiers::CTRL_LEFT, None, false)), vec![]);
        assert_eq!(matcher.process(&key(Modifiers::empty(), None, false)), vec![(Slot::Cancel, false)]);
    }

    #[test]
    fn default_shortcuts_parse() {
        for binding in ["ctrl+space", "ctrl+alt+space", "escape"] {
            assert!(binding.parse::<Hotkey>().is_ok(), "{binding}");
        }
    }
}
