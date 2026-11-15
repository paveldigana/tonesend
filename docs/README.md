# ToneSend documentation

ToneSend moves data between devices **as sound**: a speaker plays it, a microphone hears it.
No Wi‑Fi, Bluetooth, pairing or internet is involved. It uses many frequencies at once (OFDM),
error correction, and per-block checksums with Reed–Solomon repair, so files arrive intact.

| Document | For | What's inside |
|---|---|---|
| [Overview](overview.md) | Everyone, business | What it is, how fast, use cases, limitations |
| [User guide](user-guide.md) | App users, testers | Installing on iPhone, sending, receiving, repair, troubleshooting |
| [Architecture](architecture.md) | Developers | Components, data flow, threading, C interface |
| [Signal processing](signal-processing.md) | Developers | Modulation, profiles, sync, error correction, drift tracking |
| [Protocol](protocol.md) | Developers | Byte-exact wire format: frames, blocks, transfers, repair |
| [Development](development.md) | Developers | Build, test, CLI reference, simulator, measurement tools |
| [Measurements](measurements.md) | Developers, business | Benchmarks and real-device results so far |

Quick start for developers: [development.md → Setup](development.md#setup).
Quick start for testers: [user-guide.md → First transfer](user-guide.md#your-first-transfer).
