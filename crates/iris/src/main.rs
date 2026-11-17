/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod capture;
mod cli;
mod dictation;
mod favicons;
mod headless;
mod hotkey;
mod local;
mod overlay;
mod paste;
mod providers;
mod services;
#[cfg(target_os = "linux")]
mod signals;
mod single_instance;
mod sounds;
mod system_audio;
mod tray;
mod ui;

use std::fs::{self, File};
use std::path::PathBuf;
use std::process::Command;

use clap::Parser;
use directories::ProjectDirs;
use log::LevelFilter;
use simplelog::{ColorChoice, CombinedLogger, Config, TermLogger, TerminalMode, WriteLogger};

use crate::cli::CliArgs;

slint::include_modules!();

fn main() {
    set_vulkan_environment();
    // The transcription worker runs transcribe.cpp in isolation and never touches the UI, the CLI or the single
    // instance. After the environment setup above, so it inherits it.
    if local::supervisor::is_worker_invocation() {
        std::process::exit(local::supervisor::run_worker());
    }
    local::memory::init_allocator();
    let args = CliArgs::try_parse().unwrap_or_else(|e| {
        cli::attach_console();
        e.exit()
    });
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("Panic: {info}");
        default(info);
    }));
    // Neither the headless run nor a forwarding instance touches the running app's log file.
    if let Some(file) = &args.transcribe_file {
        cli::attach_console();
        init_logging(args.debug, false);
        std::process::exit(headless::transcribe_file(file, args.model.as_deref()));
    }
    let relaunched = std::env::var_os("IRIS_RENDERER_FALLBACK");
    // A running Iris gets the command; with none, this one starts (an action flag is then ignored, as in Handy).
    if relaunched.is_none() && single_instance::forward(args.instance_command()) {
        return;
    }
    init_logging(args.debug, true);
    if let Some(reason) = &relaunched {
        log::warn!("Using the software renderer: femtovg failed ({})", reason.to_string_lossy());
    }
    let femtovg = std::env::var_os("SLINT_BACKEND").is_none();
    select_backend();
    if let Err(e) = ui::run(args.start_hidden, args.no_tray) {
        // femtovg (e.g. no GL) once more with the software renderer; the relaunch has SLINT_BACKEND set.
        if femtovg {
            log::warn!("The app failed with femtovg ({e}); relaunching with the software renderer");
            let relaunched = std::env::current_exe().and_then(|exe| {
                Command::new(exe)
                    .args(std::env::args_os().skip(1))
                    .env("SLINT_BACKEND", "winit-software")
                    .env("IRIS_RENDERER_FALLBACK", e.to_string())
                    .spawn()
            });
            if let Err(spawn_error) = relaunched {
                log::error!("Relaunching with the software renderer failed: {spawn_error}");
                std::process::exit(1);
            }
            return;
        }
        log::error!("The app failed: {e}");
        std::process::exit(1);
    }
}

/// Handy `main.rs`: Vulkan is only used for compute. Windows: skip implicit layers (overlay/capture layers crash,
/// Handy #2049); Linux: skip the Gamescope WSI layer (aborts on SteamOS, Handy #2264). A user's own value wins.
fn set_vulkan_environment() {
    #[cfg(windows)]
    if std::env::var_os("VK_LOADER_LAYERS_DISABLE").is_none() {
        // SAFETY: first thing in `main`, before any other thread exists.
        unsafe { std::env::set_var("VK_LOADER_LAYERS_DISABLE", "~implicit~") };
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISABLE_GAMESCOPE_WSI").is_none() {
        // SAFETY: first thing in `main`, before any other thread exists.
        unsafe { std::env::set_var("DISABLE_GAMESCOPE_WSI", "1") };
    }
}

/// Logs to `<data>/logs/iris.log` (truncated per run; only with `to_file`) and warnings to stderr (`--debug`:
/// everything to both). Never log keys or transcripts.
fn init_logging(debug: bool, to_file: bool) {
    let (term, file) =
        if debug { (LevelFilter::Debug, LevelFilter::Debug) } else { (LevelFilter::Warn, LevelFilter::Info) };
    let mut loggers: Vec<Box<dyn simplelog::SharedLogger>> =
        vec![TermLogger::new(term, Config::default(), TerminalMode::Stderr, ColorChoice::Auto)];
    let dir = project_dirs().data_local_dir().join("logs");
    if to_file && let Ok(log) = fs::create_dir_all(&dir).and_then(|_| File::create(dir.join("iris.log"))) {
        loggers.push(WriteLogger::new(file, Config::default(), log));
    }
    let _ = CombinedLogger::init(loggers);
}

/// winit with femtovg; `SLINT_BACKEND` overrides it. Selecting creates no GL context, so a femtovg that can't render
/// (e.g. no GL) isn't caught here but fails `ui::run`, and `main` relaunches the app with
/// `SLINT_BACKEND=winit-software`. The software selection below only covers a backend that fails to initialise.
/// Every selector carries the overlay's winit attributes hook (`overlay::backend_selector`).
fn select_backend() {
    if std::env::var_os("SLINT_BACKEND").is_some() {
        // The selector reads `SLINT_BACKEND` itself; selecting here only adds the hook.
        if let Err(e) = overlay::backend_selector().select() {
            log::warn!("The SLINT_BACKEND backend is unavailable ({e})");
        }
        return;
    }
    let femtovg = overlay::backend_selector().backend_name("winit".into()).renderer_name("femtovg".into()).select();
    if let Err(e) = femtovg {
        log::warn!("femtovg renderer unavailable ({e}); using the software renderer");
        overlay::backend_selector()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .select()
            .expect("no Slint backend");
    }
}

pub fn project_dirs() -> ProjectDirs {
    ProjectDirs::from("dev", "jamius", "Iris").expect("no home directory")
}

/// Where downloaded models live: `<data>/models/<id>/`.
pub fn models_dir() -> PathBuf {
    project_dirs().data_local_dir().join("models")
}

/// The HTTP client for model downloads: HTTP/1.1 with a 15 s connect timeout (Handy's download client).
pub fn download_client() -> reqwest::Client {
    reqwest::Client::builder()
        .http1_only()
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("HTTP client")
}
