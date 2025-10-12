use std::io;
use std::thread;
use std::path::Path;
use std::fs::{self, DirEntry};
use std::sync::{mpsc, Arc, atomic::{Ordering, AtomicUsize}};

use audio_scripting::{CpalDevice, Engine, TrackData, TrackState, ThreadPool};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use rubato::{FftFixedIn, FftFixedInOut, Resampler};

fn CPAL_init() -> anyhow::Result<Arc<CpalDevice>> {
    // CPAL setup
    let cdev = CpalDevice::new()?;
    
    Ok(cdev)
}

fn start_engine() -> anyhow::Result<Engine> {
    let engine = Engine::new();

    Ok(engine)
}

fn load_tracks(out_rate: usize) -> anyhow::Result<Vec<Arc<Track>>> {
    // get path strings
    let paths: Vec<String> = fs::read_dir("assets")?
        .map(|res| { 
            let path = res?.path();
            Ok::<String, io::Error>(path.to_string_lossy().to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    let pool = ThreadPool::build(4)?;
    let (tx, rx) = mpsc::channel::<anyhow::Result<Arc<Track>>>();

    // multithread file loading
    for path in paths {
        let tx = tx.clone();
        pool.execute(move || {
            let track = Track::new(&path, out_size);
            tx.send(track).unwrap();
        });
    }

    drop(tx);

    println!("out_rate = {out_rate}");
    let mut tracks = Vec::new();
    for result in rx {
        match result {
            Ok(track) => tracks.push(new_track),
            Err(err) => eprintln!("Error loading track: {err}"),
        }
    }

    Ok(tracks)
}

/* updating with real time version
 * fn stream_audio(tracks: Vec<Arc<Track>>, cdev: Arc<CpalDevice>) -> anyhow::Result<()> {
    let cfg = Arc::new(cdev.cfg.clone()); 
    let cfg_cb = Arc::clone(&cfg);
    let out_ch = cfg.channels as usize;  
    
    let tracks = Arc::new(tracks);
    let tracks_cb = Arc::clone(&tracks);

    let stream = cdev.device.build_output_stream::<f32, _, _>(
        &cfg,
        move |out: &mut [f32], _| {
            // out is interleaved: frames * out_ch
            let frames_out = out.len() / out_ch;

            for s in out.iter_mut() {
                *s = 0.0;
            }

            for track in tracks_cb.iter() {
                let pos = track.position.load(Ordering::Relaxed);
                let in_ch = track.channels;
                let total = track.total_frames;
                
                if pos >= total {
                    continue; // finished track
                }

                let mut frames_mixed = 0;
                for frame_idx in 0..frames_out {
                    let f = pos + frame_idx;
                    if f >= total {
                        break; // end of track
                    }

                    for c in 0..out_ch {
                        let sample = if c < in_ch && f * in_ch + c < track.data.len() {
                            track.data[f * in_ch + c]
                        } else {
                            0.0
                        };
                        out[frame_idx * out_ch + c] += sample;
                    }
                    frames_mixed += 1;
                }

                // advance track playback
                track
                    .position
                    .fetch_add(frames_mixed, Ordering::Relaxed);
            }

            // soft clipping to prevent overflow
            //for s in out.iter_mut() {
              //  *s = s.clamp(-1.0, 1.0);
            //}
        },
        |err| eprintln!("Stream error: {err}"),
        None,
    )?;

    stream.play()?;

    // Keep running while any track still has data left
    loop {
        let done = tracks
            .iter()
            .all(|t| t.position.load(Ordering::Relaxed) >= t.total_frames);
        if done {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    Ok(())
}
*/

fn main() -> anyhow::Result<()> {
    let device = CPAL_init()?;
    println!("Successfully initialized device.");

    let tracks = load_tracks(device.sample_rate)?; 
    println!("Loaded all tracks.");

    let engine = start_engine()?;
    println!("Started engine.");

    //  stream_audio(tracks, device);

    Ok(())
}
