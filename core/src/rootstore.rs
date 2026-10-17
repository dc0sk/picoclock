//! Trust anchors the user adds for NTS (certificate authorities, or a server's own self-signed certificate), kept in their
//! own flash sector on both chips. Up to [`MAX_ROOTS`] certificates in [`MAX_BYTES`] bytes. The record carries a magic, a
//! length and a CRC32, so a blank or torn sector reads as "no roots".
//!
//! Layout: `PCR1 | len u16 | crc32 | count u8 | (cert len u16) * count | certificate DER bytes`.

use heapless::{String, Vec};

use crate::config::crc32;
use crate::x509::{self, CaInfo, KeyKind, MAX_CERT, X509Error};

pub const MAX_ROOTS: usize = 4;
pub const MAX_BYTES: usize = 3584;
/// Largest encoded record (the sector is 4 KiB).
pub const MAX_RECORD: usize = 4096;
const MAGIC: [u8; 4] = *b"PCR1";
const HEADER: usize = 4 + 2 + 4;

#[derive(Debug, PartialEq, Eq)]
pub enum RootError {
    /// No PEM certificate in the text.
    NoCertificate,
    /// Not a certificate we can read.
    Invalid(X509Error),
    /// It carries an RSA key, which this build cannot verify (the Pico W has no allocator for it).
    RsaNotSupported,
    /// Already built in, or already added.
    AlreadyTrusted,
    /// Past its end date.
    Expired,
    /// Four roots or 3.5 KB are used already.
    Full,
}

impl RootError {
    pub fn message(&self) -> &'static str {
        match self {
            RootError::NoCertificate => "no certificate found; paste the PEM text (-----BEGIN CERTIFICATE-----)",
            RootError::Invalid(_) => "that is not a certificate this clock can read (X.509 v3, at most 1536 bytes)",
            RootError::RsaNotSupported => "RSA certificates are only supported on the Pico 2 W",
            RootError::AlreadyTrusted => "that certificate is already trusted (built in or added before)",
            RootError::Expired => "that certificate has expired",
            RootError::Full => "no room: remove a root first (at most 4 roots, 3.5 KB)",
        }
    }
}

/// The user's trust anchors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roots {
    chain: Vec<u8, MAX_BYTES>,
    lens: Vec<u16, MAX_ROOTS>,
}

impl Roots {
    pub const fn new() -> Self {
        Self { chain: Vec::new(), lens: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.lens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lens.is_empty()
    }

    /// The certificates (DER), in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        let mut at = 0usize;
        self.lens.iter().map(move |&l| {
            let c = &self.chain[at..at + l as usize];
            at += l as usize;
            c
        })
    }

    fn contains(&self, der: &[u8]) -> bool {
        crate::nts_ke::BUILT_IN_ROOTS.contains(&der) || self.iter().any(|c| c == der)
    }

    /// Adds every certificate of the PEM `text`, or none if any of them is refused. `now` (unix seconds) is used to refuse
    /// an expired certificate; without a time that check is skipped. Returns what was added.
    pub fn add_pem(&mut self, text: &str, now: Option<i64>) -> Result<Vec<CaInfo, MAX_ROOTS>, RootError> {
        let mut chain: Vec<u8, { x509::MAX_CHAIN }> = Vec::new();
        let mut lens: Vec<u16, { x509::MAX_CHAIN_CERTS }> = Vec::new();
        x509::parse_pem_chain(text, &mut chain, &mut lens).map_err(|e| match e {
            X509Error::Unsupported if !text.contains("-----BEGIN CERTIFICATE-----") => RootError::NoCertificate,
            e => RootError::Invalid(e),
        })?;
        let mut staged = self.clone();
        let mut added = Vec::new();
        let mut at = 0usize;
        for &len in lens.iter() {
            let der = &chain[at..at + len as usize];
            at += len as usize;
            let info = staged.add_der(der, now)?;
            let _ = added.push(info);
        }
        *self = staged;
        Ok(added)
    }

