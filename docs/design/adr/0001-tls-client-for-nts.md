---
project: picoclock
doc: docs/design/adr/0001-tls-client-for-nts.md
status: ledger
last_updated: 2026-10-16
---

# 0001. TLS 1.3 client for NTS: embedded-tls with a key exporter

## Status

Proposed (owner review pending; implementation of NTS may start on this basis)

## Context

Secure NTP (NTS, RFC 8915) has two parts. NTS-KE is a TLS 1.3 exchange (TCP 4460, ALPN `ntske/1`) in which the server
sends cookies and the AEAD algorithm; both sides derive the client-to-server and server-to-client keys from the TLS
session with the **TLS exporter** (RFC 8446 section 7.5, label `EXPORTER-network-time-security`). The NTP packets are
then protected with AES-SIV-CMAC-256 (AEAD id 15) in extension fields. The client must validate the server's
certificate, so it needs a root store and a roughly correct clock.

The target is an RP2040 (125 MHz Cortex-M0+, 264 KiB RAM, no crypto hardware, no atomic compare-and-swap) and, from
the same source, an RP2350. Requirement REQ-HW-08 (draft) covers NTS.

## Decision

- Use `embedded-tls` 0.19 (TLS 1.3 client, `no_std`, no allocator, maintained under embassy-rs) with its `webpki`
  certificate verifier, which is `rustls-webpki` and works without `std`. It already supports ALPN (`with_alpn`).
- Add the one missing piece, the exporter: derive `exp master` from the master secret and the transcript hash at the
  server Finished, then the exporter output per RFC 8446. `embedded-tls` already holds the key schedule and the
  transcript, so this is a small patch. Carry it as a vendored patch in this repository and offer it upstream.
- Use `aes-siv` (RustCrypto) for AES-SIV-CMAC-256.
- Validate certificate dates against the RTC; if the RTC does not hold a valid time, NTS-KE is not attempted (the next
  server in the list is used) and this is logged. Whether NTS may fall back to plain NTP is a setting, never silent.

## Alternatives considered

### Option A: rustls client — REJECTED for the RP2040 (measured, build, 2026-10-16)

- Pros: the most reviewed Rust TLS stack; has an exporter API.
- Cons: with `default-features = false` and the RustCrypto provider (`rustls-rustcrypto` 0.0.2-alpha) it **builds for
  `thumbv8m.main-none-eabihf` but not for `thumbv6m-none-eabi`**: `spin` needs atomic compare-and-swap, and `rustls`
  and its types use `alloc::sync::Arc` in about 30 files, which does not exist on that target. Making it build means
  maintaining a fork of a security library. The provider is an alpha release.
- Evidence: `spikes/rustls-check`, compile only; not run on hardware.

### Option B: write a TLS 1.3 client — REJECTED (assumed, not built)

- Rejected on reasoning alone: a new TLS implementation is the highest-risk way to meet the requirement, and
  `embedded-tls` already provides the handshake, record layer and certificate checking.

### Option C: NTS with a pre-shared cookie set, no TLS on the device — REJECTED (assumed, not built)

- The cookies and keys come from NTS-KE; without it there is no NTS. Provisioning them from a PC would not survive
  their expiry and defeats the point.

## Consequences

- **Cost, measured on the clock (hardware, RP2040 at 125 MHz, RustCrypto crate versions used by `embedded-tls`
  0.19, opt-level 3, 2026-10-16):** ECDSA P-256 verify 558 to 580 ms, P-256 ECDH 283 to 294 ms, P-256 ephemeral key
  294 ms, AES-SIV-CMAC-256 of an NTP request 1.4 to 1.5 ms, SHA-256 of 4 KiB 4.5 ms, AES-128-GCM of 1 KiB 4.2 ms.
  A handshake with a two-signature chain plus CertificateVerify would take about 2.3 s (**estimated from these numbers,
  not measured as a handshake**). Cookies make this a once-per-key-exchange cost, not per request.
- The signature algorithms the verifier supports must match what NTS servers present (ECDSA P-256 is common; RSA is
  larger and slower and is not enabled by default).
- RAM: `embedded-tls` needs record buffers (up to 16 KiB each way for full-size records); the real peak must be
  measured with the WiFi stack running before the design is final.
- Root certificates cost flash; start with the few roots needed by the supported servers, not a full store.
- The exporter patch must be verified against a real NTS server: a wrong derivation shows up as the server rejecting
  our authenticated request, which is a decisive test.
- NTS-KE cookies are kept in RAM and re-obtained after a reboot, which costs the handshake once per boot.
