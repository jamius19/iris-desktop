# Iris desktop – project guide

Rust + Slint voice dictation app for Windows 10+ and Linux, recreating the Android app Iris (the Iris Android repo)
with desktop mechanisms from Handy (the Handy repo, MIT).
`PLAN.md` is the plan (milestones M1–M4); `docs/decisions.md` holds decisions and spike results.

Status: M1 (shell, design system, settings pages, persistence), M2 (local dictation: engine worker, model
downloads, capture, VAD gate, controller, hotkeys, overlay, paste, CLI) and M3 (cloud speech-to-text: OpenAI, Vercel AI
Gateway, Deepgram, Soniox; LLM cleanup: Gateway, OpenRouter, custom OpenAI-compatible; model menus) implemented. Cloud
providers are tested against local mock servers only (no API keys on the dev machine). `cargo deny check licenses`
not yet run (cargo-deny not installed). M4 not started.

## Commands (Git Bash, repo root)
```
cargo run -p iris                          # the app (femtovg; SLINT_BACKEND=winit-software for the software renderer)
cargo run -p iris -- --debug               # verbose log to stderr and <data>/logs/iris.log
target/debug/iris -f file.wav --model whisper-small   # headless: on-device provider on a 16 kHz mono WAV
target/debug/iris --toggle-transcription   # (or --cancel) sent to the running instance
cargo build -p iris-overlay-helper         # Linux overlay helper `iris-overlay`, spawned from next to `iris`
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check licenses                  # needs cargo-deny (not installed on the dev machine yet)
cargo xtask assets                         # regenerate assets/icons/{lucide,brand,app}; outputs are committed
```
Linux check from Windows: WSL `Ubuntu` with `CARGO_TARGET_DIR=$HOME/.cache/iris-desktop-target cargo build
--workspace` and `cargo test --workspace` (WSL has no clippy component; no layer-shell, no `/dev/input`).
Engine tests against a real worker (ignored by default; stream tests need a streaming model such as Nemotron):
`IRIS_TRANSCRIBE_WORKER_EXE=$PWD/target/debug/iris.exe IRIS_TEST_MODEL=<gguf> IRIS_TEST_WAV=<16 kHz mono wav>
cargo test -p iris -- --ignored --test-threads=1`.

## Layout
- `crates/iris-core`: UI-free logic and its tests. `settings/` (model = Android `Settings.kt` + `toSettings`,
  store = typed JSON file in Android's backup encoding, secrets = keyring), `copy.rs` (Android `SettingsText.kt`,
  desktop wording), `ui_math.rs` (dropdown placement, slider math, autosave commit), `net.rs` (the providers' HTTP
  client, size-capped reads, WebSocket connect, keychain key reads, warm-up throttle; `net/test_support.rs`: the local
  WebSocket test server), `cleanup/` (`CleanupProvider`, timed cleanup, glossary; `protocol.rs` Chat Completions body,
  `vercel.rs`, `openrouter.rs`, `compatible.rs`, `options.rs`), `stt/` (`SpeechToTextProvider`, `AudioSource`,
  `wav.rs`; `openai/`, `vercel/`, `deepgram/`, `soniox/`: one directory per cloud provider, `protocol.rs` + streaming
  + upload (+ Soniox catalogue/models); `local/`: model table, download store + transport, on-device provider with the
  engine injected, idle rule), `audio/` (speech gate, resample, Sonic time-stretch from
  `vendor/sonic`, visualizer), `dictation/` (state, controller, Handy's coordinator, filler words, language, custom
  words).
