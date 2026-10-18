---
project: picoclock
doc: docs/verification-log.md
status: ledger
last_updated: 2026-10-17
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

Verifies: REQ-NTS-06, REQ-NTS-11
(Re-pointed from REQ-NTS-07 to its successor REQ-NTS-11 on 2026-10-17; the behaviour recorded here is unchanged.)

### VR-33 · 2026-10-16 · host · How long NTS-KE servers wait for the client

Experiment (`core/tests/nts_slow_client.rs`, ignored by default): the certificate check was slowed by sleeping in
the clock callback. With the checks done during the handshake, `time.cloudflare.com` and `nts.time.nl` completed up to
a total stall of 1.5 s and dropped the connection at 2 s, `ntp3.fau.de` completed with stalls of 2.5 s and dropped the connection at 3.5 s;
`nts.teambelgium.net`, `time.txryan.com` and `ntppool1.time.nl` dropped it when the stall totalled 3 s (shorter stalls were
not tried). The clock needs about 5 s (VR-34),
so a normal handshake cannot work against most servers. With the checks deferred until after the key exchange, all four
servers tested completed with a 5 s check. An earlier run of this experiment was wrong (the first clock call fell before
the handshake and delayed an idle connection) and was repeated with that call excluded.

Verifies: REQ-NTS-11
(Re-pointed from REQ-NTS-07 to its successor REQ-NTS-11 on 2026-10-17; the behaviour recorded here is unchanged.)

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

### VR-37 · 2026-10-16 · host · rustls with the RustCrypto provider as a TLS 1.3 server (feasibility for ADR-0002)

`spikes/rustls-server-host`: `rustls` 0.23 (std API, TLS 1.3 only) with `rustls-rustcrypto` 0.0.2-alpha, an ECDSA
P-256 certificate with a subject alternative name, on localhost. `curl` with default settings connected (X25519,
`TLS13_AES_256_GCM_SHA384`), verified the certificate and received the page; `openssl s_client` connected with
`-groups X25519` and with `-groups P-256` (secp256r1). A client that offered only the post-quantum hybrid group
`X25519MLKEM768` was refused (`NoKxGroupsInCommon`), as expected; browsers also offer plain X25519. Handshakes took 2 to 6 ms
on this PC. Not done: a browser, the unbuffered `no_std` API that the device would use, any run on an RP2350. The server
preferred AES-256-GCM/SHA-384; the device build should restrict the suites to AES-128-GCM/SHA-256 (cheaper on this CPU).

Verifies: REQ-WEB-06

### VR-38 · 2026-10-16 · host · The device's certificate code against OpenSSL

`core/tests/x509_openssl.rs` (part of the gate, needs the `openssl` binary) and the unit tests in `core/src/x509.rs`:
OpenSSL reads the generated certificate (version 3, ecdsa-with-SHA256, P-256, CA:FALSE critical, Digital Signature critical,
TLS Web Server Authentication, the three DNS names, validity exactly from one day before to ten years after the given
time, serial positive with a nonzero first byte), verifies its self-signature (`-check_ss_sig`), accepts it for the
listed names and refuses another name, a time before its start and a time after its end, and prints the same SHA-256
fingerprint as ours. Flipping any one of every seventh byte makes OpenSSL refuse it. Uploaded material made by OpenSSL:
the same key in PEM and DER, PKCS#8 and SEC1 gives the same secret; a certificate is accepted with its own key and refused
with another; a P-384 certificate or key is refused as unsupported; garbage, an empty input and a certificate given as a
key are refused; a PEM chain keeps order and lengths and a fifth certificate is refused. Six mutations of the generator
(validity one day short, key usage or extended key usage dropped, serial sign bit kept, signature over the wrong bytes,
alternative name with the wrong tag) were each caught. A flaw in the first version of the test was found and fixed:
`openssl verify` does not check the signature of a self-signed trust anchor unless `-check_ss_sig` is given, so neither the
positive check nor the tamper check had tested our signature. Not done: the certificate has not been served by a browser
or run on a device.

Verifies: REQ-WEB-07

### VR-39 · 2026-10-16 · hardware · The Pico 2 W (RP2350) runs the firmware

