use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

fn main() -> anyhow::Result<()> {
    // Decode
    let (mut data, spec) = decode_to_f32("assets/mith.wav")?;
    let in_ch = spec.channels as usize;

    // CPAL setup
    let host   = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("no output device"))?;
    let out_cfg = device.default_output_config()?;
    let cfg: cpal::StreamConfig = out_cfg.clone().into();
    let out_ch = cfg.channels as usize;

    // Share buffers
    let data = Arc::new(data);
    let pos  = Arc::new(AtomicUsize::new(0)); // index in *frames* (not samples)

    let err_fn = |e| eprintln!("stream error: {e}");
    let data_cb = Arc::clone(&data);
    let pos_cb  = Arc::clone(&pos);

    // Compute total frames in the source
    let total_frames = data.len() / in_ch;

    let stream = device.build_output_stream::<f32, _, _>(
        &cfg,
        move |out: &mut [f32], _| {
            // out is interleaved: frames * out_ch
            let frames_out = out.len() / out_ch;
            let mut f = pos_cb.load(Ordering::Relaxed);

            for frame_idx in 0..frames_out {
                if f >= total_frames {
                    // Past end: write silence for all channels
                    for c in 0..out_ch {
                        out[frame_idx*out_ch + c] = 0.0;
                    }
                    continue;
                }

                // Copy available input channels; zero the rest
                for c in 0..out_ch {
                    let sample = if c < in_ch {
                        data_cb[f*in_ch + c]
                    } else {
                        0.0
                    };
                    out[frame_idx*out_ch + c] = sample;
                }

                f += 1;
            }

            pos_cb.store(f, Ordering::Relaxed);
        },
        err_fn,
        None,
    )?;

    stream.play()?;

    // Keep alive until done
    while pos.load(Ordering::Relaxed) < total_frames {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    Ok(())
}

use hound::{SampleFormat, WavReader};

fn decode_to_f32(path: &str) -> anyhow::Result<(Vec<f32>, hound::WavSpec)> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();

    let data: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Int, 8)  => reader.samples::<i8>() .map(|s| s.unwrap() as f32 / 128.0).collect(),
        (SampleFormat::Int, 16) => reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect(),
        (SampleFormat::Int, 24) => reader.samples::<i32>().map(|s| s.unwrap() as f32 / 8_388_608.0).collect(), // <-- no shift
        (SampleFormat::Int, 32) => reader.samples::<i32>().map(|s| s.unwrap() as f32 / 2_147_483_648.0).collect(),
        (SampleFormat::Float, 32) => reader.samples::<f32>().map(Result::unwrap).collect(),
        (fmt, bits) => anyhow::bail!("Unsupported WAV: {fmt:?} {bits}-bit"),
    };

    Ok((data, spec))
}

