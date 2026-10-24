//! The u-blox binary protocol (UBX) as far as the clock uses it: frames, the configuration of the receiver, the few messages it
//! reads back (acknowledgement, version, navigation solution), and the splitting of a serial stream that carries both NMEA lines
//! and UBX frames. The time itself still comes from the NMEA sentence and the PPS edge ([`crate::gps`]); UBX is for setting the
//! receiver up and for showing how it is doing (ADR-0007, REQ-GPS-08, REQ-GPS-09).
//!
//! Source of every number: u-blox M9 SPG 4.04 interface description, UBX-21022436 (protocol version 32.01): frame layout (section
//! 3.1), UBX-CFG-VALSET 0x06 0x8a (3.10.28), UBX-CFG-RST 0x06 0x04 (3.10.21), UBX-MON-VER 0x0a 0x04 (3.14.15), UBX-NAV-PVT 0x01
//! 0x07 (3.15.11), UBX-ACK-ACK/NAK (3.9), and the configuration keys of section 6.

use heapless::Vec;

use crate::gps::LineBuffer;

pub const SYNC: [u8; 2] = [0xB5, 0x62];
/// Most payload bytes kept of a received frame (the rest is read and checked, then dropped). NAV-PVT is 92 bytes.
pub const KEEP: usize = 96;
/// Longest frame the clock sends (a VALSET with the whole table).
pub const FRAME_MAX: usize = 96;

pub const CLS_NAV: u8 = 0x01;
pub const CLS_ACK: u8 = 0x05;
pub const CLS_CFG: u8 = 0x06;
pub const CLS_MON: u8 = 0x0A;
pub const ID_PVT: u8 = 0x07;
pub const ID_ACK: u8 = 0x01;
pub const ID_NAK: u8 = 0x00;
pub const ID_VALSET: u8 = 0x8A;
pub const ID_RST: u8 = 0x04;
pub const ID_VER: u8 = 0x04;

/// The Fletcher checksum over class, id, length and payload.
pub fn checksum(bytes: &[u8]) -> [u8; 2] {
    let (mut a, mut b) = (0u8, 0u8);
    for &x in bytes {
        a = a.wrapping_add(x);
        b = b.wrapping_add(a);
    }
    [a, b]
}

/// A complete frame: sync, class, id, length (little endian), payload, checksum.
pub fn frame(class: u8, id: u8, payload: &[u8]) -> Option<Vec<u8, FRAME_MAX>> {
    let mut v = Vec::new();
    v.extend_from_slice(&SYNC).ok()?;
    v.extend_from_slice(&[class, id]).ok()?;
    v.extend_from_slice(&(payload.len() as u16).to_le_bytes()).ok()?;
    v.extend_from_slice(payload).ok()?;
    let ck = checksum(&v[2..]);
    v.extend_from_slice(&ck).ok()?;
    Some(v)
}

/// `UBX-MON-VER` poll (no payload): the receiver answers with its software and hardware version.
pub fn poll_version() -> Vec<u8, FRAME_MAX> {
    frame(CLS_MON, ID_VER, &[]).unwrap()
}

/// How much the receiver forgets at a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetKind {
    /// Keeps everything: a restart of the software, the position and satellite data stay.
    Hot,
    /// Forgets the ephemerides.
    Warm,
    /// Forgets everything stored (a new search for satellites, minutes to the first fix).
    Cold,
}

/// `UBX-CFG-RST`, controlled software reset of the GNSS part only (reset mode 2), with the start kind in the mask. The receiver does
/// not acknowledge it.
pub fn reset(kind: ResetKind) -> Vec<u8, FRAME_MAX> {
    let mask: u16 = match kind {
        ResetKind::Hot => 0x0000,
        ResetKind::Warm => 0x0001,
        ResetKind::Cold => 0xFFFF,
    };
    let m = mask.to_le_bytes();
    frame(CLS_CFG, ID_RST, &[m[0], m[1], 2, 0]).unwrap()
}

/// The constellations to receive (`gps_gnss`). GPS is what the U.S. calls NAVSTAR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gnss(pub u8);

