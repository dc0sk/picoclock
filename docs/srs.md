---
project: picoclock
doc: docs/srs.md
status: living
last_updated: 2026-10-20
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
  plain-HTTP request to an IP geolocation service when the timezone is `auto`.
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

**REQ-HW-08** (ratified, medium)  
The clock shall support secure NTP (NTS, RFC 8915) for its upstream servers.  
*Why:* Requested. Feasibility and design are in ADR-0001; the specific obligations are REQ-NTS-01 to REQ-NTS-07.  
*Implemented by:* NTS client: key exchange, authenticated packets, sessions, SNTP packets and client transport, WiFi, hourly sync, geolocation request and NTP server


### 4.4 NTS

**REQ-NTS-01** (ratified, high)  
A server entry `nts:<host>` in `ntp_servers` shall be contacted with NTS-KE over TLS 1.3 (ALPN ntske/1), and each NTP exchange with it shall then be authenticated with AES-SIV-CMAC-256 using the negotiated keys and a fresh cookie.  
*Why:* RFC 8915: authenticated time that a network attacker cannot forge.  
*Implemented by:* NTS client: key exchange, authenticated packets, sessions, SNTP packets and client transport, WiFi, hourly sync, geolocation request and NTP server, Configuration model and flash storage

**REQ-NTS-02** (ratified, high)  
The NTS client shall never set the clock from a response that is unauthenticated, forged, corrupted, not an answer to its own request or replayed, and shall repeat the key exchange after an NTS NAK.  
*Why:* An authenticated protocol is only as good as its refusals.  
*Implemented by:* NTS client: key exchange, authenticated packets, sessions

**REQ-NTS-03** (ratified, high)  
The NTS key exchange shall not start while the clock has no valid time, and the order of `ntp_servers` shall be the only fallback policy: a failed `nts:` entry is followed by the next entry, and a list made only of `nts:` entries never falls back to plain NTP.  
*Why:* Certificate dates cannot be checked without a time (the TLS library would skip the check silently), and a silent downgrade to unauthenticated time defeats the purpose.  
*Implemented by:* NTS client: key exchange, authenticated packets, sessions, SNTP packets and client transport, Configuration model and flash storage

**REQ-NTS-04** (ratified, medium)  
The terminal's `status` shall say whether the last sync was authenticated.  
*Why:* The user must be able to see that the time they rely on is the authenticated kind.  
*Implemented by:* Terminal commands and USB serial console

**REQ-NTS-05** (ratified, medium)  
The client shall hold up to eight cookies, spend one per request, take replacement cookies from each authenticated response, and repeat the key exchange only when it has no cookie left or after a NAK.  
*Why:* The key exchange costs several seconds of CPU on the clock; hourly syncs must not repeat it.  
*Implemented by:* NTS client: key exchange, authenticated packets, sessions

**REQ-NTS-06** (ratified, high)  
The TLS exporter used to derive the NTS keys shall produce exactly what OpenSSL produces for the same handshake.  
*Why:* The exporter is a patch to a vendored TLS library; a wrong derivation makes every server reject our requests, a subtly wrong one could weaken the keys.  
*Implemented by:* NTS client: key exchange, authenticated packets, sessions

**REQ-NTS-07** (retired, high)  
The server certificate chain, the host name, the validity dates and the CertificateVerify signature shall all be verified against a built-in root before any result of the key exchange (cookies, keys) is used, even though the checks run after the key exchange.  
*Why:* On the RP2040 the checks take about 5 s and NTS-KE servers drop a client that is silent for more than about 2 s in the middle of the handshake, so they are deferred. That is safe only because NTS-KE sends no secret and nothing is used before the checks pass.  
*Implemented by:* none yet  
*Retired:* Assumed the only trust anchors are built in. Users can now add their own (REQ-NTS-09), and a chain that ends at one of those is checked just as strictly, so the statement named the wrong set of roots.

**REQ-NTS-11** (ratified, high)  
The server certificate chain, the host name, the validity dates and the CertificateVerify signature shall all be verified against a trusted root (a built-in one or one the user added) before any result of the key exchange (cookies, keys) is used, even though the checks run after the key exchange; a chain that ends at no trusted root shall be refused.  
*Why:* On the RP2040 the checks take about 5 s and NTS-KE servers drop a client that is silent for more than about 2 s in the middle of the handshake, so they are deferred. That is safe only because NTS-KE sends no secret and nothing is used before the checks pass. All roots are tried against the one recorded chain, so a longer list costs no extra connection.  
*Implemented by:* NTS client: key exchange, authenticated packets, sessions

