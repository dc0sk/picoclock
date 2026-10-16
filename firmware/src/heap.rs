//! Global allocator for the HTTPS stack, Pico 2 W only (`https` feature): rustls allocates. The rest of the firmware does
//! not, so the heap is a fixed region of its own and its use can be watched in `status`.

use core::mem::MaybeUninit;

use embedded_alloc::LlffHeap as Heap;

#[global_allocator]
static HEAP: Heap = Heap::empty();

/// 144 KiB of the RP2350's 512 KiB: a TLS connection needs the record buffers (kept outside the heap), the server
/// configuration with its certificate, and rustls' per-connection state.
const HEAP_SIZE: usize = 144 * 1024;

/// Call once, before anything allocates.
pub fn init() {
    static mut MEMORY: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
    // SAFETY: called once at start-up before the executor runs; MEMORY is used by nothing else.
    unsafe { HEAP.init(&raw mut MEMORY as usize, HEAP_SIZE) }
}

/// (bytes in use, bytes free)
pub fn usage() -> (usize, usize) {
    (HEAP.used(), HEAP.free())
}

/// `rustls` and its RustCrypto provider ask the `getrandom` crate for entropy; it has no backend on this target, so
/// it is told to use the same `embassy-crypto` RNG driver as everything else (the ring oscillator on the RP2040, and
/// the same source on the RP2350).
fn device_rng(buf: &mut [u8]) -> Result<(), getrandom::Error> {
    embassy_crypto::rng_fill_bytes(buf);
    Ok(())
}
getrandom::register_custom_getrandom!(device_rng);
