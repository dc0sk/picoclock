//! DCF77 decoder (77.5 kHz time signal from Mainflingen). The receiver module outputs a pulse at the start of every second,
//! 100 ms long for a 0 and 200 ms for a 1; the pulse of second 59 is left out, so the next pulse starts a minute. The 59 bits
//! of one minute carry the time of the **next** minute in local German time (CET or CEST) with three parity bits.
//!
//! The firmware measures the rising edge and the width of every pulse (`Decoder::pulse`); everything else, the pulse
//! classification, the frame, the parity checks, the calendar checks and the conversion to UTC, is here so that it can be
//! tested without a receiver. A time is only reported once two frames in a row decoded and are exactly one minute apart.

use crate::time::{DateTime, datetime_to_unix, days_from_civil, days_in_month, weekday_of_days};

/// Pulses shorter than this are interference spikes and are ignored altogether.
const SPIKE_US: u32 = 50_000;
const ZERO_US: core::ops::RangeInclusive<u32> = 70_000..=140_000;
const ONE_US: core::ops::RangeInclusive<u32> = 170_000..=260_000;
/// Distance between two pulse starts: a second, or two seconds around the missing pulse of second 59.
const SECOND_US: core::ops::RangeInclusive<u64> = 900_000..=1_100_000;
const MINUTE_GAP_US: core::ops::RangeInclusive<u64> = 1_900_000..=2_100_000;

/// A time fix: `unix_utc` is the UTC time (whole seconds) at which the pulse that starts the minute began, `rise_us` is when
/// that pulse was seen on the caller's microsecond clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fix {
    pub unix_utc: i64,
    pub rise_us: u64,
}

/// Counters for `status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Pulses of a valid width (a healthy receiver in good reception gives about 60 a minute).
    pub pulses: u32,
    /// Frames that decoded (parity and calendar fine), whether or not confirmed.
    pub frames_ok: u32,
    /// Complete frames refused (parity, impossible date) and frames that broke off (bad pulse, wrong timing, missing bits).
    pub frames_bad: u32,
    /// Pulses of an impossible width.
    pub pulses_bad: u32,
}

/// Longest receiver delay that can be set or learned (ms).
pub const MAX_DELAY_MS: u16 = 500;

/// Learns the receiver delay: how late the demodulated pulse edge is against true time (filters in the receiver, the signal's
/// travel time, the wake-up of the firmware). Each reading is the delay that would have made a DCF77 fix agree with a fresh
/// NTP time; the answer is the median of the last few, so one bad NTP reading or a noisy edge does not move it.
#[derive(Debug, Default)]
pub struct DelayEstimator {
    readings: [i32; 9],
    n: usize,
    next: usize,
}

impl DelayEstimator {
    /// Fewest readings before an answer is given: a DCF77 fix comes every minute, and an NTP time is only a reference for a
    /// few minutes after it arrived.
    pub const MIN_READINGS: usize = 4;

    pub const fn new() -> Self {
        Self { readings: [0; 9], n: 0, next: 0 }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Adds a reading (ms) and returns the delay learned so far, clamped to 0 to [`MAX_DELAY_MS`]. A reading that is
    /// implausible (more than 1 s from zero) is dropped.
    pub fn push(&mut self, reading_ms: i32) -> Option<u16> {
        if reading_ms.abs() > 1000 {
            return None;
        }
        self.readings[self.next] = reading_ms;
        self.next = (self.next + 1) % self.readings.len();
        self.n = (self.n + 1).min(self.readings.len());
        if self.n < Self::MIN_READINGS {
            return None;
        }
        let mut v = self.readings;
        let v = &mut v[..self.n];
        v.sort_unstable();
        // median; the mean of the two middle readings when there is an even number of them
        let mid = v.len() / 2;
        let median = if v.len() % 2 == 0 { (v[mid - 1] + v[mid]) / 2 } else { v[mid] };
        Some(median.clamp(0, MAX_DELAY_MS as i32) as u16)
    }
}

/// The monotonic time (ms) a pulse that began at `rise_us` really began, after taking the receiver delay off.
pub fn corrected_ms(rise_us: u64, delay_ms: u16) -> u64 {
    (rise_us / 1000).saturating_sub(delay_ms as u64)
}

#[derive(Debug)]
pub struct Decoder {
    bits: [u8; 60],
    n: usize,
    /// The pulse starts of the frame so far were all one second apart and all widths valid.
    clean: bool,
    last_rise: Option<u64>,
    /// The last decoded frame: the minute it announced and when that minute began on the caller's clock.
    previous: Option<Fix>,
    pub stats: Stats,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub const fn new() -> Self {
        Self { bits: [0; 60], n: 0, clean: false, last_rise: None, previous: None, stats: Stats { pulses: 0, frames_ok: 0, frames_bad: 0, pulses_bad: 0 } }
    }

