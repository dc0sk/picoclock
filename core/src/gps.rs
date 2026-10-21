//! GPS time from a u-blox receiver (ADR-0007). The receiver sends NMEA 0183 sentences on its serial line and a pulse per
//! second (PPS) on the time-pulse pin; the sentence `RMC` names the UTC second that began at the pulse before it. The firmware
//! stamps every PPS edge and every finished line with its microsecond timer and gives them to the [`Tracker`]; everything else,
//! the framing, the checksum, the fields, the calendar, the pairing of a sentence with its edge, the settling and the rate
//! of the offers, is here so that it can be tested without a receiver.
//!
//! The clock only listens. A GPS time is reported only for a sentence that is valid *and* paired with exactly one edge, and only
//! after [`SETTLE`] such seconds in a row that follow one another exactly (REQ-GPS-01 to 04).

use core::fmt::Write;

use crate::time::{days_from_civil, days_in_month};

/// Longest NMEA sentence is 82 bytes; a line that does not end within this many bytes after the `$` is dropped.
pub const LINE_MAX: usize = 100;
/// How long before the end of its line the PPS edge of a sentence may lie (the receiver sends the sentence of a second within
/// a fraction of it).
pub const PAIR_WINDOW_US: u64 = 900_000;
/// Valid, consecutive, paired seconds needed before a time is offered.
pub const SETTLE: u32 = 30;
/// Two PPS edges of consecutive seconds are this far apart, within [`EDGE_TOLERANCE_US`].
const SECOND_US: u64 = 1_000_000;
pub const EDGE_TOLERANCE_US: u64 = 10_000;
/// Time between two offers to the source selection while settled.
pub const OFFER_EVERY_US: u64 = 60_000_000;

/// Why a sentence gave no time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// Not an RMC sentence (the receiver sends many others; they are not counted).
    NotRmc,
    Checksum,
    /// The receiver says it has no valid fix (status V, or a mode indicator other than A or D).
    NoFix,
    /// A field missing or out of range.
    Malformed,
}

/// The UTC second an RMC sentence names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rmc {
    pub unix_s: i64,
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Two decimal digits.
fn two(s: &[u8]) -> Option<u32> {
    match s {
        [a @ b'0'..=b'9', b @ b'0'..=b'9'] => Some((a - b'0') as u32 * 10 + (b - b'0') as u32),
        _ => None,
    }
}

/// Parses one line (from the `$`, without the line end). Only an RMC sentence with a valid fix gives a time.
pub fn parse_rmc(line: &[u8]) -> Result<Rmc, Reject> {
    let Some(body) = line.strip_prefix(b"$") else { return Err(Reject::Malformed) };
    // the talker is any two letters; the sentence type is the next three
    if body.len() < 5 || &body[2..5] != b"RMC" || !body[..2].iter().all(u8::is_ascii_uppercase) {
        return Err(Reject::NotRmc);
    }
    let star = body.iter().rposition(|b| *b == b'*').ok_or(Reject::Checksum)?;
    let (payload, tail) = (&body[..star], &body[star + 1..]);
    let [h, l] = tail else { return Err(Reject::Checksum) };
    let (Some(h), Some(l)) = (hex(*h), hex(*l)) else { return Err(Reject::Checksum) };
    if payload.iter().fold(0u8, |x, b| x ^ b) != (h << 4 | l) {
        return Err(Reject::Checksum);
    }
    let mut f = payload.split(|b| *b == b',');
    let _ = f.next(); // GNRMC
    let time = f.next().ok_or(Reject::Malformed)?;
    let status = f.next().ok_or(Reject::Malformed)?;
    // an empty status or empty time is what a receiver without a fix sends
    if status != b"A" {
        return if status == b"V" || status.is_empty() { Err(Reject::NoFix) } else { Err(Reject::Malformed) };
    }
    let date = f.nth(6).ok_or(Reject::Malformed)?; // lat, N, lon, E, speed, course, then the date
    let mode = f.nth(2); // magnetic variation, its direction, then the mode indicator (absent before NMEA 2.3)
    if let Some(m) = mode
        && !m.is_empty()
        && m != b"A"
        && m != b"D"
    {
        return Err(if m == b"N" || m == b"E" || m == b"S" || m == b"M" { Reject::NoFix } else { Reject::Malformed });
    }
    // time: hhmmss, optionally .fff; a fraction other than zero is not a whole second
    let (hms, frac) = match time.iter().position(|b| *b == b'.') {
        Some(p) => (&time[..p], &time[p + 1..]),
        None => (time, &b""[..]),
    };
    if hms.len() != 6 || !frac.iter().all(|b| *b == b'0') {
        return Err(Reject::Malformed);
    }
    let (hh, mm, ss) = (two(&hms[0..2]), two(&hms[2..4]), two(&hms[4..6]));
    let (Some(hh), Some(mm), Some(ss)) = (hh, mm, ss) else { return Err(Reject::Malformed) };
    if hh > 23 || mm > 59 || ss > 59 {
        return Err(Reject::Malformed); // 60 (a leap second) is refused: one second lost is cheaper than a wrong one
    }
    if date.len() != 6 {
        return Err(Reject::Malformed);
    }
    let (d, m, y) = (two(&date[0..2]), two(&date[2..4]), two(&date[4..6]));
    let (Some(d), Some(m), Some(y)) = (d, m, y) else { return Err(Reject::Malformed) };
    let year = 2000 + y as i64;
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(year, m as u8) as u32 {
        return Err(Reject::Malformed);
    }
    let days = days_from_civil(year, m as u8, d as u8);
    Ok(Rmc { unix_s: days * 86_400 + (hh * 3600 + mm * 60 + ss) as i64 })
}

