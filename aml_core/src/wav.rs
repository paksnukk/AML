//! Mono WAV helpers for offline TX/RX tests.

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::io::{Read, Seek, Write};
use std::path::Path;

pub fn write_wav_f32(
    path: impl AsRef<Path>,
    sample_rate: u32,
    samples: &[f32],
) -> Result<(), String> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut writer = WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for &s in samples {
        writer.write_sample(s).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())
}

pub fn read_wav_f32(path: impl AsRef<Path>) -> Result<(u32, Vec<f32>), String> {
    let mut reader = WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let sample_rate = spec.sample_rate;
    let channels = spec.channels as usize;

    let samples: Result<Vec<f32>, _> = match spec.sample_format {
        SampleFormat::Float => reader.samples::<f32>().collect(),
        SampleFormat::Int => reader
            .samples::<i32>()
            .map(|r| {
                r.map(|s| {
                    let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
                    s as f32 / max
                })
            })
            .collect(),
    };
    let interleaved = samples.map_err(|e| e.to_string())?;

    // Downmix / take channel 0
    let mono: Vec<f32> = if channels <= 1 {
        interleaved
    } else {
        interleaved.chunks(channels).map(|c| c[0]).collect()
    };

    Ok((sample_rate, mono))
}

pub fn write_wav_to<W: Write + Seek>(
    writer: W,
    sample_rate: u32,
    samples: &[f32],
) -> Result<(), String> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut wav = WavWriter::new(writer, spec).map_err(|e| e.to_string())?;
    for &s in samples {
        wav.write_sample(s).map_err(|e| e.to_string())?;
    }
    wav.finalize().map_err(|e| e.to_string())
}

pub fn read_wav_from<R: Read + Seek>(reader: R) -> Result<(u32, Vec<f32>), String> {
    let mut reader = WavReader::new(reader).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let sample_rate = spec.sample_rate;
    let channels = spec.channels as usize;
    let samples: Result<Vec<f32>, _> = match spec.sample_format {
        SampleFormat::Float => reader.samples::<f32>().collect(),
        SampleFormat::Int => reader
            .samples::<i32>()
            .map(|r| {
                r.map(|s| {
                    let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
                    s as f32 / max
                })
            })
            .collect(),
    };
    let interleaved = samples.map_err(|e| e.to_string())?;
    let mono: Vec<f32> = if channels <= 1 {
        interleaved
    } else {
        interleaved.chunks(channels).map(|c| c[0]).collect()
    };
    Ok((sample_rate, mono))
}
