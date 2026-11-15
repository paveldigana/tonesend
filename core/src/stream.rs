//! Real-time receiver: feed microphone samples in arbitrary chunks, get frames
//! out as soon as they are complete, plus live status (level, incoming frame
//! progress) for a UI.

use crate::modem::{
    block_sizes, decode_block, detection_filter, try_decode_at, Attempt, Frame, Hints, BLOCK_BYTES,
    DETECT_THRESHOLD,
};
use crate::ofdm::Ofdm;
use crate::profiles::{Band, BandConfig, Rate};
use crate::sync;

/// Scan for preambles once at least this many new samples have arrived (~0.2 s).
const SCAN_EVERY: usize = 9600;
/// Without a frame in progress, keep at most this much audio (~3 s).
const IDLE_KEEP: usize = 3 * 48_000;
/// Diagnostic records kept until read (oldest dropped beyond this).
const DIAG_KEEP: usize = 2000;
/// Carrier SNR is summarised into this many equal frequency slices in diagnostics.
const DIAG_SLICES: usize = 8;
/// Soft bits of failed frames are kept this long for combining with later copies (60 s).
const COMBINE_WINDOW: usize = 60 * 48_000;
/// At most this many failed frames are kept for combining.
const COMBINE_KEEP: usize = 8;
/// Pairwise combining tries the newest this-many stored copies before summing all of them.
const COMBINE_PAIRS: usize = 3;
/// A preamble this strong is real enough to try the band's last header when its own is unreadable.
const ASSUME_HEADER_SCORE: f32 = 0.4;
/// The last header of a band is assumed only within this time (60 s).
const HEADER_MEMORY: usize = 60 * 48_000;
/// Weight of a new good frame's drift in the running drift hint.
const DRIFT_SMOOTHING: f64 = 0.3;

/// What the receiver saw, for field logs (one record per detected preamble).
#[derive(Clone, Debug, PartialEq)]
pub enum Diag {
    /// A frame was demodulated (some blocks may have failed their checksum).
    Frame {
        band: Band,
        rate: Rate,
        /// Seconds since the receiver started (preamble start) and frame length.
        at_s: f64,
        dur_s: f64,
        bytes: usize,
        blocks_ok: usize,
        blocks: usize,
        snr_db: f32,
        /// Mean SNR of 8 equal frequency slices, low to high.
        slice_snr_db: Vec<f32>,
        drift_ppm: f64,
        score: f32,
        /// Copies combined to decode it (1 = this one alone).
        copies: usize,
        /// Decode attempts with different drift tracking.
        attempts: usize,
        /// Header was unreadable; the band's last header was assumed.
        header_assumed: bool,
    },
    /// A preamble was detected but no frame could be decoded from it.
    Rejected {
        band: Band,
        at_s: f64,
        score: f32,
        /// Header (payload length, rate) if it decoded; `None` = header failed too.
        header: Option<(usize, Rate)>,
        /// The header in `header` was assumed (from the band's last frame), not decoded;
        /// no block passed even after combining with stored copies.
        header_assumed: bool,
    },
}

impl Diag {
    /// One-line JSON object (no trailing newline).
    pub fn to_json(&self) -> String {
        fn f(v: f64) -> String {
            if v.is_finite() { format!("{:.2}", v) } else { "null".into() }
        }
        match self {
            Diag::Frame {
                band, rate, at_s, dur_s, bytes, blocks_ok, blocks, snr_db, slice_snr_db, drift_ppm, score,
                copies, attempts, header_assumed,
            } => {
                let slices: Vec<String> = slice_snr_db.iter().map(|&s| f(s as f64)).collect();
                format!(
                    "{{\"type\":\"rx_frame\",\"band\":\"{}\",\"rate\":\"{}\",\"at_s\":{},\"dur_s\":{},\"bytes\":{},\"blocks_ok\":{},\"blocks\":{},\"snr_db\":{},\"slice_snr_db\":[{}],\"drift_ppm\":{},\"score\":{},\"copies\":{},\"attempts\":{},\"header_assumed\":{}}}",
                    band.name(), rate.name(), f(*at_s), f(*dur_s), bytes, blocks_ok, blocks,
                    f(*snr_db as f64), slices.join(","), f(*drift_ppm), f(*score as f64),
                    copies, attempts, header_assumed
                )
            }
            Diag::Rejected { band, at_s, score, header, header_assumed } => {
                let h = match header {
                    Some((len, rate)) => format!("{{\"bytes\":{},\"rate\":\"{}\"}}", len, rate.name()),
                    None => "null".into(),
                };
                format!(
                    "{{\"type\":\"rx_rejected\",\"band\":\"{}\",\"at_s\":{},\"score\":{},\"header\":{},\"header_assumed\":{}}}",
                    band.name(), f(*at_s), f(*score as f64), h, header_assumed
                )
            }
        }
    }
}

