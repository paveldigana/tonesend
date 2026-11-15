//! Channel sounding: a probe signal to play through real speakers/mics, and
//! the analysis of its recording.
//!
//! Probe layout (48 kHz):
//! ```text
//! lead 0.5 s | ESS sweep 1 s | silence 1.5 s | periodic multitone 36×4096 | silence 0.5 s
//!   | ESS sweep 1 s | silence 1 s | data frames (every band × rate) | tail
//! ```
//! - ESS (exponential sine sweep, 50 Hz–22 kHz) → impulse response → frequency
//!   response, RT60, direct-to-reverberant ratio, per-profile echo leakage past the CP.
//! - Second ESS at a fixed distance → sample-clock drift between the devices.
//! - Silence → background noise spectrum.
//! - Periodic multitone → per-frequency SNR (noise only: the signal is periodic,
//!   so in steady state its reverb is periodic too and drops out of the variance).
//! - Real frames → actual goodput per profile, the ground truth.

use crate::modem::{self, Frame};
use crate::profiles::{Band, Rate, SAMPLE_RATE};
use crate::sim::resample;
use crate::sync;
use crate::util::Rng;
use rustfft::{num_complex::Complex32 as C32, FftPlanner};
use std::f64::consts::PI;

const FS: f64 = SAMPLE_RATE as f64;
const LEAD: usize = 24_000;
const ESS_LEN: usize = 48_000;
const ESS_F1: f64 = 50.0;
const ESS_F2: f64 = 22_000.0;
const GAP1: usize = 72_000;
const PERIOD: usize = 4096;
const REPS: usize = 36;
/// Periods skipped before measuring so the room's reverb reaches steady state.
const SKIP: usize = 12;
const GAP2: usize = 24_000;
const GAP3: usize = 48_000;
const FRAME_GAP: usize = 14_400;
const TAIL: usize = 24_000;
const SOUND_F_LO: f64 = 100.0;
const SOUND_F_HI: f64 = 22_000.0;
/// ESS1 start → ESS2 start, in samples of the sender's clock.
const ESS_SPACING: usize = ESS_LEN + GAP1 + PERIOD * REPS + GAP2;

/// SNR (dB, as estimated by the decoder) each rate needs to decode reliably.
/// Calibrated from simulated noise- and reverb-limited channels (2 KiB frames).
pub fn required_snr_db(rate: Rate) -> f32 {
    match rate {
        Rate::Safe => 5.0,
        Rate::Normal => 8.0,
        Rate::Fast => 10.0,
        Rate::Turbo => 16.0,
    }
}

/// The decoder copes ~2.5–4 dB better with late echo than its raw energy
/// suggests (part of it is still correlated with the current symbol).
/// Conservative empirical offset, calibrated in simulation.
const ECHO_SIR_CALIBRATION_DB: f64 = 2.5;

/// Deterministic payload for the probe frame of (band, rate).
pub fn expected_payload(band: Band, rate: Rate, len: usize) -> Vec<u8> {
    let bi = Band::ALL.iter().position(|&b| b == band).unwrap() as u64;
    let mut v = vec![0u8; len];
    Rng::new(0x5052_4F42 ^ (bi << 8) ^ rate.id() as u64).fill_bytes(&mut v);
    v
}

fn ess() -> Vec<f32> {
    let dur = ESS_LEN as f64 / FS;
    let r = (ESS_F2 / ESS_F1).ln();
    let fade_in = (0.02 * FS) as usize;
    let fade_out = (0.005 * FS) as usize;
    (0..ESS_LEN)
        .map(|i| {
            let t = i as f64 / FS;
            let ph = 2.0 * PI * ESS_F1 * dur / r * ((t * r / dur).exp() - 1.0);
            let w = if i < fade_in {
                0.5 - 0.5 * (PI * i as f64 / fade_in as f64).cos()
            } else if i >= ESS_LEN - fade_out {
                0.5 - 0.5 * (PI * (ESS_LEN - 1 - i) as f64 / fade_out as f64).cos()
            } else {
                1.0
            };
            (0.7 * ph.sin() * w) as f32
        })
        .collect()
}

fn sound_bins() -> (usize, usize) {
    let bin = FS / PERIOD as f64;
    (
        (SOUND_F_LO / bin).ceil() as usize,
        (SOUND_F_HI / bin).floor() as usize,
    )
}

