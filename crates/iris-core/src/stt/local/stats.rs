/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Speed verdicts from Android's `OnDeviceStats.kt` (the rest of it comes with statistics).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedVerdict {
    KeepsUp,
    Borderline,
    FallsBehind,
}

/// [x] rounded to one decimal, as the page shows it: the verdict and the speed text both use it, so they agree.
/// Half up, as Java's `Math.round`.
pub fn round_to_tenth(x: f64) -> f64 {
    (x * 10.0 + 0.5).floor() / 10.0
}

/// On the factor rounded like the text ([`round_to_tenth`]): 1.45 keeps up, 0.95 is borderline.
pub fn speed_verdict(factor: f64) -> SpeedVerdict {
    if round_to_tenth(factor) >= 1.5 {
        SpeedVerdict::KeepsUp
    } else if round_to_tenth(factor) >= 1.0 {
        SpeedVerdict::Borderline
    } else {
        SpeedVerdict::FallsBehind
    }
}
