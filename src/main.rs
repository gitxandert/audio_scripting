use std::io;
use std::thread;
use std::path::Path;
use std::fs::{self, DirEntry};
use std::sync::{
    mpsc, Arc, 
    atomic::{Ordering, AtomicBool, AtomicUsize}
};

use audio_scripting::{CpalDevice, Engine, Track, TrackState, ThreadPool};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use rubato::{FftFixedIn, FftFixedInOut, Resampler};

fn CPAL_init() -> anyhow::Result<Arc<CpalDevice>> {
    // CPAL setup
    let cdev = CpalDevice::new()?;
    
    Ok(cdev)
}

fn engine_init(channels: u32) -> anyhow::Result<Arc<Engine>> {
    Ok(Arc::new(Engine::new(channels)))
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

fn stream_audio(engine: Arc<Engine>, cdev: Arc<CpalDevice>) -> anyhow::Result<()> {
    let cfg = cdev.cfg.clone(); 
    let out_ch = cfg.channels as usize;  

    // clone to move into CPAL callback
    let engine_cb = Arc::clone(&engine);

    let stream = cdev.device.build_output_stream::<f32, _, _>(
        &cfg,
        move |data: &mut [f32], _| {
            for frame in data.chunks_mut(out_ch) {
                engine_cb.process(frame);
            }
        },
        |err| eprintln!("Stream error: {err}"),
        None,
    )?;

    stream.play()?;

    loop {
        print!("> ");
        io::stdout().flush().unwrap();
        let mut cmd = String::new();

        io::stdin()
            .read_line(&mut cmd)
            .expect("Failed to read line");
        let cmd = cmd.trim();

        if cmd == "quit" {
            break;
        }
        
        if let Some(command) = match_cmd(cmd) {
            spawn_worker(command, Arc::clone(&engine);
        } else {
            println!("Unrecognized command: {cmd}");
        };
    }

    Ok(())
}


fn main() -> anyhow::Result<()> {
    let device = CPAL_init()?;
    println!("Successfully initialized device.");

    let channels = 2;
    let engine = engine_init(channels)?;
    println!("Initialized engine.");
    
    let tracks = load_tracks(device.sample_rate)?; 
    println!("Loaded all tracks.");
    // TODO: integrate tracks into stream_audio
    //  stream_audio(engine, device);

    Ok(())
}
