---
project: picoclock
doc: docs/design/adr/0007-gps-time-source.md
status: ledger
last_updated: 2026-10-20
---

# 0007. GPS as the third time source (u-blox NEO-M9N)

## Status

Proposed (2026-10-20). The owner planned the receiver on 2026-10-17/18 (SparkFun u-blox NEO-M9N, UART1 on GP4/GP5, PPS on GP3) and asked
for it to be the next release cycle, after a full-code review (done, `docs/design/reviews/2026-10-20-full-code-review.md`). The module is
**not connected yet** (owner, 2026-10-20): everything that can be built and tested without it is built first, and the delivery as
software 0.3.0 waits for the hardware records listed under Consequences. Requirements: REQ-GPS-01 to 07, REQ-SRC-07 to 10 (all draft).

## Context

ADR-0004 built the source selection (modes `order` and `vote`) for several sources and delivered DCF77 as the second. The review of
2026-10-20 listed what a third source, and one that delivers every second with millisecond accuracy, has to settle first (B2, B3, B4,
B8, B9, B10, B16, C16 in the review record): a keep rule per source, a limit on RTC rewrites, PPS edges stamped outside the main
executor, confirmation of a far-away time by a *different* source, the vote tie rule, and the status/config/web code that was written
for two sources. The NEO-M9N sends NMEA 0183 sentences at 38400 baud (factory setting; the sentences include RMC) and a 1 Hz pulse on
its time-pulse pin that is aligned to UTC once the receiver has a fix.

## Decision

