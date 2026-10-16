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
    /// Config differs from what is saved in flash.
    pub unsaved: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
  reboot | bootsel      restart / enter USB flashing mode
  help                  this text
";

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
                    let _ = write!(out, "{} s ago", (now - s).max(0));
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
            let _ = write!(out, "\nlight   : {} (raw ADC)\n", st.light_raw);
            if st.unsaved {
                let _ = out.write_str("unsaved changes: type 'save'\n");
            }
            Action::None
        }
        "sync" => Action::SyncNow,
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

    #[test]
    fn set_without_value_clears_text_settings() {
        let mut c = Config::default();
        run(&mut c, "set web_password abc");
        assert_eq!(run(&mut c, "set web_password").0, Action::Changed);
        assert!(c.web_password.is_empty());
    }

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

    #[test]
    fn tz_listing_is_capped() {
        let mut c = Config::default();
        let (_, out) = run(&mut c, "tz");
        assert_eq!(out.lines().count(), 41, "40 names plus the 'more' line");
        assert!(out.contains("more; narrow it down"));
        assert!(out.len() < 1500, "must fit the firmware's output buffer, got {}", out.len());
    }

    #[test]
    fn help_lists_every_key() {
        let mut c = Config::default();
        let (_, out) = run(&mut c, "help");
        for k in KEYS {
            assert!(out.contains(k.name), "{}", k.name);
        }
    }
}
