//! Capture and playback through PipeWire (cpal's PipeWire host, ADR 0009 decision 1). Each
//! stream lives on its own thread (cpal streams cannot move between threads) until its stop
//! flag is set. Capture never starts on its own: only on an explicit request (the owner's
//! consent to record is a product action, never a test).

use crate::{AudioError, Pcm, RATE};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A running stream; stops when dropped.
pub struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Running {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(std::thread::JoinHandle::is_finished)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

fn derr(e: impl std::fmt::Display) -> AudioError {
    AudioError::Device(e.to_string())
}

/// Starts the stream built by `build` on its own thread; returns once it plays (or failed).
fn run<B>(name: &str, build: B) -> Result<Running, AudioError>
where
    B: FnOnce(Arc<AtomicBool>) -> Result<cpal::Stream, AudioError> + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    let s2 = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let stream = match build(Arc::clone(&s2)).and_then(|st| st.play().map(|()| st).map_err(derr)) {
                Ok(st) => {
                    let _ = tx.send(Ok(()));
                    st
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            while !s2.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            drop(stream);
        })
        .map_err(derr)?;
    match rx.recv() {
        Ok(Ok(())) => Ok(Running {
            stop,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => Err(AudioError::Device("audio thread ended".into())),
    }
}

/// Records the default input (48 kHz mono); `on_samples` runs on PipeWire's thread and must
/// not block (hand the samples to another thread).
pub fn capture(on_samples: impl FnMut(&[f32]) + Send + 'static) -> Result<Running, AudioError> {
    run("vibenote-capture", move |_stop| {
        let host = cpal::default_host();
        let dev = host
            .default_input_device()
            .ok_or_else(|| AudioError::Device("no input device".into()))?;
        let cfg = cpal::StreamConfig {
            channels: 1,
            sample_rate: RATE,
            buffer_size: cpal::BufferSize::Default,
        };
        let mut f = on_samples;
        dev.build_input_stream(
            cfg,
            move |buf: &[f32], _: &cpal::InputCallbackInfo| f(buf),
            |_| {},
            None,
        )
        .map_err(derr)
    })
}

/// Plays `pcm` from frame `from` on the default output; the stream ends by itself at the end.
/// `position` follows the frame being played.
pub fn play(pcm: Arc<Pcm>, from: usize, position: Arc<AtomicUsize>) -> Result<Running, AudioError> {
    run("vibenote-playback", move |stop| {
        let host = cpal::default_host();
        let dev = host
            .default_output_device()
            .ok_or_else(|| AudioError::Device("no output device".into()))?;
        let ch = usize::from(pcm.channels.max(1));
        let cfg = cpal::StreamConfig {
            channels: pcm.channels.max(1),
            sample_rate: pcm.rate,
            buffer_size: cpal::BufferSize::Default,
        };
        position.store(from, Ordering::Release);
        let mut at = from.saturating_mul(ch);
        dev.build_output_stream(
            cfg,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let src = pcm.samples.get(at..).unwrap_or(&[]);
                let n = src.len().min(out.len());
                if let (Some(dst), Some(s)) = (out.get_mut(..n), src.get(..n)) {
                    dst.copy_from_slice(s);
                }
                at = at.saturating_add(n);
                position.store(at.checked_div(ch).unwrap_or(0), Ordering::Release);
                if n < out.len() {
                    stop.store(true, Ordering::Release); // the end: the thread drops the stream
                }
            },
            |_| {},
            None,
        )
        .map_err(derr)
    })
}
