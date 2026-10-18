//! Conditioning for a weak random source (the RP2040's ring oscillator). The raw bits are only jitter and are correlated
//! when read back to back, so they are never used directly: every 32 output bytes are a SHA-256 hash over 64 raw bytes
//! (two raw bits for each output bit), the time of the reading, a counter and the state left by the previous block.
//! The chip's own true random number generator (RP2350) does not go through here.

use sha2::{Digest, Sha256};

/// Raw bytes consumed for each 32-byte output block.
pub const RAW_PER_BLOCK: usize = 64;

pub struct Pool {
    state: [u8; 32],
    counter: u64,
}

impl Pool {
    pub const fn new() -> Self {
        Self { state: [0; 32], counter: 0 }
    }

    /// Mixes `raw` and `extra` (a clock reading, for instance) into the pool and returns 32 output bytes. The output is
    /// a second hash of the new state, so it does not reveal the state the next block builds on.
    pub fn block(&mut self, raw: &[u8; RAW_PER_BLOCK], extra: u64) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"picoclock-rng-state");
        h.update(self.state);
        h.update(self.counter.to_le_bytes());
        h.update(extra.to_le_bytes());
        h.update(raw);
        self.state = h.finalize().into();
        self.counter = self.counter.wrapping_add(1);
        let mut o = Sha256::new();
        o.update(b"picoclock-rng-out");
        o.update(self.state);
        o.finalize().into()
    }
}

impl Default for Pool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // VERIFIES: REQ-RNG-01
    #[test]
    fn output_depends_on_every_input_and_never_repeats() {
        let raw = [0x5Au8; RAW_PER_BLOCK];
        let a = Pool::new().block(&raw, 1);
        // a different raw byte, a different time, each change the block
        let mut raw2 = raw;
        raw2[63] ^= 1;
        assert_ne!(a, Pool::new().block(&raw2, 1));
        assert_ne!(a, Pool::new().block(&raw, 2));
        // the same inputs twice in a row (a stuck source) still give different blocks
        let mut p = Pool::new();
        let (x, y, z) = (p.block(&raw, 1), p.block(&raw, 1), p.block(&raw, 1));
        assert!(x != y && y != z && x != z);
        // all-zero input is not mapped to zeros
        assert_ne!(Pool::new().block(&[0; RAW_PER_BLOCK], 0), [0; 32]);
    }

    // VERIFIES: REQ-RNG-01
    #[test]
    fn a_biased_source_comes_out_balanced() {
        // raw bytes with only one bit in eight ever set (a very poor source): the hash output is still balanced
        let mut p = Pool::new();
        let mut ones = 0u32;
        let blocks = 2000;
        for i in 0..blocks {
            let mut raw = [0u8; RAW_PER_BLOCK];
            for (j, b) in raw.iter_mut().enumerate() {
                *b = 1 << ((i + j * 7) % 8) & (0x81u8.rotate_left((i % 8) as u32));
            }
            ones += p.block(&raw, i as u64).iter().map(|b| b.count_ones()).sum::<u32>();
        }
        let total = blocks as f64 * 256.0;
        let frac = ones as f64 / total;
        assert!((0.49..0.51).contains(&frac), "{frac}");
    }
}
