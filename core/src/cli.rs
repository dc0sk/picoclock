//! Terminal command interpreter for the USB serial console. Pure: it edits a [`Config`], writes text, and returns
//! an [`Action`] for the firmware to carry out. Line editing and I/O live in the firmware.

use core::fmt::Write;

use crate::config::{Config, KEYS};
use crate::time::unix_to_datetime;
use crate::tz;

pub const MAX_LINE: usize = 160;

/// Runtime facts the firmware supplies for `status` and `time`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Status {
    pub utc_unix: Option<i64>,
    pub last_sync_unix: Option<i64>,
    pub wifi_up: bool,
    /// Which stored network (1-based) the clock is connected to.
    pub wifi_slot: Option<u8>,
    /// The WiFi MAC address; its last octet names the device on the network unless `hostname` is set.
    pub mac: Option<[u8; 6]>,
    pub ip: Option<[u8; 4]>,
    pub gateway: Option<[u8; 4]>,
    /// Tenths of a degree Celsius.
    pub temp_c10: Option<i16>,
    /// RP2040 die temperature, tenths of a degree Celsius (reads warmer than the room, see README).
    pub chip_temp_c10: Option<i16>,
    pub light_raw: u16,
    /// Display scan timing since boot: worst lateness of the row timer (us), waits later than 100 us, uptime (s).
    pub scan_late_max_us: u32,
    pub scan_late_count: u32,
    pub uptime_s: u32,
    /// The last attempt at each timezone-lookup provider, indexed like `geo::PROVIDERS`.
    pub geo: [GeoTry; crate::geo::PROVIDERS.len()],
    /// The last sync was authenticated (NTS).
    pub sync_secure: bool,
    /// The HTTPS server: 0 = not part of this build, 1 = starting (needs a valid time), 2 = serving.
    pub https_state: u8,
    /// Random numbers come from a hardware TRNG (RP2350) rather than ring-oscillator jitter (RP2040).
    pub rng_hardware: bool,
    /// NTP servers named by the DHCP lease (option 42); `dhcp_ntp_n` of the four entries are set.
    pub dhcp_ntp: [[u8; 4]; 4],
    pub dhcp_ntp_n: u8,
    /// Access points the last scan for `wifi_select strongest` saw (`None` = no scan was made).
    pub wifi_scanned: Option<u8>,
    /// mDNS: 0 multicast group not joined yet, 1 joined, 2 join failed; packets received on UDP 5353 and answers sent.
    pub mdns_group: u8,
    pub mdns_seen: u16,
    pub mdns_answered: u16,
    /// Trust anchors the user added for NTS.
    pub roots_user: u8,
    /// The last NTS key exchange: 0 none yet, 1 ok, 2 certificate or TLS refused, 3 no agreement, 4 bad reply, 5 no connection or timeout;
    /// how long it took (ms), and the server name (first 32 bytes).
    pub nts_ke: u8,
    pub nts_ke_ms: u16,
    pub nts_ke_host: [u8; 32],
    /// HTTPS heap, bytes: in use now, the most ever in use at once, and its size (0 where there is no heap).
    pub heap_used: u32,
    pub heap_peak: u32,
    pub heap_size: u32,
    /// SHA-256 of the HTTPS certificate in use, its end of validity (unix seconds), and whether the user uploaded it.
    pub cert_fp: [u8; 32],
    pub cert_not_after: i64,
    pub cert_uploaded: bool,
    /// Config differs from what is saved in flash.
    pub unsaved: bool,
    /// The previous start was a reset by the watchdog (a task hung or the program crashed).
    pub watchdog_reset: bool,
    /// The source that last set the clock (`sync_secure` says whether an NTP time was authenticated).
    pub sync_source: crate::sources::Source,
    /// What the source selection made of each source's last time (index = `Source as usize`); `None` before the first look.
    pub sources: [Option<crate::sources::Report>; crate::sources::SOURCES.len()],
    /// DCF77 receiver counters.
    pub dcf: crate::dcf77::Stats,
    pub gps: crate::gps::Report,
    /// The DHCP server that gave the lease, the prefix length and the first DNS server (all from the lease).
    pub dhcp_server: Option<[u8; 4]>,
    pub prefix: u8,
    pub dns: Option<[u8; 4]>,
    /// How often the server, address or gateway changed while the link stayed up (a second DHCP server answering a renewal).
    pub lease_changes: u8,
    /// The signal of the joined access point (dBm) and the access points with the stored name that a scan right after the
    /// join saw, strongest first.
    pub link_rssi: Option<i16>,
    pub aps: [Option<ApSeen>; 6],
    /// The firmware update check: its state, the newest release it saw and when it last asked (unix seconds).
    pub update: crate::update::Check,
    pub update_latest: Option<crate::update::Version>,
    pub update_checked_unix: Option<i64>,
    /// Signing keys a release revoked (bitmask, bit 0 = key 1).
    pub revoked_keys: u8,
    /// Where an uploaded firmware image stands (ADR-0006).
    pub install: crate::image::Install,
    /// This is a custom build (cargo feature `custom-images`): uploaded images are not checked against the signing keys.
    pub custom_images: bool,
    /// This build can fetch the update from GitHub itself (Pico 2 W; not in a custom build).
    pub can_download: bool,
}

/// One access point from the scan after a join.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApSeen {
    pub bssid: [u8; 6],
    pub channel: u8,
    pub rssi: i16,
}

/// One timezone-lookup attempt, as shown in `status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GeoTry {
    /// 0 never tried, 1 DNS failed, 2 connect failed, 3 request failed, 4 no usable reply, 5 ok.
    pub stage: u8,
    /// Bytes received in the reply.
    pub bytes: u16,
    /// How the read ended: 0 timeout, 1 peer closed, 2 read error.
    pub end: u8,
    /// The address the host name resolved to.
    pub addr: [u8; 4],
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Action {
    None,
    /// A setting changed; the firmware should apply it (e.g. rejoin WiFi, redraw).
    Changed,
    Save,
    Defaults,
    Reboot,
    Bootsel,
    SyncNow,
    Geolocate,
    /// Replace the HTTPS certificate with a new self-signed one.
    NewCertificate,
    /// `root` / `root list`: show the trust anchors for NTS.
    RootList,
    /// `root add`: the next lines are a pasted PEM certificate (see [`Paste`]).
    RootAdd,
    /// `root remove <n>`: remove your own root number n (1-based, as listed).
    RootRemove(usize),
    /// `update check`: ask GitHub now whether a newer release exists (nothing is installed).
    UpdateCheck,
    /// `update download`: fetch the update file of the newest release from GitHub into the download slot.
    ImageDownload,
    /// `update upload <bytes>`: the session now takes that many raw bytes (an update file: the image, then its 64-byte signature).
    ImageUpload(usize),
    /// `update apply`: the session asks for confirmation (see [`INSTALL_WORD`]), then swaps the staged image in and restarts.
    ImageApply,
    /// `gps reset hot|warm|cold|hard`: restart the receiver (REQ-GPS-08). `hard` pulls its reset pin (nRST) low.
    GpsReset(GpsReset),
    /// `factory-reset`: the session asks for confirmation (see [`reset_confirmed`]) before anything is erased.
    FactoryReset,
}

/// How a receiver is restarted by `gps reset`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpsReset {
    /// A UBX software reset that keeps (hot), forgets the ephemerides of (warm) or forgets everything stored by (cold) the receiver.
    Soft(crate::ubx::ResetKind),
    /// The reset pin.
    Hard,
}

/// What the user must type (terminal) or enter (web page) to confirm a factory reset.
pub const RESET_WORD: &str = "RESET";

