//! Runtime configuration. One `key=value` text model serves the USB terminal, the web form and flash storage,
//! so there is a single place that parses and validates every setting.

use core::fmt::{self, Write};
use heapless::String;

use crate::tz::Tz;

pub type Text = String<64>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    UnknownKey,
    InvalidValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brightness {
    Auto,
    Fixed(u8),
}

/// Number of stored WiFi networks; they are tried in slot order.
pub const WIFI_SLOTS: usize = 8;

/// One stored network. An empty SSID means the slot is unused; an empty password means an open network.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiNet {
    pub ssid: Text,
    pub password: Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub wifi: [WifiNet; WIFI_SLOTS],
    /// Comma-separated NTP servers tried in order; `gateway` means the DHCP router.
    pub ntp_servers: String<96>,
    /// `utc` (show UTC), `auto` (look up by IP address), an IANA zone name, or a POSIX TZ rule.
    pub timezone: Text,
    /// POSIX rule resolved by the last successful `auto` lookup (empty until then).
    pub geo_rule: Text,
    pub hour12: bool,
    pub fahrenheit: bool,
    pub brightness: Brightness,
    pub key_beep: bool,
    pub hourly_chime: bool,
    pub alarm_enabled: bool,
    /// Local time of day in minutes after midnight.
    pub alarm_minutes: u16,
    /// Empty = no web password (the web interface is then open to the whole LAN).
    pub web_password: Text,
    pub web_enabled: bool,
    /// Serve NTP to the LAN (UDP 123). Off by default.
    pub ntp_serve: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wifi: Default::default(),
            ntp_servers: String::try_from("gateway,pool.ntp.org").unwrap(),
            timezone: String::try_from("utc").unwrap(),
            geo_rule: Text::new(),
            hour12: false,
            fahrenheit: false,
            brightness: Brightness::Auto,
            key_beep: true,
            hourly_chime: false,
            alarm_enabled: false,
            alarm_minutes: 7 * 60,
            web_password: Text::new(),
            web_enabled: true,
            ntp_serve: false,
        }
    }
}

pub struct KeyInfo {
    pub name: &'static str,
    pub help: &'static str,
    pub secret: bool,
}

pub const KEYS: &[KeyInfo] = &[
    KeyInfo { name: "wifi1_ssid", help: "slot 1 network name (empty = unused; tried first)", secret: false },
    KeyInfo { name: "wifi1_password", help: "slot 1 passphrase (empty = open network)", secret: true },
    KeyInfo { name: "wifi2_ssid", help: "slot 2 network name (empty = unused)", secret: false },
    KeyInfo { name: "wifi2_password", help: "slot 2 passphrase (empty = open network)", secret: true },
    KeyInfo { name: "wifi3_ssid", help: "slot 3 network name (empty = unused)", secret: false },
    KeyInfo { name: "wifi3_password", help: "slot 3 passphrase (empty = open network)", secret: true },
    KeyInfo { name: "wifi4_ssid", help: "slot 4 network name (empty = unused)", secret: false },
    KeyInfo { name: "wifi4_password", help: "slot 4 passphrase (empty = open network)", secret: true },
    KeyInfo { name: "wifi5_ssid", help: "slot 5 network name (empty = unused)", secret: false },
    KeyInfo { name: "wifi5_password", help: "slot 5 passphrase (empty = open network)", secret: true },
    KeyInfo { name: "wifi6_ssid", help: "slot 6 network name (empty = unused)", secret: false },
    KeyInfo { name: "wifi6_password", help: "slot 6 passphrase (empty = open network)", secret: true },
    KeyInfo { name: "wifi7_ssid", help: "slot 7 network name (empty = unused)", secret: false },
    KeyInfo { name: "wifi7_password", help: "slot 7 passphrase (empty = open network)", secret: true },
    KeyInfo { name: "wifi8_ssid", help: "slot 8 network name (empty = unused)", secret: false },
    KeyInfo { name: "wifi8_password", help: "slot 8 passphrase (empty = open network)", secret: true },
    KeyInfo { name: "ntp_servers", help: "comma-separated; 'gateway' = DHCP router (default gateway,pool.ntp.org)", secret: false },
    KeyInfo { name: "timezone", help: "utc | auto | IANA name (Europe/Berlin) | POSIX rule (CET-1CEST,M3.5.0,M10.5.0/3)", secret: false },
    KeyInfo { name: "geo_rule", help: "POSIX rule from the last 'auto' lookup (read-only cache)", secret: false },
    KeyInfo { name: "hour12", help: "on|off: 12-hour display", secret: false },
    KeyInfo { name: "fahrenheit", help: "on|off: temperature in Fahrenheit", secret: false },
    KeyInfo { name: "brightness", help: "auto | 1..100", secret: false },
    KeyInfo { name: "key_beep", help: "on|off: beep on button press", secret: false },
    KeyInfo { name: "hourly_chime", help: "on|off: beep on the hour", secret: false },
    KeyInfo { name: "alarm_enabled", help: "on|off", secret: false },
    KeyInfo { name: "alarm_time", help: "HH:MM local time", secret: false },
    KeyInfo { name: "web_password", help: "password for the web interface (user 'admin'); empty = none", secret: true },
    KeyInfo { name: "web_enabled", help: "on|off: web interface", secret: false },
    KeyInfo { name: "ntp_serve", help: "on|off: answer NTP requests from the LAN (accuracy about +-1 s)", secret: false },
];