**REQ-NTS-08** (ratified, high)  
On the Pico 2 W the NTS key exchange shall also accept servers whose certificate chain uses RSA (2048 to 8192 bit keys, RSA-PSS signatures; the built-in ISRG Root X1 covers the Let's Encrypt RSA chains of Netnod, PTB and others). The Pico W build has no allocator for it and shall refuse every RSA chain, never accept one.  
*Why:* Roadmap item. The RSA code needs a heap, which only the Pico 2 W build has; the refusal on the Pico W must be a certificate error, not a silent pass.  
*Implemented by:* NTS client: key exchange, authenticated packets, sessions, WiFi order, DHCP option 42 and mDNS packet logic

**REQ-NTS-09** (ratified, high)  
The user shall be able to add up to four trust anchors for NTS as PEM certificates, on both boards through the USB terminal (`root add`, `root`, `root remove`) and on the Pico 2 W also through the HTTPS page, never through plain HTTP; they shall be kept in their own flash sector across reboots, listed with name, expiry and SHA-256 fingerprint, and a key exchange shall accept a server whose chain ends at any built-in or added root and at no other. A certificate that is unreadable, expired, already trusted, over the size limit, or (without RSA support) RSA shall be refused, and a refused paste shall change nothing.  
*Why:* Roadmap item. A private NTS server or a CA missing from the built-in list needs a way in; trusting a CA changes who the clock believes, so it is offered only over USB or TLS.  
*Implemented by:* NTS client: key exchange, authenticated packets, sessions, WiFi order, DHCP option 42 and mDNS packet logic, Terminal commands and USB serial console, HTTP parsing, settings page and web server, Web page TLS certificate: generate, inspect, validate

**REQ-NTS-10** (ratified, medium)  
The terminal's `status` shall show the outcome of the last NTS key exchange (server, accepted or why refused, and how long it took) and how many trust anchors are built in and added.  
*Why:* A refused certificate and a server that was never tried look the same without it; found while testing a private root on the clock, where the only sign of success was a later sync.  
*Implemented by:* Terminal commands and USB serial console, NTS client: key exchange, authenticated packets, sessions


### 4.5 NTP server

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


### 4.6 Network

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

**REQ-NET-04** (ratified, medium)  
With the setting `wifi_select strongest` the clock shall scan before joining and try the stored networks that are in range from the strongest signal down, then the stored networks it did not see in slot order; with `order` (the default) it shall use slot order as before.  
*Why:* Roadmap item, now built. A clock moved between rooms or access points should use the best network, without changing the default behaviour; a hidden network must not become unreachable.  
*Implemented by:* WiFi, hourly sync, geolocation request and NTP server, WiFi order, DHCP option 42 and mDNS packet logic, Configuration model and flash storage, Terminal commands and USB serial console

**REQ-NET-05** (ratified, medium)  
The clock shall request DHCP option 42 and accept the NTP servers the router offers as the `dhcp` entry of `ntp_servers`; only usable unicast addresses are taken (not 0.0.0.0, loopback, multicast or broadcast), at most four, and `status` shall list them or say that none were offered.  
*Why:* Roadmap item, now built. Part of the original draft that also named mDNS, which moved to REQ-NET-07.  
*Implemented by:* WiFi, hourly sync, geolocation request and NTP server, WiFi order, DHCP option 42 and mDNS packet logic, Configuration model and flash storage, Terminal commands and USB serial console

**REQ-NET-07** (ratified, medium)  
The clock shall answer multicast DNS queries (type A or ANY) for `<device name>.local` with its IPv4 address, and for no other name, unless the setting `mdns` is off; replies shall go to the querier if it did not use port 5353 and to the multicast group otherwise.  
*Why:* Roadmap item, now built; the HTTPS certificate already names `<device name>.local`, so the name should resolve. The device name is used rather than a fixed `picoclock.local` because two clocks on one LAN would collide.  
*Implemented by:* WiFi, hourly sync, geolocation request and NTP server, WiFi order, DHCP option 42 and mDNS packet logic, Configuration model and flash storage, Terminal commands and USB serial console

**REQ-NET-08** (ratified, medium)  
When several networks are stored, the clock shall try every one of them once before it retries any, and shall not retry a network whose join failed with an invalid passphrase or a network not found; a single stored network keeps all its attempts.  
*Why:* A wrong passphrase costs about ten seconds of handshake timeout per attempt, and three attempts in a row on a bad first slot kept the clock offline for about 30 s although the second slot was fine (measured on the Pico 2 W, 2026-10-17).  
*Implemented by:* WiFi, hourly sync, geolocation request and NTP server, WiFi order, DHCP option 42 and mDNS packet logic

**REQ-NET-09** (ratified, low)  
The clock shall answer ICMP echo requests (ping) sent to its IPv4 address.  
*Why:* A first thing a user tries to see whether the clock is on the network; the web page and mDNS name worked while ping did not because the network stack's ping reply option was not enabled.  
*Implemented by:* WiFi, hourly sync, geolocation request and NTP server

**REQ-NET-10** (ratified, medium)  
`status` shall show the DHCP server that gave the lease with the prefix length and the first DNS server, the signal of the WiFi link and the access points with the joined network's name that a scan right after the join saw (address, channel, signal, the one closest to the link signal marked), and say loudly when the server, address or gateway changed while the link stayed up; when the lease is gone the old address shall not be shown as current.  
*Why:* Investigation of 2026-10-18: the clock once got an address from another network (192.168.1.x) and nothing recorded which access point or server it had used. The chip chooses the access point itself and reports no address for it, so the closest signal is the best available indicator.  
*Implemented by:* WiFi, hourly sync, geolocation request and NTP server, Terminal commands and USB serial console


### 4.7 Configuration

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

**REQ-CFG-05** (ratified, medium)  
Settings changed on the USB terminal shall take effect at once and be written to flash only by `save`; the web page and the buttons save for the user.  
*Why:* What the terminal and the README say; found on the hardware that a debounced save also wrote terminal edits, so trying a value was not reversible by a reboot.  
*Implemented by:* Terminal commands and USB serial console, Configuration model and flash storage

**REQ-CFG-06** (ratified, medium)  
The software version, the source repository and the license shall be shown on the web page and by the terminal command `version` (alias `about`); the version is the package version, identical for the core and firmware packages.  
*Why:* Requested by the owner: a user of a running clock must be able to tell which build it runs and where the source is, as the AGPL expects.  
*Implemented by:* Terminal commands and USB serial console, HTTP parsing, settings page and web server, Configuration model and flash storage

**REQ-CFG-07** (ratified, medium)  
The WiFi MAC address shall be shown by the terminal command `status` and on the web page, as six hexadecimal bytes separated by colons.  
*Why:* Requested by the owner (2026-10-17): the address is needed to find the clock in a router's client list and to set up a DHCP reservation.  
*Implemented by:* Terminal commands and USB serial console, HTTP parsing, settings page and web server, WiFi, hourly sync, geolocation request and NTP server

**REQ-CFG-08** (ratified, medium)  
The setting `hostname` (terminal and web page) shall name the clock on the network: 1 to 24 lowercase letters, digits and hyphens, not starting or ending with a hyphen, upper case stored as lower case, anything else refused with the setting unchanged. The name is used for the DHCP host name, the mDNS name `<name>.local` and the names in a self-signed HTTPS certificate; empty means `picoclock-<last MAC block>`. A change takes effect without a reboot (the clock rejoins the network so the router learns the name, and makes a new self-signed certificate; an uploaded certificate is left alone).  
*Why:* Requested by the owner (2026-10-17). Two clocks on one LAN need different names; the MAC-based default stays so that an unconfigured clock is still unique.  
*Implemented by:* Configuration model and flash storage, Terminal commands and USB serial console, HTTP parsing, settings page and web server, WiFi, hourly sync, geolocation request and NTP server, HTTPS server (Pico 2 W): heap, rustls glue, certificate lifecycle and flash record

**REQ-CFG-09** (ratified, high)  
A factory reset shall be possible from the terminal (`factory-reset`) and from the web page. Each asks a second time and erases nothing unless the exact word RESET is entered; the question says what is erased: all settings including every WiFi network and password and the web password, the NTS roots the user added, and the HTTPS certificate and key. After the erase the clock restarts with the default settings. The web request is refused when it comes from another site.  
*Why:* Requested by the owner (2026-10-17), guarded by a second query because the erase cannot be undone and the clock is off the network afterwards until it is given WiFi over USB.  
*Implemented by:* Terminal commands and USB serial console, HTTP parsing, settings page and web server, Configuration model and flash storage, Composition: task wiring, shared state, chip selection

**REQ-CFG-10** (ratified, high)  
The saved settings shall be kept in two flash sectors, written one after the other, so that a power cut at any moment during a save leaves either the previous or the new settings and never a blank record; at start the first valid copy is used. A factory reset erases both.  
*Why:* Found by the full-code review of 2026-10-17: a save erased the only copy before writing the new one, so a power cut in that window brought the clock back with default settings, no WiFi networks and a new HTTPS certificate. Decided by the owner the same day.  
*Implemented by:* Configuration model and flash storage, Composition: task wiring, shared state, chip selection

**REQ-CFG-11** (ratified, medium)  
The NTS roots the user added and the HTTPS key and certificate shall each be kept in two flash sectors written one after the other, like the settings, so that a power cut while saving leaves the old or the new record; a record found in only one sector (written by an older version, or torn) shall be copied to the other at start, and a factory reset shall erase both copies.  
*Why:* Review of 2026-10-16: the roots and the certificate had a single sector each; a cut during the erase-write window lost them (the failure was only a missing root or a new self-signed certificate, but it is the same fault the settings backup already fixes). The free sectors before the settings made it possible without moving the bootloader layout.  
*Implemented by:* Configuration model and flash storage, Composition: task wiring, shared state, chip selection


### 4.8 Update

**REQ-UPD-01** (ratified, medium)  
The clock shall ask GitHub's releases API for the newest release of the project, once a day while connected (a first time a few minutes after joining) and whenever the user asks (`update check`, the Check button on the web page's Status page), and show whether a newer version exists in `status`, `update` and on the web page, with the address of the releases page. Checking can be switched off (`update_check off`). The check never downloads or installs anything.  
*Why:* Requested by the owner (2026-10-18): check first, notify, install only on the user's request (ADR-0006).  
*Implemented by:* Firmware update check (GitHub releases), WiFi, hourly sync, geolocation request and NTP server, Terminal commands and USB serial console, HTTP parsing, settings page and web server, Configuration model and flash storage

