//! GPS receiver (u-blox NEO-M9N, ADR-0007): NMEA on UART1 RX (GP5, the module's TX) and the 1 pulse per second on GP3. Runs on an
//! interrupt executor of higher priority than the main one, so that a stalled main executor (the NTS key exchange blocks it for
//! up to 7 s on the RP2040) cannot delay the stamp of a PPS edge. The edge is stamped with the microsecond timer the moment
//! the task wakes; sentences and edges go to `picoclock_core::gps::Tracker`, which pairs them, settles and decides when a time
//! is offered to the source selection. The clock never sends anything to the module.

use defmt::{info, warn};
#[cfg(not(feature = "gps-sim"))]
use embassy_futures::select::{Either, select};
use embassy_rp::Peri;
use embassy_rp::gpio::Input;
use embassy_rp::peripherals::{PIN_5, UART1};
#[cfg(not(feature = "gps-sim"))]
use embassy_rp::uart::{self, BufferedUartRx};
#[cfg(not(feature = "gps-sim"))]
use embassy_time::Duration;
use embassy_time::{Instant, Timer};
#[cfg(not(feature = "gps-sim"))]
use static_cell::StaticCell;
use picoclock_core::gps::{LineBuffer, Report, Tracker};
use picoclock_core::sources::{Sample, Source};

use crate::state::{self, Offer};

/// No byte for this long: the line is silent and a run of good seconds is over.
#[cfg(not(feature = "gps-sim"))]
const SILENT_AFTER: Duration = Duration::from_secs(3);

fn wanted() -> Option<u32> {
    state::with_config(|c| c.time_sources.contains(&Source::Gps).then_some(c.gps_baud))
}

fn publish(tracker: &Tracker, bytes: u32, silent: bool, uart_errors: u32, lines: &LineBuffer) {
    let report = Report {
        stats: tracker.stats,
        run: tracker.run(),
        bytes,
        silent,
        since_edge_ms: tracker.since_edge(Instant::now().as_micros()).map(|us| (us / 1000).min(u32::MAX as u64) as u32),
        uart_errors: uart_errors.wrapping_add(lines.overlong),
    };
    state::with_runtime(|r| r.gps = report);
}

#[cfg(not(feature = "gps-sim"))]
#[embassy_executor::task]
pub async fn gps_task(mut uart: Peri<'static, UART1>, mut rx_pin: Peri<'static, PIN_5>, mut pps: Input<'static>) -> ! {
    static RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    let buf: &'static mut [u8; 256] = RX_BUF.init([0; 256]);
    loop {
        // not listening: the UART is not set up, the pins are idle, the old counters are cleared
        let baud = loop {
            if let Some(b) = wanted() {
                break b;
            }
            state::with_runtime(|r| r.gps = Report::default());
            Timer::after_secs(1).await;
        };
        info!("gps: listening at {} baud", baud);
        let mut config = uart::Config::default();
        config.baudrate = baud;
        let mut rx = BufferedUartRx::new(uart.reborrow(), rx_pin.reborrow(), crate::Irqs, &mut buf[..], config);
        let mut tracker = Tracker::new();
        let mut lines = LineBuffer::new();
        let (mut bytes, mut uart_errors, mut silent) = (0u32, 0u32, false);
        let mut chunk = [0u8; 64];
        let mut deadline = Instant::now() + SILENT_AFTER;
        'listen: loop {
            // one edge future for as long as it takes an edge to come: dropping and re-creating it on every chunk of bytes would
            // leave short gaps in which an edge is not noticed
            let mut edge = core::pin::pin!(pps.wait_for_rising_edge());
            loop {
                // the silence deadline moves only when a byte arrives (an edge every second must not postpone it)
                match select(&mut edge, select(rx.read(&mut chunk), Timer::at(deadline))).await {
                    Either::First(()) => {
                        // stamped first, before anything else is done
                        tracker.edge(Instant::now().as_micros());
                        publish(&tracker, bytes, silent, uart_errors, &lines);
                        break;
                    }
                    Either::Second(Either::Second(())) => {
                        if !silent && bytes > 0 {
                            warn!("gps: no data for {} s", SILENT_AFTER.as_secs());
                        }
                        silent = true;
                        tracker.silent();
                        deadline = Instant::now() + SILENT_AFTER;
                        if wanted() != Some(baud) {
                            break 'listen;
                        }
                    }
                    Either::Second(Either::First(Err(_))) => uart_errors = uart_errors.wrapping_add(1),
                    Either::Second(Either::First(Ok(n))) => {
                        let now = Instant::now();
                        deadline = now + SILENT_AFTER;
                        silent = false;
                        bytes = bytes.wrapping_add(n as u32);
                        for &b in &chunk[..n] {
                            let Some(line) = lines.push(b) else { continue };
                            if let Some(fix) = tracker.line(line, now.as_micros()) {
                                let (unix_ms, at_ms) = fix.sample_ms();
                                info!("gps: time offered ({} consecutive seconds)", tracker.run());
                                let _ = state::SAMPLES.try_send(Offer { src: Source::Gps, sample: Sample { unix_ms, at_ms }, secure: false });
                            }
                            lines.done();
                        }
                    }
                }
                publish(&tracker, bytes, silent, uart_errors, &lines);
            }
        }
        drop(rx);
        info!("gps: receiver off");
    }
}

