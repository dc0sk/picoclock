//! Live check of the update check against GitHub: the real TLS 1.3 handshake, the real certificate chain (Sectigo, ending at the
//! pinned USERTrust ECC root) and the real JSON. Needs internet access, so it is ignored by default:
//!
//!     cargo test -p picoclock-core --target <host> --test update_live -- --ignored --nocapture

use std::time::{SystemTime, UNIX_EPOCH};

use embassy_crypto_rand as _;
use embassy_crypto_rustcrypto as _;
use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::TlsClock;
use picoclock_core::nts_ke::{self, KeError};
use picoclock_core::update::{self, Outcome, Version};

struct Now;
impl TlsClock for Now {
    fn now() -> Option<u64> {
        SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
    }
}

async fn get(host: &str, request: &[u8], roots: &[&[u8]]) -> Result<Vec<u8>, KeError> {
    let stream = tokio::net::TcpStream::connect((host, 443)).await.expect("TCP connect to port 443");
    let (mut read_buf, mut write_buf, mut response) = (vec![0u8; 16640], vec![0u8; 2048], vec![0u8; 4096]);
    let n = nts_ke::https_get::<_, Now>(FromTokio::new(stream), host, request, roots, &mut read_buf, &mut write_buf, &mut response).await?;
    response.truncate(n);
    Ok(response)
}

// (live evidence for REQ-UPD-03, recorded in the verification log)
#[tokio::test]
#[ignore = "needs internet access"]
async fn github_answers_over_the_pinned_root() {
    let answer = get(update::HOST, update::request().as_bytes(), nts_ke::UPDATE_ROOTS).await.expect("TLS to api.github.com with the pinned root");
    let text = String::from_utf8_lossy(&answer);
    println!("{}", text.lines().next().unwrap_or(""));
    // no release exists yet (404) or one does (200 with a version): both are understood, neither is "unreadable"
    let outcome = update::parse_response(&answer, Version::running());
    println!("outcome: {outcome:?}");
    assert_ne!(outcome, Outcome::Unreadable, "{}", &text[..text.len().min(300)]);
}

// (live evidence for REQ-UPD-03, recorded in the verification log)
#[tokio::test]
#[ignore = "needs internet access"]
async fn the_pinned_root_is_the_only_one_and_a_wrong_root_refuses() {
    // GitHub's chain does not end at the NTS roots, so with those the connection must fail and no answer be returned
    let r = get(update::HOST, update::request().as_bytes(), nts_ke::BUILT_IN_ROOTS).await;
    assert!(matches!(r, Err(KeError::Tls(_))), "{:?}", r.map(|v| v.len()));
    // the host name must match too: api.github.com's chain is no good for another name
    let stream = tokio::net::TcpStream::connect((update::HOST, 443)).await.unwrap();
    let (mut read_buf, mut write_buf, mut response) = (vec![0u8; 16640], vec![0u8; 2048], vec![0u8; 4096]);
    let r = nts_ke::https_get::<_, Now>(FromTokio::new(stream), "example.org", update::request().as_bytes(), nts_ke::UPDATE_ROOTS, &mut read_buf, &mut write_buf, &mut response).await;
    assert!(matches!(r, Err(KeError::Tls(_))), "a certificate for another name must be refused: {:?}", r.ok());
}

// (live evidence for REQ-UPD-03, recorded in the verification log)
#[ignore = "needs internet access"]
#[tokio::test]
async fn a_real_release_answer_is_read_from_the_first_kilobytes() {
    // a repository that has releases (its tag need not parse as a version; the point is that the answer is complete and the tag found)
    let request = "GET /repos/rust-lang/rust/releases/latest HTTP/1.1\r\nHost: api.github.com\r\nUser-Agent: picoclock-test\r\nAccept: application/vnd.github+json\r\nConnection: close\r\n\r\n";
    let answer = get(update::HOST, request.as_bytes(), nts_ke::UPDATE_ROOTS).await.expect("TLS");
    let outcome = update::parse_response(&answer, Version { major: 0, minor: 0, patch: 1 });
    println!("rust-lang/rust latest: {outcome:?} after {} bytes", answer.len());
    assert!(matches!(outcome, Outcome::Newer(_)), "{outcome:?}\n{}", String::from_utf8_lossy(&answer[..answer.len().min(400)]));
}