**REQ-UPD-02** (ratified, medium)  
Release tags shall be read as `major.minor.patch` (an optional leading `v`; anything else, such as a pre-release suffix, is unreadable) and compared by number; the answer shall be classed as newer, the same, ahead of the latest release, no release yet (404), rate limited (403, 429) or unreadable, and a check that cannot be read shall never claim a version.  
*Why:* A text comparison would call 0.10.0 older than 0.9.9, and a guess from a half-read answer could tell the owner to install something that does not exist.  
*Implemented by:* Firmware update check (GitHub releases)

**REQ-UPD-03** (ratified, high)  
The update check shall trust only the pinned USERTrust ECC Certification Authority root (not the NTS roots and not roots the user added), shall verify the certificate chain, the host name and the dates, shall refuse to run without a valid time, and shall use nothing of the answer unless those checks passed.  
*Why:* Decided in ADR-0006: the check is not what protects the install (a signature does), but a connection that tells the clock a new firmware exists must not be open to every public authority.  
*Implemented by:* Firmware update check (GitHub releases), NTS client: key exchange, authenticated packets, sessions, WiFi, hourly sync, geolocation request and NTP server

**REQ-UPD-04** (ratified, high)  
The clock shall start through a bootloader that keeps two flash slots (the running image and a staged one) and a state sector. When a staged image is marked, the bootloader swaps the two slots in steps that survive a power cut; the running image confirms itself only after its clock task has run for 20 seconds without a stall (never because WiFi joined), and an image that was swapped in but did not confirm is swapped back at the next start. The memory maps of the bootloader and the firmware shall describe the same layout, with the settings sectors at the end of flash outside both slots, and the one-time image shall carry an erased state sector.  
*Why:* Decided in ADR-0006 (points 4, 13, 14): an install that goes wrong must be recoverable without a debug probe, and the owner's clock sits in a case with only a USB cable. Confirming from local health only keeps a router outage from reverting a good image.  
*Implemented by:* Bootloader: swap in a staged image, swap back an unconfirmed one, Configuration model and flash storage, Composition: task wiring, shared state, chip selection

