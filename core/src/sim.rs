//! Acoustic channel simulator: speaker/mic frequency response, room reverb,
//! sample-clock drift, clipping and additive white noise.

use crate::util::Rng;
use rustfft::{num_complex::Complex32 as C32, FftPlanner};
use std::f64::consts::PI;

#[derive(Clone, Debug)]
pub struct Channel {
    /// Full-band (0..fs/2) SNR of white noise vs. *transmitted* signal power
    /// (before speaker/mic roll-off and room). None = noiseless.
    pub snr_db: Option<f32>,
    /// Reverberation time in seconds (0 = no reverb).
    pub rt60: f32,
    /// Direct-to-reverberant energy ratio in dB.
    pub drr_db: f32,
    /// Receiver sample clock relative to sender, parts per million (+ = receiver fast).
    pub ppm: f64,
    /// Speaker/mic response: 2nd-order high-pass and 4th-order low-pass corners (0 = off).
    pub low_cut_hz: f32,
    pub high_cut_hz: f32,
    pub clip: Option<f32>,
    pub gain: f32,
    /// Silence before/after the signal (a random 0–100 ms is added to the lead).
    pub lead_s: f32,
    pub tail_s: f32,
    pub seed: u64,
}

impl Channel {
    pub const PRESETS: [&'static str; 5] = ["clean", "desk", "room", "noisy", "far"];

    pub fn clean() -> Self {
        Channel {
            snr_db: None,
            rt60: 0.0,
            drr_db: 100.0,
            ppm: 0.0,
            low_cut_hz: 0.0,
            high_cut_hz: 0.0,
            clip: None,
            gain: 1.0,
            lead_s: 0.25,
            tail_s: 0.25,
            seed: 1,
        }
    }

    /// Named scenarios. "desk": phones ~30 cm apart on a table; "room": ~1–2 m
    /// in a furnished room; "noisy": desk distance with chatter-level noise;
    /// "far": several metres, reverberant.
    pub fn preset(name: &str) -> Option<Self> {
        let c = Channel::clean();
        Some(match name {
            "clean" => c,
            "desk" => Channel {
                snr_db: Some(30.0),
                rt60: 0.25,
                drr_db: 12.0,
                ppm: 40.0,
                low_cut_hz: 300.0,
                high_cut_hz: 16000.0,
                ..c
            },
            "room" => Channel {
                snr_db: Some(20.0),
                rt60: 0.4,
                drr_db: 4.0,
                ppm: 80.0,
                low_cut_hz: 300.0,
                high_cut_hz: 15000.0,
                ..c
            },
            "noisy" => Channel {
                snr_db: Some(10.0),
                rt60: 0.3,
                drr_db: 8.0,
                ppm: 60.0,
                low_cut_hz: 300.0,
                high_cut_hz: 16000.0,
                ..c
            },
            "far" => Channel {
                snr_db: Some(12.0),
                rt60: 0.6,
                drr_db: 0.0,
                ppm: 100.0,
                low_cut_hz: 300.0,
                high_cut_hz: 14000.0,
                ..c
            },
            _ => return None,
        })
    }

    pub fn apply(&self, x: &[f32], fs: u32) -> Vec<f32> {
        let mut rng = Rng::new(self.seed);
        let fsf = fs as f32;

        let h = self.impulse_response(fs, &mut rng);
        let y = self.filter(x, &h, fs);

        let lead = (self.lead_s * fsf) as usize + rng.below(fs as usize / 10 + 1);
        let tail = (self.tail_s * fsf) as usize;
        let mut z = vec![0f32; lead];
        z.extend(y);
        z.extend(std::iter::repeat_n(0f32, tail));

        if self.ppm != 0.0 {
            z = resample(&z, 1.0 / (1.0 + self.ppm * 1e-6));
        }
        if self.gain != 1.0 {
            z.iter_mut().for_each(|v| *v *= self.gain);
        }
        if let Some(c) = self.clip {
            z.iter_mut().for_each(|v| *v = v.clamp(-c, c));
        }
        if let Some(snr) = self.snr_db {
            // Reference = transmitted power (before speaker/mic response and room),
            // so a band the hardware attenuates really does arrive with lower SNR.
            let power = x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64
                * (self.gain as f64).powi(2);
            let sigma = (power / 10f64.powf(snr as f64 / 10.0)).sqrt();
            z.iter_mut()
                .for_each(|v| *v += (sigma * rng.gauss()) as f32);
        }
        z
    }

    fn impulse_response(&self, fs: u32, rng: &mut Rng) -> Vec<f32> {
        if self.rt60 <= 0.0 {
            return vec![1.0];
        }
        let len = (self.rt60 as f64 * fs as f64) as usize;
        let start = (0.0015 * fs as f64) as usize; // first reflection after ~1.5 ms
        let mut h = vec![0f32; len.max(start + 1)];
        let decay = 6.9 / (self.rt60 as f64 * fs as f64); // -60 dB at rt60
        let mut e = 0f64;
        for (n, v) in h.iter_mut().enumerate().skip(start) {
            let s = rng.gauss() * (-decay * n as f64).exp();
            *v = s as f32;
            e += s * s;
        }
        let target = 10f64.powf(-self.drr_db as f64 / 10.0);
        let g = if e > 0.0 {
            (target / e).sqrt() as f32
        } else {
            0.0
        };
        h.iter_mut().skip(start).for_each(|v| *v *= g);
        h[0] = 1.0;
        h
    }

    /// FFT convolution with `h`, plus zero-phase speaker/mic magnitude response.
    fn filter(&self, x: &[f32], h: &[f32], fs: u32) -> Vec<f32> {
        let out_len = x.len() + h.len() - 1;
        let identity = h.len() == 1 && self.low_cut_hz <= 0.0 && self.high_cut_hz <= 0.0;
        if identity {
            return x.iter().map(|v| v * h[0]).collect();
        }
        let n = out_len.next_power_of_two();
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);

        let mut xs = vec![C32::default(); n];
        xs.iter_mut().zip(x).for_each(|(d, &s)| d.re = s);
        let mut hs = vec![C32::default(); n];
        hs.iter_mut().zip(h).for_each(|(d, &s)| d.re = s);
        fft.process(&mut xs);
        fft.process(&mut hs);

        for k in 0..n {
            let f = k.min(n - k) as f32 * fs as f32 / n as f32;
            let mut mag = 1.0f32;
            if self.low_cut_hz > 0.0 {
                mag /= (1.0 + (self.low_cut_hz / f.max(1e-3)).powi(4)).sqrt();
            }
            if self.high_cut_hz > 0.0 {
                mag /= (1.0 + (f / self.high_cut_hz).powi(8)).sqrt();
            }
            xs[k] = xs[k] * hs[k] * mag;
        }
        ifft.process(&mut xs);
        xs[..out_len].iter().map(|c| c.re / n as f32).collect()
    }
}

/// Band-limited (windowed-sinc) resampling: `out[m] = x(m * ratio)`,
/// where `ratio` = input samples per output sample.
pub fn resample(x: &[f32], ratio: f64) -> Vec<f32> {
    const HALF: i64 = 16;
    let out_len = (x.len() as f64 / ratio).floor() as usize;
    let cutoff = (1.0 / ratio).min(1.0) * 0.95;
    let mut out = Vec::with_capacity(out_len);
    for m in 0..out_len {
        let t = m as f64 * ratio;
        let n0 = t.floor() as i64;
        let mut acc = 0f64;
        for n in (n0 - HALF + 1)..=(n0 + HALF) {
            if n < 0 || n as usize >= x.len() {
                continue;
            }
            let u = t - n as f64;
            let a = PI * cutoff * u;
            let sinc = if a.abs() < 1e-9 { 1.0 } else { a.sin() / a };
            let r = u / HALF as f64;
            let w = 0.42 + 0.5 * (PI * r).cos() + 0.08 * (2.0 * PI * r).cos();
            acc += x[n as usize] as f64 * cutoff * sinc * w;
        }
        out.push(acc as f32);
    }
    out
}
