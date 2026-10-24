//! GPS receiver (u-blox NEO-M9N, ADR-0007): NMEA on UART1 RX (GP5, the module's TX), the 1 pulse per second on GP8 (not GP3: the
//! DS3231's 1 Hz square wave is on it), and, only when `gps_config` is on, UBX on UART1 TX (GP4, the module's RX) and the module's
//! reset pin on GP9.
//!
//! Two tasks. `gps_io_task` runs on an interrupt executor of lower priority than the display's and higher than the main one, so that
//! a stalled main executor (the NTS key exchange blocks it for up to 7 s on the RP2040) cannot delay the stamp of a PPS edge: it
//! stamps every edge with the microsecond timer the moment it wakes, cuts the serial stream into NMEA lines and UBX frames, and sends
//! edges, RMC lines and the few UBX frames the clock reads through `EVENTS`; what the thread-mode task wants sent to the module comes
//! back through `TX`. It must not touch `state::with_config` and the like: those are thread-mode mutexes and panic from an interrupt
//! (found by the first run of the simulated receiver). `gps_task` runs in thread mode, pairs the events
//! (`picoclock_core::gps::Tracker`), offers a time to the source selection, and runs the setup of the receiver
//! (`picoclock_core::ubx::Configurator`). Without `gps_config` the clock sends nothing to the module.

use core::cell::Cell;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use defmt::info;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex as Cs;
use embassy_sync::channel::Channel;
use embassy_time::{Instant, Timer};
use picoclock_core::cli::GpsReset;
use picoclock_core::gps::{LINE_MAX, Report, Tracker};
use picoclock_core::sources::{Sample, Source};
use picoclock_core::ubx::{self, Configurator, Gnss, Item, Module, Send as UbxSend};

use crate::state::{self, Offer};

/// What the interrupt-level task tells the thread-mode task, in the order it happened.
pub enum Event {
    /// A PPS edge, stamped (microseconds of the monotonic timer).
    Edge(u64),
    /// A finished RMC line and the instant its last byte was read.
    Line(u64, heapless::Vec<u8, LINE_MAX>),
    /// A UBX frame the clock reads (acknowledgement, version, navigation solution): class, id, announced length, the kept payload.
    Ubx(u8, u8, u16, heapless::Vec<u8, { ubx::KEEP }>),
}

/// What the thread-mode task asks the interrupt-level task to do to the module.
pub enum Tx {
    /// Send these bytes (a UBX frame).
    Frame(heapless::Vec<u8, { ubx::FRAME_MAX }>),
    /// Pull the module's reset pin low for a moment.
    HardReset,
}

/// Room for a stall of the main executor: 3 events a second, a stall of up to 7 s fills 21 of the 32.
static EVENTS: Channel<Cs, Event, 32> = Channel::new();
static TX: Channel<Cs, Tx, 4> = Channel::new();
/// The speed to listen at, 0 = do not listen (set by `gps_task` from the settings).
static LISTEN_BAUD: AtomicU32 = AtomicU32::new(0);
/// Bytes received since the receiver was switched on, and errors of the serial line. Two tasks touch them and the RP2040 has no
/// atomic read-modify-write, hence a critical section (a few instructions long).
static COUNTS: Mutex<Cs, Cell<(u32, u32)>> = Mutex::new(Cell::new((0, 0)));

fn add_bytes(n: u32) {
    COUNTS.lock(|c| {
        let (b, e) = c.get();
        c.set((b.wrapping_add(n), e));
    });
}

#[cfg(not(feature = "gps-sim"))]
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
/// A receiver that talked and has been silent this long gets its reset pin pulled, at most once per [`HARD_RESET_EVERY_S`].
const HARD_RESET_AFTER_S: u32 = 60;
const HARD_RESET_EVERY_S: u64 = 600;

/// The settings the task follows.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Wanted {
    baud: u32,
    configure: bool,
    gnss: u8,
}

fn wanted() -> Option<Wanted> {
    state::with_config(|c| c.time_sources.contains(&Source::Gps).then_some(Wanted { baud: c.gps_baud, configure: c.gps_config, gnss: c.gps_gnss }))
}

