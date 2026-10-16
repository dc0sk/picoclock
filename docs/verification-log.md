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

### VR-19 · 2026-10-16 · hardware · WiFi join robustness after the join timeout and retry change

Before the change, RTT logs of six reboots showed `AuthenticationFailure` on the first attempt at slot 1 in most
boots, followed by a 30 s wait; in one boot the join call had not returned after 40 s and the clock stayed offline
until reset (`status`: `not connected`, `synced: never`). The radio driver waits for an event without a time limit.
After the change (15 s limit per attempt, three attempts per slot, 10 s pause between rounds, 30 s limit for link and
DHCP): eight reboots in a row all connected, and three RTT logs show first-attempt success at 7 s and 16 s and a
first-attempt `AuthenticationFailure` followed by success on attempt 2 three seconds later. Why the access point
rejects some first attempts is not known. Link loss while running was not tested.

Verifies: REQ-NET-01, REQ-NET-06

### VR-20 · 2026-10-16 · hardware · Alarm and menu work (user observation)

The clock's owner reported that the alarm and the settings menu work on the current image. This is the user's word, not
a measurement. It does not say which menu items were changed, whether the alarm was stopped by a button or by its own
timeout, or whether it stayed silent when disabled. The hourly chime and the key-beep setting were not singled out.

Verifies: REQ-UI-04, REQ-UI-05

### VR-21 · 2026-10-16 · hardware · Light sensor and auto-brightness work (user observation)

The clock's owner reported that the light sensor works on the current image. This is the user's word, not a
measurement. It is taken to mean that `brightness auto` follows the room light in the right direction, which would
confirm that a high ADC reading means dark, as copied from the vendor demo. No readings or brightness values were
recorded, so the thresholds (full brightness up to a raw 1500, 15 % at 3500) are not checked beyond "works".

Verifies: REQ-HW-04

### VR-22 · 2026-10-16 · hardware · Display flicker found, measured and fixed

The clock's owner reported LED lines flickering at 1 to 3 Hz, every 1 to 3 seconds. A diagnostic added to `status`
(worst lateness of the row timer and the count of waits more than 100 µs late) showed 256 late waits in 156 s, worst
588 µs, with and without WiFi. At the 15 % duty that auto brightness gave in this room, a 150 µs row on-time that fires
500 µs late is about four times brighter for that row. Cause: the configuration and runtime status sat behind a
critical-section mutex, which switches interrupts off; cloning the 8-slot configuration (twice a second in the network
task) and rendering a frame (four times a second in the UI task) did so for hundreds of microseconds. After moving both
to a thread-mode mutex (they are only used by thread-mode tasks): 1 late wait in 161 s, worst 140 µs. Under load
(30 page loads, three syncs, ten zone listings) still 1 late wait. A settings save still stalls the scan for about
34 ms (flash erase; known). The owner has not yet confirmed by eye that the flicker is gone.

Verifies: REQ-DSP-02

### VR-23 · 2026-10-16 · hardware · Timezone lookup: works against a LAN server, fails against ip-api.com

The owner reported `timezone=auto` works. It could not be reproduced here: in six attempts (RAM-only `auto`, `geo`, up
to 50 s) the real service gave no result. `status` now reports the stage: DNS resolves to the right address
(208.95.112.1), the connection opens and the request is sent, but the data is never acknowledged and the read ends
after the 8 s socket timeout with an error and 0 bytes. The same request sent from the Pi returns the reply in
0.06 s. Pointing a test build at a small HTTP server on the Pi (port 8000) made the lookup succeed on the clock: the
request arrived as expected, the reply was parsed, and the Berlin POSIX rule was cached. So the parsing, caching and
TCP code work; what fails is the path to ip-api.com. Not found: why packets to that server are lost (not tried: a
second external server, a packet capture on the router).

Verifies: REQ-TZ-04

### VR-24 · 2026-10-16 · hardware · Flicker gone (user observation)

After the thread-mode mutex change the clock's owner looked at the display and reported that the flicker is gone. This is
the user's word, not a measurement, and it does not say for how long it was watched or at which brightness. It agrees with
the timing numbers in VR-22.

Verifies: REQ-DSP-02

### VR-25 · 2026-10-16 · hardware · Timezone lookup works through ipwho.is

After adding a provider list (`ipwho.is` first, `ip-api.com` as fallback; both parsers have unit tests with captured
replies): with `timezone auto` and `geo`, the clock cached `CET-1CEST,M3.5.0,M10.5.0/3` within 4 s, `status` showed
`geo : ok via ipwho.is (391 bytes ...)`, and `time` showed local time `UTC+02:00`. A comparison build pointed at
`example.com` had shown that outbound TCP data from the clock works in general (407 bytes received), so the earlier
failure is specific to ip-api.com on this network. The fallback to ip-api.com after a failure of ipwho.is was not
exercised on the hardware. The timezone was set back to Europe/Berlin afterwards.

