---
project: picoclock
doc: docs/design/reviews/2026-10-21-gps-code-review.md
status: ledger
last_updated: 2026-10-21
---

# Review of the GPS code (2026-10-21)

One Fable reader, read-only, on `main` at `5d203b0` (the decoder `core/src/gps.rs`, the tasks `firmware/src/gps.rs`, the selector, the settings). Its report is model
output; each finding was checked against the code by the author. Two of them (1, 3) the author had found by reading the same file before the report arrived.

| # | Finding | Disposition |
|---|---|---|
| 1 | A change of `gps_baud`, or `time_sources` without `gps`, was never applied while the module talks: the I/O task's one-second timer was made anew on every pass | **fixed**: fixed deadline. Not seen on hardware (needs a talking module; measurement plan step 10) |
| 2 | The GPS executor ran at P1, above the display's P2 (a lower number is more urgent); the ADR said the opposite | **fixed**: P3 |
| 3 | A stall of the main executor of 3 s or more made the task declare the receiver silent and restart the settling (the missed ticks ran off at once) | **fixed**: the next check is a second from now. Not seen on hardware |
| 4 | A sentence that ends 1.0 to 1.9 s after its own pulse is tied to the next pulse and sets the clock 1 s slow, undetected | **documented**: not expected with the NEO-M9N (RMC first, short burst); measurement plan step 1 records the delay |
| 5 | ADR point 4 claimed that settling guards against GPS time that is not UTC yet (18 s); it cannot | **fixed** (ADR text); plan step 5 compares the first offer with NTP |
| 6 | A leap second costs a full new settling (30 s), not one second | **accepted**, written in the ADR |
| 7 | Events lost from a full queue were counted as serial errors, which the status blames on `gps_baud` | **fixed**: logged, not counted |
| 8 | The queue's margin was 2 events for a 7 s stall | **fixed**: 24 events |
| - | (author, not the reviewer) an error of the serial line that repeats at once could hold the interrupt executor | **fixed**: yield after an error |
| - | (found by running the real task, VR-86) GP3 carries the DS3231's 1 Hz square wave | **fixed**: the PPS input is GP8 |

Found sound by the reviewer: every slice of `parse_rmc` is length-checked, the edge ring cannot index out of range, the selector keeps a held-back time out of the vote, a clock without GPS changes only by the date floor and two pins, the I/O task touches no thread-mode mutex.
