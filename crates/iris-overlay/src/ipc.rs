/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The app ↔ `iris-overlay` helper protocol (`docs/architecture.md`, IPC): frames of a u32 little-endian length and
//! JSON, over the helper's stdin (app → helper) and stdout (helper → app). The helper exits on stdin EOF.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{BARS, OverlayConfig, OverlayState};

/// Larger frames are refused (a corrupt length would otherwise allocate gigabytes).
const MAX_FRAME: usize = 1 << 20;

/// App → helper.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToHelper {
    Config(OverlayConfig),
    State(OverlayState),
    Levels([f32; BARS]),
    Hide,
}

/// How the helper draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    LayerShell,
    X11,
}

/// Helper → app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FromHelper {
    Ready { mode: Mode },
    CancelClicked,
    NoticeClicked,
    Failed(String),
}

/// Writes one frame and flushes.
pub fn write_frame(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let json = serde_json::to_vec(message)?;
    if json.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
    }
    writer.write_all(&(json.len() as u32).to_le_bytes())?;
    writer.write_all(&json)?;
    writer.flush()
}

/// Reads one frame; `None` on a clean EOF before a frame starts.
pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<Option<T>> {
    let mut length = [0u8; 4];
    let mut filled = 0;
    while filled < length.len() {
        match reader.read(&mut length[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut json = vec![0u8; length];
    reader.read_exact(&mut json)?;
    Ok(Some(serde_json::from_slice(&json)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Phase, Position};

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(messages: &[T]) {
        let mut bytes = Vec::new();
        for message in messages {
            write_frame(&mut bytes, message).unwrap();
        }
        let mut reader = bytes.as_slice();
        for message in messages {
            assert_eq!(Some(message), read_frame::<T>(&mut reader).unwrap().as_ref());
        }
        assert_eq!(None, read_frame::<T>(&mut reader).unwrap());
    }

    #[test]
    fn app_messages_round_trip() {
        round_trip(&[
            ToHelper::Config(OverlayConfig { position: Position::Top, lines: 4, max_width_pct: 70, dark: true }),
            ToHelper::State(OverlayState {
                phase: Phase::Listening,
                text: "Grüße, naïve café".into(),
                session: 3,
                loading_model: false,
            }),
            ToHelper::Levels([0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 1.0]),
            ToHelper::Hide,
        ]);
    }

    #[test]
    fn helper_messages_round_trip() {
        round_trip(&[
            FromHelper::Ready { mode: Mode::LayerShell },
            FromHelper::Ready { mode: Mode::X11 },
            FromHelper::CancelClicked,
            FromHelper::NoticeClicked,
            FromHelper::Failed("no display".into()),
        ]);
    }

    #[test]
    fn frame_is_length_prefixed_json() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &ToHelper::Hide).unwrap();
        assert_eq!(&bytes[..4], &6u32.to_le_bytes());
        assert_eq!(&bytes[4..], b"\"Hide\"");
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &FromHelper::CancelClicked).unwrap();
        bytes.pop();
        assert!(read_frame::<FromHelper>(&mut bytes.as_slice()).is_err());
        // A partial length prefix is an error, not EOF.
        assert!(read_frame::<FromHelper>(&mut &[1u8, 0][..]).is_err());
    }

    #[test]
    fn oversized_frame_is_refused() {
        let bytes = (MAX_FRAME as u32 + 1).to_le_bytes();
        let error = read_frame::<FromHelper>(&mut &bytes[..]).unwrap_err();
        assert_eq!(io::ErrorKind::InvalidData, error.kind());
    }
}
