//! In-memory audio and MIDI endpoints: the hardware-free loopback used by
//! [`crate::loopback`] (CLI `loopback`, GUI self-test) and by tests.

use crate::audio::{AudioSink, AudioSource, DeviceErrorQueue, OpenAudioSink, OpenAudioSource};
use crate::midi::{MidiCallback, MidiInputGuard, MidiSink, MidiSource};
use anyhow::Result;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

type Pipe = Arc<Mutex<VecDeque<f32>>>;

/// An in-memory audio cable: whatever the [`MemorySink`] receives, the
/// [`MemorySource`] delivers, in order, as fast as it is pulled (there is no
/// real-time clock and no silence is inserted when the pipe runs dry). A pipe
/// that runs dry therefore behaves like a stalled audio device.
///
/// Each end has a [`DeviceErrorQueue`] (see `errors()`) through which tests
/// inject device errors, as cpal's error callback would.
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
                errors: DeviceErrorQueue::new(),
            },
            MemorySource {
                pipe,
                sample_rate,
                errors: DeviceErrorQueue::new(),
            },
        )
    }
}

/// Write end of [`MemoryAudio`]. Use via `AudioOut::Custom(Box::new(sink))`.
pub struct MemorySink {
    pipe: Pipe,
    sample_rate: u32,
    capacity: usize,
    errors: DeviceErrorQueue,
}

impl MemorySink {
    /// Handle for injecting device errors (reported via `take_error`).
    pub fn errors(&self) -> DeviceErrorQueue {
        self.errors.clone()
    }
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
    fn take_error(&mut self) -> Option<String> {
        self.errors.pop()
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
    errors: DeviceErrorQueue,
}

impl MemorySource {
    /// Handle for injecting device errors (reported via `take_error`).
    pub fn errors(&self) -> DeviceErrorQueue {
        self.errors.clone()
    }

    /// Same pipe, but samples are delivered at real-time rate when pulled.
    pub fn into_paced(self) -> PacedMemorySource {
        PacedMemorySource {
            pipe: self.pipe,
            sample_rate: self.sample_rate,
            clock: Instant::now(),
            delivered: 0,
            errors: self.errors,
        }
    }
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
    fn take_error(&mut self) -> Option<String> {
        self.errors.pop()
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

/// Like [`MemorySource`], but releases samples only as wall-clock time × sample
/// rate allows (real-time pacing for loopback latency tests).
pub struct PacedMemorySource {
    pipe: Pipe,
    sample_rate: u32,
    clock: Instant,
    delivered: u64,
    errors: DeviceErrorQueue,
}

impl AudioSource for PacedMemorySource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> usize {
        1
    }
    fn start(&mut self) -> Result<()> {
        self.clock = Instant::now();
        self.delivered = 0;
        Ok(())
    }
    fn pull(&mut self, out: &mut [f32]) -> usize {
        // Silence must not accumulate playback credit. A packet pushed after
        // an idle stretch is then released over its own duration, which is
        // the airtime a real device would take.
        if self.pipe.lock().unwrap().is_empty() {
            self.clock = Instant::now();
            self.delivered = 0;
            return 0;
        }
        let elapsed = self.clock.elapsed().as_secs_f64();
        let target = (elapsed * self.sample_rate as f64).floor() as u64;
        let budget = target.saturating_sub(self.delivered) as usize;
        if budget == 0 {
            return 0;
        }
        let mut q = self.pipe.lock().unwrap();
        let n = out.len().min(budget).min(q.len());
        for (dst, src) in out.iter_mut().zip(q.drain(..n)) {
            *dst = src;
        }
        self.delivered += n as u64;
        n
    }
    fn take_error(&mut self) -> Option<String> {
        self.errors.pop()
    }
}

impl OpenAudioSource for PacedMemorySource {
    fn open(
        self: Box<Self>,
        _preferred_rate: u32,
        _buffer_samples: usize,
    ) -> Result<Box<dyn AudioSource>> {
        Ok(self)
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
