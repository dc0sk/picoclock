---
project: picoclock
doc: docs/design/gps-measurement-plan.md
status: design
last_updated: 2026-10-21
---

# GPS: what to measure when the module is connected

ADR-0007 and the draft requirements REQ-GPS-01 to 07 and REQ-SRC-07 to 10 are built and tested on the host and with the simulated
receiver (VR-84, VR-85). Nothing has seen a real receiver. This plan lists, in the order to do them, what has to be seen and recorded
(as VR entries in `docs/verification-log.md`, tier hardware) before the draft requirements are ratified and software 0.3.0 is cut.
A step that fails is a finding: write it down, fix the cause, and repeat the step; do not adjust the limits to fit the result.

**Setup.** The module's TX to GP5, PPS to GP3, GND, 3.3 V from the 3V3 pin (README, *GPS*). The module needs a view of the sky for
the fix (a window is rarely enough; the first fix of a cold NEO-M9N can take minutes). The Debug Probe gives the `gps:` log lines over RTT (README *Debugging*). The firmware does not log the stamps
of the edges; for step 6 add a temporary `info!("gps edge {}", at)` to `gps_task` in `firmware/src/gps.rs` (a measuring aid, not to be committed). Use the normal release build for everything except steps 1 and 3.

## Order

| # | Question | How | Pass when | Requirement |
|---|----------|-----|-----------|-------------|
| 1 | What does the module send as shipped? | Read GP5 with a USB-serial adapter and a terminal at 38400 baud, before connecting it to the clock | NMEA sentences once a second including `RMC`; the baud and the talker prefix (`GN`, `GP`) written down; a time pulse on PPS only with a fix (or always: write down which) | REQ-GPS-01 |
| 2 | Does the clock receive it? | `set time_sources ntp,gps`, `status` | `gps  : silent` becomes `settling n/30` within seconds; `bytes` grows; `serial errors` stays 0 | REQ-GPS-01, 06 |
| 3 | Which edge is the second? | Scope or logic analyser on PPS and on the module's TX, or (without one) RTT: for 60 s log the stamp of each edge and compare the interval | rising edge starts the second (the sentence for the second arrives after the edge); edge interval 1 s ± 10 ms; pulse width written down | REQ-GPS-02 |
| 4 | Is the wiring without a fix handled? | Module indoors with no fix | sentences with status V or mode N are counted as `no fix`, the source never settles, nothing is offered | REQ-GPS-03 |
| 5 | Does it settle, and how long after the fix? | Take the module outdoors (or to a window), start the clock | `settled` 30 s after the first good sentence; time to the first fix written down; `time offered` log line once a minute afterwards | REQ-GPS-04 |
| 6 | How close is the stamp? (the open question of REQ-GPS-07) | 30 minutes settled: log each edge stamp. The interval error against the clock's own crystal is a line (drift); the scatter around it is the jitter. Then repeat while an NTS key exchange blocks the main executor (`set ntp_servers nts:time.cloudflare.com`, `sync`) | scatter of the stamp within 1 ms (target: tens of µs) also during the NTS stall; no `settling` restart caused by the stall | REQ-GPS-07 |
| 7 | How far is GPS from NTP? | `gps,ntp` in mode `order`, then `ntp,gps`; read the offset in `status` for an hour | the typical offset (NTP is only as good as the network, 1 to 10 ms) decides whether the 50 ms keep limit is right; write the numbers down, change the limit only through a new requirement | REQ-SRC-08 |
| 8 | Is a lost fix handled? | Cover the antenna (a metal tin) for 2 minutes, uncover it | `settled` goes back to `settling` after the first bad second (or `silent` if the module stops), the 10-minute freshness runs out, the source falls back to NTP, and 30 good seconds are needed again | REQ-GPS-03, 04, REQ-SRC-03 |
| 9 | Cable pull | Pull the TX wire, then the PPS wire, for 10 s each | TX pulled: `silent` in 3 s. PPS pulled: sentences arrive but nothing is offered (`unpaired` grows) | REQ-GPS-02, 06 |
| 10 | Does a baud change take effect? | `set gps_baud 9600` while the module sends at 38400, then back | the clock follows within a second or two (`bytes` stops growing sensibly at the wrong speed, then recovers); `set time_sources ntp` switches the receiver off | REQ-GPS-01 |
| 11 | Three sources in vote mode | `set time_sources ntp,dcf77,gps`, `time_mode vote`, with the DCF77 module connected | the groups form as in VR-85; a DCF77 time that disagrees is outvoted | REQ-SRC-03, 09 |
| 12 | Long run | 48 hours on `order` with GPS first | no restart (`status` has no `reset` line), the RTC is never rewritten more than every 10 s, flash writes stay at the usual number | REQ-NFR-03 |

## After the measurements

1. Record each step as a VR entry with the numbers (the `Verifies:` line names the requirements it covers).
2. Ratify REQ-GPS-01 to 07 and REQ-SRC-07 to 10; retire REQ-SRC-06 in favour of REQ-SRC-07 and re-point its tests (a ratified statement is never edited).
3. Set ADR-0007 to Accepted, with the measured numbers in its consequences.
4. Bump the version to 0.3.0, raise `MIN_UNIX` to the release date, move the README section *GPS (in development)* into *Time sources*, run `tools/release.sh`.
