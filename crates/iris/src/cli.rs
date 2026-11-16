/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Command-line flags (Handy `cli.rs` minus its benchmark and catalogue flags).

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug, Clone, Default)]
#[command(name = "iris", about = "Iris - voice dictation", version)]
pub struct CliArgs {
    /// Start with the main window hidden (needs the tray, M4; shown until then)
    #[arg(long)]
    pub start_hidden: bool,

    /// Disable the system tray icon
    #[arg(long)]
    pub no_tray: bool,

    /// Toggle transcription on/off (sent to the running instance)
    #[arg(long)]
    pub toggle_transcription: bool,

    /// Cancel the current dictation (sent to the running instance)
    #[arg(long)]
    pub cancel: bool,

    /// Verbose logging
    #[arg(long)]
    pub debug: bool,

    /// Transcribe this WAV (16 kHz mono 16-bit) with the on-device provider and print the text. The model must
    /// already be downloaded.
    #[arg(short = 'f', long, value_name = "WAV")]
    pub transcribe_file: Option<PathBuf>,

    /// On-device model id for --transcribe-file (default: the selected one)
    #[arg(long, value_name = "ID")]
    pub model: Option<String>,
}

impl CliArgs {
    /// The command for a running instance: `toggle`, `cancel`, or `show` (no action flag).
    pub fn instance_command(&self) -> &'static str {
        if self.toggle_transcription {
            "toggle"
        } else if self.cancel {
            "cancel"
        } else {
            "show"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_map_to_instance_commands() {
        let parse = |args: &[&str]| CliArgs::parse_from([&["iris"], args].concat());
        assert_eq!("toggle", parse(&["--toggle-transcription"]).instance_command());
        assert_eq!("cancel", parse(&["--cancel"]).instance_command());
        assert_eq!("show", parse(&["--debug"]).instance_command());
        let file = parse(&["-f", "a.wav", "--model", "whisper-small"]);
        assert_eq!(Some(PathBuf::from("a.wav")), file.transcribe_file);
        assert_eq!(Some("whisper-small".to_string()), file.model);
    }
}
