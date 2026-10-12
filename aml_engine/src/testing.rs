//! In-memory audio and MIDI endpoints for hardware-free tests.
//!
//! Available under `cfg(test)` and with the `testing` feature.

use crate::audio::{AudioSink, AudioSource, OpenAudioSink, OpenAudioSource};
use crate::midi::{MidiCallback, MidiInputGuard, MidiSink, MidiSource};
use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

type Pipe = Arc<Mutex<VecDeque<f32>>>;

/// An in-memory audio cable: whatever the [`MemorySink`] receives, the
/// [`MemorySource`] delivers, in order, as fast as it is pulled (there is no
/// real-time clock and no silence is inserted when the pipe runs dry).
pub struct MemoryAudio;

impl MemoryAudio {
    /// A connected `(sink, source)` pair at `sample_rate`; the sink reports
    /// full once `capacity` samples are waiting.
    pub fn pair(sample_rate: u32, capacity: usize) -> (MemorySink, MemorySource) {
        let pipe: Pipe = Arc::new(Mutex::new(VecDeque::new()));
        (
            MemorySink {
                pipe: pipe.clone(),
                sample_rate,
                capacity,
            },
            MemorySource { pipe, sample_rate },
        )
    }
}

/// Write end of [`MemoryAudio`]. Use via `AudioOut::Custom(Box::new(sink))`.
pub struct MemorySink {
    pipe: Pipe,
    sample_rate: u32,
    capacity: usize,
}

impl AudioSink for MemorySink {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> usize {
        1
    }
    fn start(&mut self) -> Result<()> {
        Ok(())
    }
    fn push(&mut self, samples: &[f32]) -> usize {
        let mut q = self.pipe.lock().unwrap();
        let n = samples.len().min(self.capacity.saturating_sub(q.len()));
        q.extend(&samples[..n]);
        n
    }
    fn is_full(&self) -> bool {
        self.pipe.lock().unwrap().len() >= self.capacity
    }
    fn buffered(&self) -> usize {
        self.pipe.lock().unwrap().len()
    }
}

impl OpenAudioSink for MemorySink {
    fn open(
        self: Box<Self>,
        _preferred_rate: u32,
        _buffer_samples: usize,
    ) -> Result<Box<dyn AudioSink>> {
        Ok(self)
    }
}

/// Read end of [`MemoryAudio`]. Use via `AudioIn::Custom(Box::new(source))`.
pub struct MemorySource {
    pipe: Pipe,
    sample_rate: u32,
}

impl AudioSource for MemorySource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> usize {
        1
    }
    fn start(&mut self) -> Result<()> {
        Ok(())
    }
    fn pull(&mut self, out: &mut [f32]) -> usize {
        let mut q = self.pipe.lock().unwrap();
        let n = out.len().min(q.len());
        for (dst, src) in out.iter_mut().zip(q.drain(..n)) {
            *dst = src;
        }
        n
    }
}

impl OpenAudioSource for MemorySource {
    fn open(
        self: Box<Self>,
        _preferred_rate: u32,
        _buffer_samples: usize,
    ) -> Result<Box<dyn AudioSource>> {
        Ok(self)
    }
}

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

/// Records every MIDI message sent to it. Clones share the same record.
#[derive(Clone, Default)]
pub struct RecordingMidiSink {
    messages: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl RecordingMidiSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of everything received so far.
    pub fn messages(&self) -> Vec<Vec<u8>> {
        self.messages.lock().unwrap().clone()
    }
}

impl MidiSink for RecordingMidiSink {
    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.messages.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
}

type SharedCallback = Arc<Mutex<Option<MidiCallback>>>;

/// A MIDI input fed from test code through its [`MemoryMidiInjector`].
pub struct MemoryMidiInput {
    callback: SharedCallback,
}

/// Injects raw MIDI into a connected [`MemoryMidiInput`].
#[derive(Clone)]
pub struct MemoryMidiInjector {
    callback: SharedCallback,
}

impl MemoryMidiInput {
    pub fn new() -> (MemoryMidiInput, MemoryMidiInjector) {
        let callback: SharedCallback = Arc::new(Mutex::new(None));
        (
            MemoryMidiInput {
                callback: callback.clone(),
            },
            MemoryMidiInjector { callback },
        )
    }
}

impl MidiSource for MemoryMidiInput {
    fn connect(self: Box<Self>, on_message: MidiCallback) -> Result<MidiInputGuard> {
        *self.callback.lock().unwrap() = Some(on_message);
        Ok(MidiInputGuard::new(DisconnectOnDrop(self.callback)))
    }
}

struct DisconnectOnDrop(SharedCallback);

impl Drop for DisconnectOnDrop {
    fn drop(&mut self) {
        if let Ok(mut cb) = self.0.lock() {
            *cb = None;
        }
    }
}

impl MemoryMidiInjector {
    /// Deliver `bytes` as an incoming MIDI message. Returns false if the
    /// input is not (or no longer) connected.
    pub fn inject(&self, bytes: &[u8]) -> bool {
        match self.callback.lock().unwrap().as_mut() {
            Some(cb) => {
                cb(bytes);
                true
            }
            None => false,
        }
    }
}