Verifies: REQ-TZ-04

### VR-26 · 2026-10-16 · hardware · Hourly chime works (user observation)

The clock's owner reported that the hourly chime works on the current image. This is the user's word, not a measurement.
It does not say which hour it was heard at, whether the local time (not UTC) was the one chimed, or that it stayed
silent while an alarm rang. The key-beep setting was not singled out.

Verifies: REQ-UI-06

### VR-27 · 2026-10-16 · hardware · Web page in daily use (user observation)

The clock's owner reported using the web page all the time and that it works. This is the user's word, not a measurement:
it does not say which browser or device, which settings were changed, or whether a web password was set. It covers the
page's look in a browser, which VR-09 (curl only) did not.

Verifies: REQ-WEB-01

### VR-28 · 2026-10-16 · hardware · Fallbacks: NTP servers, WiFi slots, location providers

NTP server order, with the firmware log read over RTT: with `ntp_servers 192.0.2.1,pool.ntp.org` the first server
failed twice (3 s each) and `pool.ntp.org` answered 3 s later and set the RTC; with only `192.0.2.1` there was no
sync and the clock kept its time; with the default `gateway,pool.ntp.org` the gateway answered at once.

WiFi slots (settings changed in RAM only, then restored): with slot 1 holding a nonexistent SSID and the real network
in slot 3, the clock connected on slot 3 in 5 s. With slot 1 the real SSID but a wrong passphrase, while slot 1 was the
active slot, it connected on slot 3 after 37 s: three authentication attempts of about 8 s each are made before the
next slot is tried. A change to a slot that is not in use did not drop the connection.

Location providers, using throwaway builds with broken provider entries (not the shipped code): with a first provider
that does not resolve, the lookup moved on and finished `ok via ipwho.is` in 4 s. With all three entries broken the
status showed `DNS lookup failed`, the cached rule stayed in place, and local time kept following it. Not tested: a
provider that connects but returns a bad reply, and the real ip-api.com entry as a working fallback (it does not work
from this clock on this network).

Verifies: REQ-NTP-01, REQ-NET-01, REQ-NET-03, REQ-TZ-04

### VR-29 · 2026-10-16 · hardware · Crypto cost on the RP2040 (feasibility, not a verification of the requirements)

A throwaway benchmark (`spikes/crypto-bench`, pure-Rust RustCrypto crates at the versions `embedded-tls` 0.19 uses,
opt-level 3, RP2040 at 125 MHz, timed with the 1 MHz timer, one run each, read over RTT): SHA-256 of 4 KiB 4.5 ms;
AES-128-GCM seal of 1 KiB 4.2 ms; AES-SIV-CMAC-256 of an NTS-sized request 1.4 to 1.5 ms; ECDSA P-256 sign 346 to 358 ms
and verify 558 to 580 ms; P-256 ephemeral key 283 to 294 ms and ECDH 283 to 294 ms; X25519 key generation 150 ms and
shared secret 146 ms. Run-to-run spread was about 4 %; one run per primitive, no warm-up effects expected. Handshake
times derived from these are estimates (see ADR-0001 and ADR-0002). The clock ran the benchmark instead of the product
firmware for about two minutes per run and was reflashed afterwards.

Verifies: REQ-HW-08, REQ-WEB-06

### VR-30 · 2026-10-16 · build · rustls without std on both chips (feasibility)

`spikes/rustls-check`: `rustls` 0.23 with `default-features = false` and `rustls-rustcrypto` 0.0.2-alpha builds for
`thumbv8m.main-none-eabihf` (RP2350) once a `getrandom` backend is registered; for `thumbv6m-none-eabi` (RP2040) it fails
in `spin` (no atomic `compare_exchange`), and `rustls` itself uses `alloc::sync::Arc`, unavailable on that target, in
about 30 files. Compile only; nothing was run.

Verifies: REQ-WEB-06

### VR-31 · 2026-10-16 · host · NTS against real servers

`cargo test -p picoclock-core --test nts_live -- --ignored` (internet access needed, so it is not part of the gate):
a complete NTS-KE (TLS 1.3, certificate chain with P-384 intermediates, exporter) and three authenticated NTP
exchanges each with `time.cloudflare.com` and `nts.time.nl`. Both servers returned 8 cookies, answered every request
with a valid authenticator, the time was within a second of this machine's clock, and the cookie store was back at 8
after every exchange. `ptbtime1.ptb.de` (RSA certificate) was refused with a TLS error, as intended.
The test is able to fail: with the client-to-server key derived with the server-to-client context the server answered
every request with an NTS NAK (84 bytes, recognised as a NAK); with a wrong exporter label for the server-to-client key
the replies failed authentication; with four bytes missing from the associated data of a request the server NAKed it.

