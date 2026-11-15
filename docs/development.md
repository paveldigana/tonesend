# Development

## Setup

macOS with Apple Silicon (other platforms work for `core/`, `audio/`, `cli/`).

```sh
# Rust (Homebrew's rustup is keg-only: add it to PATH, e.g. in ~/.zshrc)
brew install rustup
export PATH=/opt/homebrew/opt/rustup/bin:$PATH
rustup default stable
rustup target add aarch64-apple-ios aarch64-apple-ios-sim

# iOS project generator
brew install xcodegen
```

Xcode 16 or newer for the iOS app.

## Repository layout

```text
core/      crate modem     — DSP + protocol (lib), tests in core/tests/
audio/     crate audio     — cpal playback/capture (desktop)
cli/       binary tonesend — command-line tools
ffi/       crate tonesend   — C ABI static library; header in ffi/include/
ios/       SwiftUI app     — project.yml (xcodegen), App/
scripts/   build-ios.sh    — builds libtonesend.a for iOS (run by Xcode)
docs/      this documentation
PLAN.md    roadmap and progress log
```

## Build and test

```sh
cargo build --release          # all crates
cargo test --release           # 24 tests: unit, roundtrip, streaming, transfer, probe, C ABI
cargo clippy --release --all-targets
cargo fmt --all
```

Tests run in release mode because the DSP is slow unoptimised (dev builds use opt-level 1).

| Test file | Covers |
|---|---|
| `core/src/*` unit tests | CRC vectors, bit packing, interleaver, FEC at all rates, Reed–Solomon recovery, profile sanity |
| `core/tests/roundtrip.rs` | Every band × rate through clean and "desk" channels, ±150 ppm drift, empty payload, several frames/bands in one recording, noise-only (no false frames), streaming receiver = batch decoder |
| `core/tests/transfer.rs` | Loss within parity, burst loss → incomplete → repaired, 200 KB multi-group, single-shard, audio end-to-end with a 0.5 s noise burst |
| `core/tests/probe.rs` | Channel measurement recovers simulated drift/RT60/DRR/roll-off; predictions are never optimistic |
| `ffi/src/lib.rs` | C API transfer round trip |

## iOS app

```sh
cd ios
xcodegen generate              # regenerate after editing project.yml or adding files
open ToneSend.xcodeproj
```

- A pre-build phase runs `scripts/build-ios.sh`, which builds `libtonesend.a` for the current
  platform (`aarch64-apple-ios` or `aarch64-apple-ios-sim`, release mode). Rust changes are
  picked up on the next Xcode build.
- Swift sees the library as module `ToneSendKit` via `SWIFT_INCLUDE_PATHS = ffi/include`.
- Simulator builds exclude x86_64 (the Rust library is arm64 only).
- Bundle id `cz.ittailors.soundtransfer`; set the signing team in Xcode.

Command-line builds:

```sh
xcodebuild -project ios/ToneSend.xcodeproj -scheme ToneSend \
  -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' build
xcodebuild -project ios/ToneSend.xcodeproj -scheme ToneSend \
  -sdk iphoneos -destination 'generic/platform=iOS' CODE_SIGNING_ALLOWED=NO build
```

Simulator launch arguments (no audio hardware needed):

| Argument | Effect |
|---|---|
| `-demo YES` | Fills Receive with transfers produced by a real encode → decode through the Rust modem |
| `-tab N` | Opens tab 0 Send, 1 Receive, 2 Lab |
| `-openFirst YES` | Opens the newest received transfer |
| `-send.band N -send.rate N` | Preselect profile ids |

```sh
xcrun simctl launch booted cz.ittailors.soundtransfer -demo YES -tab 1
```

## CLI reference (`tonesend`)

Run `cargo build --release`, then `./target/release/tonesend <command> --help`.

