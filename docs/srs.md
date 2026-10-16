---
project: picoclock
doc: docs/srs.md
status: living
last_updated: 2026-10-16
---

# Software requirements specification

> Generated from `docs/requirements.toml` by `tools/trace.py render`. Do not edit by hand.

## 1. Purpose and scope

This document specifies the firmware for the Waveshare Pico-Clock-Green fitted with a Raspberry Pi Pico W or
Pico 2 W: an LED matrix clock that keeps UTC in its DS3231 real-time clock, synchronises over WiFi/NTP, shows local
time with daylight saving, and is configured over a USB serial terminal, a web page and three buttons.

Each requirement has a stable ID. A ratified statement is never edited: it is retired with a reason and replaced by a
new ID. Evidence for each requirement is in the [traceability matrix](traceability-matrix.md).

## 2. Definitions

| Term       | Meaning                                                                                  |
|------------|------------------------------------------------------------------------------------------|
| draft      | Proposed or on the roadmap; code may be written against it but it is not yet agreed.     |
| ratified   | Agreed. Needs evidence in proportion to its criticality, or a stated reason why not.     |
| retired    | Withdrawn; kept so history resolves.                                                     |
| criticality| high / medium / low: how much evidence a requirement must carry.                         |
| unverified | A ratified requirement for which no evidence exists yet; the reason is given.            |

## 3. Interfaces and constraints

- Hardware: Pico W (RP2040) or Pico 2 W (RP2350) with CYW43439 radio; DS3231 on I2C1 (0x68); 32x8 LED matrix on
  shift registers; three buttons; buzzer; ambient light sensor on ADC; USB device port.
- Network: WiFi station with DHCP; SNTP client (UDP 123), optional SNTP server (UDP 123), HTTP on port 80, one
  plain-HTTP request to ip-api.com when the timezone is `auto`.
- Language and platform: Rust, `no_std`, Embassy; logic that does not touch hardware lives in a host-testable crate.
- The web interface is plain HTTP. Anything on the LAN can read what crosses it.

## 4. Requirements


### 4.1 Time

**REQ-TIME-01** (ratified, high)  
The real-time clock shall hold UTC, and the displayed local time shall be computed from it at display time using the configured timezone.  
*Why:* A clock that stores local time needs a resync at every daylight-saving change and cannot serve correct UTC to others.  
*Implemented by:* Composition: task wiring, shared state, chip selection, Button, menu, alarm and view state machine, DS3231 driver, Calendar maths

**REQ-TIME-02** (ratified, medium)  
When the timezone is set to `utc`, the clock shall display UTC directly.  
*Why:* Requested as an alternative to any timezone.  
*Implemented by:* Button, menu, alarm and view state machine, Configuration model and flash storage

**REQ-TIME-03** (ratified, high)  
Calendar conversions between Unix time and civil date shall be correct across leap years, weekdays and the 2036 NTP era rollover.  
*Why:* Every other time feature builds on these conversions.  
*Implemented by:* Calendar maths, SNTP packets and client transport

**REQ-TIME-04** (ratified, high)  
After an NTP sync the real-time clock shall be set on a UTC second boundary, so its seconds match real time.  
*Why:* The DS3231 restarts its second when written; writing mid-second leaves it up to one second behind.  
*Implemented by:* Composition: task wiring, shared state, chip selection, DS3231 driver


### 4.2 Timezone

**REQ-TZ-01** (ratified, high)  
The clock shall apply daylight-saving rules at the correct instants, including southern-hemisphere, half-hour and negative-transition-time rules.  
*Why:* A wrong DST instant shows the wrong hour for a week.  
*Implemented by:* Timezone rules and zone table

**REQ-TZ-02** (ratified, high)  
The clock shall accept IANA zone names from a built-in table, case-insensitively, and every table entry shall resolve to a valid rule.  
*Why:* Users know zone names, not POSIX rules; the device has no internet-independent tz database.  
*Implemented by:* Timezone rules and zone table, Terminal commands and USB serial console

**REQ-TZ-03** (ratified, medium)  
The clock shall accept POSIX TZ rules as timezone values and reject malformed values without changing the setting.  
*Why:* Covers zones missing from the table.  
*Implemented by:* Timezone rules and zone table, Configuration model and flash storage

**REQ-TZ-04** (ratified, medium)  
When the timezone is `auto`, the clock shall look the zone up from its public IP, use the reply only if it names a built-in zone, cache the result and refresh it daily.  
*Why:* Requested location lookup; plain HTTP is the only free option, so the reply is untrusted input.  
*Implemented by:* IP geolocation reply parser, WiFi, hourly sync, geolocation request and NTP server, Configuration model and flash storage