impl Gnss {
    pub const GPS: u8 = 1;
    pub const GALILEO: u8 = 2;
    pub const BEIDOU: u8 = 4;
    pub const GLONASS: u8 = 8;
    /// The default: GPS and Galileo.
    pub const DEFAULT: Gnss = Gnss(Self::GPS | Self::GALILEO);
    const NAMES: [(&'static str, u8); 4] = [("gps", Self::GPS), ("galileo", Self::GALILEO), ("beidou", Self::BEIDOU), ("glonass", Self::GLONASS)];

    /// `gps,galileo`; at least one name, each at most once, no unknown name.
    pub fn parse(s: &str) -> Option<Gnss> {
        let mut bits = 0u8;
        for part in s.split(',') {
            let name = part.trim();
            let bit = Self::NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name))?.1;
            if bits & bit != 0 {
                return None;
            }
            bits |= bit;
        }
        (bits != 0).then_some(Gnss(bits))
    }

    pub fn write(&self, out: &mut impl core::fmt::Write) -> core::fmt::Result {
        let mut first = true;
        for (name, bit) in Self::NAMES {
            if self.0 & bit != 0 {
                if !first {
                    out.write_char(',')?;
                }
                out.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

// configuration keys (section 6 of the interface description); the second hex digit says how many bytes the value has (1 or 2 = one byte)
const SIGNAL_GPS: u32 = 0x1031_001f;
const SIGNAL_GAL: u32 = 0x1031_0021;
const SIGNAL_BDS: u32 = 0x1031_0022;
const SIGNAL_GLO: u32 = 0x1031_0025;
const NAVSPG_DYNMODEL: u32 = 0x2011_0021;
const MSGOUT_UBX_NAV_PVT_UART1: u32 = 0x2091_0007;
const MSGOUT_NMEA_RMC_UART1: u32 = 0x2091_00ac;
/// The NMEA sentences the clock does not need: switching them off leaves the RMC sentence alone and short after the pulse.
const MSGOUT_NMEA_OFF: [u32; 5] = [
    0x2091_00bb, // GGA
    0x2091_00ca, // GLL
    0x2091_00c0, // GSA
    0x2091_00c5, // GSV
    0x2091_00b1, // VTG
];
/// `CFG-NAVSPG-DYNMODEL` = STAT (2): the clock does not move.
const DYN_STATIONARY: u8 = 2;

/// Keys and one-byte values the clock sets: the constellations, only RMC of the NMEA sentences, the navigation solution
/// (UBX-NAV-PVT, for the status) once a second, and the stationary platform model.
pub fn items(g: Gnss) -> Vec<(u32, u8), 16> {
    let mut v = Vec::new();
    for (key, bit) in [(SIGNAL_GPS, Gnss::GPS), (SIGNAL_GAL, Gnss::GALILEO), (SIGNAL_BDS, Gnss::BEIDOU), (SIGNAL_GLO, Gnss::GLONASS)] {
        let _ = v.push((key, (g.0 & bit != 0) as u8));
    }
    for key in MSGOUT_NMEA_OFF {
        let _ = v.push((key, 0));
    }
    let _ = v.push((MSGOUT_NMEA_RMC_UART1, 1));
    let _ = v.push((MSGOUT_UBX_NAV_PVT_UART1, 1));
    let _ = v.push((NAVSPG_DYNMODEL, DYN_STATIONARY));
    v
}

/// `UBX-CFG-VALSET` (version 0) into the RAM layer only: a power cycle of the receiver returns it to its factory state and the
/// clock sets it up again; nothing is written to the receiver's flash.
pub fn config_frame(g: Gnss) -> Vec<u8, FRAME_MAX> {
    let mut payload: Vec<u8, 80> = Vec::new();
    let _ = payload.extend_from_slice(&[0, 0b001, 0, 0]);
    for (key, val) in items(g) {
        let _ = payload.extend_from_slice(&key.to_le_bytes());
        let _ = payload.push(val);
    }
    frame(CLS_CFG, ID_VALSET, &payload).unwrap()
}

/// What came out of the serial stream.
#[derive(Debug, PartialEq, Eq)]
pub enum Item<'a> {
    /// An NMEA sentence from `$` to before the line end.
    Nmea(&'a [u8]),
    /// A UBX frame whose checksum was right: the first [`KEEP`] bytes of its payload, and the length it announced.
    Ubx { class: u8, id: u8, len: u16, payload: &'a [u8] },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Text,
    Sync2,
    Class,
    Id,
    Len1,
    Len2,
    Payload,
    Ck1,
    Ck2,
}

/// Splits the serial line into NMEA sentences and UBX frames. A byte 0xB5 never occurs in a sentence (ASCII), so it starts a frame.
#[derive(Debug)]
pub struct Demux {
    text: LineBuffer,
    st: St,
    class: u8,
    id: u8,
    len: u16,
    got: u16,
    sum: [u8; 2],
    ck1: u8,
    buf: Vec<u8, KEEP>,
    /// Frames dropped for a wrong checksum.
    pub bad_frames: u32,
}

impl Demux {
    pub const fn new() -> Self {
        Self { text: LineBuffer::new(), st: St::Text, class: 0, id: 0, len: 0, got: 0, sum: [0; 2], ck1: 0, buf: Vec::new(), bad_frames: 0 }
    }

    pub fn overlong(&self) -> u32 {
        self.text.overlong
    }

    fn feed_sum(&mut self, b: u8) {
        self.sum[0] = self.sum[0].wrapping_add(b);
        self.sum[1] = self.sum[1].wrapping_add(self.sum[0]);
    }

    /// Feeds one byte; returns an item when `b` finishes one. Call [`Demux::done`] after using it.
    pub fn push(&mut self, b: u8) -> Option<Item<'_>> {
        match self.st {
            St::Text => {
                if b == SYNC[0] {
                    self.text.done();
                    self.st = St::Sync2;
                    return None;
                }
                self.text.push(b).map(Item::Nmea)
            }
            St::Sync2 => {
                if b == SYNC[1] {
                    self.st = St::Class;
                    self.buf.clear();
                    self.sum = [0; 2];
                    None
                } else {
                    self.st = St::Text;
                    self.push(b)
                }
            }
            St::Class => {
                self.class = b;
                self.feed_sum(b);
                self.st = St::Id;
                None
            }
            St::Id => {
                self.id = b;
                self.feed_sum(b);
                self.st = St::Len1;
                None
            }
            St::Len1 => {
                self.len = b as u16;
                self.feed_sum(b);
                self.st = St::Len2;
                None
            }
            St::Len2 => {
                self.len |= (b as u16) << 8;
                self.got = 0;
                self.feed_sum(b);
                self.st = if self.len == 0 { St::Ck1 } else { St::Payload };
                None
            }
            St::Payload => {
                self.feed_sum(b);
                let _ = self.buf.push(b); // the part beyond KEEP is only summed
                self.got += 1;
                if self.got == self.len {
                    self.st = St::Ck1;
                }
                None
            }
            St::Ck1 => {
                self.ck1 = b;
                self.st = St::Ck2;
                None
            }
            St::Ck2 => {
                self.st = St::Text;
                if [self.ck1, b] == self.sum {
                    Some(Item::Ubx { class: self.class, id: self.id, len: self.len, payload: &self.buf })
                } else {
                    self.bad_frames += 1;
                    None
                }
            }
        }
    }

    /// Forgets the finished item.
    pub fn done(&mut self) {
        self.text.done();
        self.buf.clear();
    }
}

impl Default for Demux {
    fn default() -> Self {
        Self::new()
    }
}

/// An acknowledgement of a configuration message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ack {
    pub accepted: bool,
    pub class: u8,
    pub id: u8,
}

pub fn ack(id: u8, payload: &[u8]) -> Option<Ack> {
    match (id, payload) {
        (ID_ACK | ID_NAK, [class, msg, ..]) => Some(Ack { accepted: id == ID_ACK, class: *class, id: *msg }),
        _ => None,
    }
}

/// The navigation solution as far as `status` shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pvt {
    /// 0 none, 1 dead reckoning, 2 2D, 3 3D, 4 GNSS plus dead reckoning, 5 time only.
    pub fix: u8,
    pub sats: u8,
    /// UTC date and time valid and fully resolved (the receiver knows the leap seconds): the 18 s problem of a cold start is over.
    pub utc_ok: bool,
    pub t_acc_ns: u32,
}

/// `UBX-NAV-PVT` is 92 bytes: valid at 11, tAcc at 12, fixType at 20, numSV at 23.
pub fn pvt(len: u16, p: &[u8]) -> Option<Pvt> {
    if len != 92 || p.len() < 24 {
        return None;
    }
    Some(Pvt { fix: p[20], sats: p[23], utc_ok: p[11] & 0b110 == 0b110, t_acc_ns: u32::from_le_bytes([p[12], p[13], p[14], p[15]]) })
}

/// Longest software version kept for `status`.
pub const VER_LEN: usize = 24;

/// The first (software version) string of `UBX-MON-VER`, up to its NUL, printable ASCII only.
pub fn version(p: &[u8]) -> Option<([u8; VER_LEN], u8)> {
    let field = p.get(..30)?;
    let end = field.iter().position(|&c| c == 0).unwrap_or(30);
    let mut out = [0u8; VER_LEN];
    let mut n = 0;
    for &c in &field[..end] {
        if (0x20..0x7f).contains(&c) && n < VER_LEN {
            out[n] = c;
            n += 1;
        }
    }
    (n > 0).then_some((out, n as u8))
}

/// How far the receiver's setup has come (shown in `status`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Setup {
    /// `gps_config` is off: the clock sends nothing.
    #[default]
    Off,
    Sending,
    Applied,
    /// The receiver answered NAK (an unknown key or a combination it does not accept).
    Refused,
    /// Three tries without an answer: no u-blox receiver, or the TX wire is missing.
    NoAnswer,
}

/// What the receiver told about itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Module {
    pub setup: Setup,
    pub ver: [u8; VER_LEN],
    pub ver_len: u8,
    /// A navigation solution has arrived (the fields below mean something).
    pub pvt_seen: bool,
    pub fix: u8,
    pub sats: u8,
    pub utc_ok: bool,
    pub t_acc_ns: u32,
}