    /// Forgets everything (the receiver was switched off or lost power).
    pub fn reset(&mut self) {
        let stats = self.stats;
        *self = Self::new();
        self.stats = stats;
    }

    /// One pulse: `rise_us` is when it began, `width_us` how long the output stayed high. Returns a fix when a second frame
    /// in a row has confirmed the first one.
    pub fn pulse(&mut self, rise_us: u64, width_us: u32) -> Option<Fix> {
        if width_us < SPIKE_US {
            return None;
        }
        let bit = if ZERO_US.contains(&width_us) {
            Some(0)
        } else if ONE_US.contains(&width_us) {
            Some(1)
        } else {
            None
        };
        self.stats.pulses += bit.is_some() as u32;
        let gap = self.last_rise.map(|l| rise_us.wrapping_sub(l));
        self.last_rise = Some(rise_us);
        let Some(bit) = bit else {
            self.stats.pulses_bad += 1;
            self.abandon_frame();
            return None;
        };
        match gap {
            Some(g) if SECOND_US.contains(&g) => {
                if self.clean && self.n < 59 {
                    self.bits[self.n] = bit;
                    self.n += 1;
                } else if self.clean {
                    self.abandon_frame(); // more than 59 pulses without a gap
                }
                None
            }
            Some(g) if MINUTE_GAP_US.contains(&g) => self.minute_mark(rise_us, bit),
            _ => {
                // the first pulse, or the timing is off: wait for the next minute mark
                self.abandon_frame();
                None
            }
        }
    }

    fn abandon_frame(&mut self) {
        if self.n > 0 {
            self.stats.frames_bad += 1;
        }
        self.n = 0;
        self.clean = false;
        self.previous = None;
    }

    /// The pulse after the missing one: the frame so far is complete if it holds 59 bits.
    fn minute_mark(&mut self, rise_us: u64, bit: u8) -> Option<Fix> {
        let complete = self.clean && self.n == 59;
        let decoded = if complete { decode(&self.bits[..59]) } else { None };
        // this pulse is bit 0 of the next frame, which must be a 0
        self.bits[0] = bit;
        self.n = 1;
        self.clean = bit == 0;
        let Some(unix_utc) = decoded else {
            if complete || self.previous.is_some() {
                self.stats.frames_bad += 1;
            }
            self.previous = None;
            return None;
        };
        self.stats.frames_ok += 1;
        let fix = Fix { unix_utc, rise_us };
        let confirmed = matches!(self.previous, Some(p) if p.unix_utc + 60 == fix.unix_utc);
        self.previous = Some(fix);
        confirmed.then_some(fix)
    }
}

fn bcd(bits: &[u8], weights: &[u8]) -> Option<u8> {
    Some(bits.iter().zip(weights).filter(|(b, _)| **b == 1).map(|(_, w)| w).sum())
}

fn parity_even(bits: &[u8]) -> bool {
    bits.iter().map(|&b| b as u32).sum::<u32>() % 2 == 0
}

/// The UTC time of the minute announced by a complete frame of 59 bits, or `None` if any check fails.
pub fn decode(b: &[u8]) -> Option<i64> {
    if b.len() != 59 || b[0] != 0 || b[20] != 1 {
        return None;
    }
    // Z1 (17) set = CEST (UTC+2), Z2 (18) set = CET (UTC+1); both equal is a fault
    let offset_h = match (b[17], b[18]) {
        (1, 0) => 2,
        (0, 1) => 1,
        _ => return None,
    };
    if !parity_even(&b[21..29]) || !parity_even(&b[29..36]) || !parity_even(&b[36..59]) {
        return None;
    }
    let minute = bcd(&b[21..28], &[1, 2, 4, 8, 10, 20, 40])?;
    let hour = bcd(&b[29..35], &[1, 2, 4, 8, 10, 20])?;
    let day = bcd(&b[36..42], &[1, 2, 4, 8, 10, 20])?;
    let weekday = bcd(&b[42..45], &[1, 2, 4])?;
    let month = bcd(&b[45..50], &[1, 2, 4, 8, 10])?;
    let year = 2000 + bcd(&b[50..58], &[1, 2, 4, 8, 10, 20, 40, 80])? as u16;
    // each BCD digit (the low four weights of a field) must be a decimal digit: 10 to 15 are not
    let nibble = |bits: &[u8]| bits.iter().enumerate().map(|(i, &x)| (x as u32) << i).sum::<u32>();
    let digits_ok = [&b[21..25], &b[29..33], &b[36..40], &b[45..49], &b[50..54], &b[54..58]].iter().all(|d| nibble(d) <= 9);
    if !digits_ok || minute > 59 || hour > 23 || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year as i64, month) {
        return None;
    }
    // the transmitted weekday must agree with the date: another check on the whole date
    if weekday != weekday_of_days(days_from_civil(year as i64, month, day)) {
        return None;
    }
    let local = datetime_to_unix(&DateTime { year, month, day, hour, minute, second: 0 });
    Some(local - offset_h * 3600)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::time::unix_to_datetime;

