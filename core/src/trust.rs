//! The trust record (ADR-0006, point 10): which of the built-in signing keys were revoked by a release the user installed. It
//! lives in its own pair of flash sectors (`slots`), is not touched by a factory reset (a revoked key must stay revoked), and is
//! read by the install check. Layout: `PTR1 | revoked u8 | 3 zero bytes | crc32`.

use crate::config::crc32;

const MAGIC: [u8; 4] = *b"PTR1";
pub const RECORD_LEN: usize = 12;

/// Bit `n` set = key `n + 1` is revoked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trust {
    pub revoked: u8,
}

impl Trust {
    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let mut b = [0u8; RECORD_LEN];
        b[..4].copy_from_slice(&MAGIC);
        b[4] = self.revoked;
        let crc = crc32(&b[..8]);
        b[8..].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// `None` for a blank, torn or damaged sector.
    pub fn decode(data: &[u8]) -> Option<Trust> {
        let b = data.get(..RECORD_LEN)?;
        if b[..4] != MAGIC || b[5..8] != [0, 0, 0] || u32::from_le_bytes([b[8], b[9], b[10], b[11]]) != crc32(&b[..8]) {
            return None;
        }
        Some(Trust { revoked: b[4] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // VERIFIES: REQ-UPD-11
    #[test]
    fn the_trust_record_round_trips_and_damage_reads_as_none() {
        for revoked in [0u8, 1, 2, 3] {
            assert_eq!(Trust::decode(&Trust { revoked }.encode()), Some(Trust { revoked }));
        }
        assert_eq!(Trust::decode(&[0xFF; 4096]), None, "a blank sector");
        let good = Trust { revoked: 1 }.encode();
        for i in 0..RECORD_LEN {
            let mut bad = good;
            bad[i] ^= 0x04;
            assert_eq!(Trust::decode(&bad), None, "byte {i}");
        }
        assert_eq!(Trust::decode(&good[..8]), None, "too short");
    }
}
