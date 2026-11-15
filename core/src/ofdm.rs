//! One OFDM symbol: subcarrier values <-> real time-domain body (no CP).

use crate::profiles::OfdmParams;
use rustfft::{num_complex::Complex32 as C32, Fft, FftPlanner};
use std::sync::Arc;

pub struct Ofdm {
    pub p: OfdmParams,
    pub k_lo: usize,
    pub nsc: usize,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
}

impl Ofdm {
    pub fn new(p: &OfdmParams) -> Self {
        let mut planner = FftPlanner::new();
        Self {
            p: *p,
            k_lo: p.k_lo(),
            nsc: p.nsc(),
            fft: planner.plan_fft_forward(p.nfft),
            ifft: planner.plan_fft_inverse(p.nfft),
        }
    }

    /// Subcarrier values (len nsc) -> real symbol body (len nfft), peak-limited
    /// and re-filtered so clipping noise stays inside the band.
    pub fn modulate(&self, sc: &[C32]) -> Vec<f32> {
        assert_eq!(sc.len(), self.nsc);
        let n = self.p.nfft;
        let mut spec = vec![C32::default(); n];
        for (i, &v) in sc.iter().enumerate() {
            let k = self.k_lo + i;
            spec[k] = v;
            spec[n - k] = v.conj();
        }
        self.ifft.process(&mut spec);
        let mut body: Vec<f32> = spec.iter().map(|c| c.re / n as f32).collect();
        if self.p.clip_ratio > 0.0 {
            self.clip_and_filter(&mut body);
        }
        body
    }

    fn clip_and_filter(&self, body: &mut [f32]) {
        let n = body.len();
        let rms = (body.iter().map(|v| v * v).sum::<f32>() / n as f32).sqrt();
        let lim = self.p.clip_ratio * rms;
        let mut buf: Vec<C32> = body
            .iter()
            .map(|&v| C32::new(v.clamp(-lim, lim), 0.0))
            .collect();
        self.fft.process(&mut buf);
        let k_hi = self.k_lo + self.nsc - 1;
        for (k, c) in buf.iter_mut().enumerate() {
            let kk = if k <= n / 2 { k } else { n - k };
            if kk < self.k_lo || kk > k_hi {
                *c = C32::default();
            }
        }
        self.ifft.process(&mut buf);
        for (b, c) in body.iter_mut().zip(&buf) {
            *b = c.re / n as f32;
        }
    }

    /// Time window (len nfft) -> data subcarrier values (len nsc).
    pub fn demodulate(&self, window: &[f32]) -> Vec<C32> {
        assert_eq!(window.len(), self.p.nfft);
        let mut buf: Vec<C32> = window.iter().map(|&v| C32::new(v, 0.0)).collect();
        self.fft.process(&mut buf);
        buf[self.k_lo..self.k_lo + self.nsc].to_vec()
    }
}
