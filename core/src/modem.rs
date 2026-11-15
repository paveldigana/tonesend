//! Frame encoder / decoder.
//!
//! Frame on air:
//! ```text
//! [ chirp | guard | training | header (DBPSK, r=1/2) | payload (rate profile) ]
//! ```
//! Every OFDM symbol is differentially encoded against the previous one on the
//! same subcarrier (training symbol = reference), so no channel estimation is
//! needed. Sample-clock drift is tracked decision-directed from the phase slope
//! across subcarriers.
//!
//! Payload is split into blocks of `BLOCK_BYTES`; each block is scrambled,
//! CRC-32 protected, convolutionally coded, punctured and interleaved
//! independently, so a damaged block can be identified (and later re-requested).

use crate::fec;
use crate::ofdm::Ofdm;
use crate::profiles::{Band, BandConfig, OfdmParams, Rate};
use crate::sync;
use crate::util::{self, Rng};
use rustfft::num_complex::Complex32 as C32;
use std::f32::consts::{FRAC_1_SQRT_2, FRAC_PI_2, FRAC_PI_4, SQRT_2};
use std::f64::consts::PI;

pub const BLOCK_BYTES: usize = 512;
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;
/// Minimum normalised chirp correlation to attempt decoding at a position.
pub const DETECT_THRESHOLD: f32 = 0.2;

const MAGIC: u8 = 0x5A;
const VERSION: u8 = 1;
const HEADER_BYTES: usize = 8;
const SEED_TRAINING: u64 = 0x5452_4149;
const SEED_HEADER: u64 = 0x4844_5252;
const SEED_PAYLOAD: u64 = 0x5041_594C;
const SEED_SCRAMBLE: u64 = 0x5343_524D;
/// Loop gain of the drift tracker (0..1).
const TIMING_GAIN: f64 = 0.25;
/// Largest per-symbol timing correction the tracker accepts (samples). Real
/// clock drift is ~0.1 sample/symbol even at 100 ppm.
const MAX_STEP: f64 = 0.5;
/// Pseudo-observations pulling per-subcarrier noise toward the band average.
const NOISE_REG: f32 = 4.0;

// ---------------------------------------------------------------- layout

#[derive(Clone, Debug)]
pub struct BlockLayout {
    pub data_bytes: usize,
    pub coded_bits: usize,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub blocks: Vec<BlockLayout>,
    pub total_bits: usize,
    pub symbols: usize,
}

/// Deterministic payload layout — receiver recomputes it from the header.
pub fn layout(len: usize, rate: Rate, nsc: usize) -> Layout {
    let mut blocks = Vec::new();
    let mut rem = len;
    while rem > 0 {
        let b = rem.min(BLOCK_BYTES);
        rem -= b;
        let info = (b + 4) * 8;
        let coded_bits = fec::punctured_len(fec::coded_len(info), rate.code_rate());
        blocks.push(BlockLayout {
            data_bytes: b,
            coded_bits,
        });
    }
    let total_bits: usize = blocks.iter().map(|b| b.coded_bits).sum();
    let symbols = total_bits.div_ceil(nsc * rate.bits_per_carrier());
    Layout {
        blocks,
        total_bits,
        symbols,
    }
}

fn header_coded_bits() -> usize {
    fec::coded_len(HEADER_BYTES * 8)
}

pub fn header_symbols(nsc: usize) -> usize {
    header_coded_bits().div_ceil(nsc)
}

/// Total frame length in samples for a payload of `len` bytes.
pub fn frame_samples(p: &OfdmParams, rate: Rate, len: usize) -> usize {
    let nsc = p.nsc();
    let nsym = 1 + header_symbols(nsc) + layout(len, rate, nsc).symbols;
    p.chirp_len + p.guard + nsym * p.sym_len() + p.win
}

/// Payload bits per second including all framing/FEC overhead.
pub fn net_bitrate(p: &OfdmParams, rate: Rate, len: usize) -> f64 {
    len as f64 * 8.0 * p.sample_rate as f64 / frame_samples(p, rate, len) as f64
}

// ---------------------------------------------------------------- helpers

fn training(nsc: usize) -> Vec<C32> {
    let mut r = Rng::new(SEED_TRAINING);
    (0..nsc)
        .map(|_| C32::from_polar(1.0, FRAC_PI_4 + r.below(4) as f32 * FRAC_PI_2))
        .collect()
}