/// Shown by `version`, `update` and `status` in a custom build.
pub const CUSTOM_NOTICE: &str = "custom build: images you upload are NOT checked against the signing keys, and the update check only informs (README, \"Custom builds\")\n";

/// What the user must type to confirm that a staged image is installed.
pub const INSTALL_WORD: &str = "INSTALL";

/// `true` if `answer` is exactly the install confirmation word.
pub fn install_confirmed(answer: &str) -> bool {
    answer.trim() == INSTALL_WORD
}

/// What a factory reset erases, shown before the confirmation on both interfaces.
pub const RESET_WARNING: &str = "A factory reset erases ALL settings, every stored WiFi network and password, the web password, \
the trusted NTS roots you added and the HTTPS certificate and key, then restarts the clock. It will not be on the network \
again until you give it a WiFi network over the USB terminal.";

/// `true` if `answer` is exactly the confirmation word (surrounding spaces ignored; case matters).
pub fn reset_confirmed(answer: &str) -> bool {
    answer.trim() == RESET_WORD
}

/// The longest answer the terminal sends (`firmware/src/usb_cli.rs` has a buffer of this size and cuts what is longer); `help`
/// has to fit, or the settings listed last cannot be found.
pub const OUTPUT_MAX: usize = 3000;

/// Longest pasted certificate text (one PEM certificate is about 2.1 KB for the largest we accept).
pub const PASTE_MAX: usize = 3000;

/// Collects the lines of a pasted PEM certificate after `root add`, so the terminal need not hold a whole certificate on one
/// line. It ends at the `-----END CERTIFICATE-----` line; `cancel` (or Ctrl-C in the terminal) gives up.
#[derive(Default)]
pub struct Paste {
    text: heapless::String<PASTE_MAX>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PasteStep {
    /// Keep sending lines.
    More,
    /// The certificate text is complete.
    Done(heapless::String<PASTE_MAX>),
    Cancelled,
    /// More than [`PASTE_MAX`] bytes: not a certificate we accept.
    TooLong,
}

impl Paste {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, line: &str) -> PasteStep {
        let line = line.trim();
        if line.eq_ignore_ascii_case("cancel") {
            return PasteStep::Cancelled;
        }
        if self.text.push_str(line).is_err() || self.text.push('\n').is_err() {
            return PasteStep::TooLong;
        }
        if line.starts_with("-----END ") {
            return PasteStep::Done(core::mem::take(&mut self.text));
        }
        PasteStep::More
    }
}

/// The `root` listing: the built-in authorities, then the user's own with their numbers.
pub fn root_listing(roots: &crate::rootstore::Roots, now: Option<i64>, out: &mut impl Write) {
    let _ = writeln!(out, "built in: {} certificate authorities (ISRG Root X2, SSL.com ECC{})", crate::nts_ke::BUILT_IN_ROOTS.len(), if cfg!(feature = "rsa") { ", ISRG Root X1" } else { "; RSA chains are not supported on this board" });
    if roots.is_empty() {
        let _ = out.write_str("your own: none (add one with 'root add')\n");
    }
    for i in 0..roots.len() {
        match roots.describe(i) {
            Some(info) => {
                let _ = writeln!(out, "{}", crate::rootstore::summary(i + 1, &info, now));
            }
            None => {
                let _ = writeln!(out, "{}  (unreadable)", i + 1);
            }
        }
    }
}

const HELP: &str = "\
commands:
  status                show WiFi, time sync and sensor state
  show                  list all settings
  get <key>             show one setting
  set <key> <value>     change a setting (not saved until 'save')
  save                  write settings to flash
  defaults              reset all settings to defaults (then 'save')
  time                  show UTC and local time
  tz [prefix]           list known timezone names, e.g. 'tz europe/'
  sync                  query NTP now
  gps reset <how>       restart the GPS receiver: hot, warm, cold (u-blox, needs gps_config on) or hard (its reset pin)
  geo                   look up the timezone from the public IP now (needs timezone=auto)
  root                  list the certificate authorities trusted for NTS (built in and your own)
  root add              paste a CA certificate (PEM) to trust for NTS; then it ends at the END line
  root remove <n>       remove one of your own roots
  update [check]        show whether a newer release exists on GitHub; 'update check' asks now (nothing is installed)
  update download       fetch the newest release from GitHub into the download slot (Pico 2 W; then 'update apply')
  update apply          install the uploaded image that waits in the download slot (asks you to confirm)
  version               show the software version, source repository and license
  cert                  show the HTTPS certificate (fingerprint, validity)
  cert new              replace the HTTPS certificate with a new self-signed one
  reboot | bootsel      restart / enter USB flashing mode
  factory-reset         erase ALL settings, WiFi networks, trusted roots and the HTTPS certificate (asks again first)
  help                  this text
";

/// `YYYY-MM-DD` of a unix time.
fn date(unix: i64) -> heapless::String<12> {
    let d = crate::time::unix_to_datetime(unix);
    let mut s = heapless::String::new();
    let _ = write!(s, "{:04}-{:02}-{:02}", d.year, d.month, d.day);
    s
}

fn ip(out: &mut impl Write, a: Option<[u8; 4]>) {
    match a {
        Some([a, b, c, d]) => {
            let _ = write!(out, "{a}.{b}.{c}.{d}");
        }
        None => {
            let _ = out.write_str("-");
        }
    }
}

fn write_dt(out: &mut impl Write, unix: i64, offset: i32) {
    let d = unix_to_datetime(unix + offset as i64);
    let _ = write!(out, "{:04}-{:02}-{:02} {:02}:{:02}:{:02}", d.year, d.month, d.day, d.hour, d.minute, d.second);
    if offset == 0 {
        let _ = out.write_str(" UTC");
    } else {
        let s = if offset < 0 { '-' } else { '+' };
        let o = offset.unsigned_abs();
        let _ = write!(out, " (UTC{s}{:02}:{:02})", o / 3600, o % 3600 / 60);
    }
}