**REQ-UPD-05** (ratified, high)  
An update file shall be a firmware image followed by a 64-byte Ed25519 signature made over the SHA-512 digest of the image. The clock shall accept it only if its length is plausible for the image slot, the info block within the first 4 KiB of the image names the chip the clock runs on, the version in that block is strictly newer than the running one (compared by number), and the signature is valid under one of the two built-in public keys (strict verification). The chip and the version shall be read from the signed image, never from a file name or a release tag, and nothing shall be marked for the bootloader's swap before every check passed.  
*Why:* ADR-0006 points 5, 6 and 9: the install is the one place where a wrong input replaces the whole firmware. Taking chip and version from the signed bytes stops a signed old image being presented as new (rollback) and an image for the other chip being installed; two keys let the owner keep one apart as a backup.  
*Implemented by:* Update files: info block, acceptance rules, Ed25519 check, staging in the download slot

**REQ-UPD-06** (ratified, high)  
On the USB terminal `update upload <size>` shall take an update file (REQ-UPD-05) and write it into the bootloader's download slot, then read it back from flash for the checks; the running image and the settings are untouched whatever happens; a transfer that stalls for 10 seconds is abandoned. A staged image is installed only by `update apply` and the confirmation word INSTALL, which marks the swap and restarts; it is refused while the running image has not yet confirmed itself to the bootloader. `update` and `status` show what is staged or why the last upload was refused.  
*Why:* ADR-0006 point 7 (two-step confirmation, as for the factory reset) and the owner's decision of 2026-10-18 that the Pico W installs by upload only. Reading back from flash catches a write error that checking the received bytes would miss.  
*Implemented by:* Update files: info block, acceptance rules, Ed25519 check, staging in the download slot, Terminal commands and USB serial console, Configuration model and flash storage

**REQ-UPD-07** (ratified, high)  
Anyone shall be able to run their own build: a build made with the cargo feature `custom-images` shall accept an uploaded image without the signature and the newer-version checks (the chip and the length are still checked), shall say so in `version`, `update` and `status`, and shall show an unsigned stage as such. The feature shall be off in every official build, and in a custom build the update check shall only inform and never fetch or install a release.  
*Why:* Requested by the owner (2026-10-19): the signature protects the owner's clocks from foreign firmware, but this is an open-source project and others must be able to install their own builds without the owner's key. A compile-time switch keeps the protection on by default, and a build that opens it says so wherever the version is shown.  
*Implemented by:* Update files: info block, acceptance rules, Ed25519 check, staging in the download slot, Terminal commands and USB serial console

