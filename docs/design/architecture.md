---
project: picoclock
doc: docs/design/architecture.md
status: design
last_updated: 2026-10-16
---

# Architecture and design

This document explains how the firmware is built and why. It states design and intent. What is currently
implemented and verified is not claimed here: that lives in the generated
[traceability matrix](../traceability-matrix.md), and the requirements are in the [SRS](../srs.md).

## 1. System context

```
                         +------------------ Pico-Clock-Green board -------------------+
   router / LAN          |                                                             |
  +-------------+  WiFi  |  +------------------+   I2C1    +-----------------------+   |
  | DHCP, DNS,  |<------>|  | Pico W / Pico 2 W|<--------->| DS3231 RTC (UTC)      |   |
  | NTP gateway |        |  |  RP2040/RP2350   |  GP6/GP7  | + temperature sensor  |   |
  +-------------+        |  |  + CYW43439      |           +-----------------------+   |
  pool.ntp.org           |  |                  |  GPIO     +-----------------------+   |
  ipwho.is, ip-api.com   |  |                  |---------->| 32x8 LED matrix       |   |
                         |  |                  |  74HC595  | (row scan, 8 rows)    |   |
  phone / PC browser --->|  |                  |           +-----------------------+   |
        HTTP :80         |  |                  |<-- 3 buttons (SET, UP, DOWN)          |
                         |  |                  |--> buzzer (GP14)                      |
  PC terminal ---------->|  |                  |<-- light sensor (ADC GP26)            |
        USB serial       |  +------------------+                                       |
                         +-------------------------------------------------------------+
```

Pin map (from the Waveshare demo): matrix OE=13, SDI=11, CLK=10, LE=12, A0=16, A1=18, A2=22; buttons SET=2, UP=17,
DOWN=15 (active low, pull-ups); I2C1 SDA=6, SCL=7, DS3231 at 0x68; buzzer 14; light sensor ADC on 26. The
CYW43 radio uses 23, 24, 25 and 29, so there is no conflict. Pico W and Pico 2 W share this map.

## 2. Structure

Two crates in one Cargo workspace:

| Crate       | Kind                | Contents                                                                              |
|-------------|---------------------|---------------------------------------------------------------------------------------|
| `core/`     | `no_std` library    | everything that does not touch hardware; compiled and unit-tested on the host         |
| `firmware/` | `no_std` binary     | Embassy application: drivers, tasks, networking; depends on `core`                    |

The split is the main design decision of the code base. Logic that can be wrong in subtle ways (calendar and DST
maths, packet formats, parsing of untrusted input, the config format, the button state machine) is in `core`, where
its unit tests run in milliseconds without hardware. `firmware` is kept thin: it moves bytes between hardware and `core`.

### 2.1 `core` modules

| Module   | Responsibility                                                                                       |
|----------|------------------------------------------------------------------------------------------------------|
| `time`   | Unix time and civil date conversions, weekday, the next-second-boundary calculation                  |
| `tz`     | POSIX TZ rule parser, `Tz::offset_at(utc)`, IANA name lookup, prefix listing                         |
| `tzdb`   | Generated table of 486 IANA names to POSIX rules (`tools/gen_tzdb.py`)                               |
| `sntp`   | SNTP client request and reply parsing; SNTP server reply construction                                |
| `geo`    | Provider list and parsers for the IP geolocation replies                                             |
| `config` | `Config` (all settings), `KEYS` table, validation in `set`, text serialisation with CRC32, hostname  |
| `cli`    | `execute(line, config, status, out) -> Action`: every terminal command                               |
| `http`   | Bounded request parser, Basic auth, same-origin check, form decoding, page rendering                 |
| `ui`     | State machine: views, menu, brightness, alarm, chime, rendering into a `Frame`                       |
| `frame`  | The 32x8 matrix, weekday LEDs and icons as the panel's row bytes; `font` holds the glyph table      |

### 2.2 `firmware` modules

