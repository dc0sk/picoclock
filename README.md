# picoclock

Rust firmware for the Waveshare [Pico-Clock-Green](https://www.waveshare.com/wiki/Pico-Clock-Green) with WiFi, NTP,
a USB terminal, a web configuration page and full use of the clock's hardware. Built on [Embassy](https://embassy.dev).

**Requires a Raspberry Pi Pico W** (RP2040) **or Pico 2 W** (RP2350), both with the CYW43439 radio, in the clock.
A plain Pico or Pico 2 has no WiFi. The Pico W is tested on hardware; the Pico 2 W build is not (see *Status*).

## Features

- **Time:** the DS3231 RTC always holds **UTC**. Local time is computed at display time from the configured
  timezone, including daylight saving, so DST changes need no sync. `timezone=utc` shows UTC directly.
- **Timezone:** `utc`, an IANA name (`Europe/Berlin`, 486 built in), a POSIX TZ rule
  (`CET-1CEST,M3.5.0,M10.5.0/3`), or `auto` to look it up from your public IP.
- **NTP client:** servers tried in order, default `gateway,pool.ntp.org` (`gateway` = your DHCP router). Hourly
  resync; the RTC is set exactly on a UTC second boundary, so the displayed seconds match real time.
- **Secure NTP (NTS, RFC 8915):** an `nts:<host>` entry in `ntp_servers` (for example `nts:time.cloudflare.com`)
  authenticates the time with TLS 1.3 and AES-SIV; see *Secure NTP* below.
- **NTP server (optional):** `ntp_serve on` lets other devices on the LAN use the clock as a time source. Off by default.
- **WiFi:** up to 8 stored networks, tried in slot order. The device name on the network is
  `picoclock-<last MAC octet>`.
- **Configuration** in three ways, all validated by the same code and stored in flash:
  USB serial terminal, web page (port 80), and the buttons for the everyday settings.
- **Hardware:** RTC with its temperature sensor plus the RP2040 die sensor (`chip` in `status`; it reads warmer
  than the room), buzzer (key beep, hourly chime, alarm), ambient-light auto-brightness, and the three buttons.

## Build and flash

```sh
rustup target add thumbv6m-none-eabi
cargo install elf2uf2-rs --locked
cargo build --release -p picoclock
# Hold BOOTSEL while plugging in the Pico, then:
elf2uf2-rs -d target/thumbv6m-none-eabi/release/picoclock
```

For the **Pico 2 W** build the other chip feature and target:

```sh
rustup target add thumbv8m.main-none-eabihf
cargo build --release -p picoclock --no-default-features --features rp235x --target thumbv8m.main-none-eabihf
elf2uf2-rs -d target/thumbv8m.main-none-eabihf/release/picoclock    # BOOTSEL as above
```

The two builds differ only in the chip feature (`rp2040` default / `rp235x`), the memory map (`firmware/memory-*.x`;
the settings sector is the last 4 KiB of the 2 MiB / 4 MiB flash), the RP2350 `IMAGE_DEF` boot block and the WiFi
SPI clock divider. Everything else, including the pin map, is shared.

To flash with a debug probe instead (for example the Raspberry Pi Debug Probe, CMSIS-DAP, with OpenOCD):

```sh
openocd -f interface/cmsis-dap.cfg -f target/rp2040.cfg -c "adapter speed 5000" \
  -c "program target/thumbv6m-none-eabi/release/picoclock verify reset exit"
```

Nothing is configured at build time. After the first flash, configure over USB (below). Once running you can
re-enter flashing mode with the `bootsel` terminal command.

## Configure over USB

Plug the clock into a computer and open the serial port (115200 8N1, any speed works):

```sh
picocom /dev/ttyACM0        # or: screen /dev/ttyACM0 115200
```

```
picoclock> set wifi1_ssid My Network
picoclock> set wifi1_password my-passphrase
picoclock> set timezone Europe/Berlin       # or: utc | auto | CET-1CEST,M3.5.0,M10.5.0/3
picoclock> save
picoclock> status
```

`help` lists every command and setting; `tz europe/` lists matching zone names; `time` shows UTC and local time.
Changes apply immediately but are only written to flash by `save` (the web page and the buttons save for you).

| setting | meaning | default |
|---|---|---|
| `wifi1_ssid`, `wifi1_password` ... `wifi8_ssid`, `wifi8_password` | up to 8 networks, tried in slot order (empty SSID = unused slot; empty password = open network, else 8-63 characters). `wifi_ssid` / `wifi_password` are aliases for slot 1 | empty |
| `ntp_servers` | comma-separated, tried in order; `gateway` = DHCP router; `nts:<host>` = authenticated NTP (NTS) | `gateway,pool.ntp.org` |
| `timezone` | `utc`, `auto`, IANA name or POSIX rule | `utc` |
| `dst` | `on`: apply daylight saving time; `off`: the zone's standard time all year (no effect for `utc`) | on |
| `hour12`, `fahrenheit` | display options | off |
| `brightness` | `auto` (light sensor) or 1-100 | `auto` |
| `key_beep`, `hourly_chime` | buzzer | on / off |
| `alarm_enabled`, `alarm_time` | daily alarm at `HH:MM` local time | off, 07:00 |
| `web_enabled`, `web_password` | web page; if a password is set, user is `admin` | on, none |
| `ntp_serve` | `on`: answer NTP requests from the LAN on UDP 123 (see below) | off |

## Secure NTP (NTS)

Put `nts:` in front of a server name and the clock authenticates that server's time (RFC 8915):

```
picoclock> set ntp_servers nts:time.cloudflare.com,pool.ntp.org
picoclock> save
```

The clock first runs NTS-KE: a TLS 1.3 connection (port 4460) in which it checks the server's certificate chain and
host name against a built-in root and the RTC's date, and receives *cookies* and the keys. Every NTP request after that
carries a cookie and an AES-SIV authenticator, and a reply is accepted only if its authenticator verifies and it answers
that very request. `status` says `synced : 5 s ago (NTS, authenticated)` or `(plain NTP)`.

What you need to know:

- **Fallback is the list.** If an `nts:` entry fails, the next entry is tried, whatever kind it is. A list made only of
  `nts:` entries never falls back to plain NTP. The default list stays `gateway,pool.ntp.org`.
- **It needs a good RTC time** to check certificate dates; without one the key exchange is not attempted and the next
  entry is used. (A clock that has never been set gets its first time from a plain entry.)
- **Which servers work:** those whose certificate chain is ECDSA and ends at ISRG Root X2 or the SSL.com ECC root:
  `time.cloudflare.com`, `nts.time.nl`, `ntppool1.time.nl`, `nts.teambelgium.net`, `ntp3.fau.de`, `time.txryan.com`, and any
  server with a Let's Encrypt ECDSA certificate. Servers with RSA certificates (Netnod, PTB, System76) are not supported.
- **The key exchange takes 5 to 7 seconds** on the RP2040, during which the console, the web page and the buttons wait
  (the display stays lit). It happens once after boot; later hourly syncs reuse the cookies and are as fast as plain NTP.
- **The certificate is checked after the key exchange, not during the handshake.** The clock's CPU needs about five
  seconds for the checks, and NTS servers drop a client that is silent for more than about two seconds in the middle of the
  handshake. Nothing the server sent is used before the checks pass, and NTS-KE sends no secret, so this is safe for this
  protocol; the reasoning and the measurements are in [ADR-0001](docs/design/adr/0001-tls-client-for-nts.md).
- The RP2040 has no hardware random number generator; the keys and identifiers use the ring-oscillator source.

## Network name and NTP server

The clock registers itself on the network as `picoclock-<last MAC octet>` (for example `picoclock-54`), via the
DHCP hostname; `status` shows it. Routers such as a FRITZ!Box then resolve `picoclock-54.fritz.box`.

With `ntp_serve on` the clock answers NTP client requests (SNTP, UDP 123), so other devices can use it as a time
source. It is off by default. The RTC is set exactly on a UTC second boundary after each sync and then a software
clock runs until the next hourly sync. Measured against an NTP-synced host right after a sync the offset was 1-2 ms;
drift of the RP2040 crystal can add some 0.1 s by the next resync.

Replies are marked stratum 5. If no NTP sync happened in the last 24 hours they are marked unsynchronised and clients
ignore them. A reply is never larger than the request, and anything that is not a client request is dropped. Do not
forward port 123 from the internet to the clock.

## Web page

Find the clock's IP with `status` (or your router) and open `http://<ip>/`. The page shows status and all settings.
**It is plain HTTP**: the web password and WiFi passphrase cross your LAN unencrypted, and without a web password
anyone on the LAN can change the settings. Set `web_password`, or `set web_enabled off`, if that matters to you.
Password fields are never echoed; leave them blank to keep the current value. Cross-site form posts are refused.

## Buttons

| | short press | hold |
|---|---|---|
| **SET** | next view: time, date, temperature | open the settings menu (SET = next item, SET held or 10 s idle = leave) |
| **UP / DOWN** | brightness step up / down (auto, 20, 40 ... 100 %) | in the menu: change value, auto-repeating |

Menu items in order: alarm on/off, alarm hour, alarm minute, hourly chime, key beep, 12/24 h, C/F, brightness.
While the alarm rings, any button stops it. Menu and brightness edits are saved a few seconds after the last press.

Status icons: alarm = alarm enabled, bell = hourly chime, auto-light = auto brightness, AM/PM in 12 h mode,
C/F = unit, **count-up icon = time was synced by NTP within the last 3 h**. The colon blinks once synced and stays
solid before. The display shows `--:--` until the RTC holds a valid time.

## Timezone lookup by IP (`timezone=auto`)

There is no suitable `no_std` crate, so the firmware makes one plain-HTTP request to a free service: `ipwho.is`,
and `ip-api.com` if that gives no usable answer (HTTPS is paid or needs a TLS client). That is **opt-in**: it tells the
service your public IP and it is unauthenticated, so a hostile network could answer with a wrong zone. The reply is only
used if it names a zone in the built-in table. The result is cached (`geo_rule`) and refreshed daily; run `geo` to look
up now. The `geo:` line of `status` shows the provider and stage of the last lookup. Two providers are used because a
service can be unreachable from a given network (see *Status*).

## How it is organised

Documentation is in [`docs/`](docs/README.md): [architecture and design](docs/design/architecture.md), the
[software requirements specification](docs/srs.md), the [traceability matrix](docs/traceability-matrix.md) and the
[verification log](docs/verification-log.md). `tools/gate.sh` runs the unit tests, the traceability and document checks
and both firmware builds; run it before committing.

- `core/` hardware-independent, unit-tested on the host: calendar and POSIX-TZ maths, the generated zone table,
  SNTP packets, config model and storage format, terminal commands, HTTP parsing and page, button/menu/alarm
  state machine, frame buffer and font.
  `cargo test -p picoclock-core --target <host triple>` (add `-- --ignored` for a live pool.ntp.org query).
- `firmware/` Pico W / Pico 2 W application: display scan (own high-priority interrupt executor, so WiFi cannot make it flicker),
  DS3231, buttons, buzzer, USB console, web server, WiFi/NTP/geolocation, flash storage.
- `vendor/embedded-tls/` the TLS 1.3 client used for NTS-KE, a snapshot of the upstream repository with a small patch
  (the TLS exporter); see `vendor/embedded-tls/PATCHES.md`. `core/roots/` holds the two trusted root certificates.
- `spikes/` throwaway experiments that answered design questions with measurements (see its README).
- `tools/gen_tzdb.py` regenerates `core/src/tzdb.rs` from the system tz database.

Flash layout: program image, then a reserved 4 KiB sector at the end of the 2 MiB flash for settings
(2 KiB record with a CRC32; a corrupt or blank sector means defaults). Embassy is pinned by git revision in `firmware/Cargo.toml`
because the networking APIs used here are newer than the crates.io releases. The CYW43 firmware blobs in
`firmware/fw/` come from the Embassy repository under their own license.

## Debugging

`status` on the USB terminal shows more than the network: both temperatures, the light sensor's raw value, the display
scan's timing (`scan:` worst lateness of the row timer and the number of waits more than 100 µs late; a growing count
means something is holding interrupts off) and the stage of the last location lookup (`geo:`).

