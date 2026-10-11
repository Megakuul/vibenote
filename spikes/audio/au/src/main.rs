//! Audio spike (Phase 2.8): PipeWire capture via cpal, Opus encoding, timing for ink↔audio sync.
//!
//! `au loop <device> <secs>`: plays a 20 ms 1 kHz beep every 500 ms into <device> (a null sink)
//! and records <device>'s monitor. For each beep: write time (CLOCK_MONOTONIC when the first beep
//! sample was handed to PipeWire) vs. capture estimate (input-callback time − frames after onset).
//! Difference = loopback delay; its jitter bounds the timestamp error for ink sync.
//! Also encodes the captured audio to Opus (48 kHz mono, 20 ms frames, 32 kbit/s) and measures CPU.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

fn mono_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn cpu_s() -> f64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 / 1e6 + ru.ru_stime.tv_sec as f64 + ru.ru_stime.tv_usec as f64 / 1e6
}

const SR: u32 = 48_000;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let dev_name = a.get(2).cloned().unwrap_or("vibespike".into());
    let secs: u64 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(20);
    let host = cpal::default_host();
    let find = |it: Box<dyn Iterator<Item = cpal::Device>>| it.into_iter().find(|d| d.description().map(|x| x.name() == dev_name).unwrap_or(false));
    let out = find(Box::new(host.output_devices()?)).ok_or_else(|| anyhow::anyhow!("no output {dev_name}"))?;
    let inp = find(Box::new(host.input_devices()?)).ok_or_else(|| anyhow::anyhow!("no input {dev_name}"))?;
    let cfg = cpal::StreamConfig { channels: 1, sample_rate: SR, buffer_size: cpal::BufferSize::Default };

    let writes: Arc<Mutex<Vec<u64>>> = Arc::default();
    let w2 = writes.clone();
    let mut n: u64 = 0;
    let period = SR as u64 / 2;
    let beep = SR as u64 / 50;
    let ostream = out.build_output_stream(
        cfg.clone(),
        move |buf: &mut [f32], _info: &cpal::OutputCallbackInfo| {
            let t = mono_ns();
            for (i, s) in buf.iter_mut().enumerate() {
                let k = n % period;
                if k == 0 && n > SR as u64 {
                    w2.lock().map(|mut v| v.push(t + i as u64 * 1_000_000_000 / SR as u64)).ok();
                }
                *s = if k < beep && n > SR as u64 { 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * (n as f32 / SR as f32)).sin() } else { 0.0 };
                n += 1;
            }
        },
        |e| eprintln!("out err {e}"),
        None,
    )?;
    let caps: Arc<Mutex<(Vec<u64>, Vec<f32>, Vec<usize>, bool)>> = Arc::default();
    let c2 = caps.clone();
    let istream = inp.build_input_stream(
        cfg.clone(),
        move |buf: &[f32], _info: &cpal::InputCallbackInfo| {
            let t = mono_ns();
            if let Ok(mut c) = c2.lock() {
                c.2.push(buf.len());
                for (i, &s) in buf.iter().enumerate() {
                    let on = s.abs() > 0.1;
                    if on && !c.3 {
                        let est = t - (buf.len() - i) as u64 * 1_000_000_000 / SR as u64;
                        c.0.push(est);
                    }
                    c.3 = on || (c.3 && s.abs() > 0.01);
                }
                c.1.extend_from_slice(buf);
            }
        },
        |e| eprintln!("in err {e}"),
        None,
    )?;
    let cpu0 = cpu_s();
    istream.play()?;
    ostream.play()?;
    std::thread::sleep(std::time::Duration::from_secs(secs));
    drop(ostream);
    drop(istream);
    let cpu_capture = cpu_s() - cpu0;
    let (onsets, audio, cb, _) = std::mem::take(&mut *caps.lock().map_err(|_| anyhow::anyhow!("lock"))?);
    let writes = writes.lock().map_err(|_| anyhow::anyhow!("lock"))?.clone();
    // pair each write with the first onset after it
    let mut d: Vec<f64> = vec![];
    for w in &writes {
        if let Some(o) = onsets.iter().find(|&&o| o + 50_000_000 > *w && o < w + 400_000_000) {
            d.push((*o as f64 - *w as f64) / 1e6);
        }
    }
    let mut ds = d.clone();
    ds.sort_by(|a, b| a.total_cmp(b));
    let mean = d.iter().sum::<f64>() / d.len().max(1) as f64;
    let sd = (d.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / d.len().max(1) as f64).sqrt();
    // Opus encode of the captured audio
    let mut enc = opus::Encoder::new(SR, opus::Channels::Mono, opus::Application::Voip)?;
    enc.set_bitrate(opus::Bitrate::Bits(32_000))?;
    let t0 = std::time::Instant::now();
    let mut bytes = 0usize;
    let mut out_buf = vec![0u8; 4000];
    for fr in audio.chunks_exact(960) {
        bytes += enc.encode_float(fr, &mut out_buf)?;
    }
    let enc_s = t0.elapsed().as_secs_f64();
    let audio_s = audio.len() as f64 / SR as f64;
    let mut cbs = cb.clone();
    cbs.sort();
    println!(
        "{}",
        serde_json::json!({
            "seconds": secs, "beeps_written": writes.len(), "beeps_matched": d.len(),
            "loopback_ms": {"min": ds.first(), "p50": ds.get(ds.len()/2), "max": ds.last(), "mean": mean, "sd": sd},
            "input_callback_frames": {"p50": cbs.get(cbs.len()/2), "max": cbs.last()},
            "cpu_capture_playback_pct_of_one_core": cpu_capture / secs as f64 * 100.0,
            "opus": {"audio_s": audio_s, "encode_s": enc_s, "realtime_factor": audio_s / enc_s.max(1e-9), "kbytes_per_min": bytes as f64 / audio_s * 60.0 / 1000.0}
        })
    );
    Ok(())
}
