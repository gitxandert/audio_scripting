use std::io;
use std::thread;
use std::path::Path;
use std::fs::{self, DirEntry};
use std::sync::{mpsc, Arc, atomic::Ordering};

use audio_scripting::{CpalDevice, Track, ThreadPool};

fn CPAL_init() -> anyhow::Result<Arc<CpalDevice>> {
    // CPAL setup
    let CDev = CpalDevice::new()?;
    
    Ok(CDev)
}

fn load_tracks() -> anyhow::Result<Vec<Arc<Track>>> {
    let paths: Vec<String> = fs::read_dir("assets")?
        .map(|res| { 
            let path = res?.path();
            Ok::<String, io::Error>(path.to_string_lossy().to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    let pool = ThreadPool::build(4)?;

    let (tx, rx) = mpsc::channel::<anyhow::Result<Arc<Track>>>();

    for path in paths {
        let tx = tx.clone();
        pool.execute(move || {
            let track = Track::new(&path);
            tx.send(track).unwrap();
        });
    }

    drop(tx);

    let mut tracks = Vec::new();
    for result in rx {
        match result {
            Ok(track) => tracks.push(track),
            Err(err) => eprintln!("Error loading track: {err}"),
        }
    }

    Ok(tracks)
}

/* fn stream_audio(tracks: Vec<Track>, CDev: CpalDevice) -> anyhow::Result<()> {
    let cfg = CDev.cfg; 
    let out_ch = cfg.channels as usize;  
    
    /* implement per track
    let data_cb = Arc::clone(&data); 
    let pos_cb  = Arc::clone(&pos);
    */

    let stream = CDev.device.build_output_stream::<f32, _, _>(
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
*/

fn main() -> anyhow::Result<()> {
    if let device = CPAL_init() {
        print!("Successfully initialized device.\n");
    }

    if let tracks = load_tracks()? {
        print!("Loaded all tracks\n");
    }

    Ok(())
}


