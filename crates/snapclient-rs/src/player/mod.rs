//! Audio output — cpal callback reads from Stream directly.

use std::sync::{Arc, Mutex};

use snapcast_client::connection::now_usec;
use snapcast_client::stream::{SampleEncoding, Stream};
use snapcast_client::time_provider::TimeProvider;
use snapcast_proto::SampleFormat;

use crate::mixer::VolumeState;

/// Run audio output forever: wait for the Stream to have a format, play it
/// through cpal, and restart whenever the format changes or output fails.
pub async fn play_audio(
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    volume: Arc<VolumeState>,
) {
    loop {
        // Wait for the Stream to have a valid format
        let (format, encoding) = loop {
            {
                let s = stream.lock().unwrap_or_else(|e| e.into_inner());
                let f = s.format();
                if f.rate() > 0 && f.channels() > 0 {
                    break (f, s.encoding());
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };

        tracing::info!(
            rate = format.rate(),
            bits = format.bits(),
            channels = format.channels(),
            "Audio format detected"
        );

        // cpal blocks its thread until the format changes; run it off the executor.
        let stream_clone = Arc::clone(&stream);
        let tp_clone = Arc::clone(&time_provider);
        let vol_clone = Arc::clone(&volume);
        let result = tokio::task::spawn_blocking(move || {
            run_cpal(stream_clone, tp_clone, format, encoding, vol_clone)
        })
        .await;

        match result {
            Ok(Ok(())) => {
                tracing::info!("Audio format change detected, restarting player");
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "Audio output failed, retrying in 1s");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => {
                tracing::error!(error = %e, "Audio thread failed, restarting in 1s");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

/// Pick the device config for `format`: the exact rate and channel count if
/// supported, else the exact rate with another channel count (channels are
/// remapped), else the device default. Returns whether the rate differs.
fn select_config(
    device: &cpal::Device,
    format: SampleFormat,
) -> anyhow::Result<(cpal::StreamConfig, bool)> {
    use cpal::traits::DeviceTrait;

    let supported: Vec<_> = device.supported_output_configs()?.collect();
    let has_rate = |f: &&cpal::SupportedStreamConfigRange| {
        (f.min_sample_rate()..=f.max_sample_rate()).contains(&format.rate())
    };
    let matching = supported
        .iter()
        .filter(has_rate)
        .find(|f| f.channels() == format.channels())
        .or_else(|| supported.iter().find(has_rate));
    if let Some(f) = matching {
        return Ok((f.with_sample_rate(format.rate()).into(), false));
    }
    let config: cpal::StreamConfig = device.default_output_config()?.into();
    let resample = config.sample_rate != format.rate();
    Ok((config, resample))
}

fn run_cpal(
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    format: snapcast_proto::SampleFormat,
    encoding: SampleEncoding,
    volume: Arc<VolumeState>,
) -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output device"))?;

    tracing::info!(device = %device.description().map(|d| d.name().to_string()).unwrap_or_default(), "Using audio device");

    let (config, needs_resampling) = select_config(&device, format)?;
    let device_rate = config.sample_rate;
    let device_channels = config.channels as usize;
    if device_channels != format.channels() as usize {
        tracing::warn!(
            stream = format.channels(),
            device = device_channels,
            "Device can't play the stream's channel count, remapping channels"
        );
    }

    #[cfg(feature = "resampler")]
    let mut resampled = if needs_resampling {
        tracing::warn!(
            stream = format.rate(),
            device = device_rate,
            "Device can't play the stream's sample rate, resampling"
        );
        let device_format = SampleFormat::new(device_rate, 32, format.channels());
        // 20 ms resampler chunks (a typical Snapcast chunk size)
        snapcast_client::resampler::Resampler::new_if_needed(
            format,
            device_format,
            encoding,
            (format.rate() / 50) as usize,
        )?
        .map(ResampledOutput::new)
    } else {
        None
    };
    #[cfg(not(feature = "resampler"))]
    if needs_resampling {
        anyhow::bail!(
            "device can't play {} Hz (it uses {device_rate} Hz); build with the `resampler` feature",
            format.rate()
        );
    }

    let stream_cb = Arc::clone(&stream);
    let tp_cb = Arc::clone(&time_provider);
    // Reused across callbacks and resized in place, so the realtime audio path
    // performs no heap allocation after warmup. Allocating inside a cpal
    // callback can stall the audio thread and cause xruns/glitches.
    let mut pcm_buf: Vec<u8> = Vec::new();
    let cpal_stream = device.build_output_stream(
        config,
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let num_frames = data.len() / device_channels;

            let buffer_dac_usec = info
                .timestamp()
                .playback
                .duration_since(info.timestamp().callback)
                .as_micros() as i64
                + (num_frames as i64 * 1_000_000) / device_rate as i64;

            let server_now = {
                let tp = tp_cb.lock().unwrap_or_else(|e| e.into_inner());
                now_usec() + tp.diff_to_server_usec()
            };

            let mut s = stream_cb.lock().unwrap_or_else(|e| e.into_inner());

            // Format change: play silence until the outer loop restarts us
            if s.format() != format || s.encoding() != encoding {
                data.fill(0.0);
                return;
            }

            #[cfg(feature = "resampler")]
            let done = if let Some(ref mut r) = resampled {
                r.render(
                    &mut s,
                    server_now,
                    buffer_dac_usec,
                    device_rate,
                    data,
                    device_channels,
                );
                true
            } else {
                false
            };
            #[cfg(not(feature = "resampler"))]
            let done = false;

            if !done {
                pcm_buf.resize(num_frames * format.frame_size() as usize, 0);
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec,
                    &mut pcm_buf,
                    num_frames as u32,
                );
                drop(s);
                write_samples_to_output(data, device_channels, &pcm_buf, format, encoding);
            }

            // Apply software volume
            let gain = volume.gain();
            if gain < 1.0 {
                for sample in data.iter_mut() {
                    *sample *= gain;
                }
            }
        },
        |err| tracing::error!(error = %err, "Audio stream error"),
        None,
    )?;

    cpal_stream.play()?;
    tracing::info!("Audio playback started");

    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let s = stream.lock().unwrap_or_else(|e| e.into_inner());
        if s.format() != format || s.encoding() != encoding {
            return Ok(());
        }
    }
}

/// Resampled output: stream frames are resampled in whole resampler chunks,
/// and the output not yet played is kept for the next callbacks.
#[cfg(feature = "resampler")]
struct ResampledOutput {
    resampler: snapcast_client::resampler::Resampler,
    in_buf: Vec<u8>,
    /// Resampled interleaved samples (stream channel layout) not yet played.
    out: std::collections::VecDeque<f32>,
}

#[cfg(feature = "resampler")]
impl ResampledOutput {
    fn new(resampler: snapcast_client::resampler::Resampler) -> Self {
        Self {
            resampler,
            in_buf: Vec::new(),
            out: std::collections::VecDeque::new(),
        }
    }

    /// Fill `data` (device layout) from the stream via the resampler.
    fn render(
        &mut self,
        stream: &mut Stream,
        server_now: i64,
        buffer_dac_usec: i64,
        device_rate: u32,
        data: &mut [f32],
        device_channels: usize,
    ) {
        let format = stream.format();
        let channels = format.channels() as usize;
        let frames = data.len() / device_channels;
        while self.out.len() < frames * channels {
            // Output already queued, plus the resampler's latency, plays
            // before the frames read now.
            let ahead_frames = self.out.len() / channels + self.resampler.output_delay();
            let dac_usec = buffer_dac_usec + ahead_frames as i64 * 1_000_000 / device_rate as i64;
            let in_frames = self.resampler.input_frames_until_output();
            self.in_buf
                .resize(in_frames * format.frame_size() as usize, 0);
            stream.get_player_chunk_or_silence(
                server_now,
                dac_usec,
                &mut self.in_buf,
                in_frames as u32,
            );
            if let Err(e) = self.resampler.process(&mut self.in_buf) {
                tracing::error!(error = %e, "Resampling failed");
                break;
            }
            if self.in_buf.is_empty() {
                break;
            }
            self.out.extend(
                self.in_buf
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            );
        }

        let available = (self.out.len() / channels).min(frames);
        data.fill(0.0);
        for (frame, out) in data
            .chunks_exact_mut(device_channels)
            .take(available)
            .enumerate()
        {
            for (c, sample) in out.iter_mut().enumerate() {
                *sample = self.out[frame * channels + c % channels];
            }
        }
        self.out.drain(..available * channels);
    }
}

/// Decode one sample of `samples` at sample index `idx` to f32.
fn sample_at(samples: &[u8], idx: usize, format: SampleFormat, encoding: SampleEncoding) -> f32 {
    let bytes = |n: usize| samples.get(idx * n..idx * n + n);
    match (encoding, format.bits()) {
        (SampleEncoding::Float32, _) => {
            bytes(4).map_or(0.0, |b| f32::from_le_bytes(b.try_into().unwrap()))
        }
        (SampleEncoding::PcmInt, 16) => bytes(2).map_or(0.0, |b| {
            i16::from_le_bytes(b.try_into().unwrap()) as f32 / i16::MAX as f32
        }),
        (SampleEncoding::PcmInt, 24) => bytes(4).map_or(0.0, |b| {
            i32::from_le_bytes(b.try_into().unwrap()) as f32 / snapcast_proto::PCM_24BIT_MAX
        }),
        (SampleEncoding::PcmInt, 32) => bytes(4).map_or(0.0, |b| {
            i32::from_le_bytes(b.try_into().unwrap()) as f32 / i32::MAX as f32
        }),
        _ => 0.0,
    }
}

/// Convert interleaved stream samples to the device's f32 output, mapping the
/// stream's channels onto the device's (device channel `c` plays stream
/// channel `c % stream_channels`, so mono feeds every output).
fn write_samples_to_output(
    output: &mut [f32],
    output_channels: usize,
    samples: &[u8],
    format: SampleFormat,
    encoding: SampleEncoding,
) {
    let channels = format.channels() as usize;
    if channels == 0 || output_channels == 0 {
        output.fill(0.0);
        return;
    }
    for (frame, out) in output.chunks_exact_mut(output_channels).enumerate() {
        for (c, sample) in out.iter_mut().enumerate() {
            *sample = sample_at(samples, frame * channels + c % channels, format, encoding);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i16_bytes(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn writes_matching_layout() {
        let f = SampleFormat::new(48000, 16, 2);
        let mut out = [9.0f32; 4];
        write_samples_to_output(
            &mut out,
            2,
            &i16_bytes(&[i16::MAX, 0, 0, -i16::MAX]),
            f,
            SampleEncoding::PcmInt,
        );
        assert_eq!(out, [1.0, 0.0, 0.0, -1.0]);
    }

    #[test]
    fn mono_stream_feeds_every_device_channel() {
        let f = SampleFormat::new(48000, 16, 1);
        let mut out = [9.0f32; 4];
        write_samples_to_output(
            &mut out,
            2,
            &i16_bytes(&[i16::MAX, 0]),
            f,
            SampleEncoding::PcmInt,
        );
        assert_eq!(out, [1.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn stereo_stream_on_mono_device_keeps_frame_alignment() {
        let f = SampleFormat::new(48000, 32, 2);
        let samples: Vec<u8> = [0.1f32, 0.2, 0.3, 0.4]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut out = [9.0f32; 2];
        write_samples_to_output(&mut out, 1, &samples, f, SampleEncoding::Float32);
        assert_eq!(out, [0.1, 0.3]);
    }

    #[test]
    fn short_input_is_silence() {
        let f = SampleFormat::new(48000, 16, 2);
        let mut out = [9.0f32; 4];
        write_samples_to_output(
            &mut out,
            2,
            &i16_bytes(&[i16::MAX]),
            f,
            SampleEncoding::PcmInt,
        );
        assert_eq!(out, [1.0, 0.0, 0.0, 0.0]);
    }
}