**REQ-TZ-05** (ratified, medium)  
Daylight saving time shall be a setting that is on by default; when it is off, the clock shall use the zone's standard time all year.  
*Why:* Requested; some users keep standard time year-round, and the option must work for every zone, including those whose rule has a negative daylight-saving offset.  
*Implemented by:* Timezone rules and zone table, Configuration model and flash storage


### 4.3 NTP client

**REQ-NTP-01** (ratified, high)  
The clock shall synchronise over SNTP, trying the configured servers in order, defaulting to the DHCP gateway and pool.ntp.org.  
*Why:* Requested default servers.  
*Implemented by:* SNTP packets and client transport, WiFi, hourly sync, geolocation request and NTP server, Configuration model and flash storage

**REQ-NTP-02** (ratified, high)  
The client shall reject replies that are not server-mode, are unsynchronised, are stratum 0, or do not echo the request's transmit timestamp.  
*Why:* A forged or stale reply must not set the clock.  
*Implemented by:* SNTP packets and client transport

**REQ-NTP-03** (ratified, medium)  
The clock shall resynchronise hourly and on the `sync` command.  
*Why:* Keeps the RTC and the software clock close to real time.  
*Implemented by:* WiFi, hourly sync, geolocation request and NTP server, Terminal commands and USB serial console


### 4.4 NTP server

**REQ-SRV-01** (ratified, medium)  
The clock shall offer an NTP server on UDP 123 that is off by default and can be switched on and off at run time.  
*Why:* Requested option; exposing a service must be opt-in.  
*Implemented by:* Configuration model and flash storage, WiFi, hourly sync, geolocation request and NTP server

**REQ-SRV-02** (ratified, high)  
The server shall answer valid client requests with a server-mode reply that echoes the client's transmit timestamp and the request's version.  
*Why:* Clients reject replies that do not echo their timestamp.  
*Implemented by:* SNTP packets and client transport, WiFi, hourly sync, geolocation request and NTP server

**REQ-SRV-03** (ratified, high)  
The server shall ignore anything that is not a well-formed client request, shall never send a reply larger than the request, and shall mark replies unsynchronised when no NTP sync happened in the last 24 hours.  
*Why:* Prevents reflection and amplification, reply loops and serving stale time as good time.  
*Implemented by:* SNTP packets and client transport, WiFi, hourly sync, geolocation request and NTP server

**REQ-SRV-04** (ratified, medium)  
Right after a sync the server's time shall be within 100 ms of an NTP-synchronised reference.  
*Why:* Whole-second RTC time alone gave errors above one second; the server uses a software clock anchored at the RTC set instant.  
*Implemented by:* WiFi, hourly sync, geolocation request and NTP server, Composition: task wiring, shared state, chip selection


### 4.5 Network

**REQ-NET-01** (ratified, high)  
The clock shall store up to eight WiFi networks and join the first one in slot order that is reachable.  
*Why:* Requested; allows moving between locations without reconfiguring.  
*Implemented by:* Configuration model and flash storage, WiFi, hourly sync, geolocation request and NTP server

**REQ-NET-02** (ratified, medium)  
The clock's network name shall be `picoclock-` followed by the last octet of its MAC address in hexadecimal.  
*Why:* Requested; tells several clocks on one LAN apart.  
*Implemented by:* Configuration model and flash storage, WiFi, hourly sync, geolocation request and NTP server, Terminal commands and USB serial console

**REQ-NET-03** (ratified, medium)  
When the credentials of the active network change, the clock shall leave it and join again without a reboot.  
*Why:* Settings changes must take effect at once.  
*Implemented by:* WiFi, hourly sync, geolocation request and NTP server

**REQ-NET-06** (ratified, medium)  
A WiFi join attempt that does not finish within 15 seconds shall be abandoned, a transient failure shall be retried on the same network, and the clock shall keep trying until it is connected.  
*Why:* Found on the hardware: the radio driver waits for an event that can be lost, so one lost event left the clock offline until reset, and an access point sometimes rejects the first authentication.  
*Implemented by:* WiFi, hourly sync, geolocation request and NTP server

**REQ-NET-04** (draft, low)  
The clock shall scan and prefer the strongest known network instead of trying slots in order.  
*Why:* Roadmap.  
*Implemented by:* none yet

