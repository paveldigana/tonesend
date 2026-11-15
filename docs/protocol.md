# Protocol specification

Byte-exact description of what is on air. Version 1 of every layer.

```text
App envelope        "ST" 01 | kind | [name] | data              (ios/App/Transfer.swift)
  └ Transfer layer  shards: 18-byte header + ≤494 data bytes    (core/src/transfer.rs)
      └ Modem frame header + blocks of ≤512 bytes               (core/src/modem.rs)
          └ OFDM    chirp | guard | training | symbols          (core/src/ofdm.rs)
```

All multi-byte integers are little-endian unless noted. Bits are packed MSB-first.

## Modem frame

### Frame header

8 bytes, sent first after the training symbol:

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | Magic `0x5A` |
| 1 | 1 | `version << 4 \| rate_id` (version = 1) |
| 2 | 4 | Payload length in bytes (u32 LE), ≤ 16 MiB |
| 6 | 2 | CRC-16/CCITT-FALSE of bytes 0–5, **big-endian** |

Encoding: 64 bits → K=7 convolutional code rate ½ with 6 tail bits (140 coded bits) →
interleaved (seed `0x48445252`) → DBPSK, one bit per subcarrier, padded with zeros to whole
symbols (1 symbol for 149+ carriers, 3 for the 64-carrier inaudible band).

The **band** is not in the header: it is identified by which band's chirp was detected.

Band ids (FFI, app): 0 `wide`, 1 `narrow`, 2 `inaudible`, 3 `max`, 4 `high`.
Rate ids: 0 `safe`, 1 `normal`, 2 `fast`, 3 `turbo`.

### Payload blocks

The payload is cut into blocks of 512 bytes (the last may be shorter). For block `i`:

1. XOR the data with the scrambler stream (xorshift64* seeded `0x5343524D + i`, top byte of
   each output).
2. Append CRC-32 (IEEE) of the scrambled data, **big-endian**.
3. Convolutional encode (rate ½, 6 tail bits), puncture to the rate's code rate.
4. Interleave with a permutation seeded `0x5041594C + i`.

Blocks are concatenated, padded with zero bits to fill the last symbol, and mapped to
differential (D)PSK increments: `bits_per_carrier` bits per subcarrier, subcarriers in
ascending frequency, symbols in time order.

The receiver recomputes the block layout from the header length and rate, so no per-block
headers are needed at this layer.

### Training symbol

QPSK phases `π/4 + q·π/2`, `q` from xorshift64* seeded `0x54524149`, one per subcarrier.

### PRNG and interleaver

- xorshift64*: state = `seed ^ 0x9E3779B97F4A7C15` (1 if zero), advanced once; output
  `x * 0x2545F4914F6CDD1D` after `x ^= x>>12; x ^= x<<25; x ^= x>>27`.
- Permutation of length n: Fisher–Yates with the PRNG seeded `seed ^ (n * 0x9E3779B9)`;
  interleaved[i] = input[perm[i]].

## Transfer layer

Splits a file over many frames, adds whole-file integrity and repair data.

### Shards and groups

- **Shard size** `S` = 494 bytes, or the file length if smaller (minimum 1).
- `n = ceil(len / S)` data shards (last one zero-padded).
- Shards are split into `G = ceil(n / 128)` **groups** of nearly equal size (`k ≤ 128` each,
  the first `n mod G` groups get one extra), consecutive ranges of the file.
- Per group, parity shards are computed with a **systematic Reed–Solomon erasure code over
  GF(2⁸)** (polynomial 0x11D) using a Cauchy matrix: parity shard with index `k + i` is
  `Σⱼ inv((k+i) ⊕ j) · data_j`. Any `k` distinct shards of a group rebuild it.
- Redundancy `r` gives `m = round(k · r)` parity shards per group (max `256 − k`).

### Shard header (18 bytes)

| Offset | Size | Field |
|---|---|---|
| 0 | 2 | Magic `0x53 0x48` ("SH") |
| 2 | 1 | Version (1) |
| 3 | 1 | Shard index within group (0…k−1 data, k… parity) |
| 4 | 2 | Group number (u16 LE) |
| 6 | 4 | Transfer id (u32 LE, random per transfer) |
| 10 | 4 | File length in bytes (u32 LE) |
| 14 | 4 | CRC-32 (IEEE) of the whole file (u32 LE) |

Followed by `S` bytes of shard data. A full shard is exactly 512 bytes = one modem block, so
**every modem block that passes its CRC is a self-describing, usable shard**, whatever frame
it arrived in.

### Framing

- Shards are emitted round-robin across groups (shard 0 of every group, then shard 1, …), so a
  burst loss spreads over groups.
- **8 shards per frame** (~4 KB) when shards are full size; 1 shard per frame for single-shard
  (short) payloads.
- 2400 samples (50 ms) of silence between frames.

### Receiving

1. For each decoded frame, take the transfer id from its first good block; store every good
   shard by (group, index).
2. When every group has ≥ k distinct shards: rebuild the missing data shards, concatenate,
   truncate to the file length, verify the file CRC-32 → **Complete**.
3. Frames without shard headers (raw frames, e.g. IDs embedded in music) are delivered as
   one-frame transfers with id = CRC-32 of the payload; the same payload heard again within 60 s
   is reported as a **repeat** (event kind 3 with a running count), not a new transfer.
4. If no new frame arrives for 3 s (and no frame is in progress) while shards are missing →
   **Incomplete** (state is kept).

### Repair

The sender stores how much redundancy it has sent (`sent`). *Send repair data* transmits parity
shards with indices `k + round(k·sent)` … `k + round(k·(sent + extra)) − 1` for each group:
**new** shards the receiver has not seen, so it can complete as soon as it holds `k` per group,
whatever was lost. When a group's parity budget (`256 − k`) is exhausted, data shards are resent
instead. The receiver merges by transfer id; the shards it already has stay valid.

## Content envelope

Built by the framework (`core/src/content.rs`) for every app and the CLI, before the transfer
layer.

**Version 2** (current):

| Offset | Size | Field |
|---|---|---|
| 0 | 3 | `0x53 0x54 0x02` ("ST", version 2) |
| 3 | 1 | Flags: bits 0–3 kind (0 text, 1 file, 2 image), bit 4 = body Brotli-compressed, bits 5–7 reserved |
| 4 | 1 | (file, image) name length L ≤ 200 |
| 5 | L | (file, image) UTF-8 name |
| … | | Body |

- **Text and files:** the body is compressed with **Brotli** (quality 11, window 2²²; quality 9
  above 4 MB) using the framework's shared **custom dictionary** (`content::DICTIONARY`: vCard
  and calendar fields, Wi‑Fi/URL fragments, everyday English and Czech words), but only when
  that makes it smaller. The dictionary is part of version 2 and must never change; a new
  dictionary needs a new envelope version.
- **Images:** the framework downscales (area average) to the requested longest side and
  encodes **lossy WebP** (default quality 55); the body is the WebP file, not Brotli-compressed.
  Decoders get RGBA back via `content::decode_image` / `ts_image_decode`.

**Version 1** (still decoded): `"ST" 0x01 | kind (0 text, 1 file) | [name_len | name] | data`,
uncompressed. Payloads without either prefix are delivered as raw data.

## Compatibility rules

- Band and rate ids are part of the format: never renumber, only append.
- Magic + version bytes exist at the frame header, shard header and envelope; bump the version
  when changing the corresponding layout.
- The modem layer is independent of the transfer layer: `tonesend encode`/`tonesend decode` (desktop)
  send raw frames without shards.
