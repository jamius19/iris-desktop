# Architecture

How Iris desktop is put together. Sources it ports: the Android app Iris (Kotlin, `dev.jamius.iris`) and Handy
(`github.com/cjpais/Handy`, MIT). Decisions and their reasons are in `decisions.md`.

## Workspace
```
crates/
  iris-core/            UI-free logic and its tests: settings, keychain, backup, statistics, update check, cloud
                        speech-to-text and cleanup providers, on-device model table/downloads/provider, dictation
                        controller and Handy's coordinator, audio gate / time-stretch / visualizer, copy and UI math
  iris/                 the `iris` binary: Slint UI, app wiring, capture, hotkeys, CLI, single instance, paste, local
                        engine (transcribe-cpp in a worker process), VAD, overlay host, tray, autostart, sounds
  iris-overlay/         shared overlay: `ui/overlay.slint`, `OverlayState`, geometry, helper IPC frames
  iris-overlay-helper/  Linux-only `iris-overlay` binary: layer-shell mode, or an X11 override-redirect window
xtask/                  dev-only asset generation (icons, tray icons, sounds, licence text)
assets/  vendor/sonic/  packaging/  scripts/  .github/workflows/release.yml
```
`iris-core` has no Slint and no transcribe-cpp, so `cargo test -p iris-core` (nearly all ported tests) builds fast and
without CMake/Vulkan. `iris-overlay` lets the Windows app and the Linux helper compile the same `overlay.slint`.

## Threads, processes and event loops
| Context | Owns | Talks to others through |
|---|---|---|
| Main thread | Slint winit event loop: main window, Windows overlay window, tray | `slint::invoke_from_event_loop` (inbound) |
| Tokio runtime (2 workers) | HTTP/WebSocket providers, cleanup, downloads, update check, model lists, favicons, keychain calls (`spawn_blocking`), single-instance server, dictation sessions | `tokio::sync::{mpsc, watch}` |
| Capture thread (handy-recorder) | Microphone stream, 50 ms PCM chunks | channel to the session |
| Engine worker **process** (`iris --transcribe-worker`) | transcribe-cpp model and inference | Handy's framed stdin/stdout protocol (`EngineSupervisor`) |
| Hotkey thread | handy-keys listeners (Windows hook, Linux evdev) | coordinator inputs; registration status via `invoke_from_event_loop` |
| Signal thread (Linux) | SIGUSR2 → toggle | coordinator |
| Paste | enigo, clipboard, external tools (`spawn_blocking`) | result back to the session |
| Overlay helper **process** (Linux) | layer-shell or X11 overlay window | framed JSON over its stdin/stdout |
| Idle-unload thread | unloads the model after the Unload Model delay | `EngineSupervisor::unload` |
| Sounds thread | rodio playback of the start/stop tones; muting or lowering other audio and restoring it, in order | channel |
| Tray watcher thread (Linux) | `NameOwnerChanged` of `org.kde.StatusNotifierWatcher` | recreates the tray on the main thread |

