//! Audio I/O: the [`AudioSource`] / [`AudioSink`] traits the workers use, and
//! their cpal implementations (the single copy of the stream setup formerly
//! duplicated in the GUI and CLI).
//!
//! Workers see mono `f32` samples only. The cpal source takes channel 0 of
//! the input; the cpal sink writes the mono signal to every output channel.
//!
//! `cpal::Stream` is `!Send`, so endpoints are described by a `Send` *opener*
//! ([`AudioIn`] / [`AudioOut`]) and opened inside the worker thread, which
//! then owns the stream for its whole life.

use crate::devices::DeviceSelector;
use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Most errors a [`DeviceErrorQueue`] holds; further ones are dropped (and
/// counted) until the worker drains it.
const MAX_QUEUED_ERRORS: usize = 16;

/// Errors reported by an audio stream outside the worker thread (cpal's error
/// callback), queued until the worker polls them with `take_error`.
///
/// Pushing locks a mutex, so call it only from the stream's *error* callback
/// (or other non-real-time code), never from the audio data callback.
/// Clones share the same queue.
#[derive(Clone, Default)]
pub struct DeviceErrorQueue {
    inner: Arc<Mutex<(VecDeque<String>, u64)>>,
}

impl DeviceErrorQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an error message.
    pub fn push(&self, message: impl Into<String>) {
        if let Ok(mut q) = self.inner.lock() {
            if q.0.len() < MAX_QUEUED_ERRORS {
                q.0.push_back(message.into());
            } else {
                q.1 += 1;
            }
        }
    }

    /// Oldest unreported error. Errors dropped because the queue was full are
    /// reported as one summary message after the queued ones.
    pub fn pop(&self) -> Option<String> {
        let mut q = self.inner.lock().ok()?;
        if let Some(m) = q.0.pop_front() {
            return Some(m);
        }
        if q.1 > 0 {
            let n = std::mem::take(&mut q.1);
            return Some(format!("{} further audio device errors not shown", n));
        }
        None
    }
}

/// A live mono capture stream.
pub trait AudioSource {
    /// Actual stream sample rate (may differ from the preferred one).
    fn sample_rate(&self) -> u32;
    /// Channel count of the underlying device (informational; only ch0 is used).
    fn channels(&self) -> usize;
    /// Start delivering samples.
    fn start(&mut self) -> Result<()>;
    /// Non-blocking: copy up to `out.len()` buffered samples into `out` and
    /// return how many were written (0 when nothing is buffered).
    fn pull(&mut self, out: &mut [f32]) -> usize;
    /// Samples lost because the capture buffer was full.
    fn dropped(&self) -> u64 {
        0
    }
    /// Next error the device reported since the last call (e.g. the stream
    /// was invalidated because the device was unplugged). Polled by the
    /// decoder worker and reported as `AlarmKind::AudioDeviceError`.
    fn take_error(&mut self) -> Option<String> {
        None
    }
}

/// A live mono playback stream.
pub trait AudioSink {
    /// Actual stream sample rate (may differ from the preferred one).
    fn sample_rate(&self) -> u32;
    /// Channel count of the underlying device (the signal goes to all of them).
    fn channels(&self) -> usize;
    /// Start playback.
    fn start(&mut self) -> Result<()>;
    /// Non-blocking: queue as many of `samples` as fit and return that count.
    fn push(&mut self, samples: &[f32]) -> usize;
    /// True when no further sample can be queued right now.
    fn is_full(&self) -> bool;
    /// Samples queued but not yet played.
    fn buffered(&self) -> usize;
    /// Next error the device reported since the last call. Polled by the
    /// encoder worker and reported as `AlarmKind::AudioDeviceError`.
    fn take_error(&mut self) -> Option<String> {
        None
    }
}

/// Opens an [`AudioSource`] inside the worker thread.
pub trait OpenAudioSource: Send {
    /// `preferred_rate` is the profile rate; `buffer_samples` the size of the
    /// capture buffer between the device callback and the worker.
    fn open(
        self: Box<Self>,
        preferred_rate: u32,
        buffer_samples: usize,
    ) -> Result<Box<dyn AudioSource>>;
}

