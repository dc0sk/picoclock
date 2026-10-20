//! Links the cryptographic drivers that `embedded-tls` (NTS-KE) and `rustls` (HTTPS) call: the primitives come from
//! `embassy-crypto-rustcrypto` (features listed in Cargo.toml), the random numbers from here.
//!
//! Pico 2 W (RP2350): the chip's true random number generator (TRNG, with its built-in health tests), started once in
//! `init_trng`. Pico W (RP2040): the chip has none, so bits are sampled from the ring oscillator's jitter, spaced out in time
//! and hashed (`picoclock_core::entropy`). That is the best entropy the RP2040 offers, but it is not a certified TRNG.
//! Either source feeds the TLS ephemeral keys, the HTTPS private key, the NTS unique identifiers, the AEAD nonces, the plain-NTP
//! origin nonces and the network stack's seed.

use embassy_crypto::driver::Rng;
use embassy_crypto_rustcrypto as _;

#[cfg(feature = "rp235x")]
mod source {
    use core::cell::RefCell;

    use embassy_rp::Peri;
    use embassy_rp::peripherals::TRNG;
    use embassy_rp::trng::{Config, Trng};
    use embassy_rp::clocks::RoscRng;
    use embassy_rp::mode::Blocking;
    use embassy_sync::blocking_mutex::Mutex;
    use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;

    // A thread-mode lock, not a critical section: generating a block takes a while and must not hold off the display
    // interrupt. Every caller runs in the thread-mode executor (the display task never asks for random numbers).
    static TRNG_DEV: Mutex<ThreadModeRawMutex, RefCell<Option<Trng<'static, Blocking>>>> = Mutex::new(RefCell::new(None));

    /// Starts the TRNG and keeps it only if two 32-byte draws are different and not constant (a dead source would give
    /// zeros or repeats); otherwise the ring-oscillator source stays in use. Returns whether the TRNG is in use.
    pub fn init(trng: Peri<'static, TRNG>) -> bool {
        let mut t = Trng::new_blocking(trng, Config::default());
        let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
        t.blocking_fill_bytes(&mut a);
        t.blocking_fill_bytes(&mut b);
        let ok = a != b && a.iter().any(|&x| x != a[0]) && b.iter().any(|&x| x != b[0]);
        if ok {
            TRNG_DEV.lock(|d| *d.borrow_mut() = Some(t));
        }
        ok
    }

    pub fn fill(buf: &mut [u8]) {
        let done = TRNG_DEV.lock(|d| match d.borrow_mut().as_mut() {
            Some(t) => {
                t.blocking_fill_bytes(buf);
                true
            }
            None => false,
        });
        if !done {
            // only before `init`, which main calls first; never a silent weak source afterwards
            RoscRng.fill_bytes(buf)
        }
    }
}

#[cfg(not(feature = "rp235x"))]
mod source {
    use core::cell::RefCell;

    use embassy_rp::clocks::RoscRng;
    use embassy_sync::blocking_mutex::Mutex;
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use picoclock_core::entropy::{Pool, RAW_PER_BLOCK};

    static POOL: Mutex<CriticalSectionRawMutex, RefCell<Pool>> = Mutex::new(RefCell::new(Pool::new()));

    /// The ring oscillator's bits are jitter, correlated when read back to back. Each raw byte is taken after a pause of
    /// about a microsecond (so the oscillators drift apart between readings), and every 32 output bytes are a hash over 64
    /// raw bytes and the time (`picoclock_core::entropy`). Slower than the register read (about 0.1 ms per 32 bytes), which
    /// is nothing next to a key exchange.
    pub fn fill(buf: &mut [u8]) {
        for chunk in buf.chunks_mut(32) {
            let mut raw = [0u8; RAW_PER_BLOCK];
            for b in raw.iter_mut() {
                cortex_m::asm::delay(125);
                *b = RoscRng::next_u8();
            }
            let now = embassy_time::Instant::now().as_micros();
            let out = POOL.lock(|p| p.borrow_mut().block(&raw, now));
            chunk.copy_from_slice(&out[..chunk.len()]);
        }
    }
}

#[cfg(feature = "rp235x")]
pub use source::init as init_trng;

/// Fills `buf` from the chip's random source.
pub fn fill(buf: &mut [u8]) {
    source::fill(buf)
}

pub fn next_u64() -> u64 {
    let mut b = [0u8; 8];
    fill(&mut b);
    u64::from_le_bytes(b)
}

struct Driver;

impl Rng for Driver {
    fn fill_bytes(buf: &mut [u8]) {
        fill(buf)
    }
}

embassy_crypto::rng_impl!(Driver);