A Pico 2 W, fitted in the clock board (the DS3231 answered with a valid time and temperature), flashed over USB with no
debug probe attached. The UF2 from `elf2uf2-rs` was ignored: both a plain copy and `elf2uf2-rs -d` left the board in
BOOTSEL with no sign of an error, because the file carries the RP2040 family ID (see VR-40). With the family set to RP2350
Arm Secure the board rebooted at once and enumerated as `1209:0001`. From the console, then: `status` shows the name
`picoclock-d6` (so the radio chip answered and its MAC was read), WiFi joined with the provisioned credentials, plain NTP
synced; the DS3231 and the die temperature read plausibly; the settings were saved to flash and a reboot loaded them again
(the last sector of the 4 MB flash); the web page answered 200 (5662 bytes) and refused a cross-site POST with 403; with
`ntp_serve on` three queries from a PC returned stratum 5, an echoed timestamp and an offset within 1 ms, and none after
`off`; `timezone auto` resolved through `ipwho.is`; `dst off`/`on` gave UTC+01:00/+02:00; the display scan's worst
lateness was 81 µs with no wait over 100 µs in 74 s including 40 page loads and 10 zone-list downloads, and 34 ms during a
settings save (the flash erase, as on the Pico W). NTS key exchange, measured as how long the console stalled: 0.85 s with
`time.cloudflare.com` and 1.25 s with `nts.time.nl` (5.0 s and 6.9 s on the Pico W), after which the status showed
`(NTS, authenticated)`. **Not verified by me:** the display content, the buttons, the buzzer and the light sensor on this
board (they need someone to look and press), the firmware log (RTT needs a probe), long-term behaviour.
I changed the saved settings during the run (`hour12`, `ntp_servers`) and restored them; the final `show` matches the defaults
plus the WiFi and timezone settings.

Verifies: REQ-HW-06

### VR-40 · 2026-10-16 · build · Correction: the UF2 family ID

VR-12 and the README said the UF2 made by `elf2uf2-rs` from the RP2350 image carried "the RP2350 family ID". It does not:
`elf2uf2-rs` writes the RP2040 family ID (`0xe48bff56`) for every chip, and the RP2350 boot ROM ignores such a file without any
indication. The RP2350 Arm Secure family is `0xe48bff59`. I had compared the number with nothing and repeated it; the first
real attempt on a board showed the mistake. Fixed by `tools/uf2_family.py` (sets or checks the family), the corrected README
instructions, and a check in `tools/gate.sh` that fails if either chip's UF2 has the wrong family or if `elf2uf2-rs` starts
writing the right one (so the patch step can be dropped). The file the tool makes is byte-identical to the one that booted
the Pico 2 W.

Verifies: REQ-NFR-01

### VR-41 · 2026-10-16 · hardware · Pico 2 W: display and beeper fine (user observation)

After VR-39 the clock's owner looked at and listened to the clock with the Pico 2 W fitted and reported that the user-facing
elements, "like display and beeper", are all fine. This is the user's word, not a measurement. The display and the beeper
are named; the report does not say whether the buttons, the menu, the alarm or the light sensor were tried, so those are
not claimed here.

Verifies: REQ-HW-06

### VR-42 · 2026-10-16 · hardware · Pico 2 W: HTTPS server, certificate lifecycle, redirect and upload

On the Pico 2 W in the clock (name `picoclock-d6`), tested with `curl` and `openssl s_client` from the Raspberry Pi.
**HTTPS:** TLS 1.3 only with `TLS_AES_128_GCM_SHA256`; X25519 and P-256 key exchange work; TLS 1.2 and a hybrid-only client are
refused. A handshake took 0.14 to 0.2 s. `GET /` (5.7 KB, 30 form fields) and `/zones` (7.7 KB) work; 100 sequential requests
all succeeded (17.7 s); the display scan had one wait over 100 us (106 us) in 75 s. A POST with the right `https://` Origin
saved a setting; a wrong scheme or a foreign Origin gave 403; with the web password set: none and wrong gave 401, right gave
200, and the page never shows the password. **Certificate:** generated at first boot, OpenSSL shows CN and SAN
`picoclock-d6`, `picoclock-d6.local`, CA:FALSE, digital signature, server authentication, ten years. The fingerprint in
`status`/`cert` equals the one `openssl s_client` sees. After a reboot the same certificate was served (stored in its own
flash sector); `cert new` replaced it and the new fingerprint was served and reported. **Redirect:** plain `GET /` and
`GET /zones` gave 307 with the `https://` location; a POST over HTTP gave 403; a Host header with an injection attempt
(`evil.example/x`) was replaced by the clock's own address; HTTP/1.0 without Host worked; `http_redirect off` served the page
on port 80 again. **Upload:** a matching P-256 pair was accepted and served, shown as "uploaded", and survived a reflash and
reboot; refused with the right message: wrong key, RSA, P-384, missing key; refused (403) over plain HTTP and with a foreign
Origin. **Not verified:** a browser (no one has opened the page in one yet), a chain with intermediates on hardware (host
test only), heap use over a long run (no RTT on this board), the expiry path on hardware (host test only).
I restored the board afterwards to a fresh self-signed certificate and the default settings.

