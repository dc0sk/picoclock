//! The HTTPS key and certificate chain as stored in their own flash sector (Pico 2 W). Kept separate from the settings
//! record, which is too small for a chain. A record carries a magic, a length and a CRC32, so a blank or torn sector reads
//! as "no certificate yet" and the device makes a new one.
//!
//! Layout: `PCT1 | len u16 | crc32 | flags u8 | key[32] | count u8 | (cert len u16) * count | certificate DER bytes`.

use heapless::Vec;

use crate::config::crc32;
use crate::x509::{self, MAX_CHAIN, MAX_CHAIN_CERTS};

const MAGIC: [u8; 4] = *b"PCT1";
const HEADER: usize = 4 + 2 + 4;
const FLAG_UPLOADED: u8 = 1;
/// Largest encoded record (the sector is 4 KiB).
pub const MAX_RECORD: usize = 4096;

/// The private key (P-256 scalar) and the certificate chain, leaf first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Material {
    pub key: [u8; 32],
    /// The user uploaded it (rather than the device having made it).
    pub uploaded: bool,
    pub chain: Vec<u8, MAX_CHAIN>,
    pub lens: Vec<u16, MAX_CHAIN_CERTS>,
}

impl Material {
    /// A single self-signed certificate.
    pub fn single(key: [u8; 32], cert: &[u8], uploaded: bool) -> Option<Self> {
        let mut chain = Vec::new();
        chain.extend_from_slice(cert).ok()?;
        let mut lens = Vec::new();
        lens.push(u16::try_from(cert.len()).ok()?).ok()?;
        Some(Self { key, uploaded, chain, lens })
    }

    /// The certificates as separate DER slices, leaf first.
    pub fn certs(&self) -> impl Iterator<Item = &[u8]> {
        let mut at = 0usize;
        self.lens.iter().map(move |&l| {
            let c = &self.chain[at..at + l as usize];
            at += l as usize;
            c
        })
    }

    pub fn leaf(&self) -> &[u8] {
        self.certs().next().unwrap_or(&[])
    }

    /// Internally consistent: the lengths add up and the leaf is a P-256 certificate for this key.
    fn consistent(&self) -> bool {
        let total: usize = self.lens.iter().map(|&l| l as usize).sum();
        !self.lens.is_empty() && total == self.chain.len() && self.lens.iter().all(|&l| l > 0) && x509::key_matches(self.leaf(), &self.key)
    }

    /// Serialises into `buf`; returns the number of bytes.
    pub fn encode(&self, buf: &mut [u8]) -> Option<usize> {
        if !self.consistent() {
            return None;
        }
        let body_len = 1 + 32 + 1 + 2 * self.lens.len() + self.chain.len();
        if buf.len() < HEADER + body_len || HEADER + body_len > MAX_RECORD {
            return None;
        }
        let body = &mut buf[HEADER..HEADER + body_len];
        body[0] = if self.uploaded { FLAG_UPLOADED } else { 0 };
        body[1..33].copy_from_slice(&self.key);
        body[33] = self.lens.len() as u8;
        let mut at = 34;
        for l in &self.lens {
            body[at..at + 2].copy_from_slice(&l.to_le_bytes());
            at += 2;
        }
        body[at..].copy_from_slice(&self.chain);
        let crc = crc32(&buf[HEADER..HEADER + body_len]);
        buf[..4].copy_from_slice(&MAGIC);
        buf[4..6].copy_from_slice(&(body_len as u16).to_le_bytes());
        buf[6..10].copy_from_slice(&crc.to_le_bytes());
        Some(HEADER + body_len)
    }