/// Network name of this device: `picoclock-` plus the last block (octet) of its MAC address, so several clocks
/// on one LAN can be told apart.
pub fn hostname(mac: [u8; 6]) -> String<16> {
    let mut s = String::new();
    let _ = write!(s, "picoclock-{:02x}", mac[5]);
    s
}

/// `wifi3_ssid` -> (slot index 2, is_password). `wifi_ssid` / `wifi_password` are aliases for slot 1.
fn wifi_key(key: &str) -> Option<(usize, bool)> {
    let rest = key.strip_prefix("wifi")?;
    let (slot, field) = match rest {
        "_ssid" => (0, "ssid"),
        "_password" => (0, "password"),
        _ => {
            let (n, field) = rest.split_once('_')?;
            let n: usize = n.parse().ok().filter(|n| (1..=WIFI_SLOTS).contains(n))?;
            (n - 1, field)
        }
    };
    match field {
        "ssid" => Some((slot, false)),
        "password" => Some((slot, true)),
        _ => None,
    }
}

fn parse_bool(v: &str) -> Result<bool, ConfigError> {
    match v {
        "on" | "true" | "1" | "yes" => Ok(true),
        "off" | "false" | "0" | "no" => Ok(false),
        _ => Err(ConfigError::InvalidValue),
    }
}