impl Module {
    pub fn version_str(&self) -> &str {
        core::str::from_utf8(&self.ver[..self.ver_len as usize]).unwrap_or("")
    }
}

/// What the configurator wants sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Send {
    Config,
    PollVersion,
}

/// Time to wait for an answer, number of tries, and the pauses (milliseconds).
const ANSWER_MS: u64 = 2_000;
const TRIES: u8 = 3;
const PVT_MISSING_MS: u64 = 5_000;
const RETRY_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Config,
    Version,
    Done,
    Refused,
    NoAnswer,
}

/// Sets the receiver up after it has started talking, and notices when it has forgotten the setup (a power cycle: its navigation
/// solution stops arriving because that output is part of the setup).
#[derive(Debug)]
pub struct Configurator {
    phase: Phase,
    tries: u8,
    next_ms: u64,
    since_ms: u64,
    last_pvt_ms: u64,
}

impl Configurator {
    pub const fn new() -> Self {
        Self { phase: Phase::Idle, tries: 0, next_ms: 0, since_ms: 0, last_pvt_ms: 0 }
    }

    /// Begin (again) at `now_ms`: the receiver has just started talking, or the setting changed.
    pub fn restart(&mut self, now_ms: u64) {
        self.phase = Phase::Config;
        self.tries = 0;
        self.next_ms = now_ms;
        self.since_ms = now_ms;
    }

