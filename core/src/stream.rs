//! Real-time receiver: feed microphone samples in arbitrary chunks, get frames
//! out as soon as they are complete, plus live status (level, incoming frame
//! progress) for a UI.

use crate::modem::{detection_filter, try_decode_at, Attempt, Frame, DETECT_THRESHOLD};
use crate::ofdm::Ofdm;
use crate::profiles::{Band, BandConfig, Rate};
use crate::sync;

/// Scan for preambles once at least this many new samples have arrived (~0.2 s).
const SCAN_EVERY: usize = 9600;
/// Without a frame in progress, keep at most this much audio (~3 s).
const IDLE_KEEP: usize = 3 * 48_000;

struct BandState {
    cfg: BandConfig,
    ofdm: Ofdm,
    chirp: Vec<f32>,
    /// Absolute sample index from which new preamble positions are accepted.
    scan_from: usize,
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
                    }
                })
                .collect(),
            buf: Vec::new(),
            base: 0,
            since_scan: 0,
            pending: None,
            level_db: -120.0,
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
                match try_decode_at(&self.buf, p.pos - self.base, &b.cfg, &b.ofdm, p.score) {
                    Attempt::Frame(mut f) => {
                        f.start += self.base;
                        f.end += self.base;
                        let end = f.end;
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
