//! State shared between tasks: the live configuration, runtime facts for `status`, and the signals that tell
//! tasks something happened. Everything sits behind blocking critical-section mutexes held only for short,
//! non-awaiting closures.

use core::cell::RefCell;
use core::sync::atomic::AtomicBool;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex as Cs;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use picoclock_core::cli::Status;
use picoclock_core::config::Config;
use picoclock_core::ui::{Beep, Button, Press};

static CONFIG: Mutex<Cs, RefCell<Option<Config>>> = Mutex::new(RefCell::new(None));
static RUNTIME: Mutex<Cs, RefCell<Status>> = Mutex::new(RefCell::new(Status {
    utc_unix: None,
    last_sync_unix: None,
    wifi_up: false,
    wifi_slot: None,
    mac_last: None,
    ip: None,
    gateway: None,
    temp_c10: None,
    chip_temp_c10: None,
    light_raw: 0,
    unsaved: false,
}));

/// Button events from the button task to the UI.
pub static BUTTONS: Channel<Cs, (Button, Press), 8> = Channel::new();
/// Unix time (UTC) from the latest successful NTP exchange, for the clock task to write to the RTC.
pub static NTP_TIME: Signal<Cs, (u64, u16, embassy_time::Instant)> = Signal::new();
/// (unix seconds, instant) of the moment the RTC was last set exactly on a UTC second boundary. Time since then is
/// `secs + anchor.elapsed()`, which is what the NTP server serves.
static ANCHOR: Mutex<Cs, RefCell<Option<(i64, embassy_time::Instant)>>> = Mutex::new(RefCell::new(None));

pub fn set_anchor(secs: i64, at: embassy_time::Instant) {
    ANCHOR.lock(|a| *a.borrow_mut() = Some((secs, at)));
}

pub fn anchor() -> Option<(i64, embassy_time::Instant)> {
    ANCHOR.lock(|a| *a.borrow())
}
/// `true` = save immediately, `false` = save once edits have been quiet for a few seconds.
pub static SAVE_REQUEST: Signal<Cs, bool> = Signal::new();
/// Any setting changed (the net task re-checks WiFi credentials and timezone mode).
pub static CONFIG_CHANGED: Signal<Cs, ()> = Signal::new();
pub static SYNC_NOW: Signal<Cs, ()> = Signal::new();
pub static GEO_NOW: Signal<Cs, ()> = Signal::new();
pub static BEEP: Signal<Cs, Beep> = Signal::new();
pub static ALARM_RINGING: AtomicBool = AtomicBool::new(false);

pub fn with_config<R>(f: impl FnOnce(&mut Config) -> R) -> R {
    CONFIG.lock(|c| f(c.borrow_mut().get_or_insert_with(Config::default)))
}

pub fn config_snapshot() -> Config {
    with_config(|c| c.clone())
}

pub fn set_config(cfg: Config) {
    with_config(|c| *c = cfg);
}

pub fn with_runtime<R>(f: impl FnOnce(&mut Status) -> R) -> R {
    RUNTIME.lock(|r| f(&mut r.borrow_mut()))
}

pub fn status() -> Status {
    with_runtime(|r| *r)
}

/// Call after any edit of the configuration. `save_now` skips the quiet-period debounce.
pub fn mark_changed(save_now: bool) {
    with_runtime(|r| r.unsaved = true);
    SAVE_REQUEST.signal(save_now);
    CONFIG_CHANGED.signal(());
}
