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
/// What flash holds (or will hold after the pending save). The terminal edits `CONFIG` only; the web page, the buttons and the
/// location cache carry their own changes into this copy too, and the storage task writes this copy, so a save never takes
/// along terminal edits that were not saved (REQ-CFG-05).
static PERSISTED: Mutex<Tm, RefCell<Option<Config>>> = Mutex::new(RefCell::new(None));
static RUNTIME: Mutex<Tm, RefCell<Status>> = Mutex::new(RefCell::new(Status {
    utc_unix: None,
    last_sync_unix: None,
    wifi_up: false,
    wifi_slot: None,
    mac: None,
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
    roots_user: 0,
    nts_ke: 0,
    nts_ke_ms: 0,
    nts_ke_host: [0; 32],
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
    watchdog_reset: false,
    sync_dcf: false,
    sources: [None; 2],
    dcf: picoclock_core::dcf77::Stats { pulses: 0, frames_ok: 0, frames_bad: 0, pulses_bad: 0 },
    dhcp_server: None,
    prefix: 0,
    dns: None,
    lease_changes: 0,
    link_rssi: None,
    aps: [None; 6],
    update: picoclock_core::update::Check::Never,
    update_latest: None,
    update_checked_unix: None,
}));

/// Milliseconds since boot at the last pass of the clock task (`ui_task`), which owns the RTC bus. The watchdog task feeds the
/// hardware watchdog only while this keeps moving.
pub static HEARTBEAT_MS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// How long the chip waits for a feed before it restarts. 8 s is under the RP2040's limit (8.38 s). The NTS key exchange
/// keeps the executor busy for up to about 7 s on the RP2040, so it feeds the watchdog itself before it starts.
pub const WATCHDOG_TIMEOUT: embassy_time::Duration = embassy_time::Duration::from_secs(8);
static WATCHDOG: Mutex<Cs, RefCell<Option<embassy_rp::watchdog::Watchdog<'static>>>> = Mutex::new(RefCell::new(None));

pub fn heartbeat() {
    HEARTBEAT_MS.store(embassy_time::Instant::now().as_millis() as u32, core::sync::atomic::Ordering::Relaxed);
}

/// Kept in a watchdog scratch register (which survives a watchdog reset) while the program runs. The boot ROM also resets
/// through the watchdog (after a UF2 upload, `bootsel`), so the reset reason alone cannot tell a hang from that; our own
/// deliberate resets clear the marker first (`forget_watchdog_marker`).
const WATCHDOG_MARKER: u32 = 0x7C10_C10C;

pub fn watchdog_start(mut w: embassy_rp::watchdog::Watchdog<'static>) {
    heartbeat();
    w.start(WATCHDOG_TIMEOUT);
    w.set_scratch(0, WATCHDOG_MARKER);
    WATCHDOG.lock(|d| *d.borrow_mut() = Some(w));
}

/// Whether the last restart was the watchdog firing while the program ran. Clears the marker; reads it from `w` before the
/// watchdog is started again.
pub fn watchdog_caused_last_reset(w: &mut embassy_rp::watchdog::Watchdog<'static>) -> bool {
    let marked = w.scratch(0) == WATCHDOG_MARKER;
    w.set_scratch(0, 0);
    marked && matches!(w.reset_reason(), Some(embassy_rp::watchdog::ResetReason::TimedOut))
}

/// Call before a deliberate restart (`reboot`, `bootsel`, factory reset) so it is not reported as a hang. The watchdog is
/// stopped too: a core reset does not reset it, and its countdown must not run on into the next start.
pub fn forget_watchdog_marker() {
    WATCHDOG.lock(|d| {
        if let Some(w) = d.borrow_mut().as_mut() {
            w.set_scratch(0, 0);
            w.stop();
        }
    });
}

pub fn feed_watchdog() {
    WATCHDOG.lock(|d| {
        if let Some(w) = d.borrow_mut().as_mut() {
            w.feed(WATCHDOG_TIMEOUT)
        }
    });
}

