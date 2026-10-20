//! Which time source sets the clock. Every source (NTP/NTS, DCF77, later GPS) hands in a [`Sample`] when it has a time; the
//! [`Selector`] decides, by the user's [`Policy`], whether that sample may set the clock. Two modes:
//!
//! * `order`: the selected sources form a list; a sample is used if no source ranked above its own has a fresh sample.
//!   A source that is silent or too old is skipped, so the next one takes over, and the higher one takes back as soon as it
//!   delivers again.
//! * `vote`: a plausibility check. The fresh samples of all selected sources are compared; sources whose times agree within
//!   the tolerance form a group, and a sample is used only if its source is in the group whose vote power is more than
//!   half of the power of all sources that deliver. Without such a group nothing is changed and the status says so.
//!
//! The decision is made here, in plain code, so that it can be tested without a clock; the firmware only applies it.

/// The time sources the clock knows. The index is the position in the arrays of [`Policy`] and [`Selector`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Ntp,
    Dcf77,
    Gps,
}

pub const SOURCES: [Source; 3] = [Source::Ntp, Source::Dcf77, Source::Gps];
const N: usize = SOURCES.len();

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Ntp => "ntp",
            Source::Dcf77 => "dcf77",
            Source::Gps => "gps",
        }
    }

    pub fn from_name(s: &str) -> Option<Source> {
        SOURCES.into_iter().find(|x| x.name().eq_ignore_ascii_case(s.trim()))
    }

    fn index(self) -> usize {
        self as usize
    }

    /// How long a sample counts as fresh. NTP syncs hourly (and the display calls a clock synced for three hours); DCF77
    /// delivers every minute while it receives, but daytime reception is often lost for hours, so a quarter of an hour.
    pub fn fresh_for_ms(self) -> u64 {
        match self {
            Source::Ntp => 3 * 3600 * 1000,
            Source::Dcf77 => 15 * 60 * 1000,
            // offered once a minute while the receiver is settled; ten minutes bridge a short loss of the fix (REQ-GPS-04)
            Source::Gps => 10 * 60 * 1000,
        }
    }

    /// The clock rewrites the RTC for a sample of this source only if it differs from it by more than this (ms); `None` = always.
    /// DCF77 has second resolution and some delay and the clock's phase is better set by NTP; a GPS time is tied to the PPS edge
    /// but each RTC write costs a restart of its prescaler, so a clock that already agrees is left alone (REQ-SRC-08).
    pub fn keep_ms(self) -> Option<i64> {
        match self {
            Source::Ntp => None,
            Source::Dcf77 => Some(300),
            Source::Gps => Some(50),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Order,
    Vote,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Order => "order",
            Mode::Vote => "vote",
        }
    }
}

/// The user's settings for source selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub mode: Mode,
    /// The selected sources, best first. A source that is not in the list is not used (and a receiver can be switched off).
    pub order: heapless::Vec<Source, N>,
    /// Vote power per source (index = `Source as usize`); 0 = compared and shown, but cannot decide.
    pub power: [u8; N],
    pub tolerance_s: u16,
}

impl Policy {
    pub fn selected(&self, s: Source) -> bool {
        self.order.contains(&s)
    }

    fn rank(&self, s: Source) -> Option<usize> {
        self.order.iter().position(|x| *x == s)
    }
}

/// A time a source delivered: `unix_ms` was UTC at the monotonic instant `at_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub unix_ms: i64,
    pub at_ms: u64,
}

impl Sample {
    /// The UTC time in ms at the monotonic instant `now_ms`.
    pub fn now_ms(&self, now_ms: u64) -> i64 {
        self.unix_ms + now_ms.saturating_sub(self.at_ms) as i64
    }
}

/// What became of a sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Set the clock to this sample. `write` = the RTC needs writing; otherwise the clock already agrees and only the time of
    /// the last synchronisation is renewed.
    Use { write: bool },
    /// Mode `order`: a source ranked higher is fresh and decides.
    Standby,
    /// Mode `vote`: this source's time is outside the group that holds the majority.
    Outvoted,
    /// Mode `vote`: no group holds more than half of the vote power; the clock is left alone.
    NoMajority,
    /// The sample is further from the clock than [`STEP_LIMIT_MS`] and nothing confirmed it yet: the clock is left alone and a
    /// second reading that agrees (within [`CONFIRM_MS`]) is waited for.
    Suspect,
    /// The sample is far from the clock and another selected source with a fresh time disagrees with it (REQ-SRC-07): the clock
    /// is left alone however often this source repeats it.
    Contradicted,
    /// The source is not selected.
    Off,
}