Verifies: REQ-NTS-01, REQ-NTS-02, REQ-NTS-05, REQ-NTS-06

### VR-32 · 2026-10-16 · host · TLS exporter against OpenSSL, and the deferred certificate checks

`core/tests/tls_exporter_openssl.rs` (part of the gate, needs the `openssl` binary): the client and `openssl s_server
-keymatexport` derived the same 32 bytes from one handshake, the output changes with label and context, and four
deliberate breaks of the derivation (label, hash of the context, the "exp master" label, the HKDF label) were each
caught. The deferred certificate verifier was tested against a three-level chain (root, intermediate, server): the right
root is accepted; another CA's root, a wrong host name, an expired and a not-yet-valid certificate, a chain without its
intermediate, and a corrupted certificate are refused; driven by hand with the server's real key, a genuine
`CertificateVerify` signature is accepted while a signature over another transcript, a corrupted one, one by another key,
an empty and a truncated one are refused. Mutations of the verifier were tried: skipping the chain check, skipping the signature check, remembering the wrong
transcript, and replaying or storing only the first chain entry were all caught (the last two only after the test chain
got an intermediate, because a one-certificate chain cannot show them). Tolerating a missing chain or a missing signature
is not caught by any test, because the later checks fail anyway; there is no observable difference. The test first failed intermittently
because `openssl s_server` quits when its stdin is closed; the harness now keeps a pipe open.

Verifies: REQ-NTS-06, REQ-NTS-07

### VR-33 · 2026-10-16 · host · How long NTS-KE servers wait for the client

Experiment (`core/tests/nts_slow_client.rs`, ignored by default): the certificate check was slowed by sleeping in
the clock callback. With the checks done during the handshake, `time.cloudflare.com` and `nts.time.nl` completed up to
a total stall of 1.5 s and dropped the connection at 2 s, `ntp3.fau.de` completed with stalls of 2.5 s and dropped the connection at 3.5 s;
`nts.teambelgium.net`, `time.txryan.com` and `ntppool1.time.nl` dropped it when the stall totalled 3 s (shorter stalls were
not tried). The clock needs about 5 s (VR-34),
so a normal handshake cannot work against most servers. With the checks deferred until after the key exchange, all four
servers tested completed with a 5 s check. An earlier run of this experiment was wrong (the first clock call fell before
the handshake and delayed an idle connection) and was repeated with that call excluded.

Verifies: REQ-NTS-07

### VR-34 · 2026-10-16 · hardware · NTS on the clock

With the certificate checks done inside the handshake, the key exchange with Cloudflare failed twice with a connection
reset after 4.7 s (and again with the crypto crates built for speed: 4.72 s and 4.76 s, so CPU optimisation did not help).
With the checks deferred: `nts:time.cloudflare.com` key exchange 4977 ms and 5042 ms (two runs), 8 cookies, an
authenticated reply, `status` showing `synced : 5 s ago (NTS, authenticated)`; a second sync reused the cookies without a
new key exchange (8 left again). `nts:nts.time.nl` took 6.8 to 6.9 s. `nts:ptbtime1.ptb.de` (RSA) failed after about
0.4 s with both roots; alone in the list no new sync happened and the sync age kept growing; followed by
`pool.ntp.org` the clock synced by plain NTP and `status` said `(plain NTP)`. During a key exchange the other tasks wait
(the display stays lit; the clock, console and web page pause for 5 to 7 s), which happens once per boot. Entries with
invalid hosts (`nts:192.168.1.1`) are refused by `set`.

Verifies: REQ-HW-08, REQ-NTS-01, REQ-NTS-03, REQ-NTS-04, REQ-NTS-05

### VR-35 · 2026-10-16 · hardware · Terminal edits are not saved until `save`; correction of earlier entries

While testing NTS, `ntp_servers` was found stored in flash with a value that had only been `set` on the terminal: the
terminal answered "not saved yet; 'save' to keep" but a debounced save fired three seconds after every edit (a call to
`mark_changed(false)`). After the fix: `set` then an 8 s wait and a reboot restored the stored value; `set` then `save`
and a reboot kept the new value; `status` mentions unsaved changes in between.
**Correction:** VR-07, VR-15 and VR-28 describe settings changed "in RAM only". Because of this fault those changes were in
fact saved about three seconds after each edit. Every test restored the values by hand and later `show` listings
confirmed the final state, so no result changes, but the description was wrong.

Verifies: REQ-CFG-05

### VR-36 · 2026-10-16 · build · Size of the NTS build

Pico W: 658,372 bytes of code, 115,980 bytes of static RAM (before NTS: about 451 KB and 74 KB; the TLS client with
P-256/P-384 verification, AES-GCM, SHA-2 and the deferred-verification buffers). Pico 2 W: 656,012 bytes and 116,380
bytes. Both build; the NTS code has not run on a Pico 2 W.

Verifies: REQ-NFR-01