fn text<const N: usize>(v: &str) -> Result<String<N>, ConfigError> {
    // Newlines would corrupt the stored text and control characters have no business in any setting.
    if v.chars().any(char::is_control) {
        return Err(ConfigError::InvalidValue);
    }
    String::try_from(v).map_err(|_| ConfigError::InvalidValue)
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

impl Config {
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), ConfigError> {
        let value = value.trim();
        match key {
            k if wifi_key(k).is_some() => {
                let (slot, is_password) = wifi_key(k).ok_or(ConfigError::UnknownKey)?;
                if is_password {
                    if !value.is_empty() && !(8..=63).contains(&value.len()) {
                        return Err(ConfigError::InvalidValue);
                    }
                    self.wifi[slot].password = text(value)?
                } else {
                    if value.len() > 32 {
                        return Err(ConfigError::InvalidValue);
                    }
                    self.wifi[slot].ssid = text(value)?
                }
            }
            "ntp_servers" => {
                let s: String<96> = text(value)?;
                if s.split(',').any(|h| h.trim().is_empty()) {
                    return Err(ConfigError::InvalidValue);
                }
                self.ntp_servers = s
            }
            "timezone" => {
                let ok = value.eq_ignore_ascii_case("utc")
                    || value.eq_ignore_ascii_case("auto")
                    || Tz::from_name_or_rule(value).is_ok();
                if !ok {
                    return Err(ConfigError::InvalidValue);
                }
                self.timezone = text(value)?
            }
            "geo_rule" => {
                if !value.is_empty() && Tz::parse(value).is_err() {
                    return Err(ConfigError::InvalidValue);
                }
                self.geo_rule = text(value)?
            }
            "hour12" => self.hour12 = parse_bool(value)?,
            "fahrenheit" => self.fahrenheit = parse_bool(value)?,
            "brightness" => {
                self.brightness = if value.eq_ignore_ascii_case("auto") {
                    Brightness::Auto
                } else {
                    match value.parse::<u8>() {
                        Ok(n @ 1..=100) => Brightness::Fixed(n),
                        _ => return Err(ConfigError::InvalidValue),
                    }
                }
            }
            "key_beep" => self.key_beep = parse_bool(value)?,
            "hourly_chime" => self.hourly_chime = parse_bool(value)?,
            "alarm_enabled" => self.alarm_enabled = parse_bool(value)?,
            "alarm_time" => {
                let (h, m) = value.split_once(':').ok_or(ConfigError::InvalidValue)?;
                let (h, m): (u16, u16) = (
                    h.parse().map_err(|_| ConfigError::InvalidValue)?,
                    m.parse().map_err(|_| ConfigError::InvalidValue)?,
                );
                if h > 23 || m > 59 {
                    return Err(ConfigError::InvalidValue);
                }
                self.alarm_minutes = h * 60 + m;
            }
            "web_password" => self.web_password = text(value)?,
            "web_enabled" => self.web_enabled = parse_bool(value)?,
            "ntp_serve" => self.ntp_serve = parse_bool(value)?,
            _ => return Err(ConfigError::UnknownKey),
        }
        Ok(())
    }

    /// Writes the value of `key`. Secrets are written as-is; use [`Config::write_display`] to redact.
    pub fn write_value(&self, key: &str, out: &mut impl Write) -> Result<(), ConfigError> {
        let r = match key {
            k if wifi_key(k).is_some() => {
                let (slot, is_password) = wifi_key(k).ok_or(ConfigError::UnknownKey)?;
                out.write_str(if is_password { &self.wifi[slot].password } else { &self.wifi[slot].ssid })
            }
            "ntp_servers" => out.write_str(&self.ntp_servers),
            "timezone" => out.write_str(&self.timezone),
            "geo_rule" => out.write_str(&self.geo_rule),
            "hour12" => out.write_str(on_off(self.hour12)),
            "fahrenheit" => out.write_str(on_off(self.fahrenheit)),
            "brightness" => match self.brightness {
                Brightness::Auto => out.write_str("auto"),
                Brightness::Fixed(n) => write!(out, "{n}"),
            },
            "key_beep" => out.write_str(on_off(self.key_beep)),
            "hourly_chime" => out.write_str(on_off(self.hourly_chime)),
            "alarm_enabled" => out.write_str(on_off(self.alarm_enabled)),
            "alarm_time" => write!(out, "{:02}:{:02}", self.alarm_minutes / 60, self.alarm_minutes % 60),
            "web_password" => out.write_str(&self.web_password),
            "web_enabled" => out.write_str(on_off(self.web_enabled)),
            "ntp_serve" => out.write_str(on_off(self.ntp_serve)),
            _ => return Err(ConfigError::UnknownKey),
        };
        r.map_err(|_: fmt::Error| ConfigError::InvalidValue)
    }

    /// Like [`Config::write_value`] but secrets show as `(set)` / `(empty)`.
    pub fn write_display(&self, key: &str, out: &mut impl Write) -> Result<(), ConfigError> {
        if wifi_key(key).is_some_and(|(_, pw)| pw) || KEYS.iter().any(|k| k.name == key && k.secret) {
            let empty = match wifi_key(key) {
                Some((slot, _)) => self.wifi[slot].password.is_empty(),
                None => self.web_password.is_empty(),
            };
            return out.write_str(if empty { "(empty)" } else { "(set)" }).map_err(|_| ConfigError::InvalidValue);
        }
        self.write_value(key, out)
    }

    /// The effective timezone: `utc`, the cached `auto` result, or the configured zone. Falls back to UTC.
    pub fn effective_tz(&self) -> Tz {
        let t = self.timezone.as_str();
        if t.eq_ignore_ascii_case("utc") {
            Tz::UTC
        } else if t.eq_ignore_ascii_case("auto") {
            Tz::parse(&self.geo_rule).unwrap_or(Tz::UTC)
        } else {
            Tz::from_name_or_rule(t).unwrap_or(Tz::UTC)
        }
    }

    pub fn wants_geolocation(&self) -> bool {
        self.timezone.eq_ignore_ascii_case("auto")
    }

    pub fn shows_utc(&self) -> bool {
        self.timezone.eq_ignore_ascii_case("utc")
    }
}

