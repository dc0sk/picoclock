//! The web page's TLS certificate (ADR-0002): build a self-signed ECDSA P-256 certificate on the device, and inspect and
//! validate one the user uploads. Deliberately small and strict: one key type (P-256), one signature algorithm
//! (ecdsa-with-SHA256), DER only, fixed extensions. No I/O, no allocation, no randomness (the caller supplies the key and
//! serial bytes), so everything here is testable on the host and checked against OpenSSL (`core/tests/x509_openssl.rs`).

use heapless::{String, Vec};
use p256::SecretKey;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use sha2::{Digest, Sha256};

use crate::time::{DateTime, datetime_to_unix, unix_to_datetime};

/// Largest certificate we build or accept (the generated one is about 350 bytes; an uploaded leaf is usually 400 to 900).
pub const MAX_CERT: usize = 1536;
/// Largest uploaded chain (leaf and intermediates) in DER.
pub const MAX_CHAIN: usize = 3072;
pub const MAX_CHAIN_CERTS: usize = 4;
/// How long a generated certificate is valid.
pub const VALID_DAYS: i64 = 3650;
/// A generated certificate is valid from a day before it was made, so a client whose clock is a little behind accepts it.
const BACKDATE_SECS: i64 = 86_400;

#[derive(Debug, PartialEq, Eq)]
pub enum X509Error {
    /// The 32 random bytes are not a usable P-256 secret (zero or not below the group order): draw new ones.
    BadSecret,
    /// A host name is empty, too long or uses characters a DNS name cannot.
    BadName,
    /// The time is before 2024: the clock was not set, so a validity period would be wrong.
    BadTime,
    /// The output does not fit.
    TooLarge,
    /// Not DER we understand, or not strictly formed.
    Der,
    /// The certificate is not a P-256 key signed with ecdsa-with-SHA256, or the PEM is not what it claims.
    Unsupported,
    /// The private key is not the one in the certificate.
    KeyMismatch,
    NotYetValid,
    Expired,
}

// ---- DER writer --------------------------------------------------------------------------------------------------

type Buf = Vec<u8, MAX_CERT>;

fn tlv(out: &mut Buf, tag: u8, body: &[u8]) -> Result<(), X509Error> {
    let n = body.len();
    let header: ([u8; 4], usize) = match n {
        0..=127 => ([tag, n as u8, 0, 0], 2),
        128..=255 => ([tag, 0x81, n as u8, 0], 3),
        256..=65535 => ([tag, 0x82, (n >> 8) as u8, n as u8], 4),
        _ => return Err(X509Error::TooLarge),
    };
    out.extend_from_slice(&header.0[..header.1]).map_err(|_| X509Error::TooLarge)?;
    out.extend_from_slice(body).map_err(|_| X509Error::TooLarge)
}

/// Wraps `parts` (already DER) in a constructed element.
fn wrap(tag: u8, parts: &[&[u8]]) -> Result<Buf, X509Error> {
    let mut body = Buf::new();
    for p in parts {
        body.extend_from_slice(p).map_err(|_| X509Error::TooLarge)?;
    }
    let mut out = Buf::new();
    tlv(&mut out, tag, &body)?;
    Ok(out)
}

fn leaf(tag: u8, body: &[u8]) -> Result<Buf, X509Error> {
    let mut out = Buf::new();
    tlv(&mut out, tag, body)?;
    Ok(out)
}

const OID_ECDSA_SHA256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02];
const OID_EC_PUBLIC_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const OID_PRIME256V1: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1D, 0x13];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1D, 0x0F];
const OID_EXT_KEY_USAGE: &[u8] = &[0x55, 0x1D, 0x25];
const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1D, 0x11];
const OID_SERVER_AUTH: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

fn oid(body: &[u8]) -> Result<Buf, X509Error> {
    leaf(0x06, body)
}

fn sig_alg() -> Result<Buf, X509Error> {
    wrap(0x30, &[&oid(OID_ECDSA_SHA256)?])
}

/// A DNS name usable as the common name and in the alternative names: letters, digits, dots and hyphens, 1 to 63 bytes.
pub fn valid_dns_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 63 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') && !n.starts_with(['-', '.']) && !n.ends_with(['-', '.']) && !n.contains("..")
}

