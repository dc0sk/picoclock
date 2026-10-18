# Changelog

All notable changes to picoclock. Format after [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), dates ISO 8601,
versions follow [Semantic Versioning](https://semver.org/) (a new time source or feature set raises the minor number).

Settings saved by an older version keep working; new settings start at their defaults.

## [Unreleased]

### Planned
- Signed firmware install from the web and terminal (ADR-0006, step 2): bootloader, Ed25519 signature, rollback protection.
- u-blox GPS as a third time source (0.3.0), reviewed before it is built.

## [0.2.0] - 2026-10-18

Second time source (DCF77 radio), a choice between time sources, HAM mode, a web interface in six pages and a firmware
update check. The default behaviour is unchanged: NTP only, Pico-Clock-Green style display.

### Added
- **Time source selection.** `time_sources` lists the sources best first (`ntp`, `dcf77`; default `ntp`).
  `time_mode order` takes the first source that is fresh (NTP 3 h, DCF77 15 min).
  `time_mode vote` accepts the time that more than half of the delivering vote power agrees on within `vote_tolerance`
  seconds (1–3600, default 2); a tie leaves the clock alone. `vote_ntp` and `vote_dcf77` (0–9, default 1) set the power.
  `status` shows the state of every source. (REQ-SRC-01..05, ADR-0004)
- **DCF77 receiver** (Pollin DCF1 or the same-pinout module): PON on GP0, TCO on GP1. The decoder checks the parity
  bits, the calendar and the weekday and sets the clock only after two consecutive good minutes. The receiver is
  powered only while `dcf77` is selected. Works without NTP, so a clock without WiFi can run on radio alone.
  (REQ-DCF-01..04, VR-59)
- **Receiver delay.** `dcf77_delay auto` (default) learns the delay against NTP (median of at least four readings that
  are at most 5 minutes old); `dcf77_delay 0..500` sets it by hand. Measured about 55–75 ms behind NTP. (ADR-0005)
- **HAM mode.** `ham_mode on` turns the hourly chime into the time in Morse code (HHMM); holding UP or DOWN for 1.5 s
  sends it on demand. `morse_wpm` 5–40, default 18. Off by default. (REQ-HAM-01/02, VR-67)
- **Web interface in six pages:** Status, WiFi, Time, Clock, Web, Security. Each page has its own form and Save button;
  no JavaScript, each page below 6.5 KB. A link to the timezone list sits under the timezone field. (REQ-WEB-11)
- **Firmware update check.** Once a day (first about 3 minutes after joining WiFi) and on request (`update`,
  `update check`, web button) the clock asks GitHub for the latest release over TLS 1.3, trusting only the pinned
  USERTrust ECC root. It reports the result in `status` and on the Status page and never downloads or installs anything.
  `update_check on|off`, default on. (REQ-UPD-01..03, ADR-0006)
- **Network evidence in `status`:** DHCP server, prefix and DNS, a note when the lease changes while connected, link
  strength and the access points that carry the SSID (closest marked). (REQ-NET-10)
- **Settings kept twice.** Two flash sectors with a backup copy, so a power cut during a save loses nothing.
  (REQ-CFG-10)
- **Watchdog** (8 s). A hung clock restarts; `status` shows `reset:` after a watchdog restart. (REQ-NFR-03)
- Documentation: hardware table, screenshots of all web pages and the terminal, wiring plan for DCF77 and GPS,
  ADR-0004/0005/0006, 96 requirements with a generated traceability matrix.

### Changed
- **The colon between hours and minutes no longer blinks by default**, because blinking can add radio noise.
  `colon_blink on` brings it back (it then blinks while the clock is synced). (REQ-UI-08, replaces REQ-UI-07)
- **Every web form answers with a redirect** and shows its message once, so reloading a page never resends a form or
  starts a second update check. The Status page refreshes by itself while an update check runs.
- Saving from the web or the buttons writes only that change; unsaved terminal edits are not stored by accident.
  (`Config::carry_changes`)
- A DCF77 time renews the sync state; the RTC is rewritten only if the clock is more than 300 ms off.
- Factory reset over plain HTTP is refused while HTTPS is on. (REQ-WEB-09)

### Fixed
- Status block of the web page was cut off at 512 bytes.
- HTTP header names containing spaces were accepted.
- NTP replies with stratum 16 (unsynchronised server) were accepted.
- DS3231: 12-hour mode and out-of-range register values were mishandled; the RTC woke late in rare cases.
- A rare panic while printing to the terminal.
- A certificate chain of about 3 KB could not be uploaded; the request limit is now 8 KiB. (REQ-WEB-10)
- Pico W: the random number source is conditioned before use. (REQ-RNG-01)
- The NTP server answers only after the first sync (documented behaviour; it used to answer with an unset clock).

### Known issues
- About every second boot, the first WiFi join attempt fails and the second one succeeds, mostly when the clock picks
  the weaker of two meshed access points. Cause not found; WPA2-only did not help.
- Twice in about 100 boots the clock received 192.168.1.100 from an unidentified DHCP server instead of the router's
  lease. `status` now records the evidence; not reproduced since.
- Not yet tested on hardware: the Pico W build (RP2040) for any 0.2.0 feature, DCF77 reception at night, and the vote
  mode with two live sources that disagree.
- HTTPS, NTS with RSA chains and the update check need the Pico 2 W; the Pico W serves plain HTTP only.

## [0.1.0] - 2026-10-17

First working firmware, written in Rust (embassy) for the Waveshare Pico-Clock-Green with a Pico 2 W (preferred) or
Pico W.

### Added
- WiFi with up to 8 stored networks; the clock joins the strongest known one. Credentials are provisioned from a
  git-ignored file by `tools/provision.py`; the repository holds no secrets.
- NTP client (defaults: the gateway or DHCP option 42, then `pool.ntp.org`); optional NTP server (off by default).
- NTS (RFC 8915) secure time, with RSA and ECDSA certificate chains and user-added trust anchors.
- The internal clock runs on UTC. Time zones with daylight saving rules (`dst`, default on), a UTC-only display mode,
  and the zone looked up from the IP address through several geolocation providers.
- Terminal configuration over USB, and a web interface with HTTPS (uploadable certificate) on the Pico 2 W,
  Basic authentication, mDNS, ping, a changeable host name (default `picoclock-xx`) and the MAC address.
- Integrated DS3231 RTC, buzzer (chime, alarm), ambient light sensor with automatic brightness, three buttons and menu,
  temperature display.
- Factory reset from the terminal and the web, each behind a second confirmation.
- Hardware random number generator on the Pico 2 W; AGPL-3.0-or-later licence; architecture notes, software
  requirements specification and verification log.

[Unreleased]: https://github.com/dc0sk/picoclock/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/dc0sk/picoclock/releases/tag/v0.2.0
[0.1.0]: https://github.com/dc0sk/picoclock/commits/main
