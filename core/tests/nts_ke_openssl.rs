//! The whole NTS-KE client (`nts_ke::key_exchange`) against `openssl s_server` with certificate chains made on the spot:
//! it must trust a server whose chain ends at any root it is given, and no other. Runs the ECDSA hierarchy always and the
//! RSA one when the `rsa` feature is on (cargo test --features rsa, as on the Pico 2 W). Needs the `openssl` binary; skipped
//! (with a message) when it is missing.

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

/// An NTS-KE response (RFC 8915 section 4.1.2): NTPv4, AES-SIV-CMAC-256, eight cookies, end of message.
fn ke_response() -> Vec<u8> {
    let mut m = Vec::new();
    let mut rec = |ty: u16, critical: bool, body: &[u8]| {
        m.extend_from_slice(&(ty | if critical { 0x8000 } else { 0 }).to_be_bytes());
        m.extend_from_slice(&(body.len() as u16).to_be_bytes());
        m.extend_from_slice(body);
    };
    rec(1, true, &[0, 0]);
    rec(4, true, &[0, 15]);
    for i in 0..8u8 {
        rec(5, false, &[i + 1; 100]);
    }
    rec(0, true, &[]);
    m
}

/// A server (`openssl s_server`) with a root -> intermediate -> leaf chain of the given key type, plus an unrelated root.
struct Server {
    dir: std::path::PathBuf,
    _child: KillOnDrop,
    port: u16,
}

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn start(rsa: bool) -> Option<Server> {
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
        .args(["s_server", "-accept", &port.to_string(), "-cert", "server.pem", "-cert_chain", "inter.pem", "-key", "server.key", "-tls1_3", "-ciphersuites", "TLS_AES_128_GCM_SHA256", "-groups", "P-256", "-alpn", "ntske/1", "-naccept", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("server.err")).unwrap())
        .spawn()
        .expect("start openssl s_server");
    let mut stdin = child.stdin.take().unwrap();
    // what the server says after the handshake: s_server sends its stdin to the client; the pipe stays open for the test
    stdin.write_all(&ke_response()).unwrap();
    stdin.flush().unwrap();
    Some(Server { dir, _child: KillOnDrop(child, Some(stdin)), port })
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn key_exchange(rsa: bool, roots: &[&str], host: &str) -> Option<Result<(), KeError>> {
    let server = start(rsa)?;
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
    let mut write_buf = vec![0u8; 4096];
    let r = nts_ke::key_exchange::<_, Now>(FromTokio::new(stream.expect("openssl s_server did not start listening")), host, &roots, &mut read_buf, &mut write_buf).await;
    Some(r.map(|s| assert_eq!(s.cookies_left(), 8)))
}

async fn check_hierarchy(rsa: bool) {
    // the root that signed the chain is accepted, alone or among others and wherever it stands in the list
    for roots in [&["ca.pem"][..], &["other.pem", "ca.pem"][..], &["ca.pem", "other.pem"][..]] {
        if let Some(r) = key_exchange(rsa, roots, "localhost").await {
            assert!(r.is_ok(), "rsa={rsa} roots {roots:?}: {:?}", r.err());
        }
    }
    // an unrelated root, or none at all: the handshake completes (the checks are deferred) but the session is refused
    for roots in [&["other.pem"][..], &[][..]] {
        if let Some(r) = key_exchange(rsa, roots, "localhost").await {
            assert!(matches!(r, Err(KeError::Tls(_))), "rsa={rsa} roots {roots:?} must be refused: {r:?}");
        }
    }
    // the right root but a certificate for another name
    if let Some(r) = key_exchange(rsa, &["ca.pem"], "not-localhost.example").await {
        assert!(matches!(r, Err(KeError::Tls(_))), "rsa={rsa}: wrong name must be refused: {r:?}");
    }
}

// VERIFIES: REQ-NTS-09
#[tokio::test]
async fn ecdsa_chain_is_trusted_only_through_a_given_root() {
    check_hierarchy(false).await;
}

// VERIFIES: REQ-NTS-08
#[cfg(feature = "rsa")]
#[tokio::test]
async fn rsa_chain_is_trusted_only_through_a_given_root() {
    check_hierarchy(true).await;
}

// VERIFIES: REQ-NTS-08
#[cfg(not(feature = "rsa"))]
#[tokio::test]
async fn rsa_chain_is_refused_without_rsa_support() {
    // the Pico W build: even with the right root, an RSA chain cannot be verified, so it must never be accepted
    if let Some(r) = key_exchange(true, &["ca.pem"], "localhost").await {
        assert!(matches!(r, Err(KeError::Tls(_))), "{r:?}");
    }
}
