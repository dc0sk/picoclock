//! GPS receiver (u-blox NEO-M9N, ADR-0007): NMEA on UART1 RX (GP5, the module's TX) and the 1 pulse per second on GP3.
//!
//! Two tasks. `gps_io_task` runs on an interrupt executor of higher priority than the main one (below the display), so that a
//! stalled main executor (the NTS key exchange blocks it for up to 7 s on the RP2040) cannot delay the stamp of a PPS edge: it
//! stamps every edge with the microsecond timer the moment it wakes, cuts the serial bytes into lines, and sends edges and RMC
//! lines with their stamps through `EVENTS`. It must not touch `state::with_config` and the like: those are thread-mode
//! mutexes and panic from an interrupt (found by the first run of the simulated receiver). `gps_task` runs in thread mode, pairs
//! the events (`picoclock_core::gps::Tracker`), and offers a time to the source selection. The clock never sends anything to the module.

use defmt::info;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex as Cs;
use embassy_sync::channel::Channel;
use embassy_time::{Instant, Timer};
use picoclock_core::gps::{LINE_MAX, Report, Tracker};
use picoclock_core::sources::{Sample, Source};
use core::cell::Cell;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};
use embassy_sync::blocking_mutex::Mutex;

use crate::state::{self, Offer};

/// What the interrupt-level task tells the thread-mode task, in the order it happened.
pub enum Event {
    /// A PPS edge, stamped (microseconds of the monotonic timer).
    Edge(u64),
    /// A finished RMC line and the instant its last byte was read.
    Line(u64, heapless::Vec<u8, LINE_MAX>),
}

/// Room for a stall of the main executor: 2 events a second, a stall of up to 7 s.
static EVENTS: Channel<Cs, Event, 16> = Channel::new();
/// The speed to listen at, 0 = do not listen (set by `gps_task` from the settings).
static LISTEN_BAUD: AtomicU32 = AtomicU32::new(0);
/// Bytes received since the receiver was switched on, and errors of the serial line plus events that could not be queued. Two
/// tasks touch them and the RP2040 has no atomic read-modify-write, hence a critical section (a few instructions long).
static COUNTS: Mutex<Cs, Cell<(u32, u32)>> = Mutex::new(Cell::new((0, 0)));

fn add_bytes(n: u32) {
    COUNTS.lock(|c| {
        let (b, e) = c.get();
        c.set((b.wrapping_add(n), e));
    });
}

fn add_error() {
    COUNTS.lock(|c| {
        let (b, e) = c.get();
        c.set((b, e.wrapping_add(1)));
    });
}

fn counts() -> (u32, u32) {
    COUNTS.lock(|c| c.get())
}

/// Whole seconds without a byte after which the line counts as silent.
const SILENT_TICKS: u8 = 3;

fn wanted() -> Option<u32> {
    state::with_config(|c| c.time_sources.contains(&Source::Gps).then_some(c.gps_baud))
}

#[cfg(not(feature = "gps-sim"))]
mod io {
    use embassy_futures::select::{Either, select};
    use embassy_rp::Peri;
    use embassy_rp::gpio::Input;
    use embassy_rp::peripherals::{PIN_5, UART1};
    use embassy_rp::uart::{self, BufferedUartRx};
    use embassy_time::{Instant, Timer};
    use picoclock_core::gps::LineBuffer;
    use static_cell::StaticCell;

    use super::{EVENTS, Event, LISTEN_BAUD, add_bytes, add_error};
    use core::sync::atomic::Ordering::Relaxed;

    #[embassy_executor::task]
    pub async fn gps_io_task(mut uart: Peri<'static, UART1>, mut rx_pin: Peri<'static, PIN_5>, mut pps: Input<'static>) -> ! {
        static RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
        let buf: &'static mut [u8; 256] = RX_BUF.init([0; 256]);
        loop {
            let baud = LISTEN_BAUD.load(Relaxed);
            if baud == 0 {
                Timer::after_millis(500).await;
                continue;
            }
            let mut config = uart::Config::default();
            config.baudrate = baud;
            let mut rx = BufferedUartRx::new(uart.reborrow(), rx_pin.reborrow(), crate::Irqs, &mut buf[..], config);
            let mut lines = LineBuffer::new();
            let mut chunk = [0u8; 64];
            'listen: loop {
                // one edge future for as long as it takes an edge to come: dropping and re-creating it on every chunk of bytes
                // would leave short gaps in which an edge is not noticed
                let mut edge = core::pin::pin!(pps.wait_for_rising_edge());
                loop {
                    match select(&mut edge, select(rx.read(&mut chunk), Timer::after_secs(1))).await {
                        Either::First(()) => {
                            // stamped first, before anything else is done
                            let at = Instant::now().as_micros();
                            if EVENTS.try_send(Event::Edge(at)).is_err() {
                                add_error();
                            }
                            break;
                        }
                        Either::Second(Either::Second(())) => {
                            if LISTEN_BAUD.load(Relaxed) != baud {
                                break 'listen;
                            }
                        }
                        Either::Second(Either::First(Err(_))) => {
                            add_error();
                        }
                        Either::Second(Either::First(Ok(n))) => {
                            let at = Instant::now().as_micros();
                            add_bytes(n as u32);
                            for &b in &chunk[..n] {
                                let Some(line) = lines.push(b) else { continue };
                                // only the RMC sentence is of use; the receiver sends five or six others a second
                                if line.len() >= 6 && &line[3..6] == b"RMC" {
                                    let mut v = heapless::Vec::new();
                                    let _ = v.extend_from_slice(line);
                                    if EVENTS.try_send(Event::Line(at, v)).is_err() {
                                        add_error();
                                    }
                                }
                                lines.done();
                            }
                        }
                    }
                }
            }
            drop(rx);
        }
    }
}
#[cfg(not(feature = "gps-sim"))]
pub use io::gps_io_task;

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

