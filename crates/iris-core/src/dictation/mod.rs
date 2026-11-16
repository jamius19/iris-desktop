/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Dictation helpers: the hold-or-toggle input machine ([coordinator]) and transcript post-processing (filler words,
//! their language, custom words).

pub mod controller;
pub mod coordinator;
pub mod custom_words;
pub mod filler_words;
pub mod language;
pub mod state;

pub use state::DictationState;
