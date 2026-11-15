//! Transfer layer: files of any size over many short modem frames, with a
//! whole-file checksum and Reed–Solomon repair data.
//!
//! - The payload is cut into *shards*. Every shard is exactly one modem block
//!   (≤ 512 B, own CRC-32 in the modem) and starts with an 18-byte header
//!   (transfer id, file length, file CRC-32, group, index), so any correctly
//!   received block is usable on its own — regardless of which frame it was in.
//! - Shards are grouped (≤ 128 data shards per group); each group gets parity
//!   shards. Any `k` of a group's shards rebuild it.
//! - Frames carry 4 shards (~2 KB), so a glitch costs one short frame, not the file.
//! - Repair: the sender can later transmit *new* parity shards (higher indices);
//!   the receiver merges them with what it already has. No back channel needed.

use crate::modem::{self, Frame, BLOCK_BYTES};
use crate::profiles::{Band, BandConfig, Rate};
use crate::rs;
use crate::stream::{Receiver, Status};
use crate::util;
use std::collections::HashMap;

pub const SHARD_HEADER: usize = 18;
pub const SHARD_DATA: usize = BLOCK_BYTES - SHARD_HEADER;
pub const MAX_GROUP: usize = 128;
/// 8 shards ≈ 4 KB per frame: fewer preambles per byte; losses stay per block.
pub const SHARDS_PER_FRAME: usize = 8;
/// Silence between frames of one transfer (50 ms).
const FRAME_GAP: usize = 2400;
/// A transfer with no new frames for this long is reported as incomplete (3 s).
const IDLE_REPORT: usize = 3 * 48_000;
const MAGIC: [u8; 2] = [0x53, 0x48];
const VERSION: u8 = 1;

/// Deterministic split of a payload into groups of equal-size shards.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub total_len: usize,
    pub shard_size: usize,
    /// (first data shard, data shard count k) per group.
    pub groups: Vec<(usize, usize)>,
}

pub fn plan(total_len: usize) -> Plan {
    let shard_size = if total_len <= SHARD_DATA {
        total_len.max(1)
    } else {
        SHARD_DATA
    };
    let n = total_len.div_ceil(shard_size).max(1);
    let g = n.div_ceil(MAX_GROUP);
    let (base, extra) = (n / g, n % g);
    let mut groups = Vec::with_capacity(g);
    let mut start = 0;
    for i in 0..g {
        let k = base + (i < extra) as usize;
        groups.push((start, k));
        start += k;
    }
    Plan {
        total_len,
        shard_size,
        groups,
    }
}

/// Parity shards per group of `k` for a cumulative redundancy fraction.
fn parity_upto(k: usize, redundancy: f32) -> usize {
    ((k as f32 * redundancy.max(0.0)).round() as usize).min(rs::max_parity(k))
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ShardHeader {
    id: u32,
    total_len: u32,
    crc: u32,
    group: u16,
    index: u8,
}

impl ShardHeader {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(self.index);
        out.extend_from_slice(&self.group.to_le_bytes());
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.total_len.to_le_bytes());
        out.extend_from_slice(&self.crc.to_le_bytes());
    }

    fn parse(b: &[u8]) -> Option<ShardHeader> {
        if b.len() <= SHARD_HEADER || b[0..2] != MAGIC || b[2] != VERSION {
            return None;
        }
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Some(ShardHeader {
            index: b[3],
            group: u16::from_le_bytes([b[4], b[5]]),
            id: u32_at(6),
            total_len: u32_at(10),
            crc: u32_at(14),
        })
    }
}

/// Shards to transmit, interleaved across groups so bursts spread out.
/// `from`/`to` = cumulative redundancy already sent / to reach; `with_data`
/// includes the data shards themselves.
pub fn shards(data: &[u8], id: u32, from: f32, to: f32, with_data: bool) -> Vec<Vec<u8>> {
    let p = plan(data.len());
    let crc = util::crc32(data);
    let mut padded = data.to_vec();
    let n_data: usize = p.groups.iter().map(|g| g.1).sum();
    padded.resize(n_data * p.shard_size, 0);
    let chunk = |s: usize| &padded[s * p.shard_size..(s + 1) * p.shard_size];

    // Per group: list of (index, bytes) to send.
    let mut per_group: Vec<Vec<(u8, Vec<u8>)>> = Vec::new();
    for &(start, k) in &p.groups {
        let refs: Vec<&[u8]> = (start..start + k).map(chunk).collect();
        let mut list: Vec<(u8, Vec<u8>)> = Vec::new();
        if with_data {
            list.extend((0..k).map(|j| (j as u8, refs[j].to_vec())));
        }
        let (a, b) = (parity_upto(k, from), parity_upto(k, to));
        list.extend((a..b).map(|i| ((k + i) as u8, rs::parity(&refs, k + i))));
        // Parity budget exhausted: repair by resending data shards instead.
        if !with_data && a == b && to > from {
            let extra = ((k as f32 * (to - from)).round() as usize).clamp(1, k);
            list.extend((0..extra).map(|j| (j as u8, refs[j].to_vec())));
        }
        per_group.push(list);
    }
    let rounds = per_group.iter().map(|l| l.len()).max().unwrap_or(0);
    let mut out = Vec::new();
    for r in 0..rounds {
        for (g, list) in per_group.iter().enumerate() {
            if let Some((index, bytes)) = list.get(r) {
                let mut s = Vec::with_capacity(SHARD_HEADER + bytes.len());
                ShardHeader {
                    id,
                    total_len: data.len() as u32,
                    crc,
                    group: g as u16,
                    index: *index,
                }
                .write(&mut s);
                s.extend_from_slice(bytes);
                out.push(s);
            }
        }
    }
    out
}