// ---- persistence -------------------------------------------------------------------------------------------

const MAGIC: [u8; 4] = *b"PCK1";
const HEADER: usize = 4 + 2 + 4;

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

struct SliceWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl Write for SliceWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len.checked_add(s.len()).ok_or(fmt::Error)?;
        self.buf.get_mut(self.len..end).ok_or(fmt::Error)?.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

impl Config {
    /// Serialises to `magic | len | crc32 | key=value lines`. Returns the number of bytes written.
    pub fn to_bytes(&self, buf: &mut [u8]) -> Result<usize, ConfigError> {
        if buf.len() < HEADER {
            return Err(ConfigError::InvalidValue);
        }
        let (head, body) = buf.split_at_mut(HEADER);
        let mut w = SliceWriter { buf: body, len: 0 };
        for k in KEYS {
            write!(w, "{}=", k.name).map_err(|_| ConfigError::InvalidValue)?;
            self.write_value(k.name, &mut w)?;
            w.write_str("\n").map_err(|_| ConfigError::InvalidValue)?;
        }
        let len = w.len;
        head[..4].copy_from_slice(&MAGIC);
        head[4..6].copy_from_slice(&(len as u16).to_le_bytes());
        head[6..10].copy_from_slice(&crc32(&body[..len]).to_le_bytes());
        Ok(HEADER + len)
    }

