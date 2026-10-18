---
project: picoclock
doc: docs/design/adr/0005-dcf77-receiver-delay.md
status: ledger
last_updated: 2026-10-18
---

# 0005. DCF77 receiver delay: manual and learned

## Status

Accepted (requested by the owner on 2026-10-18, after the first receiver was wired; REQ-DCF-04). It adds to ADR-0004 and
changes none of its decisions.

## Context

The first DCF77 receiver delivered times that were behind NTP: measured against the NTP-set clock, 54 to 75 ms behind over
about 20 fixes (hardware, Pico 2 W, 2026-10-18). The delay is in the receiver (filters), the signal's travel time (about 2 to
5 ms) and the wake-up of the firmware. It differs from receiver to receiver, so it cannot be a constant in the code.

## Decision

1. **Setting `dcf77_delay`: `auto` (default) or 0 to 500 ms.** The pulse is taken to have begun that many ms before it was seen
   (`dcf77::corrected_ms`), which moves the whole DCF77 time forward by the delay.
2. **Auto mode learns the delay against NTP.** When a confirmed DCF77 time arrives and an NTP time is at most 5 minutes old, the
   reading is `delay in use - (DCF77 time - NTP time)`, in ms; the answer is the median of the last up to 9 readings, given
   from the 4th on, readings more than a second from zero are dropped, the result is clamped to 0 to 500 ms
   (`dcf77::DelayEstimator`). No NTP time, no learning: the stored value stays in use.
3. **The learned value is stored (`dcf77_learned`)** when it differs by 5 ms or more from the stored one and the last write is
   10 minutes old, through the normal save path for background changes (it does not write unsaved terminal edits).
4. **Reading against the NTP sample, not the clock.** Both are projected with the same monotonic timer, so their difference does
   not depend on when the RTC was last written.

## Alternatives considered

### A. A fixed default of 55 ms — REJECTED (measured on one receiver)
Individual fixes scatter between 41 and 82 ms (hardware, 2026-10-18), and another receiver or antenna position gives another
delay. A default would be right for one setup only.

### B. Learn against the RTC or the clock — REJECTED (assumed)
After a DCF77-only start the clock is the DCF77 time itself; learning against it would learn nothing. NTP is the independent
reference, and its error (tens of ms with the gateway's NTP, less with NTS) is spread by the median.

### C. Learn from an NTP time of any age — REJECTED (assumed)
The monotonic timer drifts (about 30 ppm assumed, 0.1 s in an hour), so an hour-old NTP time is not a reference at the
accuracy wanted. Five minutes limits that to about 10 ms.

### D. Mean instead of median — REJECTED (assumed)
One bad NTP exchange or a late wake-up would move a mean; the first run showed readings of 41 to 82 ms for a delay that is
probably about 55.

## Consequences

- Learning needs NTP to be selected and reachable for some time after each start; a DCF77-only clock keeps the last learned
  value.
- Fixes arrive about every minute in good reception, so the first answer takes at least 4 minutes after an NTP sync.
- The estimate is only as good as the NTP reference (gateway NTP is plain; an `nts:` server is better).