fn frames_audio(shards: &[Vec<u8>], cfg: &BandConfig, rate: Rate) -> Vec<f32> {
    // Full-size shards are exactly one modem block, so 4 fit per frame. A short
    // (single-shard) payload goes one shard per frame to keep block alignment.
    let per = if shards.first().is_some_and(|s| s.len() == BLOCK_BYTES) {
        SHARDS_PER_FRAME
    } else {
        1
    };
    let mut out = Vec::new();
    for (i, group) in shards.chunks(per).enumerate() {
        if i > 0 {
            out.extend(std::iter::repeat_n(0.0, FRAME_GAP));
        }
        out.extend(modem::encode(&group.concat(), cfg, rate));
    }
    out
}

/// Audio for a complete transfer with `redundancy` (e.g. 0.25 = 25 % repair data).
pub fn encode(data: &[u8], cfg: &BandConfig, rate: Rate, id: u32, redundancy: f32) -> Vec<f32> {
    frames_audio(&shards(data, id, 0.0, redundancy, true), cfg, rate)
}

/// Audio carrying *additional* repair data (parity not sent before).
/// `sent` = redundancy already transmitted, `extra` = how much more.
pub fn encode_repair(
    data: &[u8],
    cfg: &BandConfig,
    rate: Rate,
    id: u32,
    sent: f32,
    extra: f32,
) -> Vec<f32> {
    frames_audio(&shards(data, id, sent, sent + extra, false), cfg, rate)
}

/// Air time (s) of a transfer of `len` bytes, without encoding it.
pub fn air_time(len: usize, cfg: &BandConfig, rate: Rate, redundancy: f32) -> f64 {
    let p = plan(len);
    let n: usize = p
        .groups
        .iter()
        .map(|&(_, k)| k + parity_upto(k, redundancy))
        .sum();
    let shard = p.shard_size + SHARD_HEADER;
    let per = if shard == BLOCK_BYTES {
        SHARDS_PER_FRAME
    } else {
        1
    };
    let frames = n.div_ceil(per);
    let mut samples = 0;
    for f in 0..frames {
        let in_frame = per.min(n - f * per);
        samples += modem::frame_samples(&cfg.params, rate, in_frame * shard);
    }
    samples += frames.saturating_sub(1) * FRAME_GAP;
    samples as f64 / cfg.params.sample_rate as f64
}

// ------------------------------------------------------------------ receiving

#[derive(Clone, Debug, Default)]
pub struct TransferStats {
    pub band: Option<Band>,
    pub rate: Option<Rate>,
    pub frames: usize,
    pub blocks_ok: usize,
    pub blocks_total: usize,
    /// Sum of frame air times (s).
    pub air_time: f64,
    pub snr_db: f32,
    pub drift_ppm: f64,
    /// Per-carrier SNR of the last frame and its band edges (Hz).
    pub carrier_snr_db: Vec<f32>,
    pub f_lo: f32,
    pub f_hi: f32,
    /// Distinct shards received vs. needed (Σ k).
    pub shards_have: usize,
    pub shards_needed: usize,
    /// Data shards rebuilt from parity.
    pub repaired: usize,
    pub crc: u32,
    pub total_len: usize,
}