- `crates/iris`: the binary. `ui/*.slint` (design system in `ui/theme.slint`, `ui/components/`, pages in
  `ui/pages/`, globals in `ui/globals.slint`), `src/ui/*.rs` (glue: fills `Prefs` from settings, dispatches setters by
  settings key; `shortcuts.rs` for the Shortcut section and Global Shortcut row; `catalogs.rs` the model menus),
  `src/services.rs` (Home checks), `src/favicons.rs`, `src/providers.rs` (Android `AppContainer`: every provider,
  and the one the settings select), `src/dictation.rs` (wires controller, coordinator, hotkeys, cleanup, paste,
  overlay), `src/local/`
  (Handy's `engine_supervisor` verbatim in `supervisor/`, `engine.rs`, Silero `vad.rs`), `src/capture.rs`,
  `src/hotkey.rs`, `src/paste/`, `src/overlay/` (host; Windows window / Linux helper client), `src/cli.rs`,
  `src/single_instance.rs`, `src/signals.rs`, `src/headless.rs` (`-f`).
- `crates/iris-overlay`: `ui/overlay.slint`, `OverlayState`/`apply`, geometry, helper IPC frames.
  `crates/iris-overlay-helper`: Linux-only `iris-overlay` binary (layer-shell via SCTK + Slint software renderer, else
  an X11 override-redirect window).
- `xtask`: dev-only asset generation. `assets/`: fonts, icons, licence texts, `models/` (Silero VAD, embedded).
  `vendor/sonic`: Sonic C. `packaging/linux/`: udev rule. `docs/`: decisions, manual checks, third-party list.

## Conventions
- Author header in every code file (`/* Author: Jamius Siam / Since: dd/mm/yyyy */`, `#` comments in TOML), date =
  creation day.
- 120-column lines; rustfmt with `max_width = 120`, `use_small_heuristics = "Max"`.
- Minimum code, Android names in snake_case, Android copy verbatim unless PLAN.md rewords it for desktop.
- Port tests with the Kotlin/Handy test as the spec (same cases, snake_case names).
- Never push. Commit only when asked, on `feature/port-iris-android`.

## Gotchas
- Slint: wrapped `Text` inside a horizontal layout gets a one-line height unless its column has an explicit width
  (see `IconRow`, `SwitchRow`).
- Slint: a child without `x` is centred in a non-layout parent; set `x: 0` for left-aligned fills.
- Slint: `@children` can't sit inside an `if`; elements inside `if` can't be referenced from outside it; global
  callbacks can't be handled in `.slint` (route through Rust, e.g. `Nav.focus-root`).
- Slint 1.18 codegen bug: assigning a `let` string to a property and then comparing it moves it; compare the property.
- The software renderer drops sub-pixel lines (separators use `1phx`) and doesn't rotate images.
- The window's own `FocusScope` must hold focus for Esc / Alt+Left; fields hand focus back with `Nav.focus-root()`.
- Keychain calls never run on the UI thread (`ui::off_thread`); results come back as `Prefs.key-op-*` events.
- `main.rs` checks for `--transcribe-worker` before anything else: the binary is also the engine worker.
- The controller's state reaches the UI only through `dictation.rs` → `invoke_from_event_loop`; hotkey callbacks run
  on the hotkey thread and must not call `Hotkeys::register_transcribe` (deadlock).
- Never log transcripts: `dictation.rs` logs the state's variant name only. Providers never log keys, transcripts,
  custom words or bodies; `NetError` keeps only the innermost cause (reqwest's own message holds the URL).
- Provider tests use real sockets on 127.0.0.1 (wiremock, `net::test_support::WsServer`): no `start_paused` there
  (paused time auto-advances while waiting on I/O). Timeouts are injectable through `with_urls`-style constructors.
- Anything that sends a request needs a tokio runtime context: callbacks on plain threads (the coordinator's stop,
  hotkeys) must `Handle::enter()` first (warm-ups silently skip without one).
- Python heredocs in Git Bash mangle backslash escapes (a `\n` in the text becomes a real line break); write such
  scripts to a file first, or use the Edit tool.
- Python edits on Windows: open files with `encoding='utf-8'` (the default code page corrupts "…").
- `crates/iris/build.rs` stages transcribe-cpp libs into `crates/iris/transcribe-libs/` (gitignored, packaging) and,
  on Linux, next to the binary (`$ORIGIN` rpath). Windows and WSL builds share that staging dir.
- Running the app writes `%APPDATA%\jamius\Iris` / `%LOCALAPPDATA%\jamius\Iris` (Linux: `~/.config/iris`,
  `~/.local/share/iris`, `~/.cache/iris`).
