# ToneSend — Plan

Transfer data between devices over sound using many simultaneous frequencies (OFDM), optimised for transfer speed. Cross-platform. Audible and inaudible bands are both supported as **profiles**; the app picks the fastest one that the channel actually supports.

---

## 1. Key decisions

| Decision | Choice | Why |
|---|---|---|
| Modulation | **OFDM** (many subcarriers, each PSK/QAM) | Highest bits per Hz; "multiple frequencies" by design |
| Core language | **Rust** modem core | One DSP implementation for iOS, Android, desktop, web (WASM); fast, safe, easy FFI |
| Audio I/O | **cpal** (CoreAudio / AAudio / WASAPI / ALSA / WebAudio) + tiny native shims | Same capture/playback code everywhere; shims only for OS audio-session settings |
| UI | **Flutter** + `flutter_rust_bridge` | One UI for iOS, Android, macOS, Windows, Linux, web |
| Audible vs inaudible | **Both, as band profiles**, auto-selected by measured speed | User's rule: choose by transfer speed |
| Link mode | v1 one-way broadcast (FEC); v2 two-way (ARQ + rate negotiation) | Ship simple first, add adaptivity later |

Alternative if UI should feel fully native: keep the Rust core and write SwiftUI (iOS/macOS) + Jetpack Compose (Android) frontends. More work, better platform feel.

---

## 2. Band profiles (audible vs inaudible)

The modem is parameterised by band, so both modes share all code.

| Profile | Band | Bandwidth | Character |
|---|---|---|---|
| **Audible-Wide** | ~1–8 kHz (tuned per device) | ~7 kHz | Fastest, best SNR on phone speakers; sounds like a modem |
| **Audible-Narrow** | ~1.5–5 kHz | ~3.5 kHz | Robust fallback for noisy rooms / distance |
| **Inaudible** | ~17.5–20.5 kHz | ~3 kHz | Silent to most adults; phone speakers/mics roll off here → lower SNR, shorter range |

Rough expectation: Inaudible is **~2–4× slower** than Audible-Wide (less bandwidth *and* lower SNR so fewer bits per subcarrier). Real numbers are measured in Phase 2.

**How "depends on speed" is decided:**
- **One-way mode:** the user chooses *Fast (audible)* / *Silent (inaudible)* / *Auto*. In Auto, the sender transmits a short **sounding burst** in both bands before the data; receivers report nothing back, so Auto in one-way mode just uses a stored per-device-pair calibration or defaults to Audible-Wide.
- **Two-way mode:** both devices send a sounding burst covering both bands → receiver measures SNR per subcarrier → computes achievable goodput for each profile → replies with the choice. Rule: **use Inaudible if its goodput ≥ a user-set threshold (e.g. "silent unless more than 2× slower"), otherwise Audible-Wide.**
- Later option: **Hybrid** — use both bands simultaneously for maximum throughput (audible).

---

## 3. Modem design

**OFDM parameters (starting point, tuned later)**
- 48 kHz sample rate (44.1 kHz fallback — some Android devices/web)
- FFT 1024–2048 → subcarrier spacing ~23–47 Hz
- Cyclic prefix ~5–10 ms (covers room reverb)
- Data subcarriers = whatever fits the band profile (≈100–250 audible-wide, ≈60–120 inaudible)

**Modulation per subcarrier**
- v1: **DBPSK/DQPSK** (differential — no channel estimation needed)
- v2: pilots + equaliser → coherent **QPSK / 16-QAM**, **per-subcarrier bit loading** (more bits on clean frequencies)

**Framing**
- Preamble: **linear chirp** in the profile's band → correlation detection, timing, coarse clock-offset estimate
- Header (always most robust mode: DBPSK, r=1/2): magic, version, rate profile, payload length, CRC-16. The band is identified by which band's chirp was detected, so it isn't in the header.
- Payload: blocks with **FEC** (v1 convolutional + Viterbi, v2 LDPC) + **interleaver** across time/frequency + CRC-32 per block