| Command | Purpose | Example |
|---|---|---|
| `info` | Profiles and their bit rates | `tonesend info` |
| `encode` | Text/file → WAV (one raw frame; same envelope + Brotli as the apps) | `tonesend encode --text "hi" --band max --rate fast -o hi.wav` |
| `decode` | Find and decode all frames in a WAV | `tonesend decode -i rec.wav -o out.bin` |
| `listen` | Streaming receive from the mic, or replay a WAV through the streaming receiver | `tonesend listen -s 60` · `tonesend listen -i rec.wav` |
| `channel` | Apply the simulated acoustic channel to a WAV | `tonesend channel -i hi.wav -o room.wav --preset room` |
| `sim` | encode → channel → decode with details | `tonesend sim --band max --rate turbo --preset desk --snr 25` |
| `bench` | Goodput table across SNR for all profiles | `tonesend bench --preset desk --snr 30,20,10` |
| `devices` | Audio devices | `tonesend devices` |
| `play` / `record` | Speaker / microphone (48 kHz mono WAV) | `tonesend record -o rec.wav -s 30 --device iphone` |
| `music` | Embed a data ID in music, test it through MP3/AAC/Opus at several levels | `tonesend music --band high --level=-15,-20` · `tonesend music -i song.mp3 --out-dir out/` |
| `embed` | Write text into a song (repeated) for the iPhone app to pick up | `tonesend embed -i song.mp3 -t "Hello" -o out.mp3` |
| `spectrum` | Power per frequency band of a WAV section | `tonesend spectrum -i rec.wav --from 10 --to 12 --width 500` |
| `probe generate` | Write the measurement probe | `tonesend probe generate -o probe.wav` |
| `probe analyze` | Report from a recording of the probe | `tonesend probe analyze -i rec.wav --json report.json` |
| `probe run` | Play + record the probe on this machine, analyse | `tonesend probe run --save rec.wav` |

WAV input of any sample rate is resampled to 48 kHz; multi-channel input uses channel 0.
Band names: `max`, `wide`, `narrow`, `inaudible`, `high`. Rates: `safe`, `normal`, `fast`, `turbo`.

`tonesend music` needs `ffmpeg` (`brew install ffmpeg`). Without `--input` it uses a synthetic track.
`--out-dir` keeps the mixed WAVs and encoded files for listening.

### Channel simulator

Presets for `channel`, `sim`, `bench` (`--snr`, `--rt60`, `--drr`, `--ppm`, `--seed` override):

| Preset | SNR | RT60 | Direct-to-reverberant | Drift | Speaker/mic band |
|---|---|---|---|---|---|
| `clean` | ∞ | — | — | 0 | flat |
| `desk` | 30 dB | 0.25 s | 12 dB | 40 ppm | 300 Hz–16 kHz |
| `room` | 20 dB | 0.4 s | 4 dB | 80 ppm | 300 Hz–15 kHz |
| `noisy` | 10 dB | 0.3 s | 8 dB | 60 ppm | 300 Hz–16 kHz |
| `far` | 12 dB | 0.6 s | 0 dB | 100 ppm | 300 Hz–14 kHz |

SNR is white noise over 0–24 kHz relative to the **transmitted** power, so bands the
speaker/mic attenuate really do arrive with lower SNR.

## Measuring real devices

1. `tonesend probe generate -o probe.wav` and play it on the sender (`tonesend play`, AirDrop to a phone, or
   the app's **Lab → Play probe signal**).
2. Record on the receiver: `tonesend record -o rec.wav -s 60`, or **Lab → Record raw audio** on a phone
   (then AirDrop the WAV). Use WAV or other lossless formats: AAC/MP3 discard high frequencies.
3. `tonesend probe analyze -i rec.wav --json report.json`.

The report lists recording level (clipping / too quiet), clock drift, RT60 and direct-to-
reverberant ratio, 1/3-octave frequency response and SNR, a per-band prediction, and the **actual**
result of one test frame per band × rate, plus an audible-vs-inaudible verdict. The test-frame
table is the ground truth; the prediction is simulator-calibrated. Repeat 2–3 times per setup.

Avoid microphones with voice processing (noise suppression / AGC): the measured SNR then
collapses faster than the playback level. The app's `.measurement` audio mode avoids it on
iPhone.

## Conventions

- Commit messages: `type(KA-1234): description` (Conventional Commits with the Jira key).
- Keep `ffi/include/tonesend.h` and `ffi/src/lib.rs` in sync; never renumber band/rate ids.
- New DSP behaviour needs a test in `core/tests/` that runs through `sim::Channel`.