fn slices(snr: &[f32]) -> Vec<f32> {
    if snr.is_empty() {
        return Vec::new();
    }
    (0..DIAG_SLICES)
        .filter_map(|i| {
            let a = i * snr.len() / DIAG_SLICES;
            let b = ((i + 1) * snr.len() / DIAG_SLICES).max(a + 1).min(snr.len());
            (a < b).then(|| snr[a..b].iter().sum::<f32>() / (b - a) as f32)
        })
        .collect()
}

struct BandState {
    cfg: BandConfig,
    ofdm: Ofdm,
    chirp: Vec<f32>,
    /// Absolute sample index from which new preamble positions are accepted.
    scan_from: usize,
    /// Header of the last frame decoded on this band, and where that frame ended.
    last_header: Option<((usize, Rate), usize)>,
}

/// Soft bits of a frame with failed blocks, kept to combine with later copies.
struct Stored {
    band: Band,
    rate: Rate,
    len: usize,
    at: usize,
    soft: Vec<Vec<f32>>,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    band: usize,
    pos: usize,
    score: f32,
    needed: usize,
    header: Option<(usize, Rate)>,
}

/// What the receiver is doing right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Activity {
    Listening,
    /// Preamble found, header not decoded yet.
    Detected {
        band: Band,
    },
    /// Header decoded; `progress` in 0..1 of the frame's air time.
    Receiving {
        band: Band,
        rate: Rate,
        bytes: usize,
        progress: f32,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub activity: Activity,
    /// RMS level of the most recent chunk, dBFS.
    pub level_db: f32,
    /// Seconds of audio consumed so far.
    pub seconds: f64,
}

pub struct Receiver {
    bands: Vec<BandState>,
    buf: Vec<f32>,
    /// Absolute index of `buf[0]`.
    base: usize,
    since_scan: usize,
    pending: Option<Pending>,
    level_db: f32,
    diag: Vec<Diag>,
    /// Running clock-drift estimate from good frames (ppm).
    drift_hint: Option<f64>,
    stored: Vec<Stored>,
}

impl Receiver {
    pub fn new(bands: &[Band]) -> Self {
        Receiver {
            bands: bands
                .iter()
                .map(|b| {
                    let cfg = b.config();
                    BandState {
                        ofdm: Ofdm::new(&cfg.params),
                        chirp: sync::chirp(&cfg.params),
                        cfg,
                        scan_from: 0,
                        last_header: None,
                    }
                })
                .collect(),
            buf: Vec::new(),
            base: 0,
            since_scan: 0,
            pending: None,
            level_db: -120.0,
            diag: Vec::new(),
            drift_hint: None,
            stored: Vec::new(),
        }
    }

    fn end(&self) -> usize {
        self.base + self.buf.len()
    }

    pub fn status(&self) -> Status {
        let activity = match self.pending {
            None => Activity::Listening,
            Some(p) => {
                let band = self.bands[p.band].cfg.band;
                match p.header {
                    None => Activity::Detected { band },
                    Some((bytes, rate)) => Activity::Receiving {
                        band,
                        rate,
                        bytes,
                        progress: ((self.end() - p.pos) as f32 / (p.needed - p.pos) as f32)
                            .clamp(0.0, 1.0),
                    },
                }
            }
        };
        Status {
            activity,
            level_db: self.level_db,
            seconds: self.end() as f64 / crate::SAMPLE_RATE as f64,
        }
    }

