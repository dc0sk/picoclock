//! Live check against public NTS servers: a real NTS-KE (TLS 1.3, certificate chain, exporter) and real authenticated
//! NTP exchanges. Needs internet access, so it is ignored by default:
//!
//!     cargo test -p picoclock-core --target <host> --test nts_live -- --ignored --nocapture
//!
//! A wrong exporter derivation, AEAD input or extension field layout makes the server drop or NAK the request, so a
//! passing run is strong evidence that the whole NTS client is right, which no offline test can give.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use embassy_crypto_rand as _;
use embassy_crypto_rustcrypto as _;
use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::TlsClock;
use picoclock_core::nts::{DriverRng, Session};
use picoclock_core::nts_ke::{self, KeError};
use rand_core::RngCore;

struct Now;
impl TlsClock for Now {
    fn now() -> Option<u64> {
        SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
    }
}

async fn session_for(host: &str) -> Session {
    let mut last = None;
    for root in nts_ke::roots_for(host) {
        let stream = tokio::net::TcpStream::connect((host, 4460)).await.expect("TCP connect to the NTS-KE port");
        let mut read_buf = vec![0u8; 16640];
        let mut write_buf = vec![0u8; 4096];
        match nts_ke::key_exchange::<_, Now>(FromTokio::new(stream), host, root, &mut read_buf, &mut write_buf).await {
            Ok(s) => return s,
            Err(e) => last = Some(e),
        }
    }
    panic!("NTS-KE with {host} failed with every root: {:?}", last.unwrap());
}

async fn exchange(host: &str) {
    let mut session = session_for(host).await;
    println!("{host}: NTS-KE ok, NTP server {}:{}, {} cookies", session.server, session.port, session.cookies_left());
    assert!(session.cookies_left() >= 1);
    let addr = tokio::net::lookup_host((session.server.as_str(), session.port)).await.unwrap().find(|a| a.is_ipv4()).expect("an IPv4 address");
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await.unwrap();
    let mut rng = DriverRng;
    for round in 1..=3 {
        let mut nonce = [0u8; 8];
        rng.fill_bytes(&mut nonce);
        let (request, pending) = session.build_request(&mut rng, nonce).unwrap();
        socket.send_to(&request, addr).await.unwrap();
        let mut buf = [0u8; 1500];
        let (n, from) = tokio::time::timeout(Duration::from_secs(5), socket.recv_from(&mut buf)).await.expect("no reply within 5 s").unwrap();
        assert_eq!(from.ip(), addr.ip());
        let (secs, ms) = session.handle_response(&buf[..n], &pending).unwrap_or_else(|e| panic!("{host} round {round}: {e:?} ({n} bytes)"));
        let local = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let diff = secs as i64 - local;
        println!("{host}: round {round}: server time {secs}.{ms:03}, offset {diff:+} s, cookies now {}", session.cookies_left());
        assert!(diff.abs() <= 3, "server time is {diff} s from this machine's clock");
    }
    // each request spends one cookie and the response brings one back, so the store stays full
    assert!(session.cookies_left() >= 7, "cookies were not replenished: {}", session.cookies_left());
}

#[tokio::test]
#[ignore = "needs internet access"]
async fn cloudflare() {
    exchange("time.cloudflare.com").await;
}

#[tokio::test]
#[ignore = "needs internet access"]
async fn time_nl() {
    exchange("nts.time.nl").await;
}

#[tokio::test]
#[ignore = "needs internet access"]
async fn an_rsa_server_is_refused_cleanly_not_trusted() {
    // The built-in verifier has no RSA: a server with an RSA certificate must fail with a TLS error, never connect.
    let stream = tokio::net::TcpStream::connect(("ptbtime1.ptb.de", 4460)).await.unwrap();
    let mut read_buf = vec![0u8; 16640];
    let mut write_buf = vec![0u8; 4096];
    let r = nts_ke::key_exchange::<_, Now>(FromTokio::new(stream), "ptbtime1.ptb.de", nts_ke::roots_for("ptbtime1.ptb.de")[0], &mut read_buf, &mut write_buf).await;
    assert!(matches!(r, Err(KeError::Tls(_))), "{:?}", r.err());
}
