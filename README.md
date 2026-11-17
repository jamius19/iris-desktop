# Iris

Voice dictation for Windows 10+ and Linux. Press a shortcut, speak, and Iris types what you said into the app you're
using. It is the desktop version of the Iris Android app, with the same settings, providers and backup format.

- **Transcription** on this computer (offline, four models: Nemotron Streaming English, Parakeet Unified English,
  Parakeet TDT v3, Whisper Small) or in the cloud (OpenAI, Vercel AI Gateway, Deepgram, Soniox) with your own API key.
- **Post processing**: filler-word removal, Custom Words, and optional LLM cleanup (Vercel AI Gateway, OpenRouter or
  any OpenAI-compatible server) with your own prompts.
- A small overlay while you dictate (live text, cancel with ✕ or Esc), a tray icon, start/stop sounds, muting or
  lowering other audio while you dictate, launch at startup, statistics, and encrypted backups that move between Iris
  on Android and desktop.

API keys are kept in the system keychain (Windows Credential Manager, the Secret Service on Linux), never in a file.

**Status:** dictation, the settings and the backup format were tested on Windows. Not yet tested on real systems:
Linux desktops (X11 and Wayland), the installers and packages, the cloud providers with real API keys, and restoring
on a phone. `docs/manual-checks.md` lists what is still to check.

## Install