impl TransferStats {
    /// Correct payload bits per second of air time.
    pub fn goodput_bps(&self) -> f64 {
        if self.air_time > 0.0 {
            self.total_len as f64 * 8.0 / self.air_time
        } else {
            0.0
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    /// The same looped message (e.g. an ID in a song) heard again; `count` = times so far.
    Repeat {
        id: u32,
        count: usize,
        stats: TransferStats,
    },
    /// Reassembled and whole-file CRC verified.
    Complete {
        id: u32,
        data: Vec<u8>,
        stats: TransferStats,
    },
    /// Transmission stopped before enough shards arrived; send repair data.
    Incomplete { id: u32, stats: TransferStats },
}

struct State {
    plan: Plan,
    crc: u32,
    groups: Vec<HashMap<u8, Vec<u8>>>,
    stats: TransferStats,
    snr_lin_sum: f64,
    last_sample: usize,
    reported: bool,
}

impl State {
    fn have_needed(&self) -> (usize, usize) {
        let mut have = 0;
        let mut need = 0;
        for (g, &(_, k)) in self.plan.groups.iter().enumerate() {
            have += self.groups[g].len().min(k);
            need += k;
        }
        (have, need)
    }

    fn try_complete(&mut self) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(self.plan.total_len);
        let mut repaired = 0;
        for (g, &(_, k)) in self.plan.groups.iter().enumerate() {
            let shards: Vec<(usize, &[u8])> = self.groups[g]
                .iter()
                .map(|(&i, s)| (i as usize, s.as_slice()))
                .collect();
            if shards.len() < k {
                return None;
            }
            // Prefer data shards (no maths needed for them).
            let mut sorted = shards;
            sorted.sort_by_key(|s| s.0);
            repaired += k - sorted.iter().filter(|s| s.0 < k).count().min(k);
            for d in rs::reconstruct(k, &sorted)? {
                out.extend(d);
            }
        }
        out.truncate(self.plan.total_len);
        self.stats.repaired = repaired;
        Some(out)
    }
}

/// Collects shards from decoded frames and emits completed / incomplete transfers.
#[derive(Default)]
pub struct Assembler {
    states: HashMap<u32, State>,
    done: Vec<u32>,
    /// Raw (looped) messages: id → (times heard in this session, sample index last heard).
    raw_seen: HashMap<u32, (usize, usize)>,
}

/// A looped message heard again within this window counts as a repeat, not a new reception (60 s).
const REPEAT_WINDOW: usize = 60 * 48_000;

impl Assembler {
    /// Feed one decoded frame. `now` = absolute sample index of the frame end.
    pub fn add_frame(&mut self, f: &Frame, out: &mut Vec<Event>) {
        let blocks: Vec<(&[u8], bool)> = f
            .payload
            .chunks(BLOCK_BYTES)
            .zip(&f.blocks_ok)
            .map(|(b, &ok)| (b, ok))
            .collect();
        // Transfer this frame belongs to = id of its good shards.
        let Some(hdr) = blocks
            .iter()
            .filter(|b| b.1)
            .find_map(|b| ShardHeader::parse(b.0))
        else {
            self.add_raw_frame(f, out);
            return;
        };
        if self.done.contains(&hdr.id) {
            return;
        }
        let st = self.states.entry(hdr.id).or_insert_with(|| {
            let plan = plan(hdr.total_len as usize);
            State {
                groups: vec![HashMap::new(); plan.groups.len()],
                plan,
                crc: hdr.crc,
                stats: TransferStats {
                    crc: hdr.crc,
                    total_len: hdr.total_len as usize,
                    ..Default::default()
                },
                snr_lin_sum: 0.0,
                last_sample: 0,
                reported: false,
            }
        });
        for (b, ok) in &blocks {
            if !ok {
                continue;
            }
            let Some(h) = ShardHeader::parse(b) else {
                continue;
            };
            if h.id != hdr.id || h.crc != st.crc || (h.group as usize) >= st.groups.len() {
                continue;
            }
            let body = &b[SHARD_HEADER..];
            if body.len() == st.plan.shard_size {
                st.groups[h.group as usize]
                    .entry(h.index)
                    .or_insert_with(|| body.to_vec());
            }
        }
        let s = &mut st.stats;
        s.band = Some(f.band);
        s.rate = Some(f.rate);
        s.frames += 1;
        s.blocks_ok += f.blocks_ok.iter().filter(|&&b| b).count();
        s.blocks_total += f.blocks_ok.len();
        s.air_time += f.duration_s();
        st.snr_lin_sum += 10f64.powf(f.snr_db as f64 / 10.0);
        s.snr_db = (10.0 * (st.snr_lin_sum / s.frames as f64).log10()) as f32;
        s.drift_ppm = f.drift_ppm;
        s.carrier_snr_db = f.carrier_snr_db.clone();
        let p = f.band.params();
        s.f_lo = p.k_lo() as f32 * p.bin_hz();
        s.f_hi = p.k_hi() as f32 * p.bin_hz();
        st.last_sample = f.end;
        st.reported = false;
        let (have, need) = st.have_needed();
        st.stats.shards_have = have;
        st.stats.shards_needed = need;

        if have >= need {
            if let Some(data) = st.try_complete() {
                if util::crc32(&data) == st.crc {
                    let stats = st.stats.clone();
                    self.states.remove(&hdr.id);
                    self.done.push(hdr.id);
                    if self.done.len() > 64 {
                        self.done.remove(0);
                    }
                    out.push(Event::Complete {
                        id: hdr.id,
                        data,
                        stats,
                    });
                }
            }
        }
    }

