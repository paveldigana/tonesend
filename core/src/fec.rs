//! Forward error correction: K=7 convolutional code (133/171 octal, as in
//! 802.11a/g) with puncturing and a soft-decision Viterbi decoder.
//!
//! Soft-value convention everywhere: positive = bit 0, negative = bit 1,
//! magnitude = confidence. Punctured positions are fed as 0.0 (erasure).

pub const TAIL: usize = 6;
const G0: u8 = 0o133;
const G1: u8 = 0o171;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeRate {
    R1_2,
    R2_3,
    R3_4,
}

impl CodeRate {
    /// Keep-mask over the mother-code output stream A0 B0 A1 B1 ...
    fn pattern(self) -> &'static [u8] {
        match self {
            CodeRate::R1_2 => &[1, 1],
            CodeRate::R2_3 => &[1, 1, 1, 0],
            CodeRate::R3_4 => &[1, 1, 1, 0, 0, 1],
        }
    }

    pub fn ratio(self) -> f64 {
        match self {
            CodeRate::R1_2 => 1.0 / 2.0,
            CodeRate::R2_3 => 2.0 / 3.0,
            CodeRate::R3_4 => 3.0 / 4.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CodeRate::R1_2 => "1/2",
            CodeRate::R2_3 => "2/3",
            CodeRate::R3_4 => "3/4",
        }
    }
}

#[inline]
fn parity(x: u8) -> u8 {
    (x.count_ones() & 1) as u8
}

/// Length of the mother-code output (rate 1/2, terminated) for `info_bits`.
pub fn coded_len(info_bits: usize) -> usize {
    2 * (info_bits + TAIL)
}

/// Rate-1/2 encode, terminated with TAIL zero bits (trellis ends in state 0).
pub fn encode(bits: &[u8]) -> Vec<u8> {
    let mut state = 0u8;
    let mut out = Vec::with_capacity(coded_len(bits.len()));
    for &b in bits.iter().chain(std::iter::repeat_n(&0u8, TAIL)) {
        let reg = ((b & 1) << 6) | state;
        out.push(parity(reg & G0));
        out.push(parity(reg & G1));
        state = reg >> 1;
    }
    out
}

pub fn punctured_len(n: usize, rate: CodeRate) -> usize {
    let pat = rate.pattern();
    let ones = pat.iter().filter(|&&b| b == 1).count();
    (n / pat.len()) * ones + pat[..n % pat.len()].iter().filter(|&&b| b == 1).count()
}

pub fn puncture(coded: &[u8], rate: CodeRate) -> Vec<u8> {
    let pat = rate.pattern();
    coded
        .iter()
        .enumerate()
        .filter(|(i, _)| pat[i % pat.len()] == 1)
        .map(|(_, &b)| b)
        .collect()
}

/// Re-insert erasures (0.0) at punctured positions; `n` = mother-code length.
pub fn depuncture(soft: &[f32], n: usize, rate: CodeRate) -> Vec<f32> {
    let pat = rate.pattern();
    let mut it = soft.iter();
    (0..n)
        .map(|i| {
            if pat[i % pat.len()] == 1 {
                *it.next().unwrap_or(&0.0)
            } else {
                0.0
            }
        })
        .collect()
}

/// Soft-decision Viterbi for a terminated rate-1/2 stream of `coded_len(info_bits)` values.
pub fn viterbi(soft: &[f32], info_bits: usize) -> Vec<u8> {
    let steps = info_bits + TAIL;
    assert!(soft.len() >= 2 * steps, "viterbi: soft input too short");

    let mut outs = [[0u8; 2]; 128];
    for reg in 0..128u8 {
        outs[reg as usize] = [parity(reg & G0), parity(reg & G1)];
    }

    const NEG: f32 = -1.0e30;
    let mut metric = [NEG; 64];
    metric[0] = 0.0;
    let mut decisions: Vec<u64> = Vec::with_capacity(steps);

    for t in 0..steps {
        let (s0, s1) = (soft[2 * t], soft[2 * t + 1]);
        let mut next = [NEG; 64];
        let mut dec = 0u64;
        for (ns, slot) in next.iter_mut().enumerate() {
            // ns = (b << 5) | (ps >> 1); predecessors ps = ((ns & 31) << 1) | x
            let b = ns >> 5;
            let base = (ns & 31) << 1;
            let mut best = NEG;
            for x in 0..2usize {
                let ps = base | x;
                let o = outs[(b << 6) | ps];
                let m0 = if o[0] == 0 { s0 } else { -s0 };
                let m1 = if o[1] == 0 { s1 } else { -s1 };
                let m = metric[ps] + m0 + m1;
                if x == 0 {
                    best = m;
                } else if m > best {
                    best = m;
                    dec |= 1 << ns;
                }
            }
            *slot = best;
        }
        let mx = next.iter().copied().fold(NEG, f32::max);
        for m in next.iter_mut() {
            *m -= mx;
        }
        metric = next;
        decisions.push(dec);
    }

    let mut state = 0usize;
    let mut bits = vec![0u8; steps];
    for t in (0..steps).rev() {
        bits[t] = (state >> 5) as u8;
        let x = ((decisions[t] >> state) & 1) as usize;
        state = ((state & 31) << 1) | x;
    }
    bits.truncate(info_bits);
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    fn roundtrip(rate: CodeRate, sigma: f64, n: usize, seed: u64) -> usize {
        let mut r = Rng::new(seed);
        let bits: Vec<u8> = (0..n).map(|_| (r.next_u64() & 1) as u8).collect();
        let tx = puncture(&encode(&bits), rate);
        let soft: Vec<f32> = tx
            .iter()
            .map(|&b| ((1.0 - 2.0 * b as f64) + sigma * r.gauss()) as f32)
            .collect();
        let full = depuncture(&soft, coded_len(n), rate);
        let dec = viterbi(&full, n);
        bits.iter().zip(&dec).filter(|(a, b)| a != b).count()
    }

    #[test]
    fn noiseless_all_rates() {
        for rate in [CodeRate::R1_2, CodeRate::R2_3, CodeRate::R3_4] {
            assert_eq!(roundtrip(rate, 0.0, 1000, 1), 0, "{rate:?}");
        }
    }

    #[test]
    fn corrects_noise() {
        // Eb/N0 well above the code's threshold: expect zero errors.
        assert_eq!(roundtrip(CodeRate::R1_2, 0.7, 4000, 2), 0);
        assert_eq!(roundtrip(CodeRate::R3_4, 0.5, 4000, 3), 0);
    }

    #[test]
    fn punctured_len_matches() {
        for rate in [CodeRate::R1_2, CodeRate::R2_3, CodeRate::R3_4] {
            for n in [0usize, 1, 5, 12, 13, 100] {
                let coded = vec![0u8; n];
                assert_eq!(puncture(&coded, rate).len(), punctured_len(n, rate));
            }
        }
    }
}