pub fn execute(line: &str, cfg: &mut Config, st: &Status, out: &mut impl Write) -> Action {
    let line = line.trim();
    let (cmd, rest) = match line.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (line, ""),
    };
    match cmd {
        "" => Action::None,
        "help" | "?" => {
            let _ = out.write_str(HELP);
            let _ = out.write_str("keys: ");
            for (i, k) in KEYS.iter().enumerate() {
                let _ = write!(out, "{}{}", if i > 0 { ", " } else { "" }, k.name);
            }
            let _ = out.write_str("\n");
            Action::None
        }
        "show" => {
            for k in KEYS {
                let _ = write!(out, "{:<14} = ", k.name);
                let _ = cfg.write_display(k.name, out);
                let _ = out.write_str("\n");
            }
            if st.unsaved {
                let _ = out.write_str("(unsaved changes: type 'save')\n");
            }
            Action::None
        }
        "get" => {
            if rest.is_empty() {
                let _ = out.write_str("usage: get <key>\n");
            } else if cfg.write_display(rest, &mut NullWriter).is_err() {
                let _ = writeln!(out, "unknown key '{rest}'");
            } else {
                let _ = cfg.write_display(rest, out);
                let _ = out.write_str("\n");
            }
            Action::None
        }
        "set" => {
            let Some((key, value)) = rest.split_once(char::is_whitespace).or(if rest.is_empty() { None } else { Some((rest, "")) }) else {
                let _ = out.write_str("usage: set <key> <value>\n");
                return Action::None;
            };
            match cfg.set(key, value) {
                Ok(()) => {
                    let _ = writeln!(out, "ok (not saved yet; 'save' to keep)");
                    Action::Changed
                }
                Err(crate::config::ConfigError::UnknownKey) => {
                    let _ = writeln!(out, "unknown key '{key}' (see 'help')");
                    Action::None
                }
                Err(_) => {
                    let _ = writeln!(out, "invalid value for {key}");
                    if let Some(k) = KEYS.iter().find(|k| k.name == key) {
                        let _ = writeln!(out, "  {}", k.help);
                    }
                    Action::None
                }
            }
        }
        "save" => Action::Save,
        "defaults" => {
            *cfg = Config::default();
            let _ = out.write_str("defaults loaded (not saved yet; 'save' to keep)\n");
            Action::Changed
        }
        "time" => {
            match st.utc_unix {
                None => {
                    let _ = out.write_str("clock not set yet\n");
                }
                Some(t) => {
                    let _ = out.write_str("utc   : ");
                    write_dt(out, t, 0);
                    let _ = out.write_str("\nlocal : ");
                    let off = if cfg.shows_utc() { 0 } else { cfg.effective_tz().offset_at(t) };
                    write_dt(out, t, off);
                    let _ = out.write_str("\n");
                }
            }
            Action::None
        }
        "tz" => {
            const MAX_LISTED: usize = 40;
            let total = tz::zones_with_prefix(rest).count();
            for z in tz::zones_with_prefix(rest).take(MAX_LISTED) {
                let _ = writeln!(out, "{z}");
            }
            if total == 0 {
                let _ = writeln!(out, "no zone starts with '{rest}'");
            } else if total > MAX_LISTED {
                let _ = writeln!(out, "... {} more; narrow it down, e.g. 'tz europe/b'", total - MAX_LISTED);
            }
            Action::None
        }
        "status" => {
            let _ = write!(out, "wifi    : {}", if st.wifi_up { "connected" } else { "not connected" });
            if let (true, Some(n)) = (st.wifi_up, st.wifi_slot) {
                let _ = write!(out, " (slot {n})");
            }
            if let (true, Some(seen)) = (st.wifi_up, st.wifi_scanned) {
                let _ = write!(out, ", chosen by signal from {seen} access points seen");
            }
            if let Some(m) = st.mac {
                let _ = write!(out, "\nname    : {}", cfg.device_name(m[5]));
                let _ = write!(out, "\nmac     : {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
            }
            let _ = out.write_str("\nip      : ");
            ip(out, st.ip);
            let _ = out.write_str("\ngateway : ");
            ip(out, st.gateway);
            if let Some(server) = st.dhcp_server {
                let _ = write!(out, "\ndhcp    : server {}.{}.{}.{}, /{}", server[0], server[1], server[2], server[3], st.prefix);
                if let Some(d) = st.dns {
                    let _ = write!(out, ", dns {}.{}.{}.{}", d[0], d[1], d[2], d[3]);
                }
                if st.lease_changes > 0 {
                    let _ = write!(out, ", CHANGED {} time(s) while connected", st.lease_changes);
                }
            }
            if st.aps[0].is_some() || st.link_rssi.is_some() {
                let _ = out.write_str("\nap      : ");
                match st.link_rssi {
                    Some(r) => { let _ = write!(out, "link {} dBm", r); }
                    None => { let _ = out.write_str("link signal unknown"); }
                }
                let closest = st.link_rssi.and_then(|l| st.aps.iter().flatten().enumerate().min_by_key(|(_, a)| (a.rssi - l).abs()).map(|(i, _)| i));
                for (i, a) in st.aps.iter().flatten().enumerate() {
                    let b = a.bssid;
                    let _ = write!(out, "{} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ch{} {}dBm{}", if i == 0 { ";" } else { "," }, b[0], b[1], b[2], b[3], b[4], b[5], a.channel, a.rssi, if closest == Some(i) { " <" } else { "" });
                }
            }
            let _ = out.write_str("\nsynced  : ");
            match (st.last_sync_unix, st.utc_unix) {
                (Some(s), Some(now)) => {
                    let how = match st.sync_source {
                        crate::sources::Source::Dcf77 => "DCF77",
                        crate::sources::Source::Gps => "GPS",
                        crate::sources::Source::Ntp if st.sync_secure => "NTS, authenticated",
                        crate::sources::Source::Ntp => "plain NTP",
                    };
                    let _ = write!(out, "{} s ago ({})", (now - s).max(0), how);
                }
                _ => {
                    let _ = out.write_str("never");
                }
            }
            let _ = out.write_str("\nupdate  : ");
            crate::update::describe(out, cfg.update_check, st.update, st.update_latest, st.update_checked_unix, st.utc_unix);
            if st.install != crate::image::Install::Idle {
                let _ = out.write_str("\n");
                crate::image::describe_install(out, st.install);
            }
            if st.custom_images {
                let _ = out.write_str("\nimages  : custom build, uploads are not signature-checked");
            } else if st.revoked_keys != 0 {
                let _ = out.write_str("\n");
                crate::image::describe_keys(out, st.revoked_keys);
            }
            let _ = write!(out, "\nsources : mode {}", cfg.time_mode.name());
            if cfg.time_mode == crate::sources::Mode::Vote {
                let _ = write!(out, ", tolerance {} s", cfg.vote_tolerance);
            }
            for src in crate::sources::SOURCES {
                let _ = write!(out, "\n  {:6}: ", src.name());
                let Some(r) = st.sources[src as usize] else {
                    let _ = out.write_str(if cfg.time_sources.contains(&src) { "waiting" } else { "off" });
                    continue;
                };
                let _ = out.write_str(r.state.text());
                if let Some(age) = r.age_s {
                    let _ = write!(out, ", last {} s ago", age);
                }
                if let Some(o) = r.offset_ms {
                    let _ = write!(out, ", {}{}.{:03} s from the clock", if o < 0 { "-" } else { "+" }, o.abs() / 1000, o.abs() % 1000);
                }
                if cfg.time_mode == crate::sources::Mode::Vote && cfg.time_sources.contains(&src) {
                    let _ = write!(out, ", vote power {}", cfg.vote_power[src as usize]);
                }
                if src == crate::sources::Source::Gps && cfg.time_sources.contains(&src) {
                    let _ = out.write_str("; ");
                    let _ = crate::gps::describe(out, &st.gps);
                }
                if src == crate::sources::Source::Dcf77 && cfg.time_sources.contains(&src) {
                    let _ = write!(out, "; pulses {}, frames ok {}, bad {}, bad pulses {}; receiver delay {} ms ({})", st.dcf.pulses, st.dcf.frames_ok, st.dcf.frames_bad, st.dcf.pulses_bad, cfg.dcf77_delay(), if cfg.dcf77_auto { "learned" } else { "manual" });
                }
            }
            for (label, t) in [("temp    ", st.temp_c10), ("chip    ", st.chip_temp_c10)] {
                if let Some(t) = t {
                    let _ = write!(out, "\n{}: {}{}.{} C", label, if t < 0 { "-" } else { "" }, (t / 10).abs(), (t % 10).abs());
                }
            }
            let _ = write!(out, "\nlight   : {} (raw ADC)", st.light_raw);
            for (p, t) in crate::geo::PROVIDERS.iter().zip(st.geo.iter()) {
                let what = match t.stage {
                    0 => {
                        let _ = write!(out, "\ngeo     : {}: never tried", p.host);
                        continue;
                    }
                    1 => "DNS lookup failed",
                    2 => "connect failed",
                    3 => "request failed",
                    4 => "no usable reply",
                    _ => "ok",
                };
                let end = match t.end {
                    0 => "read timed out",
                    1 => "peer closed the connection",
                    _ => "read error",
                };
                let _ = write!(out, "\ngeo     : {}: {what} ({} bytes, {end}, server {}.{}.{}.{})", p.host, t.bytes, t.addr[0], t.addr[1], t.addr[2], t.addr[3]);
            }
            if st.wifi_up {
                let _ = out.write_str("\ndhcp ntp: ");
                if st.dhcp_ntp_n == 0 {
                    let _ = out.write_str("none offered by the router (option 42)");
                }
                for (i, a) in st.dhcp_ntp.iter().take(st.dhcp_ntp_n as usize).enumerate() {
                    let _ = write!(out, "{}{}.{}.{}.{}", if i > 0 { ", " } else { "" }, a[0], a[1], a[2], a[3]);
                }
            }
            if st.wifi_up {
                let group = match st.mdns_group {
                    0 => "group not joined",
                    1 => "group joined",
                    _ => "group join failed",
                };
                let _ = write!(out, "\nmdns    : {}, {group}, {} queries seen, {} answered", if cfg.mdns { "on" } else { "off" }, st.mdns_seen, st.mdns_answered);
            }
            if st.nts_ke != 0 {
                let host = core::str::from_utf8(&st.nts_ke_host).unwrap_or("?").trim_end_matches('\0');
                let what = match st.nts_ke {
                    1 => "ok",
                    2 => "refused: TLS or the certificate (no trusted root, wrong name or dates)",
                    3 => "refused: the server does not offer NTPv4 with AES-SIV-CMAC-256",
                    4 => "refused: bad or incomplete reply",
                    _ => "no connection, or timed out",
                };
                let _ = write!(out, "\nnts ke  : {host}: {what} ({} ms)", st.nts_ke_ms);
            }
            let _ = write!(out, "\nnts roots: {} built in, {} of your own", crate::nts_ke::BUILT_IN_ROOTS.len(), st.roots_user);
            let _ = write!(out, "\nrandom  : {}", if st.rng_hardware { "hardware TRNG" } else { "ring-oscillator jitter (no hardware TRNG on this chip)" });
            if st.heap_size > 0 {
                let _ = write!(out, "\nheap    : {} of {} bytes in use, peak {} ({}%)", st.heap_used, st.heap_size, st.heap_peak, st.heap_peak as u64 * 100 / st.heap_size as u64);
            }
            let _ = write!(out, "\nhttps   : ");
            match (st.https_state, cfg.https) {
                (0, _) => { let _ = out.write_str("not in this build (Pico 2 W only)"); }
                (_, false) => { let _ = out.write_str("off"); }
                (_, true) if !cfg.web_enabled => { let _ = out.write_str("off (the web interface is switched off: web_enabled)"); }
                (1, true) => { let _ = out.write_str("starting (needs a valid time)"); }
                _ => {
                    let _ = write!(out, "on, certificate {} ({}), valid until {}", crate::x509::fingerprint_hex(&st.cert_fp), if st.cert_uploaded { "uploaded" } else { "self-signed" }, date(st.cert_not_after));
                }
            }
            let _ = write!(out, "\nscan    : worst late {} us, {} waits > 100 us in {} s\n", st.scan_late_max_us, st.scan_late_count, st.uptime_s);
            if st.watchdog_reset {
                let _ = out.write_str("reset   : the last restart was caused by the watchdog (the clock had hung)\n");
            }
            if st.unsaved {
                let _ = out.write_str("unsaved changes: type 'save'\n");
            }
            Action::None
        }
        "root" => match rest.split_once(char::is_whitespace).map_or((rest, ""), |(a, b)| (a, b.trim())) {
            ("" | "list", "") => Action::RootList,
            ("add", "") => Action::RootAdd,
            ("remove", n) => match n.parse::<usize>() {
                Ok(n) if n >= 1 => Action::RootRemove(n),
                _ => {
                    let _ = out.write_str("usage: root remove <number from 'root'>\n");
                    Action::None
                }
            },
            _ => {
                let _ = out.write_str("usage: root | root add | root remove <n>\n");
                Action::None
            }
        },
        "version" | "about" => {
            let _ = writeln!(out, "picoclock {}\nsource : {}\nlicense: {}", crate::VERSION, crate::REPOSITORY, crate::LICENSE);
            if st.custom_images {
                let _ = out.write_str(CUSTOM_NOTICE);
            }
            Action::None
        }
        "sync" => Action::SyncNow,
        "gps" => {
            let how = match rest {
                "reset hot" => Some(GpsReset::Soft(crate::ubx::ResetKind::Hot)),
                "reset warm" => Some(GpsReset::Soft(crate::ubx::ResetKind::Warm)),
                "reset cold" => Some(GpsReset::Soft(crate::ubx::ResetKind::Cold)),
                "reset hard" => Some(GpsReset::Hard),
                _ => None,
            };
            match how {
                Some(GpsReset::Soft(_)) if !cfg.gps_config => {
                    let _ = out.write_str("gps    : a software reset is a u-blox command; set gps_config on first (or use 'gps reset hard')\n");
                    Action::None
                }
                Some(h) => Action::GpsReset(h),
                None => {
                    let _ = out.write_str("usage  : gps reset hot|warm|cold|hard   (hot keeps what the receiver knows, cold forgets everything and needs minutes for a fix, hard pulls the reset pin)\n");
                    Action::None
                }
            }
        }
        "update" => match rest {
            "" => {
                let _ = out.write_str("update  : ");
                crate::update::describe(out, cfg.update_check, st.update, st.update_latest, st.update_checked_unix, st.utc_unix);
                let _ = out.write_str("\n");
                if st.install != crate::image::Install::Idle {
                    crate::image::describe_install(out, st.install);
                    let _ = out.write_str("\n");
                }
                if !st.custom_images {
                    crate::image::describe_keys(out, st.revoked_keys);
                    let _ = out.write_str("\n");
                }
                if st.custom_images {
                    let _ = out.write_str(CUSTOM_NOTICE);
                }
                Action::None
            }
            "check" => {
                let _ = out.write_str("asking GitHub for the latest release (the answer shows in 'update' in a few seconds)\n");
                Action::UpdateCheck
            }
            "download" => match (st.custom_images, st.can_download, st.update, st.update_latest, st.install) {
                (true, ..) => {
                    let _ = out.write_str("a custom build does not download releases; upload your own file (update upload)\n");
                    Action::None
                }
                (_, false, ..) => {
                    let _ = out.write_str("this board cannot download: it has no HTTPS to GitHub's download host (Pico W). Download the .pcu file on a computer and upload it (tools/upload_image.py, or the web page's Security tab)\n");
                    Action::None
                }
                (_, _, _, _, crate::image::Install::Receiving | crate::image::Install::Downloading(_)) => {
                    let _ = out.write_str("an update file is being received; wait until it is done\n");
                    Action::None
                }
                (_, _, crate::update::Check::Newer, Some(v), _) => {
                    let _ = writeln!(out, "downloading {v} from GitHub (about a minute; 'update' shows how it went; nothing is installed until 'update apply')");
                    Action::ImageDownload
                }
                _ => {
                    let _ = out.write_str("no newer release is known; type 'update check' first\n");
                    Action::None
                }
            },
            "apply" => match st.install {
                crate::image::Install::Staged(v, _) => {
                    let _ = writeln!(out, "Install {v}? The clock restarts with it; if it does not start properly the bootloader brings the old version back.");
                    Action::ImageApply
                }
                _ => {
                    let _ = out.write_str("nothing is staged; upload an update file first (tools/upload_image.py, or the web page's Security tab)\n");
                    Action::None
                }
            },
            _ if rest.starts_with("upload ") => match rest[7..].trim().parse::<usize>() {
                Ok(n) if n > 0 && n <= 4 * 1024 * 1024 => Action::ImageUpload(n),
                _ => {
                    let _ = out.write_str("usage: update upload <size of the update file in bytes> (use tools/upload_image.py)\n");
                    Action::None
                }
            },
            _ => {
                let _ = out.write_str("usage: update | update check | update download | update upload <size> | update apply\n");
                Action::None
            }
        },
        "cert" => match (rest, st.https_state) {
            (_, 0) => {
                let _ = out.write_str("no HTTPS in this build (Pico 2 W only)\n");
                Action::None
            }
            (_, _) if !cfg.https && rest == "new" => {
                let _ = out.write_str("HTTPS is off: nothing to replace ('set https on' first)\n");
                Action::None
            }
            ("" | "show", 1) | ("new", 1) => {
                let _ = out.write_str("starting: the certificate is made once the clock has a valid time\n");
                Action::None
            }
            ("" | "show", _) => {
                let _ = writeln!(out, "fingerprint (SHA-256): {}", crate::x509::fingerprint_hex(&st.cert_fp));
                let _ = writeln!(out, "kind                 : {}", if st.cert_uploaded { "uploaded" } else { "self-signed, made by this device" });
                let _ = writeln!(out, "valid until          : {}", date(st.cert_not_after));
                Action::None
            }
            ("new", _) => {
                let _ = out.write_str("making a new self-signed certificate; browsers will ask you to trust it again\n");
                Action::NewCertificate
            }
            _ => {
                let _ = out.write_str("usage: cert | cert new\n");
                Action::None
            }
        },
        "geo" => {
            if cfg.wants_geolocation() {
                Action::Geolocate
            } else {
                let _ = out.write_str("set timezone auto first\n");
                Action::None
            }
        }
        "reboot" => Action::Reboot,
        "factory-reset" => Action::FactoryReset,
        "bootsel" => Action::Bootsel,
        other => {
            let _ = writeln!(out, "unknown command '{other}' (try 'help')");
            Action::None
        }
    }
}

