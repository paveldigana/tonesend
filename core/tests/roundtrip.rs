use modem::sim::Channel;
use modem::util::Rng;
use modem::{decode, encode, Band, Rate, SAMPLE_RATE};

fn payload(n: usize, seed: u64) -> Vec<u8> {
    let mut v = vec![0u8; n];
    Rng::new(seed).fill_bytes(&mut v);
    v
}

fn roundtrip(band: Band, rate: Rate, ch: &Channel, n: usize) -> modem::Frame {
    let cfg = band.config();
    let data = payload(n, n as u64 ^ rate.id() as u64);
    let rx = ch.apply(&encode(&data, &cfg, rate), SAMPLE_RATE);
    let frames = decode(&rx, &[cfg]);
    assert_eq!(
        frames.len(),
        1,
        "{band}/{rate}: expected one frame, got {}",
        frames.len()
    );
    let f = frames.into_iter().next().unwrap();
    assert!(
        f.ok(),
        "{band}/{rate}: blocks {:?}, snr {:.1} dB",
        f.blocks_ok,
        f.snr_db
    );
    assert_eq!(f.payload, data);
    f
}

#[test]
fn clean_all_profiles() {
    for band in Band::ALL {
        for rate in Rate::ALL {
            roundtrip(band, rate, &Channel::clean(), 1500);
        }
    }
}

#[test]
fn desk_channel_all_profiles() {
    let ch = Channel::preset("desk").unwrap();
    for band in Band::ALL {
        for rate in Rate::ALL {
            roundtrip(band, rate, &ch, 2000);
        }
    }
}

#[test]
fn tracks_clock_drift() {
    for ppm in [-150.0, 150.0] {
        let ch = Channel {
            ppm,
            snr_db: Some(30.0),
            ..Channel::clean()
        };
        let f = roundtrip(Band::AudibleWide, Rate::Fast, &ch, 6000);
        assert!(
            (f.drift_ppm - ppm).abs() < 20.0,
            "drift est {:.1} vs {ppm}",
            f.drift_ppm
        );
        roundtrip(Band::Inaudible, Rate::Normal, &ch, 3000);
    }
}

#[test]
fn empty_payload() {
    let f = roundtrip(Band::AudibleWide, Rate::Normal, &Channel::clean(), 0);
    assert!(f.payload.is_empty());
}

#[test]
fn multiple_frames_and_bands_in_one_recording() {
    let a = payload(700, 1);
    let b = payload(300, 2);
    let c = payload(900, 3);
    let mut sig = encode(&a, &Band::AudibleWide.config(), Rate::Fast);
    sig.extend(vec![0.0; 5000]);
    sig.extend(encode(&b, &Band::Inaudible.config(), Rate::Safe));
    sig.extend(vec![0.0; 5000]);
    sig.extend(encode(&c, &Band::AudibleNarrow.config(), Rate::Normal));
    let rx = Channel {
        snr_db: Some(25.0),
        ..Channel::clean()
    }
    .apply(&sig, SAMPLE_RATE);

    let cfgs: Vec<_> = Band::ALL.iter().map(|b| b.config()).collect();
    let frames = decode(&rx, &cfgs);
    let got: Vec<_> = frames.iter().map(|f| (f.band, f.payload.clone())).collect();
    assert_eq!(
        got,
        vec![
            (Band::AudibleWide, a),
            (Band::Inaudible, b),
            (Band::AudibleNarrow, c)
        ]
    );
}

#[test]
fn noise_only_yields_nothing() {
    let mut r = Rng::new(9);
    let noise: Vec<f32> = (0..SAMPLE_RATE as usize * 3)
        .map(|_| 0.1 * r.gauss() as f32)
        .collect();
    let cfgs: Vec<_> = Band::ALL.iter().map(|b| b.config()).collect();
    assert!(decode(&noise, &cfgs).is_empty());
}

#[test]
fn streaming_receiver_matches_batch() {
    // Three frames on different bands, delivered in irregular small chunks like a mic.
    let a = payload(2500, 11);
    let b = payload(400, 12);
    let c = payload(900, 13);
    let mut sig = vec![0.0; 20_000];
    sig.extend(encode(&a, &Band::AudibleWide.config(), Rate::Fast));
    sig.extend(vec![0.0; 30_000]);
    sig.extend(encode(&b, &Band::Inaudible.config(), Rate::Normal));
    sig.extend(vec![0.0; 7_000]);
    sig.extend(encode(&c, &Band::AudibleNarrow.config(), Rate::Safe));
    let rx = Channel {
        ppm: 60.0,
        ..Channel::preset("desk").unwrap()
    }
    .apply(&sig, SAMPLE_RATE);

    let mut r = modem::Receiver::new(&Band::ALL);
    let mut got = Vec::new();
    let mut saw_progress = false;
    let mut i = 0;
    let mut n = 333;
    while i < rx.len() {
        let j = (i + n).min(rx.len());
        got.extend(r.push(&rx[i..j]));
        if let modem::Activity::Receiving { progress, .. } = r.status().activity {
            saw_progress |= progress > 0.0 && progress < 1.0;
        }
        i = j;
        n = if n == 333 { 1024 } else { 333 }; // vary chunk size
    }
    got.extend(r.push(&vec![0.0; 48_000])); // flush
    let got: Vec<_> = got
        .iter()
        .map(|f| (f.band, f.payload.clone(), f.ok()))
        .collect();
    assert_eq!(
        got,
        vec![
            (Band::AudibleWide, a, true),
            (Band::Inaudible, b, true),
            (Band::AudibleNarrow, c, true)
        ]
    );
    assert!(saw_progress, "status never reported partial progress");
    assert!(matches!(r.status().activity, modem::Activity::Listening));
}

#[test]
fn streaming_finds_max_band_after_other_bands() {
    // Regression: the Audible chirp partly matches the Max chirp; the false Audible
    // candidate must not hide the real Max frame.
    let items = [
        (Band::AudibleWide, Rate::Fast),
        (Band::Inaudible, Rate::Normal),
        (Band::AudibleMax, Rate::Fast),
    ];
    let mut r = modem::Receiver::new(&Band::ALL);
    for (i, (band, rate)) in items.iter().enumerate() {
        let data = payload(100 + i * 40, i as u64);
        let mut sig = vec![0.0; 4800];
        sig.extend(encode(&data, &band.config(), *rate));
        sig.extend(vec![0.0; 48_000 * 2]);
        let got: Vec<_> = sig.chunks(1024).flat_map(|c| r.push(c)).collect();
        assert_eq!(got.len(), 1, "{band}/{rate}: {} frames", got.len());
        assert_eq!(got[0].payload, data);
    }
}

/// iOS hands over exact zeros while the microphone starts or is muted. Silence around a
/// frame must not produce detections with absurd scores (field log: scores of 1e11–1e12).
#[test]
fn digital_silence_gives_no_absurd_detections() {
    use modem::stream::{Diag, Receiver};
    let cfg = Band::High.config();
    let mut audio = vec![0f32; 48_000 * 2];
    audio.extend(modem::encode(&payload(85, 3), &cfg, Rate::Safe));
    audio.extend(vec![0f32; 48_000 * 3]);
    let mut rx = Receiver::new(&Band::ALL);
    let mut frames = 0;
    for c in audio.chunks(960) {
        frames += rx.push(c).len();
    }
    assert_eq!(frames, 1);
    for d in rx.take_diag() {
        if let Diag::Rejected { score, band, at_s, .. } = d {
            assert!(score < 1.5, "{band:?} at {at_s:.2}s: score {score}");
        }
    }
}
