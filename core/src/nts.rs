//! Network Time Security for NTP (RFC 8915): the NTS-KE message format, the NTP extension fields and the AEAD that
//! protects NTP packets. No I/O and no allocation; the TLS exchange that carries NTS-KE is in `nts_ke`.
//!
//! Flow: [`ke_request`] -> (TLS 1.3, ALPN `ntske/1`) -> [`parse_ke_response`] gives cookies; the TLS exporter gives the
//! two keys ([`EXPORTER_LABEL`], [`exporter_context`]); [`Session::build_request`] / [`Session::handle_response`] then
//! protect each NTP exchange with AES-SIV-CMAC-256 and hand out fresh cookies.

use aes_siv::KeyInit;
use aes_siv::siv::Aes128Siv;
use heapless::{String, Vec};
use rand_core::RngCore;

use crate::sntp;

/// AEAD_AES_SIV_CMAC_256 (RFC 5297, IANA AEAD id 15): the one algorithm we offer; the key is 32 bytes.
pub const AEAD_AES_SIV_CMAC_256: u16 = 15;
pub const KEY_LEN: usize = 32;
/// TLS exporter label of RFC 8915 section 4.2.
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-network-time-security";
/// ALPN protocol name of NTS-KE.
pub const ALPN: &[u8] = b"ntske/1";
pub const NTS_KE_PORT: u16 = 4460;
pub const MAX_COOKIES: usize = 8;
/// Longest cookie we store. RFC 8915 leaves the size to the server; real ones are about 100 to 200 bytes.
pub const MAX_COOKIE: usize = 256;
/// Largest NTS-KE response we accept (cookies dominate: 8 x ~110 bytes).
pub const MAX_KE_RESPONSE: usize = 2048;
/// Largest NTP packet we build or accept. Kept under a typical 1500-byte MTU with room for IP and UDP headers.
pub const MAX_PACKET: usize = 1400;

// NTS-KE record types (RFC 8915 section 4.1)
const KE_END: u16 = 0;
const KE_NEXT_PROTOCOL: u16 = 1;
const KE_ERROR: u16 = 2;
const KE_WARNING: u16 = 3;
const KE_AEAD: u16 = 4;
const KE_COOKIE: u16 = 5;
const KE_SERVER: u16 = 6;
const KE_PORT: u16 = 7;
const NEXT_PROTOCOL_NTPV4: u16 = 0;

// NTP extension field types (RFC 8915 section 5.2 and following)
const EF_UNIQUE_ID: u16 = 0x0104;
const EF_COOKIE: u16 = 0x0204;
const EF_COOKIE_PLACEHOLDER: u16 = 0x0304;
const EF_AUTHENTICATOR: u16 = 0x0404;
const UID_LEN: usize = 32;
const NONCE_LEN: usize = 16;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = sntp::PACKET_LEN;

pub type Cookie = Vec<u8, MAX_COOKIE>;
pub type Key = [u8; KEY_LEN];

#[derive(Debug, PartialEq, Eq)]
pub enum NtsError {
    /// NTS-KE message is truncated, malformed or larger than we accept.
    BadKeMessage,
    /// The server sent an NTS-KE error record (code).
    ServerError(u16),
    /// A critical record we do not understand.
    UnknownCritical(u16),
    /// The server did not agree on NTPv4 or on our AEAD algorithm.
    NoAgreement,
    /// The server sent no usable cookie.
    NoCookies,
    /// A cookie is longer than we can store, or its length is not a multiple of 4 (an extension field cannot carry it).
    BadCookie,
    /// The NTP response is malformed, lacks the unique identifier or the authenticator, or the identifier is not ours.
    BadResponse,
    /// The authenticator did not verify: forged, corrupted, or made with other keys.
    AuthenticationFailed,
    /// The server answered with an NTS NAK: our cookies are no longer valid, a new key exchange is needed.
    Nak,
    /// The packet would not fit.
    TooLarge,
    /// The time part of the response was rejected (see [`sntp::SntpError`]).
    Time(sntp::SntpError),
}

/// Random bytes from the `embassy-crypto` RNG driver the final binary links (the ring oscillator on the clock, the
/// operating system on the host). Implements `RngCore` so the same code serves tests that pass a fixed generator.
pub struct DriverRng;

impl RngCore for DriverRng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        embassy_crypto::rng_fill_bytes(dest);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

// ---- NTS-KE ------------------------------------------------------------------------------------------------------

/// The client request: NTPv4 next protocol, our AEAD algorithm, end of message (RFC 8915 section 4.1.1).
pub fn ke_request() -> [u8; 16] {
    let mut m = [0u8; 16];
    // next protocol negotiation: critical, type 1, 2-byte body = NTPv4
    m[0..2].copy_from_slice(&(0x8000 | KE_NEXT_PROTOCOL).to_be_bytes());
    m[2..4].copy_from_slice(&2u16.to_be_bytes());
    m[4..6].copy_from_slice(&NEXT_PROTOCOL_NTPV4.to_be_bytes());
    // AEAD algorithm negotiation: critical, type 4, one algorithm
    m[6..8].copy_from_slice(&(0x8000 | KE_AEAD).to_be_bytes());
    m[8..10].copy_from_slice(&2u16.to_be_bytes());
    m[10..12].copy_from_slice(&AEAD_AES_SIV_CMAC_256.to_be_bytes());
    // end of message: critical, type 0, empty
    m[12..14].copy_from_slice(&(0x8000 | KE_END).to_be_bytes());
    m[14..16].copy_from_slice(&0u16.to_be_bytes());
    m
}