Verifies: REQ-WEB-06, REQ-WEB-08

### VR-43 · 2026-10-16 · hardware · Pico 2 W: per-provider geo status and the page footer

Flashed the Pico 2 W. `status` now lists each timezone provider on its own line (`ipwho.is: ...`, `ip-api.com: ...`), so a
failure of the first stays visible when the second succeeds; each provider is tried twice, two seconds apart, before the next.
With `timezone=auto`, `geo` gave `ipwho.is: ok (391 bytes, ...)` and `ip-api.com: never tried`. **Not verified on hardware:**
the retry and the second provider's line after a real failure (host test of the output only; no failure could be provoked).
The HTTPS page ends with the version, a link to the repository and the license. I set `timezone` back to Europe/Berlin.

Verifies: REQ-TZ-04, REQ-CFG-06

### VR-44 · 2026-10-16 · hardware · HTTPS web page in daily use (user observation)

The clock's owner reports that they use the web interface over HTTPS all the time and that it works fine. This is the user's
word, not a measurement; it settles the open question of a browser test (which browser, and the certificate warning flow,
were not stated).

Verifies: REQ-WEB-06, REQ-WEB-08

### VR-45 · 2026-10-16 · hardware · Pico 2 W: hardware random number generator, heap peak, display scan under load

Flashed the Pico 2 W. `status` shows `random  : hardware TRNG` (the boot self-test passed: two 32-byte draws differ and
are not constant). HTTPS, an NTS sync with `time.cloudflare.com` (`NTS, authenticated`) and the certificate upload all work
with it. **A regression found and fixed:** the first version took the TRNG under a critical section, which held off the
display interrupt; with 60 HTTPS requests the scan was late 181 times (worst 4.9 ms) against 1 wait idle. With a thread-mode
lock instead, 60 requests with 6 in parallel gave 0 waits over 100 us (worst 86 us in 100 s). **Heap:** peak 7.5 KB of the
144 KiB after 60 parallel requests, `/zones` and a certificate upload (the allocator records its peak; `status` shows it).
**Not verified:** the quality of the random numbers beyond that self-test (no statistical test suite was run), and long-term
heap behaviour (the peak after days, not minutes).
I changed `ntp_servers` and `timezone` for tests and set them back; nothing was saved except the certificate.

Verifies: REQ-WEB-06, REQ-NFR-02


### VR-46 · 2026-10-16 · hardware · Display scan priority held under network load (summary of earlier runs)

REQ-NFR-02 (the scan runs above all networking tasks) had a waiver because only its outcome was recorded. The outcome now
has two further measurements on the Pico 2 W under heavy network load: 100 sequential HTTPS requests with one scan wait over
100 us in 75 s (VR-42), and 60 HTTPS requests, 6 in parallel, with none over 100 us and a worst lateness of 86 us in 100 s
(VR-45). The same arrangement on the Pico W is covered by VR-22 and VR-24. The VR-45 run also showed what the property does
not cover: work in the thread-mode executor that holds interrupts off (a critical section around the hardware random number
generator) delayed the scan 181 times, so the priority alone is not the guarantee; no interrupt-masking section may be long.
**Not verified:** the priority arrangement itself is still not tested directly, only its effect.

Verifies: REQ-NFR-02

### VR-47 · 2026-10-17 · hardware · Pico 2 W: buttons, beeper and light sensor (user observation)

The clock's owner tried the buttons, the beeper and the light sensor on the Pico 2 W and reports that they work. This is the
user's word, not a measurement; the key-beep setting and the light-sensor thresholds were not singled out.

Verifies: REQ-HW-06

### VR-48 · 2026-10-17 · hardware · Pico 2 W: strongest-network scan, DHCP option 42, mDNS

On the Pico 2 W in the clock, on the owner's network (one stored WiFi slot). **Scan:** with `wifi_select strongest` (saved, then
reboot) the clock scanned, saw 24 access points and joined slot 1; `status` says so. **Not seen on hardware:** a choice between
two stored networks (only one slot is configured, and no credentials for a second network exist), so the ordering is checked by
the host test only. **Option 42:** the router (192.168.121.1) offers itself as NTP server; `status` lists it, and with
`ntp_servers dhcp` alone the clock synced ("plain NTP", 8 s after `sync`); with `nosuchhost.invalid,dhcp` the first entry failed
and the second answered. **mDNS:** the clock joined the group (`group joined`) and counted LAN queries; for a query to
`picoclock-d6.local` sent as unicast, as a multicast query from an ephemeral port, and as a multicast query from port 5353,
it answered every time (counter and a raw capture on the Pi show the reply: name `picoclock-d6`, address 192.168.121.41);
`other.local` got no answer. **Limit of my test:** the Raspberry Pi's firewall (INPUT policy DROP) discards the reply to a
multicast query before an application sees it, so I could only confirm it on the wire, not with a resolver on the Pi; no
real resolver (macOS, Windows, Android, avahi) has been tried. A bug found on the way: the first version replied with the
multicast group as its source address and nothing arrived; fixed by building the reply destination fresh. The settings I
changed for the tests were set back (`wifi_select order`, `ntp_servers gateway,pool.ntp.org`), and saved once.

