//! Minimal SNTPv4 client packet handling (RFC 4330). Transport lives in the firmware.

pub const PACKET_LEN: usize = 48;
const NTP_TO_UNIX: u64 = 2_208_988_800;

#[derive(Debug, PartialEq, Eq)]
pub enum SntpError {
    TooShort,
    NotServerReply,
    /// Server clock unsynchronised (LI = 3) or stratum 0 (kiss-o'-death).
    Unsynchronised,
    /// The reply does not echo our transmit timestamp (stale or spoofed).
    OriginMismatch,
    NoTimestamp,
}

/// Builds a client request. `nonce` goes in the transmit timestamp and must come back as the origin timestamp.
pub fn build_request(nonce: [u8; 8]) -> [u8; PACKET_LEN] {
    let mut p = [0u8; PACKET_LEN];
    p[0] = 0b00_100_011; // LI 0, VN 4, mode 3 (client)
    p[40..48].copy_from_slice(&nonce);
    p
}

/// Returns the server transmit time as (unix seconds, milliseconds).
pub fn parse_response(buf: &[u8], nonce: [u8; 8]) -> Result<(u64, u16), SntpError> {
    if buf.len() < PACKET_LEN {
        return Err(SntpError::TooShort);
    }
    if buf[0] & 0x07 != 4 {
        return Err(SntpError::NotServerReply);
    }
    if buf[0] >> 6 == 3 || buf[1] == 0 {
        return Err(SntpError::Unsynchronised);
    }
    if buf[24..32] != nonce {
        return Err(SntpError::OriginMismatch);
    }
    let secs = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]) as u64;
    let frac = u32::from_be_bytes([buf[44], buf[45], buf[46], buf[47]]) as u64;
    if secs == 0 {
        return Err(SntpError::NoTimestamp);
    }
    // Era 0 ends in 2036; a clear MSB means the timestamp is in era 1.
    let ntp = if secs & 0x8000_0000 == 0 { secs + (1 << 32) } else { secs };
    Ok((ntp - NTP_TO_UNIX, ((frac * 1000) >> 32) as u16))
}

/// Stratum we claim when synchronised. The upstream stratum is not tracked, so this is deliberately high: a client
/// that also has a better source will prefer that one.
const SERVER_STRATUM: u8 = 5;