Download the latest release from [Releases](https://github.com/jamius19/iris-desktop/releases/latest):

| System | File | Notes |
|---|---|---|
| Windows 10/11 (x64) | `iris_<version>_x64-setup.exe` | Installs for your user, no admin rights needed. The installer isn't signed yet, so SmartScreen may warn: choose **More info → Run anyway**. |
| Debian, Ubuntu (x64) | `iris_<version>-1_amd64.deb` | `sudo apt install ./iris_<version>-1_amd64.deb` — also installs the keyboard access rule (see [Linux](#linux)). |
| Other Linux (x64) | `iris_<version>_x86_64.AppImage` | `chmod +x` it and run it. The keyboard access rule is installed from inside the app (Home → Global Shortcut → Setup). |

On-device models are downloaded from the Transcription page (about 270–740 MB each). They run on the GPU through
Vulkan when it works, else on the CPU.

## Use

1. Open Iris and pick a transcription provider (Home → Transcription Settings). For on-device, download a model.
2. Press the shortcut — **Ctrl+Space** on Windows, **Ctrl+Alt+Space** on Linux — and speak. Hold it and let go to
   stop, or tap it to start and tap again to stop (Dictation Settings → Activation). **Esc** cancels.
3. The text is pasted into the focused app. If pasting fails, it stays on the clipboard.

Closing the window keeps Iris running in the tray; use the tray's **Quit** to exit.

### Command line
```
iris --toggle-transcription   # start/stop dictation in the running Iris (bind this to a key in your desktop)
iris --cancel                 # cancel the dictation
iris --start-hidden           # start in the tray
iris --no-tray                # no tray icon; closing the window quits
iris --debug                  # verbose log (also in the log file)
iris -f speech.wav --model whisper-small   # transcribe a 16 kHz mono WAV with a downloaded model and print it
```
On Linux, `pkill -USR2 -n iris` also toggles dictation.

Logs: Windows `%LOCALAPPDATA%\jamius\Iris\data\logs\iris.log`, Linux `~/.local/share/iris/logs/iris.log`. They never
contain your API keys or what you dictated.

## Linux

Iris is built for X11 and Wayland (KDE, Sway, Hyprland, GNOME), but hasn't been tested on a real Linux desktop yet.
Wayland needs a little setup for the global shortcut and for typing into other apps; Home shows what's missing.

### Global shortcut
Iris reads the keyboard directly (evdev), which needs read access to keyboard devices. The .deb installs a udev rule
for this; with the AppImage, use Home → Global Shortcut → **Setup**, which installs the same rule with `pkexec`. The
rule (`/etc/udev/rules.d/70-iris-input.rules` or `/usr/lib/udev/rules.d/70-iris-input.rules`) gives the user at the
seat access to keyboards only (not mice or touchpads) and to `/dev/uinput`:
```
SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_INPUT_KEYBOARD}=="1", TAG+="uaccess"
SUBSYSTEM=="misc", KERNEL=="uinput", OPTIONS+="static_node=uinput", TAG+="uaccess"
```
To install it by hand:
```
sudo install -m644 70-iris-input.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger --action=change --subsystem-match=input --property-match=ID_INPUT_KEYBOARD=1
sudo udevadm trigger --action=change --subsystem-match=misc --sysname-match=uinput
```
Note that any app you run can then read the keyboard. If you'd rather not, skip the rule and bind
`iris --toggle-transcription` to a key in your desktop's keyboard settings instead (GNOME: Settings → Keyboard →
Custom Shortcuts; KDE: System Settings → Shortcuts → Custom Shortcuts; Sway/Hyprland: a `bindsym` / `bind` line).
Hold-to-talk and the Esc cancel shortcut need the rule; with a binding, press once to start and once to stop.

### Typing into other apps (Wayland)
Iris puts the text on the clipboard and presses the paste keys. On Wayland that needs:
- **wl-clipboard** (`wl-copy`, `wl-paste`): `sudo apt install wl-clipboard` / `sudo dnf install wl-clipboard`.
- **ydotool** with its daemon **ydotoold** running as you: `sudo apt install ydotool`, then start `ydotoold` (for
  example `systemctl --user enable --now ydotool` where the package ships a user service, or run `ydotoold &` from
  your session's autostart). ydotoold needs `/dev/uinput`, which the udev rule above allows.

Without them the text stays on the clipboard and Iris says so. Paste Method "Direct typing" uses `ydotool type`,
which can mangle non-English text; Ctrl+V (the default) is more reliable. On X11 nothing extra is needed
(`xdotool` is used when installed).

### Overlay and tray
- The overlay uses the layer-shell protocol (KDE, Sway, Hyprland and most wlroots compositors). GNOME has no
  layer-shell, so the overlay runs through XWayland there and shows on the primary monitor.
- The tray uses StatusNotifierItem. GNOME needs the **AppIndicator and KStatusNotifierItem Support** extension;
  without a tray Iris keeps its window open instead of starting hidden.
- API keys need a Secret Service (GNOME Keyring or KWallet). On a bare window manager, start `gnome-keyring-daemon`
  before Iris.
- Mute Other Audio and Lower Other Audio need `wpctl` (PipeWire), `pactl` (PulseAudio) or `amixer` (ALSA); without
  them other audio is left alone.

## Backup and restore
Home → Backup and Restore saves all settings, prompts, custom providers and API keys to a password-protected
`.irisbackup` file (AES-256-GCM, PBKDF2-SHA256 with 600,000 iterations). The same file restores on Iris for Android
and on desktop. Statistics and downloaded models aren't included. A restore replaces all settings, so restoring a
backup made on the other platform resets this one's own settings to their defaults (on desktop: the shortcut, paste,
sounds and Launch at Startup).

## Build from source
Needs Rust 1.92+, CMake and the Vulkan SDK (Windows: MSVC Build Tools and the LunarG SDK; Linux: `libvulkan-dev
glslc spirv-headers glslang-tools cmake` plus `libasound2-dev libfontconfig1-dev libwayland-dev libx11-dev
libxkbcommon-dev`).
```
cargo run -p iris                 # the app
cargo build -p iris-overlay-helper   # Linux: the overlay helper, run from next to `iris`
cargo test --workspace
```
Packages: `scripts/package.ps1` (Windows, NSIS, needs `cargo install cargo-packager --locked`) and
`scripts/package.sh` (Linux .deb and AppImage, needs `cargo-deb` and `cargo-packager`). Developer notes are in
`CLAUDE.md` and `docs/`.

## Licence
Iris is open source under the Apache License 2.0. It is built with [Slint](https://slint.dev) (used under its
Royalty-free licence) and ports code from [Handy](https://github.com/cjpais/Handy) (MIT). Third-party licences are on
the About page and in `assets/licenses/`.
