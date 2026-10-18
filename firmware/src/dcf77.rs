//! DCF77 receiver on GP0 (PON, power: low = on) and GP1 (TCO, the demodulated carrier: high while the carrier is reduced).
//! Measures the rising edge and the width of every pulse and gives them to `picoclock_core::dcf77`, which finds the minute
//! mark, decodes the frame and confirms it with the next one. A confirmed time goes to the source selection like an NTP time.
//! The receiver is only powered while `dcf77` is in `time_sources`.

use defmt::info;
use embassy_futures::select::{Either, select};
use embassy_rp::gpio::{Input, Output};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use picoclock_core::dcf77::Decoder;
use picoclock_core::sources::{Sample, Source};

use crate::state::{self, Offer};

fn wanted() -> bool {
    state::with_config(|c| c.time_sources.contains(&Source::Dcf77))
}

#[embassy_executor::task]
pub async fn dcf77_task(mut tco: Input<'static>, mut pon: Output<'static>) -> ! {
    let mut decoder = Decoder::new();
    loop {
        // off: PON high. The receiver restarts on the falling edge of PON, so switching on is high, a pause, low.
        pon.set_high();
        while !wanted() {
            Timer::after_secs(1).await;
        }
        info!("dcf77: receiver on");
        decoder.reset();
        Timer::after_millis(100).await;
        pon.set_low();
        loop {
            // a rising edge starts a pulse; wake up regularly to see whether the receiver is still wanted
            match select(tco.wait_for_rising_edge(), Timer::after(Duration::from_secs(2))).await {
                Either::Second(()) => {
                    if !wanted() {
                        break;
                    }
                    continue;
                }
                Either::First(()) => {}
            }
            let rise = Instant::now();
            if with_timeout(Duration::from_millis(400), tco.wait_for_falling_edge()).await.is_err() {
                // stuck high (no receiver, or a wire loose): not a pulse
                tco.wait_for_low().await;
                continue;
            }
            let width_us = rise.elapsed().as_micros() as u32;
            if let Some(fix) = decoder.pulse(rise.as_micros(), width_us) {
                info!("dcf77: time confirmed");
                let delay = state::with_config(|c| c.dcf77_delay());
                let sample = Sample { unix_ms: fix.unix_utc * 1000, at_ms: picoclock_core::dcf77::corrected_ms(fix.rise_us, delay) };
                let _ = state::SAMPLES.try_send(Offer { src: Source::Dcf77, sample, secure: false });
            }
            state::with_runtime(|r| r.dcf = decoder.stats);
        }
        info!("dcf77: receiver off");
    }
}
