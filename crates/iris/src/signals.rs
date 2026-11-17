/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! SIGUSR2 toggles transcription (Handy `signal_handle.rs`), e.g. `pkill -USR2 -n iris` from a compositor binding.
//! SIGUSR1 is left alone, as in Handy.

use signal_hook::consts::SIGUSR2;
use signal_hook::iterator::Signals;

/// Calls `on_toggle` on a signal thread for every SIGUSR2.
pub fn listen(on_toggle: impl Fn() + Send + 'static) {
    let mut signals = match Signals::new([SIGUSR2]) {
        Ok(signals) => signals,
        Err(e) => {
            log::warn!("Couldn't listen for SIGUSR2: {e}");
            return;
        }
    };
    std::thread::Builder::new()
        .name("iris-signals".into())
        .spawn(move || {
            for _ in signals.forever() {
                log::debug!("Received SIGUSR2");
                on_toggle();
            }
        })
        .expect("failed to start the signal thread");
}