/// An RMC sentence for the UTC second `unix_s` as a u-blox receiver sends it (a fix at a fixed place), with its checksum, without
/// the line end. For the simulated receiver of the test build and for the tests.
pub fn build_rmc(unix_s: i64, out: &mut impl core::fmt::Write) -> core::fmt::Result {
    let (y, m, d) = crate::time::civil_from_days(unix_s.div_euclid(86_400));
    let secs = unix_s.rem_euclid(86_400);
    let mut body = heapless::String::<100>::new();
    write!(
        body,
        "GNRMC,{:02}{:02}{:02}.00,A,4717.11437,N,00833.91522,E,0.004,77.52,{:02}{:02}{:02},,,A,V",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        d,
        m,
        y.rem_euclid(100)
    )?;
    write!(out, "${body}*{:02X}", body.bytes().fold(0u8, |x, b| x ^ b))
}

/// Collects bytes from the serial line into sentences.
#[derive(Debug)]
pub struct LineBuffer {
    buf: heapless::Vec<u8, LINE_MAX>,
    /// Lines that were too long to be a sentence.
    pub overlong: u32,
}

impl LineBuffer {
    pub const fn new() -> Self {
        Self { buf: heapless::Vec::new(), overlong: 0 }
    }

    /// Feeds one byte; returns the finished line (starting at `$`, without the line end) when `b` ends it.
    pub fn push(&mut self, b: u8) -> Option<&[u8]> {
        match b {
            b'$' => {
                self.buf.clear();
                let _ = self.buf.push(b);
                None
            }
            b'\n' if !self.buf.is_empty() => {
                // strip the CR that precedes the LF
                if self.buf.last() == Some(&b'\r') {
                    self.buf.pop();
                }
                Some(&self.buf)
            }
            b'\r' | b'\n' => None,
            _ if self.buf.is_empty() => None, // noise before the first $
            _ => {
                if self.buf.push(b).is_err() {
                    self.overlong += 1;
                    self.buf.clear();
                }
                None
            }
        }
    }

    /// Forgets the finished line (call after using the result of [`LineBuffer::push`]).
    pub fn done(&mut self) {
        self.buf.clear();
    }
}