**Rate profiles (v1, built):** Safe (DBPSK, r=1/2) · Normal (DQPSK, r=1/2) · Fast (DQPSK, r=3/4). Planned: Max (coherent QAM + bit loading).

---

## 4. Impairments → mitigations

| Problem | Mitigation |
|---|---|
| Room reverb / multipath | Cyclic prefix, equaliser |
| Sample-clock drift between devices | Drift estimate from preamble + pilot tracking, fractional resampling |
| Speaker/mic frequency roll-off (severe near 18–20 kHz) | Per-device band calibration; bit loading |
| High peak-to-average power → clipping | PAPR reduction (clipping+filtering), conservative headroom |
| OS mic processing (AGC, noise suppression, echo cancel) | iOS: `AVAudioSession` `.measurement` mode; Android: `AudioSource.UNPROCESSED` / `VOICE_RECOGNITION`; Web: `getUserMedia` with `echoCancellation/noiseSuppression/autoGainControl: false` |
| Inaudible band filtered out by some devices / browsers | Calibration detects it → Inaudible profile disabled on that device |
| Background noise | FEC, CRC, fallback to Safe / Audible-Narrow |

---

## 5. Architecture

```
tonesend/
├─ core/                 Rust crate "modem" — pure DSP, no I/O
│   ├─ modem.rs          frame encode/decode, block layout, drift tracking
│   ├─ fec.rs            convolutional + puncturing + soft Viterbi (later LDPC)
│   ├─ ofdm.rs           symbol modulator/demodulator, clip-and-filter (rustfft)
│   ├─ sync.rs           chirp preamble, matched filter, peak detection
│   ├─ profiles.rs       band + rate profiles (selection logic: phase 6)
│   ├─ util.rs           RNG, CRCs, bit packing, interleaver
│   └─ sim.rs            channel simulator: noise, reverb, drift, clipping, freq response
├─ cli/                  binary `tonesend`: info, encode, decode, channel, sim, bench
├─ audio/  app/          (not yet — phases 3 and 5)
├─ audio/                Rust crate: cpal capture/playback, ring buffers
├─ app/                  Flutter app (flutter_rust_bridge to core + audio)
│   ├─ ios/ android/     audio-session shims, mic permission
│   └─ web/              WASM build, WebAudio
└─ PLAN.md
```

The core is **testable entirely offline**: bytes → samples → simulated channel → bytes. The CLI makes real-device testing easy via WAV files before any app exists.

---

## 6. Phases

1. **Offline modem (Rust core + simulator + CLI).** Round-trip at increasing noise/reverb/drift; BER and goodput curves for each band profile.
2. **Real-world band measurement.** CLI plays a sweep/sounding signal from one device, records on another; measure usable band and SNR in audible vs inaudible on real phones/laptops. Also **measure RT60/DRR from the recorded chirp** to calibrate the simulator presets. **This produces the real audible-vs-inaudible speed numbers.** Known risk: the chirp detector normalises by full-band energy, so strong low-frequency ambient noise will lower detection scores; band-limit that energy term if recordings show it.
3. **Live receive.** cpal streaming → detector → decoder in real time on desktop, then iOS/Android.
4. **Speed tuning.** Pilots, coherent QAM, bit loading, rate profiles, FFT/CP tuning per band.
5. **Flutter app.** Send text/file, receive screen, profile picker (Fast / Silent / Auto), live goodput + error stats.
6. **Two-way mode.** Sounding handshake, automatic band + rate selection, ARQ for lost blocks.
7. *(Optional)* Hybrid both-band mode, web build polish.

### Phase 1 status — done (2026-11-14)

Implemented in `core/` (crate `modem`) and `cli/` (binary `tonesend`):
chirp preamble + analytic matched filter, differential OFDM (DBPSK/DQPSK),
K=7 convolutional code with puncturing (1/2, 2/3, 3/4) + soft Viterbi,
per-block CRC-32 / scrambling / interleaving, decision-directed clock-drift
tracking, clip-and-filter PAPR control, raised-cosine symbol windowing,
and a channel simulator (speaker/mic response, RT60/DRR reverb, ppm drift,
clipping, AWGN). 14 tests pass (`cargo test --release`).

