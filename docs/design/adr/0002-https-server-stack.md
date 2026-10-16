---
project: picoclock
doc: docs/design/adr/0002-https-server-stack.md
status: ledger
last_updated: 2026-10-16
---

# 0002. HTTPS server for the web page

## Status

Proposed. **The owner has to choose between the options below**; nothing is built beyond the feasibility spike.

## Context

REQ-WEB-06 (draft): the web page shall be available over HTTPS, with a certificate the user can upload and a default
certificate included. A default private key shipped in an open-source repository is public, so it protects nothing; a
certificate generated on the device at first boot (self-signed, ECDSA P-256) does.

`embedded-tls` is a client only. A TLS **server** therefore needs a different library, or code written for this
project. The device is an RP2040 (no atomic compare-and-swap) or an RP2350.

Browsers matter: Chrome offers key shares for X25519 (and a hybrid) but not P-256, so a server that supports only
P-256 would force a HelloRetryRequest round trip (more code and a second flight); supporting X25519 avoids it.

## Decision

None yet (see Status). Recommendation: **Option C** below, and revisit the RP2040 when a vetted route exists.

## Alternatives considered

### Option A: rustls server with the RustCrypto provider on both chips — REJECTED as stated (measured, build, 2026-10-16)

- Does not build for the RP2040 (`spin` and `alloc::sync::Arc` need atomic compare-and-swap); would need a maintained
  fork of `rustls` and its dependencies. Builds for the RP2350.
- Evidence: `spikes/rustls-check`, compile only.

### Option B: write a minimal TLS 1.3 server — NOT RECOMMENDED (assumed, not built)

- Pros: works on both chips; one profile is enough (TLS_AES_128_GCM_SHA256, X25519, ECDSA P-256, no resumption, no
  client certificates); RFC 8448 gives exact handshake traces to test against; the primitives exist in RustCrypto.
- Cons: a hand-written TLS server is the riskiest security code this project could contain; subtle errors (transcript
  handling, record padding and alerts, key update, downgrade behaviour) do not show in a working demonstration.
- Rejected on reasoning alone, not built. If chosen, it needs its own review and interop testing (openssl, curl,
  browsers, a TLS test suite).

### Option C: rustls on the Pico 2 W (RP2350) only; the Pico W keeps HTTP — RECOMMENDED (build measured, run not tried)

- Pros: a vetted TLS implementation; no fork; the RP2350 is faster and has more RAM, so handshakes and buffers are
  easier.
- Cons: the installed Pico W gets no HTTPS, and cannot be used to test it; the RustCrypto provider for rustls is an
  alpha release (a different provider or a newer release may be needed); the feature depends on the Pico 2 W port,
  which has never run on hardware (REQ-HW-06).
- Evidence: `spikes/rustls-check` builds for `thumbv8m.main-none-eabihf` (build tier, 2026-10-16).

### Option D: no HTTPS; document the risk and offer web password plus `web_enabled off` — the status quo

- Cons: the web password and WiFi passphrase cross the LAN in clear.

## Consequences

- **Cost of a connection on the RP2040, from primitives measured on hardware (2026-10-16; estimates, not a measured
  handshake):** X25519 key generation 150 ms, X25519 shared secret 146 ms, ECDSA P-256 sign 358 ms: about 0.65 s per
  new connection. A browser that opens several connections pays it each time unless resumption is supported.
- The default certificate is generated on first boot (key generation about 0.3 s and one signature), stored in the
  settings sector, and shown by fingerprint in `status` so the user can verify it in the browser.
- An uploaded certificate and key must be ECDSA P-256 in PEM or DER; the private key is a secret and must be handled
  like the other secrets (never displayed).
- A self-signed certificate gives a browser warning; the benefit is encryption against passive eavesdropping, not
  identity.
- The settings sector may need to grow: certificate chain and key add a few hundred bytes up to a few KiB.
