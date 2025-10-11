use std::io;
use std::thread;
use std::path::Path;
use std::fs::{self, DirEntry};
use std::sync::{mpsc, Mutex, Arc, atomic::{AtomicUsize, Ordering}};

use crossbeam_channel::{unbounded, Sender, Receiver};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::platform::{Host, Device};

use hound::{SampleFormat, WavReader};

struct CpalDevice {
    host: Host,
    device: Device,
    cfg: cpal::StreamConfig,
}

impl CpalDevice {
    fn new() -> anyhow::Result<Arc<Self>> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("no output device"))?;
        let out_cfg = device.default_output_config()?;
        let cfg: cpal::StreamConfig = out_cfg.clone().into();

        Ok(Arc::new(Self { host, device, cfg }))
    }
}

fn CPAL_init() -> anyhow::Result<Arc<CpalDevice>> {
    // CPAL setup
    let CDev = CpalDevice::new()?;
    
    Ok(CDev)
}

struct Track {
    path: String,
    in_ch: usize,
    data_cb: Arc<Vec<f32>>, 
    pos_cb: Arc<AtomicUsize>,
    total_frames: usize,
}

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

impl Track {
    fn new(path: &str) -> anyhow::Result<Arc<Self>> {
        println!("Loading {path}...");
        // Decode
        let (mut data, spec) = decode_to_f32(path)?;
        let in_ch = spec.channels as usize;

        // Share buffers
        let data = Arc::new(data);
        let pos  = Arc::new(AtomicUsize::new(0)); // index in *frames* (not samples)

        let data_cb = Arc::clone(&data);
        let pos_cb  = Arc::clone(&pos);

        // Compute total frames in the source
        let total_frames = data.len() / in_ch;
 
        println!("Finished loading {path}");
        Ok(Arc::new(Self { 
            path: path.to_string(), 
            in_ch, 
            data_cb, 
            pos_cb, 
            total_frames 
        }))
    }
}

pub struct ThreadPool {
    workers: Vec<Worker>,
    sender: Option<Sender<Job>>,
}

type Job = Box<dyn FnOnce() + Send + 'static>;

impl ThreadPool {
    pub fn build(size: usize) -> anyhow::Result<ThreadPool> {
        assert!(size > 0);

        let (sender, receiver) = unbounded::<Job>();
        let receiver= Arc::new(receiver);

        let mut workers = Vec::with_capacity(size);
        
        for id in 0..size {
            workers.push(Worker::new(id, Arc::clone(&receiver)));
        }

        Ok(ThreadPool { workers, sender: Some(sender) })
    }

    pub fn execute<F>(&self, f: F)
    where 
        F: FnOnce() + Send + 'static,
    {
        let job = Box::new(f);
        self.sender.as_ref().unwrap().send(job).unwrap();
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        drop(self.sender.take());

        for worker in self.workers.drain(..) {
            worker.thread.join().unwrap();
        }
    }
}

struct Worker {
    id: usize,
    thread: thread::JoinHandle<()>,
}

impl Worker {
    fn new(id: usize, receiver: Arc<Receiver<Job>>) -> Worker {
        let thread = thread::spawn(move || {
            while let Ok(job) = receiver.recv() {
                job();
        
            }
        });

        Worker { id, thread }
    }
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