/// What the server told us in NTS-KE.
#[derive(Debug, PartialEq, Eq)]
pub struct KeResponse {
    pub cookies: Vec<Cookie, MAX_COOKIES>,
    /// Host name or address string of the NTP server, if it is not the NTS-KE host.
    pub server: Option<String<64>>,
    pub port: Option<u16>,
}

/// Length of the complete NTS-KE message at the start of `buf`, or `None` while the End of Message record has not
/// arrived yet. Used to know when to stop reading from the TLS connection.
pub fn ke_message_len(buf: &[u8]) -> Result<Option<usize>, NtsError> {
    let mut i = 0;
    while i + 4 <= buf.len() {
        let ty = u16::from_be_bytes([buf[i], buf[i + 1]]) & 0x7FFF;
        let len = u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize;
        if i + 4 + len > MAX_KE_RESPONSE {
            return Err(NtsError::BadKeMessage);
        }
        if i + 4 + len > buf.len() {
            return Ok(None);
        }
        i += 4 + len;
        if ty == KE_END {
            return Ok(Some(i));
        }
    }
    if buf.len() >= MAX_KE_RESPONSE { Err(NtsError::BadKeMessage) } else { Ok(None) }
}

/// Parses a complete NTS-KE response (RFC 8915 section 4.1.2 and following).
pub fn parse_ke_response(msg: &[u8]) -> Result<KeResponse, NtsError> {
    let (mut next_protocol, mut aead, mut ended) = (false, false, false);
    let mut out = KeResponse { cookies: Vec::new(), server: None, port: None };
    let mut i = 0;
    while i < msg.len() {
        if ended || i + 4 > msg.len() {
            return Err(NtsError::BadKeMessage); // data after the end record, or a partial header
        }
        let critical = msg[i] & 0x80 != 0;
        let ty = u16::from_be_bytes([msg[i], msg[i + 1]]) & 0x7FFF;
        let len = u16::from_be_bytes([msg[i + 2], msg[i + 3]]) as usize;
        let body = msg.get(i + 4..i + 4 + len).ok_or(NtsError::BadKeMessage)?;
        i += 4 + len;
        match ty {
            KE_END => {
                if len != 0 || !critical {
                    return Err(NtsError::BadKeMessage);
                }
                ended = true;
            }
            KE_NEXT_PROTOCOL => {
                // exactly the protocol we asked for; a list with anything else means no agreement
                if body.len() != 2 || u16::from_be_bytes([body[0], body[1]]) != NEXT_PROTOCOL_NTPV4 || next_protocol {
                    return Err(NtsError::NoAgreement);
                }
                next_protocol = true;
            }
            KE_AEAD => {
                if body.len() != 2 || u16::from_be_bytes([body[0], body[1]]) != AEAD_AES_SIV_CMAC_256 || aead {
                    return Err(NtsError::NoAgreement);
                }
                aead = true;
            }
            KE_ERROR => {
                let code = if body.len() == 2 { u16::from_be_bytes([body[0], body[1]]) } else { 0xFFFF };
                return Err(NtsError::ServerError(code));
            }
            KE_WARNING => {} // informational
            KE_COOKIE => {
                if body.is_empty() || body.len() > MAX_COOKIE || body.len() % 4 != 0 {
                    return Err(NtsError::BadCookie);
                }
                // keep the first MAX_COOKIES; a server that sends more is not an error
                if out.cookies.len() < MAX_COOKIES {
                    let mut c = Cookie::new();
                    c.extend_from_slice(body).map_err(|_| NtsError::BadCookie)?;
                    let _ = out.cookies.push(c);
                }
            }
            KE_SERVER => {
                let s = core::str::from_utf8(body).map_err(|_| NtsError::BadKeMessage)?;
                if s.is_empty() || s.bytes().any(|b| b <= 0x20 || b >= 0x7F) {
                    return Err(NtsError::BadKeMessage);
                }
                out.server = Some(String::try_from(s).map_err(|_| NtsError::BadKeMessage)?);
            }
            KE_PORT => {
                if body.len() != 2 {
                    return Err(NtsError::BadKeMessage);
                }
                out.port = Some(u16::from_be_bytes([body[0], body[1]]));
            }
            other => {
                if critical {
                    return Err(NtsError::UnknownCritical(other));
                }
            }
        }
    }
    if !ended {
        return Err(NtsError::BadKeMessage);
    }
    if !next_protocol || !aead {
        return Err(NtsError::NoAgreement);
    }
    if out.cookies.is_empty() {
        return Err(NtsError::NoCookies);
    }
    Ok(out)
}

