/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Launch at Startup: an `auto-launch` login item named "Iris" that runs `<path> --start-hidden`, or `<path> ` with
//! Start Hidden on System Start off.
//! Windows: HKCU `...\CurrentVersion\Run` value `Iris` (the NSIS uninstaller deletes it by that name); Linux:
//! `~/.config/autostart/Iris.desktop`. The path is the running executable, or the AppImage it runs from (`$APPIMAGE`).

use auto_launch::{AutoLaunch, AutoLaunchBuilder};
use iris_core::settings::Settings;

const NAME: &str = "Iris";
const HIDDEN: &str = "--start-hidden";

/// The Launch at Startup switch's error.
pub const ERROR: &str = "Couldn't update the startup entry.";

/// Makes the login item match the settings: (re)registers it when on and missing or different (the app moved, Start
/// Hidden on System Start changed), removes it when off and present. Leaves an entry the user disabled in Task
/// Manager alone.
pub fn apply(settings: &Settings) -> auto_launch::Result<()> {
    let path = app_path()?;
    let args: &[&str] = if settings.start_hidden { &[HIDDEN] } else { &[] };
    // As auto-launch writes it: a space after the path even without arguments.
    let command = format!("{path} {}", args.join(" "));
    let stored = stored_command();
    if settings.launch_at_startup && stored.as_deref() != Some(command.as_str()) {
        log::info!("Registering the login item");
        launcher(&path, args)?.enable()
    } else if !settings.launch_at_startup && stored.is_some() {
        log::info!("Removing the login item");
        launcher(&path, args)?.disable()
    } else {
        Ok(())
    }
}

fn launcher(path: &str, args: &[&str]) -> auto_launch::Result<AutoLaunch> {
    let mut builder = AutoLaunchBuilder::new();
    builder.set_app_name(NAME).set_app_path(path).set_args(args);
    #[cfg(windows)]
    builder.set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser);
    builder.build()
}

/// The quoted path as it appears in the command (auto-launch writes `<path> <args>` and quotes nothing).
fn app_path() -> std::io::Result<String> {
    let path = match std::env::var_os("APPIMAGE").filter(|_| cfg!(target_os = "linux")) {
        Some(appimage) => appimage.into(),
        None => std::env::current_exe()?,
    };
    Ok(format!("\"{}\"", path.display()))
}

#[cfg(windows)]
fn stored_command() -> Option<String> {
    windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Run")
        .and_then(|key| key.get_string(NAME))
        .ok()
}

/// The `Exec=` value of auto-launch's desktop entry (`$HOME/.config/autostart`, not `$XDG_CONFIG_HOME`).
#[cfg(not(windows))]
fn stored_command() -> Option<String> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    let entry = std::fs::read_to_string(home.join(".config/autostart").join(format!("{NAME}.desktop"))).ok()?;
    entry.lines().find_map(|line| line.strip_prefix("Exec=")).map(str::to_string)
}
