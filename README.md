# picoclock

Rust firmware for the Waveshare [Pico-Clock-Green](https://www.waveshare.com/wiki/Pico-Clock-Green) with WiFi, NTP,
a USB terminal, a web configuration page and full use of the clock's hardware. Built on [Embassy](https://embassy.dev).

**Requires a Raspberry Pi Pico W** (RP2040 + CYW43439 radio) in the clock. A plain Pico has no WiFi. (Pico 2 W:
see *Roadmap*.)

## Features

- **Time:** the DS3231 RTC always holds **UTC**. Local time is computed at display time from the configured
  timezone, including daylight saving, so DST changes need no sync. `timezone=utc` shows UTC directly.
- **Timezone:** `utc`, an IANA name (`Europe/Berlin`, 486 built in), a POSIX TZ rule
  (`CET-1CEST,M3.5.0,M10.5.0/3`), or `auto` to look it up from your public IP.
- **NTP:** servers tried in order, default `gateway,pool.ntp.org` (`gateway` = your DHCP router). Hourly resync.
- **Configuration** in three ways, all validated by the same code and stored in flash:
  USB serial terminal, web page (port 80), and the buttons for the everyday settings.
- **Hardware:** RTC with its temperature sensor plus the RP2040 die sensor (`chip` in `status`; reads warmer than the room), buzzer (key beep, hourly chime, alarm), ambient-light
  auto-brightness, and the three buttons.

## Build and flash

```sh
rustup target add thumbv6m-none-eabi
cargo install elf2uf2-rs --locked
cargo build --release -p picoclock
# Hold BOOTSEL while plugging in the Pico, then:
elf2uf2-rs -d target/thumbv6m-none-eabi/release/picoclock
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
clock runs until the next hourly sync, giving an offset of a few milliseconds when measured against a reference
right after a sync (drift of the RP2040 crystal can add some 0.1 s by the next resync). Replies are marked stratum 5; if no NTP sync happened in the last 24 hours they are marked unsynchronised and clients ignore
them. Replies are never larger than the request, and anything that is not a client request is dropped. Do not forward port 123 from the internet to it.

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

- `core/` hardware-independent, unit-tested on the host: calendar and POSIX-TZ maths, the generated zone table,
  SNTP packets, config model and storage format, terminal commands, HTTP parsing and page, button/menu/alarm
  state machine, frame buffer and font.
  `cargo test -p picoclock-core --target <host triple>` (add `-- --ignored` for a live pool.ntp.org query).
- `firmware/` Pico W application: display scan (own high-priority interrupt executor, so WiFi cannot make it flicker),
  DS3231, buttons, buzzer, USB console, web server, WiFi/NTP/geolocation, flash storage.
- `tools/gen_tzdb.py` regenerates `core/src/tzdb.rs` from the system tz database.

Flash layout: program image, then a reserved 4 KiB sector at the end of the 2 MiB flash for settings
(checksummed; a corrupt or blank sector means defaults). Embassy is pinned by git revision in `firmware/Cargo.toml`
because the networking APIs used here are newer than the crates.io releases. The CYW43 firmware blobs in
`firmware/fw/` come from the Embassy repository under their own license.

## Status and limits

**Verified:** 67 host unit tests pass (mutation-checked: 8 deliberately introduced bugs in DST, alarm, HTTP, CRC and
SNTP code were each caught); every one of the 486 zones parses and gives sane offsets; the SNTP exchange and the
ip-api response format were checked against the real servers; the firmware builds for the RP2040
(about 430 KB flash, 61 KB RAM).

**Not verified on hardware:** the display scan, RTC, buttons, buzzer, light sensor, USB console, WiFi join, web page and
flash storage have never run on a real clock. They are ports of the Waveshare demo and the Embassy examples, but treat
the first flash as a test. Known unknowns: whether the buzzer is the active type the demo drives, the polarity of the
light sensor (copied from the demo: high reading = dark), and whether the count-up icon suits the sync indicator.

Other limits: saving pauses the CPU for tens of milliseconds (the display blinks once); no countdown/stopwatch;
the USB VID/PID is the pid.codes test pair `1209:0001`.

## Roadmap

- Pico 2 W (RP2350): needs a chip feature, a different `memory.x` and flash size, and a second target; the
  hardware pin map and radio are the same.
- DHCP option 42 (NTP server from the router) and mDNS (`picoclock.local`).

## Provisioning from a local file

Put your WiFi credentials in `local/wifi.conf` (git-ignored; one `key value` per line, e.g. `wifi_ssid MyNet`),
then run `tools/provision.py` with the clock plugged in. It sets the values over the USB console, saves them
and prints the status once WiFi has had time to join. Needs `pyserial`.