Verifies: REQ-NET-04, REQ-NET-05, REQ-NET-07

### VR-49 · 2026-10-17 · host and hardware · NTS with RSA certificate chains

**Host (live, this PC, `cargo test -p picoclock-core --features rsa --test nts_live -- --ignored`):** the key exchange with
`ptbtime1.ptb.de` and `nts.netnod.se` (Let's Encrypt RSA chains ending at ISRG Root X1) succeeded, and PTB's NTP exchange
with three authenticated rounds gave offsets under 1 s; Cloudflare and `nts.time.nl` (ECDSA) still pass. Netnod's NTP server is
IPv6-only, so the clock could not use it; only its key exchange was checked. Without the feature the same PTB server is
refused with a TLS error (the Pico W behaviour). **Hardware (Pico 2 W):** `ntp_servers nts:ptbtime1.ptb.de` gave
`synced ... (NTS, authenticated)`; the console stalled 0.45 s during the RSA key exchange (0.55 s for Cloudflare's ECDSA one,
measured by polling `time` every 50 ms after a reboot); the heap peaked at 23 KB of 144 KiB with RSA against 1 KB with ECDSA.
**Not verified:** a Pico W (none attached; the refusal is checked on the host with the same code path, `rsa` feature off), a
chain with an RSA root signed by another RSA root, and System76 servers (`time.system76.com` did not answer).

Verifies: REQ-NTS-08

### VR-50 · 2026-10-17 · hardware · Pico 2 W: trust anchors added by the user

On the Pico 2 W, against a test NTS-KE server on this PC (`openssl s_server`, TCP 4460, reached by the router name
`index-s2.fritz.box`; certificates made for the test: a root, an intermediate and a leaf, ECDSA P-256 and RSA 2048; a
temporary firewall rule for the clock's address on the PC was added and removed afterwards). The clock's own `status` line
reported each result. **ECDSA:** untrusted root -> `refused: TLS or the certificate`; pasting the root's PEM into `root add`
over USB -> `added` with the same SHA-256 fingerprint OpenSSL gives; the next key exchange -> `ok (514 ms)`; `root remove 1`
followed by a reboot -> refused again (without the reboot the clock reuses its cookies and shows no new exchange); the root
added again and a reboot -> still listed and trusted. **RSA:** the RSA test root added, key exchange `ok (448 ms)`, heap peak
12 KB. **HTTPS page:** the list shows the roots; adding a PEM, a duplicate (refused), junk and a damaged certificate (refused
with the message), remove by number, remove 9 and 0 (refused); a POST with a foreign Origin gave 403; over plain HTTP a POST
gave 403 whether the redirect was on or off, and the plain page has no roots form. **Not verified:** the Pico W (no board
attached; the terminal path is the same code, the RSA refusal is covered on the host), a browser using the form, and a real
public CA that is not built in. I restored the clock afterwards (`ntp_servers`, `http_redirect`, a new self-signed
certificate) and saved.

Verifies: REQ-NTS-09, REQ-NTS-10, REQ-NTS-11


### VR-51 · 2026-10-17 · hardware · Ping and a real mDNS resolver

On the Pico 2 W (`picoclock-d6`) with the network stack's `icmp-ping-reply` feature enabled: `ping 192.168.121.41` from the
Raspberry Pi 5 gave 3 of 3 replies (2 to 44 ms); before the change the same ping gave 0 of 3 and the owner could not ping
it either. `ping picoclock-d6.local` gave 2 of 2 replies, so the Pi's resolver found the clock through mDNS (this is the
first check of the mDNS answer by a real resolver; the earlier one was a packet capture). The owner also reported that
`picoclock-d6.local` resolves and the web page opens over HTTP and HTTPS.

Verifies: REQ-NET-07, REQ-NET-09

### VR-52 · 2026-10-17 · hardware · Faster switch to the next WiFi slot

Pico 2 W. Slot 1 set to the real network's name with a wrong passphrase, slot 2 the real credentials, `save`; the time from
`save` until the clock's web port answered again was measured from the Raspberry Pi 5. Before (every network retried up to
three times in turn): back after 32.5 s. After (`JoinPlan`: every network once before any is retried): back after 12.6 s,
twice, identical. The remaining ten seconds are the radio chip's handshake timeout for one wrong passphrase, which the
driver does not let us shorten. The settings were restored afterwards. A host test covers the plan: all networks before a
retry, no retry after a hopeless failure, all attempts for a single network.