Simulated goodput, 2 KiB payload (kbit/s, `tonesend bench --preset <p>`). SNR = white noise vs.
*transmitted* power, so the inaudible band pays for the speaker/mic roll-off above ~16 kHz:

| profile | ideal | desk @10 dB | desk @5 dB | desk @0 dB | room @20 dB | room @10 dB |
|---|---|---|---|---|---|---|
| wide/fast | 7.68 | **7.68** | 6.40 (partial) | ✗ | ✗ | ✗ |
| wide/normal | 5.25 | 5.25 | 5.25 | ✗ | partial | partial |
| wide/safe | 2.69 | 2.69 | 2.69 | 2.69 | **2.69** | 2.69 |
| narrow/normal | 2.63 | 2.63 | 2.63 | 2.63 | **2.63** | 2.63 |
| narrow/fast | 3.84 | 3.84 | 3.84 | partial | ✗ | ✗ |
| inaudible/fast | 3.39 | **3.39** | ✗ | ✗ | ✗ | ✗ |
| inaudible/normal | 2.30 | 2.30 | partial | ✗ | partial | ✗ |
| inaudible/safe | 1.17 | 1.17 | 1.17 | ✗ | 1.17 | 1.17 |

Findings (simulated — real numbers come from Phase 2):
- Close range (desk preset, DRR 12 dB): audible-wide/fast ≈ **7.7 kbit/s** vs inaudible/fast ≈ **3.4 kbit/s** (≈ 2.3× slower from bandwidth alone), and inaudible also loses ~5 dB of noise margin to the speaker roll-off. How big that roll-off is on real phones is a Phase 2 measurement.
- **With the assumed room presets (DRR 4 dB / RT60 0.4 s), reverb, not noise, is the limiter**: echo energy arriving after the 5.3 ms CP acts as a noise floor (≈ 7–9 dB post-detection SNR) regardless of loudness. The long-symbol `narrow` profile survives it; "far" (DRR 0 dB, RT60 0.6 s) fails everything. These preset values are guesses until measured.
- Drift tracking: accurate in clean conditions (≈ 99 ppm estimated for 100 ppm true, tests pass at ±150 ppm); under heavy reverb the estimate is noisy (e.g. +128 for 100, +27 for 0) but decoding still held.
- DQPSK r=1/2 needs ≈ 7.5 dB estimated SNR in both AWGN and reverb — FEC behaves consistently.

Phase 4 priorities from this: (1) long-CP / long-symbol profile for reverberant rooms, (2) coherent detection with pilots (~3 dB gain over differential), (3) bit loading, (4) automatic rate/band choice using the per-carrier SNR the decoder already reports.

### Phase 2 status — tooling done, real measurements pending (2026-11-14)

Built:
- `core/src/probe.rs`: 43 s probe signal (exponential sweep ×2, silence, periodic multitone,
  one 1 KiB test frame per band × rate) and its analysis → clock drift, impulse response
  (RT60, DRR), 1/3-octave frequency response, per-frequency SNR, per-profile predicted SNR
  (noise and echo-after-CP) and best rate, plus **actual** decode results of the test frames.
- `audio/` crate (cpal): playback/capture at 48 kHz mono on any OS.
- CLI: `tonesend devices | play | record`, `tonesend probe generate | analyze | run [--json]`.

Validated against the simulator (truth known), measured / true:

| preset | drift ppm | RT60 s | DRR dB |
|---|---|---|---|
| desk | 40.1 / 40 | 0.17 / 0.25 (underestimates when the direct sound dominates) | 12.1 / 12 |
| room | 79.8 / 80 | 0.41 / 0.40 | 4.1 / 4 |
| far | 100.2 / 100 | 0.59 / 0.60 | 0.1 / 0 |
| clean | −55.0 / −55 | ~0 / 0 | 50 / none |