/// What `status` shows for a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Off,
    /// Selected, nothing delivered yet.
    Waiting,
    /// Delivered, but longer ago than it counts as fresh.
    Stale,
    Used,
    Standby,
    Outvoted,
    NoMajority,
    /// The last time was far from the clock and is waiting for a second reading.
    Suspect,
    /// The last time was far from the clock and another source disagrees with it.
    Contradicted,
}

impl State {
    pub fn text(self) -> &'static str {
        match self {
            State::Off => "off",
            State::Waiting => "waiting",
            State::Stale => "stale",
            State::Used => "in use",
            State::Standby => "standby (a higher source decides)",
            State::Outvoted => "rejected (outvoted)",
            State::NoMajority => "no majority, clock left alone",
            State::Suspect => "far from the clock, waiting for a second reading",
            State::Contradicted => "far from the clock and contradicted by another source",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub source: Source,
    pub state: State,
    /// The source's last time against the clock now (ms, positive = the source is ahead), if it has one that is fresh.
    pub offset_ms: Option<i64>,
    pub age_s: Option<u64>,
}

/// A source with a keep limit does not rewrite the RTC more often than this, unless the clock is more than [`BIG_JUMP_MS`] off.
pub const MIN_WRITE_GAP_MS: u64 = 10_000;
pub const BIG_JUMP_MS: i64 = 1_000;

/// A time further than this from the clock (1000 s, the limit ntpd calls the panic threshold) is not taken from one reading: a
/// wrong or forged answer must not move the clock by hours. It is used once a second reading of the same source agrees. Not
/// applied while the clock has no valid time (nothing to compare with).
pub const STEP_LIMIT_MS: i64 = 1_000_000;
/// Two readings of a far-away time agree if they differ by no more than this.
pub const CONFIRM_MS: i64 = 5_000;

/// The ranks of the members of a group in `time_sources`, best first: a group with the better key has the better-ranked member
/// (compared member by member).
fn rank_key(policy: &Policy, group: &[Source]) -> heapless::Vec<usize, N> {
    let mut k: heapless::Vec<usize, N> = group.iter().map(|s| policy.rank(*s).unwrap_or(usize::MAX)).collect();
    k.sort_unstable();
    k
}

#[derive(Debug, Default)]
pub struct Selector {
    last: [Option<Sample>; N],
    verdict: [Option<Verdict>; N],
    /// The first reading of a far-away time, per source, and how many such readings in a row.
    suspect: [Option<Sample>; N],
    suspect_n: [u8; N],
    /// When the RTC was last written for a sample of each source (monotonic ms).
    last_write: [Option<u64>; N],
}

impl Selector {
    pub const fn new() -> Self {
        Self { last: [None; N], verdict: [None; N], suspect: [None; N], suspect_n: [0; N], last_write: [None; N] }
    }

    fn fresh(&self, s: Source, now_ms: u64) -> Option<Sample> {
        self.last[s.index()].filter(|x| now_ms.saturating_sub(x.at_ms) <= s.fresh_for_ms())
    }

    /// A source delivered a time. `clock_ms` is the clock's own UTC time now in ms, if it has one.
    pub fn offer(&mut self, policy: &Policy, src: Source, sample: Sample, now_ms: u64, clock_ms: Option<i64>) -> Verdict {
        let i = src.index();
        if let Some(clock) = clock_ms.filter(|_| policy.selected(src)) {
            if (sample.now_ms(now_ms) - clock).abs() > STEP_LIMIT_MS {
                let confirmed = self.suspect[i].is_some_and(|first| (first.now_ms(now_ms) - sample.now_ms(now_ms)).abs() <= CONFIRM_MS);
                // REQ-SRC-07: a source that repeats itself proves nothing (a receiver that is consistently wrong does), so a fresh
                // time of any other selected source that disagrees keeps the clock where it is
                let tol = policy.tolerance_s as i64 * 1000;
                let contradicted = SOURCES.into_iter().filter(|o| *o != src && policy.selected(*o)).any(|o| {
                    self.fresh(o, now_ms).is_some_and(|x| (x.now_ms(now_ms) - sample.now_ms(now_ms)).abs() > tol)
                });
                if !confirmed || contradicted {
                    self.suspect[i] = Some(sample);
                    self.suspect_n[i] = self.suspect_n[i].saturating_add(1);
                    let v = if confirmed { Verdict::Contradicted } else { Verdict::Suspect };
                    self.verdict[i] = Some(v);
                    return v;
                }
            }
        }
        self.suspect[i] = None;
        self.suspect_n[i] = 0;
        self.last[src.index()] = Some(sample);
        let verdict = self.decide(policy, src, sample, now_ms, clock_ms);
        if verdict == (Verdict::Use { write: true }) {
            self.last_write[i] = Some(now_ms);
        }
        self.verdict[src.index()] = Some(verdict);
        verdict
    }

