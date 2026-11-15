# Signal processing

How bits become sound and back. Code: `core/src/{profiles,ofdm,sync,fec,modem}.rs`.

## Why OFDM

Speed over sound comes from using **many frequencies at once**. OFDM (orthogonal frequency-
division multiplexing) splits the band into hundreds of narrow, equally spaced subcarriers, each
carrying a few bits per symbol. Symbols are long (~27 ms), which tolerates echo, and the
receiver separates subcarriers with one FFT. Simpler schemes (one tone at a time, MFSK) are more
robust but much slower per Hz.

## Numerology (band profiles)

All audio is **48 kHz mono float32**.

| Band | Frequencies | FFT | Spacing | Carriers | Cyclic prefix | Symbol | Symbols/s |
|---|---|---|---|---|---|---|---|
| `max` (Max) | 1031–12984 Hz | 1024 | 46.9 Hz | 256 | 256 (5.3 ms) | 26.7 ms | 37.5 |
| `wide` (Audible) | 1031–7969 Hz | 1024 | 46.9 Hz | 149 | 256 (5.3 ms) | 26.7 ms | 37.5 |
| `narrow` (Robust) | 1500–4992 Hz | 2048 | 23.4 Hz | 150 | 512 (10.7 ms) | 53.3 ms | 18.75 |
| `inaudible` (Silent) | 17531–20484 Hz | 1024 | 46.9 Hz | 64 | 256 (5.3 ms) | 26.7 ms | 37.5 |
| `high` (experimental) | 14625–16359 Hz | 1024 | 46.9 Hz | 38 | 256 (5.3 ms) | 26.7 ms | 37.5 |

Other parameters: raised-cosine overlap between symbols `win` = 48 samples (64 for narrow);
receiver FFT window starts `backoff` = 24 samples early inside the CP; peak clipping at
3 × RMS; preamble chirp 4096 samples + 1024 guard (8192 + 2048 for narrow).

## Rate profiles

| Rate | Modulation | Bits/carrier | FEC | Info bits/carrier | Needed SNR* |
|---|---|---|---|---|---|
| `safe` | DBPSK | 1 | ½ | 0.5 | ≈ 5 dB |
| `normal` | DQPSK | 2 | ½ | 1.0 | ≈ 8 dB |
| `fast` | DQPSK | 2 | ¾ | 1.5 | ≈ 10 dB |
| `turbo` | D8PSK | 3 | ¾ | 2.25 | ≈ 16 dB |

*Average per-carrier SNR as estimated by the decoder; calibrated in simulation.
Raw bit rate = carriers × bits/carrier × symbols/s; e.g. max/turbo = 256 × 3 × 37.5 = 28.8 kbit/s,
19.2 kbit/s net after FEC, CRC and framing.

## Frame structure

```text
| chirp | guard | training | header | payload symbols … | tail |
   85ms   21ms    1 sym     1–3 sym   N sym               win
```

1. **Chirp preamble:** a linear sweep across the band. The receiver finds it with a matched
   filter, which gives the frame start to ±1 sample and identifies the band.
2. **Training symbol:** known pseudo-random QPSK phases; the phase reference for differential
   decoding.