Verifies: REQ-NET-08

### VR-53 · 2026-10-17 · hardware · Pinned self-signed NTS server certificate

Pico 2 W against an `openssl s_server` NTS-KE test server on the Raspberry Pi 5 that sends only its own self-signed P-256
certificate (no chain). Certificate with `basicConstraints CA:TRUE` (the `openssl req -x509` default): refused before and
after `root add` of that same certificate (`refused: TLS or the certificate ...`). The same key with `CA:FALSE`: `root add`
accepted it and the key exchange gave `ok (426 ms)`. The reason for the `CA:TRUE` refusal was not read from the clock (the
status line does not distinguish certificate errors); it is recorded as observed. Also checked from the Pi: of 27 public NTS
servers probed on port 4460, 18 answered; every one of those chains to ISRG Root X1 or the SSL.com ECC root, both built in.
The root was removed and the clock restored afterwards.

Verifies: REQ-NTS-09, REQ-NTS-11

### VR-54 · 2026-10-17 · hardware · MAC address and custom host name

Pico 2 W `picoclock-d6`. `status` on the terminal and the top of the web page (fetched with curl over HTTPS) show
`mac     : 2c:cf:67:b8:84:d6` under the name. `set hostname Uhr-Test` gave `uhr-test`; `set hostname bad_name` was refused and
the value stayed. After `save`, without a reboot, the name became `uhr-test` within 20 s: the FRITZ!Box resolved
`uhr-test.fritz.box` to the clock (DHCP name), `ping uhr-test.local` from the Raspberry Pi 5 gave 2 of 2 replies (mDNS), and
the HTTPS certificate had a new fingerprint with subject `CN=uhr-test` and names `uhr-test` and `uhr-test.local`. The web
form (`POST /save`, same origin) changed it to `web-name`; a name with a space was refused with the message. A factory reset
(VR-55) set it back to the default. **Not verified:** an uploaded certificate staying in place after a name change (the
code keeps it; no upload was made), and a browser.

Verifies: REQ-CFG-07, REQ-CFG-08

### VR-55 · 2026-10-17 · hardware · Factory reset from both interfaces

Pico 2 W. **Guards:** `POST /reset` with no word showed the second page ("Type RESET to confirm"); `confirm=reset` showed
"not the confirmation word" and erased nothing; a foreign `Origin` gave 403; the plain-HTTP port gave 403 (it only redirects
while HTTPS runs). On the terminal `factory-reset` followed by `RESEt` or an empty line printed "cancelled; nothing was
erased" and the WiFi network was still stored. **Terminal:** with a root added, the timezone changed and a custom name, `factory-reset` then
`RESET` restarted the clock; afterwards the WiFi network, the name (back to `picoclock-d6`), the root (0 of your own) and
the HTTPS certificate (new fingerprint) were gone or new, and the clock was `not connected`. **Web:** the same after `POST
/reset` with `confirm=RESET` (name `x-web` back to the default, WiFi network empty, answer page shown first). The clock was
then provisioned again with `tools/provision.py` and came back online. **Not verified:** a browser clicking through the two
pages, and a flash erase failure (the clock then stays up without restarting, by design).

Verifies: REQ-CFG-09

### VR-56 · 2026-10-17 · hardware · Fixes from the full-code review (Fable, before the time-source phase)

