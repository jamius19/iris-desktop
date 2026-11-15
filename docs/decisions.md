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
