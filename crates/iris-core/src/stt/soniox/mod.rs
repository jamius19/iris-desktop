/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Soniox speech-to-text: real-time WebSocket, async REST jobs, regions and the model catalogue.

use std::sync::Arc;

use crate::stt::BoxFuture;

pub mod catalog;
pub mod file;
pub mod models;
pub mod protocol;
pub mod streaming;

pub use catalog::{SonioxCatalog, SonioxCatalogState, SonioxList};
pub use file::SonioxFileTranscription;
pub use models::{SonioxModel, list_soniox_models, soniox_streams};
pub use protocol::{SonioxRegion, soniox_region};
pub use streaming::SonioxSpeechToText;

/// A model's catalogue entry (None if unknown), for the strict-hints check (the app passes `SonioxCatalog::entry`).
pub type ModelInfo = Arc<dyn Fn(String) -> BoxFuture<'static, Option<SonioxModel>> + Send + Sync>;
