//! Systematic Reed–Solomon erasure code over GF(2^8) with a Cauchy matrix.
//!
//! `k` data shards, parity shard `i` has shard index `k + i`; any `k` distinct
//! shards (data or parity) reconstruct the data. Indices must stay < 256.

use std::sync::OnceLock;

struct Gf {
    exp: [u8; 512],
    log: [u8; 256],
}

fn gf() -> &'static Gf {
    static T: OnceLock<Gf> = OnceLock::new();
    T.get_or_init(|| {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x: u16 = 1;
        for i in 0..255 {
            exp[i] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11d;
            }
        }
        for i in 255..512 {
            exp[i] = exp[i - 255];
        }
        Gf { exp, log }
    })
}

fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        let t = gf();
        t.exp[t.log[a as usize] as usize + t.log[b as usize] as usize]
    }
}

fn inv(a: u8) -> u8 {
    assert!(a != 0, "GF inverse of 0");
    let t = gf();
    t.exp[255 - t.log[a as usize] as usize]
}

/// dst ^= c · src
fn mul_add(dst: &mut [u8], src: &[u8], c: u8) {
    if c == 0 {
        return;
    }
    let mut table = [0u8; 256];
    for (x, v) in table.iter_mut().enumerate() {
        *v = mul(c, x as u8);
    }
    for (d, &s) in dst.iter_mut().zip(src) {
        *d ^= table[s as usize];
    }
}

/// Coefficient of data shard `j` in the shard with global index `idx` (≥ k for parity).
fn coeff(k: usize, idx: usize, j: usize) -> u8 {
    if idx < k {
        (idx == j) as u8
    } else {
        inv((idx as u8) ^ (j as u8))
    }
}

/// Largest number of parity shards available for `k` data shards.
pub fn max_parity(k: usize) -> usize {
    256 - k
}

/// Parity shard with global index `idx` (k ≤ idx < 256). All data shards equal length.
pub fn parity(data: &[&[u8]], idx: usize) -> Vec<u8> {
    let k = data.len();
    assert!(idx >= k && idx < 256 && k > 0);
    let mut out = vec![0u8; data[0].len()];
    for (j, d) in data.iter().enumerate() {
        mul_add(&mut out, d, coeff(k, idx, j));
    }
    out
}

/// Rebuild the `k` data shards from any `k` distinct (index, shard) pairs.
pub fn reconstruct(k: usize, shards: &[(usize, &[u8])]) -> Option<Vec<Vec<u8>>> {
    if shards.len() < k || k == 0 {
        return None;
    }
    let shards = &shards[..k];
    let size = shards[0].1.len();
    // Fast path: all data shards present.
    if shards.iter().all(|&(i, _)| i < k) {
        let mut out = vec![Vec::new(); k];
        for &(i, s) in shards {
            out[i] = s.to_vec();
        }
        return out
            .iter()
            .all(|s| !s.is_empty() || size == 0)
            .then_some(out);
    }
    // Gauss–Jordan inversion of the k×k coefficient matrix.
    let mut m: Vec<Vec<u8>> = shards
        .iter()
        .map(|&(i, _)| (0..k).map(|j| coeff(k, i, j)).collect())
        .collect();
    let mut invm: Vec<Vec<u8>> = (0..k)
        .map(|r| (0..k).map(|c| (r == c) as u8).collect())
        .collect();
    for col in 0..k {
        let piv = (col..k).find(|&r| m[r][col] != 0)?;
        m.swap(col, piv);
        invm.swap(col, piv);
        let f = inv(m[col][col]);
        for c in 0..k {
            m[col][c] = mul(m[col][c], f);
            invm[col][c] = mul(invm[col][c], f);
        }
        for r in 0..k {
            if r != col && m[r][col] != 0 {
                let f = m[r][col];
                let (mrow, irow) = (m[col].clone(), invm[col].clone());
                for c in 0..k {
                    m[r][c] ^= mul(f, mrow[c]);
                    invm[r][c] ^= mul(f, irow[c]);
                }
            }
        }
    }
    // data_j = Σ_r invm[j][r] · shard_r
    let mut out = vec![vec![0u8; size]; k];
    for (j, o) in out.iter_mut().enumerate() {
        for (r, &(_, s)) in shards.iter().enumerate() {
            mul_add(o, s, invm[j][r]);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    #[test]
    fn recovers_from_any_k_shards() {
        let mut r = Rng::new(5);
        for &(k, m) in &[(1usize, 1usize), (3, 2), (10, 4), (128, 32)] {
            let data: Vec<Vec<u8>> = (0..k)
                .map(|_| (0..37).map(|_| r.next_u64() as u8).collect())
                .collect();
            let refs: Vec<&[u8]> = data.iter().map(|d| d.as_slice()).collect();
            let par: Vec<Vec<u8>> = (k..k + m).map(|i| parity(&refs, i)).collect();
            // Drop up to m random shards, reconstruct from the rest.
            let mut all: Vec<(usize, &[u8])> = (0..k)
                .map(|i| (i, data[i].as_slice()))
                .chain((0..m).map(|i| (k + i, par[i].as_slice())))
                .collect();
            for _ in 0..m {
                let i = r.below(all.len());
                all.remove(i);
            }
            // shuffle order
            for i in (1..all.len()).rev() {
                all.swap(i, r.below(i + 1));
            }
            assert_eq!(reconstruct(k, &all).unwrap(), data, "k={k} m={m}");
        }
    }
}