| Module      | Responsibility                                                                                    |
|-------------|---------------------------------------------------------------------------------------------------|
| `main`      | Chip bring-up, peripheral wiring, task spawning, `ui_task` and `light_task`                      |
| `state`     | Shared configuration, runtime status, signals and the time anchor                                 |
| `display`   | Row-by-row matrix scan and brightness (duty) control                                              |
| `rtc`       | DS3231 over I2C: read, write, temperature, oscillator-stopped flag                                |
| `buttons`   | 10 ms polling, debounce, short / long / auto-repeat events                                        |
| `buzzer`    | Beep patterns; the alarm pattern repeats until stopped                                            |
| `storage`   | Load and save the config record in the last flash sector                                          |
| `net`       | WiFi join and slot handling, hourly sync, geolocation request, NTP server task                    |
| `ntp`       | SNTP client transport: resolve, send, receive, half-round-trip correction                         |
| `usb_cli`   | USB CDC-ACM device, line editing, prompt; commands run in `core::cli`                            |
| `web`       | TCP listener on port 80, one connection at a time, request deadline                               |

## 3. Runtime model

```
 high-priority interrupt executor (SWI_IRQ_1, priority 2)
   scan_task  ------------------------------> LED matrix (1 row per ms, 125 Hz)
        ^ reads FRAME, FIXED_DUTY, AUTO_DUTY
 ---------------------------------------------------------------------------------
 thread-mode executor (cooperative, one core)
   ui_task      every 250 ms or on a button: RTC -> Ui state machine -> FRAME
   light_task   1 Hz: ADC light sensor -> AUTO_DUTY; RP2040 temperature -> Status
   button_task  10 ms poll -> BUTTONS channel
   buzzer_task  BEEP signal / ALARM_RINGING flag
   storage_task SAVE_REQUEST (3 s debounce) -> flash
   usb_task, cli_task    USB device and terminal
   cyw43_task, net_task  radio driver and TCP/IP stack
   wifi_task    join, hourly NTP, geolocation, rejoin on credential change
   ntp_server_task       UDP 123, only while enabled and online
   web_task     TCP 80, only while enabled
```

**Why two executors.** The matrix must be refreshed continuously or it flickers, and the WiFi driver and the TCP/IP
stack can keep the cooperative executor busy for milliseconds at a time. `scan_task` therefore runs on an
interrupt-driven executor at a higher interrupt priority, so it pre-empts every networking task. The scan itself is
short: shift four bytes, latch, set the row address, open the output enable for the duty-cycle fraction of 1 ms.

**Shared state** is deliberately small and all of it lives in `state.rs` (or `display.rs` for the frame):

| Item                          | Kind                          | Writers / readers                                              |
|-------------------------------|-------------------------------|----------------------------------------------------------------|
| `CONFIG`                      | thread-mode mutex             | terminal, web, UI menu write; everything reads (snapshot)      |
| `RUNTIME` (`Status`)          | thread-mode mutex, `Copy`     | tasks write their own fields; terminal and web read            |
| `FRAME`                       | critical-section mutex        | `ui_task` writes; `scan_task` reads                            |
| `NTP_TIME`                    | signal                        | `wifi_task` -> `ui_task`: (seconds, ms, instant of reply)      |
| `ANCHOR`                      | mutex                         | `ui_task` writes when it sets the RTC; NTP server reads        |
| `SAVE_REQUEST`, `CONFIG_CHANGED`, `SYNC_NOW`, `GEO_NOW`, `BEEP` | signals | requests between tasks                       |
| `BUTTONS`                     | channel (8)                   | `button_task` -> `ui_task`                                     |
| `FIXED_DUTY`, `AUTO_DUTY`, `ALARM_RINGING` | atomics          | single writer each                                             |

No task holds a lock across an `await`. `CONFIG` and `RUNTIME` use a thread-mode mutex, not a critical-section
mutex: only thread-mode tasks touch them, so no interrupt masking is needed, and masking was measurably harmful.
With a critical-section mutex, cloning the 8-slot configuration or rendering a frame inside the lock switched
interrupts off for hundreds of microseconds, the display scan's timer fired late and a row flickered (found on the
hardware, see VR-22). The rule: **nothing that runs for more than a few microseconds may hold a critical section**; the
only deliberate exception is the flash erase during a save. The scan's lateness is reported by `status`. Readers take a snapshot (`config_snapshot()`, `status()`), which is why
`Status` is `Copy` and why it holds only small values.

## 4. Time

The central rule: **the DS3231 holds UTC; local time is computed when it is displayed.**

