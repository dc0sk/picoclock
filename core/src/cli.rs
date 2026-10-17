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
    /// Last octet of the WiFi MAC address (names the device on the network).
    pub mac_last: Option<u8>,
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
  geo                   look up the timezone from the public IP now (needs timezone=auto)
  version               show the software version, source repository and license
  cert                  show the HTTPS certificate (fingerprint, validity)
  cert new              replace the HTTPS certificate with a new self-signed one
  reboot | bootsel      restart / enter USB flashing mode
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
            if let Some(m) = st.mac_last {
                let _ = write!(out, "\nname    : {}", crate::config::hostname([0, 0, 0, 0, 0, m]));
            }
            let _ = out.write_str("\nip      : ");
            ip(out, st.ip);
            let _ = out.write_str("\ngateway : ");
            ip(out, st.gateway);
            let _ = out.write_str("\nsynced  : ");
            match (st.last_sync_unix, st.utc_unix) {
                (Some(s), Some(now)) => {
                    let _ = write!(out, "{} s ago ({})", (now - s).max(0), if st.sync_secure { "NTS, authenticated" } else { "plain NTP" });
                }
                _ => {
                    let _ = out.write_str("never");
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
            let _ = write!(out, "\nrandom  : {}", if st.rng_hardware { "hardware TRNG" } else { "ring-oscillator jitter (no hardware TRNG on this chip)" });
            if st.heap_size > 0 {
                let _ = write!(out, "\nheap    : {} of {} bytes in use, peak {} ({}%)", st.heap_used, st.heap_size, st.heap_peak, st.heap_peak as u64 * 100 / st.heap_size as u64);
            }
            let _ = write!(out, "\nhttps   : ");
            match (st.https_state, cfg.https) {
                (0, _) => { let _ = out.write_str("not in this build (Pico 2 W only)"); }
                (_, false) => { let _ = out.write_str("off"); }
                (1, true) => { let _ = out.write_str("starting (needs a valid time)"); }
                _ => {
                    let _ = write!(out, "on, certificate {} ({}), valid until {}", crate::x509::fingerprint_hex(&st.cert_fp), if st.cert_uploaded { "uploaded" } else { "self-signed" }, date(st.cert_not_after));
                }
            }
            let _ = write!(out, "\nscan    : worst late {} us, {} waits > 100 us in {} s\n", st.scan_late_max_us, st.scan_late_count, st.uptime_s);
            if st.unsaved {
                let _ = out.write_str("unsaved changes: type 'save'\n");
            }
            Action::None
        }
        "version" | "about" => {
            let _ = writeln!(out, "picoclock {}\nsource : {}\nlicense: {}", crate::VERSION, crate::REPOSITORY, crate::LICENSE);
            Action::None
        }
        "sync" => Action::SyncNow,
        "cert" => match (rest, st.https_state) {
            (_, 0) => {
                let _ = out.write_str("no HTTPS in this build (Pico 2 W only)\n");
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

    // VERIFIES: REQ-CLI-01 REQ-NTP-03
    #[test]
    fn actions() {
        let mut c = Config::default();
        assert_eq!(run(&mut c, "save").0, Action::Save);
        assert_eq!(run(&mut c, "sync").0, Action::SyncNow);
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

    // VERIFIES: REQ-TZ-02 REQ-NET-02 REQ-CLI-01
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
        let st = Status { wifi_up: true, wifi_slot: Some(2), mac_last: Some(0xab), ..Default::default() };
        let mut out = String::new();
        execute("status", &mut Config::default(), &st, &mut out);
        assert!(out.starts_with("wifi    : connected (slot 2)\nname    : picoclock-ab\n"), "{out}");
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
        assert_eq!(line(&Status::default()), "synced  : never");
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

    // VERIFIES: REQ-WEB-06
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
}
