/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Dictation helpers: the hold-or-toggle input machine ([coordinator]), transcript post-processing (filler words,
//! their language, custom words) and [statistics] and [history].

pub mod controller;
pub mod coordinator;
pub mod custom_words;
pub mod filler_words;
pub mod history;
pub mod language;
pub mod state;
pub mod statistics;

pub use state::DictationState;
