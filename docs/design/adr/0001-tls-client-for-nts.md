---
project: picoclock
doc: docs/design/adr/0001-tls-client-for-nts.md
status: ledger
last_updated: 2026-10-16
---

# 0001. TLS 1.3 client for NTS: vendored embedded-tls, an exporter patch, deferred certificate checks

## Status

Accepted (the owner asked for the NTS implementation on this basis on 2026-10-16; implemented and measured the same day).
The first draft of this record named embedded-tls 0.19.0 and verification inside the handshake. Both turned out wrong
and are recorded below as rejected alternatives with the measurements that showed it.

## Context

Secure NTP (NTS, RFC 8915) has two parts. NTS-KE is a TLS 1.3 exchange (TCP 4460, ALPN `ntske/1`) in which the server
sends cookies and the AEAD algorithm; both sides derive the client-to-server and server-to-client keys from the TLS
session with the **TLS exporter** (RFC 8446 section 7.5, label `EXPORTER-network-time-security`). The NTP packets are
then protected with AES-SIV-CMAC-256 (AEAD id 15) in extension fields. The client must validate the server's
certificate, so it needs a root and a roughly correct clock.

The target is an RP2040 (125 MHz Cortex-M0+, 264 KiB RAM, no crypto hardware, no atomic compare-and-swap) and, from
the same source, an RP2350. Requirements: REQ-HW-08 and REQ-NTS-01 to REQ-NTS-07.

## Decision

1. **Library.** Vendor `embedded-tls` from the embassy-rs repository at commit `58126ee5c4` (2026-10-14, Apache-2.0) in
   `vendor/embedded-tls`. It is a TLS 1.3 client for `no_std` without an allocator, with ALPN, and its certificate
   verifier is `rustls-webpki` 0.103 without a crypto provider. Its primitives come from `embassy-crypto` drivers linked
   by the final binary: `embassy-crypto-rustcrypto` (SHA-2, HMAC, AES-GCM, P-256, P-384) and a small RNG driver in the
   firmware. Its `embassy-crypto` dependency is pointed at the Embassy revision the firmware already pins, because that
   crate declares `links` and two revisions cannot coexist.
2. **Exporter patch.** Add `exp master` to the key schedule and `TlsConnection::export_keying_material`
   (`vendor/embedded-tls/PATCHES.md`). Checked against OpenSSL (`core/tests/tls_exporter_openssl.rs`) and, with the
   context bytes NTS uses, by real servers accepting the derived keys (`core/tests/nts_live.rs`).
3. **Deferred certificate checks.** The handshake runs with a verifier that only records the chain, the transcript hash
   and the `CertificateVerify` signature (`DeferredVerifier` in `core/src/nts_ke.rs`). The NTS-KE request and response
   are exchanged, the keys are exported, the connection is closed, and then the recorded data is passed to the real
   verifier (chain to a built-in root, host name, validity dates, signature). The session is returned only if that
   succeeds. This is safe for NTS-KE because the client sends no secret (its request is a fixed public 16-byte offer) and
   uses nothing the server said before the checks pass; a man in the middle completes the handshake and then fails the
   checks. It must not be reused for a protocol in which the client sends secrets before it knows the server.
4. **Packets.** `aes-siv` (RustCrypto) for AES-SIV-CMAC-256. Cookies, keys and the sessions live in RAM only; a reboot
   costs one key exchange.
5. **Scope.** Servers whose chain uses ECDSA (P-256 or P-384) and ends at ISRG Root X2 or the SSL.com ECC root: that
   covers `time.cloudflare.com`, `nts.time.nl`, `ntppool1.time.nl`, `nts.teambelgium.net`, `ntp3.fau.de`,
   `time.txryan.com` and any server with a Let's Encrypt ECDSA certificate. RSA chains are not supported (see E).
6. **Policy.** An entry `nts:<host>` in `ntp_servers`. No key exchange without a valid RTC time (embedded-tls would skip the
   date check silently). The order of the list is the only fallback policy; a list of `nts:` entries alone never falls
   back to plain NTP.

## Alternatives considered

### A: rustls client — REJECTED for the RP2040 (measured, build, 2026-10-16)

- With `default-features = false` and `rustls-rustcrypto` 0.0.2-alpha it builds for `thumbv8m.main-none-eabihf` but not
  for `thumbv6m-none-eabi`: `spin` needs atomic compare-and-swap and `rustls` uses `alloc::sync::Arc` in about 30 files.
  A fork of a security library would be needed. Evidence: `spikes/rustls-check`, compile only.

### B: write a TLS 1.3 client — REJECTED (assumed, not built)

