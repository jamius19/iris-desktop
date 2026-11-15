# Iris desktop – project guide

Rust + Slint voice dictation app for Windows 10+ and Linux, recreating the Android app Iris
(`C:\Users\jamius19\projects\iris`) with desktop mechanisms from Handy (`C:\Users\jamius19\projects\Handy`, MIT).
`PLAN.md` is the plan (milestones M1–M4); `docs/decisions.md` holds decisions and spike results.

Status: M1 implemented (shell, design system, settings pages, persistence); `cargo deny check licenses` not yet run
(cargo-deny not installed). M2–M4 not started.

## Commands (Git Bash, repo root)
```
cargo run -p iris                          # the app (femtovg; SLINT_BACKEND=winit-software for the software renderer)
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check licenses                  # needs cargo-deny (not installed on the dev machine yet)
cargo xtask assets                         # regenerate assets/icons/{lucide,brand,app}; outputs are committed
```
Linux check from Windows: WSL `Ubuntu` with `CARGO_TARGET_DIR=$HOME/.cache/iris-desktop-target cargo build -p iris`.

## Layout
- `crates/iris-core`: UI-free logic and its tests. `settings/` (model = Android `Settings.kt` + `toSettings`,
  store = typed JSON file in Android's backup encoding, secrets = keyring), `copy.rs` (Android `SettingsText.kt`,
  desktop wording), `ui_math.rs` (dropdown placement, slider math, autosave commit), `cleanup/`, `stt/`.
- `crates/iris`: the binary. `ui/*.slint` (design system in `ui/theme.slint`, `ui/components/`, pages in
  `ui/pages/`, globals in `ui/globals.slint`), `src/ui/*.rs` (glue: fills `Prefs` from settings, dispatches setters by
  settings key), `src/services.rs` (Home checks), `src/favicons.rs`.
- `xtask`: dev-only asset generation. `assets/`: fonts, icons, licence texts. `docs/`: decisions, manual checks,
  third-party list.

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
- Running the app writes `%APPDATA%\jamius\Iris` / `%LOCALAPPDATA%\jamius\Iris` (Linux: `~/.config/iris`,
  `~/.local/share/iris`, `~/.cache/iris`).