fn name(cn: &str) -> Result<Buf, X509Error> {
    let atv = wrap(0x30, &[&oid(OID_COMMON_NAME)?, &leaf(0x0C, cn.as_bytes())?])?;
    wrap(0x30, &[&wrap(0x31, &[&atv])?])
}

fn time(unix: i64) -> Result<Buf, X509Error> {
    let d = unix_to_datetime(unix);
    let mut s: String<16> = String::new();
    use core::fmt::Write;
    // UTCTime has two-digit years and runs to 2049; later dates must be GeneralizedTime (RFC 5280 section 4.1.2.5)
    let (tag, r) = if d.year < 2050 {
        (0x17, write!(s, "{:02}{:02}{:02}{:02}{:02}{:02}Z", d.year % 100, d.month, d.day, d.hour, d.minute, d.second))
    } else {
        (0x18, write!(s, "{:04}{:02}{:02}{:02}{:02}{:02}Z", d.year, d.month, d.day, d.hour, d.minute, d.second))
    };
    r.map_err(|_| X509Error::TooLarge)?;
    leaf(tag, s.as_bytes())
}

fn extension(oid_body: &[u8], critical: bool, value: &[u8]) -> Result<Buf, X509Error> {
    let o = oid(oid_body)?;
    let value = leaf(0x04, value)?;
    if critical {
        wrap(0x30, &[&o, &[0x01, 0x01, 0xFF], &value])
    } else {
        wrap(0x30, &[&o, &value])
    }
}

/// Builds a self-signed certificate for the DNS `names` (the first is the common name), signed with `secret`.
///
/// `serial` and `secret` come from the caller's random source; `now_unix` is the RTC. The certificate is a leaf (not a CA)
/// for digital signatures and server authentication, valid from a day before `now_unix` for ten years.
pub fn generate_self_signed(secret: [u8; 32], serial: [u8; 8], names: &[&str], now_unix: i64) -> Result<Buf, X509Error> {
    if names.is_empty() || names.len() > 4 || !names.iter().all(|n| valid_dns_name(n)) {
        return Err(X509Error::BadName);
    }
    if now_unix < 1_704_067_200 {
        return Err(X509Error::BadTime); // before 2024-01-01
    }
    let sk = SigningKey::from_bytes((&secret).into()).map_err(|_| X509Error::BadSecret)?;
    let point = sk.verifying_key().to_sec1_point(false);
    let mut serial = serial;
    serial[0] = (serial[0] & 0x7F) | 0x01; // positive, and the first byte is nonzero so the INTEGER is minimal

    let version = wrap(0xA0, &[&leaf(0x02, &[0x02])?])?;
    let serial_der = leaf(0x02, &serial)?;
    let alg = sig_alg()?;
    let subject = name(names[0])?;
    let validity = wrap(0x30, &[&time(now_unix - BACKDATE_SECS)?, &time(now_unix + VALID_DAYS * 86_400)?])?;
    let spki = wrap(
        0x30,
        &[&wrap(0x30, &[&oid(OID_EC_PUBLIC_KEY)?, &oid(OID_PRIME256V1)?])?, &{
            let mut bits = Buf::new();
            bits.push(0x00).map_err(|_| X509Error::TooLarge)?; // no unused bits
            bits.extend_from_slice(point.as_bytes()).map_err(|_| X509Error::TooLarge)?;
            leaf(0x03, &bits)?
        }],
    )?;

    let mut sans = Buf::new();
    for n in names {
        tlv(&mut sans, 0x82, n.as_bytes())?; // dNSName
    }
    let extensions = wrap(
        0xA3,
        &[&wrap(
            0x30,
            &[
                &extension(OID_BASIC_CONSTRAINTS, true, &[0x30, 0x00])?, // CA:FALSE (the default, so an empty SEQUENCE)
                &extension(OID_KEY_USAGE, true, &[0x03, 0x02, 0x07, 0x80])?, // digitalSignature
                &extension(OID_EXT_KEY_USAGE, false, &wrap(0x30, &[&oid(OID_SERVER_AUTH)?])?)?,
                &extension(OID_SUBJECT_ALT_NAME, false, &wrap(0x30, &[&sans])?)?,
            ],
        )?],
    )?;

    let tbs = wrap(0x30, &[&version, &serial_der, &alg, &subject, &validity, &subject, &spki, &extensions])?;
    let signature: Signature = sk.sign(&tbs);
    let der_sig = signature.to_der();
    let mut bits = Buf::new();
    bits.push(0x00).map_err(|_| X509Error::TooLarge)?;
    bits.extend_from_slice(der_sig.as_bytes()).map_err(|_| X509Error::TooLarge)?;
    wrap(0x30, &[&tbs, &alg, &leaf(0x03, &bits)?])
}