**REQ-NET-05** (draft, low)  
The clock shall accept NTP servers from DHCP option 42 and shall be reachable as picoclock.local over mDNS.  
*Why:* Roadmap.  
*Implemented by:* none yet


### 4.6 Configuration

**REQ-CFG-01** (ratified, high)  
The USB terminal, the web page and flash storage shall share one validated key/value model, and an invalid value shall leave the setting unchanged.  
*Why:* One parser removes divergence between the three front ends.  
*Implemented by:* Configuration model and flash storage, Terminal commands and USB serial console, HTTP parsing, settings page and web server

**REQ-CFG-02** (ratified, high)  
Settings shall persist in flash with a checksum; a blank or corrupt record shall give the defaults, and unknown keys or invalid stored values shall be skipped.  
*Why:* A torn write or a firmware downgrade must never brick the configuration.  
*Implemented by:* Configuration model and flash storage

**REQ-CFG-03** (ratified, high)  
Passwords shall never be displayed by the terminal or the web page, and a blank password field shall keep the current value.  
*Why:* Prevents shoulder-surfing and leaking secrets into page sources and screenshots.  
*Implemented by:* Configuration model and flash storage, HTTP parsing, settings page and web server, Terminal commands and USB serial console

**REQ-CFG-04** (ratified, medium)  
The defaults shall be: NTP servers `gateway,pool.ntp.org`, timezone `utc`, NTP server off, web page on.  
*Why:* Requested defaults.  
*Implemented by:* Configuration model and flash storage


### 4.7 Terminal

**REQ-CLI-01** (ratified, medium)  
The clock shall provide a terminal over USB serial with commands to show, get, set, save and reset settings, show status and time, list zones, sync, look up the zone, reboot and enter flashing mode.  
*Why:* Requested terminal-based configuration interface.  
*Implemented by:* Terminal commands and USB serial console

**REQ-CLI-02** (ratified, low)  
Zone listings shall be capped so one command cannot flood the console.  
*Why:* 486 names would scroll away any other output.  
*Implemented by:* Terminal commands and USB serial console


### 4.8 Web

**REQ-WEB-01** (ratified, medium)  
The clock shall serve a web page on port 80 that shows status and edits every setting, and the page shall fit the response buffer with all fields at maximum length.  
*Why:* Requested web-based configuration interface.  
*Implemented by:* HTTP parsing, settings page and web server

**REQ-WEB-02** (ratified, high)  
The HTTP parser shall be bounded: oversize, malformed, duplicate-length and hostile requests shall be refused without panicking or unbounded memory use.  
*Why:* The server faces the whole LAN.  
*Implemented by:* HTTP parsing, settings page and web server

**REQ-WEB-03** (ratified, high)  
A settings change submitted with a foreign Origin shall be refused.  
*Why:* Stops a web page in the user's browser from changing the clock's settings.  
*Implemented by:* HTTP parsing, settings page and web server

**REQ-WEB-04** (ratified, high)  
When a web password is set, the web page shall require HTTP Basic authentication with user `admin`.  
*Why:* Optional access control for a plain-HTTP interface.  
*Implemented by:* HTTP parsing, settings page and web server

**REQ-WEB-05** (ratified, medium)  
All setting values written into the page shall be HTML-escaped.  
*Why:* A stored SSID must not become script.  
*Implemented by:* HTTP parsing, settings page and web server

**REQ-WEB-06** (draft, high)  
The web page shall be available over HTTPS with a certificate the user can upload and a default certificate included.  
*Why:* Roadmap. A default private key shipped in an open repository is public; a per-device certificate generated on first boot is the better default. Needs a no_std TLS server.  
*Implemented by:* none yet


### 4.9 Display

**REQ-DSP-01** (ratified, high)  
The firmware shall compose the 32x8 matrix, weekday LEDs and status icons into the panel's row format so that glyphs, shifts and icons do not disturb each other.  
*Why:* The panel's shift-register layout has quirks (column offsets, group boundaries) inherited from the vendor demo.  
*Implemented by:* Frame buffer and font, LED matrix scan


### 4.10 Interaction

**REQ-UI-01** (ratified, high)  
The SET button shall cycle the time, date and temperature views and return to the time view after a timeout.  
*Why:* Requested button support.  
*Implemented by:* Button, menu, alarm and view state machine, Button polling

**REQ-UI-02** (ratified, medium)  
The display shall support 24-hour and 12-hour time and Celsius and Fahrenheit, with correct handling of negative and out-of-range temperatures.  
*Why:* Regional preferences.  
*Implemented by:* Button, menu, alarm and view state machine

