use modem::sim::Channel;
use modem::transfer::{self, Assembler, Event, TransferReceiver};
use modem::util::Rng;
use modem::{Band, Frame, Rate, BLOCK_BYTES, SAMPLE_RATE};

fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut v = vec![0u8; n];
    Rng::new(seed).fill_bytes(&mut v);
    v
}

/// Pack shards into fake decoded frames (4 per frame), losing blocks where `lose` says so.
fn frames(shards: &[Vec<u8>], lose: &mut dyn FnMut(usize) -> bool, t0: usize) -> Vec<Frame> {
    let per = if shards[0].len() == BLOCK_BYTES {
        transfer::SHARDS_PER_FRAME
    } else {
        1
    };
    let mut n = 0;
    shards
        .chunks(per)
        .enumerate()
        .map(|(i, group)| Frame {
            band: Band::AudibleWide,
            rate: Rate::Fast,
            sample_rate: SAMPLE_RATE,
            start: t0 + i * 48_000,
            end: t0 + i * 48_000 + 40_000,
            payload: group.concat(),
            blocks_ok: group
                .iter()
                .map(|_| {
                    n += 1;
                    !lose(n - 1)
                })
                .collect(),
            detect_score: 1.0,
            snr_db: 20.0,
            carrier_snr_db: vec![20.0; 10],
            drift_ppm: 0.0,
            soft: Vec::new(),
            copies: 1,
            header_assumed: false,
            attempts: 1,
        })
        .collect()
}

fn feed(asm: &mut Assembler, fr: &[Frame]) -> Vec<Event> {
    let mut ev = Vec::new();
    for f in fr {
        asm.add_frame(f, &mut ev);
    }
    ev
}

#[test]
fn lossless_and_lossy_within_parity() {
    let d = data(30_000, 1); // 61 shards, one group
    let sh = transfer::shards(&d, 7, 0.0, 0.25, true); // +15 parity
    let mut r = Rng::new(2);
    let mut lost = 0;
    let fr = frames(
        &sh,
        &mut |_| {
            let l = lost < 12 && r.below(5) == 0; // ~20 % loss, capped below parity count
            lost += l as usize;
            l
        },
        0,
    );
    let ev = feed(&mut Assembler::default(), &fr);
    match &ev[..] {
        [Event::Complete {
            id: 7,
            data: got,
            stats,
        }] => {
            assert_eq!(got, &d);
            assert!(stats.repaired > 0, "expected parity repair");
            assert_eq!(stats.crc, modem::util::crc32(&d));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn incomplete_then_repaired_by_extra_parity() {
    let d = data(20_000, 3);
    let sh = transfer::shards(&d, 9, 0.0, 0.10, true);
    let mut i = 0;
    // Lose a whole burst of 3 frames (12 shards) — more than the 10 % parity.
    let fr = frames(
        &sh,
        &mut |n| {
            i += 1;
            (8..20).contains(&n)
        },
        0,
    );
    let mut asm = Assembler::default();
    let mut ev = feed(&mut asm, &fr);
    asm.check_idle(10_000_000, &mut ev);
    let Some(Event::Incomplete { stats, .. }) = ev.first() else {
        panic!("{ev:?}")
    };
    assert!(stats.shards_have < stats.shards_needed);

    // Sender taps "repair": 30 % more, all new parity indices.
    let rep = transfer::shards(&d, 9, 0.10, 0.40, false);
    let ev = feed(&mut asm, &frames(&rep, &mut |_| false, 20_000_000));
    assert!(
        matches!(&ev[..], [Event::Complete { data, .. }] if *data == d),
        "{ev:?}"
    );
}

#[test]
fn large_file_multiple_groups() {
    let d = data(200_000, 4); // 405 shards → 4 groups
    assert_eq!(transfer::plan(d.len()).groups.len(), 4);
    let sh = transfer::shards(&d, 11, 0.0, 0.2, true);
    let mut r = Rng::new(5);
    let fr = frames(&sh, &mut |_| r.below(12) == 0, 0); // ~8 % loss
    let ev = feed(&mut Assembler::default(), &fr);
    assert!(matches!(&ev[..], [Event::Complete { data, .. }] if *data == d));
}

#[test]
fn tiny_message_single_shard() {
    let d = b"hi".to_vec();
    let sh = transfer::shards(&d, 1, 0.0, 0.25, true);
    assert_eq!(sh.len(), 1); // round(0.25) = 0 parity for k = 1
    let ev = feed(&mut Assembler::default(), &frames(&sh, &mut |_| false, 0));
    assert!(matches!(&ev[..], [Event::Complete { data, .. }] if *data == d));
}

#[test]
fn audio_end_to_end_survives_noise_burst() {
    let d = data(6_000, 6);
    let cfg = Band::AudibleWide.config();
    let mut sig = transfer::encode(&d, &cfg, Rate::Fast, 42, 0.5);
    let est = transfer::air_time(d.len(), &cfg, Rate::Fast, 0.5);
    assert!(
        (est - sig.len() as f64 / 48_000.0).abs() < 1e-6,
        "air_time estimate {est}"
    );
    // Wipe out ~0.5 s in the middle with loud noise.
    let mid = sig.len() / 2;
    let mut r = Rng::new(7);
    for v in &mut sig[mid..mid + 24_000] {
        *v += 0.8 * r.gauss() as f32;
    }
    sig.extend(vec![0.0; 5 * 48_000]);
    let rx = Channel::preset("desk").unwrap().apply(&sig, SAMPLE_RATE);
    let mut tr = TransferReceiver::new(&Band::ALL);
    let mut ev = Vec::new();
    for c in rx.chunks(960) {
        ev.extend(tr.push(c));
    }
    match &ev[..] {
        [Event::Complete { data, stats, .. }] => {
            assert_eq!(data, &d);
            assert!(
                stats.blocks_ok < stats.blocks_total,
                "burst should have destroyed blocks"
            );
            assert!(stats.repaired > 0);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn raw_frames_are_delivered_once_then_counted() {
    // An ID looped in audio (e.g. embedded in music) shows up as one transfer.
    let id = b"\x53\x54\x01\x00AD-2026-0042".to_vec();
    let cfg = Band::High.config();
    let mut sig = Vec::new();
    for _ in 0..3 {
        sig.extend(modem::encode(&id, &cfg, Rate::Safe));
        sig.extend(vec![0.0; 48_000]);
    }
    let rx = Channel {
        snr_db: Some(25.0),
        ..Channel::clean()
    }
    .apply(&sig, SAMPLE_RATE);
    let mut tr = TransferReceiver::new(&Band::ALL);
    let mut ev = Vec::new();
    for c in rx.chunks(1024) {
        ev.extend(tr.push(c));
    }
    match &ev[..] {
        [Event::Complete { data, id: a, .. }, Event::Repeat {
            id: b, count: 2, ..
        }, Event::Repeat {
            id: c, count: 3, ..
        }] => {
            assert_eq!(data, &id);
            assert!(a == b && b == c);
        }
        other => panic!("{other:?}"),
    }
}