    fn add_der(&mut self, der: &[u8], now: Option<i64>) -> Result<CaInfo, RootError> {
        let info = x509::describe_ca(der).map_err(RootError::Invalid)?;
        if info.key == KeyKind::Rsa && !cfg!(feature = "rsa") {
            return Err(RootError::RsaNotSupported);
        }
        if self.contains(der) {
            return Err(RootError::AlreadyTrusted);
        }
        if now.is_some_and(|t| t > info.not_after) {
            return Err(RootError::Expired);
        }
        if self.lens.is_full() || self.chain.len() + der.len() > MAX_BYTES || der.len() > MAX_CERT {
            return Err(RootError::Full);
        }
        let _ = self.chain.extend_from_slice(der);
        let _ = self.lens.push(der.len() as u16);
        Ok(info)
    }

    /// Removes root number `index` (0-based). False if there is none.
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.lens.len() {
            return false;
        }
        let start: usize = self.lens[..index].iter().map(|&l| l as usize).sum();
        let len = self.lens[index] as usize;
        let tail: Vec<u8, MAX_BYTES> = Vec::from_slice(&self.chain[start + len..]).unwrap_or_default();
        self.chain.truncate(start);
        let _ = self.chain.extend_from_slice(&tail);
        self.lens.remove(index);
        true
    }

    /// A short description of root `index` for lists: name, validity, fingerprint.
    pub fn describe(&self, index: usize) -> Option<CaInfo> {
        x509::describe_ca(self.iter().nth(index)?).ok()
    }

    pub fn encode(&self, buf: &mut [u8]) -> Option<usize> {
        let body_len = 1 + 2 * self.lens.len() + self.chain.len();
        if buf.len() < HEADER + body_len || HEADER + body_len > MAX_RECORD {
            return None;
        }
        let body = &mut buf[HEADER..HEADER + body_len];
        body[0] = self.lens.len() as u8;
        let mut at = 1;
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

    /// Parses a stored record. `None` for blank flash, a wrong magic, length or checksum, or contents that do not add up.
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < HEADER || data[..4] != MAGIC {
            return None;
        }
        let len = u16::from_le_bytes([data[4], data[5]]) as usize;
        let crc = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);
        let body = data.get(HEADER..HEADER + len)?;
        if crc32(body) != crc || body.is_empty() {
            return None;
        }
        let count = body[0] as usize;
        if count > MAX_ROOTS || body.len() < 1 + 2 * count {
            return None;
        }
        let mut lens = Vec::new();
        for i in 0..count {
            lens.push(u16::from_le_bytes([body[1 + 2 * i], body[2 + 2 * i]])).ok()?;
        }
        let mut chain = Vec::new();
        chain.extend_from_slice(&body[1 + 2 * count..]).ok()?;
        let total: usize = lens.iter().map(|&l| l as usize).sum();
        let roots = Self { chain, lens };
        // the lengths add up and every entry is still a certificate we can describe
        (total == roots.chain.len() && roots.iter().all(|c| x509::describe_ca(c).is_ok())).then_some(roots)
    }
}

