//! Band profiles (which frequencies / symbol timing) and rate profiles
//! (modulation + code rate). Both sides must agree on the band profile;
//! the rate profile travels in the frame header.

use crate::fec::CodeRate;
use std::fmt;
use std::str::FromStr;

pub const SAMPLE_RATE: u32 = 48_000;

/// OFDM numerology for one band profile. All lengths are in samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OfdmParams {
    pub sample_rate: u32,
    /// FFT size; subcarrier spacing = sample_rate / nfft.
    pub nfft: usize,
    /// Cyclic prefix — must exceed the room's significant echo spread.
    pub cp: usize,
    /// Raised-cosine overlap between symbols (limits spectral splatter / clicks).
    pub win: usize,
    pub f_lo: f32,
    pub f_hi: f32,
    pub chirp_len: usize,
    /// Silence between preamble chirp and the first OFDM symbol.
    pub guard: usize,
    /// Receiver starts its FFT window this many samples early (inside the CP)
    /// so small timing errors land in the CP instead of the next symbol.
    pub backoff: usize,
    /// Peak clipping level relative to symbol RMS (0 = off). Lower = louder, more distortion.
    pub clip_ratio: f32,
}

impl OfdmParams {
    pub fn bin_hz(&self) -> f32 {
        self.sample_rate as f32 / self.nfft as f32
    }
    pub fn k_lo(&self) -> usize {
        (self.f_lo / self.bin_hz()).ceil() as usize
    }
    pub fn k_hi(&self) -> usize {
        (self.f_hi / self.bin_hz()).floor() as usize
    }
    /// Number of data subcarriers.
    pub fn nsc(&self) -> usize {
        self.k_hi() - self.k_lo() + 1
    }
    pub fn sym_len(&self) -> usize {
        self.nfft + self.cp
    }
    pub fn symbol_rate(&self) -> f64 {
        self.sample_rate as f64 / self.sym_len() as f64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Band {
    /// ~1–8 kHz: fastest, best SNR on phone speakers. Audible.
    AudibleWide,
    /// ~1.5–5 kHz with long symbols/CP: fallback for reverb, distance, noise.
    AudibleNarrow,
    /// ~17.5–20.5 kHz: silent for most adults, lower throughput and range.
    Inaudible,
    /// ~1–13 kHz: maximum speed at short range (needs good speaker/mic response).
    AudibleMax,
    /// ~14.6–16.4 kHz: hard to hear for most adults, but below the cut-off of
    /// typical 128 kbit/s MP3/AAC — for data riding in compressed music/ads.
    High,
}

impl Band {
    // Order = wire/FFI band id; append only.
    pub const ALL: [Band; 5] = [
        Band::AudibleWide,
        Band::AudibleNarrow,
        Band::Inaudible,
        Band::AudibleMax,
        Band::High,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Band::AudibleWide => "wide",
            Band::AudibleNarrow => "narrow",
            Band::Inaudible => "inaudible",
            Band::AudibleMax => "max",
            Band::High => "high",
        }
    }

    pub fn params(self) -> OfdmParams {
        let base = OfdmParams {
            sample_rate: SAMPLE_RATE,
            nfft: 1024,
            cp: 256,
            win: 48,
            f_lo: 0.0,
            f_hi: 0.0,
            chirp_len: 4096,
            guard: 1024,
            backoff: 24,
            clip_ratio: 3.0,
        };
        match self {
            Band::AudibleWide => OfdmParams {
                f_lo: 1000.0,
                f_hi: 8000.0,
                ..base
            },
            Band::AudibleNarrow => OfdmParams {
                nfft: 2048,
                cp: 512,
                win: 64,
                f_lo: 1500.0,
                f_hi: 5000.0,
                chirp_len: 8192,
                guard: 2048,
                ..base
            },
            Band::Inaudible => OfdmParams {
                f_lo: 17500.0,
                f_hi: 20500.0,
                ..base
            },
            Band::AudibleMax => OfdmParams {
                f_lo: 1000.0,
                f_hi: 13000.0,
                ..base
            },
            Band::High => OfdmParams {
                f_lo: 14600.0,
                f_hi: 16400.0,
                ..base
            },
        }
    }

    pub fn config(self) -> BandConfig {
        BandConfig {
            band: self,
            params: self.params(),
        }
    }
}

impl fmt::Display for Band {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Band {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "wide" | "audible" | "audible-wide" => Ok(Band::AudibleWide),
            "narrow" | "audible-narrow" => Ok(Band::AudibleNarrow),
            "inaudible" | "silent" | "ultrasonic" => Ok(Band::Inaudible),
            "max" | "audible-max" => Ok(Band::AudibleMax),
            "high" => Ok(Band::High),
            _ => Err(format!(
                "unknown band '{s}' (wide | narrow | inaudible | max | high)"
            )),
        }
    }
}

/// A band profile plus its (possibly overridden) numerology.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandConfig {
    pub band: Band,
    pub params: OfdmParams,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rate {
    /// DBPSK, rate 1/2 — most robust.
    Safe,
    /// DQPSK, rate 1/2.
    Normal,
    /// DQPSK, rate 3/4.
    Fast,
    /// D8PSK, rate 3/4 — fastest; needs ~6 dB more SNR than Fast.
    Turbo,
}

impl Rate {
    pub const ALL: [Rate; 4] = [Rate::Safe, Rate::Normal, Rate::Fast, Rate::Turbo];

    pub fn id(self) -> u8 {
        match self {
            Rate::Safe => 0,
            Rate::Normal => 1,
            Rate::Fast => 2,
            Rate::Turbo => 3,
        }
    }

    pub fn from_id(id: u8) -> Option<Rate> {
        Rate::ALL.into_iter().find(|r| r.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Rate::Safe => "safe",
            Rate::Normal => "normal",
            Rate::Fast => "fast",
            Rate::Turbo => "turbo",
        }
    }

    /// Bits carried per subcarrier per OFDM symbol.
    pub fn bits_per_carrier(self) -> usize {
        match self {
            Rate::Safe => 1,
            Rate::Normal | Rate::Fast => 2,
            Rate::Turbo => 3,
        }
    }

    pub fn code_rate(self) -> CodeRate {
        match self {
            Rate::Safe | Rate::Normal => CodeRate::R1_2,
            Rate::Fast | Rate::Turbo => CodeRate::R3_4,
        }
    }

    pub fn modulation(self) -> &'static str {
        match self.bits_per_carrier() {
            1 => "DBPSK",
            2 => "DQPSK",
            _ => "D8PSK",
        }
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Rate {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "safe" => Ok(Rate::Safe),
            "normal" => Ok(Rate::Normal),
            "fast" => Ok(Rate::Fast),
            "turbo" => Ok(Rate::Turbo),
            _ => Err(format!("unknown rate '{s}' (safe | normal | fast | turbo)")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numerology_sane() {
        for b in Band::ALL {
            let p = b.params();
            assert!(p.k_hi() < p.nfft / 2, "{b}: band above Nyquist");
            assert!(p.cp >= p.backoff + p.win, "{b}: window must fit in CP");
            assert!(p.nsc() >= 32, "{b}: too few subcarriers");
        }
    }
}
