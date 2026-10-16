//! Measures what TLS 1.3 and NTS cost on an RP2040 (125 MHz Cortex-M0+, no crypto hardware), using the RustCrypto
//! crate versions that embedded-tls 0.19 uses. Results go to RTT (defmt).
#![no_std]
#![no_main]

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use aes_siv::siv::Aes128Siv; // AES-SIV-CMAC-256 (two 128-bit keys); NTS AEAD id 15
use defmt::info;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_time::Instant;
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use panic_probe as _;
use rand_core::{CryptoRng, RngCore};
use sha2::{Digest, Sha256};

/// Deterministic generator: good enough for timing, never for keys.
struct BenchRng(u64);
impl RngCore for BenchRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for c in dest.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            c.copy_from_slice(&v[..c.len()]);
        }
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}
impl CryptoRng for BenchRng {}

fn time_ms<R>(label: &str, f: impl FnOnce() -> R) -> R {
    let t = Instant::now();
    let r = f();
    let us = t.elapsed().as_micros();
    info!("{}: {} ms ({} us)", label, us / 1000, us);
    r
}

#[embassy_executor::main(executor = "embassy_rp::executor::Executor", entry = "cortex_m_rt::entry")]
async fn main(_spawner: Spawner) {
    let _p = embassy_rp::init(Default::default());
    info!("crypto bench: RP2040 at {} MHz", embassy_rp::clocks::clk_sys_freq() / 1_000_000);
    let mut rng = BenchRng(0x1234_5678_9abc_def1);

    // --- symmetric and hash -------------------------------------------------------------------------------
    let data = [0x5Au8; 4096];
    time_ms("SHA-256 over 4 KiB", || Sha256::digest(&data));

    let gcm = Aes128Gcm::new(&[7u8; 16].into());
    let mut buf = [0u8; 1024];
    time_ms("AES-128-GCM seal 1 KiB", || gcm.encrypt_in_place_detached(Nonce::from_slice(&[1u8; 12]), b"", &mut buf).unwrap());

    // NTS: AES-SIV-CMAC-256 over a 48-byte NTP header with a unique-identifier extension as associated data.
    let mut siv = Aes128Siv::new(&[9u8; 32].into());
    let ad = [3u8; 48 + 36];
    let mut out = [0u8; 16 + 8];
    let hdr: [&[u8]; 2] = [&ad, &[5u8; 16]];
    let n = time_ms("AES-SIV-CMAC-256 encrypt (NTS request)", || {
        let r = siv.encrypt_in_place(hdr, &mut heapless_vec(&mut out, 8)).unwrap();
        r
    });
    let _ = n;

    // --- public key ---------------------------------------------------------------------------------------
    let sk = SigningKey::random(&mut rng);
    let vk = VerifyingKey::from(&sk);
    let msg = [0xA5u8; 64];
    let sig: Signature = time_ms("ECDSA P-256 sign   (HTTPS server: CertificateVerify)", || sk.sign(&msg));
    time_ms("ECDSA P-256 verify (TLS client: once per certificate / CertificateVerify)", || vk.verify(&msg, &sig).unwrap());

    let a = p256::ecdh::EphemeralSecret::random(&mut rng);
    let b = p256::ecdh::EphemeralSecret::random(&mut rng);
    let pb = p256::PublicKey::from(&b);
    time_ms("P-256 ephemeral key generation (public point)", || p256::PublicKey::from(&a));
    time_ms("P-256 ECDH shared secret", || a.diffie_hellman(&pb));

    // X25519: what browsers offer first, so an HTTPS server would need it to avoid a HelloRetryRequest.
    let xa = x25519_dalek::StaticSecret::random_from_rng(&mut rng);
    let xb = x25519_dalek::StaticSecret::random_from_rng(&mut rng);
    let xpb = x25519_dalek::PublicKey::from(&xb);
    time_ms("X25519 public key (key generation)", || x25519_dalek::PublicKey::from(&xa));
    time_ms("X25519 shared secret", || xa.diffie_hellman(&xpb));

    info!("done");
    loop {
        embassy_time::Timer::after_secs(60).await;
    }
}

/// `encrypt_in_place` wants a Buffer; a fixed array wrapper keeps this spike allocation-free.
fn heapless_vec<'a>(out: &'a mut [u8; 24], len: usize) -> FixedBuf<'a> {
    FixedBuf { buf: out, len }
}
struct FixedBuf<'a> {
    buf: &'a mut [u8; 24],
    len: usize,
}
impl AsRef<[u8]> for FixedBuf<'_> {
    fn as_ref(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}
impl AsMut<[u8]> for FixedBuf<'_> {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.buf[..self.len]
    }
}
impl aes_siv::aead::Buffer for FixedBuf<'_> {
    fn extend_from_slice(&mut self, other: &[u8]) -> aes_siv::aead::Result<()> {
        if self.len + other.len() > 24 {
            return Err(aes_siv::aead::Error);
        }
        self.buf[self.len..self.len + other.len()].copy_from_slice(other);
        self.len += other.len();
        Ok(())
    }
    fn truncate(&mut self, len: usize) {
        self.len = len.min(self.len);
    }
}