Firmware logs (defmt) go to RTT, not to the USB terminal. With a debug probe:

```sh
cargo install defmt-print --locked
openocd -f interface/cmsis-dap.cfg -f target/rp2040.cfg -c "adapter speed 5000" \
  -c "init; reset halt; rtt setup 0x$(nm target/thumbv6m-none-eabi/release/picoclock | awk '/ _SEGGER_RTT$/{print $1}') 0x1000 \"SEGGER RTT\"; rtt server start 9090 0; resume; rtt start" &
nc localhost 9090 | defmt-print -e target/thumbv6m-none-eabi/release/picoclock
```

Without a probe the clock can still be reflashed: `bootsel` on the terminal, then copy the `.uf2` file to the drive
that appears.

## Status and limits

The authoritative, generated status is the [traceability matrix](docs/traceability-matrix.md): which requirement is
checked by which unit test or dated hardware run, and which have never been seen working. The summary below is prose.

**Checked on a real clock** (Pico W, flashed through a Raspberry Pi Debug Probe): USB console and all its commands,
RTC and its temperature, the RP2040 temperature, flash storage across a reboot, WiFi join (with a timeout and retries
after a lost event or a rejected first attempt), failover to another slot, DHCP hostname (resolved by the router), NTP
sync, the daylight-saving option, the web page (settings form, zone list, 404, refusal of a cross-site POST, web
password), and the NTP server (replies, off switch, junk packets ignored, offset 1 to 5 ms right after a sync).

