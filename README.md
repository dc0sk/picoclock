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
| `ntp_servers` | comma-separated, tried in order; `gateway` = DHCP router | `gateway,pool.ntp.org` |
| `timezone` | `utc`, `auto`, IANA name or POSIX rule | `utc` |
| `dst` | `on`: apply daylight saving time; `off`: the zone's standard time all year (no effect for `utc`) | on |
| `hour12`, `fahrenheit` | display options | off |
| `brightness` | `auto` (light sensor) or 1-100 | `auto` |
| `key_beep`, `hourly_chime` | buzzer | on / off |
| `alarm_enabled`, `alarm_time` | daily alarm at `HH:MM` local time | off, 07:00 |
| `web_enabled`, `web_password` | web page; if a password is set, user is `admin` | on, none |
| `ntp_serve` | `on`: answer NTP requests from the LAN on UDP 123 (see below) | off |

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

There is no suitable `no_std` crate, so the firmware makes one request to `http://ip-api.com/json` (free for
non-commercial use, HTTPS is paid). That is **opt-in**: it tells ip-api.com your public IP and it is unauthenticated,
so a hostile network could answer with a wrong zone. The reply is only used if it names a zone in the built-in table.
The result is cached (`geo_rule`) and refreshed daily; run `geo` to look up now.

## How it is organised

Documentation is in [`docs/`](docs/README.md): [architecture and design](docs/design/architecture.md), the
[software requirements specification](docs/srs.md), the [traceability matrix](docs/traceability-matrix.md) and the
[verification log](docs/verification-log.md). `tools/gate.sh` runs the unit tests, the traceability and document checks
and both firmware builds; run it before committing.

- `core/` hardware-independent, unit-tested on the host: calendar and POSIX-TZ maths, the generated zone table,
  SNTP packets, config model and storage format, terminal commands, HTTP parsing and page, button/menu/alarm
  state machine, frame buffer and font.
  `cargo test -p picoclock-core --target <host triple>` (add `-- --ignored` for a live pool.ntp.org query).
- `firmware/` Pico W application: display scan (own high-priority interrupt executor, so WiFi cannot make it flicker),
  DS3231, buttons, buzzer, USB console, web server, WiFi/NTP/geolocation, flash storage.
- `tools/gen_tzdb.py` regenerates `core/src/tzdb.rs` from the system tz database.

Flash layout: program image, then a reserved 4 KiB sector at the end of the 2 MiB flash for settings
(2 KiB record with a CRC32; a corrupt or blank sector means defaults). Embassy is pinned by git revision in `firmware/Cargo.toml`
because the networking APIs used here are newer than the crates.io releases. The CYW43 firmware blobs in
`firmware/fw/` come from the Embassy repository under their own license.

## Status and limits

The authoritative, generated status is the [traceability matrix](docs/traceability-matrix.md): which requirement is
checked by which unit test or dated hardware run, and which have never been seen working. The summary below is prose.

**Checked on a real clock** (Pico W, flashed through a Raspberry Pi Debug Probe): USB console and all its commands,
RTC and its temperature, the RP2040 temperature, flash storage across a reboot, WiFi join, failover from a dead slot to
slot 2, DHCP hostname (resolved by the router), NTP sync, the web page (settings form, zone list, 404, refusal of a
cross-site POST), and the NTP server (replies, off switch, junk packets ignored).

**Checked on the host:** the core crate's unit tests, mutation-checked by hand (bugs planted in DST, alarm, HTTP, CRC, SNTP client and server code were each caught); every one of the 486 zones parses and gives sane offsets; the SNTP exchange and the ip-api response format were checked against the real servers.

**Pico 2 W:** the RP2350 build compiles without warnings; the image has the boot block in the first 4 KiB, ends
below the settings sector, and converts to an RP2350 UF2. It has **never run on a Pico 2 W**: expect to check WiFi
SPI speed, the settings sector, the temperature sensor and the high-priority display interrupt first.

**Checked by the owner by eye and ear** (a user report, not a measurement): the display looks right, and the buttons
and the buzzer work. The long-press menu, the alarm, the hourly chime and the key-beep setting were not singled out.

**Not checked yet:** the light sensor and auto-brightness, `timezone=auto` on the clock, and the web page's look in a
browser. Known unknowns: the polarity of the light sensor (copied from the demo: high reading = dark), and whether
the count-up icon suits the sync indicator.

Other limits: saving pauses the CPU for tens of milliseconds (the display blinks once); the hourly NTP sync pauses
the display for up to a second while the RTC is set on the second boundary; no countdown/stopwatch; the USB VID/PID
is the pid.codes test pair `1209:0001`. The firmware image is about 440 KB of flash and 72 KB of static RAM.

## Roadmap

Planned for future releases (requirements, not implemented):

- **HTTPS** for the web page, with a custom certificate that can be uploaded (web page and console) and a default
  certificate included. Note: a default key shipped in an open-source repository is public, so it protects nothing
  against an attacker; a per-device self-signed certificate generated on first boot is the better default. There is
  no mature `no_std` TLS *server* crate, so this needs a design decision (and RAM/flash budget) first.
- **Secure NTP (NTS, RFC 8915)** for the NTP client. This needs a TLS 1.3 client for the key-exchange step (TCP 4460),
  server certificate validation, and AES-SIV authenticated NTP packets with a cookie store. Design notes: the
  default `pool.ntp.org` does not offer NTS and a typical router does not either, so NTS needs its own server list
  (for example `time.cloudflare.com`) and a per-server `nts` option; certificate checks need a roughly correct clock,
  which the battery-backed RTC provides but a first boot with a dead battery does not (the plain NTP fallback and
  its trade-off must be an explicit setting); and the root-certificate store costs flash. The TLS pieces are shared
  with the HTTPS item above, so the two should be designed together.
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