    fn decide(&self, policy: &Policy, src: Source, sample: Sample, now_ms: u64, clock_ms: Option<i64>) -> Verdict {
        let Some(rank) = policy.rank(src) else { return Verdict::Off };
        let usable = match policy.mode {
            Mode::Order => {
                let higher_fresh = policy.order.iter().take(rank).any(|s| self.fresh(*s, now_ms).is_some());
                if higher_fresh {
                    return Verdict::Standby;
                }
                true
            }
            Mode::Vote => match self.vote(policy, now_ms) {
                None => return Verdict::NoMajority,
                Some(group) => group.contains(&src),
            },
        };
        if !usable {
            return Verdict::Outvoted;
        }
        let write = match (src.keep_ms(), clock_ms) {
            (Some(keep), Some(clock)) => {
                let off = (sample.now_ms(now_ms) - clock).abs();
                // inside the keep limit the clock is already right; a source other than NTP also may not rewrite the RTC more
                // often than every MIN_WRITE_GAP_MS unless the clock is far off (REQ-SRC-08)
                off > keep && (off > BIG_JUMP_MS || self.last_write[src.index()].is_none_or(|t| now_ms.saturating_sub(t) >= MIN_WRITE_GAP_MS))
            }
            _ => true,
        };
        Verdict::Use { write }
    }

    /// How many far-away readings of this source in a row are waiting for a confirmation.
    pub fn suspect_count(&self, s: Source) -> u8 {
        self.suspect_n[s.index()]
    }

    /// The sources of the group that holds the majority of the vote power, if there is one.
    fn vote(&self, policy: &Policy, now_ms: u64) -> Option<heapless::Vec<Source, N>> {
        let tol = policy.tolerance_s as i64 * 1000;
        let delivering: heapless::Vec<(Source, i64), N> =
            SOURCES.into_iter().filter(|s| policy.selected(*s)).filter_map(|s| self.fresh(s, now_ms).map(|x| (s, x.now_ms(now_ms)))).collect();
        let total: u32 = delivering.iter().map(|(s, _)| policy.power[s.index()] as u32).sum();
        let mut best: Option<(u32, heapless::Vec<Source, N>)> = None;
        for mask in 1u32..(1 << delivering.len()) {
            let members: heapless::Vec<(Source, i64), N> = delivering.iter().enumerate().filter(|(i, _)| mask >> i & 1 == 1).map(|(_, x)| *x).collect();
            let (lo, hi) = members.iter().fold((i64::MAX, i64::MIN), |(lo, hi), (_, t)| (lo.min(*t), hi.max(*t)));
            if hi - lo > tol {
                continue;
            }
            let power: u32 = members.iter().map(|(s, _)| policy.power[s.index()] as u32).sum();
            let group: heapless::Vec<Source, N> = members.iter().map(|(s, _)| *s).collect();
            // more power wins, then more sources, then the group with the best-ranked member (REQ-SRC-09), so that the
            // order in which the groups are enumerated never decides
            if best.as_ref().is_none_or(|(p, g)| power > *p || (power == *p && (group.len() > g.len() || (group.len() == g.len() && rank_key(policy, &group) < rank_key(policy, g))))) {
                best = Some((power, group));
            }
        }
        match best {
            Some((power, group)) if power * 2 > total => Some(group),
            _ => None,
        }
    }

    /// The state of every source, for `status` and the web page.
    pub fn report(&self, policy: &Policy, now_ms: u64, clock_ms: Option<i64>) -> [Report; N] {
        SOURCES.map(|source| {
            let i = source.index();
            let fresh = self.fresh(source, now_ms);
            let state = if !policy.selected(source) {
                State::Off
            } else if self.verdict[i] == Some(Verdict::Contradicted) {
                State::Contradicted
            } else if self.verdict[i] == Some(Verdict::Suspect) {
                State::Suspect
            } else if self.last[i].is_none() {
                State::Waiting
            } else if fresh.is_none() {
                State::Stale
            } else {
                match self.verdict[i] {
                    Some(Verdict::Use { .. }) => State::Used,
                    Some(Verdict::Standby) => State::Standby,
                    Some(Verdict::Outvoted) => State::Outvoted,
                    Some(Verdict::NoMajority) => State::NoMajority,
                    _ => State::Waiting,
                }
            };
            Report {
                source,
                state,
                offset_ms: fresh.zip(clock_ms).filter(|_| !matches!(state, State::Suspect | State::Contradicted)).map(|(x, c)| x.now_ms(now_ms) - c),
                age_s: self.last[i].filter(|_| !matches!(state, State::Suspect | State::Contradicted)).map(|x| now_ms.saturating_sub(x.at_ms) / 1000),
            }
        })
    }

