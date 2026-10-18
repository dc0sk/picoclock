//! Runtime configuration. One `key=value` text model serves the USB terminal, the web form and flash storage,
//! so there is a single place that parses and validates every setting.

use core::fmt::{self, Write};
use heapless::String;

use crate::sources::{Mode, Policy, Source, SOURCES};
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
    /// Apply daylight saving time. Off = the zone's standard time all year.
    pub dst: bool,
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
    /// Serve the web page over HTTPS (Pico 2 W builds only; other builds have no TLS).
    pub https: bool,
    /// While HTTPS is available, answer plain-HTTP page requests with a redirect to it.
    pub http_redirect: bool,
    /// Join the strongest stored network that is in range (true) rather than the first in slot order (false).
    pub wifi_strongest: bool,
    /// Answer mDNS queries for `<device name>.local`.
    pub mdns: bool,
    /// Name on the network (DHCP, mDNS, HTTPS certificate). Empty = `picoclock-<last MAC block>`.
    pub hostname: String<HOSTNAME_MAX>,
    /// How the time sources are combined (`order` or `vote`).
    pub time_mode: Mode,
    /// The time sources in use, best first.
    pub time_sources: heapless::Vec<Source, { SOURCES.len() }>,
    /// Vote power per source (`Source as usize`), 0 to 9.
    pub vote_power: [u8; SOURCES.len()],
    /// Seconds within which sources agree in mode `vote`.
    pub vote_tolerance: u16,
}

impl Config {
    /// The source selection as the selector wants it.
    pub fn policy(&self) -> Policy {
        Policy { mode: self.time_mode, order: self.time_sources.clone(), power: self.vote_power, tolerance_s: self.vote_tolerance }
    }

