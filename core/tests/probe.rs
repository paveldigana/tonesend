//! Validate the channel-measurement analysis against the simulator, where the truth is known.

use modem::probe::{analyze, generate};
use modem::sim::Channel;
use modem::{Band, Rate, SAMPLE_RATE};

#[test]
fn measures_simulated_room() {
    let probe = generate(512);
    let ch = Channel {
        snr_db: Some(25.0),
        ..Channel::preset("room").unwrap()
    }; // RT60 0.4, DRR 4, 80 ppm
    let r = analyze(&ch.apply(&probe, SAMPLE_RATE)).expect("analysis");

    assert!((r.drift_ppm - 80.0).abs() < 1.0, "drift {}", r.drift_ppm);
    let rt60 = r.rt60_s.expect("rt60");
    assert!((0.3..0.6).contains(&rt60), "rt60 {rt60}");
    assert!((2.5..5.5).contains(&r.drr_db), "drr {}", r.drr_db);

    // Simulated speaker roll-off above 15 kHz must show up.
    let at = |hz: f32| {
        r.freq
            .iter()
            .min_by(|a, b| (a.f_center - hz).abs().total_cmp(&(b.f_center - hz).abs()))
            .unwrap()
    };
    assert!(at(20_000.0).response_db < at(2000.0).response_db - 5.0);

    // All 9 test frames are detected; the robust narrow/safe one decodes.
    assert!(r.frames.iter().all(|f| f.found), "{:?}", r.frames);
    let ns = r
        .frames
        .iter()
        .find(|f| f.band == Band::AudibleNarrow && f.rate == Rate::Safe)
        .unwrap();
    assert!(ns.ok);
}

#[test]
fn prediction_is_not_optimistic() {
    // Wherever the measurement predicts a rate works, the real frame must decode.
    for (preset, snr) in [
        ("desk", 5.0),
        ("desk", 15.0),
        ("room", 15.0),
        ("noisy", 10.0),
    ] {
        let ch = Channel {
            snr_db: Some(snr),
            ..Channel::preset(preset).unwrap()
        };
        let r = analyze(&ch.apply(&generate(512), SAMPLE_RATE)).expect("analysis");
        for p in &r.profiles {
            if let Some(best) = p.best_rate {
                let f = r
                    .frames
                    .iter()
                    .find(|f| f.band == p.band && f.rate == best)
                    .unwrap();
                assert!(
                    f.ok,
                    "{preset}@{snr}: predicted {}/{} ok, but frame failed",
                    p.band, best
                );
            }
        }
    }
}