fn scramble(data: &mut [u8], block: usize) {
    let mut r = Rng::new(SEED_SCRAMBLE.wrapping_add(block as u64));
    for b in data {
        *b ^= (r.next_u64() >> 56) as u8;
    }
}

fn header_bytes(len: u32, rate: Rate) -> [u8; HEADER_BYTES] {
    let mut h = [0u8; HEADER_BYTES];
    h[0] = MAGIC;
    h[1] = (VERSION << 4) | rate.id();
    h[2..6].copy_from_slice(&len.to_le_bytes());
    let c = util::crc16(&h[..6]);
    h[6..8].copy_from_slice(&c.to_be_bytes());
    h
}

fn parse_header(h: &[u8]) -> Option<(usize, Rate)> {
    if h.len() < HEADER_BYTES || h[0] != MAGIC || h[1] >> 4 != VERSION {
        return None;
    }
    if util::crc16(&h[..6]) != u16::from_be_bytes([h[6], h[7]]) {
        return None;
    }
    let rate = Rate::from_id(h[1] & 0x0F)?;
    let len = u32::from_le_bytes([h[2], h[3], h[4], h[5]]) as usize;
    (len <= MAX_PAYLOAD).then_some((len, rate))
}

/// Differential phase increment for 1 (DBPSK) or 2 (Gray DQPSK, π/4-offset) bits.
fn increment(bits: &[u8]) -> C32 {
    let s = |b: u8| 1.0 - 2.0 * b as f32;
    match bits.len() {
        1 => C32::new(s(bits[0]), 0.0),
        2 => C32::new(s(bits[0]) * FRAC_1_SQRT_2, s(bits[1]) * FRAC_1_SQRT_2),
        _ => {
            let label = (bits[0] << 2) | (bits[1] << 1) | bits[2];
            psk8_point(gray_inverse(label))
        }
    }
}

/// 8PSK point index m → phase m·π/4 + π/8. Its bit label is gray(m).
fn psk8_point(m: u8) -> C32 {
    C32::from_polar(1.0, (m as f32 + 0.5) * std::f32::consts::FRAC_PI_4)
}

fn gray(m: u8) -> u8 {
    m ^ (m >> 1)
}

fn gray_inverse(g: u8) -> u8 {
    g ^ (g >> 1) ^ (g >> 2)
}

fn decide(d: C32, bps: usize) -> C32 {
    match bps {
        1 => C32::new(d.re.signum(), 0.0),
        2 => C32::new(d.re.signum() * FRAC_1_SQRT_2, d.im.signum() * FRAC_1_SQRT_2),
        _ => {
            let a = d.arg() / std::f32::consts::FRAC_PI_4 - 0.5;
            psk8_point((a.round() as i32).rem_euclid(8) as u8)
        }
    }
}

fn ramp(i: usize, p: &OfdmParams) -> f32 {
    let w = p.win;
    let body_end = p.cp + p.nfft;
    if w == 0 {
        1.0
    } else if i < w {
        0.5 - 0.5 * (std::f32::consts::PI * (i as f32 + 0.5) / w as f32).cos()
    } else if i >= body_end {
        let j = i - body_end;
        0.5 + 0.5 * (std::f32::consts::PI * (j as f32 + 0.5) / w as f32).cos()
    } else {
        1.0
    }
}

// ---------------------------------------------------------------- encoder