struct NullWriter;
impl Write for NullWriter {
    fn write_str(&mut self, _: &str) -> core::fmt::Result {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cfg: &mut Config, line: &str) -> (Action, std::string::String) {
        let mut out = std::string::String::new();
        let a = execute(line, cfg, &Status::default(), &mut out);
        (a, out)
    }

    // VERIFIES: REQ-CFG-01 REQ-CFG-03 REQ-CLI-01
    #[test]
    fn set_get_show() {
        let mut c = Config::default();
        assert_eq!(run(&mut c, "set wifi_ssid My Home Net").0, Action::Changed);
        assert_eq!(c.wifi[0].ssid.as_str(), "My Home Net", "value keeps inner spaces");
        let (_, out) = run(&mut c, "get wifi_ssid");
        assert_eq!(out, "My Home Net\n");
        run(&mut c, "set wifi_password supersecret1");
        let (_, shown) = run(&mut c, "show");
        assert!(shown.contains("(set)") && !shown.contains("supersecret1"), "{shown}");
        let (_, out) = run(&mut c, "get wifi_password");
        assert_eq!(out, "(set)\n");
    }

    // VERIFIES: REQ-CFG-01
    #[test]
    fn errors_do_not_change_config() {
        let mut c = Config::default();
        let before = c.clone();
        let (a, out) = run(&mut c, "set brightness 500");
        assert_eq!((a, c == before), (Action::None, true));
        assert!(out.contains("invalid value") && out.contains("auto | 1..100"), "{out}");
        let (a, out) = run(&mut c, "set nokey 1");
        assert_eq!(a, Action::None);
        assert!(out.contains("unknown key"));
        assert!(run(&mut c, "get nokey").1.contains("unknown key"));
        assert!(run(&mut c, "frobnicate").1.contains("unknown command"));
        assert_eq!(run(&mut c, "set").0, Action::None);
        assert_eq!(run(&mut c, "").0, Action::None);
    }