Speaker roll-off is visible in the response. **The prediction is simulator-fitted** (rate
thresholds from decoder SNR: safe ≈5, normal ≈8, fast ≈10 dB, with a grey zone around each;
echo energy past the CP offset by +2.5 dB). On real hardware the *test-frame table is the
result*; the prediction is a hypothesis to check against it. Expect it to be optimistic on
real speakers: harmonic distortion is periodic, so the multitone does not count it as noise,
but data frames suffer from it. With one frame per profile per run, results near a threshold
are a coin flip: repeat each setup 2–3 times before drawing the audible-vs-inaudible verdict.
Test `prediction_is_not_optimistic` guards this.

How to measure:
1. Same machine: `tonesend probe run --save rec.wav --json report.json` (Mac speaker → Mac mic).
2. Two devices: `tonesend probe generate -o probe.wav`, play it on device A (AirDrop to the phone, or
   `tonesend play`), record on device B (`tonesend record -o rec.wav -s 50`, or a lossless phone recorder),
   then `tonesend probe analyze -i rec.wav`. iPhone Continuity mic shows up in `tonesend devices` too.
   Avoid AAC/MP3 recordings (they discard >16 kHz) and voice-processed mics.

Open questions the real runs must answer: actual speaker roll-off above 17 kHz (decides
inaudible), real DRR at 30 cm / 1 m / 3 m (decides whether the long-CP profile is needed),
whether phone mics are AGC/noise-suppressed.

First real run (2026-11-14, MacBook speaker → own mic, volume 44 %): all six audible profiles
decoded error-free — **wide/fast 7.14 kbit/s** goodput at ~19–20 dB SNR, narrow/fast 3.57 kbit/s.
Inaudible failed (safe 1/2 blocks, others lost); a level sweep showed SNR collapsing faster than
the playback level (20 → 11 → 4 dB for 0/−6/−12 dB), i.e. a level-dependent mic path (noise
suppression/gating) — the user reports the notebook mic is faulty, so inaudible on this Mac is
inconclusive. Fixes from that run: drift from sweep-to-sweep cross-correlation (template-based
locking picked different multipath arrivals: +146 ppm vs ~0 true), adjacent-period noise
estimate, band-pass before preamble detection (low-frequency room noise had diluted scores).

### Phases 3 + 5 (partial) — streaming receiver and iPhone app (2026-11-14)

- `core/src/stream.rs`: real-time `Receiver` (arbitrary chunks in, frames out, live status with
  progress). Verified identical to batch decoding (test + real recording, ~140× real time on M-series).
- `ffi/`: C ABI static library; `ios/`: SwiftUI app (AVAudioEngine, `.measurement` mode) with
  Send (text/photo/file, profile picker, air-time/speed estimate, progress), Receive (listen orb,
  level meter, incoming progress, feed), detail (content, speed, link rate, SNR, blocks, drift,
  per-carrier SNR chart), Lab (probe, raw WAV recording). Builds for device and simulator.
- `tonesend listen` (desktop live/streamed receive). Flutter / Android postponed: native iOS first.

---

## 7. Success metric

**Goodput** = correct payload bytes per second end-to-end (including preamble, FEC overhead, retries), reported per band profile with conditions (distance, volume, room noise).

Expectation: a few kbit/s audible at ~1 m in a quiet room; inaudible noticeably lower and shorter range. Sound is orders of magnitude slower than Wi-Fi/Bluetooth — fine for text, keys, small files, pairing.

---

## 8. Build vs reuse

- **ggwave** — MFSK, robust, slower; baseline reference, also cross-platform.
- **libquiet** — C OFDM modem; benchmark target.
- **Recommendation:** own Rust modem (speed tuning is the point of the project), benchmark against libquiet.

---

## 9. Testing notes

- Simulators/emulators can't validate acoustics → need physical devices (e.g. 2 phones, or laptop + phone).
- Permissions: iOS `NSMicrophoneUsageDescription`, Android `RECORD_AUDIO`, web HTTPS + mic prompt.
- Keep a WAV corpus of real recordings in the repo for regression tests.


### Transfer layer: checksum + repair (2026-11-14)

