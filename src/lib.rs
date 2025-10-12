use std::thread;
use std::collections;:HashMap;
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize}};

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
            pub tracks: HashMap<String, Arc<TrackState>>,
            pub cmd_rx: Receiver<Command>,
        }

        impl Engine {
            pub fn new(cmd_rx: Receiver<Command>) -> Self {
                let mut tracks: Hashmap<String, Arc<TrackState>> = HashMap::new();

                Self {
                    tracks,
                    cmd_rx,
                }
            }
            //
            // pub fn run(cmd: Command) {
            //   match cmd.target {
            //      "Track" => {
            //          TODO : start producer thread to fill up buffer
            //      }
            //   }   
            // 
        }

        pub struct TrackState {
            pub name: String,
            pub buffer_producer: ringbuf::Producer<f32>,
            pub buffer_consumer: ringbuf::Consumer<f32>,
            pub active: AtomicBool,
            pub position: AtomicUsize,
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

            pub fn new(path: &str, out_rate: usize) -> anyhow::Result<Arc<Self>> {
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
                Ok(Arc::new(Self { 
                    path: path.to_string(), 
                    channels, 
                    data, 
                    total_frames,
                    sample_rate,
                }))
            }
        }

    } // end pub mod tracks
} // end mod audio_setup

// re-export
pub use crate::audio_setup::{
    device::CpalDevice,
    engine::{Engine, TrackState},
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