    // VERIFIES: REQ-CLI-01
    #[test]
    fn set_without_value_clears_text_settings() {
        let mut c = Config::default();
        run(&mut c, "set web_password abc");
        assert_eq!(run(&mut c, "set web_password").0, Action::Changed);
        assert!(c.web_password.is_empty());
    }

    // VERIFIES: REQ-CLI-01 REQ-NTP-03 REQ-GPS-08
    #[test]
    fn actions() {
        let mut c = Config::default();
        assert_eq!(run(&mut c, "save").0, Action::Save);
        assert_eq!(run(&mut c, "sync").0, Action::SyncNow);
        let (a, out) = run(&mut c, "gps reset cold");
        assert_eq!((a, out.contains("gps_config on")), (Action::None, true), "a u-blox command is not sent unless the setup is on");
        assert_eq!(run(&mut c, "gps reset hard").0, Action::GpsReset(GpsReset::Hard));
        run(&mut c, "set gps_config on");
        assert_eq!(run(&mut c, "gps reset warm").0, Action::GpsReset(GpsReset::Soft(crate::ubx::ResetKind::Warm)));
        assert!(run(&mut c, "gps reset").1.contains("usage"));
        assert_eq!(run(&mut c, "reboot").0, Action::Reboot);
        assert_eq!(run(&mut c, "bootsel").0, Action::Bootsel);
        let (a, out) = run(&mut c, "geo");
        assert_eq!((a, out.contains("timezone auto")), (Action::None, true));
        run(&mut c, "set timezone auto");
        assert_eq!(run(&mut c, "geo").0, Action::Geolocate);
        run(&mut c, "set hour12 on");
        assert_eq!(run(&mut c, "defaults").0, Action::Changed);
        assert!(!c.hour12);
    }

    // VERIFIES: REQ-TIME-01 REQ-TIME-02 REQ-CLI-01
    #[test]
    fn time_shows_utc_and_local() {
        let mut c = Config::default();
        let st = Status { utc_unix: Some(1_720_000_000), ..Default::default() };
        let mut out = std::string::String::new();
        execute("time", &mut c, &st, &mut out);
        assert!(out.contains("utc   : 2024-07-03 09:46:40 UTC") && out.contains("local : 2024-07-03 09:46:40 UTC"), "{out}");
        c.set("timezone", "Europe/Berlin").unwrap();
        out.clear();
        execute("time", &mut c, &st, &mut out);
        assert!(out.contains("local : 2024-07-03 11:46:40 (UTC+02:00)"), "{out}");
        out.clear();
        execute("time", &mut c, &Status::default(), &mut out);
        assert!(out.contains("not set"));
    }

    // VERIFIES: REQ-TZ-02 REQ-CLI-01
    #[test]
    fn tz_listing_and_status() {
        let mut c = Config::default();
        let (_, out) = run(&mut c, "tz Europe/Ber");
        assert_eq!(out, "Europe/Berlin\n");
        assert!(run(&mut c, "tz zzz").1.contains("no zone"));
        let st = Status { wifi_up: true, ip: Some([192, 168, 1, 50]), gateway: Some([192, 168, 1, 1]), temp_c10: Some(-25), unsaved: true, ..Default::default() };
        let mut out = std::string::String::new();
        execute("status", &mut c, &st, &mut out);
        assert!(out.contains("192.168.1.50") && out.contains("connected") && out.contains("-2.5 C") && out.contains("unsaved"), "{out}");
    }