1. **The clock only listens.** UART1 RX on GP5 (the module's TX), PPS on GP3, 3.3 V logic, speed from the setting `gps_baud`
   (default 38400). TX on GP4 is reserved and not configured; the clock sends nothing to the module in 0.3.0 and so works with its factory
   configuration. A module that was reconfigured (no RMC, a different time pulse) shows that in the status counters (REQ-GPS-06).
   Receiving is on only while `gps` is in `time_sources`.
2. **NMEA, the RMC sentence only** (`core::gps`, plain code like `core::dcf77`): `$` talker `RMC` fields `*` hex checksum. Valid only
   with status `A` and mode `A`/`D` (not `N`, `E`, `S`), every needed field present and in range, and the date (`ddmmyy`, year 2000 to
   2099). Anything else is counted and ignored (REQ-GPS-01).
3. **The PPS edge is the instant.** u-blox sends the sentence of a second after the pulse that starts it. The sample is `{ unix = the
   second named by RMC, at = the stamp of the last edge }`; a sentence is paired with an edge only if exactly one edge was stamped in
   the 900 ms before its last byte (REQ-GPS-02). Without a PPS wire there is no GPS time: the arrival time of a sentence depends on
   the burst of sentences before it (about 0.2 s at 38400 baud) and on the executor, and a clock that calls a time "GPS" but is
   hundreds of milliseconds off would be worse than NTP.
4. **Settling.** 30 consecutive seconds, each with a valid RMC, one second after the previous one, and PPS edges 1000 ms ± 10 ms apart,
   before the first offer (REQ-GPS-03). It guards against GPS time that is not yet UTC after a cold start (18 s), against a noisy PPS line
   and against a receiver that skips seconds. The number 30 and the 10 ms are assumptions to be set by the first measurements.
5. **Offer rate and freshness.** One offer per 60 s while settled, fresh for 10 minutes (REQ-GPS-04): the selector, the log and the
   settings copy are not meant for 1 Hz (B10, B16).
6. **Stamping the edge.** A task on a second `InterruptExecutor` (SWI_IRQ_0, priority P1; the display keeps P2 on SWI_IRQ_1, the main
   executor is thread mode) waits for the edge and reads the microsecond timer at once, then reads the UART. Accuracy is the interrupt
   latency of the GPIO bank interrupt plus one preemption by the display's row scan (tens of microseconds), assumed, not measured
   (REQ-GPS-07). The UART is read with a buffered driver in the same executor.
7. **Keep rule and date floor.** The DCF77-only `DCF_KEEP_MS` becomes a table (`Source::keep_ms`: NTP 0, DCF77 300, GPS 50) plus a
   minimum of 10 s between RTC writes of a source other than NTP (unless the clock is more than 1 s off) (REQ-SRC-08). The lower date bound
   of `plausible_unix` moves from 2020 to a floor that the maintainer raises with each release, now 2026-10-01 (REQ-SRC-10): no time
   before the firmware was built is true, which stops a repeated GPS week (rollover) that no other source contradicts.
8. **Confirmation by a different source.** REQ-SRC-07 replaces REQ-SRC-06: a far-away sample needs the second reading of the same source
   **and** must not be contradicted by any other selected source with a fresh sample. Where no other source delivers nothing can
   contradict it; the date floor is the remaining defence. REQ-SRC-06 is retired when REQ-SRC-07 is implemented, in the same change.
9. **Three sources everywhere.** `Source::Gps` is appended (it is also the vote enumeration order); `fresh_for_ms` GPS 10 min; the vote
   power key `vote_gps`; `gps_baud`; the arrays and texts that were written for two sources are generalised and a test fails if
   `SOURCES` grows without them (REQ-GPS-05, 06). The vote tie rule: groups of equal power and size are decided by the best-ranked member
   of `time_sources` (REQ-SRC-09).
10. **What can be tested without the module.** The decoder and the pairing/settling logic in `core` with sentences built in the tests
    (checksums computed by the test, not copied); the selector with three sources; the config and the status text. The firmware task is
    exercised in a **test build** (`--features gps-sim`, never in a release) that feeds generated sentences and a software PPS into the
    same channel: it proves the wiring, the selection and the display of three sources, not the electrical side or the timing.

## Alternatives considered

### A. UBX binary (NAV-PVT, NAV-TIMEUTC, TIM-TP) with a configuration sent by the clock — REJECTED for 0.3.0 (assumed)
UBX-TIM-TP names the *next* pulse and the quantisation error, and NAV-TIMEUTC carries a UTC-valid flag, which is more precise than
RMC. It needs the clock to write configuration to the module (and to know its factory state), which cannot be tried without the
module and can leave a module configured in a way that the next owner of the clock does not expect. Rejected on reasoning, not built;
revisit when the module is on the bench.

### B. Poll the PPS edge in the main executor like the DCF77 pulses — REJECTED (assumed)
ADR-0004 point 7 accepts a few milliseconds of wake-up jitter for DCF77. A GPS edge is meant to give the clock's sub-second phase;
the NTS key exchange blocks the main executor for up to 7 s on the RP2040 and the edge would be stamped after it. Not measured.

### C. Capture the edge with PIO or a hardware timer input — REJECTED for now (assumed)
Exact, but PIO cannot read the system timer and the RP2040/RP2350 timers have no input capture; a PWM counter measures intervals,
not the instant. The interrupt executor is expected to be within a millisecond; if the measurement of REQ-GPS-07 says otherwise this is
the next step.

### D. Take the time from the sentence without PPS — REJECTED (assumed)
See point 3.

### E. The I2C (DDC) port of the module on the RTC's bus — REJECTED (assumed)
The bus is shared with the RTC and the clock's write is timed to the second boundary; a module that stretches the clock on a read would
delay it. UART1 is free.

### F. Offer every second to the selector — REJECTED (assumed)
See point 5.

## Consequences

- The delivery as 0.3.0 needs hardware records for: the module's factory output (RMC present, PPS 1 Hz with fix, pulse polarity and
  width), the baud rate, the jitter of the stamp (REQ-GPS-07: within 1 ms), the settle time from a cold start and the behaviour at a lost
  fix (indoors), the offset between GPS and NTP (to set the keep limit), and the three-source vote on the clock. Until then the
  requirements stay `draft`, the version stays 0.2.x, and the work lives on the branch `feat/gps` (merged to main in parts only where it
  changes nothing for a clock without GPS).
- A clock without the module and without `gps` in `time_sources` behaves as before; the date floor (point 7) and the tie rule are the
  only changes it sees.
- The NTP server still claims its fixed high stratum (`core/src/sntp.rs`); a lower stratum with a settled GPS is a possible later change
  and is not part of 0.3.0.
- If the PPS wire is missing the source never delivers; `status` says "no PPS edges" (REQ-GPS-06), so the failure is visible.
