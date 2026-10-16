//! Links the cryptographic drivers that `embedded-tls` (NTS-KE) calls through `embassy-crypto`: the primitives come
//! from `embassy-crypto-rustcrypto` (features listed in Cargo.toml), the random numbers from here.
//!
//! The RP2040 has no hardware random number generator. `RoscRng` is Embassy's source for it: bits sampled from the
//! ring oscillator's jitter. That is the best entropy the chip offers, but it is not a certified TRNG; it feeds the TLS
//! ephemeral key, the NTS unique identifiers and the AEAD nonces. (The RP2350 has a real TRNG; using it is a possible
//! hardening for the Pico 2 W build.)

use embassy_crypto::driver::Rng;
use embassy_rp::clocks::RoscRng;
use embassy_crypto_rustcrypto as _;

struct Driver;

impl Rng for Driver {
    fn fill_bytes(buf: &mut [u8]) {
        RoscRng.fill_bytes(buf)
    }
}

embassy_crypto::rng_impl!(Driver);