    pub struct Fields {
        pub year: u32,
        pub month: u32,
        pub day: u32,
        pub weekday: u32,
        pub hour: u32,
        pub minute: u32,
        pub cest: bool,
    }

    /// The 59 bits for the given fields, with correct parity (so that only the calendar checks can object).
    pub fn frame_of(f: &Fields) -> [u8; 59] {
        let mut b = [0u8; 59];
        b[17] = f.cest as u8;
        b[18] = !f.cest as u8;
        b[20] = 1;
        // BCD: the weights below 10 carry the units digit, the others the tens digit
        let put = |b: &mut [u8; 59], at: usize, v: u32, weights: &[u32]| {
            for (i, w) in weights.iter().enumerate() {
                let (digit, wt) = if *w >= 10 { (v / 10, w / 10) } else { (v % 10, *w) };
                b[at + i] = ((digit & wt) != 0) as u8;
            }
        };
        put(&mut b, 21, f.minute, &[1, 2, 4, 8, 10, 20, 40]);
        put(&mut b, 29, f.hour, &[1, 2, 4, 8, 10, 20]);
        put(&mut b, 36, f.day, &[1, 2, 4, 8, 10, 20]);
        put(&mut b, 42, f.weekday, &[1, 2, 4]);
        put(&mut b, 45, f.month, &[1, 2, 4, 8, 10]);
        put(&mut b, 50, f.year, &[1, 2, 4, 8, 10, 20, 40, 80]);
        let par = |s: &[u8]| s.iter().map(|&x| x as u32).sum::<u32>() as u8 % 2;
        b[28] = par(&b[21..28]);
        b[35] = par(&b[29..35]);
        b[58] = par(&b[36..58]);
        b
    }

    /// The frame announcing the minute that begins at `unix_utc` (a whole UTC minute).
    pub fn frame_for(unix_utc: i64, cest: bool) -> [u8; 59] {
        let dt = unix_to_datetime(unix_utc + if cest { 7200 } else { 3600 });
        frame_of(&Fields { year: (dt.year - 2000) as u32, month: dt.month as u32, day: dt.day as u32, weekday: dt.weekday() as u32, hour: dt.hour as u32, minute: dt.minute as u32, cest })
    }

    /// Feeds whole minutes back to back: frame `k` is sent from the minute mark `first_mark_us + k * 60 s`; at the end the
    /// next mark's first pulse completes the last frame. Returns every fix reported.
    pub fn feed(d: &mut Decoder, first_mark_us: u64, frames: &[[u8; 59]]) -> std::vec::Vec<Fix> {
        let mut out = std::vec::Vec::new();
        let width = |bit: u8| if bit == 1 { 200_000 } else { 100_000 };
        for (k, f) in frames.iter().enumerate() {
            for (i, &bit) in f.iter().enumerate() {
                out.extend(d.pulse(first_mark_us + k as u64 * 60_000_000 + i as u64 * 1_000_000, width(bit)));
            }
        }
        out.extend(d.pulse(first_mark_us + frames.len() as u64 * 60_000_000, 100_000));
        out
    }

    const T: i64 = 1_792_000_020 - 1_792_000_020 % 60; // a whole UTC minute in October 2026

    // VERIFIES: REQ-DCF-01
    #[test]
    fn a_frame_decodes_to_the_utc_time_of_the_announced_minute() {
        for (utc, cest) in [(T, true), (T + 86_400 * 150, false), (1_803_000_000 - 1_803_000_000 % 60, true), (951_782_400, false), (4_102_441_140, false)] {
            assert_eq!(decode(&frame_for(utc, cest)), Some(utc), "{utc} cest={cest}");
        }
        // 2026-10-25 02:30 local: the hour that happens twice is told apart by the zone bits
        let first = 1_792_881_000; // 2026-10-25 00:30 UTC = 02:30 CEST
        assert_eq!(decode(&frame_for(first, true)), Some(first));
        assert_eq!(decode(&frame_for(first + 3600, false)), Some(first + 3600), "02:30 CET, an hour later in UTC");
    }