3. **Header:** always DBPSK, rate ½: payload length, rate profile, CRC-16 (see
   [protocol.md](protocol.md#frame-header)).
4. **Payload:** blocks of ≤512 bytes, each independently scrambled, CRC-32 protected,
   convolutionally coded, punctured and interleaved.

### OFDM symbol generation (`ofdm.rs`, `modem.rs`)

- Subcarrier values → Hermitian spectrum → inverse FFT → real samples.
- **Peak limiting:** OFDM has a high peak-to-average ratio. Each symbol is clipped at 3 × RMS,
  then re-filtered in the frequency domain to remove out-of-band clipping products (important
  for the inaudible band: no audible splatter).
- **Cyclic prefix:** the last `cp` samples are repeated in front of each symbol, so echoes
  shorter than the CP do not cause interference between symbols.
- **Windowing:** raised-cosine ramps over `win` samples, overlap-added with the neighbour,
  avoid clicks and spectral splatter at symbol boundaries.
- The whole frame is normalised to peak 0.9.

## Differential modulation

Each symbol is encoded as a **phase change** relative to the previous symbol on the same
subcarrier (the training symbol is the first reference). The receiver computes
`d = Y_t · conj(Y_{t-1})` per subcarrier, so the unknown channel phase cancels: no channel
estimation or pilots are needed.

- DBPSK: increments {0, π}.
- DQPSK: Gray-coded increments {±π/4, ±3π/4}; the two bits are the signs of Re(d), Im(d).
- D8PSK: increments (m + ½)·π/4, bit label = Gray code of m.

**Soft bits (LLRs)** for the Viterbi decoder are weighted per subcarrier by its measured
reliability: `LLR ∝ Re(d) · 2P_k / noise_k`, with `P_k` the mean |d| and `noise_k` the decision-error
variance on subcarrier k (regularised toward the band average). D8PSK uses max-log LLRs over
the eight points. Weak frequencies (speaker roll-off, notches) therefore count less.

## Synchronisation

### Detection (`sync.rs`, `stream.rs`)
- The input is **band-pass filtered** (511-tap linear-phase FIR) to the profile's band, so
  out-of-band noise (e.g. low-frequency room rumble) does not dilute the score.
- **Normalised analytic matched filter:** `|Σ x·conj(chirp_analytic)| / (‖x‖‖chirp‖)`, which
  is ~1 for a clean preamble and ~0 for noise, independent of carrier phase. Overlap-save FFT.
- Peaks ≥ 0.2 are candidates; the earliest point ≥ 60 % of the peak within one CP is taken as
  the **first path** (the strongest echo may arrive after the direct sound).
- False detections are rejected by the header CRC-16.

### Clock drift tracking
Sender and receiver sample clocks differ by tens of ppm, which slowly shifts the FFT window.
A timing offset of Δ samples appears as a phase slope `2πkΔ/N` across subcarriers. After each
symbol, the decision-directed phase errors are fitted with a line through the origin
(weighted by |d|); the slope gives the timing error step, which drives a first-order loop on
the drift rate (gain 0.25). The window is shifted by whole samples and the fractional part is
removed as a phase ramp.

Robustness: symbols whose power is more than 4× or less than ¼ of the running average (noise
bursts, knocks) do **not** update the tracker, and a single update is capped at ±0.5 samples.
A burst then only damages the blocks it overlaps.

## Error correction

- **Convolutional code** K=7, generators 133/171 (octal), as in 802.11a/g; terminated with
  6 tail bits. Punctured to ⅔ or ¾ (802.11 patterns).
- **Soft-decision Viterbi** decoder, 64 states.
- **Interleaver:** a pseudo-random permutation per block spreads each block's coded bits across
  frequencies and time.
- **Scrambler:** XOR with a pseudo-random sequence per block avoids repetitive patterns.
- **CRC-32** per block marks it good or bad. Bad blocks are **erasures** for the transfer layer's
  Reed–Solomon code ([protocol.md](protocol.md#transfer-layer)).

## Channel measurement (probe)

`probe.rs` generates a ~54 s signal and analyses its recording:

| Section | Measures |
|---|---|
| Exponential sine sweep 50 Hz–22 kHz (×2) | Impulse response → frequency response, RT60, direct-to-reverberant ratio; clock drift from the lag between the two recorded sweeps |
| Silence | Background noise spectrum |
| Periodic multitone (36 × 4096 samples) | Per-frequency SNR from differences of adjacent periods |
| One frame per band × rate | Actual decode result and goodput |

From these it predicts each band's achievable SNR (noise-limited and echo-limited) and the
fastest workable rate, and compares with the real frames. The echo estimate counts energy
arriving later than the CP guard as interference (+2.5 dB empirical calibration).
