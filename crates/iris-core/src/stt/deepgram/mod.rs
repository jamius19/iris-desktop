/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Deepgram speech-to-text: streaming WebSocket and upload.

pub mod file;
pub mod protocol;
pub mod streaming;

pub use file::DeepgramFileTranscription;
pub use streaming::DeepgramSpeechToText;

#[cfg(test)]
mod test_audio;
