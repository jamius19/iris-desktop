/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Audio processing for one dictation: the VAD gate ([speech_gate]), its 24 → 16 kHz downsampler ([resample]),
//! Sonic speed-up ([time_stretch]) and the overlay's FFT levels ([visualizer]).

pub mod resample;
pub mod speech_gate;
pub mod time_stretch;
pub mod visualizer;
