/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Linux tools for paste, from Handy `clipboard.rs`: wl-copy / wl-paste on Wayland, xdotool on X11, ydotool on both
//! (ydotoold socket lookup, `key --help` syntax probe). wtype, dotool and kwtype aren't ported (PLAN M2.10).

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use log::info;

use super::{Chord, Saved};

/// Deadline for the clipboard tools and the key chords; typing gets [`type_timeout`].
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);
/// Deadline for the `ydotool key --help` probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// ydotool's socket when none is trusted: it can't exist, so ydotool fails to connect (and the paste falls back to the
/// clipboard) instead of reaching another user's ydotoold.
const NO_YDOTOOL_SOCKET: &str = "/nonexistent/.ydotool_socket";

/// Runs `cmd` with `input` on stdin, killing it at `timeout` (a hung tool or clipboard owner can't block the paste).
/// stdout and stderr are captured only when `capture`, else null: wl-copy forks a daemon that inherits them, and
/// waiting on its pipes would hang.
fn run(mut cmd: Command, input: Option<&[u8]>, capture: bool, timeout: Duration) -> Result<Output, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let pipe = |on: bool| if on { Stdio::piped() } else { Stdio::null() };
    cmd.stdin(pipe(input.is_some())).stdout(pipe(capture)).stderr(pipe(capture));
    let mut child = cmd.spawn().map_err(|e| format!("Failed to execute {program}: {e}"))?;
    // From threads, so a child that doesn't read stdin, or fills a pipe, can't block past the deadline. The stdin
    // handle is dropped after the write, which ends the input.
    let writer = child.stdin.take().zip(input).map(|(mut stdin, input)| {
        let input = input.to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
        })
    });
    let stdout = child.stdout.take().map(read_all);
    let stderr = child.stderr.take().map(read_all);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("{program} timed out"));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("Failed to wait for {program}: {e}"));
            }
        }
    };
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let joined = |reader: Option<thread::JoinHandle<Vec<u8>>>| reader.and_then(|r| r.join().ok()).unwrap_or_default();
    let (stdout, stderr) = (joined(stdout), joined(stderr));
    Ok(Output { status: status?, stdout, stderr })
}

fn read_all(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// Typing deadline: 5 s plus 50 ms per character.
fn type_timeout(text: &str) -> Duration {
    Duration::from_millis(5_000 + 50 * text.chars().count() as u64)
}

/// The app runs in a Wayland session (`WAYLAND_DISPLAY` set).
pub fn wayland() -> bool {
    env::var_os("WAYLAND_DISPLAY").is_some()
}

/// What Wayland paste still needs, as helper sentences; empty outside Wayland.
pub fn text_input_missing() -> Vec<&'static str> {
    if !wayland() {
        return Vec::new();
    }
    let mut missing = Vec::new();
    if !wl_clipboard() {
        missing.push("Install wl-clipboard.");
    }
    if !on_path("ydotool") || ydotool_socket().is_none() {
        missing.push("Install ydotool and start ydotoold.");
    }
    missing
}