Two read-only reviews (the core crate; the firmware and documents) found, among other things, a web status block cut off at
512 bytes (the page lost the heap, HTTPS, scan and time lines of a running clock), header names with a space before the
colon read as unknown headers, stratum 16 accepted in NTP replies, a possible panic in the terminal output cut, the RTC
write not checked for a late wake-up, and tests that could not fail. Fixed: status buffer 1536 bytes with a test that fills
every optional line; such headers answered `400` (checked on the clock with `nc`); stratum above 15 refused; the cut made
on a character boundary; the RTC write waits for the next second if its wake-up is more than 5 ms late (up to three
times, then writes with a warning); a negative age no longer counts as synchronised; a DS3231 in 12-hour mode or with
minutes or seconds out of range reads as no time. Tests: the cookie assertion is exact, the worst-case configuration record
holds a real 64-byte timezone rule and geo rule, the built-in roots are compared with their published SHA-256 fingerprints, and
a vacuous OpenSSL key assertion was removed; each new check failed when its fix was taken out. On the Pico 2 W: the web page
shows the whole status block down to `scan` and the `utc` and `local` lines, a normal `sync` set the clock (verified against
the Raspberry Pi's time), and a sync issued while six HTTPS requests ran in parallel completed. **Not verified:** the late
wake-up path itself (no way to provoke or observe it without a probe); no `defmt` log was read. Findings left open for the
owner are listed in the README roadmap.

Verifies: REQ-WEB-01, REQ-WEB-02

### VR-57 · 2026-10-18 · hardware · Decisions after the review: unsaved edits, 8 KiB uploads, watchdog, reset over HTTP

Pico 2 W `picoclock-d6`. **Unsaved edits (REQ-CFG-05):** `set hostname edited1` in the terminal, then a web `POST /save` with
`hour12=off`; the web save answered "1 setting(s) changed and saved", `status` still said `unsaved changes` and `name : edited1`;
after `reboot` the host name was back to the stored (empty) value and the web-saved setting had persisted. **Reset over plain
HTTP (REQ-WEB-09):** `POST /reset` to port 80 answered `403` both with the redirect on and, with `http_redirect off`, with the
text "only accepted over HTTPS while HTTPS is on"; the same request over HTTPS reached the confirmation page (`200`, nothing
erased); the plain GET page still worked; the setting was put back. **Upload size (REQ-WEB-10):** a two-certificate chain of
2959 bytes of DER (4589 bytes of form body, which the 4 KiB limit refused) uploaded over HTTPS and was served (`cert` showed it as
`uploaded`); a chain of 3353 bytes was refused with the "too large (about 3 KB)" message; a 9 KB request got `413` and the text naming the
limits; `cert new` restored the self-signed certificate. **Watchdog (REQ-NFR-03):** a temporary image hung the clock task 45 s
after boot, alternately by a `loop {}` (console dead at once) and by awaiting forever (console answered for about 12 s, then the
USB device vanished); both ended in a restart about 8 s after the last feed and `status` then showed `reset : the last restart
was caused by the watchdog`; after `bootsel` and an upload, and after `reboot`, the line was absent (no false report from the boot
ROM's own watchdog reset). The same image ran every random number (HTTPS handshakes, `cert new`) through the new ring-oscillator
conditioner on the RP2350: it works and is fast enough (`cert new` 3.5 s, 160 ms per HTTPS request). It was removed again; the
image now on the clock is the normal build. **Not verified:** the Pico W (RP2040) was not attached, so the conditioned random
bytes, the second settings sector's layout on that chip and the 8.3 s watchdog limit with an NTS key exchange there are untested;
the quality of the ring-oscillator bits is not measured; the backup sector's read path is verified only by the host test that
cuts a save off at every step (REQ-CFG-10), not by cutting power; the NTS key-exchange feed of the watchdog was not provoked.

Verifies: REQ-CFG-05, REQ-WEB-09, REQ-WEB-10, REQ-NFR-03

### VR-58 · 2026-10-18 · hardware · Software 0.2.0 on a Pico 2 W without a DCF77 module: source selection

`version` printed 0.2.0. With the default settings (`time_sources ntp`, mode `order`) the clock synced as before ("synced : 17 s
ago (plain NTP)", `ntp : in use ... +0.001 s from the clock`, `dcf77 : off`). The web page lists the five new settings. With
`set time_sources dcf77,ntp` and `time_mode vote` the status showed `dcf77 : waiting, vote power 1; pulses 0, frames ok 0, bad 0,
bad pulses 0` and an NTP sync (`sync`) still set the clock, because a source that has nothing to say does not block the
other. With `time_sources dcf77` alone, `sync` made no NTP query (`ntp : off`). Invalid values (`gps`, power 12) were refused.
The receiver pins were driven (PON high while `dcf77` is not selected). **Not verified:** everything that needs the module:
pulse timing against a real receiver, a confirmed DCF77 time reaching the clock, the fallback and vote with two live sources,
the receiver's reception next to the display and the WiFi radio, and PON switching the module. A hardware record for
REQ-DCF-03 follows when the module is fitted; until then the decoder and the selection are evidenced only by host tests
(each checked by sabotaging the rule it tests: majority threshold, tolerance, consecutive-minute check, parity 3).

Verifies: REQ-SRC-01

### VR-59 · 2026-10-18 · hardware · DCF77 receiver on a Pico 2 W, software 0.2.0 (debug probe attached, RTT logs read)

A Pollin DCF1 module wired as planned (PON on GP0, TCO on GP1, 3V3), **without** the 10 µF capacitor. The first attempt received
almost nothing (2 pulses in 9 minutes, TCO low in 400 readings over the debug probe, and driven low, not floating); after the
antenna was reseated and its wires bent away from other components it received about 60 pulses a minute. **Decoding:** confirmed
times arrive every one to four minutes in daytime reception (frames ok 24 against bad 10 over about 40 minutes). **Selection:**
in `order` with NTP first, DCF77 stood by and `status` showed its time 54 to 75 ms behind the NTP-set clock; with DCF77 alone
(`time_sources dcf77`, NTP not queried) a restarted clock had no time, took it from the second confirmed DCF77 time ("used, clock
set", RTC written), and later fixes agreed with the clock within 3 ms. **Receiver delay (REQ-DCF-04):** a manual `dcf77_delay 55`
moved the displayed offset from -54 ms to +4..+19 ms; in `auto` the readings logged over RTT were 82, 50, 73, 55 ms
(delay 0), the first answer 73 ms was stored, and with it applied the offset read +32 ms (reading 41); the median over more fixes
is expected to settle near 55 ms but this was not observed beyond 5 readings. **Not verified:** the receiver with the capacitor
fitted; reception at night; the fallback and the vote with two live sources disagreeing; whether the learned delay converges
over a day; HAM mode (REQ-HAM-02) was set on the clock but the buttons were not pressed and the sound not heard (host tests only);
the 83 ms first learned in an earlier run came from a start with few, scattered readings and the upper median (fixed: the mean of
the two middle readings for an even count).

Verifies: REQ-DCF-01, REQ-DCF-02, REQ-DCF-03, REQ-DCF-04

### VR-60 · 2026-10-18 · host · Web interface in six pages

Host tests (`core/src/http.rs`): every setting appears on exactly one settings page and nowhere else; the location cache is
on none; the status text only on the Status page; each settings page posts to its own `/save?tab=...`; six links with the open
one marked `aria-current`; every page is under 6.5 KB with everything switched on (the full WiFi page, which was the largest
single form, is under 6 KB); the Security page with four trust anchors, a 300-character message and HTTPS running fits with
room to spare; the tab comes only from `tab=` in the query, unknown values show Status and are never echoed. The Pico 2 W and
Pico W firmware build and the gate passes. **Not verified on the clock:** the pages in a browser or by `curl`, the redirect
from HTTP to HTTPS keeping the `?tab=`, and saving from each page: the clock had moved to the network 192.168.1.x (gateway
192.168.1.1), which the Raspberry Pi used for testing (192.168.121.x) cannot reach, so the pages could not be fetched. They
need one look in a browser.

Verifies: REQ-WEB-11

### VR-61 · 2026-10-18 · hardware · The six web pages on the Pico 2 W; one SSID on two networks

On the clock (192.168.121.41): all six pages answered `200` over HTTPS (2.5 to 4.1 KB), each with its own tab marked, the four
settings pages with `action="/save?tab=..."`; an unknown tab gave the Status page; the plain-HTTP redirect kept the query
(`Location: https://.../?tab=time`); `POST /save?tab=clock` applied both fields sent (`fahrenheit` and `hostname`, the latter
belonging to another tab: the parser does not know the tabs) and returned the Clock page; the Security page's reset button
reached its confirmation page (nothing erased); `/zones` still worked. Both settings were put back. **Why the clock was
unreachable earlier:** the SSID `WiFi_RGBDYYXg8Kt_nomap` is broadcast by two sets of access points (BSSIDs `DC:39:6F:CE:6D:..`
and `D4:24:DD:1C:12:..`, seen by the Raspberry Pi's WiFi scan) that belong to two different networks (gateways 192.168.121.1
and 192.168.1.1). The clock joins whichever its scan finds first, so after a restart it was once on 192.168.1.x (no NTP
answer there, hence `ntp: waiting`, and the Pi could not reach it) and three times in a row on 192.168.121.x. The firmware does
not choose between access points of one SSID and does not log the BSSID.

Verifies: REQ-WEB-11

### VR-62 · 2026-10-18 · hardware · A second DHCP answer and the access points the clock hears

**Incident:** once, after a firmware flash, the Pico 2 W held 192.168.1.100 with gateway 192.168.1.1, no DHCP option 42 and no NTP answer, for about 15 minutes; its normal network is 192.168.121.x (gateway 192.168.121.1, which offers option 42). The code has no static or fallback address (searched `firmware/` and `core/`; xarxa has none; the 192.168.x literals are in tests), so a DHCP server answered. **Checks since:** about 25 further restarts and one reflash all requested from server-id 192.168.121.1 (a passive listener on the wired port saw the clock's DISCOVER and REQUEST); six broadcast DISCOVERs from the Pi's wired port were answered only by 192.168.121.1 (`b4:fc:7d:22:07:b0`); the Pi's WiFi scan lists only the six BSSIDs of the owner's two meshed AVM access points (checked every 40 s for an hour). The owner states the AVM fallbacks are 192.168.178.1 and 169.254.1.1, so 192.168.1.1 is not theirs; they also have Shelly and Anker devices and a devolo powerline adapter on the LAN, none yet identified as the source. **New logging (REQ-NET-10):** `status` now shows `dhcp:` and `ap:`; after the join the clock's own 2.4 GHz scan heard two access points with the SSID: `dc:39:6f:ce:6d:f9` channel 1, -51 dBm (the joined one, link -53 dBm) and `b4:fc:7d:22:07:a0` channel 1, -85 dBm, which shares its first five bytes with the router's LAN address and is presumably the Fritz!Box's own radio. **Not determined:** what answered with 192.168.1.1, and where it is; it has not reproduced.

Verifies: REQ-NET-10

### VR-63 · 2026-10-18 · hardware · The firmware update check against GitHub

**Host, live (`core/tests/update_live.rs`, run with `--ignored`; same TLS code as on the clock):** TLS 1.3 to `api.github.com` with only the
pinned USERTrust ECC root succeeds (the chain is Sectigo E36 under E46 under USERTrust ECC); the same request with the NTS roots is
refused (`Tls` error, nothing returned); a connection to `api.github.com` with the name `example.org` is refused; the answer for
this repository is `404` (no release) and is understood as such; the answer for a repository that has releases
(`rust-lang/rust`, 3610 bytes read) gives its tag as the newer version. Found and fixed on the way: the server's close_notify after its
answer was reported as a failure. **Pico 2 W:** with the real repository `update check` answered "no release has been published yet,
checked 0 min ago" (the same on the web page); a temporary build pointed at `rust-lang/rust` answered "1.99.1 is available, you run
0.2.0" (the stand-in target is not a release of this project; the build was removed again); the web button
(`POST /update-check`) started a check and a request with a foreign `Origin` got `403`; `update_check off`/`on` took effect; the
setting that was found not to fit the old test (the settings record outgrew a 1 KB buffer) now has a test with every setting at its
longest (1.8 KB of 2 KB). **Not verified:** the Pico W (RP2040) was not attached, so the check's 16 KB TLS buffer and the 5 to 7 s the
handshake stops the other tasks there are untested on that chip; the daily schedule and the one-hour retry were not observed over time;
no real release of this project exists to be found as "newer".

Verifies: REQ-UPD-01, REQ-UPD-03

### VR-64 · 2026-10-18 · hardware · The update button no longer asks again when the page is reloaded

Found by the owner: the answer to `POST /update-check` was the page itself with the text "reload this page in a few seconds", and a
reload made the browser send the request again and start another check. Now the answer is `303 See Other` to `/?tab=status`, and while
a check runs the Status page carries a 4-second refresh. On the Pico 2 W: the POST returned `303` with `Location: /?tab=status`; a GET right
after showed "update  : checking GitHub..." and the refresh; a GET 14 s later showed the result without it; five further GETs left
"checked 0 min ago" unchanged, so reloading asks nothing. **Not verified in a browser** (curl only). The other forms of the web
interface (`/save`, `/root`, `/cert`) still answer a POST with the page itself and so have the same resubmission on reload; for `/root`
(remove number n) a reload would remove another authority.

Verifies: REQ-UPD-01

### VR-65 · 2026-10-18 · hardware · Every web form is answered with a redirect

After VR-64 the other forms had the same flaw. Now `POST /save`, `/cert` and `/root` also answer `303 See Other` to the page they came
from (`/?tab=clock`, `/?tab=security`), and the message they want to show ("1 setting(s) changed and saved", an error) waits in RAM and is
shown once by the next page that is loaded. On the Pico 2 W: a save from the Clock tab redirected to it, the first GET showed the
message and a reload did not; two test authorities were added (each redirect, each message once), `remove=1` was sent once (message
"Removed; saved to flash") and three further GETs of the page left one authority in place, where before each reload sent the removal again;
a refused certificate redirected and showed its error once. The test authorities and the setting were removed again. The forms for the
factory reset keep answering a POST with a page (the confirmation page, which asks and erases nothing). **Not verified in a browser**
(curl only), and a message is kept for whichever page is loaded next, so a second browser could show it instead of the one that
pressed the button.

Verifies: REQ-WEB-11