impl Default for LineBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Counters for `status` (REQ-GPS-06).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// RMC sentences that were valid and had a fix.
    pub valid: u32,
    pub bad_checksum: u32,
    /// RMC sentences without a fix.
    pub no_fix: u32,
    /// RMC sentences with a missing or impossible field.
    pub malformed: u32,
    /// PPS edges seen.
    pub edges: u32,
    /// Valid sentences without exactly one edge before them.
    pub unpaired: u32,
    /// Times a run of good seconds was broken after it had started.
    pub breaks: u32,
}

/// A GPS time: `unix_s` was the UTC second at the PPS edge seen at `edge_us` on the caller's microsecond clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fix {
    pub unix_s: i64,
    pub edge_us: u64,
}

impl Fix {
    /// The fix as the source selection takes it: UTC milliseconds at a monotonic instant in milliseconds (the edge, rounded to the
    /// nearest millisecond: at most half a millisecond of error).
    pub fn sample_ms(&self) -> (i64, u64) {
        (self.unix_s * 1000, (self.edge_us + 500) / 1000)
    }
}

/// What `status` and the web page show about the receiver (REQ-GPS-06).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub stats: Stats,
    /// Consecutive good seconds so far.
    pub run: u32,
    /// Bytes received from the receiver since the source was switched on (0 = nothing arrives on the wire).
    pub bytes: u32,
    /// No byte for a few seconds.
    pub silent: bool,
    /// Milliseconds since the last PPS edge.
    pub since_edge_ms: Option<u32>,
    /// Framing, overrun and break errors of the serial line (a wrong `gps_baud` gives many).
    pub uart_errors: u32,
}

/// One line for the terminal and the web page: the state, then the counters.
pub fn describe(out: &mut impl core::fmt::Write, r: &Report) -> core::fmt::Result {
    if r.bytes == 0 {
        out.write_str("receiver silent: nothing arrives on GP5 (check the wire, power and gps_baud)")?;
    } else if r.silent {
        out.write_str("receiver went silent")?;
    } else if r.run >= SETTLE {
        out.write_str("settled")?;
    } else {
        write!(out, "settling {}/{}", r.run, SETTLE)?;
    }
    write!(out, "; valid {}, no fix {}, bad checksum {}, malformed {}, serial errors {}", r.stats.valid, r.stats.no_fix, r.stats.bad_checksum, r.stats.malformed, r.uart_errors)?;
    write!(out, "; PPS edges {}", r.stats.edges)?;
    match r.since_edge_ms {
        Some(ms) => write!(out, " (last {}.{} s ago)", ms / 1000, ms % 1000 / 100)?,
        None if r.bytes > 0 => out.write_str(" (none seen: check the PPS wire on GP8; without it there is no GPS time)")?,
        None => {}
    }
    write!(out, ", unpaired sentences {}, runs broken {}", r.stats.unpaired, r.stats.breaks)
}

/// Pairs sentences with PPS edges, counts consecutive good seconds, and decides when a time is offered.
#[derive(Debug, Default)]
pub struct Tracker {
    pub stats: Stats,
    edges: [u64; 3],
    n_edges: usize,
    /// The last good second and its edge.
    prev: Option<(i64, u64)>,
    run: u32,
    last_offer: Option<u64>,
}

impl Tracker {
    pub const fn new() -> Self {
        Self { stats: Stats { valid: 0, bad_checksum: 0, no_fix: 0, malformed: 0, edges: 0, unpaired: 0, breaks: 0 }, edges: [0; 3], n_edges: 0, prev: None, run: 0, last_offer: None }
    }

    /// A PPS edge was seen at `at_us`.
    pub fn edge(&mut self, at_us: u64) {
        self.stats.edges = self.stats.edges.wrapping_add(1);
        self.edges = [self.edges[1], self.edges[2], at_us];
        self.n_edges = (self.n_edges + 1).min(3);
    }

    /// Consecutive good seconds so far (up to [`SETTLE`] and beyond while settled).
    pub fn run(&self) -> u32 {
        self.run
    }

    pub fn settled(&self) -> bool {
        self.run >= SETTLE
    }

    /// Microseconds since the last PPS edge, if there was one.
    pub fn since_edge(&self, now_us: u64) -> Option<u64> {
        (self.n_edges > 0).then(|| now_us.saturating_sub(self.edges[2]))
    }