    /// `gps_config` off: send nothing.
    pub fn stop(&mut self) {
        self.phase = Phase::Idle;
    }

    pub fn setup(&self) -> Setup {
        match self.phase {
            Phase::Idle => Setup::Off,
            Phase::Config | Phase::Version => Setup::Sending,
            Phase::Done => Setup::Applied,
            Phase::Refused => Setup::Refused,
            Phase::NoAnswer => Setup::NoAnswer,
        }
    }

    /// Call at least once a second; says what to send now, if anything.
    pub fn due(&mut self, now_ms: u64) -> Option<Send> {
        match self.phase {
            Phase::Idle | Phase::Refused => None,
            Phase::Config | Phase::Version if now_ms < self.next_ms => None,
            Phase::Config => {
                if self.tries >= TRIES {
                    self.phase = Phase::NoAnswer;
                    self.since_ms = now_ms;
                    return None;
                }
                self.tries += 1;
                self.next_ms = now_ms + ANSWER_MS;
                Some(Send::Config)
            }
            Phase::Version => {
                if self.tries >= TRIES {
                    // the version is only for show: the setup worked
                    self.phase = Phase::Done;
                    self.last_pvt_ms = now_ms;
                    return None;
                }
                self.tries += 1;
                self.next_ms = now_ms + ANSWER_MS;
                Some(Send::PollVersion)
            }
            Phase::Done => {
                if now_ms.saturating_sub(self.last_pvt_ms) > PVT_MISSING_MS && now_ms.saturating_sub(self.since_ms) > PVT_MISSING_MS {
                    // the solution stopped: the receiver was reset and has forgotten the setup
                    self.restart(now_ms);
                    return self.due(now_ms);
                }
                None
            }
            Phase::NoAnswer => {
                if now_ms.saturating_sub(self.since_ms) >= RETRY_MS {
                    self.restart(now_ms);
                    return self.due(now_ms);
                }
                None
            }
        }
    }

