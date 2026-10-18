//! Decides which time source may set the clock (`picoclock_core::sources`) and applies the decision. Every source sends its
//! time here instead of to the clock task; the settings `time_mode`, `time_sources` and the vote powers say how they are
//! combined. Also keeps the per-source lines of `status` up to date.

use defmt::info;
use embassy_time::{Duration, Instant, with_timeout};
use picoclock_core::dcf77::DelayEstimator;
use picoclock_core::sources::{SOURCES, Selector, Source, Verdict};

use crate::state::{self, Offer};

/// The clock's own UTC time in ms, from the moment the RTC was last set and the monotonic timer since.
fn clock_ms() -> Option<i64> {
    state::anchor().map(|(secs, at)| secs * 1000 + at.elapsed().as_millis() as i64)
}

/// An NTP time is only a reference for the receiver delay for this long after it arrived: the monotonic timer drifts by
/// about 30 ppm, 0.1 s in an hour.
const NTP_REFERENCE_MS: u64 = 300_000;
/// A learned delay is written to flash when it moved by this much (ms) and the last write is older than the interval.
const LEARNED_STEP_MS: u16 = 5;
const LEARNED_SAVE_EVERY_MS: u64 = 600_000;

#[embassy_executor::task]
pub async fn sources_task() -> ! {
    let mut selector = Selector::new();
    let mut estimator = DelayEstimator::new();
    let mut last_save_ms: Option<u64> = None;
    loop {
        let received = with_timeout(Duration::from_secs(2), state::SAMPLES.receive()).await;
        let policy = state::config_snapshot().policy();
        let now_ms = Instant::now().as_millis();
        // a source the user took out of the list must not keep a vote with an old sample
        for s in SOURCES {
            if !policy.selected(s) {
                selector.forget(s);
            }
        }
        if let Ok(Offer { src, sample, secure }) = received {
            // Automatic receiver delay: compare this DCF77 time (already corrected by the delay in use) with a fresh NTP time
            if src == Source::Dcf77 {
                let cfg = state::config_snapshot();
                let reference = selector.recent(Source::Ntp, now_ms, NTP_REFERENCE_MS).filter(|_| cfg.dcf77_auto && policy.selected(Source::Ntp));
                if let Some(ntp) = reference {
                    let ahead_ms = sample.now_ms(now_ms) - ntp.now_ms(now_ms);
                    let reading = cfg.dcf77_delay() as i32 - ahead_ms as i32;
                    info!("dcf77: {} ms from NTP with a delay of {} ms -> reading {}", ahead_ms, cfg.dcf77_delay(), reading);
                    if let Some(learned) = estimator.push(reading)
                        && learned.abs_diff(cfg.dcf77_learned) >= LEARNED_STEP_MS
                        && last_save_ms.is_none_or(|t| now_ms - t >= LEARNED_SAVE_EVERY_MS)
                    {
                        info!("dcf77: receiver delay learned {} ms (was {})", learned, cfg.dcf77_learned);
                        last_save_ms = Some(now_ms);
                        state::edit_and_save(false, |c| {
                            let mut v = heapless::String::<8>::new();
                            let _ = core::fmt::Write::write_fmt(&mut v, format_args!("{learned}"));
                            let _ = c.set("dcf77_learned", &v);
                        });
                    }
                }
            }
            let verdict = selector.offer(&policy, src, sample, now_ms, clock_ms());
            info!("sources: {} -> {}", src.name(), match verdict {
                Verdict::Use { write: true } => "used, clock set",
                Verdict::Use { write: false } => "used, clock already agrees",
                Verdict::Standby => "standby",
                Verdict::Outvoted => "outvoted",
                Verdict::NoMajority => "no majority",
                Verdict::Off => "not selected",
            });
            if let Verdict::Use { write } = verdict {
                let dcf = src == picoclock_core::sources::Source::Dcf77;
                let projected_s = (sample.unix_ms + now_ms.saturating_sub(sample.at_ms) as i64).div_euclid(1000);
                state::with_runtime(|r| {
                    r.sync_secure = secure;
                    r.sync_dcf = dcf;
                    if !write {
                        r.last_sync_unix = Some(projected_s);
                    }
                });
                if write {
                    let (secs, ms) = (sample.unix_ms.div_euclid(1000), sample.unix_ms.rem_euclid(1000));
                    state::TIME_APPLY.signal((secs as u64, ms as u16, Instant::from_millis(sample.at_ms)));
                }
            }
        }
        let reports = selector.report(&policy, Instant::now().as_millis(), clock_ms());
        state::with_runtime(|r| r.sources = reports.map(Some));
    }
}