/// The simulated receiver (test build, ADR-0007 point 10): a PPS edge on every second boundary of the clock (the RTC anchor is set
/// on one), and 300 ms later the sentence of that second, `SIM_OFFSET_S` ahead of the clock. Everything after the events is the real
/// code path.
#[cfg(feature = "gps-sim")]
#[embassy_executor::task]
pub async fn gps_sim_task() -> ! {
    use embassy_time::Duration;
    defmt::warn!("gps: SIMULATED receiver, {} s from the clock", SIM_OFFSET_S);
    loop {
        let (true, Some((secs, anchor_at))) = (LISTEN_BAUD.load(Relaxed) != 0, state::anchor()) else {
            Timer::after_secs(1).await;
            continue;
        };
        let k = anchor_at.elapsed().as_millis() / 1000 + 1;
        Timer::at(anchor_at + Duration::from_millis(k * 1000)).await;
        let _ = EVENTS.try_send(Event::Edge(Instant::now().as_micros()));
        let mut text = heapless::String::<120>::new();
        let _ = picoclock_core::gps::build_rmc(secs + k as i64 + SIM_OFFSET_S, &mut text);
        Timer::after_millis(300).await;
        let mut v = heapless::Vec::new();
        let _ = v.extend_from_slice(text.as_bytes());
        add_bytes(text.len() as u32 + 2);
        let _ = EVENTS.try_send(Event::Line(Instant::now().as_micros(), v));
    }
}

#[embassy_executor::task]
pub async fn gps_task() -> ! {
    let mut tracker = Tracker::new();
    let mut last_bytes = 0u32;
    let (mut quiet, mut silent) = (0u8, false);
    // the once-a-second check has a fixed time, so that the events (two a second) do not keep postponing it
    let mut tick = Instant::now() + embassy_time::Duration::from_secs(1);
    loop {
        let baud = wanted();
        let before = LISTEN_BAUD.load(Relaxed);
        LISTEN_BAUD.store(baud.unwrap_or(0), Relaxed);
        if before != baud.unwrap_or(0) {
            // switched on, off or to another speed: start from nothing
            info!("gps: {}", if baud.is_some() { "listening" } else { "off" });
            tracker = Tracker::new();
            COUNTS.lock(|c| c.set((0, 0)));
            last_bytes = 0;
            (quiet, silent) = (0, false);
            while EVENTS.try_receive().is_ok() {}
            state::with_runtime(|r| r.gps = Report::default());
        }
        match embassy_futures::select::select(EVENTS.receive(), Timer::at(tick)).await {
            embassy_futures::select::Either::First(event) if baud.is_some() => match event {
                Event::Edge(at) => tracker.edge(at),
                Event::Line(at, line) => {
                    if let Some(fix) = tracker.line(&line, at) {
                        let (unix_ms, at_ms) = fix.sample_ms();
                        info!("gps: time offered ({} consecutive seconds)", tracker.run());
                        let _ = state::SAMPLES.try_send(Offer { src: Source::Gps, sample: Sample { unix_ms, at_ms }, secure: false });
                    }
                }
            },
            embassy_futures::select::Either::First(_) => {}
            embassy_futures::select::Either::Second(()) => {
                // once a second: has anything arrived?
                tick += embassy_time::Duration::from_secs(1);
                let bytes = counts().0;
                if bytes != last_bytes {
                    (quiet, silent) = (0, false);
                } else if baud.is_some() {
                    quiet = quiet.saturating_add(1);
                    if quiet >= SILENT_TICKS && !silent {
                        silent = true;
                        tracker.silent();
                        info!("gps: no data for {} s", SILENT_TICKS);
                    }
                }
                last_bytes = bytes;
            }
        }
        if baud.is_some() {
            let report = Report {
                stats: tracker.stats,
                run: tracker.run(),
                bytes: counts().0,
                silent,
                since_edge_ms: tracker.since_edge(Instant::now().as_micros()).map(|us| (us / 1000).min(u32::MAX as u64) as u32),
                uart_errors: counts().1,
            };
            state::with_runtime(|r| r.gps = report);
        }
    }
}