// ---- DER reader --------------------------------------------------------------------------------------------------

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    /// The next element as (tag, body). Strict: definite lengths in minimal form, no trailing garbage inside.
    fn next(&mut self) -> Result<(u8, &'a [u8]), X509Error> {
        let b = self.0;
        if b.len() < 2 || b[0] & 0x1F == 0x1F {
            return Err(X509Error::Der); // truncated, or a multi-byte tag we never need
        }
        let (len, hdr) = match b[1] {
            n @ 0..=127 => (n as usize, 2),
            0x81 if b.len() >= 3 && b[2] >= 128 => (b[2] as usize, 3),
            0x82 if b.len() >= 4 && u16::from_be_bytes([b[2], b[3]]) >= 256 => (u16::from_be_bytes([b[2], b[3]]) as usize, 4),
            _ => return Err(X509Error::Der),
        };
        let body = b.get(hdr..hdr + len).ok_or(X509Error::Der)?;
        self.0 = &b[hdr + len..];
        Ok((b[0], body))
    }

    fn expect(&mut self, tag: u8) -> Result<&'a [u8], X509Error> {
        let (t, body) = self.next()?;
        if t == tag { Ok(body) } else { Err(X509Error::Der) }
    }

    fn done(&self) -> bool {
        self.0.is_empty()
    }
}

fn parse_time(tag: u8, b: &[u8]) -> Result<i64, X509Error> {
    let digits = |s: &[u8]| -> Result<u32, X509Error> {
        s.iter().try_fold(0u32, |acc, &c| if c.is_ascii_digit() { Ok(acc * 10 + (c - b'0') as u32) } else { Err(X509Error::Der) })
    };
    let (year, rest) = match (tag, b.len()) {
        (0x17, 13) => {
            let yy = digits(&b[..2])?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &b[2..])
        }
        (0x18, 15) => (digits(&b[..4])?, &b[4..]),
        _ => return Err(X509Error::Der),
    };
    if rest.len() != 11 || rest[10] != b'Z' {
        return Err(X509Error::Der);
    }
    let (month, day, hour, minute, second) = (digits(&rest[0..2])?, digits(&rest[2..4])?, digits(&rest[4..6])?, digits(&rest[6..8])?, digits(&rest[8..10])?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return Err(X509Error::Der);
    }
    Ok(datetime_to_unix(&DateTime { year: year as u16, month: month as u8, day: day as u8, hour: hour as u8, minute: minute as u8, second: second as u8 }))
}

/// What we read from a certificate.
#[derive(Debug, PartialEq, Eq)]
pub struct Info {
    /// The uncompressed P-256 public key (0x04 || X || Y).
    pub public_key: [u8; 65],
    pub not_before: i64,
    pub not_after: i64,
}

