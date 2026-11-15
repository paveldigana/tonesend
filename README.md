# ToneSend

Data transfer over sound using OFDM (many simultaneous frequencies).

**Documentation: [docs/](docs/README.md)**: [overview](docs/overview.md) ·
[user guide](docs/user-guide.md) · [architecture](docs/architecture.md) ·
[signal processing](docs/signal-processing.md) · [protocol](docs/protocol.md) ·
[development](docs/development.md) · [measurements](docs/measurements.md).
Roadmap and progress log: [PLAN.md](PLAN.md).

## Layout

- `core/` — crate `modem`: pure DSP (encode bytes → audio samples, decode samples → frames) + channel simulator. No audio I/O.
- `audio/` — crate `audio`: speaker/mic I/O via cpal, mono 48 kHz.
- `ffi/` — crate `tonesend`: C ABI (`ffi/include/tonesend.h`) + static library for mobile apps.
- `ios/` — SwiftUI iPhone app (Send / Receive / Lab) on top of `tonesend`.
- `cli/` — binary `tonesend` for WAV encode/decode, simulation, benchmarks and channel measurement.

## Setup

```sh
brew install rustup && export PATH=/opt/homebrew/opt/rustup/bin:$PATH && rustup default stable
cargo test --release
```

## CLI

```sh
cargo build --release
ts=./target/release/tonesend

$ts info                                              # profiles + theoretical throughput
$ts encode --text "hello" --band wide --rate fast -o hello.wav
$ts decode -i hello.wav                               # finds frames of any band
$ts channel -i hello.wav -o hello_room.wav --preset room   # simulate a room
$ts sim --band inaudible --rate normal --preset desk --snr 15
$ts bench --preset desk                               # goodput table across SNR
```

Bands: `wide` (1–8 kHz), `narrow` (1.5–5 kHz, long symbols), `inaudible` (17.5–20.5 kHz).
Rates: `safe` (DBPSK ½), `normal` (DQPSK ½), `fast` (DQPSK ¾).
Channel presets: `clean`, `desk`, `room`, `noisy`, `far`.

### Measuring a real channel (phase 2)

```sh
$ts devices                                           # pick input/output devices
$ts probe run --save rec.wav --json report.json       # this Mac: speaker -> mic (plays ~54 s of sound)
$ts probe generate -o probe.wav                       # two devices: play this on the sender…
$ts record -o rec.wav -s 50 --device iphone           # …record on the receiver
$ts probe analyze -i rec.wav                          # drift, RT60/DRR, response, SNR, real goodput
```

On macOS the terminal needs microphone permission (System Settings → Privacy & Security → Microphone).

WAVs are 48 kHz mono 16-bit; other sample rates are resampled on read. You can play
an encoded WAV on one device and record it on another, then `tonesend decode` the recording.

## iPhone app

Requirements: Xcode, Rust with iOS targets (`rustup target add aarch64-apple-ios aarch64-apple-ios-sim`),
`xcodegen` (`brew install xcodegen`).

```sh
cd ios && xcodegen generate && open ToneSend.xcodeproj
```

1. Select the **ToneSend** target → *Signing & Capabilities* → choose your Team
   (bundle id: `cz.ittailors.soundtransfer`).
2. Pick your iPhone as the run destination and press Run. Repeat for the second phone.
3. First launch on a phone: *Settings → General → VPN & Device Management* → trust your developer certificate (free accounts).

The Rust modem is rebuilt automatically by a pre-build script (`scripts/build-ios.sh`).

Transfers are split into short packets with per-block and whole-file CRC-32 and Reed–Solomon
repair data (Send tab → *Repair data*). If the receiver shows a transfer as *Incomplete*, open it
in the sender's *Recently sent* list and tap *Send repair data*; the receiver merges it.

Using it: phone A → **Receive** tab → tap the ear. Phone B → **Send** → type text / pick a photo →
choose a profile → *Send as sound*. Volume up, phones 10–50 cm apart, speaker towards the other
phone's bottom microphone. Each received item shows the content, effective speed, link rate,
SNR, blocks, clock drift and a per-frequency signal chart.

The audio session uses `.measurement` mode, so iOS applies no AGC / noise suppression to the mic.
**Lab** tab: play the ~54 s probe on one phone, *Record raw audio* on the other, AirDrop the WAV to
the Mac and run `tonesend probe analyze -i recording.wav`.

Simulator: `-demo YES` launch argument fills the Receive tab via a real encode → decode.
