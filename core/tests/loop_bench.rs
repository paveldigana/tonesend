//! A short message looped every 3 s (an ID in a song or ad), received at low SNR
//! with a real-world clock offset. Measures how many copies the streaming receiver
//! decodes and how soon the message is first delivered.
//!
//! `cargo test --release -p modem --test loop_bench -- --ignored --nocapture` prints the table.

use modem::sim::Channel;
use modem::transfer::{Event, TransferReceiver};
use modem::{Band, Rate};

const COPIES: usize = 12;
const PERIOD_S: f32 = 3.0;

/// Audio of `COPIES` copies; each copy gets its own noise level (`snr` ± 3 dB, like
/// music getting louder and quieter) and the same +35 ppm clock offset.
fn looped(snr: f32, seed: u64) -> Vec<f32> {
    let cfg = Band::High.config();
    let msg: Vec<u8> = (0..86u32).map(|i| (i * 7 + seed as u32) as u8).collect();
    // One raw frame, as `tonesend embed` puts into music.
    let one = modem::encode(&msg, &cfg, Rate::Safe);
    let mut audio = Vec::new();
    let mut r = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for c in 0..COPIES {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        let jitter = (r % 600) as f32 / 100.0 - 3.0;
        let dur = one.len() as f32 / modem::SAMPLE_RATE as f32;
        let ch = Channel {
            snr_db: Some(snr + jitter),
            ppm: 35.0,
            low_cut_hz: 200.0,
            high_cut_hz: 17_000.0,
            lead_s: 0.0,
            // Noise continues until the next copy (the simulator adds up to 0.1 s of lead).
            tail_s: PERIOD_S - dur - 0.1,
            seed: seed * 1000 + c as u64,
            ..Channel::clean()
        };
        let seg = ch.apply(&one, modem::SAMPLE_RATE);
        audio.extend(seg);
    }
    audio
}

/// (copies decoded, index of the copy that first delivered the message).
fn receive(audio: &[f32]) -> (usize, Option<usize>) {
    let mut rx = TransferReceiver::new(&[Band::High]);
    let mut decoded = 0;
    let mut first = None;
    let period = (PERIOD_S * modem::SAMPLE_RATE as f32) as usize;
    for (i, chunk) in audio.chunks(4800).enumerate() {
        for e in rx.push(chunk) {
            match e {
                Event::Complete { .. } | Event::Repeat { .. } => {
                    decoded += 1;
                    first.get_or_insert((i * 4800) / period);
                }
                _ => {}
            }
        }
    }
    (decoded, first)
}

#[test]
#[ignore]
fn table() {
    println!("in-band SNR is ~11 dB above the full-band value below");
    println!("snr_db  copies_decoded/{COPIES}  delivered  first_copy");
    for snr in [-12.0f32, -10.0, -8.0, -6.0, -4.0, -2.0] {
        let (mut dec, mut deliv, mut first_sum) = (0, 0, 0);
        let runs = 6;
        for seed in 1..=runs {
            let (d, f) = receive(&looped(snr, seed));
            dec += d;
            if let Some(f) = f {
                deliv += 1;
                first_sum += f + 1;
            }
        }
        println!(
            "{snr:6.1}  {:>9.1}          {deliv}/{runs}        {}",
            dec as f32 / runs as f32,
            if deliv > 0 { format!("{:.1}", first_sum as f32 / deliv as f32) } else { "-".into() }
        );
    }
}

/// Weak looped message: drift-hint/retry decoding, assumed headers and combining of
/// copies must turn a mostly-lost loop into a mostly-received one
/// (before these, about 1 copy in 12 decoded at this level).
#[test]
fn weak_loop_is_recovered() {
    let mut decoded = 0;
    for seed in 1..=3 {
        let (d, first) = receive(&looped(-10.0, seed));
        assert!(first.is_some(), "seed {seed}: message never delivered");
        decoded += d;
    }
    assert!(decoded >= 15, "only {decoded} of 36 copies decoded");
}
