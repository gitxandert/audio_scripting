use std::io;
use std::thread;
use std::path::Path;
use std::fs::{self, DirEntry};
use std::collections::VecDeque;
use std::sync::{
    mpsc, Arc, 
    atomic::{Ordering, AtomicBool, AtomicUsize}
};

use ringbuf::RingBuffer;

use audio_scripting::{CpalDevice, Engine, Track, TrackState, ThreadPool, Command};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use rubato::{FftFixedIn, FftFixedInOut, Resampler};

fn CPAL_init() -> anyhow::Result<Arc<CpalDevice>> {
    // CPAL setup
    let cdev = CpalDevice::new()?;
    
    Ok(cdev)
}

fn engine_init(channels: u32, tracks: HashMap<&'static str, Track>) -> anyhow::Result<Arc<Engine>> {
    Ok(Arc::new(Engine::new(channels, tracks)))
}

fn load_tracks(out_rate: usize) -> anyhow::Result<HashMap<&'static str, Track>> {
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
            let track = Track::new(&path, out_size);
            tx.send(track).unwrap();
        });
    }

    drop(tx);

    println!("out_rate = {out_rate}");
    let mut tracks = HashMap::new();
    for result in rx {
        match result {
            Ok(track) => tracks.insert(track.path, track); 
            Err(err) => eprintln!("Error loading track: {err}"),
        }
    }

    Ok(tracks)
}

fn spawn_worker(cmd: &str, engine: Arc<Engine>) {
    let worker = thread::spawn(move ||
        if let Some(ctor, args) = match_cmd(cmd) {
            let buffers = &mut engine.buffers;
            let mut buf_name: &str;
            let args: VecDeque<&str> = args.split(' ').collect();

            let track_name = args.pop_front();
            let maybe_as: Option<&&str> = args.pop_front();
            if maybe_as == "as" {
                buf_name = args.pop_front();
            } else {
                buf_name = track_name;
                args.push_front(maybe_as);
            }

            let track = Arc::new(engine.tracks.get(track_name));
            
            let (prod, cons) = RingBuffer::<f32>::new(BUFFER_CAPACITY).split();
            buffers.write().unwrap().insert(buf_name, Arc::new(Buffer { cons }));
            
            // each thread responsible for parsing args on its own
            ctor(track, Arc::new(args), Arc::new(prod));
        });
    }
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
        
        spawn_worker(cmd, Arc::clone(&engine));
    }

    Ok(())
}


fn main() -> anyhow::Result<()> {
    let device = CPAL_init()?;
    println!("Successfully initialized device.");
        
    let tracks = load_tracks(device.sample_rate)?; 
    println!("Loaded all tracks.");
    // TODO: integrate tracks into stream_audio
    let channels = 2;
    let engine = engine_init(channels, tracks)?;
    println!("Initialized engine.");

    //  stream_audio(engine, device);

    Ok(())
}
