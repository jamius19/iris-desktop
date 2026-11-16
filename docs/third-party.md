# Third-party material

Licence texts are in `assets/licenses/` and shown on the About page. The generated Rust dependency list
(`cargo about`) comes in M4.

| What | Source | Licence | In the repo |
|---|---|---|---|
| Inter (variable, opsz 14 instances 400/500/600/800 used as-is) | Android app `res/font/inter.ttf` | SIL OFL 1.1 | `assets/fonts/inter.ttf` |
| Blinker SemiBold | Android app `res/font/blinker_semibold.ttf` | SIL OFL 1.1 | `assets/fonts/blinker_semibold.ttf` |
| Lucide icons 1.72.0 | `raw.githubusercontent.com/lucide-icons/lucide/1.72.0/icons/` via `cargo xtask assets`, stroke widths from the Android drawables | ISC (+ MIT for Feather-derived icons) | `assets/icons/lucide/` |
| Iris logo, app icon layers, provider and maker marks | `iris_icons` (user-supplied); marks belong to their owners (OpenAI, Vercel, OpenRouter, Deepgram, Soniox, NVIDIA) | – | `assets/icons/brand/`, `assets/icons/app/` (generated) |
| Slint 1.18 | crates.io | Royalty-free 2.0 (chosen of GPL-3.0 / Royalty-free / commercial); the About page shows `AboutSlint` | dependency |
| Handy (code ported from M2 on) | `github.com/cjpais/Handy` | MIT, © CJ Pais | `assets/licenses/handy_LICENSE.txt` |
| Sonic (M2) | via Handy `vendor/sonic` | Apache-2.0 | `vendor/sonic/` (C source + `LICENSE`), `assets/licenses/sonic_LICENSE.txt` |
| Silero VAD v6.2 GGUF (M2) | Handy `src-tauri/resources/models/silero-vad-v6.2-F32.gguf` | MIT | `assets/models/` (embedded in the binary), `assets/licenses/silero_vad_LICENSE.txt` |
| transcribe.cpp, ggml, miniz (M2) | `transcribe-cpp-sys` 0.4.2 `LICENSE` + `THIRD-PARTY-LICENSES.md` | MIT | `assets/licenses/transcribe_cpp_LICENSE.txt` |

Rust crates added in M2 (licences read with `cargo metadata`; `cargo deny` not run, it isn't installed):
transcribe-cpp / transcribe-cpp-sys 0.4.2 (MIT), handy-keys 0.3.5 (MIT), enigo 0.6.1 (MIT), arboard 3.7 (MIT OR
Apache-2.0), interprocess 2.4 (MIT OR Apache-2.0), clap 4 (MIT OR Apache-2.0), hound 3.5 (Apache-2.0),
signal-hook 0.4 (MIT OR Apache-2.0, Linux), libc 0.2 (MIT OR Apache-2.0), windows 0.61 (MIT OR Apache-2.0, Windows),
smithay-client-toolkit 0.19 and x11rb 0.13 (MIT / MIT OR Apache-2.0, overlay helper, Linux), whatlang 0.18 (MIT),
strsim 0.11 (MIT), rustfft 6 (MIT OR Apache-2.0), regex 1 (MIT OR Apache-2.0), fs4 1.1 (MIT OR Apache-2.0), sha2 0.11
(MIT OR Apache-2.0), cc 1 (build, MIT OR Apache-2.0). Their transitive crates are permissive (see the M2 report).