    /// Feed what the receiver sent (UBX frames only; sentences are not its business).
    pub fn item(&mut self, now_ms: u64, item: &Item<'_>, module: &mut Module) {
        let Item::Ubx { class, id, len, payload } = *item else { return };
        match (class, id) {
            (CLS_ACK, ID_ACK | ID_NAK) => {
                let Some(a) = ack(id, payload) else { return };
                if (a.class, a.id) == (CLS_CFG, ID_VALSET) && self.phase == Phase::Config {
                    if a.accepted {
                        self.phase = Phase::Version;
                        self.tries = 0;
                        self.next_ms = now_ms;
                    } else {
                        self.phase = Phase::Refused;
                    }
                }
            }
            (CLS_MON, ID_VER) => {
                if let Some((ver, n)) = version(payload) {
                    module.ver = ver;
                    module.ver_len = n;
                }
                if self.phase == Phase::Version {
                    self.phase = Phase::Done;
                    self.last_pvt_ms = now_ms;
                }
            }
            (CLS_NAV, ID_PVT) => {
                if let Some(p) = pvt(len, payload) {
                    module.pvt_seen = true;
                    module.fix = p.fix;
                    module.sats = p.sats;
                    module.utc_ok = p.utc_ok;
                    module.t_acc_ns = p.t_acc_ns;
                    self.last_pvt_ms = now_ms;
                }
            }
            _ => {}
        }
        module.setup = self.setup();
    }

    /// Bring `module.setup` up to date after [`Configurator::due`] changed the phase.
    pub fn sync(&self, module: &mut Module) {
        module.setup = self.setup();
    }
}

impl Default for Configurator {
    fn default() -> Self {
        Self::new()
    }
}