/// Encode `payload` into a mono f32 signal at `cfg.params.sample_rate`, peak 0.9.
pub fn encode(payload: &[u8], cfg: &BandConfig, rate: Rate) -> Vec<f32> {
    assert!(payload.len() <= MAX_PAYLOAD, "payload too large");
    let p = &cfg.params;
    let ofdm = Ofdm::new(p);
    let nsc = ofdm.nsc;
    let bps = rate.bits_per_carrier();

    // Header bits (fixed robust mode).
    let hs = header_symbols(nsc);
    let hdr = header_bytes(payload.len() as u32, rate);
    let mut hbits = util::interleave(&fec::encode(&util::bytes_to_bits(&hdr)), SEED_HEADER);
    hbits.resize(hs * nsc, 0);

    // Payload bits.
    let lay = layout(payload.len(), rate, nsc);
    let mut pbits = Vec::with_capacity(lay.symbols * nsc * bps);
    for (i, chunk) in payload.chunks(BLOCK_BYTES).enumerate() {
        let mut data = chunk.to_vec();
        scramble(&mut data, i);
        let crc = util::crc32(&data);
        data.extend_from_slice(&crc.to_be_bytes());
        let coded = fec::puncture(&fec::encode(&util::bytes_to_bits(&data)), rate.code_rate());
        debug_assert_eq!(coded.len(), lay.blocks[i].coded_bits);
        pbits.extend(util::interleave(
            &coded,
            SEED_PAYLOAD.wrapping_add(i as u64),
        ));
    }
    pbits.resize(lay.symbols * nsc * bps, 0);

    // Differential symbol chain.
    let train = training(nsc);
    let mut cur = train.clone();
    let mut syms = vec![train];
    for s in 0..hs {
        for (i, c) in cur.iter_mut().enumerate() {
            *c *= increment(&hbits[s * nsc + i..][..1]);
        }
        syms.push(cur.clone());
    }
    for s in 0..lay.symbols {
        for (i, c) in cur.iter_mut().enumerate() {
            let o = (s * nsc + i) * bps;
            *c *= increment(&pbits[o..o + bps]);
        }
        syms.push(cur.clone());
    }

    // Time domain: chirp, guard, windowed CP-OFDM symbols (overlap-add).
    let bodies: Vec<Vec<f32>> = syms.iter().map(|s| ofdm.modulate(s)).collect();
    let rms = (bodies
        .iter()
        .flatten()
        .map(|&v| (v as f64).powi(2))
        .sum::<f64>()
        / (bodies.len() * p.nfft) as f64)
        .sqrt() as f32;

    let start = p.chirp_len + p.guard;
    let mut out = vec![0f32; start + syms.len() * p.sym_len() + p.win];
    for (o, c) in out.iter_mut().zip(sync::chirp(p)) {
        *o = c * rms * SQRT_2;
    }
    let ext = p.cp + p.nfft + p.win;
    for (t, body) in bodies.iter().enumerate() {
        let pos = start + t * p.sym_len();
        for i in 0..ext {
            out[pos + i] += body[(i + p.nfft - p.cp) % p.nfft] * ramp(i, p);
        }
    }

    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak > 0.0 {
        let g = 0.9 / peak;
        out.iter_mut().for_each(|v| *v *= g);
    }
    out
}

// ---------------------------------------------------------------- decoder

#[derive(Clone, Debug)]
pub struct Frame {
    pub band: Band,
    pub rate: Rate,
    pub sample_rate: u32,
    /// Sample index of the preamble start / one past the frame end.
    pub start: usize,
    pub end: usize,
    /// Payload as decoded; bytes of failed blocks are unreliable.
    pub payload: Vec<u8>,
    pub blocks_ok: Vec<bool>,
    pub detect_score: f32,
    /// Average post-differential SNR estimate over the band.
    pub snr_db: f32,
    pub carrier_snr_db: Vec<f32>,
    /// Estimated receiver-vs-sender sample-clock offset.
    pub drift_ppm: f64,
}

impl Frame {
    pub fn ok(&self) -> bool {
        self.blocks_ok.iter().all(|&b| b)
    }

    pub fn ok_bytes(&self) -> usize {
        self.blocks_ok
            .iter()
            .enumerate()
            .filter(|(_, &ok)| ok)
            .map(|(i, _)| BLOCK_BYTES.min(self.payload.len() - i * BLOCK_BYTES))
            .sum()
    }

    pub fn duration_s(&self) -> f64 {
        (self.end - self.start) as f64 / self.sample_rate as f64
    }

    /// Correct payload bits per second of air time.
    pub fn goodput_bps(&self) -> f64 {
        self.ok_bytes() as f64 * 8.0 / self.duration_s()
    }
}