```
 NTP reply ──> wifi_task ──(secs, ms, Instant)──> ui_task
                                                    |  wait until the next UTC second boundary
                                                    v
                                         rtc.set(UTC) ;  ANCHOR = (second, Instant)
 each 250 ms:  rtc.read() -> utc ──> Tz::offset_at(utc) ──> local ──> Ui::render ──> FRAME
 NTP server :  now = ANCHOR.second + ANCHOR.instant.elapsed()   (sub-second, independent of the RTC)
```

Consequences and reasons:

- **DST needs no sync.** The offset is a pure function of the UTC instant and the rule, evaluated at display time.
  `timezone=utc` simply uses offset 0.
- **Writing on a second boundary.** The DS3231 restarts its internal second when the seconds register is written. A
  write at an arbitrary instant leaves the RTC's second edges out of phase with UTC by up to a second (this was
  measured: offsets of −0.75 to −1.65 s before the change, a few milliseconds after). `next_second_boundary`
  (in `core::time`, unit-tested) computes the second to write and how long to wait; `ui_task` waits and writes.
  Cost: the display pauses for up to one second once an hour.
- **A software clock for the NTP server.** The RTC has whole-second resolution. The server needs sub-second time, so
  it extends the `ANCHOR` with the monotonic timer. It is only used for serving; the RTC remains the clock of
  record across power cycles. Between hourly syncs the software clock can drift by the crystal tolerance.
- **Trust.** Time is "synced" for the display if the last sync is under three hours old, and for the server if it is
  under 24 hours old; otherwise the server marks its replies unsynchronised so clients ignore them.

### 4.1 Time zones

Rules are POSIX TZ strings (`CET-1CEST,M3.5.0,M10.5.0/3`) parsed by `core::tz`. A generated table maps 486 IANA
names to those rules, so users can type `Europe/Berlin` without the device carrying the full tz database. The
generator reads the system tz database (`tools/gen_tzdb.py`), so the table is a build artefact, not hand-typed.
Limitation by design: a POSIX rule describes the current rule only, not historical changes.

### 4.2 Location by IP

No `no_std` crate exists, so `net.rs` makes plain-HTTP requests to free services, only when `timezone=auto`. The
providers are a list in `core::geo` (`PROVIDERS`: host, request bytes, reply format), tried in order; the first reply
that names a zone in the built-in table wins. There is more than one because a service can be unreachable from a given
network: on the first clock ip-api.com accepted the TCP connection but never acknowledged the request (cause not
found), while ipwho.is answered in seconds. Each provider's reply is untrusted: it is parsed by `core::geo` in that
provider's own format and used only if the zone name is in the built-in table. The result is cached (`geo_rule`) and
refreshed daily. Plain HTTP is a deliberate trade-off (HTTPS needs a paid plan or a TLS client); it is opt-in for that
reason. Adding a provider is one entry plus, if its reply is laid out differently, one parser arm.

## 5. Configuration

One model serves three front ends, so there is exactly one place that validates a value.

```
 USB terminal ─┐                      ┌─> flash record  "PCK1" | len | CRC32 | key=value lines
 web form ─────┼─> Config::set(k, v) ─┤
 buttons (menu)┘   (validates)        └─> Config::write_value / write_display (secrets redacted)
```

- `KEYS` is the single list of settings (name, help text, secret flag). The terminal's `help`, the web form and the
  serialiser are all driven from it, so adding a setting means adding one entry and one match arm.
- The stored form is human-readable text with a CRC32. Unknown keys and invalid values are skipped on load, so an
  older firmware can read a newer record and a newer one an older record (this is how the single-network settings of
  the first version migrated into slot 1 of the eight-slot version).
- A blank or corrupt record yields the defaults. The record is 2 KiB inside the last 4 KiB sector; a test checks that
  the worst case (every field at maximum length) fits.
- Saving erases and writes a flash sector, during which the CPU stalls (the display blinks once). `storage_task`
  debounces requests by three seconds, writes, then reads the record back and compares before reporting success.
- WiFi networks are eight slots (`wifiN_ssid`, `wifiN_password`). `wifi_ssid` / `wifi_password` remain aliases for
  slot 1. The device joins the first reachable slot in order and leaves only when the credentials of the slot in use
  change, so editing another slot does not disconnect it.

## 6. Networking