/// One period of a flat multitone (Schroeder phases → low crest factor).
fn sounding_period() -> Vec<f32> {
    let (k_lo, k_hi) = sound_bins();
    let n = PERIOD;
    let kk = (k_hi - k_lo + 1) as f64;
    let mut spec = vec![C32::default(); n];
    for k in k_lo..=k_hi {
        let i = (k - k_lo) as f64;
        let v = C32::from_polar(1.0, (-PI * i * i / kk) as f32);
        spec[k] = v;
        spec[n - k] = v.conj();
    }
    FftPlanner::<f32>::new()
        .plan_fft_inverse(n)
        .process(&mut spec);
    let mut x: Vec<f32> = spec.iter().map(|c| c.re).collect();
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    x.iter_mut().for_each(|v| *v *= 0.9 / peak);
    x
}

fn mean_power(x: &[f32]) -> f64 {
    x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64
}

/// Average power of a probe data frame of `band` (OFDM section only).
fn frame_power(band: Band) -> f64 {
    let cfg = band.config();
    let x = modem::encode(
        &expected_payload(band, Rate::Normal, 1024),
        &cfg,
        Rate::Normal,
    );
    mean_power(&x[cfg.params.chirp_len + cfg.params.guard..])
}

/// Build the probe signal. `frame_bytes` = payload per test frame.
pub fn generate(frame_bytes: usize) -> Vec<f32> {
    let sweep = ess();
    let period = sounding_period();
    let mut x = vec![0f32; LEAD];
    x.extend(&sweep);
    x.extend(vec![0f32; GAP1]);
    for _ in 0..REPS {
        x.extend(&period);
    }
    x.extend(vec![0f32; GAP2]);
    x.extend(&sweep);
    x.extend(vec![0f32; GAP3]);
    for band in Band::ALL {
        for rate in Rate::ALL {
            let data = expected_payload(band, rate, frame_bytes);
            x.extend(modem::encode(&data, &band.config(), rate));
            x.extend(vec![0f32; FRAME_GAP]);
        }
    }
    x.extend(vec![0f32; TAIL]);
    x
}

// ------------------------------------------------------------------ report

#[derive(Clone, Debug)]
pub struct FreqRow {
    pub f_center: f32,
    /// Speaker+room+mic magnitude response, dB relative to the 1–4 kHz average.
    pub response_db: f32,
    /// Background noise power density, dB (relative; only the shape matters).
    pub noise_db: f32,
    /// Per-frequency SNR of the full-band multitone, dB.
    pub snr_db: f32,
}

#[derive(Clone, Debug)]
pub struct ProfileEstimate {
    pub band: Band,
    /// Noise-limited per-subcarrier SNR for this band's own signal (power concentrated in-band).
    pub noise_snr_db: f32,
    /// Energy within the CP guard vs. echo energy arriving after it.
    pub reverb_sir_db: f32,
    pub effective_snr_db: f32,
    /// Fastest rate whose requirement is met, and its error-free bitrate.
    pub best_rate: Option<Rate>,
    pub predicted_bps: f64,
}

#[derive(Clone, Debug)]
pub struct FrameResult {
    pub band: Band,
    pub rate: Rate,
    pub found: bool,
    pub ok: bool,
    pub blocks_ok: usize,
    pub blocks_total: usize,
    pub snr_db: Option<f32>,
    pub goodput_bps: f64,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub peak_dbfs: f32,
    pub rms_dbfs: f32,
    pub drift_ppm: f64,
    pub rt60_s: Option<f32>,
    pub drr_db: f32,
    pub freq: Vec<FreqRow>,
    pub profiles: Vec<ProfileEstimate>,
    pub frames: Vec<FrameResult>,
}

impl Report {
    /// Fastest fully-decoded frame among the given bands.
    pub fn best_frame(&self, bands: &[Band]) -> Option<&FrameResult> {
        self.frames
            .iter()
            .filter(|f| f.ok && bands.contains(&f.band))
            .max_by(|a, b| a.goodput_bps.total_cmp(&b.goodput_bps))
    }
}

// ------------------------------------------------------------------ analysis

