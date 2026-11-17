/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Vercel AI Gateway speech-to-text: streaming WebSocket and upload.
//! [`protocol`] holds the frames, parts, options and catalogue predicates both share.

mod file;
pub mod protocol;
mod streaming;

pub use file::VercelGatewayFileTranscription;
pub use streaming::VercelGatewaySpeechToText;