    /// Repair failed blocks of `f` by adding the soft bits of earlier failed copies of a
    /// frame with the same band, rate and length (a message looped in music or an ad).
    /// Every repaired block must pass its own CRC-32, so a wrong pairing is rejected.
    /// If blocks still fail, `f` is stored for the next copy.
    fn combine(&mut self, band: usize, f: &mut Frame) {
        self.stored.retain(|s| f.start < s.at + COMBINE_WINDOW);
        let len = f.payload.len();
        let sizes = block_sizes(len, f.rate, self.bands[band].ofdm.nsc);
        let matches: Vec<usize> = (0..self.stored.len())
            .rev()
            .filter(|&i| {
                let s = &self.stored[i];
                (s.band, s.rate, s.len) == (f.band, f.rate, len) && s.soft.len() == sizes.len()
            })
            .collect();
        for (i, &n) in sizes.iter().enumerate() {
            if f.blocks_ok[i] || f.soft[i].is_empty() {
                continue;
            }
            let mut fixed = None;
            // Pairs with the newest copies first, then everything at once.
            for &m in matches.iter().take(COMBINE_PAIRS) {
                let other = &self.stored[m].soft[i];
                if other.len() != f.soft[i].len() {
                    continue;
                }
                let sum: Vec<f32> = f.soft[i].iter().zip(other).map(|(a, b)| a + b).collect();
                if let (data, true) = decode_block(&sum, i, n) {
                    fixed = Some((data, 2));
                    break;
                }
            }
            if fixed.is_none() && matches.len() >= 2 {
                let mut sum = f.soft[i].clone();
                let mut used = 1;
                for &m in &matches {
                    let other = &self.stored[m].soft[i];
                    if other.len() == sum.len() {
                        sum.iter_mut().zip(other).for_each(|(a, b)| *a += b);
                        used += 1;
                    }
                }
                if used > 2 {
                    if let (data, true) = decode_block(&sum, i, n) {
                        fixed = Some((data, used));
                    }
                }
            }
            if let Some((data, used)) = fixed {
                let at = i * BLOCK_BYTES;
                f.payload[at..at + n].copy_from_slice(&data);
                f.blocks_ok[i] = true;
                f.soft[i] = Vec::new();
                f.copies = f.copies.max(used);
            }
        }
        if !f.ok() {
            if self.stored.len() >= COMBINE_KEEP {
                self.stored.remove(0);
            }
            self.stored.push(Stored {
                band: f.band,
                rate: f.rate,
                len,
                at: f.start,
                soft: f.soft.clone(),
            });
        }
    }

    /// Diagnostic records since the last call (oldest first).
    pub fn take_diag(&mut self) -> Vec<Diag> {
        std::mem::take(&mut self.diag)
    }

    fn log(&mut self, d: Diag) {
        if self.diag.len() >= DIAG_KEEP {
            self.diag.remove(0);
        }
        self.diag.push(d);
    }

    /// Feed samples (mono, 48 kHz); returns frames completed by this chunk.
    /// Frame `start`/`end` are absolute sample indices since the receiver started.
    pub fn push(&mut self, x: &[f32]) -> Vec<Frame> {
        if !x.is_empty() {
            let p = x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64;
            self.level_db = (10.0 * p.max(1e-12).log10()) as f32;
        }
        self.buf.extend_from_slice(x);
        self.since_scan += x.len();
        let mut out = Vec::new();
        self.process(&mut out);
        self.trim();
        out
    }

    fn process(&mut self, out: &mut Vec<Frame>) {
        loop {
            if let Some(p) = self.pending {
                if self.end() < p.needed {
                    return;
                }
                let b = &self.bands[p.band];
                let hints = Hints {
                    drift_ppm: self.drift_hint,
                    header: b
                        .last_header
                        .filter(|&(_, end)| p.score >= ASSUME_HEADER_SCORE && p.pos < end + HEADER_MEMORY)
                        .map(|(h, _)| h),
                };
                match try_decode_at(&self.buf, p.pos - self.base, &b.cfg, &b.ofdm, p.score, &hints) {
                    Attempt::Frame(mut f) => {
                        f.start += self.base;
                        f.end += self.base;
                        let end = f.end;
                        let sr = crate::SAMPLE_RATE as f64;
                        if !f.ok() {
                            self.combine(p.band, &mut f);
                        }
                        let any_ok = f.blocks_ok.iter().any(|&ok| ok) || f.blocks_ok.is_empty();
                        if f.header_assumed && !any_ok {
                            // Nothing confirmed the assumed header: treat it as a failed detection,
                            // but the soft bits stay stored for the next copy.
                            self.pending = None;
                            self.log(Diag::Rejected {
                                band: f.band,
                                at_s: f.start as f64 / sr,
                                score: f.detect_score,
                                header: Some((f.payload.len(), f.rate)),
                                header_assumed: true,
                            });
                            let b = &mut self.bands[p.band];
                            b.scan_from = b.scan_from.max(p.pos + b.cfg.params.chirp_len / 2);
                            continue;
                        }
                        if !f.header_assumed {
                            self.bands[p.band].last_header = Some(((f.payload.len(), f.rate), end));
                        }
                        if f.ok() && f.copies == 1 {
                            self.drift_hint = Some(match self.drift_hint {
                                Some(h) => h + DRIFT_SMOOTHING * (f.drift_ppm - h),
                                None => f.drift_ppm,
                            });
                        }
                        self.log(Diag::Frame {
                            band: f.band,
                            rate: f.rate,
                            at_s: f.start as f64 / sr,
                            dur_s: f.duration_s(),
                            bytes: f.payload.len(),
                            blocks_ok: f.blocks_ok.iter().filter(|&&b| b).count(),
                            blocks: f.blocks_ok.len(),
                            snr_db: f.snr_db,
                            slice_snr_db: slices(&f.carrier_snr_db),
                            drift_ppm: f.drift_ppm,
                            score: f.detect_score,
                            copies: f.copies,
                            attempts: f.attempts,
                            header_assumed: f.header_assumed,
                        });
                        out.push(f);
                        self.pending = None;
                        self.bands
                            .iter_mut()
                            .for_each(|b| b.scan_from = b.scan_from.max(end));
                    }
                    Attempt::NeedMore { needed, header } => {
                        self.pending = Some(Pending {
                            needed: needed + self.base,
                            header,
                            ..p
                        });
                        return;
                    }
                    Attempt::Invalid => {
                        self.pending = None;
                        let band = self.bands[p.band].cfg.band;
                        self.log(Diag::Rejected {
                            band,
                            at_s: p.pos as f64 / crate::SAMPLE_RATE as f64,
                            score: p.score,
                            header: p.header,
                            header_assumed: false,
                        });
                        let b = &mut self.bands[p.band];
                        b.scan_from = b.scan_from.max(p.pos + b.cfg.params.chirp_len / 2);
                    }
                }
                continue;
            }
            if self.since_scan < SCAN_EVERY {
                return;
            }
            self.since_scan = 0;
            if !self.scan() {
                return;
            }
        }
    }

