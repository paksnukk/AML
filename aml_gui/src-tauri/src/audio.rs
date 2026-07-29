use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait};

pub fn list_input_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()?
        .map(|d| d.name().unwrap_or_else(|_| "Unknown Device".to_string()))
        .collect();
    Ok(devices)
}

pub fn list_output_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = host
        .output_devices()?
        .map(|d| d.name().unwrap_or_else(|_| "Unknown Device".to_string()))
        .collect();
    Ok(devices)
}

/// Callback receives interleaved buffer and channel count. Use channel 0 only.
pub fn setup_input_stream<F>(
    device_index: Option<usize>,
    preferred_sample_rate: Option<u32>,
    mut on_audio: F,
) -> Result<(cpal::Stream, u32, usize)>
where
    F: FnMut(&[f32], usize) + Send + 'static,
{
    let host = cpal::default_host();

    let device = if let Some(index) = device_index {
        host.input_devices()?
            .nth(index)
            .ok_or_else(|| anyhow!("Input device index {} not found", index))?
    } else {
        host.default_input_device()
            .ok_or_else(|| anyhow!("No default input device available"))?
    };

    let default_config = device.default_input_config()?;
    let default_sample_format = default_config.sample_format();
    let default_channels = default_config.channels();
    let default_rate = default_config.sample_rate().0;

    let mut selected_config = default_config;
    if let Some(rate) = preferred_sample_rate {
        if rate != default_rate {
            if let Ok(configs) = device.supported_input_configs() {
                for range in configs {
                    if range.channels() != default_channels {
                        continue;
                    }
                    if range.sample_format() != default_sample_format {
                        continue;
                    }
                    let min_rate = range.min_sample_rate().0;
                    let max_rate = range.max_sample_rate().0;
                    if rate >= min_rate && rate <= max_rate {
                        selected_config = range.with_sample_rate(cpal::SampleRate(rate));
                        break;
                    }
                }
            }
        }
    }

    let sample_format = selected_config.sample_format();
    let stream_config: cpal::StreamConfig = selected_config.into();
    let sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let err_fn = |err| eprintln!("an error occurred on input stream: {}", err);
    let ch = channels;

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &_| {
                on_audio(data, ch);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &_| {
                let buffer: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                on_audio(&buffer, ch);
            },
            err_fn,
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
            err_fn,
            None,
        )?,
        _ => return Err(anyhow!("Unsupported sample format: {}", sample_format)),
    };

    Ok((stream, sample_rate, channels))
}

pub fn setup_output_stream<F>(
    device_index: Option<usize>,
    preferred_sample_rate: Option<u32>,
    mut on_audio: F,
) -> Result<(cpal::Stream, u32, usize)>
where
    F: FnMut(&mut [f32], usize) + Send + 'static,
{
    let host = cpal::default_host();

    let device = if let Some(index) = device_index {
        host.output_devices()?
            .nth(index)
            .ok_or_else(|| anyhow!("Output device index {} not found", index))?
    } else {
        host.default_output_device()
            .ok_or_else(|| anyhow!("No default output device available"))?
    };

    let default_config = device.default_output_config()?;
    let default_sample_format = default_config.sample_format();
    let default_channels = default_config.channels();
    let default_rate = default_config.sample_rate().0;

    let mut selected_config = default_config;
    if let Some(rate) = preferred_sample_rate {
        if rate != default_rate {
            if let Ok(configs) = device.supported_output_configs() {
                for range in configs {
                    if range.channels() != default_channels {
                        continue;
                    }
                    if range.sample_format() != default_sample_format {
                        continue;
                    }
                    let min_rate = range.min_sample_rate().0;
                    let max_rate = range.max_sample_rate().0;
                    if rate >= min_rate && rate <= max_rate {
                        selected_config = range.with_sample_rate(cpal::SampleRate(rate));
                        break;
                    }
                }
            }
        }
    }

    let sample_format = selected_config.sample_format();
    let stream_config: cpal::StreamConfig = selected_config.into();
    let sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let err_fn = |err| eprintln!("an error occurred on output stream: {}", err);
    let ch = channels;

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &stream_config,
            move |data: &mut [f32], _: &_| {
                on_audio(data, ch);
            },
            err_fn,
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
            err_fn,
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
            err_fn,
            None,
        )?,
        _ => return Err(anyhow!("Unsupported sample format: {}", sample_format)),
    };

    Ok((stream, sample_rate, channels))
}