/// Find and decode all frames of the given band profiles in `samples`.
pub fn decode(samples: &[f32], cfgs: &[BandConfig]) -> Vec<Frame> {
    let mut frames = Vec::new();
    for cfg in cfgs {
        let p = &cfg.params;
        let filtered = detection_filter(samples, p);
        let corr = sync::correlate_normalized(&filtered, &sync::chirp(p));
        let ofdm = Ofdm::new(p);
        let mut next_free = 0;
        for (peak, score) in sync::find_peaks(&corr, DETECT_THRESHOLD, p.chirp_len) {
            if peak < next_free {
                continue;
            }
            let pos = sync::first_path(&corr, peak, p.cp);
            if let Some(f) = decode_at(samples, pos, cfg, &ofdm, score) {
                next_free = f.end;
                frames.push(f);
            }
        }
    }
    frames.sort_by_key(|f| f.start);
    frames
}

/// Band-limit to the profile (plus margin) before chirp detection.
pub(crate) fn detection_filter(x: &[f32], p: &OfdmParams) -> Vec<f32> {
    let hi = (p.f_hi * 1.05).min(p.sample_rate as f32 / 2.0 - 100.0);
    sync::bandpass(x, p.f_lo * 0.9, hi, p.sample_rate)
}

#[derive(Default)]
struct Collected {
    d: Vec<Vec<C32>>,
    dec: Vec<Vec<C32>>,
}

impl Collected {
    /// Soft bits (LLR-like, + = 0) and per-subcarrier SNR in dB.
    fn soft(&self, nsc: usize, bps: usize) -> (Vec<f32>, Vec<f32>) {
        let t = self.d.len() as f32;
        let mut pk = vec![0f32; nsc];
        for row in &self.d {
            for (p, v) in pk.iter_mut().zip(row) {
                *p += v.norm();
            }
        }
        pk.iter_mut().for_each(|p| *p /= t);

        let mut err = vec![0f32; nsc];
        for (row, dec) in self.d.iter().zip(&self.dec) {
            for k in 0..nsc {
                err[k] += (row[k] - dec[k] * pk[k]).norm_sqr();
            }
        }
        let global = err.iter().sum::<f32>() / (t * nsc as f32);
        let noise: Vec<f32> = err
            .iter()
            .map(|e| ((e + NOISE_REG * global) / (t + NOISE_REG)).max(f32::MIN_POSITIVE))
            .collect();
        // LLR ∝ Re(d)/σ², σ² ≈ var(d) / (2|H|²)
        let scale: Vec<f32> = (0..nsc).map(|k| 2.0 * pk[k] / noise[k]).collect();

        let mut soft = Vec::with_capacity(self.d.len() * nsc * bps);
        let points: Vec<(C32, u8)> = (0..8u8).map(|m| (psk8_point(m), gray(m))).collect();
        for row in &self.d {
            for (k, (v, s)) in row.iter().zip(&scale).enumerate() {
                if bps == 3 {
                    // Max-log LLR per bit over the Gray-labelled 8PSK points.
                    let y = v / pk[k].max(f32::MIN_POSITIVE);
                    let w = pk[k] * pk[k] / noise[k];
                    for bit in (0..3).rev() {
                        let (mut d0, mut d1) = (f32::MAX, f32::MAX);
                        for &(c, label) in &points {
                            let dist = (y - c).norm_sqr();
                            if (label >> bit) & 1 == 0 {
                                d0 = d0.min(dist)
                            } else {
                                d1 = d1.min(dist)
                            }
                        }
                        soft.push((d1 - d0) * w);
                    }
                } else {
                    soft.push(v.re * s);
                    if bps == 2 {
                        soft.push(v.im * s);
                    }
                }
            }
        }
        let snr = (0..nsc)
            .map(|k| 10.0 * (2.0 * pk[k] * pk[k] / noise[k]).max(1e-6).log10())
            .collect();
        (soft, snr)
    }

    fn push(&mut self, (d, dec): (Vec<C32>, Vec<C32>)) {
        self.d.push(d);
        self.dec.push(dec);
    }
}

/// Symbol-by-symbol demodulator with decision-directed drift tracking.
struct Demod<'a> {
    ofdm: &'a Ofdm,
    s: &'a [f32],
    base: usize,
    idx: usize,
    /// Estimated cumulative timing offset (samples) and its per-symbol rate.
    d_hat: f64,
    r_hat: f64,
    prev: Vec<C32>,
    /// Running average symbol power, to spot bursts (noise, knocks) that
    /// must not steer the drift tracker.
    avg_pow: f32,
}