/// Opens an [`AudioSink`] inside the worker thread.
pub trait OpenAudioSink: Send {
    /// `preferred_rate` is the profile rate; `buffer_samples` the size of the
    /// playback buffer between the worker and the device callback.
    fn open(
        self: Box<Self>,
        preferred_rate: u32,
        buffer_samples: usize,
    ) -> Result<Box<dyn AudioSink>>;
}

/// Where the decoder reads audio from.
pub enum AudioIn {
    /// A cpal input device.
    Device(DeviceSelector),
    /// Any other source (e.g. in-memory for tests).
    Custom(Box<dyn OpenAudioSource>),
}

impl AudioIn {
    pub(crate) fn open(
        self,
        preferred_rate: u32,
        buffer_samples: usize,
    ) -> Result<Box<dyn AudioSource>> {
        match self {
            AudioIn::Device(sel) => Ok(Box::new(CpalSource::open(
                &sel,
                preferred_rate,
                buffer_samples,
            )?)),
            AudioIn::Custom(opener) => opener.open(preferred_rate, buffer_samples),
        }
    }
}

/// Where the encoder writes audio to.
pub enum AudioOut {
    /// A cpal output device.
    Device(DeviceSelector),
    /// Any other sink (e.g. in-memory for tests).
    Custom(Box<dyn OpenAudioSink>),
}

impl AudioOut {
    pub(crate) fn open(
        self,
        preferred_rate: u32,
        buffer_samples: usize,
    ) -> Result<Box<dyn AudioSink>> {
        match self {
            AudioOut::Device(sel) => Ok(Box::new(CpalSink::open(
                &sel,
                preferred_rate,
                buffer_samples,
            )?)),
            AudioOut::Custom(opener) => opener.open(preferred_rate, buffer_samples),
        }
    }
}

/// Name of the default cpal host (e.g. "ALSA").
pub fn host_name() -> &'static str {
    cpal::default_host().id().name()
}

fn device_name(device: &cpal::Device) -> String {
    device
        .name()
        .unwrap_or_else(|_| "Unknown Device".to_string())
}

/// Input device names in enumeration order (index = [`DeviceSelector::Index`];
/// each name is valid for [`DeviceSelector::Name`]).
pub fn list_input_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    Ok(host.input_devices()?.map(|d| device_name(&d)).collect())
}

/// Output device names in enumeration order (index = [`DeviceSelector::Index`];
/// each name is valid for [`DeviceSelector::Name`]).
pub fn list_output_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    Ok(host.output_devices()?.map(|d| device_name(&d)).collect())
}

/// Use `preferred_rate` if a supported range with the default channel count
/// and sample format covers it; otherwise keep the device default.
/// `supported` is only called when a different rate is actually wanted.
fn select_config<I, F>(
    default_config: cpal::SupportedStreamConfig,
    preferred_rate: Option<u32>,
    supported: F,
) -> cpal::SupportedStreamConfig
where
    I: Iterator<Item = cpal::SupportedStreamConfigRange>,
    F: FnOnce() -> Option<I>,
{
    let default_sample_format = default_config.sample_format();
    let default_channels = default_config.channels();
    let default_rate = default_config.sample_rate().0;

    if let Some(rate) = preferred_rate {
        if rate != default_rate {
            for range in supported().into_iter().flatten() {
                if range.channels() != default_channels {
                    continue;
                }
                if range.sample_format() != default_sample_format {
                    continue;
                }
                let min_rate = range.min_sample_rate().0;
                let max_rate = range.max_sample_rate().0;
                if rate >= min_rate && rate <= max_rate {
                    return range.with_sample_rate(cpal::SampleRate(rate));
                }
            }
        }
    }
    default_config
}

