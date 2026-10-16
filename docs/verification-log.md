---
project: picoclock
doc: docs/verification-log.md
status: ledger
last_updated: 2026-10-16
---

# Verification log

Append-only record of verification runs that are not unit tests: what was run, on what, what was observed.
Entries are history and are never edited afterwards; a later run gets a new entry. The traceability matrix reads the
`Verifies:` line of each entry (format below) and shows the tier. An old result does not vouch for code changed since.

Tiers: **hardware** = ran on the real clock (Pico W in a Pico-Clock-Green, flashed through a Raspberry Pi Debug
Probe, host: Raspberry Pi 5); **host** = ran on the development machine; **build** = compile and link output.

Entry format (checked by `tools/trace.py`): a heading `### VR-nn · date · tier · title`, then a line
`Verifies: REQ-..., REQ-...`.

Commit note: the hardware runs below were made on the working tree that was committed as `be48d39`, partly on earlier
states of it. After later changes only the Pico W boot, WiFi join, NTP sync and `status` output were re-run
(VR-11). Everything else has not been repeated since the build-script change in `d367242`.

### VR-01 · 2026-10-16 · hardware · First flash, boot and console

Flashed the release image with OpenOCD over CMSIS-DAP (verify OK, reset). The board enumerated as
`1209:0001 picoclock_console`; `help`, `status`, `time` and `show` answered. The DS3231 held a valid UTC time.

Verifies: REQ-HW-05, REQ-TIME-01, REQ-CLI-01

### VR-02 · 2026-10-16 · hardware · Console commands and temperature sensors

`set`, `get`, `tz Europe/Ber`, `save` and a refused value (`set timezone bogus` printed the accepted forms).
`status` showed the DS3231 at 22.7 °C. The RP2040 sensor first read 636.4 °C (a 32-bit overflow in the conversion);
after the fix it read 27.1 °C, 4 °C above the DS3231, as expected for a die sensor.

Verifies: REQ-CLI-01, REQ-HW-01, REQ-HW-02, REQ-TZ-03

### VR-03 · 2026-10-16 · hardware · Settings survive a reboot

`set timezone Europe/Berlin`, `set hour12 on`, `set brightness 60`, `save`, `reboot`. Afterwards `time` showed
`10:07:21 (UTC+02:00)` against 08:07:21 UTC (Berlin on summer time), and the other values were unchanged. Later,
settings written by the 1 KiB-record firmware loaded correctly into the 2 KiB-record firmware (the 8-slot change).

Verifies: REQ-CFG-02, REQ-CFG-01, REQ-TZ-01

### VR-04 · 2026-10-16 · hardware · WiFi join and NTP sync

Credentials set from a git-ignored file through `tools/provision.py`. The clock joined, got an address, and `status`
showed `synced : 16 s ago`. The RTC had been one hour wrong and showed the correct UTC afterwards. `sync` on the
console triggered an immediate query.

Verifies: REQ-NTP-01, REQ-NTP-03, REQ-NET-01

### VR-05 · 2026-10-16 · hardware · NTP server accuracy

`ntp_serve on`; a Python SNTP client on the Pi (itself NTP-synchronised) queried the clock six times. Before the
change that sets the RTC on a second boundary the offset was −0.75 s to −1.65 s. After it: −9 ms, then +1 to +2 ms.
Measured right after a sync only; the drift towards the next hourly sync was not measured (crystal tolerance suggests
up to about 0.1 s).

Verifies: REQ-SRV-04, REQ-TIME-04

### VR-06 · 2026-10-16 · hardware · NTP server on, off and abuse

With the setting off a query timed out. With it on, replies had LI 0, version 4, mode 4, stratum 5, 48 bytes, and
echoed the client timestamp. A 10-byte packet, an empty packet, a server-mode packet, random bytes and a 1400-byte
packet got no reply. After `set ntp_serve off` queries timed out again.

Verifies: REQ-SRV-01, REQ-SRV-02, REQ-SRV-03

### VR-07 · 2026-10-16 · hardware · Eight slots, failover and config rejoin

Slot 1 set to a nonexistent SSID and the real network to slot 2: the clock reported `connected (slot 2)` within
10 s. Restoring slot 1 made it rejoin on slot 1 without a reboot. The web form showed 16 WiFi fields and a POST to
slot 8 saved. Link loss (access point switched off) was **not** tested.

Verifies: REQ-NET-01, REQ-NET-03

### VR-08 · 2026-10-16 · hardware · Network name

The router's DNS resolved `picoclock-54.fritz.box` to the clock's address; `status` showed `name : picoclock-54`.

Verifies: REQ-NET-02

### VR-09 · 2026-10-16 · hardware · Web page over HTTP