#[cfg(not(feature = "gps-sim"))]
mod io {
    use embassy_futures::select::{Either4, select4};
    use embassy_rp::Peri;
    use embassy_rp::gpio::{Flex, Input};
    use embassy_rp::peripherals::{PIN_4, PIN_5, UART1};
    use embassy_rp::uart::{self, BufferedUart};
    use embassy_time::{Duration, Instant, Timer};
    use defmt::warn;
    use picoclock_core::ubx::{self, Demux, Item};
    use static_cell::StaticCell;

    use super::{EVENTS, Event, LISTEN_BAUD, TX, Tx, add_bytes, add_error};
    use core::sync::atomic::Ordering::Relaxed;

    /// The module's reset pin is held low for this long.
    const RESET_PULSE_MS: u64 = 100;

    #[embassy_executor::task]
    pub async fn gps_io_task(
        mut uart: Peri<'static, UART1>,
        mut tx_pin: Peri<'static, PIN_4>,
        mut rx_pin: Peri<'static, PIN_5>,
        mut pps: Input<'static>,
        mut reset_pin: Flex<'static>,
    ) -> ! {
        static RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
        static TX_BUF: StaticCell<[u8; 128]> = StaticCell::new();
        let rx_buf: &'static mut [u8; 256] = RX_BUF.init([0; 256]);
        let tx_buf: &'static mut [u8; 128] = TX_BUF.init([0; 128]);
        // the reset pin floats (the module has its own pull-up); it is only ever pulled low, never driven high
        reset_pin.set_as_input();
        loop {
            let baud = LISTEN_BAUD.load(Relaxed);
            if baud == 0 {
                // nothing to send while the receiver is off
                while TX.try_receive().is_ok() {}
                Timer::after_millis(500).await;
                continue;
            }
            let mut config = uart::Config::default();
            config.baudrate = baud;
            let mut port = BufferedUart::new(uart.reborrow(), tx_pin.reborrow(), rx_pin.reborrow(), crate::Irqs, &mut tx_buf[..], &mut rx_buf[..], config);
            let (mut tx, mut rx) = port.split_ref();
            let mut demux = Demux::new();
            let mut chunk = [0u8; 64];
            // when to look at LISTEN_BAUD again: a fixed time, because a timer made anew on every pass would be dropped by every
            // chunk of bytes and edge and never run out while the receiver talks
            let mut check = Instant::now() + Duration::from_secs(1);
            'listen: loop {
                // one edge future for as long as it takes an edge to come: dropping and re-creating it on every chunk of bytes
                // would leave short gaps in which an edge is not noticed
                let mut edge = core::pin::pin!(pps.wait_for_rising_edge());
                loop {
                    match select4(&mut edge, rx.read(&mut chunk), Timer::at(check), TX.receive()).await {
                        Either4::First(()) => {
                            // stamped first, before anything else is done
                            let at = Instant::now().as_micros();
                            if EVENTS.try_send(Event::Edge(at)).is_err() {
                                warn!("gps: event queue full, a PPS edge was lost");
                            }
                            break;
                        }
                        Either4::Second(Err(_)) => {
                            add_error();
                            // a line held low or noise can return an error at once, again and again: let the other tasks run
                            embassy_futures::yield_now().await;
                        }
                        Either4::Second(Ok(n)) => {
                            let at = Instant::now().as_micros();
                            add_bytes(n as u32);
                            for &b in &chunk[..n] {
                                match demux.push(b) {
                                    // only the RMC sentence is of use; the receiver sends five or six others a second unless it is set up
                                    Some(Item::Nmea(line)) if line.len() >= 6 && &line[3..6] == b"RMC" => {
                                        let mut v = heapless::Vec::new();
                                        let _ = v.extend_from_slice(line);
                                        if EVENTS.try_send(Event::Line(at, v)).is_err() {
                                            warn!("gps: event queue full, a sentence was lost");
                                        }
                                    }
                                    Some(Item::Ubx { class, id, len, payload }) if matches!((class, id), (ubx::CLS_ACK, _) | (ubx::CLS_MON, ubx::ID_VER) | (ubx::CLS_NAV, ubx::ID_PVT)) => {
                                        let mut v = heapless::Vec::new();
                                        let _ = v.extend_from_slice(payload);
                                        if EVENTS.try_send(Event::Ubx(class, id, len, v)).is_err() {
                                            warn!("gps: event queue full, a UBX frame was lost");
                                        }
                                    }
                                    _ => {}
                                }
                                demux.done();
                            }
                        }
                        Either4::Third(()) => {
                            check = Instant::now() + Duration::from_secs(1);
                            if LISTEN_BAUD.load(Relaxed) != baud {
                                break 'listen;
                            }
                        }
                        Either4::Fourth(Tx::Frame(bytes)) => {
                            let mut rest = &bytes[..];
                            while !rest.is_empty() {
                                match tx.write(rest).await {
                                    Ok(n) => rest = &rest[n..],
                                    Err(_) => break,
                                }
                            }
                        }
                        Either4::Fourth(Tx::HardReset) => {
                            reset_pin.set_low();
                            reset_pin.set_as_output();
                            Timer::after_millis(RESET_PULSE_MS).await;
                            reset_pin.set_as_input();
                        }
                    }
                }
            }
            drop(tx);
            drop(rx);
            drop(port);
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
/// on one), and 300 ms later the sentence of that second, `SIM_OFFSET_S` ahead of the clock, a navigation solution, and answers to
/// what the setup sends. Everything after the events is the real code path.
#[cfg(feature = "gps-sim")]
#[embassy_executor::task]
pub async fn gps_sim_task() -> ! {
    use embassy_time::Duration;
    defmt::warn!("gps: SIMULATED receiver, {} s from the clock", SIM_OFFSET_S);
    loop {
        let (true, Some((secs, anchor_at))) = (LISTEN_BAUD.load(Relaxed) != 0, state::anchor()) else {
            while TX.try_receive().is_ok() {}
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
        // what a u-blox receiver would answer, so that the setup can be seen to work
        while let Ok(tx) = TX.try_receive() {
            let Tx::Frame(f) = tx else { continue };
            let (class, id) = (f[2], f[3]);
            let mut p = heapless::Vec::new();
            if (class, id) == (ubx::CLS_CFG, ubx::ID_VALSET) {
                let _ = p.extend_from_slice(&[class, id]);
                let _ = EVENTS.try_send(Event::Ubx(ubx::CLS_ACK, ubx::ID_ACK, 2, p));
            } else if (class, id) == (ubx::CLS_MON, ubx::ID_VER) {
                let mut ver = [0u8; 40];
                ver[..22].copy_from_slice(b"SIMULATED CORE 4.04 (0");
                let _ = p.extend_from_slice(&ver);
                let _ = EVENTS.try_send(Event::Ubx(ubx::CLS_MON, ubx::ID_VER, 40, p));
            }
        }
        if state::with_config(|c| c.gps_config) {
            let mut pvt = [0u8; 92];
            pvt[11] = 0b111;
            pvt[12..16].copy_from_slice(&25u32.to_le_bytes());
            pvt[20] = 3;
            pvt[23] = 12;
            let mut p = heapless::Vec::new();
            let _ = p.extend_from_slice(&pvt);
            let _ = EVENTS.try_send(Event::Ubx(ubx::CLS_NAV, ubx::ID_PVT, 92, p));
        }
    }
}

#[embassy_executor::task]
pub async fn gps_task() -> ! {
    use embassy_futures::select::{Either3, select3};
    use embassy_time::Duration;
    let mut tracker = Tracker::new();
    let mut setup = Configurator::new();
    let mut module = Module::default();
    let mut applied: Option<Wanted> = None;
    let mut last_bytes = 0u32;
    let (mut quiet, mut silent) = (0u8, false);
    let mut silent_s = 0u32;
    let mut last_hard_reset: Option<Instant> = None;
    // the once-a-second check has a fixed time, so that the events (three a second) do not keep postponing it
    let mut tick = Instant::now() + Duration::from_secs(1);
    loop {
        let want = wanted();
        let now_ms = Instant::now().as_millis();
        let before = LISTEN_BAUD.load(Relaxed);
        LISTEN_BAUD.store(want.map_or(0, |w| w.baud), Relaxed);
        if before != want.map_or(0, |w| w.baud) {
            // switched on, off or to another speed: start from nothing
            info!("gps: {}", if want.is_some() { "listening" } else { "off" });
            tracker = Tracker::new();
            COUNTS.lock(|c| c.set((0, 0)));
            last_bytes = 0;
            (quiet, silent, silent_s) = (0, false, 0);
            module = Module::default();
            setup.stop();
            applied = None;
            while EVENTS.try_receive().is_ok() {}
            state::with_runtime(|r| r.gps = Report::default());
        }
        // the setup of a u-blox receiver: when it is switched on or its choice of constellations changed, and again whenever the
        // receiver has just started talking (it forgets everything at a power cycle)
        match want {
            Some(w) if w.configure => {
                if applied != Some(w) {
                    info!("gps: setting the receiver up");
                    setup.restart(now_ms);
                    applied = Some(w);
                }
            }
            _ => {
                if applied.is_some() {
                    setup.stop();
                    applied = None;
                    module = Module::default();
                }
            }
        }
        match select3(EVENTS.receive(), Timer::at(tick), state::GPS_RESET.wait()).await {
            Either3::First(event) if want.is_some() => match event {
                Event::Edge(at) => tracker.edge(at),
                Event::Line(at, line) => {
                    if let Some(fix) = tracker.line(&line, at) {
                        let (unix_ms, at_ms) = fix.sample_ms();
                        info!("gps: time offered ({} consecutive seconds)", tracker.run());
                        let _ = state::SAMPLES.try_send(Offer { src: Source::Gps, sample: Sample { unix_ms, at_ms }, secure: false });
                    }
                }
                Event::Ubx(class, id, len, payload) => {
                    if applied.is_some() {
                        setup.item(Instant::now().as_millis(), &Item::Ubx { class, id, len, payload: &payload }, &mut module);
                    }
                }
            },
            Either3::First(_) => {}
            Either3::Second(()) => {
                // after a stall of the main executor the missed ticks would run off one after another within microseconds, find
                // no new bytes and declare the receiver silent: the next check is a second from now
                tick = (tick + Duration::from_secs(1)).max(Instant::now() + Duration::from_millis(500));
                let bytes = counts().0;
                if bytes != last_bytes {
                    if (silent || last_bytes == 0) && applied.is_some() {
                        // it started (again) to talk: it may have been switched off and on and has forgotten the setup
                        setup.restart(Instant::now().as_millis());
                    }
                    (quiet, silent, silent_s) = (0, false, 0);
                } else if want.is_some() {
                    quiet = quiet.saturating_add(1);
                    if quiet >= SILENT_TICKS && !silent {
                        silent = true;
                        tracker.silent();
                        info!("gps: no data for {} s", SILENT_TICKS);
                    }
                    if silent {
                        silent_s += 1;
                        let talked = last_bytes != 0;
                        let rested = last_hard_reset.is_none_or(|t| t.elapsed() >= Duration::from_secs(HARD_RESET_EVERY_S));
                        if applied.is_some() && talked && silent_s >= HARD_RESET_AFTER_S && rested {
                            info!("gps: silent for {} s, pulling the reset pin", silent_s);
                            let _ = TX.try_send(Tx::HardReset);
                            last_hard_reset = Some(Instant::now());
                        }
                    }
                }
                last_bytes = bytes;
            }
            Either3::Third(how) => {
                if want.is_some() {
                    match how {
                        GpsReset::Soft(kind) => {
                            info!("gps: software reset");
                            let _ = TX.try_send(Tx::Frame(ubx::reset(kind)));
                        }
                        GpsReset::Hard => {
                            info!("gps: reset pin");
                            let _ = TX.try_send(Tx::HardReset);
                        }
                    }
                }
            }
        }
        if let (Some(w), true) = (want, applied.is_some()) {
            // what to send now (the answer timeouts run on this call)
            match setup.due(Instant::now().as_millis()) {
                Some(UbxSend::Config) => {
                    let _ = TX.try_send(Tx::Frame(ubx::config_frame(Gnss(w.gnss))));
                }
                Some(UbxSend::PollVersion) => {
                    let _ = TX.try_send(Tx::Frame(ubx::poll_version()));
                }
                None => {}
            }
            setup.sync(&mut module);
        }
        if want.is_some() {
            let report = Report {
                stats: tracker.stats,
                run: tracker.run(),
                bytes: counts().0,
                silent,
                since_edge_ms: tracker.since_edge(Instant::now().as_micros()).map(|us| (us / 1000).min(u32::MAX as u64) as u32),
                uart_errors: counts().1,
                module,
            };
            state::with_runtime(|r| r.gps = report);
        }
    }
}