- **Join.** `wifi_task` derives the hostname `picoclock-<last MAC octet>` before DHCP runs, then tries the stored
  networks in order. Each attempt is abandoned after 15 s (the radio driver can wait forever for a lost event),
  an authentication or join failure is retried up to three times on the same network, and a network that is not
  there or a wrong passphrase is not. After a full round the task waits 10 s, or until a setting changes, and starts
  over. After joining, the link and a DHCP lease must arrive within 30 s.
- **NTP client.** `ntp::query` resolves each configured server (`gateway` means the DHCP router, others via DNS), sends
  one request whose transmit timestamp is a nonce (the microsecond uptime), and accepts only a reply that echoes it,
  is server-mode, not LI 3 and not stratum 0. Half the round-trip time is added. Hourly, retry after 30 s.
- **NTP server.** `ntp_server_task` binds UDP 123 only while `ntp_serve` is on and WiFi is up. `core::sntp` builds
  the reply: it answers only client-mode requests of version 1 to 4, copies the client's transmit timestamp, reports
  stratum 5 (the upstream stratum is not tracked, so it is deliberately high), root dispersion 0.25 s, and never
  returns more than 48 bytes. With no sync in 24 hours it answers LI 3 / stratum 16.
- **Web.** One connection at a time on port 80 with a six-second request deadline, 1.5 KiB receive and 2 KiB transmit
  buffers. `core::http::parse_request` is bounded (4 KiB request, 32 headers, 64-byte path), rejects duplicate
  `Content-Length`, and never panics on any input. The page is rendered into a 12 KiB buffer; a render that does not
  fit is an error response, not a panic.
- **Terminal.** CDC-ACM serial; `usb_cli` does line editing only, commands are `core::cli::execute`.

### 6.1 Security design

The device is a LAN appliance and the design says so plainly.

| Concern                      | Design                                                                                          |
|------------------------------|-------------------------------------------------------------------------------------------------|
| Web traffic                  | Plain HTTP. Passwords cross the LAN in clear. HTTPS is a roadmap item (see section 9).          |
| Web access control           | Optional HTTP Basic (`admin` + `web_password`), constant-time comparison. Off if no password.   |
| Cross-site requests          | `POST /save` is refused unless `Origin` matches `Host` (when an `Origin` is sent).              |
| Secrets                      | Never printed by the terminal or written into the page; a blank field keeps the stored value.   |
| Output encoding              | All values written into the page are HTML-escaped.                                              |
| Hostile requests             | Hard limits on size, headers, path; deadline per request; no allocation.                        |
| NTP server abuse             | Off by default; reply never larger than the request; non-requests are dropped; sequential.     |
| NTP client spoofing          | Nonce origin check; unsynchronised and stratum-0 replies are refused. Not authenticated (NTS is on the roadmap). |
| Geolocation                  | Opt-in; reply must name a known zone; sends the public IP in clear to a third party.           |
| Flash storage                | CRC; read-back verification after every write.                                                  |
| Credentials in the repo      | None. `local/` is git-ignored; `tools/provision.py` pushes them over USB.                       |

## 7. User interface

`core::ui::Ui` is a pure state machine: `button(...)` and `tick(...)` take the configuration and an `Env` (UTC, offset,
temperature, sync state, monotonic milliseconds) and return what the firmware must do (a beep, a config change);
`render(...)` fills a `Frame`. It does not own a clock or hardware, which is why alarm timing, menu timeouts and
rendering are all unit-tested with a fake `Env`.

- Views: SET cycles time, date, temperature (back to time after 5 s). Holding SET opens the menu: alarm on/off, hour,
  minute, hourly chime, key beep, 12/24 h, C/F, brightness. UP / DOWN adjust brightness outside the menu.
- Alarm: fires once per matching minute in local time, any button stops it, it stops itself after a minute (`ALARM_MAX_MS`).
- The panel has no WiFi icon, so the blinking colon and the count-up icon carry the sync state.
- Brightness: fixed levels use the display duty directly; `auto` maps the light sensor's raw ADC value to a duty
  (full brightness up to 1500, linearly down to 15 % at 3500). The polarity (high reading = dark) comes from the vendor
  demo; the owner reports that the sensor works, but the thresholds were not measured.

`core::frame` knows the panel's quirks (inherited from the vendor demo): the shift-register row layout, the column
offset caused by the status LED group, and glyphs that straddle two groups. Two rows of the vendor's font table were
wrong and are redefined.

## 8. Build variants and resources