**REQ-UPD-08** (ratified, high)  
The Security page of the web interface shall take an update file (REQ-UPD-05) as the raw body of a POST to /upload, streamed into the bootloader's download slot and checked exactly as on the terminal, and shall install a staged image only after a second page asks for the word INSTALL (REQ-UPD-06). Over the plain port the upload and the install are refused while HTTPS runs; a clock without HTTPS takes them over plain HTTP. Every refusal (login, origin, a running upload on another interface, a bad file) comes before the first byte is stored; one upload runs at a time across the terminal and the web; a connection that stalls for 10 seconds or breaks leaves nothing staged. The page says so when the build is a custom build (REQ-UPD-07).  
*Why:* Decided in ADR-0006 and requested by the owner (2026-10-19): the Pico W installs by upload only and a browser is how most users reach the clock. The signature check does not depend on the transport, so plain HTTP is acceptable where TLS does not exist; where TLS runs, the password must not travel in the clear for an action this large.  
*Implemented by:* Update files: info block, acceptance rules, Ed25519 check, staging in the download slot, HTTP parsing, settings page and web server, Composition: task wiring, shared state, chip selection

**REQ-UPD-09** (ratified, high)  
On the Pico 2 W, `update download` and the Security page's button shall fetch the update file (REQ-UPD-05) of the release the update check found, from github.com via one redirect, into the download slot, and never install it: the file goes through the same checks as an upload and waits for the INSTALL confirmation. The requests shall name the asset picoclock-<version>-pico2-w.pcu of the release tag; the redirect shall be followed only to https://release-assets.githubusercontent.com and only if its address holds visible ASCII; github.com shall be trusted through the root of the update check and the download host through ISRG Root X1 only; the file shall be accepted only if the body is exactly as long as announced (no chunked answer) and both certificate checks passed after it. A custom build (REQ-UPD-07) and the Pico W shall not download. One file is received at a time across terminal, web and download.  
*Why:* Decided in ADR-0006 points 4 and 8 and requested by the owner (2026-10-19): the Pico 2 W can verify GitHub's RSA download chain and so can fetch the file itself; the Pico W cannot and installs by upload. The signature, not the transport, decides what is installed, so the download adds only fetch-and-stage, with the same refusals as an upload; pinning one root per host and the single redirect keep the set of parties that can serve a file small.  
*Implemented by:* Firmware update check (GitHub releases), Update files: info block, acceptance rules, Ed25519 check, staging in the download slot, NTS client: key exchange, authenticated packets, sessions, HTTP parsing, settings page and web server, Terminal commands and USB serial console, Composition: task wiring, shared state, chip selection

**REQ-UPD-10** (ratified, high)  
A release signed by one of the two built-in keys shall be able to revoke the other: the image carries, in its signed bytes right behind the info block, a bitmask of revoked keys, and when the user installs that image (update apply) the clock shall record the mask in the trust record before it marks the swap. From then on a signature by a revoked key shall be refused, whatever the image. An image shall not revoke its own signing key, shall not revoke every key, and an unknown key number shall be refused. The trust record shall be kept in its own pair of flash sectors, survive a power cut during a write, and not be erased by a factory reset. A firmware that predates this block shall ignore it.  
*Why:* ADR-0006 point 10: the bootloader's key list can never change, so retiring a lost or leaked key has to be an application rule that a release signed with the other key can trigger. The signer cannot revoke itself and a compromised key cannot lock the owner out of the other, because some key must stay; a factory reset must not bring a revoked key back.  
*Implemented by:* Update files: info block, acceptance rules, Ed25519 check, staging in the download slot, Configuration model and flash storage, Terminal commands and USB serial console, Composition: task wiring, shared state, chip selection


### 4.9 Terminal

**REQ-CLI-01** (ratified, medium)  
The clock shall provide a terminal over USB serial with commands to show, get, set, save and reset settings, show status and time, list zones, sync, look up the zone, reboot and enter flashing mode.  
*Why:* Requested terminal-based configuration interface.  
*Implemented by:* Terminal commands and USB serial console

**REQ-CLI-02** (ratified, low)  
Zone listings shall be capped so one command cannot flood the console.  
*Why:* 486 names would scroll away any other output.  
*Implemented by:* Terminal commands and USB serial console


### 4.10 Web

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

**REQ-WEB-06** (ratified, high)  
On the Pico 2 W build the web page shall be available over HTTPS (TLS 1.3 only, port 443) with a certificate generated on the device, so that the settings page, the web password and the WiFi passphrases are not sent in clear; the Pico W build keeps plain HTTP.  
*Why:* Decided in ADR-0002 (2026-10-16). A default private key shipped in an open repository is public, so none is shipped. rustls does not build for the RP2040, so HTTPS comes with the Pico 2 W.  
*Implemented by:* HTTPS server (Pico 2 W): heap, rustls glue, certificate lifecycle and flash record, HTTP parsing, settings page and web server

**REQ-WEB-08** (ratified, high)  
The HTTPS certificate and key shall be kept in their own flash sector and survive a reboot; a damaged or expired one shall be replaced by a new self-signed certificate; `cert` shall show its SHA-256 fingerprint and validity and `cert new` shall replace it; an uploaded certificate and key (web form, HTTPS only, same-origin only) shall be checked before use and the key never shown; while HTTPS runs, plain-HTTP page requests shall be redirected to it (setting `http_redirect`, default on) and nothing shall be changed over the plain port.  
*Why:* A browser pins trust to the certificate, so it must not change at every boot; a private key sent over plain HTTP would defeat the point of HTTPS; a redirect must not become a header-injection or open-redirect hole.  
*Implemented by:* HTTPS server (Pico 2 W): heap, rustls glue, certificate lifecycle and flash record, HTTP parsing, settings page and web server, Web page TLS certificate: generate, inspect, validate, Terminal commands and USB serial console

