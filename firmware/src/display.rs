//! LED matrix scanning. The panel is 8 multiplexed rows driven by shift registers: for each row the four
//! row bytes are clocked out, latched, the row address is set on A0..A2 and the output enable (active low) is opened.
//! Pin map from the Waveshare demo (`define.h`).

use core::cell::RefCell;
use core::sync::atomic::{AtomicU8, Ordering};

use embassy_rp::gpio::Output;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Timer;
use picoclock_core::frame::Frame;

pub static FRAME: Mutex<CriticalSectionRawMutex, RefCell<Frame>> = Mutex::new(RefCell::new(Frame::new()));
/// 0 = automatic (follow [`AUTO_DUTY`]), otherwise a fixed 1..=100 % duty.
pub static FIXED_DUTY: AtomicU8 = AtomicU8::new(0);
/// Duty chosen from the photosensor, 1..=100 %.
pub static AUTO_DUTY: AtomicU8 = AtomicU8::new(100);

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
            Timer::after_micros(on_us).await;
            m.oe.set_high();
            Timer::after_micros(1000 - on_us).await;
        } else {
            Timer::after_micros(1000).await;
        }
        row = (row + 1) & 7;
    }
}