- The highest-risk way to meet the requirement; embedded-tls already provides handshake, record layer and checks.

### C: NTS with cookies provisioned from a PC, no TLS on the device — REJECTED (assumed, not built)

- The cookies and keys come from NTS-KE and expire; provisioning them would not last.

### D: embedded-tls 0.19.0 from crates.io — REJECTED (measured, build, 2026-10-16)

- Its `webpki` feature depends on `rustls-webpki` 0.101, which pulls in `ring`; `ring` does not build for
  `thumbv6m-none-eabi` (`getrandom` has no backend, `ring`'s build script fails). The main branch dropped that
  dependency, hence the vendored snapshot.

### E: support RSA certificate chains — NOT DONE (assumed, not built)

- Netnod, PTB and System76 use RSA certificates. embedded-tls's verifier without an allocator has no RSA; adding it
  means enabling the `alloc`/`rsa` paths and a heap, plus RSA verification time on this chip. Left as future work.

### F: verify the certificates inside the handshake — REJECTED (measured, hardware and host, 2026-10-16)

- The simplest design, and the one embedded-tls offers. On the RP2040 the checks of Cloudflare's chain (P-256 leaf, P-384
  intermediate, P-384 root) take about 4.7 s, and the server resets the connection first: the key exchange failed twice
  with a connection reset after 4.7 s, also with the crypto crates built for speed (4.72 s and 4.76 s).
- A host experiment with an artificially slow client (`core/tests/nts_slow_client.rs`) showed why: `time.cloudflare.com` and
  `nts.time.nl` complete up to a total stall of 1.5 s and drop the connection at 2 s; `ntp3.fau.de` tolerates somewhat
  longer (completed with 2.5 s stalls, dropped at 3.5 s); others drop at a 3 s total. With the checks deferred, all four
  servers tested accepted a 5 s check.

### G: pin the intermediate CA instead of the root — REJECTED (assumed, not built)

- Saves one P-384 verification (still about 2.6 s, above the servers' limit) and breaks silently when a server's
  intermediate rotates.

### H: run the verifications on the second core in parallel — NOT DONE (assumed, not built)

- Halves the chain time at best (about 2.5 s), still above the limit, and adds a second execution context to an
  otherwise single-threaded design.

## Consequences

- **Time on the clock (hardware, RP2040 at 125 MHz, 2026-10-16, one to three runs each):** key exchange with
  `time.cloudflare.com` 4.98 s and 5.04 s; with `nts.time.nl` 6.8 to 6.9 s (a longer chain); later syncs reuse the cookies
  and cost one UDP exchange plus about 1.5 ms of AES-SIV. Primitives measured separately: ECDSA P-256 verify 558 to
  580 ms, P-256 ECDH 283 to 294 ms, AES-SIV of a request 1.4 to 1.5 ms (`spikes/crypto-bench`).
- **On the RP2350 (Pico 2 W, hardware, 2026-10-16, one run each, measured as the console's stall):** 0.85 s with Cloudflare and
  1.25 s with `nts.time.nl`, roughly six times faster than the RP2040 (the Cortex-M33 has a 32x32 to 64-bit multiplier). The
  deferred checks are not needed there (well inside the servers' limit) but do no harm.
- The public-key operations run without yielding, so during a key exchange the cooperative executor is busy for 5 to 7 s:
  the console, the web page and the UI wait (the display scan runs on its own interrupt executor and stays lit). It
  happens once per boot, and again only after a NAK or when the cookies are used up.
- **Size (build, 2026-10-16):** about 207 KB more flash (658 KB in total) and 42 KB more static RAM (116 KB) than without
  NTS, Pico W; the Pico 2 W build is similar. TLS buffers take about 23 KB, the deferred chain 6 KB.
- **Entropy.** The RP2040 has no hardware random number generator; the RNG driver uses Embassy's `RoscRng` (ring
  oscillator jitter) for the TLS ephemeral key, the NTS identifiers and the AEAD nonces. The RP2350's real TRNG would be a
  hardening for the Pico 2 W.
- **Maintenance.** The vendored crate is a snapshot of an unreleased upstream state with a small patch that should be
  offered upstream; `vendor/embedded-tls/PATCHES.md` lists every change. The roots are two certificates in
  `core/roots/` (fingerprints in `core/src/nts_ke.rs`) and expire in 2040 and 2041.
- **Trust.** A time that comes from the RTC after an unauthenticated sync is used to check certificate dates; an attacker
  who controls an earlier plain-NTP sync could shift the clock to make an expired certificate look valid. Listing only
  `nts:` entries removes plain NTP from the picture once the RTC holds a good time.
