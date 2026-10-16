//! Cross-checks the TLS 1.3 exporter added to the vendored embedded-tls against OpenSSL: our client and `openssl
//! s_server -keymatexport` must derive the same keying material from the same handshake. Needs the `openssl` binary;
//! it is skipped (with a message) when that is not installed.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::webpki::CertVerifier;
use embedded_tls::{Aes128GcmSha256, Certificate, TlsClock, TlsConfig, TlsConnection, TlsContext, TlsError};
use embedded_tls::crypto::TlsHash;
use embedded_tls::{CertificateEntryRef, CertificateRef, CertificateVerifyRef, SignatureScheme, TlsCipherSuite, TlsVerifier};
use p256::ecdsa::signature::Signer;
use picoclock_core::nts_ke::DeferredVerifier;
// the crypto drivers embedded-tls calls through embassy-crypto are linked, not passed
use embassy_crypto_rand as _;
use embassy_crypto_rustcrypto as _;

struct Now;
impl TlsClock for Now {
    fn now() -> Option<u64> {
        SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
    }
}

/// Kills the child on drop, so a failing assertion cannot leave `openssl s_server` running.
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

fn pem_to_der(pem: &str) -> Vec<u8> {
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    // minimal base64 decoder, enough for a certificate
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => 255,
    };
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
    for c in b64.bytes().filter(|&c| c != b'=') {
        acc = (acc << 6) | val(c) as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

/// A running `openssl s_server` with two CAs: `ca.pem` signed the server certificate, `other-ca.pem` did not.
struct Server {
    dir: std::path::PathBuf,
    child: KillOnDrop,
    port: u16,
}

fn start_server() -> Option<Server> {
    let dir = std::env::temp_dir().join(format!("picoclock-tls-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ext.cnf"), "subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n").unwrap();
    let ca = |name: &str, cn: &str| {
        openssl(&dir, &["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", &format!("{name}.key")])
            && openssl(&dir, &["req", "-x509", "-new", "-key", &format!("{name}.key"), "-sha256", "-days", "2", "-subj", cn, "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign", "-out", &format!("{name}.pem")])
    };
    std::fs::write(dir.join("inter.cnf"), "basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign\n").unwrap();
    // root (ca) -> intermediate (inter) -> server, like a real server's chain; other-ca is a root that signed nothing
    let ok = ca("ca", "/CN=test-ca")
        && ca("other-ca", "/CN=other-ca")
        && openssl(&dir, &["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "inter.key"])
        && openssl(&dir, &["req", "-new", "-key", "inter.key", "-subj", "/CN=test-intermediate", "-out", "inter.csr"])
        && openssl(&dir, &["x509", "-req", "-in", "inter.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-CAcreateserial", "-days", "2", "-sha256", "-extfile", "inter.cnf", "-out", "inter.pem"])
        && openssl(&dir, &["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "server.key"])
        && openssl(&dir, &["req", "-new", "-key", "server.key", "-subj", "/CN=localhost", "-out", "server.csr"])
        && openssl(&dir, &["x509", "-req", "-in", "server.csr", "-CA", "inter.pem", "-CAkey", "inter.key", "-CAcreateserial", "-days", "2", "-sha256", "-extfile", "ext.cnf", "-out", "server.pem"]);
    if !ok {
        eprintln!("SKIPPED: the openssl binary is not available or could not create the test certificates");
        return None;
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    // `openssl s_server` reads commands from stdin and quits on EOF, so give it a pipe that stays open for the whole test.
    let mut child = Command::new("openssl")
        .current_dir(&dir)
        .args(["s_server", "-accept", &port.to_string(), "-cert", "server.pem", "-cert_chain", "inter.pem", "-key", "server.key", "-tls1_3", "-ciphersuites", "TLS_AES_128_GCM_SHA256", "-groups", "P-256", "-keymatexport", LABEL, "-keymatexportlen", "32", "-naccept", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(dir.join("server.err")).unwrap())
        .spawn()
        .expect("start openssl s_server");
    let stdin = child.stdin.take();
    Some(Server { dir, child: KillOnDrop(child, stdin), port })
}

const LABEL: &str = "EXPORTER-network-time-security";
static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl Server {
    fn der(&self, name: &str) -> Vec<u8> {
        pem_to_der(&std::fs::read_to_string(self.dir.join(name)).unwrap())
    }
    async fn connect(&self) -> tokio::net::TcpStream {
        // the server needs a moment to start listening: retry instead of sleeping a fixed time
        for _ in 0..50 {
            if let Ok(s) = tokio::net::TcpStream::connect(("127.0.0.1", self.port)).await {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("openssl s_server did not start listening");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// VERIFIES: REQ-NTS-06
#[tokio::test]
async fn exporter_matches_openssl() {
    let Some(mut server) = start_server() else { return };
    let der = server.der("ca.pem");
    let stream = server.connect().await;
    let mut read_buf = vec![0u8; 16640];
    let mut write_buf = vec![0u8; 4096];
    let config = TlsConfig::new().with_server_name("localhost");
    let mut tls = TlsConnection::<_, Aes128GcmSha256>::new(FromTokio::new(stream), &mut read_buf, &mut write_buf);
    let verifier: CertVerifier<Aes128GcmSha256, Now, 4096> = CertVerifier::new(Certificate::X509(&der));
    if let Err(e) = tls.open(TlsContext::new(&config, verifier)).await {
        let err = std::fs::read_to_string(server.dir.join("server.err")).unwrap_or_default();
        let _ = server.child.0.kill();
        let mut out = String::new();
        if let Some(mut so) = server.child.0.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut so, &mut out);
        }
        panic!("TLS 1.3 handshake with certificate verification failed: {e:?} ; openssl s_server said: {err}\n--- stdout ---\n{out}");
    }

    let mut ours = [0u8; 32];
    tls.export_keying_material(LABEL.as_bytes(), &[], &mut ours).expect("export");
    // anything else must differ: label, context and length are all part of the derivation
    let mut other_label = [0u8; 32];
    tls.export_keying_material(b"EXPORTER-other", &[], &mut other_label).unwrap();
    let mut other_ctx = [0u8; 32];
    tls.export_keying_material(LABEL.as_bytes(), &[0, 0, 0, 15, 0], &mut other_ctx).unwrap();
    assert!(ours != other_label && ours != other_ctx && other_label != other_ctx);
    drop(tls);

    let mut theirs = None;
    for line in BufReader::new(server.child.0.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if let Some(hex) = line.trim().strip_prefix("Keying material: ") {
            theirs = Some(hex.trim().to_lowercase());
            break;
        }
    }
    let ours_hex: String = ours.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(Some(ours_hex), theirs, "our exporter output must equal OpenSSL's");
}

/// Handshake with the deferring verifier against `openssl s_server`, then the deferred checks.
async fn deferred(root_file: &str, host: &str, clock_offset: i64) -> Option<Result<(), TlsError>> {
    let server = start_server()?;
    let root = server.der(root_file);
    let stream = server.connect().await;
    let mut read_buf = vec![0u8; 16640];
    let mut write_buf = vec![0u8; 4096];
    let config = TlsConfig::new().with_server_name(host);
    let mut tls = TlsConnection::<_, Aes128GcmSha256>::new(FromTokio::new(stream), &mut read_buf, &mut write_buf);
    CLOCK_OFFSET.store(clock_offset, std::sync::atomic::Ordering::Relaxed);
    let mut verifier = DeferredVerifier::<Offset>::new(&root);
    // the handshake must succeed whatever the certificate says: the checks are not run yet
    tls.open(TlsContext::new(&config, &mut verifier)).await.expect("the handshake itself completes with the deferred verifier");
    Some(verifier.finish())
}

static CLOCK_OFFSET: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
struct Offset;
impl TlsClock for Offset {
    fn now() -> Option<u64> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
        Some((now + CLOCK_OFFSET.load(std::sync::atomic::Ordering::Relaxed)) as u64)
    }
}

/// One test, not several: the clock offset is a global (`TlsClock::now` has no state), so the cases must not run in parallel.
// VERIFIES: REQ-NTS-07
#[tokio::test]
async fn deferred_verification_accepts_only_what_the_real_checks_accept() {
    // right root, right name, right time: accepted
    if let Some(r) = deferred("ca.pem", "localhost", 0).await {
        assert!(r.is_ok(), "{r:?}");
    }
    // a chain that does not lead to the trusted root
    if let Some(r) = deferred("other-ca.pem", "localhost", 0).await {
        assert!(r.is_err(), "a certificate from another CA must be refused");
    }
    // a certificate for another name
    if let Some(r) = deferred("ca.pem", "not-the-server.example", 0).await {
        assert!(r.is_err(), "a certificate for another name must be refused");
    }
    // the test certificates are valid for two days: three days later they are expired, three days earlier not yet valid
    if let Some(r) = deferred("ca.pem", "localhost", 3 * 86_400).await {
        assert!(r.is_err(), "an expired certificate must be refused");
    }
    if let Some(r) = deferred("ca.pem", "localhost", -3 * 86_400).await {
        assert!(r.is_err(), "a certificate that is not valid yet must be refused");
    }
    CLOCK_OFFSET.store(0, std::sync::atomic::Ordering::Relaxed);
}

// VERIFIES: REQ-NTS-07
#[test]
fn deferred_verification_refuses_when_no_handshake_happened() {
    let root = [0u8; 4];
    let mut verifier = DeferredVerifier::<Now>::new(&root);
    assert!(verifier.finish().is_err(), "finish without a recorded certificate chain and signature must fail, not pass");
}

type Hash = <Aes128GcmSha256 as TlsCipherSuite>::Hash;

/// What the server signs in `CertificateVerify` (RFC 8446 section 4.4.3): 64 spaces, a label, a zero byte, the hash.
fn certificate_verify_message(transcript_hash: &[u8]) -> Vec<u8> {
    let mut m = vec![0x20u8; 64];
    m.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
    m.extend_from_slice(transcript_hash);
    m
}

/// Drives the deferred verifier by hand with the certificate openssl made and a signature we make with its private key,
/// to prove that `finish` really checks the `CertificateVerify` signature: a man in the middle can present the real
/// server's public certificate, and only this check stops him.
// VERIFIES: REQ-NTS-07
#[test]
fn deferred_verification_checks_the_certificate_verify_signature() {
    let Some(server) = start_server() else { return };
    let leaf = server.der("server.pem");
    let inter = server.der("inter.pem");
    let root = server.der("ca.pem");
    // SEC1 ECPrivateKey: SEQUENCE { version, OCTET STRING (32 bytes private key), ... }
    let key_pem = std::fs::read_to_string(server.dir.join("server.key")).unwrap();
    let key_der = pem_to_der(&key_pem);
    let sk_bytes = &key_der[key_der.iter().position(|&b| b == 0x04).unwrap() + 2..][..32];
    let signing_key = p256::ecdsa::SigningKey::from_slice(sk_bytes).expect("the test server's private key");

    let mut transcript = Hash::new();
    TlsHash::update(&mut transcript, b"ClientHello..Certificate stand-in");
    let digest = TlsHash::finalize(transcript.clone());
    let good: p256::ecdsa::Signature = signing_key.sign(&certificate_verify_message(digest.as_ref()));
    let good = good.to_der();

    let run = |chain: &[&[u8]], transcript_for_chain: &Hash, signature: &[u8]| -> Result<(), TlsError> {
        let mut v = DeferredVerifier::<Offset>::new(&root);
        CLOCK_OFFSET.store(0, std::sync::atomic::Ordering::Relaxed);
        TlsVerifier::<Aes128GcmSha256>::set_hostname_verification(&mut &mut v, "localhost")?;
        let mut cert = CertificateRef::with_context(&[]);
        for der in chain {
            cert.add(CertificateEntryRef::X509(der))?;
        }
        TlsVerifier::<Aes128GcmSha256>::verify_certificate(&mut &mut v, transcript_for_chain, cert)?; // deferred: always Ok here
        TlsVerifier::<Aes128GcmSha256>::verify_signature(&mut &mut v, CertificateVerifyRef { signature_scheme: SignatureScheme::EcdsaSecp256r1Sha256, signature })?;
        v.finish()
    };

    assert!(run(&[&leaf, &inter], &transcript, good.as_bytes()).is_ok(), "the real chain with a genuine signature must be accepted");
    // the intermediate is needed: the root did not sign the leaf. Leaving it out (or sending only the leaf) must fail,
    // and so must presenting a chain in which the intermediate is replaced by something else.
    assert!(run(&[&leaf], &transcript, good.as_bytes()).is_err(), "a chain missing its intermediate must be refused");
    assert!(run(&[&leaf, &root], &transcript, good.as_bytes()).is_err(), "a chain with the wrong intermediate must be refused");

    // a signature over another transcript (what a man in the middle with a recorded signature would have)
    let mut other = Hash::new();
    TlsHash::update(&mut other, b"a different handshake");
    assert!(run(&[&leaf, &inter], &other, good.as_bytes()).is_err(), "a signature that does not cover this handshake must be refused");

    // a corrupted signature, a signature by another key, an empty and a truncated one
    let mut bad = good.as_bytes().to_vec();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert!(run(&[&leaf, &inter], &transcript, &bad).is_err(), "a corrupted signature must be refused");
    let other_key = p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
    let forged: p256::ecdsa::Signature = other_key.sign(&certificate_verify_message(digest.as_ref()));
    assert!(run(&[&leaf, &inter], &transcript, forged.to_der().as_bytes()).is_err(), "a signature by another key must be refused");
    assert!(run(&[&leaf, &inter], &transcript, &[]).is_err());
    assert!(run(&[&leaf, &inter], &transcript, &good.as_bytes()[..good.as_bytes().len() / 2]).is_err());

    // a corrupted certificate with a signature that is otherwise right
    let mut tampered = leaf.clone();
    let mid = tampered.len() / 2;
    tampered[mid] ^= 1;
    assert!(run(&[&tampered, &inter], &transcript, good.as_bytes()).is_err(), "a corrupted certificate must be refused");
}
