/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! About page: the licence texts, and the generated Rust libraries text by line.

use slint::{ComponentHandle, SharedString};

use super::model;
use crate::{App, Licence, MainWindow};

const LICENCES: [(&str, &str); 8] = [
    ("Iris (Apache License 2.0)", include_str!("../../../../LICENSE")),
    ("Inter (SIL Open Font License 1.1)", include_str!("../../../../assets/licenses/inter_OFL.txt")),
    ("Blinker (SIL Open Font License 1.1)", include_str!("../../../../assets/licenses/blinker_OFL.txt")),
    ("Lucide icons (ISC and MIT)", include_str!("../../../../assets/licenses/lucide_LICENSE.txt")),
    ("Sonic (Apache License 2.0)", include_str!("../../../../assets/licenses/sonic_LICENSE.txt")),
    ("Silero VAD (MIT)", include_str!("../../../../assets/licenses/silero_vad_LICENSE.txt")),
    ("transcribe.cpp and ggml (MIT)", include_str!("../../../../assets/licenses/transcribe_cpp_LICENSE.txt")),
    ("Handy (MIT, ported code)", include_str!("../../../../assets/licenses/handy_LICENSE.txt")),
];

const RUST_LICENCES: &str = include_str!("../../../../assets/licenses/THIRD_PARTY_LICENSES.txt");

pub fn fill(window: &MainWindow) {
    let licences =
        LICENCES.into_iter().map(|(title, text)| Licence { title: title.into(), text: text.trim().into() }).collect();
    window.global::<App>().set_licences(model(licences));
    window.global::<App>().set_rust_licences(model(RUST_LICENCES.trim().lines().map(SharedString::from).collect()));
}
