# Decisions and accepted trade-offs

Started 16/11/2026 (M1). Newest entries at the end of each section.

## Answers to the plan's open questions (user)
- **Q1 Moonshine**: dropped on desktop. Four local models: Nemotron Streaming English (default), Parakeet Unified
  English, Parakeet TDT v3, Whisper Small. A stored `moonshine-base-en` (e.g. from an Android backup) stays in the file
  but reads as the default.
- **Q2 Releases**: a GitHub release workflow in M4, Android's pattern (push to `main` → build, test, tag, release).
- **Q3 `--toggle-transcription` with Iris not running**: just starts Iris (Handy's behaviour).

## Defaults the user can still object to (from PLAN §1)
Phone wording → "on this computer" and the desktop RAM guide; battery, dontkillmyapp and Accessibility rows dropped;
haptics, IME placement, predictive back, the draggable bubble, smart spacing and "field changed → copied" dropped;
hover overlays, keyboard focus rings and Esc / Alt+Left / mouse back added; Overlay Position (default Bottom);
`whatlang` for gated fillers; Gateway streaming STT ported as-is; Time Saved at 40 WPM; Handy's default shortcuts;
finalizing not cancellable; black overlay surface; Handy's sounds, off by default; x86_64 only.
A backup made on Windows restores `shortcut_transcribe` on Linux as-is (it may show Not registered there).

## Spike results (16/11/2026)
Throwaway code stayed in the session scratch directory; none of it is in the repo.

| Spike | Run? | Outcome | Impact on the plan |
|---|---|---|---|
| S1 Slint basics | Windows: run (by the M1 app). Linux: run in WSLg (Weston), Wayland and X11. GNOME/KDE: **not run — needs a Linux machine**. | femtovg and software renderers start on Windows, WSLg Wayland and WSLg X11. Variable Inter renders 400/500/600/800 as distinct real weights (WSLg snapshots; Windows software-renderer captures of the app). The Windows desktop was locked, so the app was captured with `PrintWindow`, which can't read femtovg's GL surface: femtovg on Windows started and ran but its output was not seen. `AboutSlint` and `Palette` only exist in `std-widgets.slint`; a selective import works without other widget styling. `PopupWindow` works. Windows dark mode is followed (app came up dark with the OS dark). WSLg has no portal Settings, so `color-scheme` is `unknown` there; Slint 1.18's winit backend reads and watches the portal `color-scheme` itself (verified against a mock portal). | No static Inter instances, no `cargo xtask fonts`. No `iris::theme` portal watcher; `unknown` = light. |
| S2 Windows overlay | **Partly run**: the PC was locked, so the focus, click and black-fringe checks could not run. One monitor only, so mixed-DPI placement is not run. | Ex-style checks pass: after the fix-up and 400 ms later the overlay has `NOACTIVATE \| TOOLWINDOW \| TOPMOST`; winit rewrites `GWL_EXSTYLE` on every show/hide (control run fails without the fix-up). The hook only affects the overlay when gated. Placement on the 3840×2160 @ 1.5 monitor matched Handy's formula exactly. | Recipe in PLAN §2.6: gate the process-wide attributes hook with a thread-local flag; `no-frame` + `always-on-top` in `overlay.slint`; clear `WS_EX_APPWINDOW`; `WM_STYLECHANGING` guard in the subclass. Remaining checks re-run with the spike harness in M2 (needs an unlocked desktop). |
| S3 transcribe-cpp outside Tauri | Windows: run (Vulkan, RTX 3050). Linux: run in WSL (CPU; WSL only exposes llvmpipe). | `iris-spike -f test.wav` printed the exact text with Parakeet TDT v3 Q8_0 in a worker process (Handy's framed protocol). Clean debug build 300 s (Windows), 207 s (WSL). No `TRANSCRIBE_CMAKE_ARGS`, NASM or clang needed; the LunarG SDK supplies SPIRV-Headers locally. `transcribe-cpp-sys` 0.4.2 already copies its 13 DLLs into `target/<profile>/`. Only `msvcp140`/`vcruntime140`/`vcruntime140_1` imported (no `vcomp140`). First GPU inference ~10 s (pipeline build), then 143 ms. reqwest 0.13 default rustls (aws-lc-rs) builds without NASM. | M2 step 2 and R5 updated (no extra env, staging only for packaging, rpath with `$ORIGIN` first). CI needs Handy's vcpkg SPIRV-Headers step. Real-Linux Vulkan untested. |
| S4 Layer-shell helper | **Partly run**: WSLg has no `zwlr_layer_shell_v1`, so layer-shell mode is **not run — needs a Linux machine (KDE/Sway/Hyprland)**. | The SCTK 0.21 + Slint `MinimalSoftwareWindow` + `wl_shm` helper compiles; on an xdg surface it rendered and animated at 61 fps, input region following the card. X11 mode (override-redirect, notification type, `WAYLAND_DISPLAY` removed) showed on XWayland and a synthetic click reached the Slint `TouchArea` without moving focus. | Approach is workable (R2 fallback probably unneeded). Consider pinning the helper to SCTK 0.19 to share winit's copy. |
| S5 Tray + keyring on Linux | **Partly run** with mocks; KDE, GNOME (AppIndicator), GNOME Keyring and KWallet **not run — needs a Linux machine**. | Tray (ksni under Slint's `system-tray`): menu filled at once, item clicks and icon changes work with a mock StatusNotifierWatcher; without one, only a stderr line. **Bug:** a watcher that appears after the tray was created is never registered with. keyring 4.x requires feature `v1` (or `cli`); `v1` = Windows Credential Manager + zbus Secret Service (no libdbus). Without a Secret Service: `NoDefaultStore`, never retried. | Keep the Slint tray; recreate it when the watcher appears (R4). keyring row in §3 corrected; R9 notes the no-retry behaviour. |

## M1 decisions
- **Workspace**: `crates/iris-core` (UI-free, tested), `crates/iris` (Slint app), `xtask` (asset generation). The
  overlay crates come in M2.
- **Settings file** uses Android's backup encoding exactly (every value a JSON string with its type), so M4's backup can
  reuse it. A corrupt or newer-version file is moved aside (`settings.json.corrupt-<unix seconds>`), reads as empty and
  is logged; a wrongly typed or malformed known key is dropped on its own (its default shows; the file is copied aside
  first), while `to_settings` stays strict like Android's (backup restore relies on it). An unreadable file, or one
  that couldn't be set aside, makes every save fail rather than overwrite it.
- **Keychain**: keyring 4.2 default feature `v1`. Calls run on the tokio runtime's blocking pool; the result reaches the
  field as a "key event" (`Prefs.key-op-*`). Stored keys are probed once at start (keychains can't be listed).
- **Gateway `gateway_stt_live`**: saved as `true` for every typed model until the M3 catalogue decides it (Android's
  fallback when the model isn't in the catalogue).
- **Custom words "sent" predicate** (`stt_takes_custom_words`) ported now from the provider protocol helpers, so the
  M1 notes are right; M3 moves the helpers into the protocol modules.
- **Favicons** use the `psl` crate and require a known public suffix (internal names such as `llm.internal` get none).
- **Wrapped text in horizontal layouts** gets explicit widths: Slint otherwise gives it a one-line height.
- **Separators** are one physical pixel (Android 0.5 dp): the software renderer drops sub-pixel lines.
- **Monospace**: Slint has no generic family name; Rust sets Consolas on Windows, `monospace` on Linux.
- **rustfmt** uses `use_small_heuristics = "Max"` with `max_width = 120`, so lines wrap only past 120 columns.
- **deny.toml** also allows BSL-1.0 (needed by `clipboard-win` / `error-code`) and CDLA-Permissive-2.0 (needed by
  `webpki-roots`); both permissive.
- **Not done in M1, by design**: Global Shortcut row and Shortcut section (M2), download group (M2), model catalogues
  and refresh buttons (M3), Statistics and Backup pages (M4), update status in the footer (M4), Rust libraries licence
  text (M4), close-to-tray (M4: close quits now).
- **Small visual differences from Android**: the dropdown panel fades in (no scale-Y, no fade-out: Slint closes popups
  at once); the multi-line field grows without the 10-line cap; text line heights use the font's natural height.

## M2 decisions
- **Provider interface**: `SpeechToTextProvider` returns boxed futures (dyn-compatible); a session's audio is an
  `AudioSource` the provider pulls. The first pull opens the microphone, so a missing model (or, in M3, key) never
  opens it. Recording, VAD gate, keep-alive and Audio Speed run inside that pull (no separate pipeline thread; the
  Silero call is ~1 ms per 32 ms frame). Errors are `SttError(String)` with the user-facing text.
- **Controller** (`iris-core::dictation::controller`): Android's states, hold times and errors. A generation counter
  under one lock drops late updates from a cancelled session. A panicking session ends in "Something went wrong".
  Not ported yet: cleanup warm-up (M3) and statistics hooks (M4). The recording ends when the session drops the
  capture receiver; the capture thread then closes the microphone on its own, so Done can show before the
  microphone is released.
- **Coordinator**: Handy's machine for Iris's one transcribe shortcut. The app calls `notify_processing_finished` on
  every Done/Error and `notify_cancel(true)` when a listening session is cancelled. Cancel (✕, Esc shortcut,
  `--cancel`, IPC) only acts while Listening; finalizing can't be cancelled.
- **Cancel shortcut** is registered when a session enters Listening and removed when it leaves it.
- **On-device provider** (`iris-core::stt::local::provider`): Android's per-phrase / streaming logic with the engine
  injected as closures, so it is tested in iris-core. Parakeet Unified streams on desktop (catalogue
  `streaming: true`). Stream progress is sent once per feed (changed or not) so the provider's undecoded-audio count
  stays exact. A stream that can't finalize, or finalizes blank, is transcribed again in one run (Handy's fallback).
- **No tail padding**: checked with Nemotron on a WAV cut 50 ms after the last word: transcribe-cpp's `finalize`
  decodes the tail (the full sentence came back), so Android's 1.3 s padding is not added.
- **Engine** (`iris::local::engine`): one `EngineSupervisor::new(true)` for the app. Accelerator `cpu` loads with
  `Backend::Cpu`, which makes the supervisor start a CPU-only worker (the effect PLAN describes as
  `EngineSupervisor::new(false)`), and a change reloads on the next use; `auto` also clears an earlier GPU failure
  (Handy's `retry_gpu` on an accelerator change). The supervisor files are Handy's, verbatim apart from `IRIS_*`
  names and comments; its unused parts (cancel, device listing) stay, with `allow(dead_code)`.
- **Idle unload**: Android's `idleReleaseWaitMs` (ported with its tests) on a thread woken by state changes and
  checking at least every 10 s; "idle" = no session listening or finalizing. Immediately unloads as soon as a
  session ends.
- **Model loading notice**: the engine publishes a `loading` flag; the overlay shows "Loading model..." when a load
  during Listening takes over 2 s (Handy).
- **Silero VAD**: runs in the app process on CPU-only transcribe.cpp backends; the GGUF ships in the binary
  (`include_bytes!`) and is written to `<data>/vad/` when missing or different. A model that can't load makes the
  gate fail open.
- **Capture**: handy-recorder, opened per session at the provider's rate, 50 ms chunks, mixed to mono; levels from
  Handy's visualizer (16 buckets, 400–4000 Hz); the overlay draws the first 9 (Handy's choice of low buckets).
  Errors: permission denied (Windows) → "Windows blocked the microphone. Allow it in Settings > Privacy >
  Microphone.", device busy → "Couldn't start recording. Is another app using the microphone?", anything else →
  "Couldn't open the microphone".
- **Cloud providers until M3**: picking one makes every dictation end with "Cloud providers aren't available yet.
  Pick On device." (new text, removed in M3).
- **Single instance**: `interprocess` local socket. Linux: `$XDG_RUNTIME_DIR/iris-desktop.sock` (a stale file from a
  crash is overwritten), else the abstract `iris-desktop-<uid>.sock`; connections from another uid are dropped.
  Windows: the pipe `iris-desktop-<USERNAME>-<session id>.sock`. Each connection is read on its own thread (one
  256-byte line, 1 s timeout on Linux). A software-renderer relaunch skips forwarding and retries binding for 1 s
  while the failed instance exits; if binding still fails and a running instance answers, this one exits.
- **`--start-hidden`** is accepted but the window shows until the tray exists (M4).
- **`-f/--transcribe-file`** runs the on-device provider (streaming or per phrase, as in a dictation) on a 16 kHz
  mono 16-bit WAV in 50 ms chunks, without the speech gate, and prints the text. Language comes from the settings.
- **Downloads**: one GGUF per model, the store from Android with Handy's transport (see the store's module docs);
  error strings verbatim from Android except one deliberate desktop change: "Tap Download to resume." is "Click
  Download to resume.". A complete `.tmp` is verified without a request; a bad 206/416 restarts once from zero.
- **Download client**: HTTP/1.1, 15 s connect timeout (Handy's), shared by the store.
- **Shortcut field**: key caps from the handy-keys binding (`ctrl_left` → "Left Ctrl"); recording uses handy-keys'
  listener (a combination is taken on key release, Handy's capture rules); Esc or losing focus cancels; the trash
  resets the default. New error text for a binding handy-keys can't use on this platform: "This shortcut doesn't work
  here. Pick another one." `InUse` can only mean Iris's own other binding (handy-keys can't see other apps').
- **Paste**: on a failed paste the transcript stays on the clipboard (no restore) and the notice "Couldn't paste. The
  text is on the clipboard." shows (Handy restores; PLAN keeps the text). Only when the text couldn't be put on the
  clipboard either (wl-copy, then arboard on Wayland) does the notice say so and carry the text: "Couldn't paste or
  copy. Your text: …". Wayland never falls back to enigo (it would only reach XWayland apps). On Wayland the clipboard
  is saved as text when a text type is offered, else as the bytes of the first offered type (e.g. an image), restored
  with `wl-copy --type`; if it can't be read, the transcript is left on it rather than the clipboard being cleared.
- **Linux paste tools**: text always goes on stdin, never in argv (`wl-copy --type text/plain`, `xdotool type --file
  -`, `ydotool type --file -`; ydotool 0.1.8 and 1.0.x both read `--file -`, so the key-syntax probe doesn't matter
  for typing). A typing failure isn't retried with argv: the text goes down the clipboard path. Every tool has a
  deadline (2 s for the clipboard and keys, 3 s for the ydotool probe, 5 s + 50 ms per character for typing) after
  which it is killed and counts as failed. wl-copy's stdout/stderr are always null (its daemon inherits them). Without
  a trusted ydotoold socket (`$YDOTOOL_SOCKET`, or one owned by root or the user), `YDOTOOL_SOCKET` points at a path
  that can't exist, so ydotool can't reach another user's daemon and the paste falls back to the clipboard.
- **udev rule** (`packaging/linux/70-iris-input.rules`, also installed by the in-app pkexec button): uaccess gives the
  active seat user read access to keyboard event devices only (`ID_INPUT_KEYBOARD=1`, for the global shortcut) and
  access to `/dev/uinput` to create virtual input devices (ydotoold run as the user, for Wayland paste). Narrower than
  handy-keys' suggested uaccess rule (every `event*` device: mice, touchpads, tablets too, then a full `udevadm
  trigger`) and the `input` group in Handy's and handy-keys' manual setup (every input device, permanently, whoever is
  at the seat). The installer re-triggers only keyboards and uinput.
- **Overlay**: see the module docs in `crates/iris-overlay` and `crates/iris/src/overlay`. Click-through outside the
  card (Windows window region, X11 input shape); the layer-shell surface spans the output width with the input region
  on the card and uses integer buffer scale only. Windows text scale is 1 (Slint doesn't follow the text-size
  setting). The helper is respawned at most once per session.
- **Dependencies added**: see `docs/third-party.md`. `smithay-client-toolkit` is pinned to 0.19 (winit's copy).

## M3 decisions
- **Layout**: providers in `iris-core::stt::{openai,vercel,deepgram,soniox}` and `iris-core::cleanup::{protocol,
  vercel,openrouter,compatible}`, ported from the Kotlin files of the same names with their tests; shared plumbing
  in `iris-core::net` (one `reqwest::Client` for all providers, size-capped response reads, WebSocket connect with
  headers and capped messages, keychain reads off the async workers, the 3 min warm-up throttle). The app's provider
  choice is `iris::providers::Providers` (Android `AppContainer`: a plain `if`/`match` on the settings), not
  `app.rs` (there is no `app.rs`; `ui/mod.rs` builds the app).
- **No real-provider verification**: no API keys exist on the development machine. Every provider is tested against
  local mock servers only; the real checks are in `docs/manual-checks.md`. The two public catalogues were fetched once
  and parse (see `docs/providers.md`).
- **TLS for WebSockets**: tokio-tungstenite with rustls and the OS verifier (`rustls-platform-verifier`, the same
  verifier and aws-lc-rs provider reqwest uses); its bundled webpki roots only if the OS verifier can't be set up.
- **Warm-up**: the controller warms the session's speech-to-text provider when a session starts, and the cleanup
  provider at start and stop (Android's controller hooks). WebSocket providers have no warm-up (the handshake can't
  reuse a reqwest connection); upload providers send Android's keyless HEAD (3 min throttle), so the upload after the
  stop finds a warm connection. The on-device model is warmed at app start and when the on-device model or language
  changes (Android warms while its bubble shows), except with Unload Model "Immediately".
- **Timeouts**: OkHttp's read/call timeouts are per-request overall timeouts. Uploads get a send allowance of 10 s +
  the audio's duration before the STT timeout (Kotlin's read timeout starts after the body is sent). Only OpenAI's
  upload distinguishes a send timeout ("Timed out sending audio to OpenAI") from a server one; Deepgram and Gateway
  uploads always say "Timed out waiting for the transcript", Soniox's upload "Timed out sending audio to Soniox".
- **Pings**: every 30 s; a ping whose pong hasn't arrived by the next one fails the socket with OkHttp's message
  ("Network error: sent ping but didn't receive pong within 30000ms"). Writes: a WebSocket frame that can't be
  written within 10 s fails the socket (OkHttp's write timeout), all four providers.
- **Keys**: a key that can't be sent as a header (control or non-ASCII characters) fails before the microphone opens
  with "Invalid <Provider> API key" (new text; OkHttp would throw). A keychain read failure during a session or a
  model-list load shows "Couldn't read the keychain."; the API key fields show the same instead of "Not set" when the
  start probe couldn't read a key. The same rule applies to the cleanup providers (and the custom provider's model
  list): an unsendable key fails with "Invalid <Provider> API key" before any request.
- **Redirects**: provider calls follow redirects only within the same origin (up to 10); a redirect to another host
  fails with "Network error: redirect to another host refused". Stricter than OkHttp, so a key never goes to another
  host. The model download and favicon clients keep reqwest's default policy.
- **Custom cleanup providers**: the base URL is HTTPS-only at request time too (not only in the form): an http URL
  fails with "<name>: Base URL must start with https://" (new text) and no request.
- **Cleanup notices** (Android `doneNotice`, desktop paste): a timeout with a successful paste → "Cleanup timeout.
  Pasted original text."; another failure → the error, then "Inserted the original text" on the next line; a failed
  paste after a cleanup failure → the error, then "Couldn't paste. The text is on the clipboard.". Provider options
  that no longer parse → "Cleanup
  failed".
- **Model menus**: loaded as on Android (`ModelCatalogs`, `SettingsApp` effects) while the page's provider needs them;
  a failed load waits for the refresh button (Android retried when the selection changed). The refresh icon spins
  only with femtovg (the software renderer doesn't rotate images). Soniox's catalogue is app-wide and reloads at start,
  on a Soniox key change and on a region change.
- **Cancel**: a per-phrase on-device session that ends without its text (cancelled, failed, timed out) calls
  `EngineSupervisor::cancel`, which kills the worker only if a decode is running (the next use reloads the model, as
  in Handy). Streaming sessions already stop by dropping the engine stream.
- **Keychain probe race**: the start probe is repeated if a key was saved or cleared while it ran.
- **Not ported**: statistics hooks (M4; `timed_cleanup` keeps its `record` argument for them); Kotlin tests that
  check OkHttp internals (connection reuse, `EventListener`) or WebSocket warm-ups (see the M3 report).
- **Dependencies added**: see `docs/third-party.md`.