## State flow
1. Inputs (hotkey press/release, `iris --toggle-transcription` / `--cancel` over the single-instance socket, SIGUSR2,
   the overlay ✕, tray Cancel) go to the **coordinator** (Handy's hold-or-toggle machine), which starts and stops the
   **DictationController** (Android's `Idle → Listening → Finalizing → Done | Error → Idle`, hold times 1.5 s / 3 s).
2. A session: capture → VAD gate → Audio Speed → provider → filler words → custom words → cleanup → paste →
   statistics. State goes out on a `watch` channel; audio levels on a separate one.
3. Subscribers (`crates/iris/src/dictation.rs`): overlay host, cancel shortcut, idle unload, tray icon, sounds and
   other audio (mute or lower).
4. Settings changes: Slint callback → `Store::set_*` (validated, atomic write) → `refresh` → `watch<Settings>` for the
   worker threads, hotkey re-registration, overlay config, autostart.

## IPC
**Single instance** (`interprocess` local sockets): Windows named pipe `iris-desktop-<USERNAME>-<session id>.sock` (the
client checks the pipe server runs as the same user before sending); Linux `$XDG_RUNTIME_DIR/iris-desktop.sock`, else
the abstract `iris-desktop-<uid>.sock` (other uids are dropped). A later instance sends one JSON line (`toggle`,
`cancel`, `show`) and exits; with no instance answering it becomes the app (an action flag is then ignored, as Handy).

**Overlay helper** (Linux): spawned on first use from next to `iris`; frames of u32 length + JSON. App → helper:
`Config`, `State`, `Levels`, `Hide`; helper → app: `Ready { mode }`, `CancelClicked`, `NoticeClicked`, `Failed`.
The helper exits on stdin EOF; it is respawned at most once per session. Mode: layer-shell if the compositor offers
`zwlr_layer_shell_v1`, else X11 (XWayland on GNOME, or an X11 session).

**Engine worker**: Handy's `engine_supervisor` protocol, unchanged (`crates/iris/src/local/supervisor`).

## Overlay
`iris-overlay/ui/overlay.slint` is drawn by all three hosts: Windows (in-process winit window, femtovg; ex-styles
`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` re-applied on every style change, `WM_MOUSEACTIVATE → MA_NOACTIVATE`), Linux
layer-shell (Slint software renderer into a `wl_shm` buffer on an SCTK layer surface) and Linux X11 (Slint winit,
override-redirect). Geometry is pure Rust (`iris-overlay::geometry`, Handy's tests). Sizing (Handy's): a 180 px
compact pill (dot, waveform, ✕) until a session's first live text, then a card fixed at `wide_width` (the Width
setting's share of the monitor width, 10–30 %, at least 240 px) until the next session; the panel grows in height
up to the Lines setting. The window is sized for the wide card once per show and never resizes during a session; the
card animates inside it and the window region / input shape follows the card. Success fades the card out as it is,
like an error leaving; an error or warning replaces the control row's content under the live text
(`docs/decisions.md`).

## Storage
| What | Where (`ProjectDirs::from("dev", "jamius", "Iris")`) | Format |
|---|---|---|
| Settings | Windows `%APPDATA%\jamius\Iris\config\settings.json`; Linux `~/.config/iris/settings.json` | `{"version":1,"settings":{name:{"type","value"}}}` = Android's backup `settings` object; unknown keys kept |
| Statistics | `<config>/statistics.json` | `{"models","cloud","cleanup","totals"}`, Android's field names. Not in backups. |
| API keys | OS keychain, service `dev.jamius.iris`, account = Android pref name | plain secret, never logged |
| Models | Windows `%LOCALAPPDATA%\jamius\Iris\data\models\<id>\`; Linux `~/.local/share/iris/models/<id>/` | one `.gguf` (`.tmp` while downloading) |
| VAD model | `<data>/vad/silero-vad-v6.2-F32.gguf`, written from the binary when missing | GGUF |
| Favicons | `<cache>/favicons/<domain>.png` | PNG |
| Logs | `<data>/logs/iris.log` (truncated per run) | text; never keys or transcripts |
| Startup entry | Windows HKCU `...\CurrentVersion\Run` value `Iris`; Linux `~/.config/autostart/Iris.desktop` | `"<exe or $APPIMAGE>" --start-hidden` |

### Settings keys
Android's keys are kept verbatim with the same defaults and validation (`crates/iris-core/src/settings/model.rs`).
Android-only `bubble_side` / `bubble_y_fraction` are kept and unused. Desktop-only keys (Android keeps them as unknown
keys): `overlay_position`, `overlay_width_percent`, `microphone_device`, `local_accelerator`, `shortcut_transcribe`,
`shortcut_cancel`, `shortcut_activation`, `paste_method`, `append_trailing_space`, `restore_clipboard`,
`paste_delay_ms`, `paste_delay_after_ms`, `feedback_sounds`, `feedback_volume_percent`, `mute_while_recording`,
`duck_while_recording`, `duck_amount`, `launch_at_startup`.

### Backup file (`.irisbackup`, identical to Android's)
`IRISBKUP` (8) · format version 1 (1) · PBKDF2 iterations, u32 big-endian (4) · salt (16) · nonce (12) — this 41-byte
header is the AES-GCM AAD — then AES-256-GCM ciphertext and tag. Key: PBKDF2-HMAC-SHA256 of the UTF-8 password,
600,000 iterations. Payload: `{"version":1,"settings":{…},"secrets":{name:value}}`. Restore decrypts and validates
everything first, then clears every known key (five fixed names + `custom_cleanup_key_<id>` of the current custom
providers), replaces all settings, then writes the backup's keys (Android's order).

## On-device models
| Iris id | GGUF repo (Hugging Face, pinned revision + SHA-256 in `models.rs`) | Streaming |
|---|---|---|
| `nemotron-streaming-en` (default) | `handy-computer/nemotron-speech-streaming-en-0.6b-gguf`, Q8_0 | yes |
| `parakeet-unified-en` | `handy-computer/parakeet-unified-en-0.6b-gguf`, Q8_0 | yes |
| `parakeet-tdt-v3` | `handy-computer/parakeet-tdt-0.6b-v3-gguf`, Q8_0 | no (25 languages) |
| `whisper-small` | `handy-computer/whisper-small-gguf`, Q8_0 | no (99 languages) |

Android's Moonshine is not offered; a stored `moonshine-base-en` reads as the default.