/// Builds the reply to a client request, or `None` if the packet is not a client request (so it is ignored).
///
/// `now_unix` + `now_frac` (NTP 32-bit fraction) is the current UTC. The clock free-runs between hourly syncs, so the
/// reply advertises 0.25 s root dispersion and ~15 ms precision. When `synced` is false (no NTP sync within the last
/// day) the reply says "unsynchronised" (LI 3, stratum 16), which clients discard.
pub fn build_server_reply(req: &[u8], now_unix: i64, now_frac: u32, last_sync_unix: Option<i64>, synced: bool) -> Option<[u8; PACKET_LEN]> {
    if req.len() < PACKET_LEN || req[0] & 0x07 != 3 {
        return None;
    }
    let version = (req[0] >> 3) & 0x07;
    if !(1..=4).contains(&version) {
        return None;
    }
    let ntp = |unix: i64| -> [u8; 4] { ((unix + NTP_TO_UNIX as i64) as u32).to_be_bytes() }; // wraps with the era
    let mut p = [0u8; PACKET_LEN];
    let (li, stratum) = if synced { (0, SERVER_STRATUM) } else { (3, 16) };
    p[0] = (li << 6) | (version << 3) | 4; // server mode
    p[1] = stratum;
    p[2] = req[2].clamp(4, 10); // poll: echo, kept sane
    p[3] = 0xFA; // precision 2^-6 s
    p[8..12].copy_from_slice(&0x0000_4000u32.to_be_bytes()); // root dispersion: 0.25 s (16.16)
    if let Some(last) = last_sync_unix.filter(|_| synced) {
        p[16..20].copy_from_slice(&ntp(last));
    }
    p[24..32].copy_from_slice(&req[40..48]); // originate = client's transmit
    for at in [32, 40] {
        // receive and transmit
        p[at..at + 4].copy_from_slice(&ntp(now_unix));
        p[at + 4..at + 8].copy_from_slice(&now_frac.to_be_bytes());
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(nonce: [u8; 8], secs: u32, frac: u32) -> [u8; PACKET_LEN] {
        let mut p = [0u8; PACKET_LEN];
        p[0] = 0b00_100_100;
        p[1] = 2;
        p[24..32].copy_from_slice(&nonce);
        p[40..44].copy_from_slice(&secs.to_be_bytes());
        p[44..48].copy_from_slice(&frac.to_be_bytes());
        p
    }

    // VERIFIES: REQ-NTP-01
    #[test]
    fn request_layout() {
        let r = build_request([1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(r[0], 0x23);
        assert_eq!(&r[40..], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    // VERIFIES: REQ-NTP-01
    #[test]
    fn parses_valid_reply() {
        let n = [9; 8];
        // 2023-11-14 22:13:20 UTC is unix 1_700_000_000.
        let r = reply(n, (1_700_000_000u64 + NTP_TO_UNIX) as u32, 1 << 31);
        assert_eq!(parse_response(&r, n), Ok((1_700_000_000, 500)));
    }

    // VERIFIES: REQ-TIME-03
    #[test]
    fn era_one_rollover() {
        let n = [1; 8];
        // 2040-01-01 00:00:00 UTC = 2_208_988_800 NTP offset + 2_208_988_800 unix.
        let unix = 2_208_988_800u64;
        let r = reply(n, ((unix + NTP_TO_UNIX) - (1 << 32)) as u32, 0);
        assert_eq!(parse_response(&r, n), Ok((unix, 0)));
    }

    // VERIFIES: REQ-NTP-02
    #[test]
    fn rejects_bad_replies() {
        let n = [7; 8];
        let ok = reply(n, 3_900_000_000, 0);
        assert_eq!(parse_response(&ok[..47], n), Err(SntpError::TooShort));
        assert_eq!(parse_response(&ok, [8; 8]), Err(SntpError::OriginMismatch));
        let mut b = ok;
        b[0] = 0x23; // client mode
        assert_eq!(parse_response(&b, n), Err(SntpError::NotServerReply));
        let mut b = ok;
        b[1] = 0; // kiss-o'-death
        assert_eq!(parse_response(&b, n), Err(SntpError::Unsynchronised));
        let mut b = ok;
        b[0] |= 0xC0; // LI = 3
        assert_eq!(parse_response(&b, n), Err(SntpError::Unsynchronised));
        let mut b = ok;
        b[40..44].fill(0);
        assert_eq!(parse_response(&b, n), Err(SntpError::NoTimestamp));
    }

    /// Talks to a real server. Run with `cargo test -p picoclock-core --target <host> -- --ignored`.
    #[test]
    #[ignore = "needs network access"]
    fn live_server() {
        use std::net::UdpSocket;
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let sock = UdpSocket::bind("0.0.0.0:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.connect("pool.ntp.org:123").unwrap();
        let nonce = [0xA5, 1, 2, 3, 4, 5, 6, 7];
        sock.send(&build_request(nonce)).unwrap();
        let mut buf = [0u8; 128];
        let n = sock.recv(&mut buf).unwrap();
        let (secs, _) = parse_response(&buf[..n], nonce).unwrap();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(secs.abs_diff(now) < 5, "server {secs} vs local {now}");
    }

    // VERIFIES: REQ-SRV-02
    #[test]
    fn server_reply_is_accepted_by_our_own_client_and_echoes_the_nonce() {
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8];
        let req = build_request(nonce);
        let r = build_server_reply(&req, 1_720_000_000, 0x8000_0000, Some(1_719_990_000), true).unwrap();
        assert_eq!(r[0] & 0x07, 4);
        assert_eq!(r[1], 5);
        assert_eq!(parse_response(&r, nonce), Ok((1_720_000_000, 500)));
        // a client holding a different nonce must reject it
        assert_eq!(parse_response(&r, [9; 8]), Err(SntpError::OriginMismatch));
    }

    // VERIFIES: REQ-SRV-03
    #[test]
    fn unsynchronised_server_is_rejected_by_clients() {
        let req = build_request([7; 8]);
        let r = build_server_reply(&req, 1_720_000_000, 0, None, false).unwrap();
        assert_eq!((r[0] >> 6, r[1]), (3, 16));
        assert_eq!(parse_response(&r, [7; 8]), Err(SntpError::Unsynchronised));
    }

    // VERIFIES: REQ-SRV-03 REQ-SRV-02
    #[test]
    fn server_ignores_everything_that_is_not_a_client_request() {
        let mut req = build_request([0; 8]);
        assert!(build_server_reply(&req[..47], 1, 0, None, true).is_none(), "short");
        req[0] = 0b00_100_100; // a server reply (mode 4): never answer replies, no ping-pong
        assert!(build_server_reply(&req, 1, 0, None, true).is_none());
        req[0] = 0b00_100_001; // symmetric active
        assert!(build_server_reply(&req, 1, 0, None, true).is_none());
        req[0] = 0b00_000_011; // version 0
        assert!(build_server_reply(&req, 1, 0, None, true).is_none());
        req[0] = 0b00_111_011; // version 7
        assert!(build_server_reply(&req, 1, 0, None, true).is_none());
        req[0] = 0b00_011_011; // version 3 is answered in version 3
        assert_eq!(build_server_reply(&req, 1, 0, None, true).unwrap()[0] & 0x38, 3 << 3);
    }

    // VERIFIES: REQ-SRV-03
    #[test]
    fn server_reply_is_never_larger_than_the_request() {
        let mut big = [0u8; 200];
        big[0] = 0b00_100_011;
        assert_eq!(build_server_reply(&big, 1_720_000_000, 0, None, true).unwrap().len(), PACKET_LEN);
    }
}