/// The TLS exporter context of RFC 8915 section 4.2: next protocol (2), AEAD id (2) and one byte, 0 for the
/// client-to-server key and 1 for the server-to-client key.
pub fn exporter_context(client_to_server: bool) -> [u8; 5] {
    let mut c = [0u8; 5];
    c[0..2].copy_from_slice(&NEXT_PROTOCOL_NTPV4.to_be_bytes());
    c[2..4].copy_from_slice(&AEAD_AES_SIV_CMAC_256.to_be_bytes());
    c[4] = if client_to_server { 0 } else { 1 };
    c
}

// ---- NTP with NTS ------------------------------------------------------------------------------------------------

/// Everything needed to protect NTP packets after a successful key exchange. Lives in RAM only.
pub struct Session {
    c2s: Key,
    s2c: Key,
    cookies: Vec<Cookie, MAX_COOKIES>,
    /// Server host and port to send NTP to (the NTS-KE host and 123 unless the server said otherwise).
    pub server: String<64>,
    pub port: u16,
}

/// What [`Session::build_request`] returns besides the packet: the identifier the response must echo.
pub struct Pending {
    pub unique_id: [u8; UID_LEN],
    pub nonce: [u8; 8],
}

impl Session {
    pub fn new(c2s: Key, s2c: Key, ke: KeResponse, ke_host: &str) -> Result<Self, NtsError> {
        let server = match ke.server {
            Some(s) => s,
            None => String::try_from(ke_host).map_err(|_| NtsError::BadKeMessage)?,
        };
        Ok(Self { c2s, s2c, cookies: ke.cookies, server, port: ke.port.unwrap_or(123) })
    }

    pub fn cookies_left(&self) -> usize {
        self.cookies.len()
    }

    /// Builds an authenticated request. Uses up one cookie (each cookie is single-use) and asks for replacements with
    /// placeholders so that the response brings the store back to [`MAX_COOKIES`].
    pub fn build_request(&mut self, rng: &mut impl RngCore, nonce: [u8; 8]) -> Result<(Vec<u8, MAX_PACKET>, Pending), NtsError> {
        let cookie = self.cookies.pop().ok_or(NtsError::NoCookies)?;
        let mut p: Vec<u8, MAX_PACKET> = Vec::new();
        let put = |p: &mut Vec<u8, MAX_PACKET>, b: &[u8]| p.extend_from_slice(b).map_err(|_| NtsError::TooLarge);

        let mut header = sntp::build_request(nonce);
        header[0] = 0b00_100_011; // LI 0, version 4, client
        put(&mut p, &header)?;

        let mut unique_id = [0u8; UID_LEN];
        rng.fill_bytes(&mut unique_id);
        ef(&mut p, EF_UNIQUE_ID, &unique_id)?;
        ef(&mut p, EF_COOKIE, &cookie)?;
        // The server returns one new cookie for every cookie or placeholder field in the request (RFC 8915 section 5.5),
        // so the cookie field itself replaces the one we spent; add placeholders for any further cookies we are missing.
        let missing = MAX_COOKIES.saturating_sub(self.cookies.len() + 1);
        let room = MAX_PACKET.saturating_sub(p.len() + 4 + NONCE_LEN + TAG_LEN + 16);
        let placeholders = missing.min(room / (4 + cookie.len()));
        for _ in 0..placeholders {
            let zeros = [0u8; MAX_COOKIE];
            ef(&mut p, EF_COOKIE_PLACEHOLDER, &zeros[..cookie.len()])?;
        }

        // authenticator: AD = everything so far, then the nonce; no encrypted fields in a request
        let mut nonce16 = [0u8; NONCE_LEN];
        rng.fill_bytes(&mut nonce16);
        let tag = Aes128Siv::new(&self.c2s.into())
            .encrypt_in_place_detached([p.as_slice(), &nonce16[..]], &mut [])
            .map_err(|_| NtsError::AuthenticationFailed)?;
        let mut body: Vec<u8, 64> = Vec::new();
        body.extend_from_slice(&(NONCE_LEN as u16).to_be_bytes()).map_err(|_| NtsError::TooLarge)?;
        body.extend_from_slice(&(TAG_LEN as u16).to_be_bytes()).map_err(|_| NtsError::TooLarge)?;
        body.extend_from_slice(&nonce16).map_err(|_| NtsError::TooLarge)?;
        body.extend_from_slice(&tag).map_err(|_| NtsError::TooLarge)?;
        ef(&mut p, EF_AUTHENTICATOR, &body)?;
        Ok((p, Pending { unique_id, nonce }))
    }

