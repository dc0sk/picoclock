//! State shared between tasks: the live configuration, runtime facts for `status`, and the signals that tell
//! tasks something happened. Everything sits behind blocking critical-section mutexes held only for short,
//! non-awaiting closures.

use core::cell::RefCell;
use core::sync::atomic::AtomicBool;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex as Cs, ThreadModeRawMutex as Tm};
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use picoclock_core::cli::{GeoTry, Status};
use picoclock_core::config::Config;
use picoclock_core::ui::{Beep, Button, Press};

// The configuration and runtime status are only touched by tasks on the thread-mode executor, which never run
// concurrently with each other (the closures below do not await). A critical-section mutex would switch interrupts
// off for the whole closure; cloning the 8-slot configuration or rendering a frame under it took hundreds of
// microseconds and made the display scan run late (visible as a flickering row). `ThreadModeRawMutex` panics if it is
// ever locked from an interrupt, so a wrong use cannot go unnoticed.
static CONFIG: Mutex<Tm, RefCell<Option<Config>>> = Mutex::new(RefCell::new(None));
static RUNTIME: Mutex<Tm, RefCell<Status>> = Mutex::new(RefCell::new(Status {
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
    scan_late_max_us: 0,
    scan_late_count: 0,
    uptime_s: 0,
    geo: [GeoTry { stage: 0, bytes: 0, end: 0, addr: [0; 4] }; picoclock_core::geo::PROVIDERS.len()],
    sync_secure: false,
    https_state: 0,
    rng_hardware: false,
    dhcp_ntp: [[0; 4]; 4],
    dhcp_ntp_n: 0,
    wifi_scanned: None,
    mdns_group: 0,
    mdns_seen: 0,
    mdns_answered: 0,
    heap_used: 0,
    heap_peak: 0,
    heap_size: 0,
    cert_fp: [0; 32],
    cert_not_after: 0,
    cert_uploaded: false,
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

/// The HTTPS key and certificate chain in use (Pico 2 W). Loaded from flash at boot, replaced by the HTTPS task or an upload.
#[cfg(feature = "https")]
static TLS_MATERIAL: Mutex<Tm, RefCell<Option<picoclock_core::tlsstore::Material>>> = Mutex::new(RefCell::new(None));
/// Ask the storage task to write `TLS_MATERIAL` to its flash sector.
#[cfg(feature = "https")]
pub static TLS_SAVE: Signal<Cs, ()> = Signal::new();
/// Ask the HTTPS task to make a new self-signed certificate (`cert new`).
#[cfg(feature = "https")]
pub static CERT_NEW: Signal<Cs, ()> = Signal::new();
/// Tell the HTTPS task that `TLS_MATERIAL` changed (an upload) so it rebuilds its configuration.
#[cfg(feature = "https")]
pub static CERT_RELOAD: Signal<Cs, ()> = Signal::new();

#[cfg(feature = "https")]
pub fn set_tls_material(m: picoclock_core::tlsstore::Material) {
    TLS_MATERIAL.lock(|t| *t.borrow_mut() = Some(m));
}

#[cfg(feature = "https")]
pub fn tls_material() -> Option<picoclock_core::tlsstore::Material> {
    TLS_MATERIAL.lock(|t| t.borrow().clone())
}

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
    let mut st = with_runtime(|r| *r);
    st.scan_late_max_us = crate::display::LATE_MAX_US.load(core::sync::atomic::Ordering::Relaxed);
    st.scan_late_count = crate::display::LATE_COUNT.load(core::sync::atomic::Ordering::Relaxed);
    st.uptime_s = (embassy_time::Instant::now().as_millis() / 1000) as u32;
    st
}

/// A terminal edit: takes effect at once (tasks re-read the configuration) and is marked unsaved, but is **not** written
/// to flash; that happens only on `save` (as the terminal says). The web page and the buttons save for you, see
/// [`mark_changed`].
pub fn mark_edited() {
    with_runtime(|r| r.unsaved = true);
    CONFIG_CHANGED.signal(());
}

/// A change to the cached location rule: written to flash (so it need not be fetched again after a reboot) unless there
/// are unsaved terminal edits, which a background event must not write behind the user's back.
pub fn mark_cache_changed() {
    if with_runtime(|r| r.unsaved) {
        CONFIG_CHANGED.signal(());
    } else {
        mark_changed(false);
    }
}

/// Call after an edit that is saved for the user (web page, buttons). `save_now` skips the quiet-period debounce. It
/// writes the whole configuration, including any unsaved terminal edits.
pub fn mark_changed(save_now: bool) {
    with_runtime(|r| r.unsaved = true);
    SAVE_REQUEST.signal(save_now);
    CONFIG_CHANGED.signal(());
}
