use std::io::{self, Write};
use std::path::Path;
use std::fs::{self, DirEntry};
use std::collections::{VecDeque, HashMap};
use std::sync::{
    mpsc, Arc, Mutex,
    atomic::{Ordering, AtomicBool, AtomicUsize}
};

use audio_scripting::{CpalDevice, Engine, Track, ThreadPool};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use rubato::{FftFixedIn, FftFixedInOut, Resampler};

fn CPAL_init() -> anyhow::Result<Arc<CpalDevice>> {
    // CPAL setup
    let cdev = CpalDevice::new()?;
    
    Ok(cdev)
}

fn engine_init(channels: usize, tracks: HashMap<String, Track>) -> anyhow::Result<Engine> {
    Ok(Engine::new(channels, tracks))
}

fn load_tracks(out_rate: usize) -> anyhow::Result<HashMap<String, Track>> {
    // get path strings
    let paths: Vec<String> = fs::read_dir("assets")?
        .map(|res| { 
            let path = res?.path();
            Ok::<String, io::Error>(path.to_string_lossy().to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    let pool = ThreadPool::build(4)?;
    let (tx, rx) = mpsc::channel::<anyhow::Result<Track>>();

    // multithread file loading
    for path in paths {
        let tx = tx.clone();
        pool.execute(move || {
            let track = Track::new(&path, out_rate);
            tx.send(track).unwrap();
        });
    }

    drop(tx);

    println!("out_rate = {out_rate}");
    let mut tracks = HashMap::new();
    for result in rx {
        if let track = result.unwrap() {
            tracks.insert(String::from(&track.path), track);
        } else {
            eprintln!("Error opening track");
        }
    }

    Ok(tracks)
}

fn stream_audio(engine: Arc<Mutex<Engine>>, cdev: Arc<CpalDevice>) -> anyhow::Result<()> {
    let cfg = cdev.cfg.clone(); 
    let out_ch = cfg.channels as usize;  

    // clone to move into CPAL callback
    let engine_cb = Arc::clone(&engine);

    let stream = cdev.device.build_output_stream::<f32, _, _>(
        &cfg,
        move |data: &mut [f32], _| {
            let mut engine = engine_cb.lock().unwrap();
            for frame in data.chunks_mut(out_ch) {
                engine.process(frame);
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
       
        Engine::spawn_worker(&engine, cmd);
    }

    Ok(())
}


fn main() -> anyhow::Result<()> {
    let device = CPAL_init()?;
    println!("Successfully initialized device.");
        
    let tracks = load_tracks(device.sample_rate)?; 
    println!("Loaded all tracks.");
    
    let channels = 2;
    let engine = engine_init(channels, tracks)?;
    println!("Initialized engine.");

    stream_audio(Arc::new(Mutex::new(engine)), device);

    Ok(())
}