**NTS:** works on the clock against `time.cloudflare.com` and `nts.time.nl` (key exchange 5.0 s and 6.9 s, then reused
cookies); the whole client is also tested against those servers from a PC (`cargo test -p picoclock-core --test nts_live --
--ignored`, needs internet), the TLS exporter against OpenSSL, and the certificate checks against a three-level chain made
with OpenSSL (these run in `tools/gate.sh`, which needs the `openssl` binary). Not covered: RSA servers (refused), the Pico 2 W
(builds only), and a server that sends a malformed key exchange message beyond what the unit tests construct.

**Checked on the host:** the core crate's unit tests, mutation-checked by hand (bugs planted in DST, alarm, HTTP, CRC, SNTP client and server code were each caught); every one of the 486 zones parses and gives sane offsets; the SNTP exchange and the ip-api response format were checked against the real servers.

**Pico 2 W:** the RP2350 build compiles without warnings; the image has the boot block in the first 4 KiB, ends
below the settings sector, and converts to an RP2350 UF2. It has **never run on a Pico 2 W**: expect to check WiFi
SPI speed, the settings sector, the temperature sensor and the high-priority display interrupt first.

**Checked by the owner by eye and ear** (a user report, not a measurement): the display looks right, and the buttons,
the buzzer, the alarm, the settings menu, the hourly chime and the light sensor work. The key-beep setting was not
singled out, and the light-sensor thresholds were not measured.

