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
| Sonic (M2) | via Handy `vendor/sonic` | Apache-2.0 | `assets/licenses/sonic_LICENSE.txt` |
| Silero VAD (M2) | via Handy / transcribe.cpp | MIT | `assets/licenses/silero_vad_LICENSE.txt` |
| transcribe.cpp, ggml, miniz (M2) | `transcribe-cpp-sys` 0.4.2 `LICENSE` + `THIRD-PARTY-LICENSES.md` | MIT | `assets/licenses/transcribe_cpp_LICENSE.txt` |
