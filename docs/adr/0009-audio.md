# ADR 0009 — Audio recording, playback and ink sync

- **Status:** Accepted (locked) — Gate B (partial), 2026-10-11, owner approved. Owner: symphonia (MPL-2.0) allowed for audio import.
- **Date:** 2026-10-11
- **Evidence:** `spikes/audio/` (`au loop`). The system is PipeWire 1.6.9 (Pulse compatibility
  server). A temporary null sink `vibespike` was created with
  `pactl load-module module-null-sink` and removed afterwards. **No microphone was recorded.**

## Requirements
R-AUDIO-001 (record via PipeWire + import files), R-AUDIO-002 (tapping a stroke plays from when
it was written, ±100 ms), R-AUDIO-003 (recording never affects inking), R-DATA-002 (10 h
audio per note).

## Candidates
| Crate | Licence | Role |
|---|---|---|
| cpal 0.18.2, feature `pipewire` (uses pipewire-rs 0.10, MIT) | Apache-2.0 | capture/playback; native PipeWire backend (`cpal::available_hosts()` = PipeWire, Alsa) |
| opus 0.4 (binding) + libopus 1.6.1 | MIT/Apache + BSD-3-Clause | storage codec |
| ogg 0.9 | BSD-3-Clause | container |
| symphonia 0.6.1 | **MPL-2.0** | decoding imported MP3/FLAC/WAV/Ogg Vorbis; not measured |
| hound 3.5 | Apache-2.0 | WAV import (alternative) |

## Results [measured, loopback through the null sink]
A 20 ms 1 kHz beep every 500 ms is played into the null sink and its monitor recorded, mono
48 kHz. Delay = onset in the capture (input-callback CLOCK_MONOTONIC − frames after the onset)
minus write time (output-callback CLOCK_MONOTONIC + frame offset).

| Run | Beeps matched | Loopback delay mean / sd / min / max | Input callback size | CPU (capture + playback) |
|---|---|---|---|---|
| 20 s | 37/38 | 21.38 / 0.044 / 21.22 / 21.47 ms | 1,024 frames (21.3 ms) | 0.38 % of one core |
| 45 s, while the pen-latency test ran | 87/88 | 21.38 ms mean | 1,024 frames | 0.37 % |

- The delay equals one PipeWire quantum (1,024 / 48,000 = 21.3 ms) with a 44 µs standard
  deviation. Timestamping the audio as callback time − frames gives sample times accurate to
  about one quantum. With a constant offset correction it is far better than R-AUDIO-002's
  ±100 ms.
- Opus (Voip, 32 kbit/s, 20 ms frames): **176× real time** encoding on one core; 114.7 kB per
  minute ≈ 6.9 MB/h, so 10 h ≈ 69 MB per note (fine for R-DATA-002, stored in the blob store).
- **Ink latency while recording** (variant c, Mailbox, same workload as Phase 0 c2):
  9.34 / 11.86 / 12.17 ms vs. 9.18 / 11.64 / 11.96 ms without recording. That is within run
  noise, so R-AUDIO-003 holds [sim].
- At stream teardown cpal reported "Device disconnected" on both streams; recording had
  already ended. To be handled as a normal stop in the product.

## Decision
1. **cpal with the native PipeWire backend** for capture and playback, on its own real-time
   thread. Samples go to a lock-free ring buffer, then to an encoder thread, then to the blob
   store in chunks. Nothing audio-related touches the input/render thread.
2. **Opus in Ogg** (libopus, BSD) for recordings. The recording is chunked (e.g. 10 s pages),
   so a crash loses at most the last chunk: each chunk is durably committed, as in the ADR 0005
   blob protocol. The recording session row stores the CLOCK_MONOTONIC ↔ sample-index mapping.
3. **Sync:** every stroke stores its CLOCK_MONOTONIC start time (already captured for input).
   Recording sessions store the monotonic time of sample 0 (corrected by the measured
   quantum offset). Seeking = (stroke time − session start) − lead-in (default 2 s per SPEC).
4. **Import:** owner decision needed:
   - (a) **symphonia** (MPL-2.0, pure Rust, broad format support). MPL is file-level weak
     copyleft: unmodified use is fine, but modified symphonia files must be published.
   - (b) a permissive set without MP3 or with a separate decoder:
     - hound (WAV)
     - claxon (FLAC; licence to be checked)
     - lewton (Vorbis; licence to be checked)
     - libopus (Opus)

   Recommendation (a). It is not GPL-family, but AGENTS.md asks for owner approval of anything
   not clearly permissive.

## Open
- Real microphone capture, device switching (headset plug/unplug), and permission prompts
  (xdg-desktop-portal is not needed for PipeWire on this system) are not tested; they need
  the owner's consent to record.
- Playback scrubbing UI and waveform rendering are Phase 4 work.
