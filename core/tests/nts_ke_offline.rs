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
    let mut sorted: Vec<_> = roots.iter().collect();
    sorted.dedup();
    assert_eq!(sorted.len(), roots.len(), "no root twice");
}