    /// A complete frame without transfer-layer shards (e.g. `tonesend encode`, or an ID
    /// embedded in music by `tonesend embed`): deliver it as a one-frame transfer.
    /// Repeats of the same payload (IDs are usually looped) are reported once.
    fn add_raw_frame(&mut self, f: &Frame, out: &mut Vec<Event>) {
        if !f.ok() || f.payload.is_empty() {
            return;
        }
        let id = util::crc32(&f.payload);
        let repeat = match self.raw_seen.get_mut(&id) {
            Some((count, last)) if f.end.saturating_sub(*last) < REPEAT_WINDOW => {
                *count += 1;
                *last = f.end;
                Some(*count)
            }
            _ => {
                self.raw_seen.insert(id, (1, f.end));
                None
            }
        };
        if self.raw_seen.len() > 256 {
            let now = f.end;
            self.raw_seen
                .retain(|_, (_, last)| now.saturating_sub(*last) < REPEAT_WINDOW);
        }
        let p = f.band.params();
        let stats = TransferStats {
            band: Some(f.band),
            rate: Some(f.rate),
            frames: 1,
            blocks_ok: f.blocks_ok.len(),
            blocks_total: f.blocks_ok.len(),
            air_time: f.duration_s(),
            snr_db: f.snr_db,
            drift_ppm: f.drift_ppm,
            carrier_snr_db: f.carrier_snr_db.clone(),
            f_lo: p.k_lo() as f32 * p.bin_hz(),
            f_hi: p.k_hi() as f32 * p.bin_hz(),
            shards_have: 1,
            shards_needed: 1,
            repaired: 0,
            crc: util::crc32(&f.payload),
            total_len: f.payload.len(),
        };
        match repeat {
            Some(count) => out.push(Event::Repeat { id, count, stats }),
            None => out.push(Event::Complete {
                id,
                data: f.payload.clone(),
                stats,
            }),
        }
    }

    /// Report transfers that went quiet without completing.
    pub fn check_idle(&mut self, now: usize, out: &mut Vec<Event>) {
        for (&id, st) in self.states.iter_mut() {
            if !st.reported && now > st.last_sample + IDLE_REPORT {
                st.reported = true;
                out.push(Event::Incomplete {
                    id,
                    stats: st.stats.clone(),
                });
            }
        }
        // Keep memory bounded: drop the stalest when many are pending.
        while self.states.len() > 16 {
            let oldest = *self
                .states
                .iter()
                .min_by_key(|(_, s)| s.last_sample)
                .unwrap()
                .0;
            self.states.remove(&oldest);
        }
    }

    /// Progress of the most recently active unfinished transfer.
    pub fn active(&self) -> Option<(u32, &TransferStats)> {
        self.states
            .iter()
            .max_by_key(|(_, s)| s.last_sample)
            .map(|(&id, s)| (id, &s.stats))
    }
}

/// Microphone samples in, completed transfers out.
pub struct TransferReceiver {
    rx: Receiver,
    asm: Assembler,
    samples: usize,
}

impl TransferReceiver {
    pub fn new(bands: &[Band]) -> Self {
        TransferReceiver {
            rx: Receiver::new(bands),
            asm: Assembler::default(),
            samples: 0,
        }
    }

    pub fn push(&mut self, x: &[f32]) -> Vec<Event> {
        self.samples += x.len();
        let mut out = Vec::new();
        for f in self.rx.push(x) {
            self.asm.add_frame(&f, &mut out);
        }
        // Don't call a transfer stalled while a frame is still arriving.
        if matches!(
            self.rx.status().activity,
            crate::stream::Activity::Listening
        ) {
            self.asm.check_idle(self.samples, &mut out);
        }
        out
    }

    pub fn status(&self) -> Status {
        self.rx.status()
    }

    /// Frame-level diagnostic records since the last call (see [`crate::stream::Diag`]).
    pub fn take_diag(&mut self) -> Vec<crate::stream::Diag> {
        self.rx.take_diag()
    }

    /// (transfer id, stats) of the transfer currently being assembled, if any.
    pub fn active(&self) -> Option<(u32, &TransferStats)> {
        self.asm.active()
    }
}
