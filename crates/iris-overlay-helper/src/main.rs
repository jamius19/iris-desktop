/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! `iris-overlay`, the Linux overlay helper (PLAN §2.5, §2.6): layer-shell mode when the compositor offers
//! `zwlr_layer_shell_v1`, else X11 mode (X11 sessions, and XWayland e.g. on GNOME). Reads `ToHelper` frames on stdin,
//! writes `FromHelper` frames on stdout, logs to stderr and exits on stdin EOF. Elsewhere it is an empty program.

#[cfg(target_os = "linux")]
mod layer;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
fn main() {
    if !layer::run() {
        x11::run();
    }
}

/// Sends an event to the app.
#[cfg(target_os = "linux")]
fn send(event: iris_overlay::ipc::FromHelper) {
    if let Err(e) = iris_overlay::ipc::write_frame(&mut std::io::stdout().lock(), &event) {
        eprintln!("iris-overlay: couldn't write to the app: {e}");
    }
}

/// Reports a fatal error to the app and on stderr.
#[cfg(target_os = "linux")]
fn fail(message: String) {
    eprintln!("iris-overlay: {message}");
    send(iris_overlay::ipc::FromHelper::Failed(message));
}

/// Reads the app's frames on a thread: `Some` per message, then `None` at EOF or on a broken frame.
#[cfg(target_os = "linux")]
fn read_stdin(mut on_message: impl FnMut(Option<iris_overlay::ipc::ToHelper>) + Send + 'static) {
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        loop {
            match iris_overlay::ipc::read_frame(&mut stdin) {
                Ok(Some(message)) => on_message(Some(message)),
                Ok(None) => break,
                Err(e) => {
                    eprintln!("iris-overlay: couldn't read from the app: {e}");
                    break;
                }
            }
        }
        on_message(None);
    });
}
