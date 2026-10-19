//! The streaming download client (`nts_ke::https_stream`) against `openssl s_server` with certificate chains made on the spot:
//! the body arrives piece by piece and complete, a redirect is returned and not followed, a short, long or chunked answer is
//! refused, and a server that does not chain to the given root is refused even though its bytes were already passed on.
//! Needs the `openssl` binary; skipped (with a message) when it is missing.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::TlsClock;
use picoclock_core::nts_ke::{self, KeError};
// the crypto drivers embedded-tls calls through embassy-crypto are linked, not passed
use embassy_crypto_rand as _;
use embassy_crypto_rustcrypto as _;

struct Now;
impl TlsClock for Now {
    fn now() -> Option<u64> {
        SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
    }
}

struct KillOnDrop(std::process::Child, Option<std::process::ChildStdin>);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn openssl(dir: &std::path::Path, args: &[&str]) -> bool {
    Command::new("openssl").current_dir(dir).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn der_of(dir: &std::path::Path, pem: &str) -> Vec<u8> {
    let out = Command::new("openssl").current_dir(dir).args(["x509", "-in", pem, "-outform", "DER"]).output().unwrap();
    assert!(out.status.success());
    out.stdout
}

/// A server (`openssl s_server`) with a root -> intermediate -> leaf chain of the given key type, plus an unrelated root.
struct Server {
    dir: std::path::PathBuf,
    _child: KillOnDrop,
    port: u16,
}

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn start(rsa: bool, answer: &[u8], hang_up: bool) -> Option<Server> {
    let dir = std::env::temp_dir().join(format!("picoclock-ke-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ext.cnf"), "subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n").unwrap();
    std::fs::write(dir.join("ca.cnf"), "basicConstraints=critical,CA:TRUE,pathlen:1\nkeyUsage=critical,keyCertSign\n").unwrap();
    std::fs::write(dir.join("inter.cnf"), "basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign\n").unwrap();
    let key = |name: &str| -> bool {
        if rsa { openssl(&dir, &["genrsa", "-out", &format!("{name}.key"), "2048"]) } else { openssl(&dir, &["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", &format!("{name}.key")]) }
    };
    let root = |name: &str, cn: &str| key(name) && openssl(&dir, &["req", "-x509", "-new", "-key", &format!("{name}.key"), "-sha256", "-days", "2", "-subj", cn, "-addext", "basicConstraints=critical,CA:TRUE,pathlen:1", "-addext", "keyUsage=critical,keyCertSign", "-out", &format!("{name}.pem")]);
    let sign = |name: &str, cn: &str, issuer: &str, ext: &str| {
        key(name)
            && openssl(&dir, &["req", "-new", "-key", &format!("{name}.key"), "-subj", cn, "-out", &format!("{name}.csr")])
            && openssl(&dir, &["x509", "-req", "-in", &format!("{name}.csr"), "-CA", &format!("{issuer}.pem"), "-CAkey", &format!("{issuer}.key"), "-CAcreateserial", "-days", "2", "-sha256", "-extfile", ext, "-out", &format!("{name}.pem")])
    };
    let ok = root("ca", "/CN=ke-test-ca") && root("other", "/CN=ke-other-ca") && sign("inter", "/CN=ke-test-intermediate", "ca", "inter.cnf") && sign("server", "/CN=localhost", "inter", "ext.cnf");
    if !ok {
        eprintln!("SKIPPED: the openssl binary is not available or could not create the test certificates");
        return None;
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new("openssl")
        .current_dir(&dir)
        .args(["s_server", "-accept", &port.to_string(), "-cert", "server.pem", "-cert_chain", "inter.pem", "-key", "server.key", "-tls1_3", "-ciphersuites", "TLS_AES_128_GCM_SHA256", "-groups", "P-256", "-naccept", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("server.err")).unwrap())
        .spawn()
        .expect("start openssl s_server");
    let mut stdin = child.stdin.take().unwrap();
    // what the server says after the handshake: s_server sends its stdin to the client; the pipe stays open for the test
    stdin.write_all(answer).unwrap();
    stdin.flush().unwrap();
    // the pipe stays open (the connection stays up) unless the test wants the server to hang up after the answer
    Some(Server { dir, _child: KillOnDrop(child, if hang_up { None } else { Some(stdin) }), port })
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Collect {
    head: Option<picoclock_core::download::Head>,
    bytes: Vec<u8>,
    follow: bool,
    refuse_body_after: usize,
}

impl nts_ke::StreamSink for Collect {
    async fn head(&mut self, head: &picoclock_core::download::Head) -> bool {
        self.head = Some(head.clone());
        self.follow
    }
    async fn body(&mut self, data: &[u8]) -> bool {
        self.bytes.extend_from_slice(data);
        self.bytes.len() <= self.refuse_body_after
    }
}

fn http(status: &str, extra: &str, body: &[u8], length: usize) -> Vec<u8> {
    let mut v = format!("HTTP/1.1 {status}\r\n{extra}Content-Length: {length}\r\n\r\n").into_bytes();
    v.extend_from_slice(body);
    v
}

async fn fetch(rsa: bool, answer: Vec<u8>, roots: &[&str], host: &str, hang_up: bool, follow: bool, refuse_after: usize) -> Option<(Result<picoclock_core::download::Head, KeError>, Collect)> {
    let server = start(rsa, &answer, hang_up)?;
    let ders: Vec<Vec<u8>> = roots.iter().map(|r| der_of(&server.dir, r)).collect();
    let roots: Vec<&[u8]> = ders.iter().map(|d| &d[..]).collect();
    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = tokio::net::TcpStream::connect(("127.0.0.1", server.port)).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut read_buf = vec![0u8; 16640];
    let mut write_buf = vec![0u8; 2048];
    let mut sink = Collect { head: None, bytes: Vec::new(), follow, refuse_body_after: refuse_after };
    let r = nts_ke::https_stream::<_, Now, _>(FromTokio::new(stream.expect("openssl s_server did not start listening")), host, b"GET / HTTP/1.1\r\n\r\n", &roots, &mut read_buf, &mut write_buf, &mut sink).await;
    Some((r, sink))
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
}

async fn check(rsa: bool) {
    let file = pattern(150_000);
    // the whole file, exactly once and in order
    if let Some((r, sink)) = fetch(rsa, http("200 OK", "Content-Type: application/octet-stream\r\n", &file, file.len()), &["ca.pem"], "localhost", false, true, usize::MAX).await {
        let head = r.expect("a good download");
        assert_eq!((head.status, head.content_length), (200, Some(file.len())));
        assert!(sink.bytes == file, "the body arrived intact ({} of {} bytes)", sink.bytes.len(), file.len());
    }
    // a redirect with an oversized header: handed back, nothing read as body, certificate still checked
    let huge = format!("content-security-policy: {}\r\n", "x".repeat(6000));
    let redirect = http("302 Found", &format!("Location: https://release-assets.githubusercontent.com/a?b=c\r\n{huge}"), b"", 0);
    if let Some((r, sink)) = fetch(rsa, redirect.clone(), &["ca.pem"], "localhost", false, false, usize::MAX).await {
        let head = r.expect("a redirect is an answer");
        assert_eq!((head.status, head.location.as_deref()), (302, Some("https://release-assets.githubusercontent.com/a?b=c")));
        assert!(sink.bytes.is_empty());
    }
    // the same redirect from a server the clock does not trust is not an answer
    if let Some((r, _)) = fetch(rsa, redirect, &["other.pem"], "localhost", false, false, usize::MAX).await {
        assert!(matches!(r, Err(KeError::Tls(_))), "{r:?}");
    }
    // an untrusted server: the bytes were passed on before the check, but the call fails
    if let Some((r, sink)) = fetch(rsa, http("200 OK", "", &file[..20_000], 20_000), &["other.pem"], "localhost", false, true, usize::MAX).await {
        assert!(matches!(r, Err(KeError::Tls(_))), "{r:?}");
        assert_eq!(sink.bytes.len(), 20_000, "documented: the sink sees bytes before the server is authenticated");
    }
    // the wrong name
    if let Some((r, _)) = fetch(rsa, http("200 OK", "", &file[..1000], 1000), &["ca.pem"], "not-localhost.example", false, true, usize::MAX).await {
        assert!(matches!(r, Err(KeError::Tls(_))), "{r:?}");
    }
    // shorter than announced (the server hangs up), longer than announced, chunked, no length
    if let Some((r, _)) = fetch(rsa, http("200 OK", "", &file[..5000], 9000), &["ca.pem"], "localhost", true, true, usize::MAX).await {
        assert!(r.is_err(), "short: {r:?}");
    }
    if let Some((r, _)) = fetch(rsa, http("200 OK", "", &file[..5000], 3000), &["ca.pem"], "localhost", true, true, usize::MAX).await {
        assert!(matches!(r, Err(KeError::Closed)), "long: {r:?}");
    }
    if let Some((r, _)) = fetch(rsa, b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n".to_vec(), &["ca.pem"], "localhost", true, true, usize::MAX).await {
        assert!(matches!(r, Err(KeError::Closed)), "chunked: {r:?}");
    }
    if let Some((r, _)) = fetch(rsa, b"HTTP/1.1 200 OK\r\n\r\nhello".to_vec(), &["ca.pem"], "localhost", true, true, usize::MAX).await {
        assert!(matches!(r, Err(KeError::Closed)), "no length: {r:?}");
    }
    // the sink cannot keep the bytes
    if let Some((r, _)) = fetch(rsa, http("200 OK", "", &file[..40_000], 40_000), &["ca.pem"], "localhost", false, true, 10_000).await {
        assert!(matches!(r, Err(KeError::Closed)), "sink refused: {r:?}");
    }
}

// VERIFIES: REQ-UPD-09
#[tokio::test]
async fn the_download_streams_complete_and_is_refused_when_anything_is_off_ecdsa() {
    check(false).await;
}

// VERIFIES: REQ-UPD-09
#[cfg(feature = "rsa")]
#[tokio::test]
async fn the_download_streams_complete_and_is_refused_when_anything_is_off_rsa() {
    check(true).await;
}
