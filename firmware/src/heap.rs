//! Global allocator for the HTTPS stack, Pico 2 W only (`https` feature): rustls allocates. The rest of the firmware does
//! not, so the heap is a fixed region of its own and its use can be watched in `status`.

use core::alloc::{GlobalAlloc, Layout};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};

use embedded_alloc::LlffHeap as Heap;

/// The heap, with a record of the most that was ever in use at once (so a long run shows its real headroom).
struct Tracked {
    inner: Heap,
    peak: AtomicUsize,
}

// SAFETY: every call is passed on to the wrapped allocator unchanged; the peak is only bookkeeping.
unsafe impl GlobalAlloc for Tracked {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { self.inner.alloc(layout) };
        self.peak.fetch_max(self.inner.used(), Ordering::Relaxed);
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.inner.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static HEAP: Tracked = Tracked { inner: Heap::empty(), peak: AtomicUsize::new(0) };

/// 144 KiB of the RP2350's 512 KiB: a TLS connection needs the record buffers (kept outside the heap), the server
/// configuration with its certificate, and rustls' per-connection state.
const HEAP_SIZE: usize = 144 * 1024;

/// Call once, before anything allocates.
pub fn init() {
    static mut MEMORY: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
    // SAFETY: called once at start-up before the executor runs; MEMORY is used by nothing else.
    unsafe { HEAP.inner.init(&raw mut MEMORY as usize, HEAP_SIZE) }
}

/// (bytes in use now, most ever in use at once, size of the heap)
pub fn usage() -> (usize, usize, usize) {
    (HEAP.inner.used(), HEAP.peak.load(Ordering::Relaxed), HEAP_SIZE)
}

/// `rustls` and its RustCrypto provider ask the `getrandom` crate for entropy; it has no backend on this target, so
/// it is told to use the same `embassy-crypto` RNG driver as everything else (the ring oscillator on the RP2040, and
/// the same source on the RP2350).
fn device_rng(buf: &mut [u8]) -> Result<(), getrandom::Error> {
    embassy_crypto::rng_fill_bytes(buf);
    Ok(())
}
getrandom::register_custom_getrandom!(device_rng);