/// Reads a DER certificate and requires an ECDSA P-256 public key. The signature is not checked here (a self-signed or
/// CA-signed certificate is equally acceptable as a server certificate; clients decide whether to trust it).
pub fn inspect(cert: &[u8]) -> Result<Info, X509Error> {
    if cert.len() > MAX_CERT {
        return Err(X509Error::TooLarge);
    }
    let mut top = Reader(cert);
    let body = top.expect(0x30)?;
    if !top.done() {
        return Err(X509Error::Der);
    }
    let mut c = Reader(body);
    let tbs_body = c.expect(0x30)?;
    let _alg = c.expect(0x30)?;
    let _sig = c.expect(0x03)?;
    if !c.done() {
        return Err(X509Error::Der);
    }
    let mut t = Reader(tbs_body);
    // version [0] must be present and v3 (the extensions we rely on need it)
    let version = t.expect(0xA0).map_err(|_| X509Error::Unsupported)?;
    if Reader(version).expect(0x02)? != [0x02] {
        return Err(X509Error::Unsupported);
    }
    t.expect(0x02)?; // serial number
    t.expect(0x30)?; // signature algorithm
    t.expect(0x30)?; // issuer
    let validity = t.expect(0x30)?;
    t.expect(0x30)?; // subject
    let spki = t.expect(0x30)?;
    let mut v = Reader(validity);
    let (t1, b1) = v.next()?;
    let (t2, b2) = v.next()?;
    if !v.done() {
        return Err(X509Error::Der);
    }
    let (not_before, not_after) = (parse_time(t1, b1)?, parse_time(t2, b2)?);

    let mut k = Reader(spki);
    let mut alg = Reader(k.expect(0x30)?);
    let bits = k.expect(0x03)?;
    if !k.done() || alg.expect(0x06)? != OID_EC_PUBLIC_KEY || alg.expect(0x06)? != OID_PRIME256V1 || !alg.done() {
        return Err(X509Error::Unsupported);
    }
    if bits.len() != 66 || bits[0] != 0x00 || bits[1] != 0x04 {
        return Err(X509Error::Unsupported);
    }
    let mut public_key = [0u8; 65];
    public_key.copy_from_slice(&bits[1..]);
    Ok(Info { public_key, not_before, not_after })
}

/// Checks that `secret` belongs to `cert` and that the certificate is valid at `now_unix`.
pub fn validate_pair(cert: &[u8], secret: &[u8; 32], now_unix: i64) -> Result<Info, X509Error> {
    let info = inspect(cert)?;
    let sk = SigningKey::from_bytes(secret.into()).map_err(|_| X509Error::BadSecret)?;
    if sk.verifying_key().to_sec1_point(false).as_bytes() != info.public_key {
        return Err(X509Error::KeyMismatch);
    }
    if now_unix < info.not_before {
        return Err(X509Error::NotYetValid);
    }
    if now_unix > info.not_after {
        return Err(X509Error::Expired);
    }
    Ok(info)
}

/// True if `der` is one complete DER SEQUENCE and nothing else: the shape of a certificate. Used for the intermediates of an
/// uploaded chain, which are passed on to clients without being interpreted.
pub fn is_certificate_der(der: &[u8]) -> bool {
    if der.len() > MAX_CERT {
        return false;
    }
    let mut top = Reader(der);
    matches!(top.expect(0x30), Ok(body) if body.len() > 8) && top.done()
}

/// True if `secret` is the private key of the P-256 certificate `cert` (no time check; see [`validate_pair`]).
pub fn key_matches(cert: &[u8], secret: &[u8; 32]) -> bool {
    match (inspect(cert), SigningKey::from_bytes(secret.into())) {
        (Ok(info), Ok(sk)) => sk.verifying_key().to_sec1_point(false).as_bytes() == info.public_key,
        _ => false,
    }
}

/// SHA-256 of the certificate, the usual fingerprint.
pub fn fingerprint(cert: &[u8]) -> [u8; 32] {
    Sha256::digest(cert).into()
}

/// `AB:CD:...` as browsers and `openssl x509 -fingerprint` show it.
pub fn fingerprint_hex(f: &[u8; 32]) -> String<95> {
    let mut s = String::new();
    for (i, b) in f.iter().enumerate() {
        use core::fmt::Write;
        let _ = write!(s, "{}{:02X}", if i == 0 { "" } else { ":" }, b);
    }
    s
}

/// The private key as an unencrypted PKCS#8 `PrivateKeyInfo` (RFC 5208) for P-256, which is what TLS libraries load. The
/// optional public key is left out, as OpenSSL does when asked for the minimal form.
pub fn secret_to_pkcs8(secret: &[u8; 32]) -> Result<Vec<u8, 67>, X509Error> {
    SigningKey::from_bytes(secret.into()).map_err(|_| X509Error::BadSecret)?; // zero or not below the group order
    let mut out = Vec::new();
    // SEQ { INTEGER 0, SEQ { OID ecPublicKey, OID prime256v1 }, OCTET STRING { SEQ { INTEGER 1, OCTET STRING key } } }
    out.extend_from_slice(&[0x30, 0x41, 0x02, 0x01, 0x00, 0x30, 0x13, 0x06, 0x07]).map_err(|_| X509Error::TooLarge)?;
    out.extend_from_slice(OID_EC_PUBLIC_KEY).map_err(|_| X509Error::TooLarge)?;
    out.extend_from_slice(&[0x06, 0x08]).map_err(|_| X509Error::TooLarge)?;
    out.extend_from_slice(OID_PRIME256V1).map_err(|_| X509Error::TooLarge)?;
    out.extend_from_slice(&[0x04, 0x27, 0x30, 0x25, 0x02, 0x01, 0x01, 0x04, 0x20]).map_err(|_| X509Error::TooLarge)?;
    out.extend_from_slice(secret).map_err(|_| X509Error::TooLarge)?;
    Ok(out)
}