    /// Parses stored bytes. `None` if the magic, length or checksum is wrong (e.g. erased flash).
    /// Unknown keys and invalid values are skipped, so older and newer firmware can share a record.
    pub fn from_bytes(data: &[u8]) -> Option<Config> {
        if data.len() < HEADER || data[..4] != MAGIC {
            return None;
        }
        let len = u16::from_le_bytes([data[4], data[5]]) as usize;
        let crc = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);
        let body = data.get(HEADER..HEADER + len)?;
        if crc32(body) != crc {
            return None;
        }
        let mut cfg = Config::default();
        for line in core::str::from_utf8(body).ok()?.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let _ = cfg.set(k, v);
            }
        }
        Some(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(c: &Config, k: &str) -> std::string::String {
        let mut s = std::string::String::new();
        c.write_value(k, &mut s).unwrap();
        s
    }

    // VERIFIES: REQ-CFG-04 REQ-NTP-01 REQ-TIME-02
    #[test]
    fn defaults_match_the_spec() {
        let c = Config::default();
        assert_eq!(val(&c, "ntp_servers"), "gateway,pool.ntp.org");
        assert_eq!(val(&c, "timezone"), "utc");
        assert!(c.shows_utc());
        assert_eq!(c.effective_tz(), Tz::UTC);
    }

    // VERIFIES: REQ-CFG-01 REQ-TZ-03
    #[test]
    fn set_validates() {
        let mut c = Config::default();
        assert_eq!(c.set("nope", "1"), Err(ConfigError::UnknownKey));
        assert_eq!(c.set("hour12", "maybe"), Err(ConfigError::InvalidValue));
        assert_eq!(c.set("brightness", "0"), Err(ConfigError::InvalidValue));
        assert_eq!(c.set("brightness", "101"), Err(ConfigError::InvalidValue));
        assert_eq!(c.set("alarm_time", "24:00"), Err(ConfigError::InvalidValue));
        assert_eq!(c.set("alarm_time", "7:5"), Ok(()));
        assert_eq!(val(&c, "alarm_time"), "07:05");
        assert_eq!(c.set("timezone", "Mars/Base"), Err(ConfigError::InvalidValue));
        assert_eq!(c.set("wifi_password", "short"), Err(ConfigError::InvalidValue));
        assert_eq!(c.set("wifi_ssid", "a\nb"), Err(ConfigError::InvalidValue));
        assert_eq!(c.set("ntp_servers", "a,,b"), Err(ConfigError::InvalidValue));
        assert_eq!(c.set("ntp_servers", &"x".repeat(97)), Err(ConfigError::InvalidValue));
        // A failed set leaves the old value alone.
        assert_eq!(val(&c, "ntp_servers"), "gateway,pool.ntp.org");
    }

    // VERIFIES: REQ-TZ-03 REQ-TZ-04 REQ-TIME-02
    #[test]
    fn timezone_modes() {
        let mut c = Config::default();
        c.set("timezone", "Europe/Berlin").unwrap();
        assert_eq!(c.effective_tz().offset_at(1_720_000_000), 7200);
        c.set("timezone", "EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(c.effective_tz().offset_at(1_720_000_000), -4 * 3600);
        c.set("timezone", "auto").unwrap();
        assert!(c.wants_geolocation());
        assert_eq!(c.effective_tz(), Tz::UTC, "auto without a lookup result falls back to UTC");
        c.set("geo_rule", "JST-9").unwrap();
        assert_eq!(c.effective_tz().offset_at(0), 9 * 3600);
        assert_eq!(c.set("geo_rule", "garbage"), Err(ConfigError::InvalidValue));
    }

    // VERIFIES: REQ-CFG-03
    #[test]
    fn secrets_are_redacted_in_display() {
        let mut c = Config::default();
        let mut s = std::string::String::new();
        c.write_display("wifi_password", &mut s).unwrap();
        assert_eq!(s, "(empty)");
        c.set("wifi_password", "hunter2hunter2").unwrap();
        s.clear();
        c.write_display("wifi_password", &mut s).unwrap();
        assert_eq!(s, "(set)");
        assert_eq!(val(&c, "wifi_password"), "hunter2hunter2");
    }

    // VERIFIES: REQ-CFG-02
    #[test]
    fn roundtrip_every_key() {
        let mut c = Config::default();
        for (k, v) in [
            ("wifi_ssid", "my net"), ("wifi_password", "pass=word=12"), ("ntp_servers", "10.0.0.1,time.example.org"),
            ("timezone", "Asia/Tokyo"), ("geo_rule", "JST-9"), ("hour12", "on"), ("fahrenheit", "on"),
            ("brightness", "42"), ("key_beep", "off"), ("hourly_chime", "on"), ("alarm_enabled", "on"),
            ("alarm_time", "23:59"), ("web_password", "s3cret"), ("web_enabled", "off"),
        ] {
            c.set(k, v).unwrap();
        }
        let mut buf = [0u8; 1024];
        let n = c.to_bytes(&mut buf).unwrap();
        assert_eq!(Config::from_bytes(&buf[..n]), Some(c));
    }

    // VERIFIES: REQ-CFG-02
    #[test]
    fn rejects_corrupt_or_blank_storage() {
        assert_eq!(Config::from_bytes(&[0xFF; 4096]), None);
        assert_eq!(Config::from_bytes(&[]), None);
        let mut buf = [0u8; 1024];
        let n = Config::default().to_bytes(&mut buf).unwrap();
        for i in [0, 5, 7, 12, n - 1] {
            let mut bad = buf;
            bad[i] ^= 0x40;
            assert_eq!(Config::from_bytes(&bad[..n]), None, "flip at {i}");
        }
        assert_eq!(Config::from_bytes(&buf[..n - 1]), None, "truncated");
        // A length field larger than the data must not panic.
        let mut bad = buf;
        bad[4..6].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(Config::from_bytes(&bad[..n]), None);
    }

    // VERIFIES: REQ-CFG-02
    #[test]
    fn tolerates_unknown_keys_and_bad_values_in_storage() {
        let body = b"future_key=1\nhour12=perhaps\nbrightness=77\nnoequals\n";
        let mut rec = std::vec::Vec::new();
        rec.extend_from_slice(b"PCK1");
        rec.extend_from_slice(&(body.len() as u16).to_le_bytes());
        rec.extend_from_slice(&crc32(body).to_le_bytes());
        rec.extend_from_slice(body);
        let c = Config::from_bytes(&rec).unwrap();
        assert_eq!(c.brightness, Brightness::Fixed(77));
        assert!(!c.hour12);
    }

    // VERIFIES: REQ-CFG-02 REQ-NFR-01
    #[test]
    fn serialisation_fits_one_sector_with_worst_case_values() {
        let mut c = Config::default();
        c.set("wifi_ssid", &"s".repeat(32)).unwrap();
        c.set("wifi_password", &"p".repeat(63)).unwrap();
        c.set("web_password", &"w".repeat(64)).unwrap();
        c.set("ntp_servers", &"n".repeat(96)).unwrap();
        c.set("timezone", &"CET-1CEST,M3.5.0,M10.5.0/3".repeat(2)).unwrap_or(());
        let mut buf = [0u8; 1024];
        assert!(c.to_bytes(&mut buf).unwrap() < 1024);
    }

    // VERIFIES: REQ-NET-02
    #[test]
    fn hostname_ends_in_last_mac_block() {
        assert_eq!(hostname([0x28, 0xcd, 0xc1, 0x12, 0x34, 0xab]).as_str(), "picoclock-ab");
        assert_eq!(hostname([0, 0, 0, 0, 0, 0x05]).as_str(), "picoclock-05");
    }

    // VERIFIES: REQ-SRV-01 REQ-CFG-04
    #[test]
    fn ntp_server_defaults_off() {
        let mut c = Config::default();
        assert_eq!(val(&c, "ntp_serve"), "off");
        c.set("ntp_serve", "on").unwrap();
        assert!(c.ntp_serve);
        let mut buf = [0u8; 2048];
        let n = c.to_bytes(&mut buf).unwrap();
        assert!(Config::from_bytes(&buf[..n]).unwrap().ntp_serve);
    }

    // VERIFIES: REQ-NET-01 REQ-CFG-03
    #[test]
    fn eight_wifi_slots_are_independent_and_bounded() {
        let mut c = Config::default();
        for n in 1..=WIFI_SLOTS {
            c.set(&format!("wifi{n}_ssid"), &format!("net{n}")).unwrap();
            c.set(&format!("wifi{n}_password"), &format!("password{n}")).unwrap();
        }
        for n in 1..=WIFI_SLOTS {
            assert_eq!(val(&c, &format!("wifi{n}_ssid")), format!("net{n}"));
            assert_eq!(val(&c, &format!("wifi{n}_password")), format!("password{n}"));
            let mut s = std::string::String::new();
            c.write_display(&format!("wifi{n}_password"), &mut s).unwrap();
            assert_eq!(s, "(set)", "slot {n} must be redacted");
        }
        assert_eq!(val(&c, "wifi_ssid"), "net1", "legacy alias is slot 1");
        for bad in ["wifi0_ssid", "wifi9_ssid", "wifi10_ssid", "wifi1_x", "wifi_", "wifi1ssid"] {
            assert_eq!(c.set(bad, "x"), Err(ConfigError::UnknownKey), "{bad}");
        }
    }

    // VERIFIES: REQ-NET-01 REQ-CFG-02 REQ-NFR-01
    #[test]
    fn all_slots_full_survive_a_flash_round_trip() {
        let mut c = Config::default();
        for n in 1..=WIFI_SLOTS {
            c.set(&format!("wifi{n}_ssid"), &"s".repeat(32)).unwrap();
            c.set(&format!("wifi{n}_password"), &"p".repeat(63)).unwrap();
        }
        c.set("web_password", &"w".repeat(64)).unwrap();
        c.set("ntp_servers", &"n".repeat(96)).unwrap();
        c.set("timezone", "Europe/Berlin").unwrap();
        let mut buf = [0xFFu8; 2048]; // storage::RECORD
        let n = c.to_bytes(&mut buf).expect("worst case must fit the flash record");
        assert!(n <= 2048);
        assert_eq!(Config::from_bytes(&buf), Some(c));
    }
}