/// The module line of `status`: version, setup, fix, satellites, UTC validity.
pub fn describe(out: &mut impl core::fmt::Write, m: &Module) -> core::fmt::Result {
    match m.setup {
        Setup::Off => return Ok(()),
        Setup::Sending => out.write_str("setting up the receiver")?,
        Setup::Applied => out.write_str("set up")?,
        Setup::Refused => out.write_str("the receiver refused the setup (NAK)")?,
        Setup::NoAnswer => out.write_str("no answer to the setup (not a u-blox receiver, or GP4 is not connected to its RX)")?,
    }
    if m.ver_len > 0 {
        write!(out, "; {}", m.version_str())?;
    }
    if m.pvt_seen {
        let fix = match m.fix {
            0 => "no fix",
            1 => "dead reckoning",
            2 => "2D fix",
            3 => "3D fix",
            4 => "GNSS + dead reckoning",
            5 => "time only fix",
            _ => "unknown fix",
        };
        write!(out, "; {}, {} satellites, UTC {}", fix, m.sats, if m.utc_ok { "valid" } else { "not valid yet" })?;
        if m.utc_ok && m.t_acc_ns != u32::MAX {
            write!(out, ", accuracy {} ns", m.t_acc_ns)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::string::String;

    fn feed(d: &mut Demux, bytes: &[u8]) -> std::vec::Vec<std::string::String> {
        let mut out = std::vec::Vec::new();
        for &b in bytes {
            if let Some(item) = d.push(b) {
                out.push(match item {
                    Item::Nmea(l) => std::format!("nmea:{}", String::from_utf8_lossy(l)),
                    Item::Ubx { class, id, len, payload } => std::format!("ubx:{class:02x}{id:02x} len {len} kept {}", payload.len()),
                });
                d.done();
            }
        }
        out
    }

    // VERIFIES: REQ-GPS-08
    #[test]
    fn frames_carry_the_fletcher_checksum_of_the_published_examples() {
        // the MON-VER poll and the cold-start reset as printed in u-blox documents and in every UBX tool
        assert_eq!(poll_version().as_slice(), &[0xB5, 0x62, 0x0A, 0x04, 0x00, 0x00, 0x0E, 0x34]);
        assert_eq!(reset(ResetKind::Cold).as_slice(), &[0xB5, 0x62, 0x06, 0x04, 0x04, 0x00, 0xFF, 0xFF, 0x02, 0x00, 0x0E, 0x61]);
        assert_eq!(&reset(ResetKind::Hot)[6..10], &[0, 0, 2, 0], "hot start: empty mask");
        assert_eq!(&reset(ResetKind::Warm)[6..10], &[1, 0, 2, 0], "warm start: ephemerides");
    }

    // VERIFIES: REQ-GPS-08
    #[test]
    fn the_configuration_is_one_valset_into_ram_with_the_keys_of_the_manual() {
        let f = config_frame(Gnss::DEFAULT);
        assert_eq!(&f[..4], &[0xB5, 0x62, 0x06, 0x8A]);
        let len = u16::from_le_bytes([f[4], f[5]]) as usize;
        assert_eq!(f.len(), 6 + len + 2);
        assert_eq!(&f[6..10], &[0, 0b001, 0, 0], "version 0, RAM layer only, reserved");
        assert_eq!((len - 4) % 5, 0, "every item is a 4-byte key and a 1-byte value");
        assert_eq!(checksum(&f[2..f.len() - 2]), [f[f.len() - 2], f[f.len() - 1]]);
        // GPS and Galileo on, BeiDou and GLONASS off, keys little endian
        let has = |key: u32, val: u8| f[10..f.len() - 2].chunks(5).any(|c| c[..4] == key.to_le_bytes() && c[4] == val);
        assert!(has(0x1031001f, 1) && has(0x10310021, 1), "GPS and Galileo on");
        assert!(has(0x10310022, 0) && has(0x10310025, 0), "BeiDou and GLONASS off");
        assert!(has(0x209100ac, 1), "RMC stays on");
        for gga_etc in [0x209100bb, 0x209100ca, 0x209100c0, 0x209100c5, 0x209100b1] {
            assert!(has(gga_etc, 0), "the other sentences are off");
        }
        assert!(has(0x20910007, 1) && has(0x20110021, 2), "NAV-PVT once a second, stationary model");
        // each constellation on its own switches only its own key on
        for (bit, on) in [(Gnss::GPS, 0x1031001f), (Gnss::GALILEO, 0x10310021), (Gnss::BEIDOU, 0x10310022), (Gnss::GLONASS, 0x10310025)] {
            let f = config_frame(Gnss(bit));
            let has = |key: u32, val: u8| f[10..f.len() - 2].chunks(5).any(|c| c[..4] == key.to_le_bytes() && c[4] == val);
            for key in [0x1031001fu32, 0x10310021, 0x10310022, 0x10310025] {
                assert!(has(key, (key == on) as u8), "only {on:#x} is on for bit {bit}, not {key:#x}");
            }
        }
    }

    // VERIFIES: REQ-GPS-08
    #[test]
    fn constellation_lists_parse_and_print_and_refuse_nonsense() {
        assert_eq!(Gnss::parse("gps,galileo"), Some(Gnss::DEFAULT));
        assert_eq!(Gnss::parse(" Galileo , GPS "), Some(Gnss::DEFAULT));
        assert_eq!(Gnss::parse("glonass"), Some(Gnss(8)));
        for bad in ["", "gps,gps", "navstar", "gps,,galileo", "gps;galileo"] {
            assert_eq!(Gnss::parse(bad), None, "{bad:?}");
        }
        let mut s = String::new();
        Gnss(0b1111).write(&mut s).unwrap();
        assert_eq!(s, "gps,galileo,beidou,glonass");
    }

    // VERIFIES: REQ-GPS-09
    #[test]
    fn nmea_and_ubx_share_one_line_and_each_comes_out_whole() {
        let mut d = Demux::new();
        let ack_frame = frame(CLS_ACK, ID_ACK, &[CLS_CFG, ID_VALSET]).unwrap();
        let mut stream = std::vec::Vec::new();
        stream.extend_from_slice(b"$GPGGA,1*00\r\n");
        stream.extend_from_slice(&ack_frame);
        stream.extend_from_slice(b"$GNRMC,2*00\r\n");
        let got = feed(&mut d, &stream);
        assert_eq!(got, ["nmea:$GPGGA,1*00", "ubx:0501 len 2 kept 2", "nmea:$GNRMC,2*00"]);
        // a frame whose payload holds the bytes of a line end, a dollar sign and the sync bytes is still one frame
        let tricky = frame(0x0A, 0x04, b"\r\n$\xB5\x62\xB5\x62 payload").unwrap();
        assert_eq!(feed(&mut d, &tricky), ["ubx:0a04 len 15 kept 15"]);
    }

    // VERIFIES: REQ-GPS-09
    #[test]
    fn a_damaged_frame_is_counted_and_dropped_and_the_next_thing_is_found() {
        let mut d = Demux::new();
        let mut bad = frame(CLS_ACK, ID_ACK, &[6, 0x8A]).unwrap();
        let n = bad.len();
        bad[n - 1] ^= 0x01;
        let mut stream = std::vec::Vec::new();
        stream.extend_from_slice(&bad);
        stream.extend_from_slice(b"$GNRMC,3*00\r\n");
        assert_eq!(feed(&mut d, &stream), ["nmea:$GNRMC,3*00"]);
        assert_eq!(d.bad_frames, 1);
        // a lone sync byte in noise does not swallow the sentence behind it
        let mut d = Demux::new();
        assert_eq!(feed(&mut d, b"\xB5x$GNRMC,4*00\r\n"), ["nmea:$GNRMC,4*00"]);
    }

    // VERIFIES: REQ-GPS-09
    #[test]
    fn a_long_frame_is_checked_whole_and_kept_in_part() {
        let mut d = Demux::new();
        let payload: std::vec::Vec<u8> = (0..300u16).map(|i| (i % 251) as u8).collect();
        let mut f: std::vec::Vec<u8> = std::vec::Vec::new();
        f.extend_from_slice(&SYNC);
        f.extend_from_slice(&[CLS_MON, ID_VER]);
        f.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        f.extend_from_slice(&payload);
        let ck = checksum(&f[2..]);
        f.extend_from_slice(&ck);
        assert_eq!(feed(&mut d, &f), [std::format!("ubx:0a04 len 300 kept {KEEP}")]);
        let n = f.len();
        f[n / 2] ^= 0x40; // damage beyond the kept part
        assert_eq!(feed(&mut d, &f), std::vec::Vec::<String>::new());
        assert_eq!(d.bad_frames, 1, "bytes that are not kept are still part of the checksum");
    }

    fn pvt_payload(fix: u8, sats: u8, valid: u8, t_acc: u32) -> [u8; 92] {
        let mut p = [0u8; 92];
        p[11] = valid;
        p[12..16].copy_from_slice(&t_acc.to_le_bytes());
        p[20] = fix;
        p[23] = sats;
        p
    }

    // VERIFIES: REQ-GPS-09
    #[test]
    fn the_messages_the_receiver_sends_back_are_read_at_the_offsets_of_the_manual() {
        assert_eq!(ack(ID_ACK, &[6, 0x8A]), Some(Ack { accepted: true, class: 6, id: 0x8A }));
        assert_eq!(ack(ID_NAK, &[6, 0x8A]), Some(Ack { accepted: false, class: 6, id: 0x8A }));
        assert_eq!(ack(ID_ACK, &[6]), None);
        let p = pvt_payload(3, 17, 0b111, 25);
        assert_eq!(pvt(92, &p), Some(Pvt { fix: 3, sats: 17, utc_ok: true, t_acc_ns: 25 }));
        assert_eq!(pvt(92, &pvt_payload(3, 5, 0b011, 25)).unwrap().utc_ok, false, "time valid but not fully resolved");
        assert_eq!(pvt(91, &p), None, "only the 92-byte message");
        let mut v = [0u8; 70];
        v[..22].copy_from_slice(b"EXT CORE 4.04 (7f89f7)");
        let (s, n) = version(&v).unwrap();
        assert_eq!(&s[..n as usize], b"EXT CORE 4.04 (7f89f7)");
        let mut noisy = [0u8; 30];
        noisy[..4].copy_from_slice(b"a\x01b\xff");
        assert_eq!(version(&noisy).map(|(s, n)| s[..n as usize].to_vec()), Some(b"ab".to_vec()), "control bytes are not shown");
        assert_eq!(version(&[0u8; 30]), None);
    }

    // VERIFIES: REQ-GPS-08
    #[test]
    fn the_setup_is_sent_until_acknowledged_then_the_version_is_asked_and_a_lost_setup_is_noticed() {
        let mut c = Configurator::new();
        let mut m = Module::default();
        assert_eq!(c.due(0), None, "idle until told");
        c.restart(1_000);
        assert_eq!(c.due(1_000), Some(Send::Config));
        assert_eq!(c.due(2_500), None, "waits two seconds for the answer");
        assert_eq!(c.due(3_000), Some(Send::Config), "asks again");
        c.sync(&mut m);
        assert_eq!(m.setup, Setup::Sending);
        let ack_ok = [CLS_CFG, ID_VALSET];
        c.item(3_100, &Item::Ubx { class: CLS_ACK, id: ID_ACK, len: 2, payload: &ack_ok }, &mut m);
        assert_eq!(c.due(3_100), Some(Send::PollVersion));
        let mut ver = [0u8; 40];
        ver[..13].copy_from_slice(b"EXT CORE 4.04");
        c.item(3_200, &Item::Ubx { class: CLS_MON, id: ID_VER, len: 40, payload: &ver }, &mut m);
        assert_eq!((m.setup, m.version_str()), (Setup::Applied, "EXT CORE 4.04"));
        // the navigation solution arrives once a second and keeps the setup standing
        let p = pvt_payload(3, 12, 0b111, 20);
        for s in 4..30u64 {
            c.item(s * 1000, &Item::Ubx { class: CLS_NAV, id: ID_PVT, len: 92, payload: &p }, &mut m);
            assert_eq!(c.due(s * 1000), None);
        }
        assert_eq!((m.sats, m.fix, m.utc_ok), (12, 3, true));
        // it stops (the receiver was reset and forgot): after five seconds the setup is sent again
        assert_eq!(c.due(33_000), None);
        assert_eq!(c.due(35_500), Some(Send::Config));
    }

    // VERIFIES: REQ-GPS-08
    #[test]
    fn a_refusal_stops_and_silence_gives_up_after_three_tries_and_tries_again_later() {
        let mut m = Module::default();
        let mut c = Configurator::new();
        c.restart(0);
        assert_eq!(c.due(0), Some(Send::Config));
        let nak = [CLS_CFG, ID_VALSET];
        c.item(100, &Item::Ubx { class: CLS_ACK, id: ID_NAK, len: 2, payload: &nak }, &mut m);
        assert_eq!(m.setup, Setup::Refused);
        assert_eq!(c.due(100_000), None, "a refusal is not repeated until the setting changes");
        c.restart(100_000);
        assert_eq!(c.due(100_000), Some(Send::Config));

        let mut c = Configurator::new();
        c.restart(0);
        assert_eq!((c.due(0), c.due(2_000), c.due(4_000)), (Some(Send::Config), Some(Send::Config), Some(Send::Config)));
        assert_eq!(c.due(6_000), None);
        c.sync(&mut m);
        assert_eq!(m.setup, Setup::NoAnswer);
        assert_eq!(c.due(30_000), None);
        assert_eq!(c.due(66_500), Some(Send::Config), "a minute later it tries again");
        c.stop();
        c.sync(&mut m);
        assert_eq!((c.due(500_000), m.setup), (None, Setup::Off));
    }

    // VERIFIES: REQ-GPS-09
    #[test]
    fn the_status_text_says_what_the_receiver_reported() {
        let mut m = Module::default();
        let mut s = String::new();
        describe(&mut s, &m).unwrap();
        assert_eq!(s, "", "nothing while the setup is off");
        m.setup = Setup::Applied;
        m.ver[..13].copy_from_slice(b"EXT CORE 4.04");
        m.ver_len = 13;
        m.pvt_seen = true;
        m.fix = 3;
        m.sats = 14;
        m.utc_ok = true;
        m.t_acc_ns = 12;
        s.clear();
        describe(&mut s, &m).unwrap();
        assert_eq!(s, "set up; EXT CORE 4.04; 3D fix, 14 satellites, UTC valid, accuracy 12 ns");
        m.utc_ok = false;
        m.fix = 0;
        s.clear();
        describe(&mut s, &m).unwrap();
        assert!(s.contains("no fix, 14 satellites, UTC not valid yet"), "{s}");
        m.setup = Setup::NoAnswer;
        s.clear();
        describe(&mut s, &m).unwrap();
        assert!(s.contains("GP4"), "{s}");
    }
}