    // VERIFIES: REQ-NET-10
    #[test]
    fn status_names_the_dhcp_server_the_access_points_and_a_changed_lease() {
        let ap = |last: u8, ch, rssi| Some(ApSeen { bssid: [0xdc, 0x39, 0x6f, 0xce, 0x6d, last], channel: ch, rssi });
        let st = Status {
            wifi_up: true,
            ip: Some([192, 168, 121, 41]),
            gateway: Some([192, 168, 121, 1]),
            dhcp_server: Some([192, 168, 121, 1]),
            prefix: 24,
            dns: Some([192, 168, 121, 1]),
            link_rssi: Some(-53),
            aps: [ap(0xf9, 1, -51), ap(0xa0, 1, -85), None, None, None, None],
            ..Default::default()
        };
        let mut out = std::string::String::new();
        execute("status", &mut Config::default(), &st, &mut out);
        assert!(out.contains("dhcp    : server 192.168.121.1, /24, dns 192.168.121.1\n"), "{out}");
        assert!(out.contains("ap      : link -53 dBm; dc:39:6f:ce:6d:f9 ch1 -51dBm <, dc:39:6f:ce:6d:a0 ch1 -85dBm\n"), "the access point closest to the link signal is marked: {out}");
        assert!(!out.contains("CHANGED"));
        // a lease that changed while the link stayed up is said loudly
        let changed = Status { lease_changes: 2, ..st };
        let mut out = std::string::String::new();
        execute("status", &mut Config::default(), &changed, &mut out);
        assert!(out.contains("CHANGED 2 time(s) while connected"), "{out}");
        // nothing joined: no invented lines
        let mut out = std::string::String::new();
        execute("status", &mut Config::default(), &Status::default(), &mut out);
        assert!(!out.contains("dhcp    :") && !out.contains("ap      :"), "{out}");
    }

    // VERIFIES: REQ-HW-01 REQ-HW-02 REQ-NET-02
    #[test]
    fn negative_sub_degree_temperature() {
        let mut c = Config::default();
        let st = Status { temp_c10: Some(-5), ..Default::default() };
        let mut out = std::string::String::new();
        execute("status", &mut c, &st, &mut out);
        assert!(out.contains("temp    : -0.5 C"), "{out}");
        let st = Status { chip_temp_c10: Some(312), ..Default::default() };
        let mut out = String::new();
        execute("status", &mut Config::default(), &st, &mut out);
        assert!(out.contains("chip    : 31.2 C"), "{out}");
        let st = Status { wifi_up: true, wifi_slot: Some(2), mac: Some([0x28, 0xcd, 0xc1, 0x12, 0x34, 0xab]), ..Default::default() };
        let mut out = String::new();
        execute("status", &mut Config::default(), &st, &mut out);
        assert!(out.starts_with("wifi    : connected (slot 2)\nname    : picoclock-ab\nmac     : 28:cd:c1:12:34:ab\n"), "{out}");
        // a custom hostname replaces the default name; the MAC address stays what it is
        let mut named = Config::default();
        named.set("hostname", "Kitchen-Clock").unwrap();
        let mut out = String::new();
        execute("status", &mut named, &st, &mut out);
        assert!(out.contains("name    : kitchen-clock\nmac     : 28:cd:c1:12:34:ab\n"), "{out}");
    }

    // VERIFIES: REQ-CFG-09
    #[test]
    fn factory_reset_is_a_request_and_needs_the_exact_word() {
        let mut c = Config::default();
        c.set("hostname", "keepme").unwrap();
        let before = c.clone();
        assert_eq!(run(&mut c, "factory-reset").0, Action::FactoryReset);
        assert_eq!(c, before, "the command itself erases nothing; the session asks again first");
        assert!(reset_confirmed("RESET") && reset_confirmed("  RESET "));
        for no in ["", "reset", "Reset", "yes", "y", "RESET now", "RESE", "factory-reset"] {
            assert!(!reset_confirmed(no), "{no:?}");
        }
        assert!(run(&mut c, "help").1.contains("factory-reset"));
        assert!(RESET_WARNING.contains("WiFi") && RESET_WARNING.contains("certificate"), "the warning names what is lost");
    }

    // VERIFIES: REQ-CLI-02
    #[test]
    fn tz_listing_is_capped() {
        let mut c = Config::default();
        let (_, out) = run(&mut c, "tz");
        assert_eq!(out.lines().count(), 41, "40 names plus the 'more' line");
        assert!(out.contains("more; narrow it down"));
        assert!(out.len() < 1500, "must fit the firmware's output buffer, got {}", out.len());
    }

    // VERIFIES: REQ-CLI-01
    #[test]
    fn help_lists_every_key() {
        let mut c = Config::default();
        let (_, out) = run(&mut c, "help");
        for k in KEYS {
            assert!(out.contains(k.name), "{}", k.name);
        }
        assert!(out.len() <= OUTPUT_MAX - 100, "help is {} bytes; the terminal cuts at {}", out.len(), OUTPUT_MAX);
    }

    // VERIFIES: REQ-DSP-02 REQ-TZ-04
    #[test]
    fn status_shows_scan_timing_and_the_last_lookup() {
        let st = Status { scan_late_max_us: 588, scan_late_count: 37, uptime_s: 23, ..Default::default() };
        let mut out = String::new();
        execute("status", &mut Config::default(), &st, &mut out);
        assert!(out.contains("scan    : worst late 588 us, 37 waits > 100 us in 23 s"), "{out}");
        assert!(out.contains("geo     : ipwho.is: never tried\ngeo     : ip-api.com: never tried"), "{out}");
        for (stage, text) in [(1, "DNS lookup failed"), (2, "connect failed"), (3, "request failed"), (4, "no usable reply"), (5, "ok")] {
            let t = GeoTry { stage, bytes: 157, end: 2, addr: [208, 95, 112, 1] };
            let st = Status { geo: [t, GeoTry::default()], ..Default::default() };
            let mut out = String::new();
            execute("status", &mut Config::default(), &st, &mut out);
            assert!(out.contains(&format!("geo     : ipwho.is: {text} (157 bytes, read error, server 208.95.112.1)\ngeo     : ip-api.com: never tried")), "{out}");
        }
        // a failure of the first provider stays visible after the second succeeded
        let st = Status { geo: [GeoTry { stage: 2, ..Default::default() }, GeoTry { stage: 5, bytes: 120, end: 1, addr: [1, 2, 3, 4] }], ..Default::default() };
        let mut out = String::new();
        execute("status", &mut Config::default(), &st, &mut out);
        assert!(out.contains("ipwho.is: connect failed") && out.contains("ip-api.com: ok (120 bytes, peer closed the connection, server 1.2.3.4)"), "{out}");
    }

    // VERIFIES: REQ-NTS-04
    #[test]
    fn status_says_whether_the_last_sync_was_authenticated() {
        let base = Status { utc_unix: Some(1_000), last_sync_unix: Some(990), ..Default::default() };
        let line = |st: &Status| {
            let mut out = String::new();
            execute("status", &mut Config::default(), st, &mut out);
            out.lines().find(|l| l.starts_with("synced")).unwrap().to_string()
        };
        assert_eq!(line(&base), "synced  : 10 s ago (plain NTP)");
        assert_eq!(line(&Status { sync_secure: true, ..base }), "synced  : 10 s ago (NTS, authenticated)");
        assert_eq!(line(&Status { sync_source: crate::sources::Source::Gps, ..base }), "synced  : 10 s ago (GPS)");
        assert_eq!(line(&Status { sync_source: crate::sources::Source::Dcf77, sync_secure: true, ..base }), "synced  : 10 s ago (DCF77)");
        assert_eq!(line(&Status::default()), "synced  : never");
    }

