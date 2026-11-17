/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The dictation session's state (Android `DictationController.kt`).

/// `Idle → Listening(text) → Finalizing(text) → Done(text, notice?) | Error → Idle`.
#[derive(Clone, Debug, PartialEq)]
pub enum DictationState {
    Idle,
    Listening { text: String },
    Finalizing { text: String },
    Done { text: String, notice: Option<String> },
    Error { message: String },
}