fn fft_real(x: &[f32], n: usize) -> Vec<C32> {
    let mut buf = vec![C32::default(); n];
    for (b, &v) in buf.iter_mut().zip(x) {
        b.re = v;
    }
    FftPlanner::<f32>::new()
        .plan_fft_forward(n)
        .process(&mut buf);
    buf
}

fn ifft_real(mut spec: Vec<C32>) -> Vec<f32> {
    let n = spec.len();
    FftPlanner::<f32>::new()
        .plan_fft_inverse(n)
        .process(&mut spec);
    spec.iter().map(|c| c.re / n as f32).collect()
}

fn db(x: f64) -> f32 {
    (10.0 * x.max(1e-30).log10()) as f32
}

/// Sub-sample peak position by parabolic interpolation.
fn refine(c: &[f32], i: usize) -> f64 {
    if i == 0 || i + 1 >= c.len() {
        return i as f64;
    }
    let (a, b, d) = (c[i - 1] as f64, c[i] as f64, c[i + 1] as f64);
    let den = a - 2.0 * b + d;
    if den.abs() < 1e-12 {
        i as f64
    } else {
        i as f64 + 0.5 * (a - d) / den
    }
}

/// Locate both sweeps; returns (ess1_pos, ess2_pos) in samples (fractional).
fn find_sweeps(x: &[f32], sweep: &[f32]) -> Result<(f64, f64), String> {
    let corr = sync::correlate_normalized(x, sweep);
    let peaks = sync::find_peaks(&corr, 0.15, ESS_LEN / 2);
    let &(top, _) = peaks
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .ok_or("probe sweep not found — is this a recording of the probe? Too quiet?")?;
    let tol = (ESS_SPACING as f64 * 0.002) as usize; // ±2000 ppm
    let partner = peaks
        .iter()
        .filter(|&&(p, _)| {
            let d = p.abs_diff(top);
            d.abs_diff(ESS_SPACING) <= tol
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .ok_or("second probe sweep not found (recording cut short?)")?;
    let (a, b) = if partner.0 > top {
        (top, partner.0)
    } else {
        (partner.0, top)
    };
    Ok((refine(&corr, a), refine(&corr, b)))
}

/// Lag (samples) between the recorded sweep 1 and sweep 2, by cross-correlating
/// the recording with itself around the two coarse positions.
fn sweep_lag(x: &[f32], p1: f64, p2: f64) -> Option<f64> {
    let pre = 2400usize;
    let a = (p1 as usize).checked_sub(pre)?;
    let tmpl_len = ESS_LEN + 2 * pre;
    let template = x.get(a..a + tmpl_len)?;
    let coarse = (p2 - p1).round() as usize;
    let tol = 1200usize; // ±25 ms around the coarse lag
    let b = (a + coarse).checked_sub(tol)?;
    let window = x.get(b..b + tmpl_len + 2 * tol)?;
    let corr = sync::correlate_normalized(window, template);
    let i = (0..corr.len()).max_by(|&i, &j| corr[i].total_cmp(&corr[j]))?;
    if corr[i] < 0.3 {
        return None;
    }
    Some((b + i) as f64 - a as f64 + (refine(&corr, i) - i as f64))
}

pub fn analyze(recording: &[f32]) -> Result<Report, String> {
    let peak = recording.iter().fold(0f32, |m, v| m.max(v.abs()));
    let rms = (recording.iter().map(|&v| (v as f64).powi(2)).sum::<f64>()
        / recording.len().max(1) as f64)
        .sqrt();
    let sweep = ess();

    // 1. Clock drift from the two sweeps, then undo it.
    // Locating each sweep against the clean template can lock onto different
    // multipath arrivals for the two sweeps, so measure the lag between the two
    // *recorded* sweeps instead: both went through the same paths.
    let (p1, p2) = find_sweeps(recording, &sweep)?;
    let drift_ppm = sweep_lag(recording, p1, p2)
        .map(|lag| (lag / ESS_SPACING as f64 - 1.0) * 1e6)
        .unwrap_or(((p2 - p1) / ESS_SPACING as f64 - 1.0) * 1e6);
    let x: Vec<f32> = if drift_ppm.abs() > 0.5 {
        resample(recording, 1.0 + drift_ppm * 1e-6)
    } else {
        recording.to_vec()
    };
    let (p1, _) = find_sweeps(&x, &sweep)?;
    let p1 = p1.round() as usize;

    // 2. Impulse response by regularised deconvolution of sweep 1.
    let pre = 2400usize;
    let ir_len = ESS_LEN; // 1 s of response
    let seg_start = p1.saturating_sub(pre);
    let seg_end = (p1 + ESS_LEN + ir_len).min(x.len());
    let n = (seg_end - seg_start).next_power_of_two() * 2;
    let ys = fft_real(&x[seg_start..seg_end], n);
    // Reference sweep starts at index 0, so the direct path lands at index `pre`.
    let pre = p1 - seg_start;
    let xs = fft_real(&sweep, n);
    let xmax = xs.iter().map(|c| c.norm_sqr()).fold(0f32, f32::max);
    let eps = 1e-4 * xmax;
    let hspec: Vec<C32> = ys
        .iter()
        .zip(&xs)
        .map(|(y, xx)| y * xx.conj() / (xx.norm_sqr() + eps))
        .collect();
    let h = ifft_real(hspec.clone());
    let h = &h[..(pre + ir_len).min(h.len())];

    let search_lo = pre.saturating_sub(480);
    let search_hi = (pre + 4800).min(h.len());
    let t_peak = (search_lo..search_hi)
        .max_by(|&a, &b| h[a].abs().total_cmp(&h[b].abs()))
        .unwrap_or(pre);
    let hmax = h[t_peak].abs();
    let t0 = (search_lo..=t_peak)
        .find(|&i| h[i].abs() >= 0.5 * hmax)
        .unwrap_or(t_peak);

    let noise_floor = |hh: &[f32]| -> f64 {
        let a = hh.len() * 8 / 10;
        hh[a..].iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / (hh.len() - a).max(1) as f64
    };
    let nf = noise_floor(h);

    // DRR: direct window (−0.5 ms .. +2.5 ms) vs everything after.
    let d_lo = t0.saturating_sub(24);
    let d_hi = (t0 + 120).min(h.len());
    let e_direct: f64 = h[d_lo..d_hi].iter().map(|&v| (v as f64).powi(2) - nf).sum();
    // Subtract the noise floor from the sum (not per sample — that biases upward).
    let e_rev: f64 = h[d_hi..]
        .iter()
        .map(|&v| (v as f64).powi(2) - nf)
        .sum::<f64>()
        .max(0.0);
    let drr_db = db(e_direct.max(1e-30) / e_rev.max(1e-30));

    // RT60 from Schroeder backward integration (T20 fit, falls back to T10).
    let tail: Vec<f64> = h[t0..].iter().map(|&v| (v as f64).powi(2) - nf).collect();
    let mut edc = vec![0f64; tail.len()];
    let mut acc = 0f64;
    for i in (0..tail.len()).rev() {
        acc += tail[i];
        edc[i] = acc.max(1e-30);
    }
    let rt60_s = if acc > 0.0 {
        let edc_db: Vec<f32> = edc.iter().map(|&e| db(e / acc)).collect();
        let at = |lvl: f32| edc_db.iter().position(|&v| v <= lvl);
        match (at(-5.0), at(-25.0), at(-15.0)) {
            (Some(a), Some(b), _) if b > a => Some(3.0 * (b - a) as f32 / FS as f32),
            (Some(a), None, Some(b)) if b > a => Some(6.0 * (b - a) as f32 / FS as f32),
            _ => None,
        }
    } else {
        None
    };

    // 3. Background noise PSD from the silence after sweep 1 (reverb decayed).
    let noise_start = p1 + ESS_LEN + GAP1 / 2;
    let noise_seg = &x[noise_start.min(x.len())..(noise_start + GAP1 / 2).min(x.len())];
    let hann: Vec<f32> = (0..PERIOD)
        .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / PERIOD as f64).cos()) as f32)
        .collect();
    let mut noise_psd = vec![0f64; PERIOD / 2];
    let mut segs = 0;
    for w in noise_seg.windows(PERIOD).step_by(PERIOD / 2) {
        let win: Vec<f32> = w.iter().zip(&hann).map(|(a, b)| a * b).collect();
        for (p, c) in noise_psd.iter_mut().zip(fft_real(&win, PERIOD)) {
            *p += c.norm_sqr() as f64;
        }
        segs += 1;
    }
    noise_psd.iter_mut().for_each(|p| *p /= segs.max(1) as f64);

    // 4. Per-bin SNR from the periodic multitone.
    let (k_lo, k_hi) = sound_bins();
    let s0 = p1 + ESS_LEN + GAP1;
    let mut sum = vec![C32::default(); PERIOD / 2];
    let mut frames_fft = Vec::new();
    for j in SKIP..REPS {
        let a = s0 + j * PERIOD;
        if a + PERIOD > x.len() {
            break;
        }
        let f = fft_real(&x[a..a + PERIOD], PERIOD);
        for (s, c) in sum.iter_mut().zip(&f) {
            *s += c;
        }
        frames_fft.push(f);
    }
    let jn = frames_fft.len() as f32;
    if jn < 4.0 {
        return Err("recording too short for the sounding section".into());
    }
    let mean: Vec<C32> = sum.iter().map(|s| s / jn).collect();
    // Noise from differences of *adjacent* periods (like the differential decoder),
    // so slow gain/timing wander of the playback chain is not counted as noise.
    let mut var = vec![0f32; PERIOD / 2];
    for w in frames_fft.windows(2) {
        for k in 0..PERIOD / 2 {
            var[k] += (w[1][k] - w[0][k]).norm_sqr();
        }
    }
    var.iter_mut().for_each(|v| *v /= 2.0 * (jn - 1.0));
    // Band SNR = total signal power / total noise power over the bins (robust to
    // individual bins whose noisy estimate is ~0).
    let sig_pow: Vec<f64> = (0..PERIOD / 2)
        .map(|k| (mean[k].norm_sqr() - var[k] / jn).max(0.0) as f64)
        .collect();
    let band_snr = |a: usize, b: usize| -> f64 {
        let s: f64 = sig_pow[a..=b].iter().sum();
        let n: f64 = var[a..=b].iter().map(|&v| v as f64).sum();
        db(s / n.max(1e-30)) as f64
    };

    // 5. 1/3-octave summary.
    let hn = hspec.len();
    let mut freq = Vec::new();
    let mut fc = 125.0f64;
    let ref_band = |lo: f64, hi: f64| -> f64 {
        let (a, b) = (
            (lo * hn as f64 / FS) as usize,
            (hi * hn as f64 / FS) as usize,
        );
        hspec[a..=b]
            .iter()
            .map(|c| c.norm_sqr() as f64)
            .sum::<f64>()
            / (b - a + 1) as f64
    };
    let ref_pow = ref_band(1000.0, 4000.0);
    while fc <= 20_500.0 {
        let (lo, hi) = (fc / 2f64.powf(1.0 / 6.0), fc * 2f64.powf(1.0 / 6.0));
        let resp = ref_band(lo, hi.min(FS / 2.0 - 1.0));
        let bin = FS / PERIOD as f64;
        let (a, b) = (
            (lo / bin).ceil() as usize,
            ((hi / bin).floor() as usize).min(PERIOD / 2 - 1),
        );
        let nz = if b >= a {
            noise_psd[a..=b].iter().sum::<f64>() / (b - a + 1) as f64
        } else {
            0.0
        };
        let (sa, sb) = (a.max(k_lo), b.min(k_hi));
        let snr = if sb >= sa { band_snr(sa, sb) } else { f64::NAN };
        freq.push(FreqRow {
            f_center: fc as f32,
            response_db: db(resp / ref_pow),
            noise_db: db(nz),
            snr_db: snr as f32,
        });
        fc *= 2f64.powf(1.0 / 3.0);
    }

    // 6. Per-profile prediction.
    let sound_bw = (k_hi - k_lo + 1) as f64;
    let sound_power = mean_power(&sounding_period());
    let mut profiles = Vec::new();
    for band in Band::ALL {
        let p = band.params();
        let bin = FS / PERIOD as f64;
        let (a, b) = (
            (p.f_lo as f64 / bin).ceil() as usize,
            (p.f_hi as f64 / bin).floor() as usize,
        );
        let snr_sound = band_snr(a, b);
        // Probe frames put all power into the band instead of 100 Hz–22 kHz.
        // Probe frames put their power into the band instead of 100 Hz–22 kHz, but at a
        // lower average power than the low-crest multitone: scale by both.
        let noise_snr = snr_sound
            + 10.0 * (sound_bw / (b - a + 1) as f64).log10()
            + db(frame_power(band) / sound_power) as f64;

        // Echo arriving after the CP guard, band-limited to the profile.
        let banded: Vec<C32> = hspec
            .iter()
            .enumerate()
            .map(|(k, &c)| {
                let f = k.min(hn - k) as f64 * FS / hn as f64;
                if f >= p.f_lo as f64 && f <= p.f_hi as f64 {
                    c
                } else {
                    C32::default()
                }
            })
            .collect();
        let hb = ifft_real(banded);
        let hb = &hb[..h.len()];
        let nfb = noise_floor(hb);
        let guard = p.cp - p.backoff;
        let g_end = (t0 + guard).min(hb.len());
        let e_in: f64 = hb[t0.saturating_sub(24)..g_end]
            .iter()
            .map(|&v| (v as f64).powi(2))
            .sum();
        let e_out: f64 = hb[g_end..]
            .iter()
            .map(|&v| (v as f64).powi(2) - nfb)
            .sum::<f64>()
            .max(0.0);
        let sir = db(e_in / e_out.max(e_in * 1e-6)) as f64;

        let sir_eff = sir + ECHO_SIR_CALIBRATION_DB;
        let eff = -10.0 * (10f64.powf(-noise_snr / 10.0) + 10f64.powf(-sir_eff / 10.0)).log10();
        let best_rate = Rate::ALL
            .into_iter()
            .rev()
            .find(|&r| eff as f32 >= required_snr_db(r));
        profiles.push(ProfileEstimate {
            band,
            noise_snr_db: noise_snr as f32,
            reverb_sir_db: sir as f32,
            effective_snr_db: eff as f32,
            best_rate,
            predicted_bps: best_rate.map_or(0.0, |r| modem::net_bitrate(&p, r, 4096)),
        });
    }

    // 7. Ground truth: decode the test frames.
    let cfgs: Vec<_> = Band::ALL.iter().map(|b| b.config()).collect();
    let decoded: Vec<Frame> = modem::decode(&x, &cfgs);
    let mut frames = Vec::new();
    for band in Band::ALL {
        for rate in Rate::ALL {
            let f = decoded.iter().find(|f| f.band == band && f.rate == rate);
            frames.push(match f {
                None => FrameResult {
                    band,
                    rate,
                    found: false,
                    ok: false,
                    blocks_ok: 0,
                    blocks_total: 0,
                    snr_db: None,
                    goodput_bps: 0.0,
                },
                Some(f) => {
                    let ok = f.ok() && f.payload == expected_payload(band, rate, f.payload.len());
                    FrameResult {
                        band,
                        rate,
                        found: true,
                        ok,
                        blocks_ok: f.blocks_ok.iter().filter(|&&b| b).count(),
                        blocks_total: f.blocks_ok.len(),
                        snr_db: Some(f.snr_db),
                        goodput_bps: f.goodput_bps(),
                    }
                }
            });
        }
    }

    Ok(Report {
        peak_dbfs: db((peak as f64).powi(2)),
        rms_dbfs: db(rms * rms),
        drift_ppm,
        rt60_s,
        drr_db,
        freq,
        profiles,
        frames,
    })
}

/// Welch power spectrum summed into `width_hz` bands: (band start Hz, dB).
pub fn band_powers(x: &[f32], width_hz: f64) -> Vec<(f32, f32)> {
    let n = PERIOD;
    let hann: Vec<f32> = (0..n)
        .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()) as f32)
        .collect();
    let mut psd = vec![0f64; n / 2];
    let mut segs = 0;
    for w in x.windows(n).step_by(n / 2) {
        let win: Vec<f32> = w.iter().zip(&hann).map(|(a, b)| a * b).collect();
        for (p, c) in psd.iter_mut().zip(fft_real(&win, n)) {
            *p += c.norm_sqr() as f64;
        }
        segs += 1;
    }
    let bin = FS / n as f64;
    let per = (width_hz / bin).round().max(1.0) as usize;
    psd.chunks(per)
        .enumerate()
        .map(|(i, c)| {
            let p = c.iter().sum::<f64>() / segs.max(1) as f64 / (n as f64).powi(2);
            ((i * per) as f32 * bin as f32, db(p))
        })
        .collect()
}