    // VERIFIES: REQ-UPD-11
    #[test]
    fn update_and_status_name_the_revoked_signing_keys() {
        let mut c = Config::default();
        let run = |c: &mut Config, line: &str, st: &Status| {
            let mut out = String::new();
            execute(line, c, st, &mut out);
            out
        };
        let s = run(&mut c, "update", &Status::default());
        assert!(s.contains("keys    : key 1 trusted, key 2 trusted"), "{s}");
        let s = run(&mut c, "update", &Status { revoked_keys: 1, ..Default::default() });
        assert!(s.contains("keys    : key 1 REVOKED, key 2 trusted"), "{s}");
        assert!(run(&mut c, "status", &Status { revoked_keys: 1, ..Default::default() }).contains("key 1 REVOKED"));
        assert!(!run(&mut c, "status", &Status::default()).contains("keys    :"), "status stays short while nothing is revoked");
    }

    // VERIFIES: REQ-WEB-08
    #[test]
    fn cert_command_and_https_line_in_status() {
        let mut c = Config::default();
        let mut fp = [0u8; 32];
        fp[0] = 0xAB;
        fp[31] = 0x01;
        let ready = Status { https_state: 2, cert_fp: fp, cert_not_after: 2_086_000_000, cert_uploaded: false, ..Default::default() };
        let run = |c: &mut Config, line: &str, st: &Status| {
            let mut out = String::new();
            let a = execute(line, c, st, &mut out);
            (a, out)
        };
        let (_, s) = run(&mut c, "status", &ready);
        assert!(s.contains("https   : on, certificate AB:00:"), "{s}");
        assert!(s.contains(":01 (self-signed), valid until 2036-"), "{s}");
        let (_, s) = run(&mut c, "status", &Status { cert_uploaded: true, ..ready });
        assert!(s.contains("(uploaded)"), "{s}");
        let (_, s) = run(&mut c, "status", &Status { https_state: 0, ..Default::default() });
        assert!(s.contains("https   : not in this build"), "{s}");
        let (_, s) = run(&mut c, "status", &Status { https_state: 1, ..Default::default() });
        assert!(s.contains("https   : starting"), "{s}");
        c.set("https", "off").unwrap();
        let (_, s) = run(&mut c, "status", &ready);
        assert!(s.contains("https   : off"), "{s}");
        c.set("https", "on").unwrap();

        let (a, s) = run(&mut c, "cert", &ready);
        assert_eq!(a, Action::None);
        assert!(s.contains("fingerprint (SHA-256): AB:") && s.contains("self-signed, made by this device") && s.contains("valid until          : 2036-"), "{s}");
        assert_eq!(run(&mut c, "cert new", &ready).0, Action::NewCertificate);
        assert_eq!(run(&mut c, "cert new", &Status { https_state: 1, ..Default::default() }).0, Action::None, "nothing to replace yet");
        assert_eq!(run(&mut c, "cert new", &Status::default()).0, Action::None, "no HTTPS in this build");
        c.set("https", "off").unwrap();
        let (a, s) = run(&mut c, "cert new", &ready);
        assert_eq!((a, s.contains("HTTPS is off")), (Action::None, true), "{s}");
        c.set("https", "on").unwrap();
        assert!(run(&mut c, "cert", &Status::default()).1.contains("no HTTPS in this build"));
        assert!(run(&mut c, "cert bogus", &ready).1.contains("usage"));
        assert!(run(&mut c, "help", &ready).1.contains("cert new"));
    }

    // VERIFIES: REQ-CFG-06
    #[test]
    fn version_command_names_version_repository_and_license() {
        for cmd in ["version", "about"] {
            let mut out = String::new();
            assert_eq!(execute(cmd, &mut Config::default(), &Status::default(), &mut out), Action::None);
            assert!(out.contains(&format!("picoclock {}", crate::VERSION)) && out.contains("https://github.com/dc0sk/picoclock") && out.contains("AGPL-3.0-or-later"), "{out}");
        }
        let mut out = String::new();
        execute("help", &mut Config::default(), &Status::default(), &mut out);
        assert!(out.contains("version "), "{out}");
    }

    // VERIFIES: REQ-CLI-01
    #[test]
    fn status_shows_heap_use_only_where_there_is_a_heap() {
        let mut out = String::new();
        execute("status", &mut Config::default(), &Status::default(), &mut out);
        assert!(!out.contains("heap"), "{out}");
        let st = Status { heap_used: 6_000, heap_peak: 73_728, heap_size: 147_456, ..Default::default() };
        let mut out = String::new();
        execute("status", &mut Config::default(), &st, &mut out);
        assert!(out.contains("heap    : 6000 of 147456 bytes in use, peak 73728 (50%)"), "{out}");
        let mut out = String::new();
        execute("status", &mut Config::default(), &Status { rng_hardware: true, ..Default::default() }, &mut out);
        assert!(out.contains("random  : hardware TRNG\n") || out.contains("random  : hardware TRNG"), "{out}");
    }

    // VERIFIES: REQ-NET-04 REQ-NET-05 REQ-NET-07
    #[test]
    fn status_shows_scan_dhcp_ntp_and_mdns() {
        let run = |st: &Status| {
            let mut out = String::new();
            execute("status", &mut Config::default(), st, &mut out);
            out
        };
        let off = run(&Status::default());
        assert!(!off.contains("dhcp ntp") && !off.contains("mdns") && !off.contains("by signal"), "nothing while offline: {off}");
        let up = Status { wifi_up: true, wifi_slot: Some(2), wifi_scanned: Some(24), ..Default::default() };
        let s = run(&up);
        assert!(s.contains("connected (slot 2), chosen by signal from 24 access points seen"), "{s}");
        assert!(s.contains("dhcp ntp: none offered by the router (option 42)"), "{s}");
        assert!(s.contains("mdns    : on, group not joined, 0 queries seen, 0 answered"), "{s}");
        let s = run(&Status { dhcp_ntp: [[192, 168, 1, 1], [10, 0, 0, 5], [0; 4], [0; 4]], dhcp_ntp_n: 2, mdns_group: 1, mdns_seen: 9, mdns_answered: 2, wifi_scanned: None, ..up });
        assert!(s.contains("dhcp ntp: 192.168.1.1, 10.0.0.5\n"), "{s}");
        assert!(s.contains("mdns    : on, group joined, 9 queries seen, 2 answered") && !s.contains("by signal"), "{s}");
        assert!(run(&Status { mdns_group: 2, ..up }).contains("group join failed"));
        let mut c = Config::default();
        c.set("mdns", "off").unwrap();
        let mut out = String::new();
        execute("status", &mut c, &up, &mut out);
        assert!(out.contains("mdns    : off,"), "{out}");
    }