    /// The source's latest sample if it is younger than `max_age_ms` and was used to set the clock (a sample that was outvoted, on
    /// standby or held back is no reference for anything else).
    pub fn recent_used(&self, s: Source, now_ms: u64, max_age_ms: u64) -> Option<Sample> {
        self.recent(s, now_ms, max_age_ms).filter(|_| matches!(self.verdict[s.index()], Some(Verdict::Use { .. })))
    }

    /// The source's latest sample if it is younger than `max_age_ms`.
    pub fn recent(&self, s: Source, now_ms: u64, max_age_ms: u64) -> Option<Sample> {
        self.last[s.index()].filter(|x| now_ms.saturating_sub(x.at_ms) <= max_age_ms)
    }

    /// A source stopped (receiver switched off, source deselected): its old sample no longer counts.
    pub fn forget(&mut self, s: Source) {
        self.last[s.index()] = None;
        self.verdict[s.index()] = None;
        self.suspect[s.index()] = None;
        self.suspect_n[s.index()] = 0;
        self.last_write[s.index()] = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Source::{Dcf77, Gps, Ntp};

    fn policy(mode: Mode, order: &[Source], power: [u8; 2], tolerance_s: u16) -> Policy {
        Policy { mode, order: order.iter().copied().collect(), power: [power[0], power[1], 1], tolerance_s }
    }
    fn policy3(mode: Mode, order: &[Source], power: [u8; 3], tolerance_s: u16) -> Policy {
        Policy { mode, order: order.iter().copied().collect(), power, tolerance_s }
    }

    const T0: i64 = 1_792_000_000_000;
    fn at(now_ms: u64, offset_ms: i64) -> Sample {
        Sample { unix_ms: T0 + now_ms as i64 + offset_ms, at_ms: now_ms }
    }
    fn clock(now_ms: u64) -> Option<i64> {
        Some(T0 + now_ms as i64)
    }
    const MIN: u64 = 60_000;
    const H: u64 = 3_600_000;

    // VERIFIES: REQ-SRC-01
    #[test]
    fn with_only_ntp_selected_every_sample_sets_the_clock_as_before() {
        for mode in [Mode::Order, Mode::Vote] {
            let p = policy(mode, &[Ntp], [1, 1], 2);
            let mut s = Selector::new();
            for k in 0..5u64 {
                let now = k * H;
                assert_eq!(s.offer(&p, Ntp, at(now, 40), now, clock(now)), Verdict::Use { write: true }, "{mode:?}");
            }
            // a source that is not selected is never used
            assert_eq!(s.offer(&p, Dcf77, at(0, 0), 0, clock(0)), Verdict::Off);
            assert_eq!(s.report(&p, 4 * H, clock(4 * H))[1].state, State::Off);
        }
    }

    // VERIFIES: REQ-SRC-03
    #[test]
    fn only_a_sample_that_was_used_is_a_reference_for_the_dcf77_delay() {
        let p = policy(Mode::Vote, &[Dcf77, Ntp], [1, 1], 2);
        let mut s = Selector::new();
        assert_eq!(s.offer(&p, Ntp, at(0, 0), 0, clock(0)), Verdict::Use { write: true });
        assert_eq!(s.offer(&p, Dcf77, at(1, 1500), 1, clock(1)), Verdict::Use { write: true });
        // used samples are references; one that was outvoted or held back is not
        assert!(s.recent_used(Ntp, 2, 10_000).is_some());
        let q = policy(Mode::Order, &[Dcf77, Ntp], [1, 1], 2);
        let mut s = Selector::new();
        s.offer(&q, Dcf77, at(0, 0), 0, clock(0));
        s.offer(&q, Ntp, at(1, 0), 1, clock(1)); // standby behind DCF77
        assert!(s.recent(Ntp, 2, 10_000).is_some() && s.recent_used(Ntp, 2, 10_000).is_none());
        let mut s = Selector::new();
        s.offer(&q, Ntp, at(0, 5_000_000), 0, clock(0)); // held back
        assert!(s.recent_used(Ntp, 1, 10_000).is_none());
        assert_eq!(s.report(&q, 1, clock(1))[0].offset_ms, None, "a held-back sample shows no offset");
    }

    // VERIFIES: REQ-SRC-06
    #[test]
    fn a_far_away_time_needs_a_second_agreeing_reading() {
        let p = policy(Mode::Order, &[Ntp, Dcf77], [1, 1], 2);
        let mut s = Selector::new();
        let hours_ahead = 3 * H as i64;
        // no clock yet: nothing to compare with, the sample is used
        assert_eq!(s.offer(&p, Ntp, at(0, hours_ahead), 0, None), Verdict::Use { write: true });
        // a clock exists: one far reading is held back, the clock stays, and a lower source is not pushed aside by it
        assert_eq!(s.offer(&p, Ntp, at(MIN, 7 * H as i64), MIN, clock(MIN)), Verdict::Suspect);
        assert_eq!(s.suspect_count(Ntp), 1);
        assert_eq!(s.report(&p, MIN, clock(MIN))[0].state, State::Suspect);
        // a second reading far away but different is held back too (a guess moves nothing)
        assert_eq!(s.offer(&p, Ntp, at(2 * MIN, 9 * H as i64), 2 * MIN, clock(2 * MIN)), Verdict::Suspect);
        assert_eq!(s.suspect_count(Ntp), 2);
        // one that agrees with the previous reading is used
        assert_eq!(s.offer(&p, Ntp, at(3 * MIN, 9 * H as i64 + 800), 3 * MIN, clock(3 * MIN)), Verdict::Use { write: true });
        assert_eq!(s.suspect_count(Ntp), 0);
        // a normal sample in between forgets the first far reading
        let mut s = Selector::new();
        assert_eq!(s.offer(&p, Ntp, at(0, 2 * H as i64), 0, clock(0)), Verdict::Suspect);
        assert_eq!(s.offer(&p, Ntp, at(MIN, 20), MIN, clock(MIN)), Verdict::Use { write: true });
        assert_eq!(s.offer(&p, Ntp, at(2 * MIN, 2 * H as i64), 2 * MIN, clock(2 * MIN)), Verdict::Suspect, "the old first reading is gone");
        // near the limit (999 s) is still taken at once, 1001 s is not
        let mut s = Selector::new();
        assert_eq!(s.offer(&p, Ntp, at(0, 999_000), 0, clock(0)), Verdict::Use { write: true });
        let mut s = Selector::new();
        assert_eq!(s.offer(&p, Ntp, at(0, 1_001_000), 0, clock(0)), Verdict::Suspect);
        // a suspect reading is not a fresh sample: the lower source is not held back by it
        let mut s = Selector::new();
        assert_eq!(s.offer(&p, Ntp, at(0, 2 * H as i64), 0, clock(0)), Verdict::Suspect);
        assert_eq!(s.offer(&p, Dcf77, at(MIN, 400), MIN, clock(MIN)), Verdict::Use { write: true });
    }

    // VERIFIES: REQ-SRC-02
    #[test]
    fn order_falls_back_when_the_better_source_is_silent_and_returns_when_it_delivers() {
        let p = policy(Mode::Order, &[Ntp, Dcf77], [1, 1], 2);
        let mut s = Selector::new();
        // DCF77 alone first: nothing ranked above it has a time
        assert_eq!(s.offer(&p, Dcf77, at(0, 0), 0, None), Verdict::Use { write: true });
        // NTP arrives: it ranks higher and is used
        assert_eq!(s.offer(&p, Ntp, at(MIN, 20), MIN, clock(MIN)), Verdict::Use { write: true });
        // while NTP is fresh, DCF77 only stands by
        assert_eq!(s.offer(&p, Dcf77, at(2 * MIN, 0), 2 * MIN, clock(2 * MIN)), Verdict::Standby);
        assert_eq!(s.report(&p, 2 * MIN, clock(2 * MIN))[1].state, State::Standby);
        // NTP goes silent for more than its freshness: DCF77 takes over (the clock is within 300 ms, so no RTC write)
        let later = MIN + Ntp.fresh_for_ms() + MIN;
        assert_eq!(s.offer(&p, Dcf77, at(later, 100), later, clock(later)), Verdict::Use { write: false });
        assert_eq!(s.report(&p, later, clock(later))[0].state, State::Stale);
        // NTP delivers again: back to NTP at once
        let back = later + MIN;
        assert_eq!(s.offer(&p, Ntp, at(back, 5), back, clock(back)), Verdict::Use { write: true });
        // the other order puts DCF77 first
        let p = policy(Mode::Order, &[Dcf77, Ntp], [1, 1], 2);
        let mut s = Selector::new();
        assert_eq!(s.offer(&p, Dcf77, at(0, 0), 0, None), Verdict::Use { write: true });
        assert_eq!(s.offer(&p, Ntp, at(MIN, 0), MIN, clock(MIN)), Verdict::Standby);
    }

    // VERIFIES: REQ-SRC-02
    #[test]
    fn a_dcf77_sample_only_rewrites_the_rtc_when_the_clock_is_off() {
        let p = policy(Mode::Order, &[Dcf77], [1, 1], 2);
        let mut s = Selector::new();
        assert_eq!(s.offer(&p, Dcf77, at(MIN, 250), MIN, clock(MIN)), Verdict::Use { write: false });
        assert_eq!(s.offer(&p, Dcf77, at(2 * MIN, -299), 2 * MIN, clock(2 * MIN)), Verdict::Use { write: false });
        assert_eq!(s.offer(&p, Dcf77, at(3 * MIN, 301), 3 * MIN, clock(3 * MIN)), Verdict::Use { write: true });
        assert_eq!(s.offer(&p, Dcf77, at(4 * MIN, -1000), 4 * MIN, clock(4 * MIN)), Verdict::Use { write: true });
        assert_eq!(s.offer(&p, Dcf77, at(5 * MIN, 0), 5 * MIN, None), Verdict::Use { write: true }, "a clock without a time is always set");
    }

    // VERIFIES: REQ-SRC-03
    #[test]
    fn vote_accepts_agreeing_sources_and_refuses_when_nobody_has_a_majority() {
        let p = policy(Mode::Vote, &[Ntp, Dcf77], [1, 1], 2);
        let mut s = Selector::new();
        // one source alone is a majority of itself
        assert_eq!(s.offer(&p, Dcf77, at(0, 0), 0, clock(0)), Verdict::Use { write: false });
        // NTP agrees within 2 s: both in the group
        assert_eq!(s.offer(&p, Ntp, at(MIN, 900), MIN, clock(MIN)), Verdict::Use { write: true });
        assert_eq!(s.offer(&p, Dcf77, at(2 * MIN, 0), 2 * MIN, clock(2 * MIN)), Verdict::Use { write: false });
        // NTP now claims 500 s ahead: two sources with one vote each disagree, nobody has more than half
        let now = 3 * MIN;
        let spoofed = at(now, 500_000);
        assert_eq!(s.offer(&p, Ntp, spoofed, now, clock(now)), Verdict::NoMajority);
        let r = s.report(&p, now, clock(now));
        assert_eq!((r[0].state, r[0].offset_ms), (State::NoMajority, Some(500_000)));
        // DCF77 delivering meanwhile is also not used: the clock stays as it is
        assert_eq!(s.offer(&p, Dcf77, at(now + MIN, 0), now + MIN, clock(now + MIN)), Verdict::NoMajority);
    }

    // VERIFIES: REQ-SRC-03 REQ-SRC-04
    #[test]
    fn vote_power_decides_and_zero_power_cannot() {
        // NTP counts twice: it outvotes a wrong DCF77 reading
        let p = policy(Mode::Vote, &[Ntp, Dcf77], [2, 1], 2);
        let mut s = Selector::new();
        s.offer(&p, Dcf77, at(0, 3_500), 0, clock(0));
        assert_eq!(s.offer(&p, Ntp, at(10, 0), 10, clock(10)), Verdict::Use { write: true });
        assert_eq!(s.offer(&p, Dcf77, at(MIN, 3_500), MIN, clock(MIN)), Verdict::Outvoted);
        assert_eq!(s.report(&p, MIN, clock(MIN))[1].state, State::Outvoted);
        // a source with power 0 is shown and compared but cannot set the clock
        let p = policy(Mode::Vote, &[Ntp, Dcf77], [0, 1], 2);
        let mut s = Selector::new();
        assert_eq!(s.offer(&p, Ntp, at(0, 0), 0, clock(0)), Verdict::NoMajority, "total power 0: nobody can decide");
        let mut s = Selector::new();
        s.offer(&p, Ntp, at(0, 0), 0, clock(0));
        assert_eq!(s.offer(&p, Dcf77, at(1000, 0), 1000, clock(1000)), Verdict::Use { write: false }, "DCF77 decides, NTP agrees");
        // three sources would need a third selectable source; with two, a 1:1 tie at the tolerance edge
        let p = policy(Mode::Vote, &[Ntp, Dcf77], [1, 1], 2);
        let mut s = Selector::new();
        s.offer(&p, Ntp, at(0, 2000), 0, clock(0));
        assert_eq!(s.offer(&p, Dcf77, at(0, 0), 0, clock(0)), Verdict::Use { write: false }, "exactly the tolerance still agrees");
        let mut s = Selector::new();
        s.offer(&p, Ntp, at(0, 2001), 0, clock(0));
        assert_eq!(s.offer(&p, Dcf77, at(0, 0), 0, clock(0)), Verdict::NoMajority, "one ms more does not");
    }

    // VERIFIES: REQ-SRC-03
    #[test]
    fn a_stale_source_does_not_vote() {
        let p = policy(Mode::Vote, &[Ntp, Dcf77], [1, 1], 2);
        let mut s = Selector::new();
        s.offer(&p, Dcf77, at(0, 99_000), 0, clock(0)); // wrong, but alone
        let later = Dcf77.fresh_for_ms() + MIN;
        // its sample is too old to count: NTP is alone and wins
        assert_eq!(s.offer(&p, Ntp, at(later, 0), later, clock(later)), Verdict::Use { write: true });
    }

    // VERIFIES: REQ-SRC-05
    #[test]
    fn the_report_names_every_state() {
        let p = policy(Mode::Order, &[Ntp, Dcf77], [1, 1], 2);
        let mut s = Selector::new();
        assert_eq!(s.report(&p, 0, None).map(|r| r.state), [State::Waiting, State::Waiting, State::Off]);
        s.offer(&p, Ntp, at(0, 120), 0, clock(0));
        let r = s.report(&p, 5000, clock(5000));
        assert_eq!((r[0].state, r[0].offset_ms, r[0].age_s), (State::Used, Some(120), Some(5)));
        s.forget(Ntp);
        assert_eq!(s.report(&p, 5000, clock(5000))[0].state, State::Waiting);
        let none = policy(Mode::Order, &[], [1, 1], 2);
        assert_eq!(s.report(&none, 0, None).map(|r| r.state), [State::Off, State::Off, State::Off]);
        for st in [State::Off, State::Waiting, State::Stale, State::Used, State::Standby, State::Outvoted, State::NoMajority, State::Suspect, State::Contradicted] {
            assert!(!st.text().is_empty());
        }
        assert_eq!(Source::from_name(" DCF77 "), Some(Dcf77));
        assert_eq!(Source::from_name("gps"), Some(Gps));
        assert_eq!(Source::from_name("glonass"), None);
    }

    // VERIFIES: REQ-SRC-07
    #[test]
    fn a_far_away_time_that_another_fresh_source_contradicts_is_never_used() {
        // GPS ranks above NTP and repeats a time a week-rollover away (1024 weeks = 619 315 200 s); NTP is fresh and right
        let rollover = -1024 * 7 * 86_400 * 1000;
        let p = policy3(Mode::Order, &[Gps, Ntp], [1, 1, 1], 2);
        let mut s = Selector::new();
        assert_eq!(s.offer(&p, Ntp, at(0, 0), 0, clock(0)), Verdict::Use { write: true });
        let first = s.offer(&p, Gps, at(MIN, rollover), MIN, clock(MIN));
        assert_eq!(first, Verdict::Suspect, "a first far-away reading waits");
        // the second, agreeing reading would have been enough before; NTP contradicts it now
        let second = s.offer(&p, Gps, at(2 * MIN, rollover), 2 * MIN, clock(2 * MIN));
        assert_eq!(second, Verdict::Contradicted);
        assert_eq!(s.report(&p, 2 * MIN, clock(2 * MIN))[2].state, State::Contradicted);
        assert_eq!(s.report(&p, 2 * MIN, clock(2 * MIN))[2].offset_ms, None, "no offset is shown for a time that is held back");
        assert_eq!(s.offer(&p, Gps, at(3 * MIN, rollover), 3 * MIN, clock(3 * MIN)), Verdict::Contradicted, "however often it repeats itself");
        assert!(s.suspect_count(Gps) >= 3);
        // it did not count as fresh for anybody: NTP is still used
        assert_eq!(s.offer(&p, Ntp, at(4 * MIN, 5), 4 * MIN, clock(4 * MIN)), Verdict::Use { write: true });
        // no other source delivering: nothing can contradict, the second reading is enough (the date floor is the defence then)
        let mut s = Selector::new();
        s.offer(&p, Gps, at(0, 1000), 0, clock(0));
        s.forget(Ntp);
        assert_eq!(s.offer(&p, Gps, at(MIN, rollover), MIN, clock(MIN)), Verdict::Suspect);
        assert_eq!(s.offer(&p, Gps, at(2 * MIN, rollover), 2 * MIN, clock(2 * MIN)), Verdict::Use { write: true });
        // a stale NTP time does not contradict
        let mut s = Selector::new();
        s.offer(&p, Ntp, at(0, 0), 0, clock(0));
        let later = Ntp.fresh_for_ms() + MIN;
        s.offer(&p, Gps, at(later, rollover), later, clock(later));
        assert_eq!(s.offer(&p, Gps, at(later + MIN, rollover), later + MIN, clock(later + MIN)), Verdict::Use { write: true });
        // two sources that agree on a far-away time (a dead RTC battery) confirm each other
        let mut s = Selector::new();
        let year = 365 * 86_400 * 1000;
        s.offer(&p, Ntp, at(0, year), 0, clock(0));
        s.offer(&p, Gps, at(MIN, year), MIN, clock(MIN));
        assert_eq!(s.offer(&p, Ntp, at(2 * MIN, year), 2 * MIN, clock(2 * MIN)), Verdict::Use { write: true });
    }

    // VERIFIES: REQ-SRC-08
    #[test]
    fn each_source_has_its_own_keep_limit_and_a_minimum_gap_between_rtc_writes() {
        assert_eq!((Ntp.keep_ms(), Dcf77.keep_ms(), Gps.keep_ms()), (None, Some(300), Some(50)));
        let p = policy3(Mode::Order, &[Gps], [1, 1, 1], 2);
        let mut s = Selector::new();
        // within 50 ms nothing is written, from 51 ms on it is
        assert_eq!(s.offer(&p, Gps, at(MIN, 50), MIN, clock(MIN)), Verdict::Use { write: false });
        assert_eq!(s.offer(&p, Gps, at(2 * MIN, -50), 2 * MIN, clock(2 * MIN)), Verdict::Use { write: false });
        assert_eq!(s.offer(&p, Gps, at(3 * MIN, 51), 3 * MIN, clock(3 * MIN)), Verdict::Use { write: true });
        // a second write less than 10 s later is held back unless the clock is more than a second off
        assert_eq!(s.offer(&p, Gps, at(3 * MIN + 5_000, 80), 3 * MIN + 5_000, clock(3 * MIN + 5_000)), Verdict::Use { write: false });
        assert_eq!(s.offer(&p, Gps, at(3 * MIN + 5_000, 1_001), 3 * MIN + 5_000, clock(3 * MIN + 5_000)), Verdict::Use { write: true });
        assert_eq!(s.offer(&p, Gps, at(3 * MIN + 15_001, 80), 3 * MIN + 15_001, clock(3 * MIN + 15_001)), Verdict::Use { write: true }, "10 s after the last write");
        // NTP always writes, as before, however close and however often
        let p = policy3(Mode::Order, &[Ntp], [1, 1, 1], 2);
        let mut s = Selector::new();
        for i in 0..3u64 {
            assert_eq!(s.offer(&p, Ntp, at(MIN + i, 0), MIN + i, clock(MIN + i)), Verdict::Use { write: true });
        }
    }

    // VERIFIES: REQ-SRC-09
    #[test]
    fn overlapping_groups_of_equal_strength_go_to_the_best_ranked_member_whatever_the_order() {
        // A=0 s, B=1.5 s, C=3 s with a tolerance of 2 s: {A,B} and {B,C} each hold 2 of 3 votes
        let times = [(Ntp, 0), (Dcf77, 1_500), (Gps, 3_000)];
        for order in [[Ntp, Dcf77, Gps], [Gps, Dcf77, Ntp], [Dcf77, Gps, Ntp], [Gps, Ntp, Dcf77]] {
            let p = policy3(Mode::Vote, &order, [1, 1, 1], 2);
            // the same samples offered in every sequence give the same group: the one holding the best-ranked source
            for seq in [[0, 1, 2], [2, 1, 0], [1, 0, 2], [1, 2, 0]] {
                let mut s = Selector::new();
                for k in seq {
                    let (src, off) = times[k];
                    s.offer(&p, src, at(0, off), 0, clock(0));
                }
                let used: Vec<Source> = SOURCES
                    .into_iter()
                    .filter(|src| matches!(s.offer(&p, *src, at(0, times[*src as usize].1), 0, clock(0)), Verdict::Use { .. }))
                    .collect();
                // the group holds the best-ranked source and the middle one (DCF77 is in both groups); when DCF77 itself is best
                // ranked, the better-ranked of the two others joins it
                let best_other = order.into_iter().find(|s| *s != Dcf77).unwrap();
                let other = if order[0] == Dcf77 { best_other } else { order[0] };
                assert!(used.len() == 2 && used.contains(&Dcf77) && used.contains(&other), "order {order:?} seq {seq:?}: {used:?}");
            }
        }
        // a larger group beats a better-ranked smaller one
        let p = policy3(Mode::Vote, &[Ntp, Dcf77, Gps], [1, 1, 1], 2);
        let mut s = Selector::new();
        s.offer(&p, Ntp, at(0, 5_000), 0, clock(0));
        s.offer(&p, Dcf77, at(0, 0), 0, clock(0));
        assert_eq!(s.offer(&p, Gps, at(0, 10), 0, clock(0)), Verdict::Use { write: false });
        assert_eq!(s.offer(&p, Ntp, at(0, 5_000), 0, clock(0)), Verdict::Outvoted);
        // more power beats rank
        let p = policy3(Mode::Vote, &[Ntp, Dcf77, Gps], [1, 1, 3], 2);
        let mut s = Selector::new();
        s.offer(&p, Ntp, at(0, 0), 0, clock(0));
        s.offer(&p, Dcf77, at(0, 1_500), 0, clock(0));
        s.offer(&p, Gps, at(0, 3_000), 0, clock(0));
        assert_eq!(s.offer(&p, Ntp, at(0, 0), 0, clock(0)), Verdict::Outvoted, "GPS and DCF77 hold 4 of 5 votes");
    }
}