// ---- PEM and private keys ----------------------------------------------------------------------------------------

fn b64(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes base64 (whitespace ignored, padding required to be correct) and appends to `out`.
fn base64_decode<const N: usize>(text: &str, out: &mut Vec<u8, N>) -> Result<(), X509Error> {
    let (mut acc, mut bits, mut pad, mut count) = (0u32, 0, 0, 0usize);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            pad += 1;
            continue;
        }
        if pad > 0 {
            return Err(X509Error::Unsupported); // data after padding
        }
        let v = b64(c).ok_or(X509Error::Unsupported)? as u32;
        count += 1;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8).map_err(|_| X509Error::TooLarge)?;
            acc &= (1 << bits) - 1;
        }
    }
    // the leftover bits must be zero, and the padding must say exactly how many characters are missing
    let padding_ok = matches!((count % 4, pad), (0, 0) | (2, 2) | (3, 1));
    if acc != 0 || !padding_ok {
        return Err(X509Error::Unsupported);
    }
    Ok(())
}

/// The PEM blocks of `text` in order: (label, body between the lines).
fn pem_blocks(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = text;
    core::iter::from_fn(move || {
        let start = rest.find("-----BEGIN ")?;
        let after = &rest[start + 11..];
        let label_end = after.find("-----")?;
        let label = &after[..label_end];
        let body_start = &after[label_end + 5..];
        let end_marker = end_marker(label);
        let body_end = body_start.find(end_marker.as_str())?;
        let body = &body_start[..body_end];
        rest = &body_start[body_end + end_marker.len()..];
        Some((label, body))
    })
}

fn end_marker(label: &str) -> String<64> {
    let mut s = String::new();
    let _ = s.push_str("-----END ");
    let _ = s.push_str(label);
    let _ = s.push_str("-----");
    s
}

/// All certificates in PEM text (a leaf followed by its intermediates), as DER in one buffer with their lengths.
pub fn parse_pem_chain(text: &str, out: &mut Vec<u8, MAX_CHAIN>, lens: &mut Vec<u16, MAX_CHAIN_CERTS>) -> Result<(), X509Error> {
    out.clear();
    lens.clear();
    for (label, body) in pem_blocks(text) {
        if label != "CERTIFICATE" {
            continue;
        }
        let before = out.len();
        base64_decode(body, out)?;
        let len = out.len() - before;
        if len == 0 || len > MAX_CERT {
            return Err(X509Error::TooLarge);
        }
        lens.push(len as u16).map_err(|_| X509Error::TooLarge)?;
    }
    if lens.is_empty() { Err(X509Error::Unsupported) } else { Ok(()) }
}