    /// Verifies and unpacks a response. On success returns the server's time (unix seconds, milliseconds) and stores
    /// any new cookies. `Nak` means the server no longer knows our cookies; every other error means the packet must
    /// be ignored (it may be forged), not that the session is broken.
    pub fn handle_response(&mut self, buf: &[u8], pending: &Pending) -> Result<(u64, u16), NtsError> {
        if buf.len() < HEADER_LEN || buf.len() > MAX_PACKET {
            return Err(NtsError::BadResponse);
        }
        // NTS NAK: stratum 0, kiss code NTSN, and it does carry our identifier but no authenticator.
        let is_nak = buf[1] == 0 && &buf[12..16] == b"NTSN";
        let mut uid_ok = false;
        let mut auth: Option<(usize, &[u8], &[u8])> = None; // (offset of the EF, nonce, tag + ciphertext)
        let mut off = HEADER_LEN;
        while off < buf.len() {
            if off + 4 > buf.len() {
                return Err(NtsError::BadResponse);
            }
            let ty = u16::from_be_bytes([buf[off], buf[off + 1]]);
            let len = u16::from_be_bytes([buf[off + 2], buf[off + 3]]) as usize;
            if len < 4 || len % 4 != 0 || off + len > buf.len() {
                return Err(NtsError::BadResponse);
            }
            let body = &buf[off + 4..off + len];
            match ty {
                EF_UNIQUE_ID => uid_ok = body == pending.unique_id,
                EF_AUTHENTICATOR => {
                    if auth.is_some() || body.len() < 4 {
                        return Err(NtsError::BadResponse);
                    }
                    let nonce_len = u16::from_be_bytes([body[0], body[1]]) as usize;
                    let ct_len = u16::from_be_bytes([body[2], body[3]]) as usize;
                    let nonce_end = 4 + nonce_len.div_ceil(4) * 4;
                    let ct_end = nonce_end + ct_len.div_ceil(4) * 4;
                    if nonce_len == 0 || ct_len < TAG_LEN || ct_end > body.len() {
                        return Err(NtsError::BadResponse);
                    }
                    auth = Some((off, &body[4..4 + nonce_len], &body[nonce_end..nonce_end + ct_len]));
                    // anything after the authenticator is not covered by it: refuse to look at it
                    if off + len != buf.len() {
                        return Err(NtsError::BadResponse);
                    }
                }
                _ => {}
            }
            off += len;
        }
        if !uid_ok {
            return Err(NtsError::BadResponse);
        }
        if is_nak && auth.is_none() {
            return Err(NtsError::Nak);
        }
        let (auth_off, nonce, ct) = auth.ok_or(NtsError::BadResponse)?;
        // decrypt a copy: the response buffer is the caller's
        let mut plain: Vec<u8, MAX_PACKET> = Vec::new();
        plain.extend_from_slice(&ct[TAG_LEN..]).map_err(|_| NtsError::BadResponse)?;
        let tag = aes_siv::aead::generic_array::GenericArray::clone_from_slice(&ct[..TAG_LEN]);
        Aes128Siv::new(&self.s2c.into())
            .decrypt_in_place_detached([&buf[..auth_off], nonce], plain.as_mut_slice(), &tag)
            .map_err(|_| NtsError::AuthenticationFailed)?;

        // only after the authenticator checks out: the time, then the replacement cookies
        let time = sntp::parse_response(&buf[..HEADER_LEN], pending.nonce).map_err(NtsError::Time)?;
        let mut i = 0;
        while i + 4 <= plain.len() {
            let ty = u16::from_be_bytes([plain[i], plain[i + 1]]);
            let len = u16::from_be_bytes([plain[i + 2], plain[i + 3]]) as usize;
            if len < 4 || len % 4 != 0 || i + len > plain.len() {
                break; // a malformed encrypted field is dropped; the time and cookies so far stand
            }
            if ty == EF_COOKIE && len - 4 <= MAX_COOKIE && self.cookies.len() < MAX_COOKIES {
                let mut c = Cookie::new();
                if c.extend_from_slice(&plain[i + 4..i + len]).is_ok() && !c.is_empty() {
                    let _ = self.cookies.push(c);
                }
            }
            i += len;
        }
        Ok(time)
    }
}