**REQ-WEB-07** (ratified, high)  
The device shall build a self-signed ECDSA P-256 server certificate that OpenSSL reads and verifies (valid for the given host names and the current time, for digital signatures and server authentication, not a CA, positive serial), and shall accept an uploaded P-256 certificate and key only if the key matches the certificate and the certificate is valid at the time; PEM and DER, PKCS#8 and SEC1 are read, and any other curve, RSA, or malformed input is refused without panicking.  
*Why:* ADR-0002: the HTTPS certificate is generated on the device or uploaded; a wrong or mismatched certificate would fail at the first browser connection, or worse, be accepted when it should not be.  
*Implemented by:* Web page TLS certificate: generate, inspect, validate

**REQ-WEB-09** (ratified, medium)  
While HTTPS is on, a factory reset requested over the plain HTTP port shall be refused even if the redirect to HTTPS is switched off; builds and settings without HTTPS keep the web reset.  
*Why:* Found by the full-code review of 2026-10-17: with no web password, any host on the LAN could erase the clock over the plain port. Decided by the owner the same day.  
*Implemented by:* HTTP parsing, settings page and web server

**REQ-WEB-10** (ratified, medium)  
The web server shall accept a request of up to 8 KiB, so that an uploaded certificate chain of the size the page promises (3 KB of DER data, four certificates) and its key fit in one request; a larger request shall be answered with 413 and a text that names the limits.  
*Why:* Found by the full-code review of 2026-10-17: the request limit was 4 KiB and the form parser's text buffer 3.5 KB, while the PEM text of a 3 KB chain is about 4.3 KB, so the largest chain the page promised could not be uploaded.  
*Implemented by:* HTTP parsing, settings page and web server

**REQ-WEB-11** (ratified, medium)  
The web interface shall consist of six pages selected by `?tab=` (Status, WiFi, Time, Clock, Web, Security; an unknown or missing tab is the status page), without JavaScript: the status text only on the Status page, each setting on exactly one of the four settings pages, each of which is its own form saving only the settings it shows, and the factory reset, the certificate upload and the NTS trust anchors on the Security page; every page shall fit its buffer with room to spare, with every optional part present.  
*Why:* Requested by the owner (2026-10-18) because the settings had grown to about 45 on one page. Fable's review of the options measured a latent overflow of the 12 KB page buffer when a busy status block, four trust anchors and a message were all on one page; one page per group removes it. REQ-WEB-01 (the page fits) still holds, now for each page.  
*Implemented by:* HTTP parsing, settings page and web server, Configuration model and flash storage


### 4.11 Time sources

**REQ-SRC-01** (ratified, high)  
The user shall be able to select which time sources the clock uses (NTP or NTS and DCF77; GPS in a later release), and choose one of two selection modes: `order` or `vote`. The RTC keeps the time between updates in both modes. With only NTP/NTS selected the clock shall behave as it does today.  
*Why:* Requested by the owner. Today the clock trusts NTP alone; more sources need an explicit rule for which of them sets the time.  
*Implemented by:* Time source selection (order, vote) and the DCF77 receiver, Configuration model and flash storage, WiFi, hourly sync, geolocation request and NTP server

**REQ-SRC-02** (ratified, high)  
In mode `order` the selected sources shall form an ordered list; the clock shall take the time from the first source that delivers a valid, fresh time and fall back to the next one when a source has no valid time, is too old, or fails; it shall return to a higher-ranked source as soon as that delivers again.  
*Why:* Requested by the owner: a simple, predictable priority with fallbacks, the way the NTP server list works today.  
*Implemented by:* Time source selection (order, vote) and the DCF77 receiver

**REQ-SRC-03** (ratified, high)  
In mode `vote` the clock shall compare the times of all selected sources that currently deliver one (a plausibility check, or triage): sources whose times agree within a selectable tolerance form a group, and the time is accepted only if the best group holds more than half of the total vote power of the sources that deliver; sources outside that group are rejected and reported. If no group has a majority the RTC shall be left unchanged and the status shall say so.  
*Why:* Requested by the owner. A wrong source (a spoofed NTP reply, a GPS fix with a wrong week, a DCF77 decoding error) must not set the clock when the others disagree.  
*Implemented by:* Time source selection (order, vote) and the DCF77 receiver

**REQ-SRC-04** (ratified, medium)  
In mode `vote` every selected source shall have a selectable vote power (a whole number; 0 means the source is shown and compared but cannot decide), and the agreement tolerance shall be selectable, in seconds.  
*Why:* Requested by the owner: sources differ in how much they can be trusted and in precision (the RTC has whole-second resolution, DCF77 second resolution, NTP milliseconds).  
*Implemented by:* Time source selection (order, vote) and the DCF77 receiver, Configuration model and flash storage