    // VERIFIES: REQ-NTS-09
    #[test]
    fn root_commands_and_pasting() {
        let run = |line: &str| {
            let mut out = String::new();
            let a = execute(line, &mut Config::default(), &Status::default(), &mut out);
            (a, out)
        };
        assert_eq!(run("root").0, Action::RootList);
        assert_eq!(run("root list").0, Action::RootList);
        assert_eq!(run("root add").0, Action::RootAdd);
        assert_eq!(run("root remove 2").0, Action::RootRemove(2));
        assert_eq!(run("root remove  3 ").0, Action::RootRemove(3));
        for bad in ["root remove", "root remove 0", "root remove x", "root remove -1", "root remove 1 2", "root bogus", "root add now", "root list all"] {
            let (a, out) = run(bad);
            assert_eq!(a, Action::None, "{bad}");
            assert!(out.contains("usage"), "{bad}: {out}");
        }
        assert!(run("help").1.contains("root add"));
        let st = Status { roots_user: 2, ..Default::default() };
        let mut out = String::new();
        execute("status", &mut Config::default(), &st, &mut out);
        assert!(out.contains(&format!("nts roots: {} built in, 2 of your own", crate::nts_ke::BUILT_IN_ROOTS.len())), "{out}");

        // pasting: ends at the END line, keeps the text, can be cancelled, and cannot grow without limit
        let mut p = Paste::new();
        assert_eq!(p.feed("-----BEGIN CERTIFICATE-----"), PasteStep::More);
        assert_eq!(p.feed("  MIIBabc=  "), PasteStep::More);
        let PasteStep::Done(text) = p.feed("-----END CERTIFICATE-----") else { panic!("not done") };
        assert_eq!(text.as_str(), "-----BEGIN CERTIFICATE-----\nMIIBabc=\n-----END CERTIFICATE-----\n");
        let mut p = Paste::new();
        p.feed("-----BEGIN CERTIFICATE-----");
        assert_eq!(p.feed("CANCEL"), PasteStep::Cancelled);
        let mut p = Paste::new();
        let line = "A".repeat(64);
        let mut steps = 0;
        while p.feed(&line) == PasteStep::More {
            steps += 1;
            assert!(steps < 100, "must end");
        }
        assert!(steps * 65 <= PASTE_MAX && steps > 30);
        // the listing
        let mut out = String::new();
        root_listing(&crate::rootstore::Roots::new(), None, &mut out);
        assert!(out.contains("built in:") && out.contains("your own: none"), "{out}");
    }

    // VERIFIES: REQ-NTS-10
    #[test]
    fn status_shows_the_last_nts_key_exchange() {
        let run = |st: &Status| {
            let mut out = String::new();
            execute("status", &mut Config::default(), st, &mut out);
            out
        };
        assert!(!run(&Status::default()).contains("nts ke"), "nothing before the first key exchange");
        let mut host = [0u8; 32];
        host[..13].copy_from_slice(b"nts.example.n");
        for (code, text) in [(1, "ok (450 ms)"), (2, "refused: TLS or the certificate"), (3, "refused: the server does not offer NTPv4"), (4, "refused: bad or incomplete reply"), (5, "no connection, or timed out")] {
            let s = run(&Status { nts_ke: code, nts_ke_ms: 450, nts_ke_host: host, ..Default::default() });
            assert!(s.contains("nts ke  : nts.example.n: ") && s.contains(text), "{code}: {s}");
        }
        let long = [b'a'; 32];
        assert!(run(&Status { nts_ke: 1, nts_ke_host: long, ..Default::default() }).contains(&format!("nts ke  : {}: ok", "a".repeat(32))));
        assert!(run(&Status { roots_user: 2, ..Default::default() }).contains("of your own"));
    }

    // VERIFIES: REQ-UPD-06
    #[test]
    fn update_upload_takes_a_size_and_refuses_nonsense() {
        let mut c = Config::default();
        let run = |c: &mut Config, line: &str| {
            let mut out = heapless::String::<512>::new();
            let a = execute(line, c, &Status::default(), &mut out);
            (a, out)
        };
        assert_eq!(run(&mut c, "update upload 1352136").0, Action::ImageUpload(1_352_136));
        for bad in ["update upload", "update upload 0", "update upload -5", "update upload abc", "update upload 99999999999", "update upload 5000000"] {
            let (a, out) = run(&mut c, bad);
            assert_eq!(a, Action::None, "{bad}");
            assert!(out.contains("usage"), "{bad}: {out}");
        }
    }

    // VERIFIES: REQ-UPD-06
    #[test]
    fn update_apply_needs_a_staged_image_and_the_install_word() {
        let mut c = Config::default();
        let mut out = heapless::String::<512>::new();
        assert_eq!(execute("update apply", &mut c, &Status::default(), &mut out), Action::None);
        assert!(out.contains("nothing is staged"), "{out}");
        let st = Status { install: crate::image::Install::Staged(crate::update::Version { major: 0, minor: 3, patch: 0 }, 0), ..Default::default() };
        let mut out = heapless::String::<512>::new();
        assert_eq!(execute("update apply", &mut c, &st, &mut out), Action::ImageApply);
        assert!(out.contains("0.3.0"), "{out}");
        assert!(install_confirmed("INSTALL") && install_confirmed("  INSTALL ") && !install_confirmed("install") && !install_confirmed("yes") && !install_confirmed(""));
        // the staged image shows in `update` and `status`
        let mut out = heapless::String::<1500>::new();
        execute("update", &mut c, &st, &mut out);
        assert!(out.contains("0.3.0 is staged") && out.contains("update apply"), "{out}");
        let mut out = heapless::String::<4000>::new();
        execute("status", &mut c, &st, &mut out);
        assert!(out.contains("image   : 0.3.0 is staged"), "{out}");
    }

    // VERIFIES: REQ-UPD-07
    #[test]
    fn a_custom_build_says_so_and_an_official_build_stays_quiet() {
        let mut c = Config::default();
        for (custom, shown) in [(true, true), (false, false)] {
            let st = Status { custom_images: custom, ..Default::default() };
            for line in ["version", "update"] {
                let mut out = heapless::String::<1500>::new();
                execute(line, &mut c, &st, &mut out);
                assert_eq!(out.contains("custom build"), shown, "{line} with custom={custom}: {out}");
            }
            let mut out = heapless::String::<4000>::new();
            execute("status", &mut c, &st, &mut out);
            assert_eq!(out.contains("images  : custom build"), shown, "status with custom={custom}");
        }
        assert!(CUSTOM_NOTICE.contains("NOT checked") && CUSTOM_NOTICE.contains("only informs"));
    }

    // VERIFIES: REQ-UPD-09
    #[test]
    fn update_download_asks_for_the_download_only_where_it_can_happen() {
        use crate::image::Install;
        use crate::update::{Check, Version};
        let v = Version::parse("0.3.0");
        let mut c = Config::default();
        let ok = Status { can_download: true, update: Check::Newer, update_latest: v, ..Default::default() };
        let run = |c: &mut Config, st: &Status| {
            let mut out = std::string::String::new();
            (execute("update download", c, st, &mut out), out)
        };
        let (a, out) = run(&mut c, &ok);
        assert_eq!(a, Action::ImageDownload);
        assert!(out.contains("0.3.0") && out.contains("nothing is installed until 'update apply'"), "{out}");
        for (st, why) in [
            (Status { custom_images: true, ..ok }, "custom build"),
            (Status { can_download: false, ..ok }, "Pico W"),
            (Status { update: Check::UpToDate, ..ok }, "up to date"),
            (Status { update_latest: None, ..ok }, "no version"),
            (Status { install: Install::Receiving, ..ok }, "busy"),
            (Status { install: Install::Downloading(v.unwrap()), ..ok }, "downloading"),
        ] {
            let (a, out) = run(&mut c, &st);
            assert_eq!(a, Action::None, "{why}: {out}");
            assert!(!out.is_empty(), "{why} must say why");
        }
        let (_, out) = run(&mut c, &Status { can_download: false, ..ok });
        assert!(out.contains("upload"), "the Pico W is pointed at the upload: {out}");
        assert!(help_text_mentions_download());
    }

    fn help_text_mentions_download() -> bool {
        let mut out = std::string::String::new();
        execute("help", &mut Config::default(), &Status::default(), &mut out);
        out.contains("update download")
    }
}