/// The seconds the simulated receiver is ahead of the clock (build-time `GPS_SIM_OFFSET_S`, may be negative).
#[cfg(feature = "gps-sim")]
const SIM_OFFSET_S: i64 = {
    let b = match option_env!("GPS_SIM_OFFSET_S") {
        Some(s) => s.as_bytes(),
        None => b"0",
    };
    let (neg, mut i) = if !b.is_empty() && b[0] == b'-' { (true, 1) } else { (false, 0) };
    let mut v: i64 = 0;
    while i < b.len() {
        assert!(b[i] >= b'0' && b[i] <= b'9', "GPS_SIM_OFFSET_S is a whole number");
        v = v * 10 + (b[i] - b'0') as i64;
        i += 1;
    }
    if neg { -v } else { v }
};

/// The simulated receiver: a PPS edge on every second boundary of the clock (the RTC anchor is set on one), and 300 ms later the
/// sentence of that second, `SIM_OFFSET_S` ahead of the clock. Everything after the edge and the line is the real code path.
#[cfg(feature = "gps-sim")]
#[embassy_executor::task]
pub async fn gps_task(_uart: Peri<'static, UART1>, _rx_pin: Peri<'static, PIN_5>, _pps: Input<'static>) -> ! {
    use embassy_time::Duration as D;
    warn!("gps: SIMULATED receiver, {} s from the clock", SIM_OFFSET_S);
    let mut tracker = Tracker::new();
    let mut lines = LineBuffer::new();
    let mut bytes = 0u32;
    loop {
        let (Some(_), Some((secs, anchor_at))) = (wanted(), state::anchor()) else {
            if wanted().is_none() {
                state::with_runtime(|r| r.gps = Report::default());
            }
            tracker = Tracker::new();
            Timer::after_secs(1).await;
            continue;
        };
        let k = anchor_at.elapsed().as_millis() / 1000 + 1;
        let edge_at = anchor_at + D::from_millis(k * 1000);
        Timer::at(edge_at).await;
        tracker.edge(Instant::now().as_micros());
        let mut text = heapless::String::<120>::new();
        let _ = picoclock_core::gps::build_rmc(secs + k as i64 + SIM_OFFSET_S, &mut text);
        text.push_str("\r\n").ok();
        Timer::after_millis(300).await;
        let now = Instant::now();
        bytes = bytes.wrapping_add(text.len() as u32);
        for &b in text.as_bytes() {
            let Some(line) = lines.push(b) else { continue };
            if let Some(fix) = tracker.line(line, now.as_micros()) {
                let (unix_ms, at_ms) = fix.sample_ms();
                info!("gps (simulated): time offered ({} consecutive seconds)", tracker.run());
                let _ = state::SAMPLES.try_send(Offer { src: Source::Gps, sample: Sample { unix_ms, at_ms }, secure: false });
            }
            lines.done();
        }
        publish(&tracker, bytes, false, 0, &lines);
    }
}