From the Pi with `curl`: `GET /` 200 (5068 bytes, 16 WiFi fields and the other settings), `GET /zones` the zone list,
unknown path 404, `POST /save` with a foreign `Origin` 403, `POST /save` without `Origin` 200 (accepted; no web
password was set). Not tried: a browser, a web password, the layout on a phone.

Verifies: REQ-WEB-01, REQ-WEB-03

### VR-10 · 2026-10-16 · host · Mutation checks

Bugs planted one at a time and each caught by a failing unit test, sources restored afterwards: DST boundary, DST
end offset, alarm retrigger, alarm stop, duplicate Content-Length, CSRF check, config CRC, SNTP origin check (earlier
session); SNTP server mode check, originate-timestamp echo, unsynchronised marking (this session).

Verifies: REQ-TZ-01, REQ-UI-05, REQ-WEB-02, REQ-WEB-03, REQ-CFG-02, REQ-NTP-02, REQ-SRV-02, REQ-SRV-03

### VR-11 · 2026-10-16 · hardware · Pico W regression after the build restructure

After the chip-feature and build-script change (`d367242`), the Pico W image was flashed again: the clock booted,
joined slot 1, showed its name and synced. The RTC second-boundary change and display were not re-checked.

Verifies: REQ-HW-05, REQ-NET-01

### VR-12 · 2026-10-16 · build · Image size and layout, both chips

Release builds for `thumbv6m-none-eabi` (Pico W: about 441 KiB code, 72 KiB static RAM) and
`thumbv8m.main-none-eabihf` (Pico 2 W: about 440 KiB, 73 KiB). In the RP2350 image the boot block is at
`0x10000114` (inside the first 4 KiB) and the program ends below the settings sector; `elf2uf2-rs` produced a UF2
with the RP2350 family ID. This does not show that the RP2350 image runs.

Verifies: REQ-NFR-01

### VR-13 · 2026-10-16 · host · Real-server format checks

The live SNTP test (`cargo test ... -- --ignored`) queried pool.ntp.org and passed. A request to ip-api.com returned
`{"status":"success","timezone":"Europe/Berlin"}` with HTTP/1.1 keep-alive, which is why the client stops reading
once the body parses; the captured reply is a unit-test fixture.

Verifies: REQ-NTP-01, REQ-TZ-04

### VR-14 · 2026-10-16 · hardware · NTP server accuracy after the boundary logic moved to the core crate

Same method as VR-05 with the image that calls `next_second_boundary` from the core crate: five queries 43 s after a
sync gave offsets of +1, +1, +1, +1 and −5 ms against the NTP-synchronised Pi.

Verifies: REQ-TIME-04, REQ-SRV-04

### VR-15 · 2026-10-16 · hardware · Web password

With `web_password` set (in RAM, not saved): no credentials 401 with `WWW-Authenticate: Basic realm="picoclock"`;
wrong password 401; wrong user 401; `admin` with the right password 200; `POST /save` without credentials 401; the
password does not appear in the page. After clearing it the page answered 200 again. Over plain HTTP, so the password
crosses the LAN unencrypted (stated in the README).

Verifies: REQ-WEB-04, REQ-CFG-03

### VR-16 · 2026-10-16 · hardware · Display looks right (user observation)

The clock's owner looked at the display running the current image and reported that it looks fine. This is a
visual check by the user, not a measurement: no photo was taken, and it does not say which views (time, date,
temperature), brightness levels or the 12-hour setting were looked at. Flicker while WiFi is busy and the status
icons were not specifically reported on.

Verifies: REQ-DSP-01

### VR-17 · 2026-10-16 · hardware · Buttons and buzzer work (user observation)

The clock's owner reported that the buttons and the buzzer work on the current image. This is the user's word, not a
measurement. It does not say which presses were tried: the long-press menu, the alarm ringing and stopping, the
hourly chime and the key beep setting were not specifically reported. It does settle that the buzzer is the active
type the code drives (high = sound), which had been an open question.

Verifies: REQ-UI-01, REQ-UI-03, REQ-HW-03

### VR-18 · 2026-10-16 · hardware · Daylight-saving option

Berlin in October: `dst on` gave `UTC+02:00`, `dst off` gave `UTC+01:00`. For `Europe/Dublin` (negative-DST rule)
`dst off` gave UTC (GMT, its standard time) and `dst on` gave UTC+1. `set dst maybe` was refused. Through the web page,
the settings form showed the `dst` field, and `POST dst=off` changed the time to UTC+01:00 and `dst=on` restored it;
both were saved to flash. The display and the hour-by-hour behaviour over a real DST change were not observed.

Verifies: REQ-TZ-05