/// Short text for a listing: `1  Test CA  valid until 2036-01-31  AB:CD:...`
pub fn summary(n: usize, info: &CaInfo, now: Option<i64>) -> String<160> {
    use core::fmt::Write;
    let d = crate::time::unix_to_datetime(info.not_after);
    let mut s = String::new();
    let _ = write!(
        s,
        "{n}  {}  valid until {:04}-{:02}-{:02}{}{}  {}",
        if info.name.is_empty() { "(no name)" } else { info.name.as_str() },
        d.year,
        d.month,
        d.day,
        if now.is_some_and(|t| t > info.not_after) { " (EXPIRED)" } else { "" },
        if info.self_signed { "" } else { ", not self-signed" },
        x509::fingerprint_hex(&info.fingerprint)
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_000_000;

    fn pem(der: &[u8]) -> std::string::String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = std::string::String::new();
        for chunk in der.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |a, (i, &b)| a | (b as u32) << (16 - 8 * i));
            for i in 0..4 {
                s.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        let lines: std::vec::Vec<_> = s.as_bytes().chunks(64).map(|c| std::string::String::from_utf8_lossy(c).into_owned()).collect();
        format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", lines.join("\n"))
    }

    /// A self-signed P-256 certificate for `name`, as a stand-in for a pinned server certificate or a small private CA.
    fn cert(seed: u8, name: &str) -> std::vec::Vec<u8> {
        x509::generate_self_signed([seed; 32], [seed; 8], &[name], NOW).unwrap().to_vec()
    }

    // VERIFIES: REQ-NTS-09
    #[test]
    fn adding_describes_and_refuses_duplicates_and_bad_input() {
        let mut roots = Roots::new();
        let a = cert(1, "alpha.example");
        let info = roots.add_pem(&pem(&a), Some(NOW)).unwrap();
        assert_eq!(info.len(), 1);
        assert_eq!((info[0].name.as_str(), info[0].key, info[0].self_signed), ("alpha.example", KeyKind::EcP256, true));
        assert_eq!(roots.len(), 1);
        assert_eq!(roots.add_pem(&pem(&a), Some(NOW)), Err(RootError::AlreadyTrusted), "added before");
        assert_eq!(roots.add_pem(&pem(crate::nts_ke::BUILT_IN_ROOTS[0]), Some(NOW)), Err(RootError::AlreadyTrusted), "built in");
        assert_eq!(roots.add_pem("hello", Some(NOW)), Err(RootError::NoCertificate));
        assert_eq!(roots.add_pem("", Some(NOW)), Err(RootError::NoCertificate));
        assert!(matches!(roots.add_pem("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n", Some(NOW)), Err(RootError::Invalid(_))));
        assert!(matches!(roots.add_pem("-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n", Some(NOW)), Err(RootError::Invalid(_))));
        // all or nothing: a good certificate followed by a bad one adds neither
        let both = format!("{}-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n", pem(&cert(2, "beta.example")));
        assert!(roots.add_pem(&both, Some(NOW)).is_err());
        assert_eq!(roots.len(), 1, "nothing of a refused paste is kept");
        let dup = format!("{}{}", pem(&cert(3, "gamma.example")), pem(&a));
        assert_eq!(roots.add_pem(&dup, Some(NOW)), Err(RootError::AlreadyTrusted));
        assert_eq!(roots.len(), 1);
        // two in one paste
        let two = format!("{}{}", pem(&cert(4, "d.example")), pem(&cert(5, "e.example")));
        assert_eq!(roots.add_pem(&two, Some(NOW)).unwrap().len(), 2);
        assert_eq!(roots.len(), 3);
        assert_eq!(roots.describe(1).unwrap().name.as_str(), "d.example");
        assert!(roots.describe(3).is_none());
    }

    // VERIFIES: REQ-NTS-09
    #[test]
    fn expired_certificates_and_a_full_store_are_refused() {
        let mut roots = Roots::new();
        let c = cert(1, "old.example");
        assert_eq!(roots.add_pem(&pem(&c), Some(NOW + 3651 * 86_400)), Err(RootError::Expired));
        assert!(roots.add_pem(&pem(&c), None).is_ok(), "without a clock the date check is skipped");
        for seed in 2..=4 {
            roots.add_pem(&pem(&cert(seed, "x.example")), Some(NOW)).unwrap();
        }
        assert_eq!(roots.len(), MAX_ROOTS);
        assert_eq!(roots.add_pem(&pem(&cert(9, "y.example")), Some(NOW)), Err(RootError::Full));
        assert_eq!(roots.len(), MAX_ROOTS);
        assert!(roots.remove(0));
        assert!(roots.add_pem(&pem(&cert(9, "y.example")), Some(NOW)).is_ok(), "room again after a removal");
    }

    // VERIFIES: REQ-NTS-09 REQ-NTS-08
    #[test]
    fn an_rsa_root_needs_a_build_that_can_verify_rsa() {
        let x1 = include_bytes!("../roots/isrg-root-x1.der");
        let mut roots = Roots::new();
        let r = roots.add_pem(&pem(x1), Some(NOW));
        if cfg!(feature = "rsa") {
            assert_eq!(r, Err(RootError::AlreadyTrusted), "built in on the Pico 2 W");
            assert!(crate::nts_ke::BUILT_IN_ROOTS.contains(&&x1[..]));
        } else {
            assert_eq!(r, Err(RootError::RsaNotSupported), "refused where it could never verify anything");
            assert!(!crate::nts_ke::BUILT_IN_ROOTS.contains(&&x1[..]));
        }
        assert!(roots.is_empty());
    }

    // VERIFIES: REQ-NTS-09
    #[test]
    fn removal_keeps_the_others_intact_and_in_order() {
        let mut roots = Roots::new();
        let certs: std::vec::Vec<_> = (1..=4).map(|i| cert(i, &format!("n{i}.example"))).collect();
        for c in &certs {
            roots.add_pem(&pem(c), Some(NOW)).unwrap();
        }
        assert!(!roots.remove(4), "no such root");
        assert!(roots.remove(1));
        assert_eq!(roots.iter().collect::<std::vec::Vec<_>>(), [&certs[0][..], &certs[2][..], &certs[3][..]]);
        assert!(roots.remove(2));
        assert!(roots.remove(0));
        assert_eq!(roots.iter().collect::<std::vec::Vec<_>>(), [&certs[2][..]]);
        assert!(roots.remove(0));
        assert!(roots.is_empty());
        assert!(!roots.remove(0));
    }

    // VERIFIES: REQ-NTS-09
    #[test]
    fn a_record_round_trips_and_damage_reads_as_no_roots() {
        let mut roots = Roots::new();
        let mut buf = [0xFFu8; MAX_RECORD];
        let n = roots.encode(&mut buf).unwrap();
        assert_eq!(Roots::decode(&buf), Some(Roots::new()), "an empty store is a valid record");
        roots.add_pem(&format!("{}{}", pem(&cert(1, "a.example")), pem(&cert(2, "b.example"))), Some(NOW)).unwrap();
        let mut buf = [0xFFu8; MAX_RECORD];
        let n2 = roots.encode(&mut buf).unwrap();
        assert!(n2 > n);
        assert_eq!(Roots::decode(&buf), Some(roots.clone()), "the whole sector, erased tail included");
        assert_eq!(Roots::decode(&buf[..n2]), Some(roots));
        assert_eq!(Roots::decode(&[0xFF; MAX_RECORD]), None, "erased flash");
        assert_eq!(Roots::decode(&[0; MAX_RECORD]), None);
        for i in 0..n2 {
            for bit in 0..8 {
                let mut bad = buf;
                bad[i] ^= 1 << bit;
                assert_eq!(Roots::decode(&bad), None, "bit {bit} of byte {i}");
            }
        }
        for cut in 0..n2 {
            assert_eq!(Roots::decode(&buf[..cut]), None, "cut at {cut}");
        }
        assert_eq!(Roots::new().encode(&mut [0u8; 5]), None, "buffer too small");
    }

    // VERIFIES: REQ-NTS-09
    #[test]
    fn the_listing_line_shows_name_expiry_and_fingerprint() {
        let c = cert(1, "alpha.example");
        let info = x509::describe_ca(&c).unwrap();
        let line = summary(2, &info, Some(NOW));
        assert!(line.starts_with("2  alpha.example  valid until 2036-") && line.ends_with(x509::fingerprint_hex(&info.fingerprint).as_str()), "{line}");
        assert!(!line.contains("EXPIRED") && !line.contains("not self-signed"), "{line}");
        assert!(summary(1, &info, Some(NOW + 4000 * 86_400)).contains("(EXPIRED)"));
    }
}
