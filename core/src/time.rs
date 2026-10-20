//! Calendar maths, UTC offsets and DST rules. No allocation, no `std`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    /// ISO weekday: 1 = Monday ... 7 = Sunday.
    pub fn weekday(&self) -> u8 {
        weekday_of_days(days_from_civil(self.year as i64, self.month, self.day))
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
/// The RTC must be written exactly on a UTC second boundary. Time was `secs` + `ms` when the NTP reply arrived and
/// `elapsed_ms` have passed since; returns the second to write (the next whole one) and how long to wait until it begins.
/// The clock accepts times from 2020-01-01 up to the end of 2099 (the RTC keeps two year digits).
pub const MIN_UNIX: i64 = 1_577_836_800;
pub const MAX_UNIX: i64 = 4_102_444_799;

pub fn plausible_unix(secs: i64) -> bool {
    (MIN_UNIX..=MAX_UNIX).contains(&secs)
}

pub fn next_second_boundary(secs: u64, ms: u16, elapsed_ms: u64) -> (i64, u64) {
    let total = ms as u64 + elapsed_ms;
    (secs as i64 + (total / 1000) as i64 + 1, 1000 - total % 1000)
}

pub fn days_from_civil(y: i64, m: u8, d: u8) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u8, u8) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// ISO weekday (1 = Monday ... 7 = Sunday) of a day number. Day 0 (1970-01-01) was a Thursday.
pub fn weekday_of_days(days: i64) -> u8 {
    ((days + 3).rem_euclid(7) + 1) as u8
}

pub fn days_in_month(y: i64, m: u8) -> u8 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

pub fn unix_to_datetime(secs: i64) -> DateTime {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    DateTime {
        year: y as u16,
        month: m,
        day: d,
        hour: (rem / 3600) as u8,
        minute: (rem % 3600 / 60) as u8,
        second: (rem % 60) as u8,
    }
}

pub fn datetime_to_unix(dt: &DateTime) -> i64 {
    days_from_civil(dt.year as i64, dt.month, dt.day) * 86_400
        + dt.hour as i64 * 3600
        + dt.minute as i64 * 60
        + dt.second as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: u16, mo: u8, d: u8, h: u8, mi: u8, s: u8) -> DateTime {
        DateTime { year: y, month: mo, day: d, hour: h, minute: mi, second: s }
    }

    // VERIFIES: REQ-TIME-03
    #[test]
    fn unix_roundtrip_and_known_values() {
        assert_eq!(unix_to_datetime(0), dt(1970, 1, 1, 0, 0, 0));
        assert_eq!(datetime_to_unix(&dt(2000, 2, 29, 12, 34, 56)), 951_827_696);
        assert_eq!(unix_to_datetime(1_700_000_000), dt(2023, 11, 14, 22, 13, 20));
        for t in [0i64, 86_399, 951_782_400, 1_709_251_199, 4_102_444_800] {
            assert_eq!(datetime_to_unix(&unix_to_datetime(t)), t);
        }
    }

    // VERIFIES: REQ-TIME-03
    #[test]
    fn weekdays() {
        assert_eq!(dt(1970, 1, 1, 0, 0, 0).weekday(), 4); // Thursday
        assert_eq!(dt(2023, 11, 14, 0, 0, 0).weekday(), 2); // Tuesday
        assert_eq!(dt(2024, 3, 31, 0, 0, 0).weekday(), 7); // Sunday
    }

    // VERIFIES: REQ-TIME-04
    #[test]
    fn next_boundary_is_always_the_next_whole_second() {
        assert_eq!(next_second_boundary(100, 0, 0), (101, 1000));
        assert_eq!(next_second_boundary(100, 250, 0), (101, 750));
        assert_eq!(next_second_boundary(100, 999, 1), (102, 1000), "exactly on a boundary waits a full second");
        assert_eq!(next_second_boundary(100, 900, 350), (102, 750), "processing delay can cross a second");
        for secs in [0u64, 1_720_000_000] {
            for ms in 0..1000u16 {
                for elapsed in [0u64, 1, 37, 250, 999, 1000, 2500] {
                    let (target, wait) = next_second_boundary(secs, ms, elapsed);
                    assert!((1..=1000).contains(&wait));
                    // the wait ends exactly on `target` seconds
                    assert_eq!((target as u64 - secs) * 1000, ms as u64 + elapsed + wait);
                }
            }
        }
    }
}
