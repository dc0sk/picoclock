//! Buzzer on GP14 (active type: high = sound), driven by beep requests from the UI.

use core::sync::atomic::Ordering;

use embassy_futures::select::{Either, select};
use embassy_rp::gpio::Output;
use embassy_time::Timer;
use picoclock_core::ui::Beep;

use crate::state::{ALARM_RINGING, BEEP};

async fn pulse(pin: &mut Output<'static>, on_ms: u64) {
    pin.set_high();
    Timer::after_millis(on_ms).await;
    pin.set_low();
}

#[embassy_executor::task]
pub async fn buzzer_task(mut pin: Output<'static>) -> ! {
    loop {
        match BEEP.wait().await {
            Beep::Key => pulse(&mut pin, 30).await,
            Beep::Chime => {
                pulse(&mut pin, 80).await;
                Timer::after_millis(120).await;
                pulse(&mut pin, 80).await;
            }
            Beep::Morse(digits, dit) => {
                for (on, ms) in picoclock_core::morse::timeline(&digits, dit) {
                    if on {
                        pulse(&mut pin, ms as u64).await;
                    } else {
                        Timer::after_millis(ms as u64).await;
                    }
                }
            }
            Beep::Alarm => {
                // Three beeps, a pause, repeat - until the UI clears the flag (button press or timeout).
                while ALARM_RINGING.load(Ordering::Relaxed) {
                    for _ in 0..3 {
                        pulse(&mut pin, 150).await;
                        Timer::after_millis(150).await;
                    }
                    // Wake early if a new request (e.g. nothing) arrives; otherwise just pause.
                    if let Either::First(_) = select(BEEP.wait(), Timer::after_millis(600)).await {
                        break;
                    }
                }
            }
        }
    }
}