/// cpal error callback: queue the error for the worker (this callback is not
/// the real-time data callback, so locking is fine) and keep the stderr line.
fn error_callback(
    errors: &DeviceErrorQueue,
    direction: &'static str,
) -> impl FnMut(cpal::StreamError) + Send + 'static {
    let errors = errors.clone();
    move |err| {
        eprintln!("an error occurred on {} stream: {}", direction, err);
        errors.push(format!("{} stream: {}", direction, err));
    }
}

/// Build a cpal input stream. The callback receives the interleaved buffer
/// (converted to f32) and the channel count.
/// Returns `(stream, sample_rate, channels)`; the stream is not started.
fn setup_input_stream<F>(
    selector: &DeviceSelector,
    preferred_sample_rate: Option<u32>,
    errors: &DeviceErrorQueue,
    mut on_audio: F,
) -> Result<(cpal::Stream, u32, usize)>
where
    F: FnMut(&[f32], usize) + Send + 'static,
{
    let host = cpal::default_host();

    let mut devices: Vec<cpal::Device> = host.input_devices()?.collect();
    let names: Vec<String> = devices.iter().map(device_name).collect();
    let device = match selector.resolve(&names, "Input device")? {
        Some(index) => devices.swap_remove(index),
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("No default input device available"))?,
    };

    let default_config = device.default_input_config()?;
    let selected_config = select_config(default_config, preferred_sample_rate, || {
        device.supported_input_configs().ok()
    });

    let sample_format = selected_config.sample_format();
    let stream_config: cpal::StreamConfig = selected_config.into();
    let sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let err_fn = || error_callback(errors, "input");
    let ch = channels;

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &_| {
                on_audio(data, ch);
            },
            err_fn(),
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &_| {
                let buffer: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                on_audio(&buffer, ch);
            },
            err_fn(),
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _: &_| {
                let buffer: Vec<f32> = data
                    .iter()
                    .map(|&s| (s as f32 - u16::MAX as f32 / 2.0) / (u16::MAX as f32 / 2.0))
                    .collect();
                on_audio(&buffer, ch);
            },
            err_fn(),
            None,
        )?,
        _ => return Err(anyhow!("Unsupported sample format: {}", sample_format)),
    };

    Ok((stream, sample_rate, channels))
}

/// Build a cpal output stream. The callback fills the interleaved f32 buffer
/// given the channel count. Returns `(stream, sample_rate, channels)`; the
/// stream is not started.
fn setup_output_stream<F>(
    selector: &DeviceSelector,
    preferred_sample_rate: Option<u32>,
    errors: &DeviceErrorQueue,
    mut on_audio: F,
) -> Result<(cpal::Stream, u32, usize)>
where
    F: FnMut(&mut [f32], usize) + Send + 'static,
{
    let host = cpal::default_host();

    let mut devices: Vec<cpal::Device> = host.output_devices()?.collect();
    let names: Vec<String> = devices.iter().map(device_name).collect();
    let device = match selector.resolve(&names, "Output device")? {
        Some(index) => devices.swap_remove(index),
        None => host
            .default_output_device()
            .ok_or_else(|| anyhow!("No default output device available"))?,
    };

    let default_config = device.default_output_config()?;
    let selected_config = select_config(default_config, preferred_sample_rate, || {
        device.supported_output_configs().ok()
    });

    let sample_format = selected_config.sample_format();
    let stream_config: cpal::StreamConfig = selected_config.into();
    let sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let err_fn = || error_callback(errors, "output");
    let ch = channels;

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &stream_config,
            move |data: &mut [f32], _: &_| {
                on_audio(data, ch);
            },
            err_fn(),
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream(
            &stream_config,
            move |data: &mut [i16], _: &_| {
                let mut buffer = vec![0.0; data.len()];
                on_audio(&mut buffer, ch);
                for (i, sample) in buffer.into_iter().enumerate() {
                    data[i] = (sample * i16::MAX as f32) as i16;
                }
            },
            err_fn(),
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_output_stream(
            &stream_config,
            move |data: &mut [u16], _: &_| {
                let mut buffer = vec![0.0; data.len()];
                on_audio(&mut buffer, ch);
                for (i, sample) in buffer.into_iter().enumerate() {
                    data[i] = ((sample * (u16::MAX as f32 / 2.0)) + (u16::MAX as f32 / 2.0)) as u16;
                }
            },
            err_fn(),
            None,
        )?,
        _ => return Err(anyhow!("Unsupported sample format: {}", sample_format)),
    };

    Ok((stream, sample_rate, channels))
}

