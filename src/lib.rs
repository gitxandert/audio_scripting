use std::thread;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

use crossbeam_channel::{unbounded, Sender, Receiver};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::platform::{Host, Device};

use hound::{SampleFormat, WavReader};

pub mod audio_setup {
    use super::*;

    pub mod device {
        use super::*;

        pub struct CpalDevice {
            host: Host,
            device: Device,
            cfg: cpal::StreamConfig,
        }

        impl CpalDevice {
            pub fn new() -> anyhow::Result<Arc<Self>> {
                let host = cpal::default_host();
                let device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("no output device"))?;
                let out_cfg = device.default_output_config()?;
                let cfg: cpal::StreamConfig = out_cfg.clone().into();

                Ok(Arc::new(Self { host, device, cfg }))
            }
        }
    } // end pub mod device

    pub mod tracks {
        use super::*;

        pub struct Track {
            path: String,
            in_ch: usize,
            data: Arc<Vec<f32>>, 
            pos: Arc<AtomicUsize>,
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
            pub fn new(path: &str) -> anyhow::Result<Arc<Self>> {
                println!("Loading {path}...");
                // Decode
                let (mut data, spec) = decode_to_f32(path)?;
                let in_ch = spec.channels as usize;

                // Share buffers
                let data = Arc::new(data);
                let pos  = Arc::new(AtomicUsize::new(0)); // index in *frames* (not samples)

                // Compute total frames in the source
                let total_frames = data.len() / in_ch;
 
                println!("Finished loading {path}");
                Ok(Arc::new(Self { 
                    path: path.to_string(), 
                    in_ch, 
                    data, 
                    pos, 
                    total_frames 
                }))
            }
        }
    } // end pub mod tracks
} // end mod audio_setup

// re-export
pub use crate::audio_setup::{
    device::CpalDevice,
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