**`timezone=auto`:** works on the owner's network through `ipwho.is` (answer in about 4 s). `ip-api.com`, the first
provider this firmware used, does not work from the clock on that network: it accepts the connection but never
acknowledges the request, although a PC on the same network gets its reply. The cause is not found, so `ip-api.com`
is only the fallback. The move from a failing first provider to a working one was exercised with a deliberately broken
entry (see *Fallbacks tested*).

**Display flicker, fixed:** rows flickered 1 to 3 times a second because a settings lock switched interrupts off long
enough to delay the display scan. Measured before and after (`status` shows the scan's lateness), and the owner confirms
the flicker is gone. A settings save still pauses the scan for about 34 ms.

**Fallbacks tested on the clock:** the second NTP server is used when the first fails; the next WiFi slot is used when
a slot is wrong or absent (a wrong passphrase on the active slot costs about 37 s before the next slot is tried);
the location lookup moves to the next provider and keeps its cached rule when all fail. The web page is in daily use by
the owner.

**Not checked yet:** a location provider that connects but sends a bad reply. A known unknown: whether the count-up
icon suits the sync indicator.

Other limits: saving pauses the CPU for tens of milliseconds (the display blinks once; measured 34 ms); the hourly NTP sync pauses
the display for up to a second while the RTC is set on the second boundary; no countdown/stopwatch; the USB VID/PID
is the pid.codes test pair `1209:0001`. The firmware image is about 440 KB of flash and 72 KB of static RAM.

## Roadmap

Planned for future releases (requirements, not implemented):

- **HTTPS** for the web page ([ADR-0002](docs/design/adr/0002-https-server-stack.md), decided): `rustls` on the **Pico 2 W
  only**, because it does not build for the RP2040, so the Pico W keeps plain HTTP. The certificate is generated on the
  device at first boot (self-signed, fingerprint in `status`) and can be replaced by an uploaded certificate and key; no
  key is shipped in the repository, since a shipped key would be public. First step: run the Pico 2 W build on real
  hardware. Estimated cost of a handshake: under a second.
- **NTS beyond what works today:** RSA certificate chains (Netnod, PTB, System76 servers), user-supplied roots, a TRNG
  for the Pico 2 W, and offering the exporter patch upstream.
- **DCF77 receiver module** as an additional time source (a free GPIO and a module that fits the clock's case).
- **GPS module (u-blox)** as a time source over UART (NMEA/UBX, optionally the PPS pulse for sub-millisecond
  accuracy), which would also let the NTP server claim a low stratum.
- Pico 2 W: run and verify on hardware.
- Scan for the strongest known WiFi network instead of trying slots in order.
- DHCP option 42 (NTP server from the router) and mDNS (`picoclock.local`).

## Provisioning from a local file

Put your WiFi credentials in `local/wifi.conf` (git-ignored; one `key value` per line, e.g. `wifi1_ssid MyNet`; `wifi2_*` ... `wifi8_*` for more networks),
then run `tools/provision.py` with the clock plugged in. It sets the values over the USB console, saves them
and prints the status once WiFi has had time to join. Needs `pyserial`.

## License

AGPL-3.0-or-later; see `LICENSE`. If you run a modified version where others interact with it over a network, the
AGPL requires you to offer them the source. The display protocol, pin map and font table were ported from
Waveshare's Pico-Clock-Green C demo, whose license I could not find stated; check it before redistributing.
The CYW43 firmware blobs in `firmware/fw/` stay under their own Permissive Binary License. Embassy and the other
dependencies are MIT/Apache-2.0.
