/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! One Iris per user (replaces Handy's Tauri single-instance plugin): a local socket, see [`name`]. The first instance
//! serves it; a later one sends one JSON line (`{"cmd":"toggle"|"cancel"|"show"}`) and exits. If connecting fails,
//! the later instance becomes the first. Each connection is read on its own short-lived thread, at most one 256-byte
//! line (with a 1 s timeout on Linux), so a stalled or flooding client can't hold up later commands. On Linux a
//! connection from another user is dropped; on Windows a later instance sends nothing to a pipe another user serves.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Name, Stream};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Message {
    cmd: String,
}

/// Sends `command` to a running instance; false if none answered.
pub fn forward(command: &str) -> bool {
    let send = || -> io::Result<()> {
        let mut stream = Stream::connect(name()?)?;
        // Another user could have created the pipe first; a command must not reach them.
        #[cfg(windows)]
        {
            let Stream::NamedPipe(pipe) = &stream;
            let owned =
                pipe.inner().server_process_id().and_then(|server| owner::same_user(server, std::process::id()));
            if !owned.as_ref().is_ok_and(|same| *same) {
                log::warn!("The instance pipe isn't served by this user ({owned:?}); not sending to it");
                return Err(io::Error::other("the instance pipe belongs to another user"));
            }
        }
        let mut line = serde_json::to_vec(&Message { cmd: command.into() })?;
        line.push(b'\n');
        stream.write_all(&line)?;
        stream.flush()
    };
    match send() {
        Ok(()) => true,
        Err(e) => {
            log::debug!("No running instance answered ({e})");
            false
        }
    }
}

/// Serves the socket on a thread; `on_command` gets each command (`toggle`, `cancel`, `show`) on a connection's
/// thread. Exits the process if another instance turns out to own the name.
pub fn serve(on_command: impl Fn(&str) + Send + Sync + 'static) {
    // Only called after `forward` got no answer, so an existing socket file is a stale one from a crash. Not for an
    // abstract name: there is no file to remove, and the overwrite would spin while another instance holds it.
    let listen = || {
        name().and_then(|name| {
            let overwrite = name.is_path();
            ListenerOptions::new().name(name).try_overwrite(overwrite).create_sync()
        })
    };
    // A relaunch with the software renderer starts while the failed instance may still hold the name.
    let mut attempt = listen();
    for _ in 0..10 {
        if attempt.is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
        attempt = listen();
    }
    let listener = match attempt {
        Ok(listener) => listener,
        Err(e) => {
            if forward("show") {
                log::info!("Another instance owns the instance socket; exiting");
                std::process::exit(0);
            }
            log::warn!("Couldn't listen for other instances; --toggle-transcription won't reach this one: {e}");
            return;
        }
    };
    let on_command = Arc::new(on_command);
    thread::Builder::new()
        .name("iris-instance".into())
        .spawn(move || {
            for connection in listener.incoming() {
                let connection = match connection {
                    Ok(connection) => connection,
                    Err(e) => {
                        log::warn!("Accepting another instance failed: {e}");
                        continue;
                    }
                };
                #[cfg(unix)]
                if !same_user(&connection) {
                    continue;
                }
                let on_command = on_command.clone();
                let handled = thread::Builder::new()
                    .name("iris-instance-connection".into())
                    .spawn(move || handle(connection, &*on_command));
                if let Err(e) = handled {
                    log::warn!("Couldn't handle another instance: {e}");
                }
            }
        })
        .expect("failed to start the instance thread");
}

fn handle(connection: Stream, on_command: &dyn Fn(&str)) {
    // Unsupported on Windows pipes: there a stalled client only holds this thread.
    let _ = connection.set_recv_timeout(Some(Duration::from_secs(1)));
    let mut line = String::new();
    if BufReader::new(connection.take(256)).read_line(&mut line).is_err() {
        return;
    }
    match serde_json::from_str::<Message>(&line) {
        Ok(message) => on_command(&message.cmd),
        Err(e) => log::warn!("Ignored a malformed instance message: {e}"),
    }
}

/// Whether the peer runs as this user.
#[cfg(unix)]
fn same_user(connection: &Stream) -> bool {
    // SAFETY: getuid has no preconditions and can't fail.
    let uid = unsafe { libc::getuid() };
    match connection.peer_creds().map(|creds| creds.euid()) {
        Ok(Some(euid)) if euid == uid => true,
        Ok(euid) => {
            log::warn!("Dropped an instance connection from another user ({euid:?})");
            false
        }
        Err(e) => {
            log::warn!("Dropped an instance connection: its user is unknown ({e})");
            false
        }
    }
}

/// Linux: `$XDG_RUNTIME_DIR/iris-desktop.sock` (a socket file in the user's own directory), else the abstract
/// `iris-desktop-<uid>.sock` (abstract sockets aren't per user, so the user is in the name).
#[cfg(unix)]
fn name() -> io::Result<Name<'static>> {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => std::path::PathBuf::from(dir).join("iris-desktop.sock").to_fs_name::<GenericFilePath>(),
        None => format!("iris-desktop-{}.sock", user()).to_ns_name::<GenericNamespaced>(),
    }
}

/// Windows: the pipe `iris-desktop-<user>-<session id>.sock`, so each logon session (e.g. fast user switching, RDP)
/// has its own.
#[cfg(windows)]
fn name() -> io::Result<Name<'static>> {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;

    let mut session = 0u32;
    // SAFETY: `session` is a valid pointer for the call.
    unsafe { ProcessIdToSessionId(std::process::id(), &mut session) }.map_err(io::Error::other)?;
    format!("iris-desktop-{}-{session}.sock", user()).to_ns_name::<GenericNamespaced>()
}

#[cfg(windows)]
fn user() -> String {
    std::env::var("USERNAME").unwrap_or_default()
}

#[cfg(unix)]
fn user() -> String {
    // SAFETY: getuid has no preconditions and can't fail.
    unsafe { libc::getuid() }.to_string()
}

/// Windows: which user a process runs as (its token's user SID).
#[cfg(windows)]
mod owner {
    use std::io;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{EqualSid, GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

    /// Whether processes `a` and `b` run as the same user.
    pub fn same_user(a: u32, b: u32) -> io::Result<bool> {
        let (a, b) = (token_user(a)?, token_user(b)?);
        // SAFETY: both buffers hold a TOKEN_USER whose SID points into the same, still alive, buffer.
        Ok(unsafe { EqualSid(sid(&a), sid(&b)) }.is_ok())
    }

    /// Process `pid`'s TOKEN_USER, in a u64 buffer so it is aligned for the struct.
    fn token_user(pid: u32) -> io::Result<Vec<u64>> {
        // SAFETY: the out-pointers are valid for the calls, the buffer holds `len` bytes, and both handles are closed.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?;
            let mut token = HANDLE::default();
            let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
            let _ = CloseHandle(process);
            opened?;
            let mut len = 0u32;
            // Fails with the buffer size needed.
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
            let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
            let read = GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr().cast()), len, &mut len);
            let _ = CloseHandle(token);
            read?;
            Ok(buffer)
        }
    }

    fn sid(token_user: &[u64]) -> PSID {
        // SAFETY: filled by GetTokenInformation(TokenUser), so it starts with a TOKEN_USER.
        unsafe { (*token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn this_process_runs_as_its_own_user() {
            let pid = std::process::id();
            assert!(super::same_user(pid, pid).unwrap());
        }
    }
}
