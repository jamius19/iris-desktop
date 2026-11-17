/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Scripted audio for the Deepgram provider tests (the Kotlin tests' `flow { ... }`).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::time::Instant;

use crate::stt::{AudioSource, BoxFuture, SttError};

/// One step of a scripted audio flow.
pub(super) enum Step {
    Chunk(Vec<u8>),
    Sleep(u64),
    /// Waits until the condition holds.
    Until(Box<dyn Fn() -> bool + Send + Sync>),
    Fail(&'static str),
    /// Never ends (Kotlin's `awaitCancellation`).
    Pending,
}

/// Audio that runs its steps as the provider pulls, then ends. Cancel-safe like the controller's audio: a step is
/// only taken once it has finished, so a dropped `next` loses nothing.
pub(super) struct Script {
    steps: VecDeque<Step>,
    /// When the current [`Step::Sleep`] ends.
    sleep_until: Option<Instant>,
    /// Set once the provider pulls audio (Kotlin's `collected`).
    pub pulled: Arc<AtomicBool>,
}

pub(super) fn script(steps: Vec<Step>) -> Script {
    Script { steps: steps.into(), sleep_until: None, pulled: Arc::default() }
}

/// Audio of just `chunks`.
pub(super) fn chunks(chunks: &[&[u8]]) -> Script {
    script(chunks.iter().map(|chunk| Step::Chunk(chunk.to_vec())).collect())
}

impl AudioSource for Script {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        self.pulled.store(true, Ordering::SeqCst);
        Box::pin(async move {
            while let Some(step) = self.steps.front() {
                match step {
                    Step::Chunk(_) | Step::Fail(_) => break,
                    Step::Sleep(ms) => {
                        let ms = *ms;
                        let until = *self.sleep_until.get_or_insert_with(|| Instant::now() + Duration::from_millis(ms));
                        tokio::time::sleep_until(until).await;
                        self.sleep_until = None;
                    }
                    Step::Until(condition) => {
                        while !condition() {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }
                    Step::Pending => std::future::pending().await,
                }
                self.steps.pop_front();
            }
            match self.steps.pop_front() {
                Some(Step::Chunk(chunk)) => Ok(Some(chunk)),
                Some(Step::Fail(message)) => Err(SttError(message.into())),
                _ => Ok(None),
            }
        })
    }
}