/// Button events from the button task to the UI.
pub static BUTTONS: Channel<Cs, (Button, Press), 8> = Channel::new();
/// A time a source (NTP, DCF77) delivered, on its way to the source selection (`sources.rs`).
pub struct Offer {
    pub src: picoclock_core::sources::Source,
    pub sample: picoclock_core::sources::Sample,
    /// Authenticated (NTS).
    pub secure: bool,
}
pub static SAMPLES: Channel<Cs, Offer, 4> = Channel::new();
/// The time the selection chose, for the clock task to write to the RTC: (unix seconds, ms, the instant that time was true).
pub static TIME_APPLY: Signal<Cs, (u64, u16, embassy_time::Instant)> = Signal::new();
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
/// Erase every stored setting, root and certificate, then restart (`factory-reset`, web page). The storage task does it.
pub static FACTORY_RESET: Signal<Cs, ()> = Signal::new();
pub static SYNC_NOW: Signal<Cs, ()> = Signal::new();
pub static GEO_NOW: Signal<Cs, ()> = Signal::new();
/// Ask GitHub now whether a newer release exists (`update check`, the web page's button).
pub static UPDATE_NOW: Signal<Cs, ()> = Signal::new();
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

/// The NTS trust anchors the user added. Loaded from flash at boot; changed by `root add` / `root remove` and the web page.
static ROOTS: Mutex<Tm, RefCell<picoclock_core::rootstore::Roots>> = Mutex::new(RefCell::new(picoclock_core::rootstore::Roots::new()));
/// Ask the storage task to write the roots to their flash sector.
pub static ROOTS_SAVE: Signal<Cs, ()> = Signal::new();

pub fn roots_snapshot() -> picoclock_core::rootstore::Roots {
    ROOTS.lock(|r| r.borrow().clone())
}

/// Runs `f` on the roots; if it reports a change (the `bool`) they are saved to flash. `status` is updated either way.
pub fn change_roots<R>(f: impl FnOnce(&mut picoclock_core::rootstore::Roots) -> (bool, R)) -> R {
    let (changed, out, n) = ROOTS.lock(|r| {
        let mut r = r.borrow_mut();
        let (changed, out) = f(&mut r);
        (changed, out, r.len() as u8)
    });
    with_runtime(|rt| rt.roots_user = n);
    if changed {
        ROOTS_SAVE.signal(());
    }
    out
}

/// At boot: the roots read from flash (not written back).
pub fn set_roots(roots: picoclock_core::rootstore::Roots) {
    let n = roots.len() as u8;
    ROOTS.lock(|r| *r.borrow_mut() = roots);
    with_runtime(|rt| rt.roots_user = n);
}

pub fn with_config<R>(f: impl FnOnce(&mut Config) -> R) -> R {
    CONFIG.lock(|c| f(c.borrow_mut().get_or_insert_with(Config::default)))
}

pub fn config_snapshot() -> Config {
    with_config(|c| c.clone())
}

/// At boot: the configuration read from flash (or the defaults).
pub fn set_config(cfg: Config) {
    PERSISTED.lock(|p| *p.borrow_mut() = Some(cfg.clone()));
    with_config(|c| *c = cfg);
}

/// The configuration the storage task writes.
pub fn persisted_snapshot() -> Config {
    PERSISTED.lock(|p| p.borrow_mut().get_or_insert_with(Config::default).clone())
}

/// `save` in the terminal: everything, including the terminal's own edits, is to be written.
pub fn persist_all() {
    let live = config_snapshot();
    PERSISTED.lock(|p| *p.borrow_mut() = Some(live));
}

/// Whether the live configuration differs from what flash holds, for `status`.
pub fn refresh_unsaved() {
    let differs = persisted_snapshot() != config_snapshot();
    with_runtime(|r| r.unsaved = differs);
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
/// [`save_change`].
pub fn mark_edited() {
    refresh_unsaved();
    CONFIG_CHANGED.signal(());
}

/// Runs `f` on the live configuration and, if it changed something, saves exactly that change (web page, location cache).
/// Unsaved terminal edits stay unsaved. `save_now` skips the quiet-period debounce. Returns what `f` returned.
pub fn edit_and_save<R>(save_now: bool, f: impl FnOnce(&mut Config) -> R) -> R {
    let (r, before, after) = with_config(|c| {
        let before = c.clone();
        let r = f(c);
        (r, before, c.clone())
    });
    if before != after {
        carry_to_persisted(&before, &after, save_now);
    }
    r
}

/// For a change already made to the live configuration (the buttons): save exactly the difference.
pub fn carry_to_persisted(before: &Config, after: &Config, save_now: bool) {
    PERSISTED.lock(|p| p.borrow_mut().get_or_insert_with(Config::default).carry_changes(before, after));
    refresh_unsaved();
    SAVE_REQUEST.signal(save_now);
    CONFIG_CHANGED.signal(());
}