fn on_path(program: &str) -> bool {
    env::var_os("PATH").is_some_and(|path| env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// The Wayland clipboard goes through wl-clipboard when it is installed (else arboard, as on X11).
pub fn wl_clipboard() -> bool {
    wayland() && on_path("wl-copy") && on_path("wl-paste")
}

/// What the clipboard holds, to restore after the paste: its text if a text type is offered, else the bytes of the
/// first offered type (e.g. an image). [`Saved::Unreadable`] when it couldn't be read, so the restore leaves the
/// transcript rather than wiping the clipboard.
pub fn wl_save() -> Saved {
    let mut list = Command::new("wl-paste");
    list.arg("--list-types");
    let types = match run(list, None, true, TOOL_TIMEOUT) {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).into_owned(),
        // wl-paste fails when nothing is copied.
        Ok(_) => return Saved::Nothing,
        Err(e) => {
            log::warn!("Couldn't read the clipboard types: {e}");
            return Saved::Unreadable;
        }
    };
    let types: Vec<&str> = types.lines().map(str::trim).filter(|mime| !mime.is_empty()).collect();
    let Some(&first) = types.first() else {
        return Saved::Nothing;
    };
    let result = if types.iter().any(|mime| is_text(mime)) {
        wl_paste().map(|text| if text.is_empty() { Saved::Nothing } else { Saved::Text(text) })
    } else {
        let mut paste = Command::new("wl-paste");
        paste.args(["--no-newline", "--type", first]);
        match run(paste, None, true, TOOL_TIMEOUT) {
            Ok(output) if output.status.success() => Ok(Saved::Bytes { mime: first.to_owned(), data: output.stdout }),
            Ok(output) => Err(format!("wl-paste failed ({})", output.status)),
            Err(e) => Err(e),
        }
    };
    result.unwrap_or_else(|e| {
        log::warn!("Couldn't save the clipboard: {e}");
        Saved::Unreadable
    })
}

/// The MIME types wl-paste reads as text.
fn is_text(mime: &str) -> bool {
    mime.starts_with("text/") || matches!(mime, "TEXT" | "STRING" | "UTF8_STRING")
}

/// The clipboard text (empty when there is none).
fn wl_paste() -> Result<String, String> {
    let mut paste = Command::new("wl-paste");
    paste.arg("--no-newline");
    let output = run(paste, None, true, TOOL_TIMEOUT)?;
    if !output.status.success() {
        return Err(format!("wl-paste failed ({})", output.status));
    }
    String::from_utf8(output.stdout).map_err(|_| "The clipboard text isn't UTF-8".to_owned())
}

/// Writes the clipboard with wl-copy, the text on stdin (never in argv, where other users' `ps` can read it). Its
/// stdout and stderr stay null (see [`run`]).
pub fn wl_copy(text: &str) -> Result<(), String> {
    wl_copy_bytes("text/plain", text.as_bytes())
}

/// Writes `data` as `mime` with wl-copy.
pub fn wl_copy_bytes(mime: &str, data: &[u8]) -> Result<(), String> {
    let mut copy = Command::new("wl-copy");
    copy.args(["--type", mime]);
    wl_copy_status(run(copy, Some(data), false, TOOL_TIMEOUT)?)
}

pub fn wl_clear() -> Result<(), String> {
    let mut copy = Command::new("wl-copy");
    copy.arg("--clear");
    wl_copy_status(run(copy, None, false, TOOL_TIMEOUT)?)
}

fn wl_copy_status(output: Output) -> Result<(), String> {
    if !output.status.success() {
        return Err("wl-copy failed".into());
    }
    Ok(())
}

/// Sends the paste chord with the first tool that works (X11: xdotool, then ydotool; Wayland: ydotool). A tool that
/// is installed but fails (ydotool without its daemon) hands over to the next one. `false` when none sent it.
pub fn try_send_chord(chord: Chord) -> bool {
    type SendChord = fn(Chord) -> Result<(), String>;
    let mut tools: Vec<(&str, SendChord)> = Vec::new();
    if !wayland() && on_path("xdotool") {
        tools.push(("xdotool", send_chord_via_xdotool));
    }
    if on_path("ydotool") {
        tools.push(("ydotool", send_chord_via_ydotool));
    }
    for (name, send) in tools {
        info!("Using {name} for the paste keys");
        match send(chord) {
            Ok(()) => return true,
            Err(e) => log::warn!("{name} could not send the paste keys: {e}"),
        }
    }
    false
}

/// Types `text` with a native tool (X11: xdotool, then ydotool; Wayland: ydotool). `None` when none is installed.
pub fn try_type_text(text: &str) -> Option<Result<(), String>> {
    if !wayland() && on_path("xdotool") {
        info!("Using xdotool for direct text input");
        return Some(type_text_via_xdotool(text));
    }
    if on_path("ydotool") {
        info!("Using ydotool for direct text input");
        return Some(type_text_via_ydotool(text));
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum YdotoolKeySyntax {
    Symbolic,
    RawKeycodes,
}

const YDOTOOL_UNKNOWN_HELP_FALLBACK: YdotoolKeySyntax = YdotoolKeySyntax::RawKeycodes;

static YDOTOOL_KEY_SYNTAX: OnceLock<YdotoolKeySyntax> = OnceLock::new();

/// Classifies `ydotool key --help` output without relying on version or distro metadata.
fn classify_ydotool_key_syntax(help: &str) -> Option<YdotoolKeySyntax> {
    let help = help.to_ascii_lowercase();
    // Modern markers first, in case a future help message mentions the legacy syntax.
    if help.contains("syntax: <keycode>:<pressed>")
        || help.contains("[keycodes]")
        || help.contains("using raw keycodes")
    {
        Some(YdotoolKeySyntax::RawKeycodes)
    } else if help.contains("separated by plus (+)") || (help.contains("<key sequence>") && help.contains("alt+r")) {
        Some(YdotoolKeySyntax::Symbolic)
    } else {
        None
    }
}

/// ydotoold's socket: `$YDOTOOL_SOCKET`, else the first of the places distro packages use (/tmp on Fedora,
/// /run/ydotoold on NixOS) owned by root or by us, since /tmp is world-writable.
pub fn ydotool_socket() -> Option<PathBuf> {
    if let Some(socket) = env::var_os("YDOTOOL_SOCKET").filter(|v| !v.is_empty()) {
        return Some(socket.into());
    }
    // SAFETY: getuid has no preconditions and can't fail.
    let uid = unsafe { libc::getuid() };
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR").map(|dir| Path::new(&dir).join(".ydotool_socket"));
    runtime_dir
        .into_iter()
        .chain(["/tmp/.ydotool_socket".into(), "/run/ydotoold/socket".into()])
        .find(|socket| fs::metadata(socket).is_ok_and(|m| m.uid() == 0 || m.uid() == uid))
}

/// A `ydotool` command that reaches ydotoold when `YDOTOOL_SOCKET` isn't set (autostart, desktop launcher). Without a
/// trusted socket it gets one that can't exist, rather than ydotool's default /tmp path that anyone could own.
fn ydotool_command() -> Command {
    let mut command = Command::new("ydotool");
    command.env("YDOTOOL_SOCKET", ydotool_socket().unwrap_or_else(|| NO_YDOTOOL_SOCKET.into()));
    command
}

/// Detects and caches a recognised ydotool key syntax. Unknown or failed probes aren't cached, so a transient daemon
/// or PATH problem can recover on a later paste.
fn detect_ydotool_key_syntax() -> YdotoolKeySyntax {
    if let Some(syntax) = YDOTOOL_KEY_SYNTAX.get() {
        return *syntax;
    }
    let mut probe = ydotool_command();
    probe.args(["key", "--help"]);
    match run(probe, None, true, PROBE_TIMEOUT) {
        Ok(output) => {
            // ydotool 0.x writes help to stderr and its exit status varies by build.
            let mut help = String::from_utf8_lossy(&output.stdout).into_owned();
            if !help.is_empty() && !output.stderr.is_empty() {
                help.push('\n');
            }
            help.push_str(&String::from_utf8_lossy(&output.stderr));
            if let Some(syntax) = classify_ydotool_key_syntax(&help) {
                *YDOTOOL_KEY_SYNTAX.get_or_init(|| {
                    info!("Detected ydotool key syntax: {syntax:?}");
                    syntax
                })
            } else {
                log::warn!(
                    "Could not recognize ydotool key --help output (exit status {:?}); using raw-keycode syntax",
                    output.status.code()
                );
                YDOTOOL_UNKNOWN_HELP_FALLBACK
            }
        }
        Err(e) => {
            log::warn!("Could not query ydotool key syntax: {e}; using raw-keycode syntax");
            YDOTOOL_UNKNOWN_HELP_FALLBACK
        }
    }
}

/// ydotool prints its errors, such as a missing ydotoold, on stdout rather than stderr.
fn ydotool_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = [stderr.trim(), stdout.trim()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>();
    format!("ydotool failed ({}): {}", output.status, message.join(" "))
}

fn ydotool_key_args(chord: Chord, syntax: YdotoolKeySyntax) -> &'static [&'static str] {
    match (chord, syntax) {
        (Chord::CtrlV, YdotoolKeySyntax::Symbolic) => &["key", "ctrl+v"],
        (Chord::CtrlShiftV, YdotoolKeySyntax::Symbolic) => &["key", "ctrl+shift+v"],
        (Chord::ShiftInsert, YdotoolKeySyntax::Symbolic) => &["key", "shift+insert"],
        (Chord::CtrlV, YdotoolKeySyntax::RawKeycodes) => &["key", "29:1", "47:1", "47:0", "29:0"],
        (Chord::CtrlShiftV, YdotoolKeySyntax::RawKeycodes) => &["key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"],
        (Chord::ShiftInsert, YdotoolKeySyntax::RawKeycodes) => &["key", "42:1", "110:1", "110:0", "42:0"],
    }
}

fn send_chord_via_ydotool(chord: Chord) -> Result<(), String> {
    let mut command = ydotool_command();
    command.args(ydotool_key_args(chord, detect_ydotool_key_syntax()));
    let output = run(command, None, true, TOOL_TIMEOUT)?;
    if !output.status.success() {
        return Err(ydotool_error(&output));
    }
    Ok(())
}

/// The text goes on stdin (`--file -`, which 0.1.8 and 1.0.x both read); a failure isn't retried with the text in
/// argv, so it goes down the clipboard path.
fn type_text_via_ydotool(text: &str) -> Result<(), String> {
    let mut command = ydotool_command();
    command.args(["type", "--file", "-"]);
    let output = run(command, Some(text.as_bytes()), true, type_timeout(text))?;
    if !output.status.success() {
        return Err(ydotool_error(&output));
    }
    Ok(())
}

fn send_chord_via_xdotool(chord: Chord) -> Result<(), String> {
    let key_combo = match chord {
        Chord::CtrlV => "ctrl+v",
        Chord::CtrlShiftV => "ctrl+shift+v",
        Chord::ShiftInsert => "shift+Insert",
    };
    let mut command = Command::new("xdotool");
    command.args(["key", "--clearmodifiers", key_combo]);
    let output = run(command, None, true, TOOL_TIMEOUT)?;
    if !output.status.success() {
        return Err(format!("xdotool failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(())
}

/// The text goes on stdin (`--file -`), as for ydotool.
fn type_text_via_xdotool(text: &str) -> Result<(), String> {
    let mut command = Command::new("xdotool");
    command.args(["type", "--clearmodifiers", "--file", "-"]);
    let output = run(command, Some(text.as_bytes()), true, type_timeout(text));
    // `--clearmodifiers` restores the modifiers held when xdotool started; if the user releases one meanwhile, the
    // synthetic restore can leave it latched (Handy #1817). Release both sides of the push-style modifiers, before
    // checking the typing status, since xdotool may have changed them before failing. Lock keys toggle, so not them.
    let mut cleanup = Command::new("xdotool");
    cleanup.args(["keyup", "Control_L", "Control_R", "Shift_L", "Shift_R", "Alt_L", "Alt_R", "Super_L", "Super_R"]);
    match run(cleanup, None, true, TOOL_TIMEOUT) {
        Ok(cleanup) if !cleanup.status.success() => log::warn!(
            "xdotool modifier cleanup failed with status {:?}: {}",
            cleanup.status.code(),
            String::from_utf8_lossy(&cleanup.stderr).trim()
        ),
        Err(e) => log::warn!("xdotool modifier cleanup failed: {e}"),
        _ => {}
    }
    let output = output?;
    if !output.status.success() {
        return Err(format!("xdotool failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const YDOTOOL_0_1_8_HELP: &str = r#"
Usage: key [--delay <ms>] [--key-delay <ms>] [--repeat <times>] [--repeat-delay <ms>] <key sequence> ...
Each key sequence can be any number of modifiers and keys, separated by plus (+)
For example: alt+r Alt+F4 CTRL+alt+f3 aLT+1+2+3 ctrl+Backspace
"#;

    const YDOTOOL_1_0_4_HELP: &str = r#"
Usage: key [OPTION]... [KEYCODES]...
Since there's no way to know how many keyboard layouts are there in the world,
we're using raw keycodes now.
Syntax: <keycode>:<pressed>
e.g. 28:1 28:0 means pressing on the Enter button on a standard US keyboard.
"#;

    #[test]
    fn classifies_ydotool_0_1_8_symbolic_help() {
        assert_eq!(classify_ydotool_key_syntax(YDOTOOL_0_1_8_HELP), Some(YdotoolKeySyntax::Symbolic));
    }

    #[test]
    fn classifies_ydotool_1_0_4_raw_keycode_help() {
        assert_eq!(classify_ydotool_key_syntax(YDOTOOL_1_0_4_HELP), Some(YdotoolKeySyntax::RawKeycodes));
    }

    #[test]
    fn unknown_ydotool_help_falls_back_to_raw_keycodes() {
        let syntax = classify_ydotool_key_syntax("unrecognized help output").unwrap_or(YDOTOOL_UNKNOWN_HELP_FALLBACK);

        assert_eq!(syntax, YdotoolKeySyntax::RawKeycodes);
    }

    #[test]
    fn generates_symbolic_ydotool_arguments_for_all_paste_methods() {
        assert_eq!(ydotool_key_args(Chord::CtrlV, YdotoolKeySyntax::Symbolic), ["key", "ctrl+v"]);
        assert_eq!(ydotool_key_args(Chord::CtrlShiftV, YdotoolKeySyntax::Symbolic), ["key", "ctrl+shift+v"]);
        assert_eq!(ydotool_key_args(Chord::ShiftInsert, YdotoolKeySyntax::Symbolic), ["key", "shift+insert"]);
    }

    #[test]
    fn generates_raw_ydotool_arguments_for_all_paste_methods() {
        assert_eq!(
            ydotool_key_args(Chord::CtrlV, YdotoolKeySyntax::RawKeycodes),
            ["key", "29:1", "47:1", "47:0", "29:0"]
        );
        assert_eq!(
            ydotool_key_args(Chord::CtrlShiftV, YdotoolKeySyntax::RawKeycodes),
            ["key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"]
        );
        assert_eq!(
            ydotool_key_args(Chord::ShiftInsert, YdotoolKeySyntax::RawKeycodes),
            ["key", "42:1", "110:1", "110:0", "42:0"]
        );
    }
}
