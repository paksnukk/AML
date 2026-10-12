//! Endpoints for hardware-free tests: the in-memory ones from
//! [`crate::memory`], plus endpoints that fail on purpose.
//!
//! Available under `cfg(test)` and with the `testing` feature.

use crate::audio::{AudioSink, AudioSource, OpenAudioSink, OpenAudioSource};
use anyhow::{anyhow, Result};

pub use crate::memory::*;

/// An audio input that fails to open (exercises the error path).
pub struct FailingAudio(pub String);

impl OpenAudioSource for FailingAudio {
    fn open(self: Box<Self>, _: u32, _: usize) -> Result<Box<dyn AudioSource>> {
        Err(anyhow!("{}", self.0))
    }
}

impl OpenAudioSink for FailingAudio {
    fn open(self: Box<Self>, _: u32, _: usize) -> Result<Box<dyn AudioSink>> {
        Err(anyhow!("{}", self.0))
    }
}