    /// Parses a stored record. `None` for blank flash, a wrong magic, length or checksum, or inconsistent contents.
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < HEADER || data[..4] != MAGIC {
            return None;
        }
        let len = u16::from_le_bytes([data[4], data[5]]) as usize;
        let crc = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);
        let body = data.get(HEADER..HEADER + len)?;
        if crc32(body) != crc || body.len() < 34 {
            return None;
        }
        let count = body[33] as usize;
        if count == 0 || count > MAX_CHAIN_CERTS || body.len() < 34 + 2 * count {
            return None;
        }
        let mut lens = Vec::new();
        for i in 0..count {
            lens.push(u16::from_le_bytes([body[34 + 2 * i], body[35 + 2 * i]])).ok()?;
        }
        let mut chain = Vec::new();
        chain.extend_from_slice(&body[34 + 2 * count..]).ok()?;
        let mut key = [0u8; 32];
        key.copy_from_slice(&body[1..33]);
        let m = Self { key, uploaded: body[0] & FLAG_UPLOADED != 0, chain, lens };
        m.consistent().then_some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_000_000;

    fn material(uploaded: bool) -> Material {
        let key = [0x31; 32];
        let cert = x509::generate_self_signed(key, [1, 2, 3, 4, 5, 6, 7, 8], &["picoclock-d6", "picoclock-d6.local"], NOW).unwrap();
        Material::single(key, &cert, uploaded).unwrap()
    }

    // VERIFIES: REQ-WEB-08
    #[test]
    fn a_record_round_trips() {
        for uploaded in [false, true] {
            let m = material(uploaded);
            let mut buf = [0xFFu8; MAX_RECORD];
            let n = m.encode(&mut buf).unwrap();
            assert!(n < 500, "{n}");
            assert_eq!(Material::decode(&buf), Some(m.clone()), "decoding the full sector, erased tail included");
            assert_eq!(Material::decode(&buf[..n]), Some(m));
        }
    }

    // VERIFIES: REQ-WEB-08
    #[test]
    fn a_chain_keeps_its_order() {
        let m = material(true);
        let leaf = m.leaf().to_vec();
        let mut chain = m.chain.clone();
        chain.extend_from_slice(&[0x30, 0x03, 0x02, 0x01, 0x05]).unwrap();
        let mut lens = m.lens.clone();
        lens.push(5).unwrap();
        let two = Material { chain, lens, ..m };
        let mut buf = [0xFFu8; MAX_RECORD];
        two.encode(&mut buf).unwrap();
        let back = Material::decode(&buf).unwrap();
        let certs: std::vec::Vec<_> = back.certs().collect();
        assert_eq!(certs.len(), 2);
        assert_eq!(certs[0], &leaf[..]);
        assert_eq!(certs[1], &[0x30, 0x03, 0x02, 0x01, 0x05]);
    }

    // VERIFIES: REQ-WEB-08
    #[test]
    fn blank_torn_corrupt_and_inconsistent_records_read_as_no_certificate() {
        assert_eq!(Material::decode(&[0xFF; MAX_RECORD]), None, "erased flash");
        assert_eq!(Material::decode(&[0; MAX_RECORD]), None);
        assert_eq!(Material::decode(&[]), None);
        let m = material(false);
        let mut buf = [0xFFu8; MAX_RECORD];
        let n = m.encode(&mut buf).unwrap();
        // every single flipped bit anywhere in the record is detected (the CRC covers the body, magic and length are checked)
        for i in 0..n {
            for bit in 0..8 {
                let mut bad = buf;
                bad[i] ^= 1 << bit;
                assert_eq!(Material::decode(&bad), None, "bit {bit} of byte {i}");
            }
        }
        // a truncated write
        for cut in 0..n {
            assert_eq!(Material::decode(&buf[..cut]), None, "cut at {cut}");
        }
        // a key that does not belong to the certificate, even with a valid checksum
        let mut other = m.clone();
        other.key = [0x32; 32];
        assert_eq!(other.encode(&mut [0u8; MAX_RECORD]), None, "encode refuses a mismatched pair");
        let mut forged = buf;
        forged[10 + 1..10 + 33].copy_from_slice(&[0x32; 32]);
        let crc = crc32(&forged[10..n]);
        forged[6..10].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(Material::decode(&forged), None, "decode refuses a mismatched pair with a valid CRC");
        // lengths that do not add up
        let mut lens_off = m.clone();
        lens_off.lens[0] -= 1;
        assert_eq!(lens_off.encode(&mut [0u8; MAX_RECORD]), None);
        assert_eq!(Material::single([0x31; 32], b"not a certificate", false).unwrap().encode(&mut [0u8; MAX_RECORD]), None);
        // a buffer that is too small
        assert_eq!(m.encode(&mut [0u8; 20]), None);
    }
}
