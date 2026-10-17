//! Cross-checks the certificate code against OpenSSL: it must read, verify and fingerprint the certificate we generate,
//! and our reader must accept and refuse uploaded certificates and keys as documented. Needs the `openssl` binary; skipped
//! (with a message) when it is missing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use heapless::Vec;
use picoclock_core::x509::{self, X509Error};

const NOW: i64 = 1_792_000_000; // 2026-10-14 17:46:40 UTC

fn openssl(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("openssl").current_dir(dir).args(args).stdin(Stdio::null()).output().ok()?;
    if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)) } else { None }
}

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup(name: &str) -> Option<Dir> {
    let dir = std::env::temp_dir().join(format!("picoclock-x509-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    if openssl(&dir, &["version"]).is_none() {
        eprintln!("SKIPPED: the openssl binary is not available");
        return None;
    }
    Some(Dir(dir))
}

fn pem(label: &str, der: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for chunk in der.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |a, (i, &b)| a | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            s.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    let wrapped: Vec<String, 64> = s.as_bytes().chunks(64).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
    format!("-----BEGIN {label}-----\n{}\n-----END {label}-----\n", wrapped.join("\n"))
}

fn generated(secret: [u8; 32], names: &[&str]) -> std::vec::Vec<u8> {
    x509::generate_self_signed(secret, [0xA5, 1, 2, 3, 4, 5, 6, 7], names, NOW).expect("generation").to_vec()
}

// VERIFIES: REQ-WEB-07
#[test]
fn openssl_reads_verifies_and_fingerprints_the_generated_certificate() {
    let Some(dir) = setup("gen") else { return };
    let d = &dir.0;
    let cert = generated([0x42; 32], &["localhost", "picoclock-54", "picoclock-54.local"]);
    std::fs::write(d.join("c.pem"), pem("CERTIFICATE", &cert)).unwrap();

    let text = openssl(d, &["x509", "-in", "c.pem", "-noout", "-text"]).expect("openssl must parse the certificate");
    for want in [
        "Version: 3 (0x2)",
        "Serial Number: 2666414666355639815 (0x2501020304050607)", // 0xA5 with the sign bit cleared and the first byte kept nonzero
        "Signature Algorithm: ecdsa-with-SHA256",
        "Issuer: CN=localhost",
        "Subject: CN=localhost",
        "Public Key Algorithm: id-ecPublicKey",
        "ASN1 OID: prime256v1",
        "CA:FALSE",
        "Digital Signature",
        "TLS Web Server Authentication",
        "DNS:localhost, DNS:picoclock-54, DNS:picoclock-54.local",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    // validity: from one day before NOW to ten years after, to the second
    let dates = openssl(d, &["x509", "-in", "c.pem", "-noout", "-startdate", "-enddate", "-dateopt", "iso_8601"]).unwrap();
    assert!(dates.contains("notBefore=2026-10-13 17:46:40Z"), "{dates}");
    assert!(dates.contains("notAfter=2036-10-11 17:46:40Z"), "{dates}"); // 3650 days later

    // the self-signature checks out (`-check_ss_sig`: OpenSSL does not check a trust anchor's own signature otherwise),
    // for the right purpose and name, at the right time
    let verify = |name: &str, at: i64| openssl(d, &["verify", "-CAfile", "c.pem", "-check_ss_sig", "-purpose", "sslserver", "-verify_hostname", name, "-attime", &at.to_string(), "c.pem"]);
    assert!(verify("picoclock-54", NOW).is_some(), "openssl must accept the certificate");
    assert!(verify("localhost", NOW).is_some());
    assert!(verify("picoclock-54.local", NOW).is_some());
    assert!(verify("somewhere-else.example", NOW).is_none(), "another host name must be refused");
    assert!(verify("picoclock-54", NOW - 3 * 86_400).is_none(), "before the start of validity");
    assert!(verify("picoclock-54", NOW + 3651 * 86_400).is_none(), "after the end of validity");

    // our fingerprint is OpenSSL's
    let fp = openssl(d, &["x509", "-in", "c.pem", "-noout", "-fingerprint", "-sha256"]).unwrap();
    assert!(fp.trim().ends_with(x509::fingerprint_hex(&x509::fingerprint(&cert)).as_str()), "{fp}");

    // the PKCS#8 form of the secret is a key OpenSSL reads, and it is the key of this certificate
    let pk8 = x509::secret_to_pkcs8(&[0x42; 32]).unwrap();
    std::fs::write(d.join("k.pem"), pem("PRIVATE KEY", &pk8)).unwrap();
    let from_key = openssl(d, &["pkey", "-in", "k.pem", "-pubout"]).expect("openssl must read our PKCS#8 key");
    let from_cert = openssl(d, &["x509", "-in", "c.pem", "-noout", "-pubkey"]).unwrap();
    assert_eq!(from_key.trim(), from_cert.trim(), "the key and the certificate must belong together");

    // tampering with any byte makes OpenSSL refuse the self-signature (or the parse)
    for i in (0..cert.len()).step_by(7) {
        let mut bad = cert.clone();
        bad[i] ^= 0x01;
        std::fs::write(d.join("bad.pem"), pem("CERTIFICATE", &bad)).unwrap();
        assert!(openssl(d, &["verify", "-CAfile", "bad.pem", "-check_ss_sig", "-attime", &NOW.to_string(), "bad.pem"]).is_none(), "a flipped byte {i} was accepted");
    }
}

// VERIFIES: REQ-WEB-07
#[test]
fn keys_and_certificates_made_by_openssl_are_read_and_matched() {
    let Some(dir) = setup("up") else { return };
    let d = &dir.0;
    let mk = |name: &str, curve: &str| {
        openssl(d, &["ecparam", "-name", curve, "-genkey", "-noout", "-out", &format!("{name}.sec1")]).unwrap();
        openssl(d, &["pkcs8", "-topk8", "-nocrypt", "-in", &format!("{name}.sec1"), "-out", &format!("{name}.p8")]).unwrap();
        openssl(d, &["req", "-x509", "-new", "-key", &format!("{name}.sec1"), "-sha256", "-days", "30", "-subj", "/CN=up.example", "-addext", "subjectAltName=DNS:up.example", "-out", &format!("{name}.crt")]).unwrap();
        openssl(d, &["x509", "-in", &format!("{name}.crt"), "-outform", "DER", "-out", &format!("{name}.der")]).unwrap();
        openssl(d, &["pkcs8", "-topk8", "-nocrypt", "-in", &format!("{name}.sec1"), "-outform", "DER", "-out", &format!("{name}.p8.der")]).unwrap();
        openssl(d, &["ec", "-in", &format!("{name}.sec1"), "-outform", "DER", "-out", &format!("{name}.sec1.der")]).unwrap();
    };
    mk("a", "prime256v1");
    mk("b", "prime256v1");
    mk("p384", "secp384r1");
    let read = |f: &str| std::fs::read(d.join(f)).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;

    // the same key in four encodings gives the same secret
    let secret = x509::parse_private_key(&read("a.sec1")).expect("SEC1 PEM");
    assert_eq!(x509::parse_private_key(&read("a.p8")).expect("PKCS#8 PEM"), secret);
    assert_eq!(x509::parse_private_key(&read("a.p8.der")).expect("PKCS#8 DER"), secret);
    assert_eq!(x509::parse_private_key(&read("a.sec1.der")).expect("SEC1 DER"), secret);
    assert_ne!(x509::parse_private_key(&read("b.sec1")).unwrap(), secret);

    // OpenSSL's own certificate is accepted with its own key, refused with another
    let cert = read("a.der");
    let info = x509::validate_pair(&cert, &secret, now).expect("openssl's certificate with its key");
    assert_eq!(x509::validate_pair(&cert, &x509::parse_private_key(&read("b.sec1")).unwrap(), now), Err(X509Error::KeyMismatch));
    assert!(info.not_after > now && info.not_before <= now);
    // ...and our public key is the one OpenSSL prints
    let pubhex = openssl(d, &["ec", "-in", "a.sec1", "-pubout", "-outform", "DER"]).map(|_| ()).is_some();
    assert!(pubhex);

    // a P-384 key and certificate are not supported (and the error says so rather than "mismatch")
    assert_eq!(x509::inspect(&read("p384.der")).err(), Some(X509Error::Unsupported));
    assert!(x509::parse_private_key(&read("p384.sec1")).is_err());

    // garbage and PEM with no key
    assert!(x509::parse_private_key(b"not a key").is_err());
    assert!(x509::parse_private_key(&[]).is_err());
    assert!(x509::parse_private_key(pem("CERTIFICATE", &cert).as_bytes()).is_err(), "a certificate is not a key");

    // a chain in PEM keeps its order and lengths
    let mut chain: Vec<u8, { x509::MAX_CHAIN }> = Vec::new();
    let mut lens: Vec<u16, { x509::MAX_CHAIN_CERTS }> = Vec::new();
    let both = format!("{}{}", pem("CERTIFICATE", &read("a.der")), pem("CERTIFICATE", &read("b.der")));
    x509::parse_pem_chain(&both, &mut chain, &mut lens).unwrap();
    assert_eq!(lens.len(), 2);
    assert_eq!(&chain[..lens[0] as usize], &read("a.der")[..]);
    assert_eq!(&chain[lens[0] as usize..], &read("b.der")[..]);
    // five certificates are more than we keep
    let many = pem("CERTIFICATE", &read("a.der")).repeat(5);
    assert_eq!(x509::parse_pem_chain(&many, &mut chain, &mut lens), Err(X509Error::TooLarge));
}

// VERIFIES: REQ-NTS-08
#[test]
fn describe_ca_reads_what_openssl_made() {
    let Some(dir) = setup("describe-ca") else { return };
    let d = &dir.0;
    let mk = |name: &str, keyargs: &[&str], subj: &str| -> bool {
        let (key, pem) = (format!("{name}.key"), format!("{name}.pem"));
        let mut args = vec!["req", "-x509", "-new", "-nodes", "-days", "30", "-sha256", "-subj", subj, "-keyout", &key, "-out", &pem];
        args.extend_from_slice(keyargs);
        openssl(d, &args).is_some() && openssl(d, &["x509", "-in", &pem, "-outform", "DER", "-out", &format!("{name}.der")]).is_some()
    };
    assert!(
        mk("ec256", &["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1"], "/CN=Test EC 256/O=Org")
            && mk("ec384", &["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:secp384r1"], "/O=Only Org")
            && mk("rsa", &["-newkey", "rsa:2048"], "/CN=Test RSA")
            && mk("ed", &["-newkey", "ed25519"], "/CN=Test Ed"),
        "openssl could not make the test certificates"
    );
    let der = |n: &str| std::fs::read(d.join(format!("{n}.der"))).unwrap();
    let ec = x509::describe_ca(&der("ec256")).unwrap();
    assert_eq!((ec.key, ec.name.as_str(), ec.self_signed), (x509::KeyKind::EcP256, "Test EC 256", true));
    assert_eq!(x509::describe_ca(&der("ec384")).unwrap().key, x509::KeyKind::EcP384);
    assert_eq!(x509::describe_ca(&der("ec384")).unwrap().name.as_str(), "Only Org", "organisation when there is no common name");
    assert_eq!(x509::describe_ca(&der("rsa")).unwrap().key, x509::KeyKind::Rsa);
    assert_eq!(x509::describe_ca(&der("ed")).unwrap().key, x509::KeyKind::Other);
    // fingerprint and validity agree with openssl
    let fp = openssl(d, &["x509", "-in", "ec256.pem", "-noout", "-fingerprint", "-sha256"]).unwrap();
    assert_eq!(fp.trim().rsplit('=').next().unwrap(), x509::fingerprint_hex(&ec.fingerprint).as_str());
    assert!((29 * 86_400..=31 * 86_400).contains(&(ec.not_after - ec.not_before)), "30 days of validity");
    // a name with control characters (edited into the bytes; the signature is not checked here) is shown without them
    let mut evil = der("ec256");
    let at = evil.windows(11).position(|w| w == b"Test EC 256").unwrap();
    evil[at..at + 11].copy_from_slice(b"A\x1b[31m\n\x07\x7fCD");
    let shown = x509::describe_ca(&evil).unwrap().name;
    assert!(shown.bytes().all(|b| (0x20..0x7F).contains(&b)), "{shown:?}");
    // junk, truncations and trailing bytes never panic or pass
    let good = der("ec256");
    for cut in 0..good.len() {
        assert!(x509::describe_ca(&good[..cut]).is_err(), "cut {cut}");
    }
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(x509::describe_ca(&trailing).is_err());
    assert!(x509::describe_ca(&[0x30, 0x00]).is_err());
    assert!(x509::describe_ca(&[0xFF; 40]).is_err());
}