/// A P-256 private key from PEM (`PRIVATE KEY` = PKCS#8, `EC PRIVATE KEY` = SEC1) or from DER of either kind.
pub fn parse_private_key(input: &[u8]) -> Result<[u8; 32], X509Error> {
    let from_der = |der: &[u8]| -> Result<[u8; 32], X509Error> {
        let sk = SecretKey::from_pkcs8_der(der).or_else(|_| SecretKey::from_sec1_der(der)).map_err(|_| X509Error::Unsupported)?;
        Ok(sk.to_bytes().into())
    };
    if let Ok(text) = core::str::from_utf8(input) {
        if text.contains("-----BEGIN ") {
            for (label, body) in pem_blocks(text) {
                if label == "PRIVATE KEY" || label == "EC PRIVATE KEY" {
                    let mut der: Vec<u8, 256> = Vec::new();
                    base64_decode(body, &mut der)?;
                    return from_der(&der);
                }
            }
            return Err(X509Error::Unsupported);
        }
    }
    from_der(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_000_000; // 2026-10-14
    const SECRET: [u8; 32] = [0x11; 32];

    fn cert() -> Buf {
        generate_self_signed(SECRET, [0x80, 1, 2, 3, 4, 5, 6, 7], &["picoclock-54", "picoclock-54.local"], NOW).unwrap()
    }

    // VERIFIES: REQ-WEB-07
    #[test]
    fn generated_certificate_can_be_read_back_and_matches_its_key() {
        let c = cert();
        assert!(c.len() > 250 && c.len() < 450, "{}", c.len());
        let info = validate_pair(&c, &SECRET, NOW).unwrap();
        assert_eq!(info.not_before, NOW - 86_400);
        assert_eq!(info.not_after, NOW + 3650 * 86_400);
        assert_eq!(inspect(&c).unwrap(), info);
        // the same inputs give the same certificate (deterministic ECDSA), other inputs a different one
        assert_eq!(cert().as_slice(), c.as_slice());
        let other = generate_self_signed([0x22; 32], [1; 8], &["picoclock-54"], NOW).unwrap();
        assert_ne!(other.as_slice(), c.as_slice());
        assert_ne!(fingerprint(&other), fingerprint(&c));
    }

    // VERIFIES: REQ-WEB-07
    #[test]
    fn validation_refuses_a_wrong_key_and_the_wrong_time() {
        let c = cert();
        assert_eq!(validate_pair(&c, &[0x33; 32], NOW), Err(X509Error::KeyMismatch));
        assert_eq!(validate_pair(&c, &[0; 32], NOW), Err(X509Error::BadSecret));
        assert_eq!(validate_pair(&c, &SECRET, NOW - 2 * 86_400), Err(X509Error::NotYetValid));
        assert_eq!(validate_pair(&c, &SECRET, NOW + 3651 * 86_400), Err(X509Error::Expired));
        assert!(validate_pair(&c, &SECRET, NOW + 3649 * 86_400).is_ok());
    }

    // VERIFIES: REQ-WEB-07
    #[test]
    fn generation_refuses_bad_inputs() {
        let ok = |names: &[&str], now| generate_self_signed(SECRET, [1; 8], names, now).err();
        assert_eq!(ok(&[], NOW), Some(X509Error::BadName));
        assert_eq!(ok(&["a b"], NOW), Some(X509Error::BadName));
        assert_eq!(ok(&["-a"], NOW), Some(X509Error::BadName));
        assert_eq!(ok(&["a..b"], NOW), Some(X509Error::BadName));
        assert_eq!(ok(&["ü.example"], NOW), Some(X509Error::BadName));
        assert_eq!(ok(&[&"a".repeat(64)], NOW), Some(X509Error::BadName));
        assert_eq!(ok(&["a", "b", "c", "d", "e"], NOW), Some(X509Error::BadName));
        assert_eq!(ok(&["picoclock"], 0), Some(X509Error::BadTime));
        assert_eq!(ok(&["picoclock"], 1_000_000_000), Some(X509Error::BadTime));
        assert_eq!(generate_self_signed([0; 32], [1; 8], &["x"], NOW).err(), Some(X509Error::BadSecret));
        assert_eq!(generate_self_signed([0xFF; 32], [1; 8], &["x"], NOW).err(), Some(X509Error::BadSecret), "not below the group order");
        assert!(ok(&["a", "b.c", "d-e.f.g", "h"], NOW).is_none());
    }

    // VERIFIES: REQ-WEB-07
    #[test]
    fn the_serial_is_always_positive_and_minimal() {
        for first in [0x00u8, 0x01, 0x7F, 0x80, 0xFF] {
            let c = generate_self_signed(SECRET, [first, 9, 9, 9, 9, 9, 9, 9], &["x"], NOW).unwrap();
            // version [0] is 5 bytes after the 4-byte tbs header; the serial INTEGER follows it
            let tbs = Reader(&c).expect(0x30).unwrap();
            let mut t = Reader(Reader(tbs).expect(0x30).unwrap());
            t.next().unwrap();
            let serial = t.expect(0x02).unwrap();
            assert_eq!(serial.len(), 8);
            assert!(serial[0] >= 1 && serial[0] < 0x80, "{serial:02x?}");
        }
    }

    // VERIFIES: REQ-WEB-07
    #[test]
    fn inspect_rejects_malformed_and_unsupported_certificates() {
        let c = cert();
        // every truncation is refused without a panic
        for n in 0..c.len() {
            assert!(inspect(&c[..n]).is_err(), "prefix {n}");
        }
        // trailing bytes after the certificate
        let mut longer = c.to_vec();
        longer.push(0);
        assert_eq!(inspect(&longer), Err(X509Error::Der));
        // a flipped byte anywhere must never panic (it may or may not still parse)
        for i in 0..c.len() {
            let mut bad = c.to_vec();
            bad[i] ^= 0xFF;
            let _ = inspect(&bad);
        }
        // a non-minimal length is refused
        let mut nm = c.to_vec();
        nm.splice(1..2, [0x81, c.len() as u8 - 3]);
        assert!(inspect(&nm).is_err());
        assert_eq!(inspect(&[0u8; MAX_CERT + 1]), Err(X509Error::TooLarge));
        assert!(inspect(&[]).is_err());
        assert!(inspect(&[0x30, 0x00]).is_err());
    }

    // VERIFIES: REQ-WEB-07
    #[test]
    fn pkcs8_form_of_a_secret_reads_back_and_is_well_formed() {
        let der = secret_to_pkcs8(&SECRET).unwrap();
        assert_eq!(der.len(), 67);
        assert_eq!(parse_private_key(&der).unwrap(), SECRET, "our own reader gets the key back");
        // the outer lengths are right: SEQUENCE 0x41 = the 65 bytes that follow
        let mut r = Reader(&der);
        assert_eq!(r.expect(0x30).unwrap().len(), 65);
        assert!(r.done());
        assert_eq!(secret_to_pkcs8(&[0; 32]), Err(X509Error::BadSecret));
        assert_eq!(secret_to_pkcs8(&[0xFF; 32]), Err(X509Error::BadSecret));
    }

    // VERIFIES: REQ-WEB-07
    #[test]
    fn fingerprint_is_formatted_like_openssl() {
        let f = fingerprint(b"abc");
        assert_eq!(fingerprint_hex(&f).as_str(), "BA:78:16:BF:8F:01:CF:EA:41:41:40:DE:5D:AE:22:23:B0:03:61:A3:96:17:7A:9C:B4:10:FF:61:F2:00:15:AD");
    }

    // VERIFIES: REQ-WEB-07
    #[test]
    fn base64_and_pem_parsing() {
        let mut out: Vec<u8, 16> = Vec::new();
        base64_decode("TWFu", &mut out).unwrap();
        assert_eq!(out.as_slice(), b"Man");
        out.clear();
        base64_decode("TWE=", &mut out).unwrap();
        assert_eq!(out.as_slice(), b"Ma");
        out.clear();
        base64_decode("TQ==", &mut out).unwrap();
        assert_eq!(out.as_slice(), b"M");
        out.clear();
        base64_decode("T W\n Fu", &mut out).unwrap();
        assert_eq!(out.as_slice(), b"Man");
        for bad in ["TWF*", "T===", "TQ=A", "TWE=Z"] {
            out.clear();
            assert!(base64_decode(bad, &mut out).is_err(), "{bad}");
        }
        let mut tiny: Vec<u8, 2> = Vec::new();
        assert_eq!(base64_decode("TWFu", &mut tiny), Err(X509Error::TooLarge));

        let text = "junk\n-----BEGIN CERTIFICATE-----\nTWFu\n-----END CERTIFICATE-----\n-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n-----BEGIN CERTIFICATE-----\nTWE=\n-----END CERTIFICATE-----\n";
        let mut chain = Vec::new();
        let mut lens = Vec::new();
        parse_pem_chain(text, &mut chain, &mut lens).unwrap();
        assert_eq!((chain.as_slice(), lens.as_slice()), (&b"ManMa"[..], &[3u16, 2][..]));
        assert!(parse_pem_chain("no pem here", &mut chain, &mut lens).is_err());
        assert!(parse_pem_chain("-----BEGIN CERTIFICATE-----\nTWFu\n", &mut chain, &mut lens).is_err(), "unterminated block");
    }
}