/// cpal capture: channel 0 of the device into a lock-free ring buffer.
pub struct CpalSource {
    stream: cpal::Stream,
    consumer: HeapConsumer<f32>,
    drops: Arc<AtomicU64>,
    errors: DeviceErrorQueue,
    sample_rate: u32,
    channels: usize,
}

impl CpalSource {
    /// Open (but do not start) the selected input device, preferring
    /// `preferred_rate`, with a ring of `buffer_samples` mono samples.
    pub fn open(
        selector: &DeviceSelector,
        preferred_rate: u32,
        buffer_samples: usize,
    ) -> Result<Self> {
        let drops = Arc::new(AtomicU64::new(0));
        let drops_cb = drops.clone();
        let (mut producer, consumer) = HeapRb::<f32>::new(buffer_samples).split();
        let errors = DeviceErrorQueue::new();
        let (stream, sample_rate, channels) = setup_input_stream(
            selector,
            Some(preferred_rate),
            &errors,
            move |data: &[f32], channels: usize| {
                for frame in data.chunks(channels.max(1)) {
                    if let Some(&s) = frame.first() {
                        if producer.push(s).is_err() {
                            drops_cb.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            },
        )?;
        Ok(Self {
            stream,
            consumer,
            drops,
            errors,
            sample_rate,
            channels,
        })
    }
}

impl AudioSource for CpalSource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> usize {
        self.channels
    }
    fn start(&mut self) -> Result<()> {
        self.stream.play()?;
        Ok(())
    }
    fn pull(&mut self, out: &mut [f32]) -> usize {
        self.consumer.pop_slice(out)
    }
    fn dropped(&self) -> u64 {
        self.drops.load(Ordering::Relaxed)
    }
    fn take_error(&mut self) -> Option<String> {
        self.errors.pop()
    }
}

/// cpal playback: mono ring buffer copied to every device channel; silence
/// on underrun.
pub struct CpalSink {
    stream: cpal::Stream,
    producer: HeapProducer<f32>,
    errors: DeviceErrorQueue,
    sample_rate: u32,
    channels: usize,
}

impl CpalSink {
    /// Open (but do not start) the selected output device, preferring
    /// `preferred_rate`, with a ring of `buffer_samples` mono samples.
    pub fn open(
        selector: &DeviceSelector,
        preferred_rate: u32,
        buffer_samples: usize,
    ) -> Result<Self> {
        let (producer, mut consumer) = HeapRb::<f32>::new(buffer_samples).split();
        let errors = DeviceErrorQueue::new();
        let (stream, sample_rate, channels) = setup_output_stream(
            selector,
            Some(preferred_rate),
            &errors,
            move |data: &mut [f32], channels: usize| {
                for frame in data.chunks_mut(channels.max(1)) {
                    let sample = consumer.pop().unwrap_or(0.0);
                    for s in frame.iter_mut() {
                        *s = sample;
                    }
                }
            },
        )?;
        Ok(Self {
            stream,
            producer,
            errors,
            sample_rate,
            channels,
        })
    }
}

impl AudioSink for CpalSink {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> usize {
        self.channels
    }
    fn start(&mut self) -> Result<()> {
        self.stream.play()?;
        Ok(())
    }
    fn push(&mut self, samples: &[f32]) -> usize {
        self.producer.push_slice(samples)
    }
    fn is_full(&self) -> bool {
        self.producer.is_full()
    }
    fn buffered(&self) -> usize {
        self.producer.len()
    }
    fn take_error(&mut self) -> Option<String> {
        self.errors.pop()
    }
}
