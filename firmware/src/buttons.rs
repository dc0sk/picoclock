//! Three active-low buttons (SET = GP2, UP = GP17, DOWN = GP15) polled every 10 ms.
//! A press is "short" if released before [`LONG`], "long" once held that long, and UP / DOWN auto-repeat while held.

use embassy_rp::gpio::Input;
use embassy_time::{Duration, Ticker};
use picoclock_core::ui::{Button, Press};

use crate::state::BUTTONS;

const POLL_MS: u32 = 10;
const DEBOUNCE: u32 = 30;
const LONG: u32 = 800;
const REPEAT_AFTER: u32 = 500;
const REPEAT_EVERY: u32 = 150;

#[derive(Default)]
struct Track {
    held_ms: u32,
    long_sent: bool,
}

#[embassy_executor::task]
pub async fn button_task(mut pins: [Input<'static>; 3]) -> ! {
    const WHICH: [Button; 3] = [Button::Set, Button::Up, Button::Down];
    let mut tracks: [Track; 3] = Default::default();
    let mut ticker = Ticker::every(Duration::from_millis(POLL_MS as u64));
    loop {
        ticker.next().await;
        for ((pin, t), &b) in pins.iter_mut().zip(tracks.iter_mut()).zip(WHICH.iter()) {
            if pin.is_low() {
                t.held_ms += POLL_MS;
                match b {
                    Button::Set if t.held_ms >= LONG && !t.long_sent => {
                        t.long_sent = true;
                        let _ = BUTTONS.try_send((b, Press::Long));
                    }
                    Button::Up | Button::Down
                        if t.held_ms >= REPEAT_AFTER && (t.held_ms - REPEAT_AFTER) % REPEAT_EVERY < POLL_MS =>
                    {
                        t.long_sent = true; // suppress the short press on release
                        let _ = BUTTONS.try_send((b, Press::Repeat));
                    }
                    _ => {}
                }
            } else {
                if t.held_ms >= DEBOUNCE && !t.long_sent {
                    let _ = BUTTONS.try_send((b, Press::Short));
                }
                *t = Track::default();
            }
        }
    }
}
