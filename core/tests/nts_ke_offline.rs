//! Offline checks of the NTS-KE client's preconditions.

// the crypto drivers embedded-tls calls through embassy-crypto are linked, not passed
use embassy_crypto_rand as _;
use embassy_crypto_rustcrypto as _;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_tls::TlsClock;
use picoclock_core::nts_ke::{self, KeError};

/// A socket that fails the test if anything touches it.
struct Untouchable;
impl ErrorType for Untouchable {
    type Error = std::io::Error;
}
impl Read for Untouchable {
    async fn read(&mut self, _: &mut [u8]) -> Result<usize, Self::Error> {
        panic!("the socket was read although there is no valid time");
    }
}
impl Write for Untouchable {
    async fn write(&mut self, _: &[u8]) -> Result<usize, Self::Error> {
        panic!("the socket was written although there is no valid time");
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        panic!("the socket was flushed although there is no valid time");
    }
}

struct NoTime;
impl TlsClock for NoTime {
    fn now() -> Option<u64> {
        None
    }
}

// VERIFIES: REQ-NTS-03
#[tokio::test]
async fn no_key_exchange_without_a_valid_time() {
    // embedded-tls skips the certificate date check when the clock has no time, so the client must refuse up front,
    // before a single byte is sent.
    let mut read_buf = vec![0u8; 16640];
    let mut write_buf = vec![0u8; 4096];
    let r = nts_ke::key_exchange::<_, NoTime>(Untouchable, "time.cloudflare.com", nts_ke::BUILT_IN_ROOTS, &mut read_buf, &mut write_buf).await;
    assert!(matches!(r, Err(KeError::NoTime)), "{:?}", r.err());
}

// VERIFIES: REQ-NTS-01
#[test]
fn built_in_roots_are_the_known_certificates() {
    // the two ECDSA roots always; the RSA root only in builds that can verify RSA chains
    let roots = nts_ke::BUILT_IN_ROOTS;
    assert_eq!(roots.len(), if cfg!(feature = "rsa") { 3 } else { 2 });
    for r in roots {
        assert!(r.len() > 400 && r[0] == 0x30, "a DER certificate");
    }
    // no root twice, and each one is the certificate its comment in nts_ke.rs names (a swapped file must not pass)
    let fps: Vec<String> = roots
        .iter()
        .map(|r| picoclock_core::x509::fingerprint_hex(&picoclock_core::x509::fingerprint(r)).to_string())
        .collect();
    let mut unique = fps.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), roots.len(), "no root twice");
    let known = [
        "69:72:9B:8E:15:A8:6E:FC:17:7A:57:AF:B7:17:1D:FC:64:AD:D2:8C:2F:CA:8C:F1:50:7E:34:45:3C:CB:14:70", // ISRG Root X2
        "34:17:BB:06:CC:60:07:DA:1B:96:1C:92:0B:8A:B4:CE:3F:AD:82:0E:4A:A3:0B:9A:CB:C4:A7:4E:BD:CE:BC:65", // SSL.com ECC
        "96:BC:EC:06:26:49:76:F3:74:60:77:9A:CF:28:C5:A7:CF:E8:A3:C0:AA:E1:1A:8F:FC:EE:05:C0:BD:DF:08:C6", // ISRG Root X1 (rsa)
    ];
    for fp in &fps {
        assert!(known.contains(&fp.as_str()), "unknown built-in root {fp}");
    }
}

// VERIFIES: REQ-UPD-03
#[test]
fn the_update_check_trusts_exactly_one_root_and_it_is_not_an_nts_root() {
    let roots = nts_ke::UPDATE_ROOTS;
    assert_eq!(roots.len(), 1);
    let fp = picoclock_core::x509::fingerprint_hex(&picoclock_core::x509::fingerprint(roots[0])).to_string();
    assert_eq!(fp, "4F:F4:60:D5:4B:9C:86:DA:BF:BC:FC:57:12:E0:40:0D:2B:ED:3F:BC:4D:4F:BD:AA:86:E0:6A:DC:D2:A9:AD:7A", "USERTrust ECC Certification Authority");
    // and NTS does not trust it: the two lists are separate
    assert!(!nts_ke::BUILT_IN_ROOTS.contains(&roots[0]));
}