    /// Copies every setting that differs between `before` and `after` into `self`, leaving the rest alone. The web page and
    /// the buttons save only what they changed; edits made in the terminal and not yet saved must not ride along (REQ-CFG-05).
    /// The destructuring names every field, so a new setting that is not listed here fails to compile.
    pub fn carry_changes(&mut self, before: &Config, after: &Config) {
        let Config {
            wifi,
            ntp_servers,
            timezone,
            geo_rule,
            dst,
            hour12,
            fahrenheit,
            brightness,
            key_beep,
            hourly_chime,
            alarm_enabled,
            alarm_minutes,
            web_password,
            web_enabled,
            ntp_serve,
            https,
            http_redirect,
            wifi_strongest,
            mdns,
            hostname,
            time_mode,
            time_sources,
            vote_power,
            vote_tolerance,
        } = after;
        macro_rules! carry {
            ($($f:ident),*) => { $( if before.$f != *$f { self.$f = $f.clone(); } )* };
        }
        carry!(
            ntp_servers, timezone, geo_rule, dst, hour12, fahrenheit, brightness, key_beep, hourly_chime, alarm_enabled,
            alarm_minutes, web_password, web_enabled, ntp_serve, https, http_redirect, wifi_strongest, mdns, hostname, time_mode,
            time_sources, vote_power, vote_tolerance
        );
        for (i, net) in wifi.iter().enumerate() {
            if before.wifi[i] != *net {
                self.wifi[i] = net.clone();
            }
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wifi: Default::default(),
            ntp_servers: String::try_from("gateway,pool.ntp.org").unwrap(),
            timezone: String::try_from("utc").unwrap(),
            geo_rule: Text::new(),
            dst: true,
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
            https: true,
            http_redirect: true,
            wifi_strongest: false,
            mdns: true,
            hostname: String::new(),
            time_mode: Mode::Order,
            time_sources: heapless::Vec::from_slice(&[Source::Ntp]).unwrap(),
            vote_power: [1; SOURCES.len()],
            vote_tolerance: 2,
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
    KeyInfo { name: "ntp_servers", help: "comma-separated, tried in order; 'gateway' = DHCP router; 'dhcp' = the NTP servers the router hands out (DHCP option 42); 'nts:host' = authenticated NTS (default gateway,pool.ntp.org)", secret: false },
    KeyInfo { name: "timezone", help: "utc | auto | IANA name (Europe/Berlin) | POSIX rule (CET-1CEST,M3.5.0,M10.5.0/3)", secret: false },
    KeyInfo { name: "dst", help: "on|off: apply daylight saving time (default on); off = standard time all year", secret: false },
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
    KeyInfo { name: "https", help: "on|off: serve the web page over HTTPS with a self-signed or uploaded certificate (Pico 2 W only)", secret: false },
    KeyInfo { name: "http_redirect", help: "on|off: while HTTPS is available, redirect plain-HTTP page requests to it", secret: false },
    KeyInfo { name: "wifi_select", help: "order|strongest: join the first stored network in slot order, or the strongest one in range (default order)", secret: false },
    KeyInfo { name: "mdns", help: "on|off: answer for <device name>.local on the LAN (multicast DNS, default on)", secret: false },
    KeyInfo { name: "hostname", help: "name of this clock on the network (DHCP, mDNS, HTTPS certificate): 1-24 lowercase letters, digits and hyphens; empty = picoclock-<last MAC block>", secret: false },
    KeyInfo { name: "time_mode", help: "order|vote: take the time from the first source in time_sources that has one (order), or only when the sources agree by vote power (vote)", secret: false },
    KeyInfo { name: "time_sources", help: "comma-separated, best first: ntp, dcf77 (a DCF77 receiver on GP0/GP1; default ntp)", secret: false },
    KeyInfo { name: "vote_ntp", help: "0-9: vote power of NTP/NTS in mode vote (0 = compared and shown, cannot decide; default 1)", secret: false },
    KeyInfo { name: "vote_dcf77", help: "0-9: vote power of DCF77 in mode vote (default 1)", secret: false },
    KeyInfo { name: "vote_tolerance", help: "1-3600 seconds within which sources count as agreeing in mode vote (default 2)", secret: false },
    KeyInfo { name: "ntp_serve", help: "on|off: answer NTP requests from the LAN (accuracy about +-1 s)", secret: false },
];

/// Network name of this device: `picoclock-` plus the last block (octet) of its MAC address, so several clocks
/// on one LAN can be told apart.
pub fn hostname(mac: [u8; 6]) -> String<16> {
    let mut s = String::new();
    let _ = write!(s, "picoclock-{:02x}", mac[5]);
    s
}

/// Longest custom device name. The HTTPS certificate and mDNS names add `.local`, and buffers are sized for that.
pub const HOSTNAME_MAX: usize = 24;

/// A usable device name: 1 to [`HOSTNAME_MAX`] lowercase letters, digits and hyphens, not starting or ending with a hyphen
/// (one DNS label, RFC 1123).
pub fn valid_hostname(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= HOSTNAME_MAX
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Config {
    /// The name this clock uses on the network: the custom `hostname`, or `picoclock-<last MAC block>` when none is set.
    pub fn device_name(&self, mac_last: u8) -> String<32> {
        let mut s = String::new();
        if self.hostname.is_empty() {
            let _ = s.push_str(&hostname([0, 0, 0, 0, 0, mac_last]));
        } else {
            let _ = s.push_str(&self.hostname);
        }
        s
    }
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

/// The host of an `nts:<host>` entry of `ntp_servers` (prefix in any case, host trimmed); `None` for a plain entry.
pub fn nts_host(entry: &str) -> Option<&str> {
    let e = entry.trim();
    (e.len() >= 4 && e.is_char_boundary(4) && e[..4].eq_ignore_ascii_case("nts:")).then(|| e[4..].trim())
}

/// A DNS name usable as an NTS server: certificates are checked against the name, so an IP address will not do.
/// Letters, digits, dots and hyphens, labels not starting or ending with a hyphen, at least two labels, at most 64 bytes
/// (the size `nts::Session` keeps), and not made of digits and dots only.
pub fn valid_nts_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 64
        && h.contains('.')
        && !h.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && h.split('.').all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
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
                if s.split(',').map(str::trim).any(|h| h.is_empty() || nts_host(h).is_some_and(|host| !valid_nts_host(host))) {
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
            "dst" => self.dst = parse_bool(value)?,
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
            "https" => self.https = parse_bool(value)?,
            "wifi_select" => {
                self.wifi_strongest = match value {
                    "order" => false,
                    "strongest" => true,
                    _ => return Err(ConfigError::InvalidValue),
                }
            }
            "mdns" => self.mdns = parse_bool(value)?,
            "time_mode" => {
                self.time_mode = match value {
                    "order" => Mode::Order,
                    "vote" => Mode::Vote,
                    _ => return Err(ConfigError::InvalidValue),
                }
            }
            "time_sources" => {
                let mut list = heapless::Vec::new();
                for name in value.split(',') {
                    let src = Source::from_name(name).ok_or(ConfigError::InvalidValue)?;
                    if list.contains(&src) {
                        return Err(ConfigError::InvalidValue);
                    }
                    list.push(src).map_err(|_| ConfigError::InvalidValue)?;
                }
                self.time_sources = list;
            }
            "vote_ntp" | "vote_dcf77" => {
                let power: u8 = value.parse().map_err(|_| ConfigError::InvalidValue)?;
                if power > 9 {
                    return Err(ConfigError::InvalidValue);
                }
                self.vote_power[if key == "vote_ntp" { Source::Ntp } else { Source::Dcf77 } as usize] = power;
            }
            "vote_tolerance" => {
                let secs: u16 = value.parse().map_err(|_| ConfigError::InvalidValue)?;
                if !(1..=3600).contains(&secs) {
                    return Err(ConfigError::InvalidValue);
                }
                self.vote_tolerance = secs;
            }
            "hostname" => {
                // DNS names are case-insensitive; keep them lowercase so the certificate and mDNS name are unambiguous
                let mut name = String::<HOSTNAME_MAX>::new();
                for c in value.chars() {
                    name.push(c.to_ascii_lowercase()).map_err(|_| ConfigError::InvalidValue)?;
                }
                if !name.is_empty() && !valid_hostname(&name) {
                    return Err(ConfigError::InvalidValue);
                }
                self.hostname = name;
            }
            "http_redirect" => self.http_redirect = parse_bool(value)?,
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
            "dst" => out.write_str(on_off(self.dst)),
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
            "https" => out.write_str(on_off(self.https)),
            "wifi_select" => out.write_str(if self.wifi_strongest { "strongest" } else { "order" }),
            "mdns" => out.write_str(on_off(self.mdns)),
            "time_mode" => out.write_str(self.time_mode.name()),
            "time_sources" => self.time_sources.iter().enumerate().try_for_each(|(i, s)| write!(out, "{}{}", if i > 0 { "," } else { "" }, s.name())),
            "vote_ntp" => write!(out, "{}", self.vote_power[Source::Ntp as usize]),
            "vote_dcf77" => write!(out, "{}", self.vote_power[Source::Dcf77 as usize]),
            "vote_tolerance" => write!(out, "{}", self.vote_tolerance),
            "hostname" => out.write_str(&self.hostname),
            "http_redirect" => out.write_str(on_off(self.http_redirect)),
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
        let tz = if t.eq_ignore_ascii_case("utc") {
            Tz::UTC
        } else if t.eq_ignore_ascii_case("auto") {
            Tz::parse(&self.geo_rule).unwrap_or(Tz::UTC)
        } else {
            Tz::from_name_or_rule(t).unwrap_or(Tz::UTC)
        };
        if self.dst { tz } else { tz.without_dst() }
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

pub(crate) fn crc32(data: &[u8]) -> u32 {
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

    // VERIFIES: REQ-CFG-05
    #[test]
    fn carried_changes_leave_other_unsaved_edits_behind() {
        let saved = Config::default();
        // the terminal edited the timezone, not yet saved
        let mut live = saved.clone();
        live.set("timezone", "Europe/Berlin").unwrap();
        // then the web page changed the alarm and slot 2
        let before = live.clone();
        live.set("alarm_enabled", "on").unwrap();
        live.set("wifi2_ssid", "other").unwrap();
        let mut stored = saved.clone();
        stored.carry_changes(&before, &live);
        assert_eq!(val(&stored, "alarm_enabled"), "on");
        assert_eq!(val(&stored, "wifi2_ssid"), "other");
        assert_eq!(val(&stored, "timezone"), "utc", "the unsaved terminal edit is not written");
        assert_ne!(stored, live);
    }

    // VERIFIES: REQ-SRC-01 REQ-SRC-04
    #[test]
    fn time_source_settings_are_validated_and_survive_the_flash_record() {
        let mut c = Config::default();
        assert_eq!((val(&c, "time_mode"), val(&c, "time_sources"), val(&c, "vote_ntp"), val(&c, "vote_dcf77"), val(&c, "vote_tolerance")), ("order".into(), "ntp".into(), "1".into(), "1".into(), "2".into()), "defaults keep today's behaviour: NTP alone");
        c.set("time_sources", "dcf77, NTP").unwrap();
        c.set("time_mode", "vote").unwrap();
        c.set("vote_ntp", "3").unwrap();
        c.set("vote_dcf77", "0").unwrap();
        c.set("vote_tolerance", "30").unwrap();
        let p = c.policy();
        assert_eq!((p.mode, p.order.as_slice(), p.power, p.tolerance_s), (Mode::Vote, &[Source::Dcf77, Source::Ntp][..], [3, 0], 30));
        assert_eq!(val(&c, "time_sources"), "dcf77,ntp");
        for (k, bad) in [("time_mode", "both"), ("time_sources", ""), ("time_sources", "ntp,ntp"), ("time_sources", "ntp,gps"), ("time_sources", "ntp,dcf77,ntp"), ("vote_ntp", "10"), ("vote_dcf77", "-1"), ("vote_tolerance", "0"), ("vote_tolerance", "3601"), ("vote_tolerance", "x")] {
            let before = c.clone();
            assert!(c.set(k, bad).is_err(), "{k}={bad:?}");
            assert_eq!(c, before, "a refused value changes nothing");
        }
        let mut buf = [0xFFu8; 2048];
        let n = c.to_bytes(&mut buf).unwrap();
        assert_eq!(Config::from_bytes(&buf[..n]), Some(c));
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
        // the longest rule the setting takes: bracketed names and hh:mm:ss offsets stretch a POSIX rule (a doubled one is refused)
        let long = "<+1234>-12:30:15<+1235>-11:30:15,M10.5.0/-1:30:15,M3.5.0/3:30:15";
        assert!(long.len() <= 64, "{}", long.len());
        c.set("timezone", long).unwrap_or_else(|_| panic!("the long rule is accepted"));
        c.geo_rule = String::try_from("X".repeat(64).as_str()).unwrap();
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

    // VERIFIES: REQ-TZ-05
    #[test]
    fn daylight_saving_is_an_option_default_on() {
        let july = 1_720_000_000;
        let mut c = Config::default();
        assert_eq!(val(&c, "dst"), "on");
        c.set("timezone", "Europe/Berlin").unwrap();
        assert_eq!(c.effective_tz().offset_at(july), 7200);
        c.set("dst", "off").unwrap();
        assert_eq!(val(&c, "dst"), "off");
        assert_eq!(c.effective_tz().offset_at(july), 3600, "standard time all year");
        c.set("timezone", "CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        assert_eq!(c.effective_tz().offset_at(july), 3600, "POSIX rules too");
        c.set("timezone", "auto").unwrap();
        c.set("geo_rule", "EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(c.effective_tz().offset_at(july), -5 * 3600, "and the looked-up zone");
        c.set("timezone", "utc").unwrap();
        assert_eq!(c.effective_tz().offset_at(july), 0);
        assert!(c.set("dst", "maybe").is_err());
        let mut buf = [0u8; 2048];
        let n = c.to_bytes(&mut buf).unwrap();
        assert!(!Config::from_bytes(&buf[..n]).unwrap().dst, "persists");
    }

    // VERIFIES: REQ-NTS-01 REQ-NTS-03
    #[test]
    fn nts_servers_are_validated_and_plain_ones_still_work() {
        let mut c = Config::default();
        for ok in ["nts:time.cloudflare.com", "gateway,nts:nts.time.nl,pool.ntp.org", "nts: time.cloudflare.com ,pool.ntp.org", "NTS:x.y", "Nts:Time.Example.Org"] {
            assert_eq!(c.set("ntp_servers", ok), Ok(()), "{ok}");
        }
        // the prefix is recognised in any case, so a typed "NTS:" is NTS and never a plain host name by accident
        assert_eq!(nts_host("NTS:x.y"), Some("x.y"));
        assert_eq!(nts_host(" nts: x.y "), Some("x.y"));
        assert_eq!(nts_host("ntp.example.org"), None);
        assert_eq!(nts_host("nt"), None);
        assert_eq!(nts_host("gateway"), None);
        assert_eq!(nts_host("ñts:x.y"), None, "multi-byte characters must not panic the prefix check");
        for bad in ["nts:", "nts:localhost", "nts:192.168.1.1", "nts:1.2.3.4", "nts:-bad.example.com", "nts:bad-.example.com", "nts:a..b", "nts:.a.b", "nts:a.b.", "nts:exa mple.com", "nts:a_b.example.com", "nts:ex@mple.com", "gateway,,pool.ntp.org", "nts:a.b,"] {
            assert_eq!(c.set("ntp_servers", bad), Err(ConfigError::InvalidValue), "{bad:?}");
        }
        assert_eq!(c.set("ntp_servers", &format!("nts:{}.example.com", "a".repeat(60))), Err(ConfigError::InvalidValue), "over 64 bytes");
        c.set("ntp_servers", "nts:time.cloudflare.com,pool.ntp.org").unwrap();
        assert_eq!(val(&c, "ntp_servers"), "nts:time.cloudflare.com,pool.ntp.org");
        let mut buf = [0u8; 2048];
        let n = c.to_bytes(&mut buf).unwrap();
        assert_eq!(val(&Config::from_bytes(&buf[..n]).unwrap(), "ntp_servers"), "nts:time.cloudflare.com,pool.ntp.org", "survives storage");
    }

    // VERIFIES: REQ-CFG-08
    #[test]
    fn hostname_setting_names_the_device() {
        let mut c = Config::default();
        assert_eq!(c.device_name(0xab).as_str(), "picoclock-ab", "no custom name: the default from the MAC block");
        assert_eq!(val(&c, "hostname"), "");
        c.set("hostname", "Kitchen-Clock").unwrap();
        assert_eq!(val(&c, "hostname"), "kitchen-clock", "stored lowercase");
        assert_eq!(c.device_name(0xab).as_str(), "kitchen-clock");
        for good in ["a", "clock1", "my-clock", &"a".repeat(HOSTNAME_MAX)] {
            c.set("hostname", good).unwrap_or_else(|_| panic!("{good:?}"));
        }
        for bad in ["-clock", "clock-", "my clock", "my_clock", "clock.local", "uhr\u{e4}", &"a".repeat(HOSTNAME_MAX + 1), "a\nb"] {
            let before = c.clone();
            assert_eq!(c.set("hostname", bad), Err(ConfigError::InvalidValue), "{bad:?}");
            assert_eq!(c, before, "an invalid name changes nothing");
        }
        c.set("hostname", "uhr-2").unwrap();
        let mut buf = [0u8; 2048];
        let n = c.to_bytes(&mut buf).unwrap();
        assert_eq!(Config::from_bytes(&buf[..n]).unwrap().device_name(1).as_str(), "uhr-2", "survives storage");
        c.set("hostname", "").unwrap();
        assert_eq!(c.device_name(0x0c).as_str(), "picoclock-0c", "empty goes back to the default");
    }

    // VERIFIES: REQ-NET-04 REQ-NET-07
    #[test]
    fn wifi_select_and_mdns_settings() {
        let mut c = Config::default();
        assert_eq!((val(&c, "wifi_select"), val(&c, "mdns")), ("order".to_string(), "on".to_string()), "defaults keep today's behaviour");
        c.set("wifi_select", "strongest").unwrap();
        c.set("mdns", "off").unwrap();
        assert!(c.wifi_strongest && !c.mdns);
        for bad in ["best", "", "Strongest ", "on"] {
            assert!(c.set("wifi_select", bad).is_err(), "{bad:?}");
        }
        assert!(c.set("mdns", "maybe").is_err());
        let mut buf = [0u8; 2048];
        let n = c.to_bytes(&mut buf).unwrap();
        let back = Config::from_bytes(&buf[..n]).unwrap();
        assert!(back.wifi_strongest && !back.mdns, "both survive the flash record");
        c.set("ntp_servers", "dhcp,gateway,pool.ntp.org").unwrap();
    }
}