    /// Look for preambles in not-yet-scanned audio; sets `pending` on the
    /// earliest candidate. Returns true if one was found.
    fn scan(&mut self) -> bool {
        const MARGIN: usize = 600; // filter edge history
        const CONFIRM: usize = 64; // samples after a peak needed to call it a local max
        let end = self.end();
        let mut cands: Vec<(usize, usize, f32)> = Vec::new(); // (abs pos, band, score)
        for (bi, b) in self.bands.iter_mut().enumerate() {
            let l = b.chirp.len();
            let from = b.scan_from.max(self.base);
            if end < from + l + CONFIRM {
                continue;
            }
            let seg_start = from.saturating_sub(MARGIN + b.cfg.params.cp).max(self.base);
            let seg = &self.buf[seg_start - self.base..];
            let corr = sync::correlate_normalized(&detection_filter(seg, &b.cfg.params), &b.chirp);
            let valid = corr.len().saturating_sub(CONFIRM);
            for (i, score) in sync::find_peaks(&corr[..valid], DETECT_THRESHOLD, l) {
                let pos = seg_start + sync::first_path(&corr, i, b.cfg.params.cp);
                if pos >= from {
                    cands.push((pos, bi, score));
                }
            }
            // Re-scan a little overlap next time so peaks at the edge are not missed.
            b.scan_from = (seg_start + valid).saturating_sub(l / 2).max(from);
        }
        let Some(&(pos, band, score)) = cands.iter().min_by_key(|c| c.0) else {
            return false;
        };
        // Candidates of *other* bands that were not chosen must stay in their band's
        // scan window — otherwise a false detection on one band (e.g. the Audible chirp
        // partly matching the Max chirp) would hide the real frame on another.
        for &(p, bi, _) in &cands {
            if (p, bi) != (pos, band) {
                let b = &mut self.bands[bi];
                b.scan_from = b.scan_from.min(p);
            }
        }
        self.pending = Some(Pending {
            band,
            pos,
            score,
            needed: pos,
            header: None,
        });
        true
    }

    fn trim(&mut self) {
        let keep_from = match self.pending {
            Some(p) => p.pos,
            None => {
                let scan = self.bands.iter().map(|b| b.scan_from).min().unwrap_or(0);
                let hist = self
                    .bands
                    .iter()
                    .map(|b| b.chirp.len() + b.cfg.params.cp + 1200)
                    .max()
                    .unwrap_or(0);
                scan.saturating_sub(hist)
                    .max(self.end().saturating_sub(IDLE_KEEP))
            }
        };
        if keep_from > self.base {
            let n = (keep_from - self.base).min(self.buf.len());
            self.buf.drain(..n);
            self.base += n;
        }
    }
}
