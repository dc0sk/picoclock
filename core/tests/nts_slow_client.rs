//! Experiment: how long may the client take for its part of the TLS handshake before an NTS-KE server gives up?
//! `TlsClock::now()` is called while the server certificate is verified, so sleeping there simulates a slow CPU.
//!
//!     cargo test -p picoclock-core --target <host> --test nts_slow_client -- --ignored --nocapture

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use embassy_crypto_rand as _;
use embassy_crypto_rustcrypto as _;
use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::TlsClock;
use picoclock_core::nts_ke;

static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DELAY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct SlowNow;
impl TlsClock for SlowNow {
    fn now() -> Option<u64> {
        // The first call is the "do we have a time" check before the connection is used; only the calls made while the
        // certificate is verified are slowed down.
        if CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 1 {
            std::thread::sleep(Duration::from_millis(DELAY_MS.load(std::sync::atomic::Ordering::Relaxed)));
        }
        SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
    }
}

async fn try_with_delay(host: &str, delay_ms: u64) -> String {
    DELAY_MS.store(delay_ms, std::sync::atomic::Ordering::Relaxed);
    let mut out = String::new();
    CALLS.store(0, std::sync::atomic::Ordering::Relaxed);
    let started = Instant::now();
    let stream = tokio::net::TcpStream::connect((host, 4460)).await.unwrap();
    let mut rb = vec![0u8; 16640];
    let mut wb = vec![0u8; 4096];
    let r = nts_ke::key_exchange::<_, SlowNow>(FromTokio::new(stream), host, nts_ke::BUILT_IN_ROOTS, &mut rb, &mut wb).await;
    let calls = CALLS.load(std::sync::atomic::Ordering::Relaxed);
    out += &format!(" [{} after {} ms, now() called {calls}x]", if r.is_ok() { "ok".to_string() } else { format!("{:?}", r.as_ref().err().unwrap()) }, started.elapsed().as_millis());
    format!("delay {delay_ms:5} ms ->{out}")
}

#[tokio::test]
#[ignore = "needs internet access; experiment"]
async fn how_slow_may_the_client_be() {
    for host in ["time.cloudflare.com", "nts.time.nl", "ntp3.fau.de", "nts.teambelgium.net"] {
        for d in [0, 2500, 5000] {
            println!("{host}: {}", try_with_delay(host, d).await);
        }
    }
}