**REQ-SRC-05** (ratified, medium)  
The terminal status and the web page shall show the selection mode and, for each selected source, its state, its last time and offset from the accepted time, its vote power in mode `vote`, and whether it was accepted or rejected; all of it shall be settable from the terminal and the web page and saved with the other settings.  
*Why:* A rejected source is a fault the owner needs to see; hidden voting would be unexplainable.  
*Implemented by:* Time source selection (order, vote) and the DCF77 receiver, Terminal commands and USB serial console, HTTP parsing, settings page and web server

**REQ-SRC-06** (ratified, medium)  
When the clock has a valid time, a sample from a time source that differs from it by more than 1000 seconds shall not set the clock until a second reading of the same source agrees with it within 5 seconds; meanwhile the clock shall be left alone, the sample shall not count as fresh for the other sources, the status shall say so, and a far-away NTP time shall make the clock ask again at once (at most three times in a row). Without a valid time every sample is used.  
*Why:* Plain NTP is not authenticated and one wrong answer (a broken server, a forged reply on a foreign network) must not move the clock by hours; a real step (battery change, a wrong RTC) is accepted a minute later. The limit is the one ntpd calls its panic threshold. Found open by the review of 2026-10-16 (NTP plausibility).  
*Implemented by:* Time source selection (order, vote) and the DCF77 receiver

**REQ-DCF-01** (ratified, high)  
The DCF77 decoder shall turn the 59 bits of a minute into the UTC time of the minute they announce, and refuse a frame whose start bit, time-zone bits, three parity bits, BCD digits, calendar date (including the number of days in the month) or transmitted weekday are wrong.  
*Why:* DCF77 is a plain bit stream with weak redundancy; a misread frame must not set the clock. The weekday against the date is an extra check on the whole date. Local time is German time, converted to UTC with the transmitted CET/CEST bits, independent of the clock's own timezone setting.  
*Implemented by:* Time source selection (order, vote) and the DCF77 receiver

**REQ-DCF-02** (ratified, high)  
The DCF77 receiver pulses shall be classified by width (about 100 ms = 0, about 200 ms = 1, spikes under 50 ms ignored, other widths a fault), the minute mark found from the missing pulse of second 59, and a time shall be reported only when two frames in a row decoded and announce minutes exactly one minute apart; any broken pulse, wrong rhythm or refused frame restarts the confirmation.  
*Why:* A receiver in poor reception gives noise; requiring two consecutive consistent frames keeps one lucky frame or a burst of interference from setting the clock (the vote mode cannot help when DCF77 is the only source).  
*Implemented by:* Time source selection (order, vote) and the DCF77 receiver

**REQ-DCF-03** (ratified, medium)  
The DCF77 receiver module shall be wired to GP0 (PON) and GP1 (TCO) and powered from the 3V3 pin; the receiver shall be switched on (PON low, after a high-to-low edge) only while `dcf77` is in `time_sources` and off (PON high) otherwise, and pulses shall be timed on the microsecond clock.  
*Why:* Planned wiring in docs/design/architecture.md: the pins are free on the clock board, the module (1.2 to 3.3 V, PON low = on) needs no level shifting, and a receiver that is not used should not pick up or cause interference.  
*Implemented by:* Time source selection (order, vote) and the DCF77 receiver, Composition: task wiring, shared state, chip selection

