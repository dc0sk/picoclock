//! LED matrix scanning. The panel is 8 multiplexed rows driven by shift registers: for each row the four
//! row bytes are clocked out, latched, the row address is set on A0..A2 and the output enable (active low) is opened.
//! Pin map from the Waveshare demo (`define.h`).

use core::cell::RefCell;
use core::sync::atomic::{AtomicU8, Ordering};

use embassy_rp::gpio::Output;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Instant, Timer};
use portable_atomic::AtomicU32;
use picoclock_core::frame::Frame;

pub static FRAME: Mutex<CriticalSectionRawMutex, RefCell<Frame>> = Mutex::new(RefCell::new(Frame::new()));
/// 0 = automatic (follow [`AUTO_DUTY`]), otherwise a fixed 1..=100 % duty.
pub static FIXED_DUTY: AtomicU8 = AtomicU8::new(0);
/// Duty chosen from the photosensor, 1..=100 %.
pub static AUTO_DUTY: AtomicU8 = AtomicU8::new(100);

/// Scan timing diagnostics, shown by the terminal's `status`: how late the row timer fired (microseconds, worst case
/// since boot) and how many waits were more than [`LATE_THRESHOLD_US`] late. A late wake-up means an interrupt or a
/// critical section delayed the scan, which shows as a brighter or dimmer row.
pub static LATE_MAX_US: AtomicU32 = AtomicU32::new(0);
pub static LATE_COUNT: AtomicU32 = AtomicU32::new(0);
const LATE_THRESHOLD_US: u32 = 100;

async fn wait_us(us: u64) {
    let t = Instant::now();
    Timer::after_micros(us).await;
    let late = t.elapsed().as_micros().saturating_sub(us).min(u32::MAX as u64) as u32;
    if late > LATE_THRESHOLD_US {
        LATE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    LATE_MAX_US.fetch_max(late, Ordering::Relaxed);
}

pub fn update(f: impl FnOnce(&mut Frame)) {
    FRAME.lock(|fb| f(&mut fb.borrow_mut()));
}

pub struct Matrix {
    pub oe: Output<'static>,
    pub sdi: Output<'static>,
    pub clk: Output<'static>,
    pub le: Output<'static>,
    pub a: [Output<'static>; 3],
}

impl Matrix {
    fn shift_byte(&mut self, mut b: u8) {
        for _ in 0..8 {
            self.clk.set_low();
            self.sdi.set_level((b & 1 != 0).into());
            self.clk.set_high();
            b >>= 1;
        }
    }
}

/// Refreshes one row per millisecond (125 Hz full-frame). Run this on a high-priority interrupt executor
/// so that WiFi activity can never make the display flicker.
#[embassy_executor::task]
pub async fn scan_task(mut m: Matrix) -> ! {
    let mut row = 0usize;
    loop {
        m.oe.set_high(); // blank while switching rows to avoid ghosting
        let bytes = FRAME.lock(|fb| fb.borrow().row_bytes(row));
        for b in bytes {
            m.shift_byte(b);
        }
        m.le.set_high();
        m.le.set_low();
        for (bit, pin) in m.a.iter_mut().enumerate() {
            pin.set_level(((row >> bit) & 1 != 0).into());
        }
        let duty = match FIXED_DUTY.load(Ordering::Relaxed) {
            0 => AUTO_DUTY.load(Ordering::Relaxed),
            n => n,
        };
        let on_us = duty.clamp(1, 100) as u64 * 10;
        m.oe.set_low();
        if on_us < 1000 {
            wait_us(on_us).await;
            m.oe.set_high();
            wait_us(1000 - on_us).await;
        } else {
            wait_us(1000).await;
        }
        row = (row + 1) & 7;
    }
}
