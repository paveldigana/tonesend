# Overview

## What it is

ToneSend turns data into sound and back. One device plays a short burst of modem-like
audio; any nearby device with a microphone decodes it. It works:

- **without any network:** no Wi‑Fi, cellular, Bluetooth or pairing;
- **one-to-many:** every device in earshot receives the same transmission;
- **on ordinary hardware:** built-in phone or laptop speakers and microphones;
- **audibly or (nearly) silently:** audible bands for speed, a 17.5–20.5 kHz band most
  adults cannot hear.

It consists of a cross-platform modem core (Rust), an iPhone app, and desktop tools for
testing and measuring real rooms.

## How fast

Speeds are payload bits per second including all overhead (sync, error correction, checksums),
for a 4 KB file with no repair data. Simulated "desk" conditions (phones ~30 cm apart).

| Profile | Frequencies | Speed | Works down to (SNR) | Typical use |
|---|---|---|---|---|
| **Max + Turbo** | 1–13 kHz | **19.2 kbit/s** (~2.4 KB/s) | ~25 dB | Phones side by side |
| **Max + Fast** | 1–13 kHz | **13.2 kbit/s** | ~10 dB | Default: close range |
| Audible + Fast | 1–8 kHz | 8.0 kbit/s | ~10 dB | Older/weaker speakers |
| Robust + Safe | 1.5–5 kHz | 1.4 kbit/s | below 0 dB | Echoey rooms, distance |
| Silent + Fast | 17.5–20.5 kHz | 3.5 kbit/s | ~10 dB | Inaudible to most people |

What that means in practice:

| Payload | Size | Audible + Fast | Max + Fast | Max + Turbo |
|---|---|---|---|---|
| Wi‑Fi password, pairing key | ~50 B | 0.24 s | 0.21 s | 0.21 s |
| Contact card / URL | ~300 B | 0.48 s | 0.35 s | 0.29 s |
| Short document | 10 KB | 11.0 s | 6.7 s | 4.6 s |
| Small photo (≈480 px JPEG) | 30 KB | 32.7 s | 19.9 s | 13.8 s |

Air time without repair data. With the default 25 % repair data, files of several KB take
about 25 % longer (e.g. 30 KB on Max + Fast: 25.0 s). Tiny messages are a single block and
carry no repair data.

Sound is orders of magnitude slower than Wi‑Fi or Bluetooth. Its strengths are **zero setup,
broadcast, and working where radio is unavailable or unwanted**, not bulk transfer.

## Compression

Every transfer is made as small as possible before it is sent (all in the shared framework):

