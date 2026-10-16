//! POSIX TZ strings (e.g. `CET-1CEST,M3.5.0,M10.5.0/3`) and the IANA-name lookup table.
//!
//! The device keeps UTC; the offset to local time comes from [`Tz::offset_at`].

use crate::time::{civil_from_days, days_from_civil, days_in_month, weekday_of_days};
use crate::tzdb::{RULES, ZONES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TzError {
    Syntax,
    Range,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Day {
    /// Month (1-12), week (1-5, 5 = last), weekday (0 = Sunday).
    Mwd(u8, u8, u8),
    /// 1..=365, February 29th is never counted.
    Julian1(u16),
    /// 0..=365, leap days are counted.
    Julian0(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Transition {
    day: Day,
    /// Seconds after local midnight; may be negative or exceed 24 h.
    time: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Dst {
    /// Seconds east of UTC while DST is in effect.
    offset: i32,
    start: Transition,
    end: Transition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tz {
    /// Seconds east of UTC in standard time.
    std_offset: i32,
    dst: Option<Dst>,
}

impl Tz {
    pub const UTC: Tz = Tz { std_offset: 0, dst: None };

    /// Accepts a POSIX TZ rule or an IANA zone name from the built-in table (case-insensitive).
    pub fn from_name_or_rule(s: &str) -> Result<Tz, TzError> {
        let s = s.trim();
        match lookup(s) {
            Some(rule) => Tz::parse(rule),
            None => Tz::parse(s),
        }
    }

    pub fn parse(s: &str) -> Result<Tz, TzError> {
        let mut p = Parser { s: s.as_bytes(), i: 0 };
        p.name()?;
        let std_offset = -p.offset()?;
        if p.eof() {
            return Ok(Tz { std_offset, dst: None });
        }
        p.name()?;
        let dst_offset = if p.eof() || p.peek() == Some(b',') { std_offset + 3600 } else { -p.offset()? };
        // Rules are mandatory in practice; tzdata always emits them. Without them POSIX says "implementation
        // defined", which we reject rather than guess.
        if p.next() != Some(b',') {
            return Err(TzError::Syntax);
        }
        let start = p.transition()?;
        if p.next() != Some(b',') {
            return Err(TzError::Syntax);
        }
        let end = p.transition()?;
        if !p.eof() {
            return Err(TzError::Syntax);
        }
        Ok(Tz { std_offset, dst: Some(Dst { offset: dst_offset, start, end }) })
    }

    /// Seconds to add to UTC to get local time at the given UTC instant.
    pub fn offset_at(&self, utc: i64) -> i32 {
        let Some(d) = self.dst else { return self.std_offset };
        let year = civil_from_days((utc + self.std_offset as i64).div_euclid(86_400)).0;
        let start = transition_utc(year, &d.start, self.std_offset);
        let end = transition_utc(year, &d.end, d.offset);
        let in_dst = if start < end { utc >= start && utc < end } else { !(utc >= end && utc < start) };
        if in_dst { d.offset } else { self.std_offset }
    }

    pub fn has_dst(&self) -> bool {
        self.dst.is_some()
    }
}

/// UTC instant of a transition in `year`, given the offset in force just before it.
fn transition_utc(year: i64, t: &Transition, offset_before: i32) -> i64 {
    let day = match t.day {
        Day::Mwd(m, w, d) => {
            let first = weekday_of_days(days_from_civil(year, m, 1)) % 7; // 0 = Sunday
            let mut dom = 1 + (d + 7 - first) % 7 + 7 * (w - 1);
            while dom > days_in_month(year, m) {
                dom -= 7;
            }
            days_from_civil(year, m, dom)
        }
        Day::Julian1(n) => {
            let leap = days_in_month(year, 2) == 29;
            let n0 = n as i64 - 1;
            days_from_civil(year, 1, 1) + n0 + if leap && n >= 60 { 1 } else { 0 }
        }
        Day::Julian0(n) => days_from_civil(year, 1, 1) + n as i64,
    };
    day * 86_400 + t.time as i64 - offset_before as i64
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn next(&mut self) -> Option<u8> {
        let c = self.peek();
        self.i += c.is_some() as usize;
        c
    }

    /// Zone abbreviation: 3+ letters, or anything between `<` and `>`.
    fn name(&mut self) -> Result<(), TzError> {
        if self.peek() == Some(b'<') {
            self.i += 1;
            let start = self.i;
            while self.peek().is_some_and(|c| c != b'>') {
                self.i += 1;
            }
            if self.next() != Some(b'>') || self.i - start < 4 {
                return Err(TzError::Syntax);
            }
            return Ok(());
        }
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        if self.i - start < 3 { Err(TzError::Syntax) } else { Ok(()) }
    }

    fn number(&mut self, max_digits: usize) -> Result<i32, TzError> {
        let start = self.i;
        let mut v = 0i32;
        while self.i - start < max_digits && self.peek().is_some_and(|c| c.is_ascii_digit()) {
            v = v * 10 + (self.s[self.i] - b'0') as i32;
            self.i += 1;
        }
        if self.i == start { Err(TzError::Syntax) } else { Ok(v) }
    }

    /// `[+-]hh[:mm[:ss]]` in seconds (hours up to 167, as the RFC 9636 extension allows).
    fn offset(&mut self) -> Result<i32, TzError> {
        let sign = match self.peek() {
            Some(b'-') => {
                self.i += 1;
                -1
            }
            Some(b'+') => {
                self.i += 1;
                1
            }
            _ => 1,
        };
        let h = self.number(3)?;
        let (mut m, mut s) = (0, 0);
        if self.peek() == Some(b':') {
            self.i += 1;
            m = self.number(2)?;
            if self.peek() == Some(b':') {
                self.i += 1;
                s = self.number(2)?;
            }
        }
        if h > 167 || m > 59 || s > 59 {
            return Err(TzError::Range);
        }
        Ok(sign * (h * 3600 + m * 60 + s))
    }

    fn transition(&mut self) -> Result<Transition, TzError> {
        let day = match self.peek() {
            Some(b'M') => {
                self.i += 1;
                let m = self.number(2)?;
                if self.next() != Some(b'.') {
                    return Err(TzError::Syntax);
                }
                let w = self.number(1)?;
                if self.next() != Some(b'.') {
                    return Err(TzError::Syntax);
                }
                let d = self.number(1)?;
                if !(1..=12).contains(&m) || !(1..=5).contains(&w) || d > 6 {
                    return Err(TzError::Range);
                }
                Day::Mwd(m as u8, w as u8, d as u8)
            }
            Some(b'J') => {
                self.i += 1;
                let n = self.number(3)?;
                if !(1..=365).contains(&n) {
                    return Err(TzError::Range);
                }
                Day::Julian1(n as u16)
            }
            _ => {
                let n = self.number(3)?;
                if n > 365 {
                    return Err(TzError::Range);
                }
                Day::Julian0(n as u16)
            }
        };
        let time = if self.peek() == Some(b'/') {
            self.i += 1;
            self.offset()?
        } else {
            2 * 3600
        };
        Ok(Transition { day, time })
    }
}

/// POSIX rule for an IANA zone name from the built-in table, ASCII case-insensitive.
pub fn lookup(name: &str) -> Option<&'static str> {
    let key = name.as_bytes();
    let cmp = |zone: &str| {
        zone.bytes().map(|b| b.to_ascii_lowercase()).cmp(key.iter().map(|b| b.to_ascii_lowercase()))
    };
    ZONES.binary_search_by(|(z, _)| cmp(z)).ok().map(|i| RULES[ZONES[i].1 as usize])
}

/// Number of zones in the built-in table.
pub fn zone_count() -> usize {
    ZONES.len()
}

/// Iterates zone names starting with `prefix` (ASCII case-insensitive), for terminal/web listing.
pub fn zones_with_prefix(prefix: &str) -> impl Iterator<Item = &'static str> + '_ {
    ZONES.iter().map(|(n, _)| *n).filter(move |n| {
        n.len() >= prefix.len() && n.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{DateTime, datetime_to_unix};

    fn utc(y: u16, mo: u8, d: u8, h: u8, mi: u8) -> i64 {
        datetime_to_unix(&DateTime { year: y, month: mo, day: d, hour: h, minute: mi, second: 0 })
    }
    fn off(name: &str, t: i64) -> i32 {
        Tz::from_name_or_rule(name).unwrap().offset_at(t)
    }

    #[test]
    fn fixed_offsets() {
        assert_eq!(Tz::parse("UTC0").unwrap().offset_at(0), 0);
        assert_eq!(Tz::parse("EST5").unwrap().offset_at(0), -5 * 3600);
        assert_eq!(Tz::parse("<+0530>-5:30").unwrap().offset_at(0), 5 * 3600 + 1800);
        assert_eq!(off("Asia/Kolkata", 0), 19_800);
        assert_eq!(off("utc", 0), 0);
    }

    #[test]
    fn eu_transitions_berlin() {
        let z = "Europe/Berlin";
        assert_eq!(off(z, utc(2024, 3, 31, 0, 59)), 3600);
        assert_eq!(off(z, utc(2024, 3, 31, 1, 0)), 7200);
        assert_eq!(off(z, utc(2024, 10, 27, 0, 59)), 7200);
        assert_eq!(off(z, utc(2024, 10, 27, 1, 0)), 3600);
        assert_eq!(off(z, utc(2024, 7, 1, 12, 0)), 7200);
        assert_eq!(off(z, utc(2024, 1, 1, 12, 0)), 3600);
    }

    #[test]
    fn us_transitions_new_york() {
        let z = "America/New_York";
        assert_eq!(off(z, utc(2024, 3, 10, 6, 59)), -5 * 3600);
        assert_eq!(off(z, utc(2024, 3, 10, 7, 0)), -4 * 3600);
        assert_eq!(off(z, utc(2024, 11, 3, 5, 59)), -4 * 3600);
        assert_eq!(off(z, utc(2024, 11, 3, 6, 0)), -5 * 3600);
    }

    #[test]
    fn southern_hemisphere_sydney() {
        let z = "Australia/Sydney";
        assert_eq!(off(z, utc(2024, 1, 15, 0, 0)), 11 * 3600);
        assert_eq!(off(z, utc(2024, 7, 15, 0, 0)), 10 * 3600);
        assert_eq!(off(z, utc(2024, 12, 31, 23, 59)), 11 * 3600);
        // DST ends first Sunday in April at 03:00 local DST = 16:00 UTC the day before.
        assert_eq!(off(z, utc(2024, 4, 6, 15, 59)), 11 * 3600);
        assert_eq!(off(z, utc(2024, 4, 6, 16, 0)), 10 * 3600);
    }

    #[test]
    fn half_hour_dst_lord_howe() {
        assert_eq!(off("Australia/Lord_Howe", utc(2024, 1, 15, 0, 0)), 11 * 3600);
        assert_eq!(off("Australia/Lord_Howe", utc(2024, 7, 15, 0, 0)), 10 * 3600 + 1800);
    }

    #[test]
    fn default_dst_offset_and_julian_days() {
        // DST offset defaults to std + 1 h.
        let z = Tz::parse("AAA3BBB,J60,J300").unwrap();
        assert_eq!(z.offset_at(utc(2023, 6, 1, 0, 0)), -2 * 3600);
        // J60 is March 1st in both leap and non-leap years (Feb 29 is never counted).
        assert_eq!(z.offset_at(utc(2024, 2, 29, 12, 0)), -3 * 3600);
        assert_eq!(z.offset_at(utc(2024, 3, 2, 12, 0)), -2 * 3600);
        // Zero-based form counts leap days: n = 59 is Feb 29 in 2024.
        let z = Tz::parse("AAA3BBB,59,300").unwrap();
        assert_eq!(z.offset_at(utc(2024, 2, 29, 12, 0)), -2 * 3600);
        assert_eq!(z.offset_at(utc(2024, 2, 28, 12, 0)), -3 * 3600);
    }

    #[test]
    fn negative_transition_time_greenland() {
        // tzdata uses a negative time for America/Nuuk: DST starts 01:00 UTC... verify it parses and flips once.
        let z = Tz::from_name_or_rule("America/Nuuk").unwrap();
        let a = z.offset_at(utc(2025, 1, 15, 12, 0));
        let b = z.offset_at(utc(2025, 7, 15, 12, 0));
        assert_eq!(b - a, 3600);
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "X", "CET", "CET-1CEST", "CET-1CEST,M13.1.0,M10.5.0", "CET-1CEST,M3.5.0", "CET-99999", "CET-1CEST,M3.5.0,M10.5.0,x", "<ab>1"] {
            assert!(Tz::parse(bad).is_err(), "{bad:?} should fail");
        }
        assert!(Tz::from_name_or_rule("Mars/Olympus_Mons").is_err());
    }

    #[test]
    fn lookup_is_case_insensitive_and_complete() {
        assert_eq!(lookup("europe/berlin"), Some("CET-1CEST,M3.5.0,M10.5.0/3"));
        assert_eq!(lookup("Europe/Nowhere"), None);
        // Every table entry must resolve and parse (catches table/parser drift).
        for (name, _) in crate::tzdb::ZONES.iter() {
            let rule = lookup(name).unwrap_or_else(|| panic!("lookup {name}"));
            Tz::parse(rule).unwrap_or_else(|e| panic!("{name}: {rule}: {e:?}"));
        }
        assert!(zone_count() > 400);
    }

    #[test]
    fn every_zone_has_sane_offsets() {
        for (name, _) in crate::tzdb::ZONES.iter() {
            let z = Tz::from_name_or_rule(name).unwrap();
            for t in [utc(2025, 1, 1, 0, 0), utc(2025, 7, 1, 0, 0)] {
                let o = z.offset_at(t);
                assert!((-12 * 3600..=14 * 3600).contains(&o), "{name}: {o}");
            }
        }
    }

    #[test]
    fn prefix_listing() {
        assert!(zones_with_prefix("europe/b").any(|z| z == "Europe/Berlin"));
        assert!(zones_with_prefix("europe/b").all(|z| z.starts_with("Europe/B")));
    }
}
