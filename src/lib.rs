use std::thread;
use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc, 
    RwLock,
    atomic::{AtomicBool, AtomicUsize, Ordering}
};

use ringbuf::RingBuffer;

use crossbeam_channel::{unbounded, Sender, Receiver};

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::platform::{Host, Device};

use hound::{SampleFormat, WavReader};

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

    pub mod engine {
        use super::*;

        pub struct Engine {
            pub buffers: HashMap<&'static str, Buffer>,
            pub tracks: HashMap<&'static str, Track>,
            pub channels: u32,
        }

        impl Engine {
            pub fn new(channels: u32, tracks: HashMap<&'static str, Track>) -> Self {
                let buffers: HashMap<&'static str, Buffer> = HashMap::new();

                Self { buffers, tracks, channels }
            }

            pub fn process(&self, output: &mut [f32]) {
                for buf in &self.buffers {
                    if buf.active.load(Ordering::Relaxed) {
                        buf.mix_into(output, self.channels);
                        buf.active.store(false, Ordering::Relaxed);
                    }
                }
            }
        }

        pub struct Buffer {
            pub ring: ringbuf::Consumer<f32>,
            // ringbuf::Producer sent to Worker thread
            pub active: AtomicBool,
            pub gain: f32,
        }

        impl Buffer {
            pub new(ring: ringbuf::Consumer<f32>) -> Self {
                Self {
                    ring,
                    active: AtomicBool::new(true),
                    gain: 1.0f32
                }
            }

            pub fn mix_into(&self, output: &mut [f32], channels: u32) {
                let mut tmp = [0.0f32; 4096];
                let n = self.ring.pop_slice(&mut tmp[..output.len() / channels]);
                for i in 0..n {
                    for ch in 0..channels {
                        output[i * channels + ch] += tmp[i] * self.gain;
                    }
                }

                if n == 0 {
                    self.active.store(false, Ordering::Relaxed);
                }
            }
        }
    } // end pub mod engine

    pub mod tracks {
        use super::*;

        pub struct Track {
            pub path: String,
            pub channels: usize,
            pub data: Vec<f32>,
            pub total_frames: usize,
            pub sample_rate: usize,
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
            fn resample_to_device_rate(track: &Track, out_rate: usize) -> Vec<f32> {
                if track.sample_rate == out_rate {
                    return track.data.clone();
                }

                println!("Resampling {}", track.path);

                let nch = track.channels;
                let input: Vec<Vec<f32>> = (0..nch)
                    .map(|ch| track.data.iter().skip(ch).step_by(nch).copied().collect())
                    .collect();

                let mut resampler = FftFixedIn::<f32>::new(
                    track.sample_rate, out_rate, 1024, 1, nch
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

            pub fn new(path: &str, out_rate: usize) -> anyhow::Result<Self> {
                println!("Loading {path}...");
                // Decode
                let (mut data, spec) = decode_to_f32(path)?;
                let channels = spec.channels as usize;
                let sample_rate = spec.sample_rate as usize;

                if sample_rate != out_rate {
                    data = resample_to_device_rate(&track, out_rate);

                // Compute total frames in the source
                let total_frames = data.len() / channels;
 
                println!("Finished loading {path}");
                Ok(Self { 
                    path: path.to_string(), 
                    channels, 
                    data, 
                    total_frames,
                    sample_rate,
                )
            }
        }

    } // end pub mod tracks
} // end mod audio_setup

// re-export
pub use crate::audio_setup::{
    device::CpalDevice,
    engine::{Engine},
    tracks::Track
};

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

// re-export
pub use crate::concurrency::threadpool::ThreadPool;

// Commands
//
use std::sync::RwLock;

// consistent match statement expansion
macro_rules! cmd_table {
    ($($key:literal => $handler:path),* $(,)?) => {{
        fn lookup<'a>(cmd: &str) -> Option<fn(&str)> {
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

static CORE_CMDS: fn(&str) -> Option<fn(&str)> = cmd_table! {
    "start" => Engine::Play,
    "stop" => Engine::Stop,
};

lazy_static::lazy_static! {
    static ref EXT_CMDS: RwLock<HashMap<String, fn(&str)> = RwLock::new(HashMap::new());
}

pub mod command {
    use super::*;

    // user will be able to load their own commands at run time
    pub fn register_command(name: &str, handler: fn(&str)) {
        EXT_CMDS.write().unwrap().insert(name.to_string(), handler);
    }

    pub fn match_cmd(line: &str) -> Option<fn(Arc<Track>, Arc<VecDeque>, Arc<ringbuf::Producer<F32>), &str> {
        let mut parts = line.splitn(2, ' ');
        let cmd_name = parts.next()?;
        let args = parts.next().unwrap_or("");

        // check compile-time core commands first
        if let Some(ctor, args) = CORE_CMDS(cmd_name) {
            return Some(ctor, args);
        }

        // fall back to dynamically-registered commands
        if let Some(ctor, args) = EXT_CMDS.read().unwrap().get(cmd_name) {
            return Some(ctor, args);
        }

        None
    }

    pub fn Play(track: Arc<Track>, args: VecDeque<&str>, buffer: ringbuf::Producer<F32>) {
        let samples = &track.samples;

        let end = (start_frame + frame_count).min(samples.len());
        let chunk = &samples[start_frame..end];

        // feed samples into ring buffer in blocks
        let mut written = 0;
        while written < chunk.len() {
            // non-blocking push
            let n = prod_ref.push_slice(&chunk[written..]);
            written += n;
            if n == 0 {
                // if full, yield briefly (avoid busy loop)
                std::thread::sleep(std::time::Duration::from_millis(1));
            } else {
                // mark buffer active once there’s content
                buf.active.store(true, std::sync::atomic::Ordering::Release);
            }
        }

        // mark inactive when done filling
        buf.active.store(false, std::sync::atomic::Ordering::Release);
    }
}

// re-export
pub use crate::command::{match_cmd, Command};