Feedback from the phones: small messages fine, bigger files arrived damaged (one long frame,
no way to repair). Now (`core/src/transfer.rs`, `core/src/rs.rs`):
- Files are split into 494-byte shards; each shard = one modem block (own CRC-32) with an
  18-byte header (transfer id, length, whole-file CRC-32, group, index) → any good block is
  usable on its own. 4 shards per frame (~2 KB, ~2 s), so a glitch costs one short packet.
- Reed–Solomon (GF(256), Cauchy, systematic) per group of ≤128 shards: sender adds 0/10/25/50 %
  repair shards; any k of a group rebuild it.
- "Send repair data" transmits new parity indices later; receiver merges them with the shards it
  kept (one-way, no back channel). Whole-file CRC-32 verified after reassembly.
- Tests: random loss within parity, burst loss → incomplete → repaired, 200 KB multi-group,
  audio end-to-end with a 0.5 s noise burst repaired from parity, C ABI round trip.
Next: automatic repair requests (receiver → sender over sound), adaptive rate.
### Speed (2026-11-14)

- New band **Max** (1–13 kHz, 256 carriers) and rate **Turbo** (D8PSK, FEC ¾).
  Simulated desk preset, 4 KiB: max/fast 13.2 kbit/s error-free down to 10 dB SNR;
  max/turbo 19.2 kbit/s down to ~25 dB (degrades at 20 dB). Previous best wide/fast 8.0.
- 8 shards (~4 KB) per frame instead of 4 → less preamble overhead.
- Drift tracker ignores symbols with abnormal power (noise bursts, knocks) and clamps its
  step: a 0.5 s burst now costs 2 blocks instead of the rest of the frame.
- Fixed: transfers reported incomplete while a long frame was still arriving.
Next for speed: coherent 16-QAM with pilots + per-carrier bit loading, adaptive rate via
a receiver → sender feedback chirp, shorter CP at close range.

### Data in music (2026-11-14)

`tonesend music`: mixes a short ID into a track (or a synthetic one), runs it through MP3/AAC/Opus,
counts decoded IDs. Silent band dies in MP3 ≤192 and AAC ≤128 (encoder low-pass). New
experimental band `high` (14.6–16.4 kHz, id 4) survives MP3/AAC 128 at −20 dB under the music.
Audible band needs −5…−10 dB (audible). Not yet in the iOS app.

### Compression in the framework (2026-11-14)

`core/src/content.rs` (+ FFI `ts_pack_*`, `ts_unpack`, `ts_image_*`): envelope v2 with Brotli
(quality 11, shared custom dictionary for short messages) used only when smaller, and photos
downscaled + WebP-encoded in Rust (libwebp). Measured: 6 KB doc −57 %, JSON −92 %, vCard −32 %,
URL −41 %; photos 53–62 % smaller than the app's previous JPEG q50. iOS app, CLI (`encode`,
`decode`, `embed`) use it. Found and fixed a streaming-receiver bug: a false detection on one band
(Audible chirp ≈ start of Max chirp) could hide a real frame on another band.

### Field log → drift limit and copy combining (2026-11-15)

First field log (iPhone 11 Pro listening to the song with an embedded ID from a Mac): 8 of 12
copies decoded at ~10 dB SNR. Failures: two weak copies, two unreadable headers, and one where the
drift tracker ran to −400 ppm (pair is really +20…+38 ppm). Fixes in `core`: drift limited to
±200 ppm, drift hint from good packets plus steadier retry decodes when blocks fail, assumed
header for strong preambles, and soft combining of repeated copies (see signal-processing.md).
Simulation at −10 dB: 0.8 → 7.5 of 12 copies decoded, delivery 3/6 → 6/6 runs.

Second field log (same song, new build): 12 of 13 copies decoded (SNR 9–21 dB). The one failure
(SNR 0.7 dB over the whole band) survived all 3 drift attempts, so it was interference, not drift.
It also showed iOS delivering exact digital silence, which gave absurd detection scores; fixed
with an energy floor in `sync::correlate_normalized`.