**REQ-UI-03** (ratified, medium)  
UP and DOWN shall step the brightness through auto and fixed levels without wrapping.  
*Why:* Requested; everyday control without the menu.  
*Implemented by:* Button, menu, alarm and view state machine, Button polling

**REQ-UI-04** (ratified, medium)  
Holding SET shall open a settings menu covering alarm, chime, key beep, 12/24 h, C/F and brightness, which times out when idle.  
*Why:* Everyday settings without a computer.  
*Implemented by:* Button, menu, alarm and view state machine, Button polling

**REQ-UI-05** (ratified, high)  
An enabled alarm shall ring once at the configured local time, stop on any button or by itself, and not ring when disabled or when the clock is unset.  
*Why:* An alarm that retriggers or never stops is worse than none.  
*Implemented by:* Button, menu, alarm and view state machine, Buzzer driver

**REQ-UI-06** (ratified, low)  
The clock shall beep on button presses and on the hour when those settings are on, and the chime shall not sound over an alarm.  
*Why:* Requested buzzer support.  
*Implemented by:* Button, menu, alarm and view state machine, Buzzer driver

**REQ-UI-07** (ratified, low)  
The colon shall blink only while the time is NTP-synchronised, and the status icons shall reflect the settings.  
*Why:* The panel has no spare WiFi icon, so the colon and count-up icon carry the sync state.  
*Implemented by:* Button, menu, alarm and view state machine


### 4.11 Hardware

**REQ-HW-01** (ratified, medium)  
The clock shall read the DS3231 temperature sensor and show it on the display, the terminal and the web page.  
*Why:* Requested temperature sensor support.  
*Implemented by:* DS3231 driver, Button, menu, alarm and view state machine, Terminal commands and USB serial console

**REQ-HW-02** (ratified, low)  
The clock shall also read the RP2040 internal temperature sensor and show it in the terminal status.  
*Why:* Requested addition; it reads warmer than the room, so it is not shown as the room temperature.  
*Implemented by:* Composition: task wiring, shared state, chip selection, Terminal commands and USB serial console

**REQ-HW-03** (ratified, medium)  
The clock shall drive the buzzer for key beeps, the hourly chime and the alarm.  
*Why:* Requested buzzer support.  
*Implemented by:* Buzzer driver

**REQ-HW-04** (ratified, medium)  
The clock shall set the display brightness from the ambient light sensor when brightness is `auto`.  
*Why:* Requested ambient light sensor support.  
*Implemented by:* Composition: task wiring, shared state, chip selection, LED matrix scan  
*Unverified:* Light-sensor polarity (high reading = dark) is copied from the vendor demo and unconfirmed; the sensor reads, but dimming was not observed.

**REQ-HW-05** (ratified, high)  
The firmware shall run on the Raspberry Pi Pico W (RP2040).  
*Why:* The hardware installed in the clock.  
*Implemented by:* Composition: task wiring, shared state, chip selection

**REQ-HW-06** (ratified, medium)  
The firmware shall run on the Raspberry Pi Pico 2 W (RP2350) from the same source, selected by a build feature.  
*Why:* The user may fit a Pico 2 W.  
*Implemented by:* Composition: task wiring, shared state, chip selection  
*Unverified:* The RP2350 image builds and converts to a UF2, but has never run on a Pico 2 W.

**REQ-HW-07** (draft, low)  
The clock shall accept a DCF77 receiver module and a u-blox GPS module as additional time sources.  
*Why:* Roadmap. Needs free GPIOs that reach the clock's case.  
*Implemented by:* none yet

**REQ-HW-08** (draft, medium)  
The clock shall support secure NTP (NTS, RFC 8915) for its upstream servers.  
*Why:* Roadmap. Needs TLS 1.3, certificate validation, AES-SIV and a cookie store; shares its TLS work with HTTPS.  
*Implemented by:* none yet


### 4.12 Non-functional

**REQ-NFR-01** (ratified, medium)  
The firmware image and the settings sector shall fit the flash of the target board without overlapping, and the static RAM shall fit the chip.  
*Why:* The settings sector sits after the program image.  
*Implemented by:* Composition: task wiring, shared state, chip selection, Configuration model and flash storage

**REQ-NFR-02** (ratified, high)  
The display scan shall run at a priority above all networking tasks.  
*Why:* WiFi activity must not make the display flicker.  
*Implemented by:* Composition: task wiring, shared state, chip selection, LED matrix scan  
*Unverified:* Designed in (interrupt executor at higher priority) but flicker was not observed or measured on hardware.