**REQ-DCF-04** (ratified, medium)  
The DCF77 receiver delay (how late the receiver's pulse edges are) shall be settable by hand (`dcf77_delay` 0 to 500 ms) or learned automatically (`dcf77_delay auto`, the default): in auto mode the clock compares each confirmed DCF77 time with a fresh NTP time (at most 5 minutes old), keeps the median of the last readings (at least 4, readings more than 1 s off ignored), uses it, and stores it (`dcf77_learned`) so that it survives a restart; `status` shows the delay in use and whether it is learned or manual.  
*Why:* Requested by the owner (2026-10-18). On the first receiver the DCF77 time was about 55 ms behind NTP; with a vote tolerance of a second this is harmless, but it makes the offset display and the sub-second phase of a DCF77-only clock wrong by the receiver's delay.  
*Implemented by:* Time source selection (order, vote) and the DCF77 receiver, Configuration model and flash storage, Terminal commands and USB serial console


### 4.12 Buzzer

**REQ-HAM-01** (ratified, medium)  
The time shall be convertible to Morse code as the four digits HHMM of the displayed local time in 24-hour form, with standard timing (dash = 3 dits, gap inside a digit = 1 dit, between digits = 3 dits) at a speed of 5 to 40 words per minute (`morse_wpm`, default 18, one dit = 1200 / wpm ms).  
*Why:* Requested by the owner (2026-10-18) for radio amateurs: the time can be read without looking at the clock.  
*Implemented by:* Buzzer driver

**REQ-HAM-02** (ratified, medium)  
With the setting `ham_mode on` (default off) the hourly chime (when `hourly_chime` is on) shall be the time in Morse code instead of the two beeps, and holding UP or DOWN for 1.5 seconds outside the menu shall sound the time in Morse code; nothing is sent while the clock has no time, in the menu, or while the alarm rings. The alarm keeps priority and SET keeps its menu function.  
*Why:* Requested by the owner (2026-10-18): the hourly beep is the time in Morse code, also on a long hold of a button. SET's long hold already opens the menu, so UP and DOWN carry the hold.  
*Implemented by:* Button, menu, alarm and view state machine, Buzzer driver, Button polling, Configuration model and flash storage


### 4.13 Display

**REQ-DSP-01** (ratified, high)  
The firmware shall compose the 32x8 matrix, weekday LEDs and status icons into the panel's row format so that glyphs, shifts and icons do not disturb each other.  
*Why:* The panel's shift-register layout has quirks (column offsets, group boundaries) inherited from the vendor demo.  
*Implemented by:* Frame buffer and font, LED matrix scan

**REQ-DSP-02** (ratified, high)  
During normal operation (WiFi connected, web page and NTP in use) the display scan's row timer shall fire more than 100 microseconds late less than once per minute on average, and the terminal `status` shall report the measured lateness.  
*Why:* A late row timer lengthens that row's on-time, which shows as a flickering line, worst at low brightness. Found on the hardware: the settings lock disabled interrupts for hundreds of microseconds about twice a second.  
*Implemented by:* LED matrix scan, Composition: task wiring, shared state, chip selection, Terminal commands and USB serial console


### 4.14 Interaction

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

**REQ-UI-07** (retired, low)  
The colon shall blink only while the time is NTP-synchronised, and the status icons shall reflect the settings.  
*Why:* The panel has no spare WiFi icon, so the colon and count-up icon carry the sync state.  
*Implemented by:* none yet  
*Retired:* Assumed that a blinking colon is wanted by everyone. For radio amateurs the blinking display current is a source of interference (QRM), and the owner asked on 2026-10-18 for a steady colon by default; the sync state is also shown by the count-up icon.

**REQ-UI-08** (ratified, low)  
The colon between hours and minutes shall be steady by default; with the setting `colon_blink on` it blinks once a second while the time is synchronised and stays lit until the first synchronisation. The status icons shall reflect the settings.  
*Why:* Requested by the owner (2026-10-18): a blinking colon might cause more QRM, so it is off unless wanted. The count-up icon carries the sync state either way.  
*Implemented by:* Button, menu, alarm and view state machine, Configuration model and flash storage


### 4.15 Hardware

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

**REQ-HW-05** (ratified, high)  
The firmware shall run on the Raspberry Pi Pico W (RP2040).  
*Why:* The hardware installed in the clock.  
*Implemented by:* Composition: task wiring, shared state, chip selection

**REQ-HW-06** (ratified, medium)  
The firmware shall run on the Raspberry Pi Pico 2 W (RP2350) from the same source, selected by a build feature.  
*Why:* The user may fit a Pico 2 W.  
*Implemented by:* Composition: task wiring, shared state, chip selection

**REQ-HW-07** (draft, low)  
The clock shall accept a DCF77 receiver module and a u-blox GPS module as additional time sources.  
*Why:* Roadmap. Needs free GPIOs that reach the clock's case.  
*Implemented by:* none yet


### 4.16 Non-functional

**REQ-NFR-01** (ratified, medium)  
The firmware image and the settings sector shall fit the flash of the target board without overlapping, and the static RAM shall fit the chip.  
*Why:* The settings sector sits after the program image.  
*Implemented by:* Composition: task wiring, shared state, chip selection, Configuration model and flash storage

**REQ-NFR-02** (ratified, high)  
The display scan shall run at a priority above all networking tasks.  
*Why:* WiFi activity must not make the display flicker.  
*Implemented by:* Composition: task wiring, shared state, chip selection, LED matrix scan

**REQ-NFR-03** (ratified, medium)  
A hardware watchdog shall restart the clock if its clock task (which owns the RTC bus) has not run for 5 seconds or the executor is starved for 8 seconds; the restart is not caused by long operations (flash saves, the NTS key exchange); `status` shall say when the last restart was caused by the watchdog.  
*Why:* Found by the full-code review of 2026-10-17: a hung RTC bus or a spinning task froze the clock until the power was cycled. Decided by the owner the same day.  
*Implemented by:* Composition: task wiring, shared state, chip selection, Terminal commands and USB serial console

**REQ-RNG-01** (ratified, high)  
On the Pico W (RP2040), which has no hardware random number generator, random bytes shall be made from ring-oscillator bits read at spaced-out times and hashed together with the time, a counter and the previous state, using 64 raw bytes for every 32 output bytes; a stuck source must still give different blocks.  
*Why:* Found by the full-code review of 2026-10-17: the bits were read back to back, so they are correlated, and they feed the NTS key exchange, nonces and the network stack. Decided by the owner the same day. The Pico 2 W uses the chip's TRNG.  
*Implemented by:* Conditioning of the RP2040 ring-oscillator random bits, Composition: task wiring, shared state, chip selection