impl<'a> Demod<'a> {
    fn next_raw(&mut self) -> Option<Vec<C32>> {
        let p = &self.ofdm.p;
        let n = p.nfft;
        let nominal = (self.base + self.idx * p.sym_len() + p.cp - p.backoff) as f64;
        let shift = self.d_hat.round();
        let start = nominal + shift;
        if start < 0.0 || start as usize + n > self.s.len() {
            return None;
        }
        let start = start as usize;
        let mut y = self.ofdm.demodulate(&self.s[start..start + n]);
        // Remove the fractional part of the timing estimate as a phase ramp.
        let frac = shift - self.d_hat;
        for (i, v) in y.iter_mut().enumerate() {
            let k = (self.ofdm.k_lo + i) as f64;
            *v *= C32::from_polar(1.0, (-2.0 * PI * k * frac / n as f64) as f32);
        }
        self.idx += 1;
        Some(y)
    }

    fn diff_symbol(&mut self, bps: usize) -> Option<(Vec<C32>, Vec<C32>)> {
        let y = self.next_raw()?;
        let k_lo = self.ofdm.k_lo as f64;
        let d: Vec<C32> = y
            .iter()
            .zip(&self.prev)
            .map(|(a, b)| a * b.conj())
            .collect();

        // Residual timing step shows up as phase ∝ subcarrier index; fit slope through 0.
        let (mut num, mut den) = (0f64, 0f64);
        for (i, &v) in d.iter().enumerate() {
            let e = (v * decide(v, bps).conj()).arg() as f64;
            let w = v.norm() as f64;
            let k = k_lo + i as f64;
            num += w * k * e;
            den += w * k * k;
        }
        let slope = if den > 0.0 { num / den } else { 0.0 };

        let d: Vec<C32> = d
            .iter()
            .enumerate()
            .map(|(i, v)| v * C32::from_polar(1.0, (-slope * (k_lo + i as f64)) as f32))
            .collect();
        let dec = d.iter().map(|&v| decide(v, bps)).collect();

        // Only learn drift from symbols that look like signal: a burst changes
        // the power a lot and would push the tracker off for the rest of the frame.
        let pow = y.iter().map(|v| v.norm_sqr()).sum::<f32>() / y.len().max(1) as f32;
        let normal = self.avg_pow <= 0.0 || (pow < 4.0 * self.avg_pow && pow > 0.25 * self.avg_pow);
        if normal {
            let delta = (slope * self.ofdm.p.nfft as f64 / (2.0 * PI)).clamp(-MAX_STEP, MAX_STEP);
            self.r_hat -= TIMING_GAIN * delta;
            self.avg_pow = if self.avg_pow <= 0.0 {
                pow
            } else {
                0.9 * self.avg_pow + 0.1 * pow
            };
        }
        self.d_hat += self.r_hat;
        self.prev = y;
        Some((d, dec))
    }
}

/// Outcome of decoding at a candidate preamble position.
#[derive(Debug)]
pub enum Attempt {
    Frame(Frame),
    /// Not enough samples yet. `needed` = buffer length that will suffice;
    /// `header` = (payload length, rate) once the header has been decoded.
    NeedMore {
        needed: usize,
        header: Option<(usize, Rate)>,
    },
    /// No valid frame here (false detection or corrupted header).
    Invalid,
}

/// Decode a frame whose preamble starts at `pos` (streaming-friendly).
pub fn try_decode_at(
    samples: &[f32],
    pos: usize,
    cfg: &BandConfig,
    ofdm: &Ofdm,
    score: f32,
) -> Attempt {
    let p = &cfg.params;
    let hs = header_symbols(ofdm.nsc);
    let base = pos + p.chirp_len + p.guard;
    let symbols_end = |n: usize| base + n * p.sym_len() + p.win + 64;
    let header_end = symbols_end(1 + hs);
    if samples.len() < header_end {
        return Attempt::NeedMore {
            needed: header_end,
            header: None,
        };
    }
    let Some((len, rate)) = decode_header_at(samples, base, ofdm) else {
        return Attempt::Invalid;
    };
    let frame_end = symbols_end(1 + hs + layout(len, rate, ofdm.nsc).symbols);
    if samples.len() < frame_end {
        return Attempt::NeedMore {
            needed: frame_end,
            header: Some((len, rate)),
        };
    }
    match decode_at(samples, pos, cfg, ofdm, score) {
        Some(f) => Attempt::Frame(f),
        None => Attempt::Invalid,
    }
}