/// Appends one extension field (RFC 7822): type, total length, body, padded to a multiple of 4.
fn ef(p: &mut Vec<u8, MAX_PACKET>, ty: u16, body: &[u8]) -> Result<(), NtsError> {
    let padded = body.len().div_ceil(4) * 4;
    let total = 4 + padded;
    if total > u16::MAX as usize || p.len() + total > MAX_PACKET {
        return Err(NtsError::TooLarge);
    }
    p.extend_from_slice(&ty.to_be_bytes()).map_err(|_| NtsError::TooLarge)?;
    p.extend_from_slice(&(total as u16).to_be_bytes()).map_err(|_| NtsError::TooLarge)?;
    p.extend_from_slice(body).map_err(|_| NtsError::TooLarge)?;
    for _ in body.len()..padded {
        p.push(0).map_err(|_| NtsError::TooLarge)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic generator for tests only.
    struct TestRng(u64);
    impl RngCore for TestRng {
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 11
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for b in dest {
                *b = self.next_u64() as u8;
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    fn rec(critical: bool, ty: u16, body: &[u8]) -> std::vec::Vec<u8> {
        let mut v = std::vec::Vec::new();
        v.extend_from_slice(&((if critical { 0x8000 } else { 0 }) | ty).to_be_bytes());
        v.extend_from_slice(&(body.len() as u16).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    fn good_ke(cookies: usize) -> std::vec::Vec<u8> {
        let mut m = rec(true, KE_NEXT_PROTOCOL, &[0, 0]);
        m.extend(rec(true, KE_AEAD, &[0, 15]));
        for n in 0..cookies {
            m.extend(rec(false, KE_COOKIE, &[n as u8; 104]));
        }
        m.extend(rec(true, KE_END, &[]));
        m
    }

    // VERIFIES: REQ-NTS-01
    #[test]
    fn ke_request_has_the_rfc_layout() {
        assert_eq!(
            ke_request(),
            [0x80, 0x01, 0x00, 0x02, 0x00, 0x00, /* NPN: NTPv4 */ 0x80, 0x04, 0x00, 0x02, 0x00, 0x0F, /* AEAD 15 */ 0x80, 0x00, 0x00, 0x00 /* end */]
        );
        assert_eq!(exporter_context(true), [0, 0, 0, 15, 0]);
        assert_eq!(exporter_context(false), [0, 0, 0, 15, 1]);
    }

    // VERIFIES: REQ-NTS-01
    #[test]
    fn parses_a_good_ke_response() {
        let mut m = good_ke(8);
        // a server and port record, an unknown non-critical record and a warning are fine
        let end = m.len() - 4;
        m.truncate(end);
        m.extend(rec(false, KE_SERVER, b"ntp1.example.net"));
        m.extend(rec(false, KE_PORT, &[0x10, 0x00]));
        m.extend(rec(false, 0x1234, &[1, 2, 3]));
        m.extend(rec(false, KE_WARNING, &[0, 1]));
        m.extend(rec(true, KE_END, &[]));
        assert_eq!(ke_message_len(&m), Ok(Some(m.len())));
        let r = parse_ke_response(&m).unwrap();
        assert_eq!(r.cookies.len(), 8);
        assert_eq!(r.cookies[3].as_slice(), &[3u8; 104]);
        assert_eq!(r.server.as_deref(), Some("ntp1.example.net"));
        assert_eq!(r.port, Some(4096));
        // more cookies than we keep is not an error
        assert_eq!(parse_ke_response(&good_ke(12)).unwrap().cookies.len(), MAX_COOKIES);
    }

    // VERIFIES: REQ-NTS-01
    #[test]
    fn rejects_bad_ke_responses() {
        let good = good_ke(2);
        assert!(parse_ke_response(&good).is_ok());
        // truncated at every length: never a panic, never accepted (the full message is the only accepted one)
        for n in 0..good.len() {
            assert!(parse_ke_response(&good[..n]).is_err(), "prefix of {n} bytes");
        }
        let mut bad = good.clone();
        bad.extend_from_slice(&[0, 0]); // a partial record after the end record
        assert_eq!(parse_ke_response(&bad), Err(NtsError::BadKeMessage));
        let mut bad = good.clone();
        bad.extend(rec(false, KE_COOKIE, &[7; 104])); // a complete record after the end record is not part of the message
        assert_eq!(parse_ke_response(&bad), Err(NtsError::BadKeMessage));

        let mk = |recs: &[std::vec::Vec<u8>]| recs.concat();
        let np = rec(true, KE_NEXT_PROTOCOL, &[0, 0]);
        let aead = rec(true, KE_AEAD, &[0, 15]);
        let cookie = rec(false, KE_COOKIE, &[9; 100]);
        let end = rec(true, KE_END, &[]);
        assert_eq!(parse_ke_response(&mk(&[rec(true, KE_ERROR, &[0, 2]), end.clone()])), Err(NtsError::ServerError(2)));
        assert_eq!(parse_ke_response(&mk(&[rec(true, KE_ERROR, &[0]), end.clone()])), Err(NtsError::ServerError(0xFFFF)));
        assert_eq!(parse_ke_response(&mk(&[np.clone(), rec(true, KE_AEAD, &[0, 17]), cookie.clone(), end.clone()])), Err(NtsError::NoAgreement), "other AEAD");
        assert_eq!(parse_ke_response(&mk(&[rec(true, KE_NEXT_PROTOCOL, &[0, 1]), aead.clone(), cookie.clone(), end.clone()])), Err(NtsError::NoAgreement), "other protocol");
        assert_eq!(parse_ke_response(&mk(&[rec(true, KE_NEXT_PROTOCOL, &[0, 0, 0, 1]), aead.clone(), cookie.clone(), end.clone()])), Err(NtsError::NoAgreement), "a list is not an agreement");
        assert_eq!(parse_ke_response(&mk(&[aead.clone(), cookie.clone(), end.clone()])), Err(NtsError::NoAgreement), "no next protocol");
        assert_eq!(parse_ke_response(&mk(&[np.clone(), cookie.clone(), end.clone()])), Err(NtsError::NoAgreement), "no AEAD");
        assert_eq!(parse_ke_response(&mk(&[np.clone(), aead.clone(), end.clone()])), Err(NtsError::NoCookies));
        assert_eq!(parse_ke_response(&mk(&[np.clone(), aead.clone(), cookie.clone(), rec(true, 0x4242, &[]), end.clone()])), Err(NtsError::UnknownCritical(0x4242)));
        assert_eq!(parse_ke_response(&mk(&[np.clone(), aead.clone(), rec(false, KE_COOKIE, &[1; 300]), end.clone()])), Err(NtsError::BadCookie), "too long");
        assert_eq!(parse_ke_response(&mk(&[np.clone(), aead.clone(), rec(false, KE_COOKIE, &[1; 101]), end.clone()])), Err(NtsError::BadCookie), "not a multiple of 4");
        assert_eq!(parse_ke_response(&mk(&[np.clone(), aead.clone(), cookie.clone(), rec(false, KE_SERVER, b"bad host"), end.clone()])), Err(NtsError::BadKeMessage));
        assert_eq!(parse_ke_response(&mk(&[np.clone(), aead.clone(), cookie.clone(), rec(false, KE_END, &[])])), Err(NtsError::BadKeMessage), "end record must be critical");
        assert_eq!(parse_ke_response(&mk(&[np, aead, cookie])), Err(NtsError::BadKeMessage), "no end record");
        // a length field that points past the data
        assert_eq!(parse_ke_response(&[0x80, 0x01, 0xFF, 0xFF, 0, 0]), Err(NtsError::BadKeMessage));
    }

    // VERIFIES: REQ-NTS-01
    #[test]
    fn ke_message_len_waits_for_the_end_record_and_is_bounded() {
        let m = good_ke(2);
        for n in 0..m.len() {
            assert_eq!(ke_message_len(&m[..n]), Ok(None), "{n}");
        }
        assert_eq!(ke_message_len(&m), Ok(Some(m.len())));
        let mut two = m.clone();
        two.extend_from_slice(&[1, 2, 3]); // trailing bytes after the end record are not part of the message
        assert_eq!(ke_message_len(&two), Ok(Some(m.len())));
        // a record that announces more than we accept fails at once instead of buffering
        assert_eq!(ke_message_len(&[0x00, 0x05, 0xFF, 0xF0]), Err(NtsError::BadKeMessage));
    }

    /// A server for the tests: knows the keys, authenticates a request and builds a response.
    struct TestServer {
        c2s: Key,
        s2c: Key,
    }
    impl TestServer {
        fn parse_request(&self, req: &[u8]) -> ([u8; UID_LEN], usize, usize, usize) {
            // returns (uid, cookie ef count, placeholder count, authenticator offset); verifies the authenticator
            let (mut uid, mut cookies, mut placeholders, mut auth_off) = ([0u8; UID_LEN], 0, 0, 0);
            let mut off = HEADER_LEN;
            while off < req.len() {
                let ty = u16::from_be_bytes([req[off], req[off + 1]]);
                let len = u16::from_be_bytes([req[off + 2], req[off + 3]]) as usize;
                match ty {
                    EF_UNIQUE_ID => uid.copy_from_slice(&req[off + 4..off + 4 + UID_LEN]),
                    EF_COOKIE => cookies += 1,
                    EF_COOKIE_PLACEHOLDER => placeholders += 1,
                    EF_AUTHENTICATOR => {
                        auth_off = off;
                        let body = &req[off + 4..off + len];
                        let (nl, cl) = (u16::from_be_bytes([body[0], body[1]]) as usize, u16::from_be_bytes([body[2], body[3]]) as usize);
                        let nonce = &body[4..4 + nl];
                        let ct = &body[4 + nl.div_ceil(4) * 4..][..cl];
                        let tag = aes_siv::aead::generic_array::GenericArray::clone_from_slice(&ct[..16]);
                        Aes128Siv::new(&self.c2s.into()).decrypt_in_place_detached([&req[..off], nonce], &mut [], &tag).expect("request authenticator");
                    }
                    _ => {}
                }
                off += len;
            }
            (uid, cookies, placeholders, auth_off)
        }

        fn respond(&self, req: &[u8], new_cookies: &[std::vec::Vec<u8>], nonce_echo: Option<[u8; 8]>) -> std::vec::Vec<u8> {
            let (uid, _, _, _) = self.parse_request(req);
            let mut p = std::vec::Vec::new();
            let mut h = [0u8; 48];
            h[0] = 0b00_100_100; // LI 0, v4, server
            h[1] = 2;
            h[24..32].copy_from_slice(&nonce_echo.unwrap_or_else(|| req[40..48].try_into().unwrap()));
            h[40..44].copy_from_slice(&(1_810_000_000u32.wrapping_add(2_208_988_800)).to_be_bytes());
            h[44..48].copy_from_slice(&0x8000_0000u32.to_be_bytes());
            p.extend_from_slice(&h);
            let put = |p: &mut std::vec::Vec<u8>, ty: u16, body: &[u8]| {
                p.extend_from_slice(&ty.to_be_bytes());
                p.extend_from_slice(&((4 + body.len().div_ceil(4) * 4) as u16).to_be_bytes());
                p.extend_from_slice(body);
                p.resize(p.len().div_ceil(4) * 4, 0);
            };
            put(&mut p, EF_UNIQUE_ID, &uid);
            let mut plain = std::vec::Vec::new();
            for c in new_cookies {
                put(&mut plain, EF_COOKIE, c);
            }
            let nonce = [7u8; 16];
            let tag = Aes128Siv::new(&self.s2c.into()).encrypt_in_place_detached([p.as_slice(), &nonce[..]], &mut plain).unwrap();
            let mut body = std::vec::Vec::new();
            body.extend_from_slice(&16u16.to_be_bytes());
            body.extend_from_slice(&((16 + plain.len()) as u16).to_be_bytes());
            body.extend_from_slice(&nonce);
            body.extend_from_slice(&tag);
            body.extend_from_slice(&plain);
            put(&mut p, EF_AUTHENTICATOR, &body);
            p
        }
    }

    fn setup() -> (Session, TestServer, TestRng) {
        let (c2s, s2c) = ([0x11u8; 32], [0x22u8; 32]);
        let ke = parse_ke_response(&good_ke(8)).unwrap();
        (Session::new(c2s, s2c, ke, "time.example.net").unwrap(), TestServer { c2s, s2c }, TestRng(42))
    }

    // VERIFIES: REQ-NTS-01
    #[test]
    fn request_is_well_formed_authenticated_and_uses_one_cookie() {
        let (mut s, server, mut rng) = setup();
        assert_eq!((s.server.as_str(), s.port, s.cookies_left()), ("time.example.net", 123, 8));
        let (req, pending) = s.build_request(&mut rng, [1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        assert_eq!(s.cookies_left(), 7);
        assert_eq!(req[0] & 0x3F, 0b100_011, "version 4, client mode");
        assert_eq!(&req[40..48], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(req.len() % 4 == 0 && req.len() <= MAX_PACKET);
        let (uid, cookies, placeholders, auth_off) = server.parse_request(&req); // panics if the authenticator is wrong
        assert_eq!(uid, pending.unique_id);
        assert_eq!(cookies, 1);
        assert_eq!(placeholders, 0, "the cookie field itself earns the replacement of the cookie it spends");
        assert!(auth_off > HEADER_LEN);
        // each request has a fresh identifier and nonce
        let (_req2, p2) = s.build_request(&mut rng, [9; 8]).unwrap();
        assert_ne!(p2.unique_id, pending.unique_id);
    }

    // VERIFIES: REQ-NTS-01 REQ-NTS-05
    #[test]
    fn a_good_response_gives_the_time_and_refills_cookies() {
        let (mut s, server, mut rng) = setup();
        for _ in 0..3 {
            let _ = s.build_request(&mut rng, [0; 8]).unwrap();
        }
        assert_eq!(s.cookies_left(), 5);
        let (req, pending) = s.build_request(&mut rng, [4, 4, 4, 4, 4, 4, 4, 4]).unwrap();
        let (_, _, placeholders, _) = server.parse_request(&req);
        assert_eq!(placeholders, 3, "four spent: the cookie field earns one, three placeholders earn the rest");
        let fresh: std::vec::Vec<_> = (0..4).map(|n| std::vec![0xA0 + n as u8; 104]).collect();
        let resp = server.respond(&req, &fresh, None);
        assert_eq!(s.handle_response(&resp, &pending), Ok((1_810_000_000, 500)));
        assert_eq!(s.cookies_left(), 4 + 4, "the four unspent plus four new ones");
    }

    // VERIFIES: REQ-NTS-02
    #[test]
    fn forged_or_mismatched_responses_are_refused() {
        let (mut s, server, mut rng) = setup();
        let (req, pending) = s.build_request(&mut rng, [4; 8]).unwrap();
        let good = server.respond(&req, &[std::vec![1u8; 104]], None);
        let before = s.cookies_left();

        // every single-bit flip anywhere in the response must be refused (header, identifier, authenticator)
        for i in 0..good.len() {
            let mut bad = good.clone();
            bad[i] ^= 0x01;
            assert!(s.handle_response(&bad, &pending).is_err(), "flip in byte {i} was accepted");
        }
        // anything appended after the authenticator is not covered by it, so it must not be tolerated
        let mut extended = good.clone();
        extended.extend_from_slice(&0x7777u16.to_be_bytes()); // a neutral type: only the position can make it wrong
        extended.extend_from_slice(&8u16.to_be_bytes());
        extended.extend_from_slice(&[0; 4]);
        assert_eq!(s.handle_response(&extended, &pending), Err(NtsError::BadResponse));
        // a second authenticator is refused too
        let mut doubled = good.clone();
        doubled.extend_from_slice(&good[good.len() - 4 - 4 - 16 - 108 - 16..]);
        assert!(s.handle_response(&doubled, &pending).is_err());
        // wrong keys on the server side
        let other = TestServer { c2s: [0; 32], s2c: [9; 32] };
        assert_eq!(s.handle_response(&other.respond_with_uid(&req, &pending), &pending), Err(NtsError::AuthenticationFailed));
        // a response to another request (different identifier)
        let (req2, pending2) = s.build_request(&mut rng, [5; 8]).unwrap();
        assert_eq!(s.handle_response(&server.respond(&req2, &[], None), &pending), Err(NtsError::BadResponse));
        let _ = pending2;
        // our nonce not echoed in the origin timestamp (replay of an old reply with a valid authenticator)
        let (req3, pending3) = s.build_request(&mut rng, [6; 8]).unwrap();
        let replay = server.respond(&req3, &[], Some([0xEE; 8]));
        assert_eq!(s.handle_response(&replay, &pending3), Err(NtsError::Time(sntp::SntpError::OriginMismatch)));
        // nothing a refused packet carried may reach the cookie store
        // (the cookies went into the two requests built since `before`, never into anything a refused packet carried)
        assert_eq!(s.cookies_left(), before - 2);
        // truncated, empty and oversized packets
        for n in [0, 10, 47, 48, 60, good.len() - 1] {
            assert!(s.handle_response(&good[..n], &pending).is_err(), "{n}");
        }
        assert_eq!(s.handle_response(&[0u8; MAX_PACKET + 4], &pending), Err(NtsError::BadResponse));
    }

    impl TestServer {
        /// A response made with this server's (wrong) keys but the request's real identifier.
        fn respond_with_uid(&self, req: &[u8], pending: &Pending) -> std::vec::Vec<u8> {
            let mut r = std::vec::Vec::new();
            let mut h = [0u8; 48];
            h[0] = 0b00_100_100;
            h[1] = 2;
            h[24..32].copy_from_slice(&pending.nonce);
            h[40..44].copy_from_slice(&1u32.to_be_bytes());
            r.extend_from_slice(&h);
            let _ = req;
            r.extend_from_slice(&EF_UNIQUE_ID.to_be_bytes());
            r.extend_from_slice(&36u16.to_be_bytes());
            r.extend_from_slice(&pending.unique_id);
            let nonce = [3u8; 16];
            let tag = Aes128Siv::new(&self.s2c.into()).encrypt_in_place_detached([r.as_slice(), &nonce[..]], &mut []).unwrap();
            r.extend_from_slice(&EF_AUTHENTICATOR.to_be_bytes());
            r.extend_from_slice(&40u16.to_be_bytes());
            r.extend_from_slice(&16u16.to_be_bytes());
            r.extend_from_slice(&16u16.to_be_bytes());
            r.extend_from_slice(&nonce);
            r.extend_from_slice(&tag);
            r
        }
    }

    // VERIFIES: REQ-NTS-02 REQ-NTS-05
    #[test]
    fn nak_and_missing_authenticator() {
        let (mut s, _server, mut rng) = setup();
        let (_req, pending) = s.build_request(&mut rng, [1; 8]).unwrap();
        // an NTS NAK echoes the identifier, has stratum 0 and kiss code NTSN, and no authenticator
        let mut nak = std::vec::Vec::new();
        let mut h = [0u8; 48];
        h[0] = 0b00_100_100;
        h[12..16].copy_from_slice(b"NTSN");
        nak.extend_from_slice(&h);
        nak.extend_from_slice(&EF_UNIQUE_ID.to_be_bytes());
        nak.extend_from_slice(&36u16.to_be_bytes());
        nak.extend_from_slice(&pending.unique_id);
        assert_eq!(s.handle_response(&nak, &pending), Err(NtsError::Nak));
        // an unauthenticated normal-looking response is just refused
        let mut plain = nak.clone();
        plain[1] = 2;
        plain[12..16].copy_from_slice(&[0; 4]);
        assert_eq!(s.handle_response(&plain, &pending), Err(NtsError::BadResponse));
        // a NAK for someone else's identifier is not a NAK for us
        let mut other = nak.clone();
        other[60] ^= 1;
        assert_eq!(s.handle_response(&other, &pending), Err(NtsError::BadResponse));
    }

    // VERIFIES: REQ-NTS-05
    #[test]
    fn running_out_of_cookies_is_reported() {
        let (mut s, _server, mut rng) = setup();
        for _ in 0..MAX_COOKIES {
            assert!(s.build_request(&mut rng, [0; 8]).is_ok());
        }
        assert_eq!(s.build_request(&mut rng, [0; 8]).err(), Some(NtsError::NoCookies));
    }

    // VERIFIES: REQ-NTS-01
    #[test]
    fn request_stays_within_one_packet_with_large_cookies() {
        let ke = {
            let mut m = rec(true, KE_NEXT_PROTOCOL, &[0, 0]);
            m.extend(rec(true, KE_AEAD, &[0, 15]));
            for n in 0..8 {
                m.extend(rec(false, KE_COOKIE, &[n; MAX_COOKIE]));
            }
            m.extend(rec(true, KE_END, &[]));
            parse_ke_response(&m).unwrap()
        };
        let mut s = Session::new([1; 32], [2; 32], ke, "x").unwrap();
        let (req, _) = s.build_request(&mut TestRng(1), [0; 8]).unwrap();
        assert!(req.len() <= MAX_PACKET, "{}", req.len());
        assert_eq!(req.len() % 4, 0);
    }
}
