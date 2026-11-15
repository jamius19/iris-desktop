/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod favicons;
mod services;
mod ui;

use std::fs::{self, File};
use std::process::Command;

use directories::ProjectDirs;
use log::LevelFilter;
use simplelog::{ColorChoice, CombinedLogger, Config, TermLogger, TerminalMode, WriteLogger};

slint::include_modules!();

fn main() {
    init_logging();
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("Panic: {info}");
        default(info);
    }));
    if let Some(reason) = std::env::var_os("IRIS_RENDERER_FALLBACK") {
        log::warn!("Using the software renderer: femtovg failed ({})", reason.to_string_lossy());
    }
    let femtovg = std::env::var_os("SLINT_BACKEND").is_none();
    select_backend();
    if let Err(e) = ui::run() {
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

/// Logs to `<data>/logs/iris.log` (truncated per run) and warnings to stderr. Never log keys or transcripts.
fn init_logging() {
    let mut loggers: Vec<Box<dyn simplelog::SharedLogger>> =
        vec![TermLogger::new(LevelFilter::Warn, Config::default(), TerminalMode::Stderr, ColorChoice::Auto)];
    let dir = project_dirs().data_local_dir().join("logs");
    if let Ok(file) = fs::create_dir_all(&dir).and_then(|_| File::create(dir.join("iris.log"))) {
        loggers.push(WriteLogger::new(LevelFilter::Info, Config::default(), file));
    }
    let _ = CombinedLogger::init(loggers);
}

/// winit with femtovg; `SLINT_BACKEND` overrides it. Selecting creates no GL context, so a femtovg that can't render
/// (e.g. no GL) isn't caught here but fails `ui::run`, and `main` relaunches the app with
/// `SLINT_BACKEND=winit-software`. The software selection below only covers a backend that fails to initialise.
fn select_backend() {
    if std::env::var_os("SLINT_BACKEND").is_some() {
        return;
    }
    let femtovg = slint::BackendSelector::new().backend_name("winit".into()).renderer_name("femtovg".into()).select();
    if let Err(e) = femtovg {
        log::warn!("femtovg renderer unavailable ({e}); using the software renderer");
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .select()
            .expect("no Slint backend");
    }
}

pub fn project_dirs() -> ProjectDirs {
    ProjectDirs::from("dev", "jamius", "Iris").expect("no home directory")
}