| Content | Method | Typical saving |
|---|---|---|
| Documents, JSON, contacts, long text | Brotli with a built-in dictionary | 30–90 % (6 KB doc −57 %, JSON −92 %, vCard −32 %) |
| URLs, Wi‑Fi codes, short Czech/English text | Same, the dictionary helps tiny messages | 10–40 % |
| Photos | Downscale + WebP | 53–62 % smaller than the previous JPEG |
| Already compressed files (ZIP, MP3, JPEG) | Sent as is (compression is skipped when it doesn't help) | 0 % |

The speed figures above are for raw bytes; with compression, text and documents arrive
correspondingly sooner (e.g. a 6 KB document in 1.7 s on Max + Fast instead of ~4 s).

## Reliability

- Every 512-byte block carries its own **CRC-32**; the whole file is verified with a
  **file CRC-32** after reassembly. A file is only shown as received when it checks out.
- **Error correction** inside each block (convolutional code + soft Viterbi decoding) fixes
  bit errors.
- **Repair data** (Reed–Solomon): the sender adds 10–50 % extra blocks; up to that many lost
  blocks are rebuilt automatically.
- **Repair on demand:** if more was lost, the sender taps *Send repair data*; the receiver
  merges the new blocks with what it already has. No return channel needed.
- Files are sent as short packets (~4 KB each) with their own sync, so a knock or noise burst
  costs a few blocks, not the file.

## Use cases

1. **Instant pairing / provisioning.** Send Wi‑Fi credentials, device keys or a contact from one
   phone to many, without QR codes or Bluetooth pairing.
2. **Offline and air-gapped transfer.** Move small data on or off machines with no network or
   USB allowed (labs, industrial control, secure rooms), or with no connectivity at all.
3. **Proximity and presence.** Inaudible "you are here" / check-in codes for shops, classrooms
   and meeting rooms. Sound stays in the room, unlike radio.
4. **Payments, tickets, access.** A terminal or turnstile hears a short token from a phone.
5. **Broadcast over existing audio.** TV, radio, PA systems or video calls carry a link,
   coupon or code to every phone listening.
6. **Low-cost IoT.** Configure gadgets that only have a buzzer and a cheap microphone: no radio
   chip, no radio certification.
7. **Emergency / field.** Phone-to-phone data where networks are down or jammed.

## Data in songs and advertisements

Data can ride along with music or an advert. Three approaches, from easiest to hardest:

| Approach | How | Data rate | Audible? | Status |
|---|---|---|---|---|
| **Ultrasonic layer** | Mix the **Silent** band (17.5–20.5 kHz) quietly under the song or ad. Music has little energy up there, so the two barely interfere | Hundreds of bit/s to a few kbit/s; a short ID needs < 0.5 s | Inaudible to most adults | Works, but only through high-quality audio (see below) |
| **High-frequency layer** | The **High** band (14.6–16.4 kHz): just below where 128 kbit/s MP3/AAC cut off | 0.7–2 kbit/s; a 12-byte ID in 0.35 s | Hard to hear for most adults; young listeners may notice a faint hiss | Works through common codecs (tested) |
| **Sonic logo / jingle** | Make the data burst part of the sound design (a branded "chirp" at the end of the ad) | Full speed | Yes, intentionally | Works today |
| **Hidden watermark** | Spread the data in the audible band below the music's masking threshold (psychoacoustic watermarking) | Tens of bit/s | No | New modem needed |

The practical pattern for ads and songs: transmit a **short ID** (8–16 bytes), repeated every
few seconds, rather than the content itself. The phone hears the ID and opens the matching
offer, lyrics, product page or ticket. A song can carry chapter IDs (lyrics/credits per
section); a TV or radio ad, a coupon link that appears on every listening phone.

### Test results

`tonesend music` mixes a 12-byte ID every 3 s into a track (a synthetic pop track with bright
hi-hats, which put real energy at 15–20 kHz), passes it through real encoders, and decodes it.
IDs decoded out of 10, by level of the data relative to the music:

| Codec | Silent band −10 dB | Silent −20 dB | **High band −15 dB** | **High −20 dB** | Audible band −10 dB |
|---|---|---|---|---|---|
| Uncompressed WAV | 10 | 9 | 10 | 10 | 9 |
| MP3 320 kbit/s | 10 | 10 | | | |
| MP3 192 kbit/s | 0 | 0 | 10 | 10 | |
| MP3 128 kbit/s | 0 | 0 | 10 | 10 | 9 |
| MP3 96 kbit/s | 0 | 0 | 0 | 0 | |
| Apple AAC 256 kbit/s | 10 | 9 | | | |
| Apple AAC 128 kbit/s | 0 | 0 | 10 | 10 | 9 |
| Apple AAC 96 kbit/s | | | 10 | 6 | |
| Apple AAC 64 kbit/s | 0 | 0 | 0 | 0 | |
| Opus 64 kbit/s | 10 | 0 | 10 | 2 | 9 |
| Opus 32 kbit/s | | | 9 | 0 | |

Why: lossy encoders remove the top of the spectrum to save bits. Measured cut-offs: MP3 128 ≈
16.5 kHz, MP3 192 ≈ 19 kHz, Apple AAC 128 fades from 17 kHz; MP3 320, AAC 256 and Opus keep ~20 kHz.

Conclusions:
- **High band at −15 to −20 dB** is the practical choice for music and ads distributed as
  compressed audio (streaming at ≥128 kbit/s, MP3 downloads, most TV/radio streams).
- **Silent band** only where the chain keeps 17–20 kHz: uncompressed/high-bitrate files,
  cinema and live PA, in-store playback, your own app's player.
- **Audible band under music** decodes only at −5 to −10 dB, which is clearly audible. Use it
  as an intentional sonic logo, not hidden data. Truly hidden data in the audible range needs
  psychoacoustic watermarking (a different, much slower modem).

Next step: test with real songs and real distribution chains (play the produced file through
the actual TV/radio/streaming/PA path and record with a phone).

What decides success:
- **The playback chain must keep 17–20 kHz.** Low-bitrate MP3/AAC, many streaming and
  broadcast codecs, Bluetooth speakers and TV soundbars often cut everything above ~16 kHz.
  Uncompressed or high-bitrate audio, cinema systems and good speakers keep it. Test the actual
  chain (with the probe tool).
- **Mixing level:** loud enough to decode at the listener's distance, quiet enough to stay
  unnoticeable (some young people and pets hear 18–20 kHz).
- **Robust settings:** Silent + Safe with repetition, since the music acts as noise.
- **Privacy and consent:** tracking people via inaudible ad beacons has caused regulatory and
  app-store trouble for others. Listening must be opt-in and visible, and the phone should only
  react while the user has the app open for that purpose.

## Limitations

- **Range:** best at 10–50 cm; a few metres in a quiet room with the robust profiles.
  Echo (reverberation) rather than loudness limits range.
- **Speed:** tens of kbit/s at best; large files take minutes.
- **Noise:** loud environments reduce speed; the app falls back to slower profiles.
- **Inaudible band:** depends on the speaker and microphone reaching 17–20 kHz; some devices
  and recording paths cut it off. Still being measured on real phones.
- **Privacy:** anyone nearby can record the sound. Encrypt sensitive payloads (planned).
- **One-way today:** repair is triggered manually on the sender; automatic retransmission
  requests are planned.

## Status

| Area | State |
|---|---|
| Modem core (Rust) | Working; 24 automated tests; simulator and real-recording validated |
| iPhone app | Working on real iPhones (send/receive text, photos, files; stats; repair) |
| Desktop tools (macOS/Linux/Windows) | CLI for encode/decode, live listen, simulation, channel measurement |
| Android / Flutter | Planned (the Rust core is shared) |
| Automatic rate selection, two-way repair, encryption | Planned |
