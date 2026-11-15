# Measurements

Results so far. Simulated numbers come from `tonesend bench` (reproducible); real-device numbers from
`tonesend probe` runs. Dates are 2026-11.

## Simulated goodput

`tonesend bench --preset desk --bytes 4096` (kbit/s; `*` = some blocks lost; RT60 0.25 s, DRR 12 dB,
40 ppm drift, speaker/mic 300 Hz–16 kHz):

| Profile | Ideal | 40 dB | 30 dB | 25 dB | 20 dB | 15 dB | 10 dB |
|---|---|---|---|---|---|---|---|
| max/turbo | 19.19 | 19.19 | 19.19 | 19.19 | 18.39* | 13.59* | 0* |
| max/fast | 13.21 | 13.21 | 13.21 | 13.21 | 13.21 | 13.21 | 13.21 |
| max/normal | 9.03 | 9.03 | 9.03 | 9.03 | 9.03 | 9.03 | 9.03 |
| wide/turbo | 11.70 | 11.70 | 11.70 | 11.70 | 11.70 | 10.72* | 0.97* |
| wide/fast | 7.98 | 7.98 | 7.98 | 7.98 | 7.98 | 7.98 | 7.98 |
| narrow/turbo | 5.91 | 5.91 | 5.91 | 5.91 | 5.91 | 5.91 | 5.91 |
| inaudible/turbo | 5.16 | 5.16 | 4.95* | 4.95* | 4.73* | 1.94* | 0* |

Earlier 2 KiB sweeps (room preset, RT60 0.4 s, DRR 4 dB): wide/fast fails at every SNR because
echo arriving after the 5.3 ms cyclic prefix acts as a noise floor; narrow/normal (10.7 ms CP)
holds 2.63 kbit/s to 15 dB; safe rates survive. **At room distance, echo, not loudness, limits
speed.**

## Decoder thresholds

Average decoder-estimated SNR needed for reliable decoding (simulation, noise- and
echo-limited channels): safe ≈ 5 dB, normal ≈ 8 dB, fast ≈ 10 dB, turbo ≈ 16 dB. There is a grey
zone of ~1 dB around each.

## Measurement tool validation

Probe analysis of simulated channels (measured / true):

| Preset | Drift (ppm) | RT60 (s) | DRR (dB) |
|---|---|---|---|
| desk | 40.1 / 40 | 0.17 / 0.25 (underestimates when direct sound dominates) | 12.1 / 12 |
| room | 79.8 / 80 | 0.41 / 0.40 | 4.1 / 4 |
| far | 100.2 / 100 | 0.59 / 0.60 | 0.1 / 0 |
| clean | −55.0 / −55 | ~0 / 0 | 50 / ∞ |

## Real devices

### MacBook Pro speaker → own microphone (output volume ~44 %)

| Profile | Result | SNR | Goodput |
|---|---|---|---|
| wide/safe, normal, fast | OK | 18–20 dB | 2.62 / 4.95 / **7.14** kbit/s |
| narrow/safe, normal, fast | OK | 16.5–17 dB | 1.31 / 2.48 / 3.57 kbit/s |
| inaudible/* | failed / partial | ~8 dB | — |

Clock drift ≈ 0 (same machine). A level sweep showed SNR falling much faster than the playback
level (20 → 11 → 4 dB for 0/−6/−12 dB), which points to processing in the microphone path; the
notebook microphone was reported faulty, so the inaudible result is inconclusive.

Lessons that changed the code: band-pass before preamble detection (low-frequency room noise
diluted detection scores 0.24 → 0.97), sweep-to-sweep drift measurement (template matching
locked onto different multipath arrivals), adjacent-period noise estimation.

### iPhone ↔ iPhone (iPhone 11 Pro + second iPhone)

Text and small files: received correctly. Larger files arrived damaged before the transfer
layer existed; this led to packetisation, per-block repair data and *Send repair data*.
Systematic per-profile numbers from the phones are still to be collected; use
**Lab → Play probe / Record raw audio** and `tonesend probe analyze`.

## Data embedded in music (`tonesend music`)

Synthetic 30 s pop track (RMS −20 dBFS; its own energy in 17.5–20.5 kHz is 22 dB below its
total, mostly hi-hats). 12-byte ID, rate `safe`, repeated every 3 s (10 copies). Encoded
stereo 48 kHz with ffmpeg (libmp3lame, aac, aac_at = Apple AudioToolbox, libopus), decoded
back, IDs counted. Full table in [overview.md](overview.md#test-results).

Key numbers: High band (14.6–16.4 kHz) survives MP3 ≥128 and AAC ≥128 at −20 dB with ~12 dB
decoder SNR; Silent band needs MP3 320 / AAC 256 / Opus; MP3 96 and AAC 64 remove both. The
audible Wide band needs ≥ −10 dB relative to the music (audible).

## Open questions

- Speaker/mic response of the phones above 8 kHz (decides Max vs Audible) and above 17 kHz
  (decides Silent).
- Real direct-to-reverberant ratio at 30 cm / 1 m / 3 m (decides whether longer-CP profiles are
  needed).
- Turbo's real-world success rate at short range.
- Real songs and real distribution chains (streaming apps, TV/radio, Bluetooth speakers) for
  embedded IDs; audibility of the High band to listeners of different ages.