| Variant   | Chip / board          | Target                         | Feature  | Flash   | Settings sector       |
|-----------|-----------------------|--------------------------------|----------|---------|-----------------------|
| Pico W    | RP2040                | `thumbv6m-none-eabi`           | `rp2040` | 2 MiB   | last 4 KiB (`memory-rp2040.x`) |
| Pico 2 W  | RP2350                | `thumbv8m.main-none-eabihf`    | `rp235x` | 4 MiB   | last 4 KiB (`memory-rp235x.x`) |

The chip feature selects the memory map and linker scripts (`build.rs`), the RP2350 boot block (`IMAGE_DEF`), the
flash size constant and the WiFi SPI clock divider. Nothing else differs. Both images are about 440 KiB of code and
72 KiB of static RAM. Embassy is pinned to a git revision because the networking APIs used are newer than the
crates.io releases.

## 9. Decisions and their reasons

| Decision                                   | Reason                                                                                      | Alternative not taken                  |
|--------------------------------------------|---------------------------------------------------------------------------------------------|----------------------------------------|
| Rust and Embassy                           | Requested; async tasks fit WiFi, USB and UI; strong types for the parsers                   | C (Waveshare demo has no WiFi), Python |
| `core` / `firmware` split                  | Most bugs are in logic; logic can be tested on the host in milliseconds                     | One crate with `cfg` for tests         |
| RTC holds UTC                              | DST needs no sync; correct UTC for the NTP server                                           | RTC holds local time                   |
| Generated zone table + POSIX rules         | Names for users, small rules for the device                                                 | Full tz database on flash              |
| One config model, text storage             | One validator for three front ends; migratable between firmware versions                    | Binary struct in flash                 |
| Display on a high-priority interrupt executor | WiFi must not cause flicker                                                              | Scan in the main executor              |
| RTC written on a second boundary           | RTC seconds then match UTC; enables an accurate software clock                              | Write immediately                      |
| NTP server serves a software clock         | Sub-second time the RTC cannot provide                                                      | Serve whole RTC seconds (±1 s)         |
| Stratum 5, 24 h freshness for the server   | Honest about quality; clients with better sources prefer them                               | Claim stratum 2                        |
| Plain HTTP geolocation, opt-in             | Only free option; untrusted reply is validated against a table                              | HTTPS paid API, or none                |
| Single-connection web server               | Tiny RAM use; adequate for a settings page                                                  | Concurrent connections                 |
| Verification records beside unit tests     | Hardware runs are evidence too and must be dated and tied to requirements                   | Claims in the README only              |

## 10. Extension points (roadmap)

- **Further time sources (DCF77, u-blox GPS).** Today the only producer of time is `wifi_task` through `NTP_TIME`.
  A source abstraction would let several producers publish (seconds, sub-second, quality) and let `ui_task` pick the
  best; a GPS with a PPS pulse could replace the anchor's free-running phase and allow a lower stratum. Needs free
  GPIOs that reach the clock's case.
- **HTTPS and NTS.** Both need TLS 1.3 and certificate handling. The feasibility spike (ADR-0001, ADR-0002) showed that
  they do not share a library after all: NTS needs a TLS *client* (`embedded-tls` plus an exporter patch works on both
  chips) and HTTPS needs a TLS *server* (nothing vetted builds for the RP2040). Open questions: a `no_std` TLS server, RAM and flash budget, certificate storage and upload,
  and a per-device generated certificate instead of a shipped default key.
- **DHCP option 42, mDNS, strongest-network selection** touch only `net.rs`.

## 11. Testing strategy

- **Unit tests** in `core` (`cargo test -p picoclock-core --target <host>`; the count is in the traceability matrix), each bound to
  the requirement it verifies by a `// VERIFIES:` comment.
- **Mutation checks**: planting a bug and confirming a test fails, done by hand for DST, alarm, HTTP, CRC and SNTP.
- **Hardware runs** are recorded in the [verification log](../verification-log.md), with date and what was observed.
- **Traceability gate**: `tools/trace.py check` fails on a ratified requirement without evidence, source files owned
  by no capability, dangling references, bound tests that did not run, and stale generated documents.
- **Not covered by automated tests**: the display, buttons, buzzer, light sensor, USB and WiFi drivers, and the
  firmware tasks. They are exercised by hand on the clock only.