fn decode_header_at(samples: &[f32], base: usize, ofdm: &Ofdm) -> Option<(usize, Rate)> {
    let nsc = ofdm.nsc;
    let mut dm = Demod {
        ofdm,
        s: samples,
        base,
        idx: 0,
        d_hat: 0.0,
        r_hat: 0.0,
        prev: Vec::new(),
        avg_pow: 0.0,
    };
    dm.prev = dm.next_raw()?;
    let mut hc = Collected::default();
    for _ in 0..header_symbols(nsc) {
        hc.push(dm.diff_symbol(1)?);
    }
    let (hsoft, _) = hc.soft(nsc, 1);
    let hsoft = util::deinterleave(&hsoft[..header_coded_bits()], SEED_HEADER);
    parse_header(&util::bits_to_bytes(&fec::viterbi(
        &hsoft,
        HEADER_BYTES * 8,
    )))
}

fn decode_at(
    samples: &[f32],
    pos: usize,
    cfg: &BandConfig,
    ofdm: &Ofdm,
    score: f32,
) -> Option<Frame> {
    let p = &cfg.params;
    let nsc = ofdm.nsc;
    let base = pos + p.chirp_len + p.guard;
    let mut dm = Demod {
        ofdm,
        s: samples,
        base,
        idx: 0,
        d_hat: 0.0,
        r_hat: 0.0,
        prev: Vec::new(),
        avg_pow: 0.0,
    };
    dm.prev = dm.next_raw()?;

    // Header.
    let hs = header_symbols(nsc);
    let mut hc = Collected::default();
    for _ in 0..hs {
        hc.push(dm.diff_symbol(1)?);
    }
    let (hsoft, hsnr) = hc.soft(nsc, 1);
    let hsoft = util::deinterleave(&hsoft[..header_coded_bits()], SEED_HEADER);
    let hbits = fec::viterbi(&hsoft, HEADER_BYTES * 8);
    let (len, rate) = parse_header(&util::bits_to_bytes(&hbits))?;

    // Payload.
    let bps = rate.bits_per_carrier();
    let lay = layout(len, rate, nsc);
    let mut pc = Collected::default();
    for _ in 0..lay.symbols {
        pc.push(dm.diff_symbol(bps)?);
    }
    let (psoft, snr) = if lay.symbols > 0 {
        pc.soft(nsc, bps)
    } else {
        (Vec::new(), hsnr)
    };

    let mut payload = Vec::with_capacity(len);
    let mut blocks_ok = Vec::with_capacity(lay.blocks.len());
    let mut off = 0;
    for (i, bl) in lay.blocks.iter().enumerate() {
        let s = util::deinterleave(
            &psoft[off..off + bl.coded_bits],
            SEED_PAYLOAD.wrapping_add(i as u64),
        );
        off += bl.coded_bits;
        let info_bits = (bl.data_bytes + 4) * 8;
        let full = fec::depuncture(&s, fec::coded_len(info_bits), rate.code_rate());
        let bytes = util::bits_to_bytes(&fec::viterbi(&full, info_bits));
        let (data, crc) = bytes.split_at(bl.data_bytes);
        blocks_ok.push(util::crc32(data) == u32::from_be_bytes([crc[0], crc[1], crc[2], crc[3]]));
        let mut data = data.to_vec();
        scramble(&mut data, i);
        payload.extend(data);
    }

    let lin_mean = snr.iter().map(|&s| 10f32.powf(s / 10.0)).sum::<f32>() / snr.len() as f32;
    Some(Frame {
        band: cfg.band,
        rate,
        sample_rate: p.sample_rate,
        start: pos,
        end: base + (1 + hs + lay.symbols) * p.sym_len() + p.win,
        payload,
        blocks_ok,
        detect_score: score,
        snr_db: 10.0 * lin_mean.log10(),
        carrier_snr_db: snr,
        drift_ppm: dm.r_hat / p.sym_len() as f64 * 1e6,
    })
}
