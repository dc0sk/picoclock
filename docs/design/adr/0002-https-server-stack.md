---
project: picoclock
doc: docs/design/adr/0002-https-server-stack.md
status: ledger
last_updated: 2026-10-16
---

# 0002. HTTPS server for the web page

## Status

Accepted on 2026-10-16: the owner chose Option C and a per-device certificate. Nothing is built yet beyond the
feasibility spike; the Pico 2 W build has to run on hardware first (REQ-HW-06).

## Context

REQ-WEB-06 (draft): the web page shall be available over HTTPS, with a certificate the user can upload and a default
certificate included. A default private key shipped in an open-source repository is public, so it protects nothing; a
certificate generated on the device at first boot (self-signed, ECDSA P-256) does.

`embedded-tls` is a client only. A TLS **server** therefore needs a different library, or code written for this
project. The device is an RP2040 (no atomic compare-and-swap) or an RP2350.

Browsers matter: Chrome offers key shares for X25519 (and a hybrid) but not P-256, so a server that supports only
P-256 would force a HelloRetryRequest round trip (more code and a second flight); supporting X25519 avoids it.

## Decision

1. **Stack.** Option C: `rustls` (unbuffered, `no_std` with `alloc`) with a RustCrypto provider, in the **Pico 2 W
   (RP2350) build only**. The Pico W (RP2040) build keeps plain HTTP, the optional web password and the `web_enabled`
   switch. The RP2040 gets HTTPS only if a vetted route to it appears (a `rustls` that builds there, or an audited
   alternative); revisit then.
2. **Certificate.** Generated on the device at first boot (self-signed, ECDSA P-256), stored in the settings sector, and
   shown by fingerprint in `status`. The user can replace it by uploading a certificate and key (P-256, PEM or DER). No
   key is shipped in the repository.
3. **Key exchange.** X25519 (browsers offer it first), so no HelloRetryRequest round trip is needed.
4. **Port.** HTTPS on 443; whether port 80 stays open or redirects is decided at implementation and recorded then.
5. **Order of work.** First fit a Pico 2 W and run the existing build on it (WiFi, display, storage, temperature), then
   the heap and the `rustls` integration, then certificate generation and upload.

Rationale for choosing it over the others, in the owner's words of the choice and the measurements: it uses a vetted TLS
implementation without a fork or hand-written protocol code, at the price that the installed Pico W gets no HTTPS.

## Alternatives considered

### Option A: rustls server with the RustCrypto provider on both chips — NOT CHOSEN as stated (measured, build, 2026-10-16)

- Does not build for the RP2040 (`spin` and `alloc::sync::Arc` need atomic compare-and-swap); would need a maintained
  fork of `rustls` and its dependencies. Builds for the RP2350.
- Evidence: `spikes/rustls-check`, compile only.

### Option B: write a minimal TLS 1.3 server — NOT CHOSEN (assumed, not built)

- Pros: works on both chips; one profile is enough (TLS_AES_128_GCM_SHA256, X25519, ECDSA P-256, no resumption, no
  client certificates); RFC 8448 gives exact handshake traces to test against; the primitives exist in RustCrypto.
- Cons: a hand-written TLS server is the riskiest security code this project could contain; subtle errors (transcript
  handling, record padding and alerts, key update, downgrade behaviour) do not show in a working demonstration.
- Rejected on reasoning alone, not built. If chosen, it needs its own review and interop testing (openssl, curl,
  browsers, a TLS test suite).

### Option C: rustls on the Pico 2 W (RP2350) only; the Pico W keeps HTTP — CHOSEN (build measured, run not tried)

- Pros: a vetted TLS implementation; no fork; the RP2350 is faster and has more RAM, so handshakes and buffers are
  easier.
- Cons: the installed Pico W gets no HTTPS, and cannot be used to test it; the RustCrypto provider for rustls is an
  alpha release (a different provider or a newer release may be needed); the feature depends on the Pico 2 W port,
  which has never run on hardware (REQ-HW-06).
- Evidence: `spikes/rustls-check` builds for `thumbv8m.main-none-eabihf` (build tier, 2026-10-16). As a TLS 1.3 server on a
  PC (host tier, 2026-10-16, `spikes/rustls-server-host`, VR-37) it interoperated with `curl` (X25519) and `openssl
  s_client` (X25519 and P-256) using an ECDSA P-256 certificate, and refused a client that offered only the hybrid
  post-quantum group. Not yet shown: the unbuffered `no_std` API, an RP2350, a browser. The alpha provider chose
  AES-256-GCM/SHA-384; the device build should offer only AES-128-GCM/SHA-256.

### Option D: no HTTPS; document the risk and offer web password plus `web_enabled off` — NOT CHOSEN; remains the behaviour of the Pico W build

- Cons: the web password and WiFi passphrase cross the LAN in clear.

## Progress (hardware-independent parts)

- The certificate code exists and is tested against OpenSSL: `core/src/x509.rs` generates the self-signed ECDSA P-256
  certificate (valid from a day before the RTC time for ten years, DNS names for the device name and `.local`, leaf only,
  digital signature and server authentication) and inspects, matches and validates an uploaded certificate and key (PEM or
  DER, PKCS#8 or SEC1, P-256 only). REQ-WEB-07, VR-38.
- **The first step is done.** The existing build runs on a Pico 2 W in the clock (VR-39): WiFi, NTP, settings in the 4 MB
  flash, the web page, the display scan, NTS. The RP2350 is much faster at this kind of work: the NTS key exchange, which
  includes verifying a P-384 chain, took 0.85 to 1.25 s against 5.0 to 6.9 s on the RP2040, so the estimate of "under a
  second" for an HTTPS handshake is conservative. Still open: the heap, the `rustls` integration, certificate storage.
- Open questions for the implementation: where the key and certificate live (the 2 KiB settings record is too small; a
  second flash sector next to the settings one, in the Pico 2 W memory map only), when to generate (only with a valid RTC
  time), and whether port 80 stays open or redirects.

## Consequences

- **Cost of a connection on the RP2040, from primitives measured on hardware (2026-10-16; estimates, not a measured
  handshake):** X25519 key generation 150 ms, X25519 shared secret 146 ms, ECDSA P-256 sign 358 ms: about 0.65 s per
  new connection. A browser that opens several connections pays it each time unless resumption is supported.
- The certificate is generated on first boot (key generation about 0.3 s and one signature), stored in the settings
  sector, and shown by fingerprint in `status` so the user can verify it in the browser.
- An uploaded certificate and key must be ECDSA P-256 in PEM or DER; the private key is a secret and must be handled
  like the other secrets (never displayed).
- A self-signed certificate gives a browser warning; the benefit is encryption against passive eavesdropping, not
  identity.
- The settings sector may need to grow: certificate chain and key add a few hundred bytes up to a few KiB.