    // VERIFIES: REQ-DCF-01
    #[test]
    fn every_single_wrong_bit_that_carries_time_is_refused() {
        // bits 1 to 16 (weather, call, announcement) and 19 (leap second) carry no time and no parity covers them
        let good = frame_for(T, true);
        for i in 0..59 {
            let mut b = good;
            b[i] ^= 1;
            let unused = (1..=16).contains(&i) || i == 19;
            assert_eq!(decode(&b), unused.then_some(T), "bit {i}");
        }
    }

    // VERIFIES: REQ-DCF-01
    #[test]
    fn impossible_dates_are_refused_even_with_good_parity() {
        let ok = Fields { year: 26, month: 10, day: 18, weekday: 7, hour: 10, minute: 30, cest: false };
        assert!(decode(&frame_of(&ok)).is_some());
        let bad = |f: Fields| decode(&frame_of(&f));
        assert_eq!(bad(Fields { weekday: 6, ..ok }), None, "weekday does not match the date");
        assert_eq!(bad(Fields { month: 11, day: 31, weekday: 3, ..ok }), None, "31 November");
        assert_eq!(bad(Fields { year: 25, month: 2, day: 29, weekday: 6, ..ok }), None, "29 February in a common year");
        assert!(bad(Fields { year: 24, month: 2, day: 29, weekday: 4, ..ok }).is_some(), "29 February 2024 exists");
        assert_eq!(bad(Fields { hour: 24, ..ok }), None);
        assert_eq!(bad(Fields { minute: 60, ..ok }), None);
        assert!(bad(Fields { minute: 59, ..ok }).is_some());
        assert_eq!(bad(Fields { month: 13, ..ok }), None);
        assert_eq!(bad(Fields { day: 0, ..ok }), None);
        // a units digit of 10 to 15 is not BCD
        let mut b = frame_of(&ok);
        b[21..25].copy_from_slice(&[0, 1, 0, 1]); // 10
        b[28] = b[21..28].iter().sum::<u8>() % 2;
        assert_eq!(decode(&b), None);
    }

    // VERIFIES: REQ-DCF-02
    #[test]
    fn a_fix_needs_two_consecutive_good_frames() {
        let mut d = Decoder::new();
        let start = 5_000_000_000u64;
        // frame k announces the minute that starts at mark k+1
        let frames: std::vec::Vec<_> = (1..=4).map(|k| frame_for(T + 60 * k, true)).collect();
        let fixes = feed(&mut d, start, &frames);
        // the first frame is ignored (no known minute mark before it), the second is not yet confirmed, the third is
        assert_eq!(fixes.len(), 2, "{fixes:?}");
        assert_eq!(fixes[0], Fix { unix_utc: T + 180, rise_us: start + 180_000_000 });
        assert_eq!(fixes[1], Fix { unix_utc: T + 240, rise_us: start + 240_000_000 });
        assert_eq!(d.stats.frames_ok, 3);
        assert_eq!(d.stats.frames_bad, 0);
    }

    // VERIFIES: REQ-DCF-02
    #[test]
    fn frames_that_do_not_follow_each_other_do_not_confirm() {
        let mut d = Decoder::new();
        // two good frames that announce times five minutes apart: one of them is wrong
        let frames = [frame_for(T + 60, true), frame_for(T + 120, true), frame_for(T + 120 + 300, true), frame_for(T + 120 + 300, true)];
        let fixes = feed(&mut d, 1_000_000_000, &frames);
        assert!(fixes.is_empty(), "{fixes:?}");
        // a frame with a parity error in between breaks the chain too
        let mut d = Decoder::new();
        let mut broken = frame_for(T + 180, true);
        broken[30] ^= 1;
        let frames = [frame_for(T + 60, true), frame_for(T + 120, true), broken, frame_for(T + 240, true), frame_for(T + 300, true)];
        let fixes = feed(&mut d, 1_000_000_000, &frames);
        assert_eq!(fixes.len(), 1, "only the last pair confirms: {fixes:?}");
        assert_eq!(fixes[0].unix_utc, T + 300);
        assert_eq!(d.stats.frames_bad, 1);
    }

