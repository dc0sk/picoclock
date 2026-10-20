---
project: picoclock
doc: docs/design/adr/0004-time-source-selection-and-dcf77.md
status: ledger
last_updated: 2026-10-18
---

# 0004. Time source selection (order, vote) and the DCF77 source

## Status

Accepted (the owner asked for source selection with two modes on 2026-10-16 and chose the pins and the module on
2026-10-18; delivered with software 0.2.0). The GPS source (u-blox NEO-M9N) is a later release and will reuse this design.
Requirements: REQ-SRC-01 to 05, REQ-DCF-01 to 03.

## Context

Until 0.1.0 the clock had one producer of time, the NTP client, which signalled a single value to the clock task. A second
source needs a rule for which of them may set the RTC, and the owner asked for two rules: an ordered list with fallbacks
(`order`) and a plausibility check between the sources with a selectable vote power each (`vote`). The full-code review of
2026-10-17 had also found that nothing checked a time before it was applied.

## Decision

1. **The decision is plain code in `core` (`core::sources`).** A `Selector` takes a `Sample` (UTC milliseconds at a monotonic
   instant) from a source and a `Policy` (mode, sources best first, vote power per source, tolerance) and returns a
   `Verdict`. The firmware (`sources.rs`) only receives samples on a channel, calls it and applies the result. Every source
   sends to the same channel; `NTP_TIME` became `SAMPLES` (in) and `TIME_APPLY` (out, to the clock task).
2. **`order`**: a sample is used if no source ranked above its own has a fresh sample. Fresh means younger than 3 h for NTP
   (the display already calls a clock synced for 3 h) and 15 min for DCF77 (a minute apart when receiving, but daytime
   reception is often lost for hours). A silent source is skipped; the higher one takes over again at its next sample.
3. **`vote`**: the fresh samples of the selected sources are projected to now and compared. Sources whose times differ by at
   most the tolerance (default 2 s) form a group; the group with the most vote power wins and is accepted only if its power is
   **more than half** of the power of all sources that deliver. A sample is used only if its source is in that group. A power
   of 0 means compared and shown, but cannot decide. Defaults: power 1 each, so two sources that disagree leave the clock
   alone; a source that is alone is a majority of itself.
4. **Defaults keep today's behaviour:** `time_mode order`, `time_sources ntp`. DCF77 is off until the user lists it, and the
   receiver is only powered then.
5. **A DCF77 time renews the sync time but rewrites the RTC only if the clock is more than 300 ms off.** The receiver has
   second resolution and some delay; the clock's sub-second phase comes from NTP. NTP samples are always written, as before.
6. **DCF77 decoding is in `core::dcf77`**, from pulse start and width only: spikes under 50 ms are ignored, 70 to 140 ms is a
   0, 170 to 260 ms a 1, anything else a fault; the missing pulse of second 59 (a gap of about 2 s) marks the minute. A frame
   is refused unless start bit, time-zone bits, three parity bits, BCD digits, the date and the transmitted weekday all fit,
   and a time is reported only when two consecutive frames announce minutes exactly 60 s apart. German local time is converted
   to UTC with the transmitted CET/CEST bits, not with the clock's timezone setting.
7. **The receiver is wired to GP0 (PON, low = on) and GP1 (TCO)**, edges timed with the microsecond timer in an async task.
8. **Added after 0.2.0 (REQ-SRC-06, 2026-10-20): a step limit.** A sample more than 1000 s from the running clock is used only
   after a second reading of the same source agrees within 5 s; meanwhile the clock is left alone and the sample does not count as
   fresh for the other sources. This does not protect against a source that is consistently wrong (a repeated week rollover would
   confirm itself): a GPS source needs the confirmation by a *different* source or a date floor (see the extension points in
   `docs/design/architecture.md`). "Stalls up to a second at an RTC write" in alternative A below describes the state of 0.2.0;
   since then the write waits in `Pending` without holding up the display.

## Alternatives considered

### A. Decide in the NTP task or in the clock task — REJECTED (assumed)
Neither can be tested without hardware, and the clock task already stalls up to a second at an RTC write. A pure function in
`core` is tested with every state in the table of `sources::tests`. Rejected on reasoning, not built.

### B. Capture the TCO edges with PIO or a timer input — REJECTED for now (assumed)
Gives microsecond-exact edges independent of the executor. The tolerance of the format (100 ms against 200 ms, a 300 ms write
threshold, two frames to confirm) makes a few milliseconds of wake-up jitter harmless; a stalled executor (the NTS key
exchange blocks it for up to 7 s on the RP2040) breaks a pulse and thereby a frame, which only delays the next fix. If bench
tests show lost frames from executor stalls, this is the next step. Not built; no measurement yet.

### C. One frame is enough — REJECTED (assumed)
A single frame passes parity with probability 1/8 for random bits, and the other checks lower that, but interference is not
random bits and DCF77 may be the only source (the vote cannot help then). Two consecutive frames cost one extra minute.

### D. A tie in `vote` goes to the more precise source — REJECTED (assumed)
That would let a spoofed NTP reply override a correct DCF77 time whenever the powers are equal, the case the vote exists for.
A tie leaves the clock alone and says so in `status`; the owner can give one source more power.

### E. Gate the sources by the clock's timezone setting — not applicable
DCF77 is transmitted in German time and is converted with its own bits, so the clock's `timezone` (which may be UTC, auto or
anywhere) does not enter.

## Consequences

- Several sources can be combined without new code in the clock task; GPS needs a `Source` variant, a sample producer and
  a receiver wiring (UART1 on GP4/GP5, PPS on GP3, see the architecture notes).
- With `time_sources dcf77` only, no NTP query is made and the clock still has a time source on a network without internet.
- `sync_secure` now follows the source that last set the clock, not the last NTP reply (a standby NTS reply no longer
  claims the clock is authenticated).
- The reception quality of the receiver in this case, next to a display and a WiFi radio, is unknown until it is fitted.
