//! Frame synchronisation: linear-chirp preamble and normalised matched filter.

use crate::profiles::OfdmParams;
use rustfft::{num_complex::Complex32 as C32, FftPlanner};
use std::f64::consts::PI;

/// Unit-amplitude linear chirp sweeping the profile's band, with tapered edges.
pub fn chirp(p: &OfdmParams) -> Vec<f32> {
    let l = p.chirp_len;
    let fs = p.sample_rate as f64;
    let dur = l as f64 / fs;
    let (f0, f1) = (p.f_lo as f64, p.f_hi as f64);
    let taper = (l / 16).max(1);
    (0..l)
        .map(|i| {
            let t = i as f64 / fs;
            let phase = 2.0 * PI * (f0 * t + (f1 - f0) * t * t / (2.0 * dur));
            let edge = i.min(l - 1 - i);
            let w = if edge < taper {
                0.5 - 0.5 * (PI * edge as f64 / taper as f64).cos()
            } else {
                1.0
            };
            (phase.sin() * w) as f32
        })
        .collect()
}

/// Envelope of the normalised cross-correlation between `x` and `template`:
/// `out[n] ≈ 1` where `x[n..n+L]` is a scaled copy of the template, ~0 for noise.
/// Uses an analytic (Hilbert) template so the result does not depend on carrier phase.
pub fn correlate_normalized(x: &[f32], template: &[f32]) -> Vec<f32> {
    let l = template.len();
    if x.len() < l || l == 0 {
        return Vec::new();
    }
    let out_len = x.len() - l + 1;
    let m = (4 * l).next_power_of_two().max(16384);
    let step = m - l + 1;

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(m);
    let ifft = planner.plan_fft_inverse(m);

    // conj(analytic template spectrum)
    let mut tspec = vec![C32::default(); m];
    for (d, &t) in tspec.iter_mut().zip(template) {
        d.re = t;
    }
    fft.process(&mut tspec);
    for (k, c) in tspec.iter_mut().enumerate() {
        *c = if k == 0 || k == m / 2 {
            c.conj()
        } else if k < m / 2 {
            (*c * 2.0).conj()
        } else {
            C32::default()
        };
    }

    let et: f64 = template.iter().map(|&v| (v as f64).powi(2)).sum();
    // Below −100 dBFS average power the window holds no usable sound.
    let min_energy = l as f64 * 1e-10;
    let mut ps = Vec::with_capacity(x.len() + 1);
    ps.push(0f64);
    let mut acc = 0f64;
    for &v in x {
        acc += (v as f64).powi(2);
        ps.push(acc);
    }

    let mut out = vec![0f32; out_len];
    let mut buf = vec![C32::default(); m];
    let mut s = 0;
    while s < out_len {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = C32::new(*x.get(s + i).unwrap_or(&0.0), 0.0);
        }
        fft.process(&mut buf);
        for (b, t) in buf.iter_mut().zip(&tspec) {
            *b *= t;
        }
        ifft.process(&mut buf);
        for i in 0..step.min(out_len - s) {
            let c = buf[i].norm() as f64 / m as f64;
            let e = ps[s + i + l] - ps[s + i];
            // Digital silence (iOS delivers exact zeros while the mic starts or is muted):
            // the ratio would divide FFT round-off by ~0 and give absurd scores.
            out[s + i] = if e < min_energy {
                0.0
            } else {
                (c / (e * et).sqrt()) as f32
            };
        }
        s += step;
    }
    out
}

/// Local maxima ≥ `threshold`, at least `min_dist` apart (strongest wins), sorted by position.
pub fn find_peaks(corr: &[f32], threshold: f32, min_dist: usize) -> Vec<(usize, f32)> {
    let n = corr.len();
    let mut cands: Vec<(usize, f32)> = (0..n)
        .filter(|&i| {
            corr[i] >= threshold
                && (i == 0 || corr[i] >= corr[i - 1])
                && (i + 1 == n || corr[i] > corr[i + 1])
        })
        .map(|i| (i, corr[i]))
        .collect();
    cands.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut chosen: Vec<(usize, f32)> = Vec::new();
    for c in cands {
        if chosen.iter().all(|&(p, _)| p.abs_diff(c.0) >= min_dist) {
            chosen.push(c);
        }
    }
    chosen.sort_by_key(|c| c.0);
    chosen
}

/// With multipath the strongest correlation peak may be a reflection; step back
/// to the earliest point within `search` samples that is still ≥ 60 % of the peak.
pub fn first_path(corr: &[f32], peak: usize, search: usize) -> usize {
    let v = corr[peak];
    (peak.saturating_sub(search)..=peak)
        .find(|&i| corr[i] >= 0.6 * v)
        .unwrap_or(peak)
}

/// Zero-phase band-pass (linear-phase FIR, delay compensated) — output aligned with input.
/// Used before detection so out-of-band noise (e.g. low-frequency room rumble)
/// does not dilute the normalised correlation score.
pub fn bandpass(x: &[f32], f_lo: f32, f_hi: f32, fs: u32) -> Vec<f32> {
    const TAPS: usize = 511;
    let half = (TAPS / 2) as f64;
    let (a, b) = (f_lo as f64 / fs as f64, f_hi as f64 / fs as f64);
    let lp = |fc: f64, n: f64| {
        if n == 0.0 {
            2.0 * fc
        } else {
            (2.0 * PI * fc * n).sin() / (PI * n)
        }
    };
    let h: Vec<f32> = (0..TAPS)
        .map(|i| {
            let n = i as f64 - half;
            let w = 0.42 + 0.5 * (PI * n / half).cos() + 0.08 * (2.0 * PI * n / half).cos();
            ((lp(b, n) - lp(a, n)) * w) as f32
        })
        .collect();

    let m = 16384usize;
    let step = m - TAPS + 1;
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(m);
    let ifft = planner.plan_fft_inverse(m);
    let mut hs = vec![C32::default(); m];
    hs.iter_mut().zip(&h).for_each(|(d, &v)| d.re = v);
    fft.process(&mut hs);

    // y[n] = Σ h[j] x[n + half - j]  (delay removed)
    let delay = TAPS / 2;
    let mut out = vec![0f32; x.len()];
    let mut buf = vec![C32::default(); m];
    let mut s: isize = -((TAPS - 1) as isize); // input index of buf[0]
    while s + (TAPS as isize - 1) < (x.len() + delay) as isize {
        for (i, b) in buf.iter_mut().enumerate() {
            let idx = s + i as isize;
            *b = C32::new(
                if idx >= 0 && (idx as usize) < x.len() {
                    x[idx as usize]
                } else {
                    0.0
                },
                0.0,
            );
        }
        fft.process(&mut buf);
        buf.iter_mut().zip(&hs).for_each(|(b, h)| *b *= h);
        ifft.process(&mut buf);
        // valid outputs: buf[TAPS-1 .. m) = full convolution at input index s+i
        for i in (TAPS - 1)..m {
            let n = s + i as isize - delay as isize; // output index after delay removal
            if n >= 0 && (n as usize) < out.len() {
                out[n as usize] = buf[i].re / m as f32;
            }
        }
        s += step as isize;
    }
    out
}