    // VERIFIES: REQ-DCF-02
    #[test]
    fn interference_does_not_set_the_time() {
        let mut d = Decoder::new();
        let width = |bit: u8| if bit == 1 { 200_000 } else { 100_000 };
        let frames: std::vec::Vec<_> = (1..=4).map(|k| frame_for(T + 60 * k, true)).collect();
        let mut fixes = std::vec::Vec::new();
        for (k, f) in frames.iter().enumerate() {
            for (i, &bit) in f.iter().enumerate() {
                let rise = 7_000_000_000 + k as u64 * 60_000_000 + i as u64 * 1_000_000;
                if k == 1 && i == 20 {
                    d.pulse(rise + 400_000, 20_000); // a spike: ignored, the frame is unharmed
                }
                if k == 2 && i == 30 {
                    d.pulse(rise, 150_000); // neither a 0 nor a 1: this frame is lost
                    continue;
                }
                fixes.extend(d.pulse(rise, width(bit)));
            }
        }
        fixes.extend(d.pulse(7_000_000_000 + 4 * 60_000_000, 100_000));
        // frame 1 (second) fine, frame 2 lost -> no pair of consecutive frames
        assert!(fixes.is_empty(), "{fixes:?}");
        assert_eq!(d.stats.pulses_bad, 1);
        // a pulse train with the wrong rhythm never forms a frame
        let mut d = Decoder::new();
        let out: std::vec::Vec<_> = (0..300u64).filter_map(|i| d.pulse(i * 1_300_000, 100_000)).collect();
        assert!(out.is_empty());
    }

    // VERIFIES: REQ-DCF-04
    #[test]
    fn the_delay_is_learned_from_noisy_readings_and_bad_ones_are_ignored() {
        let mut e = DelayEstimator::new();
        // true delay 55 ms; NTP readings scatter by +-25 ms and one is wild
        let readings = [55 + 20, 55 - 25, 55 + 5, 55 - 10, 3000, 55 + 25, 55, 55 - 5, 55 + 10];
        let mut last = None;
        for (i, r) in readings.iter().enumerate() {
            let out = e.push(*r);
            if i + 1 < DelayEstimator::MIN_READINGS {
                assert_eq!(out, None, "too few readings after {}", i + 1);
            }
            last = out.or(last);
        }
        let learned = last.unwrap();
        assert!((50..=60).contains(&learned), "{learned}");
        // a reading outside +-1 s is not counted at all
        let mut e = DelayEstimator::new();
        for _ in 0..10 {
            assert_eq!(e.push(-5000), None);
        }
        // a receiver that is ahead of NTP gives 0, never a negative or absurd delay
        for _ in 0..9 {
            e.push(-30);
        }
        assert_eq!(e.push(-30), Some(0));
        for _ in 0..9 {
            e.push(900);
        }
        assert_eq!(e.push(900), Some(MAX_DELAY_MS));
    }

    // VERIFIES: REQ-DCF-04
    #[test]
    fn an_even_number_of_readings_gives_the_middle_between_the_two() {
        let mut e = DelayEstimator::new();
        let out: std::vec::Vec<_> = [82, 50, 73, 55].iter().map(|r| e.push(*r)).collect();
        assert_eq!(out, [None, None, None, Some(64)], "(55 + 73) / 2, not the upper one");
        assert_eq!(e.push(41), Some(55), "five readings: 41 50 55 73 82");
    }

    // VERIFIES: REQ-DCF-04
    #[test]
    fn the_delay_moves_the_instant_a_pulse_began_back() {
        assert_eq!(corrected_ms(10_000_000, 0), 10_000);
        assert_eq!(corrected_ms(10_000_000, 55), 9_945);
        assert_eq!(corrected_ms(30_000, 500), 0, "never below zero");
    }

    // VERIFIES: REQ-DCF-02
    #[test]
    fn real_receivers_widths_and_jitter_are_tolerated() {
        let mut d = Decoder::new();
        let frames: std::vec::Vec<_> = (1..=3).map(|k| frame_for(T + 60 * k, false)).collect();
        let mut fixes = std::vec::Vec::new();
        for (k, f) in frames.iter().enumerate() {
            for (i, &bit) in f.iter().enumerate() {
                let jitter = ((i * 37 + k * 11) % 41) as i64 - 20; // +-20 ms
                let rise = (9_000_000_000i64 + k as i64 * 60_000_000 + i as i64 * 1_000_000 + jitter * 1000) as u64;
                let width = if bit == 1 { 200_000 + jitter * 1000 / 2 } else { 100_000 - jitter * 1000 / 2 } as u32;
                fixes.extend(d.pulse(rise, width));
            }
        }
        fixes.extend(d.pulse(9_000_000_000 + 3 * 60_000_000, 100_000));
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].unix_utc, T + 180);
    }
}