    fn lose(&mut self) {
        if self.run > 0 {
            self.stats.breaks = self.stats.breaks.wrapping_add(1);
        }
        self.run = 0;
        self.prev = None;
    }

    /// No sentence for a while (the line is silent): the run is broken.
    pub fn silent(&mut self) {
        self.lose();
    }

    /// A finished line that ended at `end_us`. Returns a [`Fix`] when a time is due to be offered.
    pub fn line(&mut self, line: &[u8], end_us: u64) -> Option<Fix> {
        let rmc = match parse_rmc(line) {
            Ok(r) => r,
            Err(Reject::NotRmc) => return None,
            Err(e) => {
                match e {
                    Reject::Checksum => self.stats.bad_checksum = self.stats.bad_checksum.wrapping_add(1),
                    Reject::NoFix => self.stats.no_fix = self.stats.no_fix.wrapping_add(1),
                    _ => self.stats.malformed = self.stats.malformed.wrapping_add(1),
                }
                self.lose();
                return None;
            }
        };
        self.stats.valid = self.stats.valid.wrapping_add(1);
        // the edges that lie in the window before the end of the line
        let mut in_window = self.edges[3 - self.n_edges..].iter().copied().filter(|e| *e <= end_us && end_us - *e <= PAIR_WINDOW_US);
        let (first, second) = (in_window.next(), in_window.next());
        let edge_us = match (first, second) {
            (Some(e), None) => e,
            _ => {
                self.stats.unpaired = self.stats.unpaired.wrapping_add(1);
                self.lose();
                return None;
            }
        };
        let follows = self.prev.is_some_and(|(s, e)| {
            rmc.unix_s == s + 1 && edge_us > e && (edge_us - e).abs_diff(SECOND_US) <= EDGE_TOLERANCE_US
        });
        if follows {
            self.run = self.run.saturating_add(1);
        } else {
            if self.run > 0 {
                self.stats.breaks = self.stats.breaks.wrapping_add(1);
            }
            self.run = 1;
        }
        self.prev = Some((rmc.unix_s, edge_us));
        if self.run >= SETTLE && self.last_offer.is_none_or(|t| edge_us.saturating_sub(t) >= OFFER_EVERY_US) {
            self.last_offer = Some(edge_us);
            return Some(Fix { unix_s: rmc.unix_s, edge_us });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-20 12:00:00 UTC
    const NOON: i64 = 1_792_497_600;

    fn sentence(body: &str) -> heapless::String<120> {
        let sum = body.bytes().fold(0u8, |x, b| x ^ b);
        let mut s = heapless::String::new();
        core::fmt::write(&mut s, format_args!("${body}*{sum:02X}")).unwrap();
        s
    }

    fn rmc_at(unix_s: i64) -> heapless::String<120> {
        let mut s = heapless::String::new();
        build_rmc(unix_s, &mut s).unwrap();
        s
    }

    // VERIFIES: REQ-GPS-01
    #[test]
    fn a_valid_rmc_gives_the_second_it_names() {
        assert_eq!(parse_rmc(rmc_at(NOON).as_bytes()), Ok(Rmc { unix_s: NOON }));
        // the date across a month end, a leap day and the end of a year
        assert_eq!(parse_rmc(rmc_at(1_709_164_800).as_bytes()), Ok(Rmc { unix_s: 1_709_164_800 })); // 2024-02-29
        assert_eq!(parse_rmc(rmc_at(1_798_761_599).as_bytes()), Ok(Rmc { unix_s: 1_798_761_599 })); // 2026-12-31 23:59:59
    }

    // VERIFIES: REQ-GPS-01
    #[test]
    fn any_talker_is_accepted_and_other_sentences_are_not_rmc() {
        for talker in ["GP", "GN", "GL", "GA", "GB"] {
            let s = sentence(&format!("{talker}RMC,120000.00,A,4717.11437,N,00833.91522,E,0.004,77.52,201026,,,A,V"));
            assert_eq!(parse_rmc(s.as_bytes()), Ok(Rmc { unix_s: NOON }), "{talker}");
        }
        let gga = sentence("GNGGA,120000.00,4717.11437,N,00833.91522,E,1,12,0.9,496.4,M,48.0,M,,");
        assert_eq!(parse_rmc(gga.as_bytes()), Err(Reject::NotRmc));
        assert_eq!(parse_rmc(b"$PUBX,00,x*00"), Err(Reject::NotRmc));
    }

    // VERIFIES: REQ-GPS-01
    #[test]
    fn a_bad_checksum_or_a_damaged_sentence_is_refused() {
        let good = rmc_at(NOON);
        let mut bad = good.clone().into_bytes();
        let n = bad.len();
        bad[n - 1] = if bad[n - 1] == b'0' { b'1' } else { b'0' };
        assert_eq!(parse_rmc(&bad), Err(Reject::Checksum));
        let mut flipped = good.clone().into_bytes();
        flipped[10] ^= 1; // a data bit, the checksum stays
        assert_eq!(parse_rmc(&flipped), Err(Reject::Checksum));
        assert_eq!(parse_rmc(&good.as_bytes()[..good.len() - 3]), Err(Reject::Checksum), "no checksum");
        assert_eq!(parse_rmc(&good.as_bytes()[..good.len() - 1]), Err(Reject::Checksum), "one digit");
        assert_eq!(parse_rmc(b""), Err(Reject::Malformed));
    }

    // VERIFIES: REQ-GPS-01
    #[test]
    fn without_a_fix_there_is_no_time() {
        // what a receiver without a fix sends: empty time or date, status V
        let none = sentence("GNRMC,,V,,,,,,,,,,N,V");
        assert_eq!(parse_rmc(none.as_bytes()), Err(Reject::NoFix));
        let v = sentence("GNRMC,120000.00,V,,,,,,,201026,,,N,V");
        assert_eq!(parse_rmc(v.as_bytes()), Err(Reject::NoFix));
        // status A but a mode indicator that says no fix / estimated / simulated
        for mode in ["N", "E", "S"] {
            let s = sentence(&format!("GNRMC,120000.00,A,4717.11437,N,00833.91522,E,0.004,77.52,201026,,,{mode},V"));
            assert_eq!(parse_rmc(s.as_bytes()), Err(Reject::NoFix), "{mode}");
        }
        // differential mode and no mode field at all (NMEA before 2.3) are fine
        let d = sentence("GNRMC,120000.00,A,4717.11437,N,00833.91522,E,0.004,77.52,201026,,,D,V");
        assert_eq!(parse_rmc(d.as_bytes()), Ok(Rmc { unix_s: NOON }));
        let old = sentence("GPRMC,120000,A,4717.11437,N,00833.91522,E,0.004,77.52,201026,,");
        assert_eq!(parse_rmc(old.as_bytes()), Ok(Rmc { unix_s: NOON }));
    }

    // VERIFIES: REQ-GPS-01
    #[test]
    fn impossible_fields_are_refused() {
        let base = |time: &str, date: &str| sentence(&format!("GNRMC,{time},A,4717.11437,N,00833.91522,E,0.004,77.52,{date},,,A,V"));
        for (time, date) in [
            ("240000.00", "201026"),
            ("126000.00", "201026"),
            ("120060.00", "201026"), // a leap second is not taken
            ("120000.50", "201026"), // not a whole second
            ("1200.00", "201026"),
            ("12000a.00", "201026"),
            ("120000.00", "320126"),
            ("120000.00", "201326"),
            ("120000.00", "001026"),
            ("120000.00", "290225"), // 2025 is no leap year
            ("120000.00", "20102"),
            ("120000.00", "2010xx"),
        ] {
            assert_eq!(parse_rmc(base(time, date).as_bytes()), Err(Reject::Malformed), "{time} {date}");
        }
    }

    // VERIFIES: REQ-GPS-01
    #[test]
    fn lines_are_cut_from_the_byte_stream() {
        let mut lb = LineBuffer::new();
        let mut got = std::vec::Vec::new();
        let rmc = rmc_at(NOON);
        let mut stream = std::vec::Vec::new();
        stream.extend_from_slice(b"junk\xff\r\n");
        stream.extend_from_slice(b"$GNGGA,partial"); // cut off by the next $
        stream.extend_from_slice(rmc.as_bytes());
        stream.extend_from_slice(b"\r\n");
        stream.extend_from_slice(b"\n\n$GNVTG,x*00\r\n");
        for b in stream {
            if let Some(l) = lb.push(b) {
                got.push(l.to_vec());
                lb.done();
            }
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], rmc.as_bytes());
        assert_eq!(got[1], b"$GNVTG,x*00");
        // a line without an end is dropped when it gets too long, and counted
        let mut lb = LineBuffer::new();
        for b in core::iter::once(b'$').chain(core::iter::repeat_n(b'x', LINE_MAX + 5)) {
            assert!(lb.push(b).is_none());
        }
        assert_eq!(lb.overlong, 1);
    }

    /// Feeds `n` consecutive seconds starting at `NOON` into the tracker; second i has its edge at `t0 + i * 1 s + jitter(i)` and its
    /// sentence ends 300 ms later. Returns the fixes.
    fn run(t: &mut Tracker, from: i64, n: i64, t0: u64, jitter: impl Fn(i64) -> i64) -> std::vec::Vec<Fix> {
        let mut fixes = std::vec::Vec::new();
        for i in 0..n {
            let edge = (t0 as i64 + i * 1_000_000 + jitter(i)) as u64;
            t.edge(edge);
            if let Some(f) = t.line(rmc_at(from + i).as_bytes(), edge + 300_000) {
                fixes.push(f);
            }
        }
        fixes
    }

    // VERIFIES: REQ-GPS-02
    #[test]
    fn a_time_is_tied_to_the_edge_before_its_sentence() {
        let mut t = Tracker::new();
        run(&mut t, NOON, 1, 5_000_000, |_| 0);
        assert_eq!(t.stats.valid, 1);
        assert_eq!(t.stats.unpaired, 0);
        assert_eq!(t.run(), 1);
        // no edge at all: the sentence is counted and discarded
        let mut t = Tracker::new();
        assert_eq!(t.line(rmc_at(NOON).as_bytes(), 1_000_000), None);
        assert_eq!((t.stats.valid, t.stats.unpaired, t.run()), (1, 1, 0));
        // the edge is older than the window
        let mut t = Tracker::new();
        t.edge(1_000_000);
        t.line(rmc_at(NOON).as_bytes(), 1_000_000 + PAIR_WINDOW_US + 1);
        assert_eq!(t.stats.unpaired, 1);
        // exactly at the window's end it still pairs
        let mut t = Tracker::new();
        t.edge(1_000_000);
        t.line(rmc_at(NOON).as_bytes(), 1_000_000 + PAIR_WINDOW_US);
        assert_eq!((t.stats.unpaired, t.run()), (0, 1));
        // two edges in the window (a glitch on the line) are ambiguous
        let mut t = Tracker::new();
        t.edge(1_000_000);
        t.edge(1_400_000);
        t.line(rmc_at(NOON).as_bytes(), 1_700_000);
        assert_eq!((t.stats.unpaired, t.run()), (1, 0));
        // an edge that came after the end of the line does not belong to it
        let mut t = Tracker::new();
        t.edge(1_000_000);
        t.edge(2_000_000);
        t.line(rmc_at(NOON).as_bytes(), 1_300_000);
        assert_eq!((t.stats.unpaired, t.run()), (0, 1));
    }

    // VERIFIES: REQ-GPS-02
    #[test]
    fn a_fix_carries_the_instant_of_the_edge() {
        assert_eq!(Fix { unix_s: NOON, edge_us: 7_123_400 }.sample_ms(), (NOON * 1000, 7_123));
        assert_eq!(Fix { unix_s: NOON, edge_us: 7_123_500 }.sample_ms(), (NOON * 1000, 7_124));
        assert_eq!(Fix { unix_s: NOON, edge_us: 7_123_900 }.sample_ms(), (NOON * 1000, 7_124));
    }

    // VERIFIES: REQ-GPS-03
    #[test]
    fn thirty_consecutive_good_seconds_settle_the_receiver() {
        let mut t = Tracker::new();
        let fixes = run(&mut t, NOON, SETTLE as i64 - 1, 5_000_000, |_| 0);
        assert!(fixes.is_empty() && !t.settled());
        assert_eq!(t.run(), SETTLE - 1);
        let mut t = Tracker::new();
        let fixes = run(&mut t, NOON, SETTLE as i64, 5_000_000, |_| 0);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].unix_s, NOON + SETTLE as i64 - 1);
        assert!(t.settled());
        // edges a few milliseconds off are fine (jitter within 10 ms of a second)
        let mut t = Tracker::new();
        let fixes = run(&mut t, NOON, SETTLE as i64, 5_000_000, |i| if i % 2 == 0 { 4_000 } else { -4_000 });
        assert_eq!(fixes.len(), 1, "alternating +-4 ms jitter gives intervals of 992 / 1008 ms");
    }

    // VERIFIES: REQ-GPS-03
    #[test]
    fn anything_that_breaks_the_run_starts_the_count_again() {
        let settle = SETTLE as i64;
        // a skipped second
        let mut t = Tracker::new();
        run(&mut t, NOON, 20, 5_000_000, |_| 0);
        let fixes = run(&mut t, NOON + 21, settle - 1, 5_000_000 + 21_000_000, |_| 0);
        assert!(fixes.is_empty() && t.run() == settle as u32 - 1, "the skip restarts at 1, and settle-1 more seconds are one short");
        assert_eq!(t.stats.breaks, 1);
        // the same second twice
        let mut t = Tracker::new();
        run(&mut t, NOON, 10, 5_000_000, |_| 0);
        run(&mut t, NOON + 9, 1, 5_000_000 + 10_000_000, |_| 0);
        assert_eq!(t.run(), 1);
        // a PPS edge 20 ms late
        let mut t = Tracker::new();
        let fixes = run(&mut t, NOON, settle, 5_000_000, |i| if i == 10 { 20_000 } else { 0 });
        assert!(fixes.is_empty() && !t.settled(), "the late edge makes two intervals off by 20 ms");
        // a sentence without a fix, a bad checksum and silence each reset
        for kind in 0..3 {
            let mut t = Tracker::new();
            run(&mut t, NOON, 20, 5_000_000, |_| 0);
            assert_eq!(t.run(), 20);
            match kind {
                0 => {
                    t.edge(26_000_000);
                    t.line(sentence("GNRMC,,V,,,,,,,,,,N,V").as_bytes(), 26_300_000);
                }
                1 => {
                    let mut s = rmc_at(NOON + 20).into_bytes();
                    let n = s.len();
                    s[n - 1] ^= 1;
                    t.line(&s, 26_300_000);
                }
                _ => t.silent(),
            }
            assert_eq!(t.run(), 0, "kind {kind}");
        }
    }

    // VERIFIES: REQ-GPS-04
    #[test]
    fn a_settled_receiver_offers_once_a_minute() {
        let mut t = Tracker::new();
        let fixes = run(&mut t, NOON, 200, 5_000_000, |_| 0);
        // settled at second 30 (index 29), then 60 s later at index 89, and 149
        let at: std::vec::Vec<i64> = fixes.iter().map(|f| f.unix_s - NOON).collect();
        assert_eq!(at, [29, 89, 149]);
        // a lost fix long enough that the next settle is more than a minute after the last offer offers at once
        let mut t = Tracker::new();
        run(&mut t, NOON, 40, 5_000_000, |_| 0);
        t.silent();
        let fixes = run(&mut t, NOON + 200, 30, 5_000_000 + 200_000_000, |_| 0);
        assert_eq!(fixes.len(), 1);
        // a short loss is not offered again before the minute is over
        let mut t = Tracker::new();
        run(&mut t, NOON, 30, 5_000_000, |_| 0);
        t.silent();
        let fixes = run(&mut t, NOON + 31, 30, 5_000_000 + 31_000_000, |_| 0);
        assert!(fixes.is_empty());
    }

    // VERIFIES: REQ-GPS-06
    #[test]
    fn the_counters_tell_the_faults_apart() {
        let mut t = Tracker::new();
        t.edge(1_000_000);
        t.line(b"$GNRMC,120000.00,A,4717.11437,N,00833.91522,E,0.004,77.52,201026,,,A,V*00", 1_300_000); // bad checksum
        t.line(sentence("GNRMC,,V,,,,,,,,,,N,V").as_bytes(), 1_300_000);
        t.line(sentence("GNRMC,126000.00,A,4717.11437,N,00833.91522,E,0.004,77.52,201026,,,A,V").as_bytes(), 1_300_000);
        t.line(rmc_at(NOON).as_bytes(), 3_300_000); // the only edge is far too old
        t.line(sentence("GNGGA,120000.00,,,,,0,00,99.99,,,,,,").as_bytes(), 1_300_000); // not counted at all
        assert_eq!(
            t.stats,
            Stats { valid: 1, bad_checksum: 1, no_fix: 1, malformed: 1, edges: 1, unpaired: 1, breaks: 0 },
        );
        assert_eq!(t.since_edge(4_000_000), Some(3_000_000));
        assert_eq!(Tracker::new().since_edge(4_000_000), None);
    }

    // VERIFIES: REQ-GPS-06
    #[test]
    fn the_status_line_tells_a_silent_line_a_missing_pps_and_a_settling_receiver_apart() {
        let text = |r: &Report| {
            let mut s = heapless::String::<400>::new();
            describe(&mut s, r).unwrap();
            s
        };
        let silent = text(&Report::default());
        assert!(silent.starts_with("receiver silent") && silent.contains("gps_baud") && silent.contains("PPS edges 0"), "{silent}");
        let no_pps = text(&Report { bytes: 5000, ..Default::default() });
        assert!(no_pps.starts_with("settling 0/30") && no_pps.contains("none seen: check the PPS wire"), "{no_pps}");
        let settling = text(&Report { bytes: 5000, run: 12, since_edge_ms: Some(2_345), stats: Stats { valid: 12, edges: 14, ..Default::default() }, ..Default::default() });
        assert!(settling.starts_with("settling 12/30") && settling.contains("valid 12") && settling.contains("PPS edges 14 (last 2.3 s ago)"), "{settling}");
        let settled = text(&Report { bytes: 9000, run: 31, since_edge_ms: Some(40), ..Default::default() });
        assert!(settled.starts_with("settled;") && settled.contains("last 0.0 s ago"), "{settled}");
        assert!(text(&Report { bytes: 9, silent: true, ..Default::default() }).starts_with("receiver went silent"));
        // the worst case fits in the line the terminal reserves for it
        let worst = text(&Report { bytes: u32::MAX, run: u32::MAX, since_edge_ms: Some(u32::MAX), uart_errors: u32::MAX, stats: Stats { valid: u32::MAX, bad_checksum: u32::MAX, no_fix: u32::MAX, malformed: u32::MAX, edges: u32::MAX, unpaired: u32::MAX, breaks: u32::MAX }, silent: false });
        assert!(worst.len() < 300, "{}", worst.len());
    }

    // VERIFIES: REQ-GPS-01
    #[test]
    fn the_sentence_everyone_quotes_parses_with_its_published_checksum() {
        // the example of the NMEA 0183 description that is printed everywhere, with the checksum it is printed with (6A); it has no mode
        // field (NMEA before 2.3), and its two-digit year 94 is read as 2094
        let published = b"$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";
        let expected = crate::time::datetime_to_unix(&crate::time::DateTime { year: 2094, month: 3, day: 23, hour: 12, minute: 35, second: 19 });
        assert_eq!(parse_rmc(published), Ok(Rmc { unix_s: expected }));
        // lower-case hex digits are fine, a wrong one is not
        assert_eq!(parse_rmc(b"$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6a"), Ok(Rmc { unix_s: expected }));
        assert_eq!(parse_rmc(b"$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6B"), Err(Reject::Checksum));
    }
}
