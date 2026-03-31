use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub fn list_devices() -> Result<()> {
    let host = cpal::default_host();
    println!("Host: {}", host.id().name());

    println!("--- Input Devices ---");
    for (index, device) in host.input_devices()?.enumerate() {
        println!("{}: {}", index, device.name().unwrap_or("Unknown".into()));
    }

    println!("--- Output Devices ---");
    for (index, device) in host.output_devices()?.enumerate() {
        println!("{}: {}", index, device.name().unwrap_or("Unknown".into()));
    }

    Ok(())
}

pub fn setup_input_stream<F>(
    device_index: Option<usize>,
    mut on_audio: F,
) -> Result<(cpal::Stream, u32)>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    let host = cpal::default_host();

    // Select device
    let device = if let Some(index) = device_index {
        host.input_devices()?
            .nth(index)
            .ok_or_else(|| anyhow!("Input device index {} not found", index))?
    } else {
        host.default_input_device()
            .ok_or_else(|| anyhow!("No default input device available"))?
    };

    println!("Using Input Device: {}", device.name()?);

    // Configure stream
    let config = device.default_input_config()?;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate.0;

    println!(
        "Input Config: {:?}, Sample Rate: {}",
        stream_config, sample_rate
    );

    let err_fn = |err| eprintln!("an error occurred on stream: {}", err);

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &_| {
                on_audio(data);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &_| {
                // Convert i16 to f32
                let buffer: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                on_audio(&buffer);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _: &_| {
                // Convert u16 to f32
                let buffer: Vec<f32> = data
                    .iter()
                    .map(|&s| (s as f32 - u16::MAX as f32 / 2.0) / (u16::MAX as f32 / 2.0))
                    .collect();
                on_audio(&buffer);
            },
            err_fn,
            None,
        )?,
        _ => return Err(anyhow!("Unsupported sample format: {}", sample_format)),
    };

    Ok((stream, sample_rate))
}

pub fn setup_output_stream<F>(
    device_index: Option<usize>,
    mut on_audio: F,
) -> Result<(cpal::Stream, u32)>
where
    F: FnMut(&mut [f32]) + Send + 'static,
{
    let host = cpal::default_host();

    // Select device
    let device = if let Some(index) = device_index {
        host.output_devices()?
            .nth(index)
            .ok_or_else(|| anyhow!("Output device index {} not found", index))?
    } else {
        host.default_output_device()
            .ok_or_else(|| anyhow!("No default output device available"))?
    };

    println!("Using Output Device: {}", device.name()?);

    // Configure stream
    let config = device.default_output_config()?;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate.0;

    println!(
        "Output Config: {:?}, Sample Rate: {}",
        stream_config, sample_rate
    );

    let err_fn = |err| eprintln!("an error occurred on stream: {}", err);

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &stream_config,
            move |data: &mut [f32], _: &_| {
                on_audio(data);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream(
            &stream_config,
            move |data: &mut [i16], _: &_| {
                let mut buffer = vec![0.0; data.len()];
                on_audio(&mut buffer);
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
                on_audio(&mut buffer);
                for (i, sample) in buffer.into_iter().enumerate() {
                    data[i] = ((sample * (u16::MAX as f32 / 2.0)) + (u16::MAX as f32 / 2.0)) as u16;
                }
            },
            err_fn,
            None,
        )?,
        _ => return Err(anyhow!("Unsupported sample format: {}", sample_format)),
    };

    Ok((stream, sample_rate))
}
