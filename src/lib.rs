use rubato::{FftFixedIn, Resampler};
use std::thread;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock, Mutex,
                atomic::{AtomicBool, Ordering}
};

use ringbuf::{traits::*, SharedRb, storage::Heap};

type Producer = <ringbuf::SharedRb<ringbuf::storage::Heap<f32>> as ringbuf::traits::Split>::Prod;
type Consumer = <ringbuf::SharedRb<ringbuf::storage::Heap<f32>> as ringbuf::traits::Split>::Cons;

use crossbeam_channel::{unbounded, Sender, Receiver};

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::platform::{Host, Device};

use hound::{SampleFormat, WavReader};

// consistent match statement expansion
macro_rules! cmd_table {
    ($($key:literal => $handler:path),* $(,)?) => {{
        fn lookup<'a>(cmd: &str) -> Option<fn(Arc<Track>, VecDeque<&str>, Producer)> {
            match cmd {
                $($key => Some($handler),)*
                _ => None,
            }
         /* prefix matching:
          * $(
          *      if $key.starts_with(cmd) {
          *          return Some($handler);
          *      }
          *  )*
          *  None
          */
        }
        lookup
    }};
}

static CORE_CMDS: fn(&str) -> Option<fn(Arc<Track>, VecDeque<&str>, Producer)> = cmd_table! {
    "play" => Engine::Play,
    //"stop" => Engine::Stop,
};

// re-export
pub use crate::command::cmd_match;

pub mod command {
    use super::*;

    pub fn cmd_match(line: &str) -> Option<(fn(Arc<Track>, VecDeque<&str>, Producer), &str)> {
        let mut parts = line.splitn(2, ' ');
        let cmd_name = parts.next()?;
        let args = parts.next().unwrap_or("");

        // check compile-time core commands first
        if let Some(ctor) = CORE_CMDS(cmd_name) {
            return Some((ctor, args));
        }

        // cmd is not registered
        None
    }
} // end mod command


//

// re-export
pub use crate::concurrency::threadpool::ThreadPool;

pub mod concurrency {
    use super::*;

    pub mod threadpool {
        use super::*;

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
    } // end pub mod threadpool
} // end pub mod concurrency


//
// re-export
pub use crate::audio_setup::{
    device::CpalDevice,
    engine::{Engine, Buffer},
    tracks::Track
};

pub mod audio_setup {
    use super::*;

    pub mod device {
        use super::*;

        pub struct CpalDevice {
            pub host: Host,
            pub device: Device,
            pub cfg: cpal::StreamConfig,
            pub sample_rate: usize,
        }

