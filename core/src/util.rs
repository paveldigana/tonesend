//! Small helpers: deterministic RNG, CRCs, bit packing, interleaving.

use std::f64::consts::PI;

/// xorshift64* — deterministic, dependency-free PRNG (not cryptographic).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        let s = seed ^ 0x9E37_79B9_7F4A_7C15;
        let mut r = Rng(if s == 0 { 1 } else { s });
        r.next_u64();
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Standard normal (Box–Muller).
    pub fn gauss(&mut self) -> f64 {
        let u1 = self.uniform().max(f64::MIN_POSITIVE);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn fill_bytes(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = (self.next_u64() >> 56) as u8;
        }
    }
}

/// CRC-32 (IEEE 802.3, reflected).
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// CRC-16/CCITT-FALSE.
pub fn crc16(data: &[u8]) -> u16 {
    let mut c = 0xFFFFu16;
    for &b in data {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x1021
            } else {
                c << 1
            };
        }
    }
    c
}

/// MSB-first unpacking into 0/1 values.
pub fn bytes_to_bits(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1))
        .collect()
}

/// MSB-first packing; a trailing partial byte is zero-padded.
pub fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
    bits.chunks(8)
        .map(|c| {
            let v = c.iter().fold(0u8, |a, &b| (a << 1) | (b & 1));
            v << (8 - c.len())
        })
        .collect()
}

/// Pseudo-random permutation of 0..n (Fisher–Yates), fixed by `seed` and `n`.
pub fn permutation(n: usize, seed: u64) -> Vec<usize> {
    let mut p: Vec<usize> = (0..n).collect();
    let mut r = Rng::new(seed ^ (n as u64).wrapping_mul(0x9E37_79B9));
    for i in (1..n).rev() {
        let j = r.below(i + 1);
        p.swap(i, j);
    }
    p
}

pub fn interleave<T: Copy>(x: &[T], seed: u64) -> Vec<T> {
    permutation(x.len(), seed).iter().map(|&i| x[i]).collect()
}

pub fn deinterleave<T: Copy + Default>(y: &[T], seed: u64) -> Vec<T> {
    let mut x = vec![T::default(); y.len()];
    for (i, &pi) in permutation(y.len(), seed).iter().enumerate() {
        x[pi] = y[i];
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }

    #[test]
    fn bits_roundtrip() {
        let b = [0xA5u8, 0x00, 0xFF, 0x3C];
        assert_eq!(bits_to_bytes(&bytes_to_bits(&b)), b);
    }

    #[test]
    fn interleave_roundtrip() {
        let x: Vec<u32> = (0..1000).collect();
        let y = interleave(&x, 7);
        assert_ne!(x, y);
        assert_eq!(deinterleave(&y, 7), x);
    }
}