        impl CpalDevice {
            pub fn new() -> anyhow::Result<Arc<Self>> {
                let host = cpal::default_host();
                let device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("no output device"))?;
                let out_cfg = device.default_output_config()?;
                let cfg: cpal::StreamConfig = out_cfg.clone().into();
                let sample_rate = cfg.sample_rate.0;
                let sample_rate: usize = sample_rate.try_into().unwrap();
                Ok(
                    Arc::new(
                        Self { 
                            host, device, cfg, sample_rate 
                        }))
            }
        }
    } // end pub mod device

    pub mod tracks {
        use super::*;

        #[derive(Clone)]
        pub struct Track {
            pub path: String,
            pub channels: usize,
            pub samples: Vec<f32>,
            pub total_frames: usize,
            pub sample_rate: usize,
        }

        fn decode_to_f32(path: &String) -> anyhow::Result<(Vec<f32>, hound::WavSpec)> {
            let mut reader = WavReader::open(path)?;
            let spec = reader.spec();

            let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
                (SampleFormat::Int, 8)  => reader.samples::<i8>() .map(|s| s.unwrap() as f32 / 128.0).collect(),
                (SampleFormat::Int, 16) => reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect(),
                (SampleFormat::Int, 24) => reader.samples::<i32>().map(|s| s.unwrap() as f32 / 8_388_608.0).collect(), // <-- no shift
                (SampleFormat::Int, 32) => reader.samples::<i32>().map(|s| s.unwrap() as f32 / 2_147_483_648.0).collect(),
                (SampleFormat::Float, 32) => reader.samples::<f32>().map(Result::unwrap).collect(),
                (fmt, bits) => anyhow::bail!("Unsupported WAV: {fmt:?} {bits}-bit"),
            };

            Ok((samples, spec))
        }

        impl Track {
            pub fn new(path: &String, out_rate: usize) -> anyhow::Result<Self> {
                println!("Loading {path}...");
                // Decode
                let path = path.clone();
                let (mut samples, spec) = decode_to_f32(&path)?;
                let channels = spec.channels as usize;
                let sample_rate = spec.sample_rate as usize;

                if sample_rate != out_rate {
                    samples = Self::resample_to_device_rate(&samples, &path, &sample_rate, &channels, out_rate);
                }

                // Compute total frames in the source
                let total_frames = samples.len() / channels;
 
                println!("Finished loading {}", &path);
                Ok(Self { 
                    path, 
                    channels, 
                    samples, 
                    total_frames,
                    sample_rate,
                })
            }

            fn resample_to_device_rate(samples: &Vec<f32>, path: &String, sample_rate: &usize, channels: &usize, out_rate: usize) -> Vec<f32> {
                if *sample_rate == out_rate {
                    return samples.clone();
                }

                println!("Resampling {}", path);

                let nch = *channels;
                let input: Vec<Vec<f32>> = (0..nch)
                    .map(|ch| samples.iter().skip(ch).step_by(nch).copied().collect())
                    .collect();

                let mut resampler = FftFixedIn::<f32>::new(
                    *sample_rate, out_rate, 1024, 1, nch
                ).unwrap();

                let mut out_chs: Vec<Vec<f32>> = (0..nch).map(|_| Vec::new()).collect();

                let mut pos = 0;
                while pos < input[0].len() {
                    let frames = resampler.input_frames_next();
                    let end = (pos + frames).min(input[0].len());
                    let chunk: Vec<Vec<f32>> = (0..nch)
                        .map(|ch| input[ch][pos..end].to_vec())
                        .collect();
                    let output = resampler.process(&chunk, None).unwrap();
                    for (ch, buf) in output.into_iter().enumerate() {
                        out_chs[ch].extend(buf);
                    }
                    pos = end;
                }
                // interleave
                let frames = out_chs[0].len();
                let mut interleaved = Vec::with_capacity(frames * nch);
                for f in 0..frames {
                    for c in 0..nch {
                        interleaved.push(out_chs[c][f]);
                    }
                }

                interleaved
            }
        }

    } // end pub mod tracks
 
    pub mod engine {
        use super::*;

        pub struct Buffer {
            pub ring: Mutex<Consumer>,
            // Producer sent to Worker thread
            pub active: AtomicBool,
            pub gain: f32,
        }

        impl Buffer {
            pub fn new(ring: Consumer) -> Self {
                Self {
                    ring: Mutex::new(ring),
                    active: AtomicBool::new(true),
                    gain: 1.0f32
                }
            }

            pub fn mix_into(&mut self, output: &mut [f32], channels: usize) {
                use ringbuf::traits::Consumer as _;

                let frames = output.len() / channels;
                let mut tmp = vec![0.0f32; frames];
                let n = {
                    let mut ring = self.ring.lock().unwrap();
                    ring.pop_slice(&mut tmp)
                };

                for i in 0..n {
                    let v = tmp[i] * self.gain;
                    for ch in 0..channels {
                        output[i * channels + ch] += v;
                    }
                }

                if n == 0 {
                    self.active.store(false, Ordering::Relaxed);
                }
            }
        }

        pub struct Engine {
            pub buffers: RwLock<HashMap<String, Buffer>>,
            pub tracks: HashMap<String, Track>,
            pub channels: usize,
        }

        impl Engine {
            pub fn new(channels: usize, tracks: HashMap<String, Track>) -> Self {
                let buffers: RwLock<HashMap<String, Buffer>> = RwLock::new(HashMap::new());

                Self { buffers, tracks, channels }
            }

            pub fn process(&mut self, output: &mut [f32]) {
                let mut buffers = self.buffers.write().unwrap();

                for (_name, buf) in buffers.iter_mut() {
                    if buf.active.load(Ordering::Relaxed) {
                        buf.mix_into(output, self.channels);
                        buf.active.store(false, Ordering::Relaxed);
                    }
                }
            }

            // Processing threads
            pub fn spawn_worker(engine: &Arc<Mutex<Engine>>, cmd: &str) {
                let cmd = cmd.to_string();
                let engine = Arc::clone(engine);

                let _worker = thread::spawn(move ||
                    if let Some((ctor, args)) = cmd_match(&cmd) {
                        let mut args: VecDeque<&str> = args.split_whitespace().collect();
            
                        let track_name = args.pop_front().unwrap_or("");
                        let mut buf_name = track_name.to_string();

                        if let Some(next) = args.front() {
                            if *next == "as" {
                                args.pop_front();
                                if let Some(name) = args.pop_front() {
                                    buf_name = name.to_string();
                                }
                            }
                        }

                        let mut eng = engine.lock().unwrap();

                        let track = match eng.tracks.get(track_name) {
                            Some(t) => Arc::new(t.clone()),
                            None => {
                                eprintln!("Unknown track '{}'", track_name);
                                return;
                            }
                        };
                        let rb = SharedRb::<Heap<f32>>::new(4096);
                        let (mut prod, mut cons) = rb.split();
                        eng.buffers.write().unwrap().insert(
                                buf_name.clone(), 
                                Buffer::new(cons)
                        );
                        // each thread responsible for parsing args on its own
                        drop(eng);

                        ctor(track, args, prod);
                    }
                );
            }

            // helper functions for processing args
            pub fn args_pop_front(args: &mut VecDeque<&str>, or_val: usize) -> usize {
                args.pop_front()
                    .and_then(|a| a.parse::<usize>().ok())
                    .unwrap_or(or_val)
            }

            // Play:
            // fills engine's buffers with samples from track;
            // optionally parses arguments for start_frame (default: beginning of track)
            // and frame_count (default: length of track)
            pub fn Play(track: Arc<Track>, mut args: VecDeque<&str>, mut prod: Producer) {
                use ringbuf::traits::Producer as _;

                let mut start_frame = 0;
                let mut frame_count = track.total_frames;

                if !args.is_empty(){
                    start_frame = Self::args_pop_front(&mut args, 0);
                }

                if !args.is_empty() {
                frame_count = Self::args_pop_front(&mut args, track.total_frames - start_frame);
                }

                if start_frame >= track.total_frames {
                    eprintln!("start_frame {} out of range", start_frame);
                    return;
                }

                let end = (start_frame + frame_count).min(track.total_frames);
                let chunk = &track.samples[start_frame * track.channels.. end * track.channels];

                // feed samples into ring buffer in blocks
                let mut written = 0;
                while written < chunk.len() {
                    // non-blocking push
                    let n = prod.push_slice(&chunk[written..]);
                    written += n;
                    if n == 0 {
                        // if full, yield briefly (avoid busy loop)
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
            
            // will implement when I can get one cmd working
            // pub fn Stop(track: Arc<Track>, args: VecDeque<&str>, buf: Producer) {}
            // end processing threads
        } // end impl engine
    } // end mod engine
} // end mod audio_setup
