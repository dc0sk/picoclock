---
project: picoclock
doc: docs/design/adr/0003-nts-rsa-chains-and-user-roots.md
status: ledger
last_updated: 2026-10-17
---

# 0003. NTS: RSA certificate chains on the Pico 2 W, and trust anchors the user adds

## Status

Accepted (the owner asked for both on 2026-10-17; implemented and run on a Pico 2 W the same day). It builds on ADR-0001
(alternative E there, "support RSA chains", was left as future work) and keeps its other decisions.

## Context

ADR-0001 verifies the NTS-KE server certificate with `rustls-webpki` over embassy-crypto drivers, against two built-in
ECDSA roots, and refuses RSA chains. Two limits followed in practice: Let's Encrypt moved to a new hierarchy in 2025 whose
RSA branch (Root YR, cross-signed by ISRG Root X1) serves Netnod, PTB, NIC.br, System76 and others, and nobody could add a
private NTS server or another authority without rebuilding the firmware. Requirements: REQ-NTS-08, REQ-NTS-09,
REQ-NTS-10, REQ-NTS-11.

## Decision

1. **RSA only where there is an allocator.** The vendored `embedded-tls` already has an `rsa` feature built on the `rsa`
   crate, which needs `alloc`. The firmware enables it for the Pico 2 W (`rp235x` implies `picoclock-core/rsa`), where the
   144 KiB heap of ADR-0002 exists; the Pico W build has no allocator, keeps the ECDSA-only verifier, and refuses every RSA
   chain with a certificate error. ISRG Root X1 (RSA 4096) is built in only with the feature.
2. **One connection, all roots.** `key_exchange` takes the list of trusted roots; the deferred checks (ADR-0001, decision 3)
   run the recorded chain against each root in turn and the first that fits decides. Before, each root cost a reconnect.
   The signature slot for `CertificateVerify` grew from 160 to 512 bytes (RSA-4096).
3. **User roots.** Up to four certificates (3.5 KB), PEM in, kept in their own flash sector on both chips (the sector before
   the HTTPS one on the Pico 2 W, before the settings on the Pico W; the memory maps reserve them). Added with `root add`
   (the terminal collects the pasted PEM lines until the END line and does not echo them), listed with `root`, removed with
   `root remove <n>`; on the Pico 2 W also from the HTTPS page. A paste is all-or-nothing; unreadable, expired, duplicate
   (also against the built-in list), oversized and, without RSA support, RSA certificates are refused.
4. **Where the trust list may change.** Over USB (physical access) and over TLS. Never over plain HTTP, whatever the redirect
   setting says: a root added over an unencrypted form could be swapped by anyone on the LAN, and a root decides whom the
   clock believes. So the Pico W's web page has no roots form; its owner uses the terminal.
5. **Status.** `status` shows the last key exchange (server, accepted or why refused, milliseconds) and the number of
   built-in and added roots, because a refused certificate and a server never tried looked alike.

## Alternatives considered

### A: RSA on the Pico W as well — REJECTED (assumed, not built)

- The `rsa` crate needs an allocator, and rustls-era arguments of ADR-0002 apply: the RP2040 has no atomic compare-and-swap
  for a general allocator crate, and verifying a 4096-bit chain on a Cortex-M0+ would come on top of the 5 s ECDSA checks
  that servers already barely tolerate. Not measured; no attempt to build it.

### B: pin a server's own certificate instead of a root list — REJECTED (assumed)

- It breaks at every certificate renewal (Let's Encrypt: every 60 to 90 days). The root list is what ADR-0001 already does.

### C: upload roots over the plain-HTTP page on the Pico W — REJECTED (assumed, reasoning only)

- See decision 4. Equals letting any LAN host choose the clock's authorities.

### D: user roots in the settings record — REJECTED (assumed)

- The record is 2 KiB and holds text settings; a certificate is up to 1.5 KB and four need 3.5 KB. A separate sector keeps
  a damaged roots record from invalidating the settings and the other way round.

## Consequences

- **Measured (hardware, Pico 2 W, 2026-10-17, VR-49, VR-50):** a key exchange with PTB's RSA chain stalls the console 0.45 s
  (Cloudflare's ECDSA one 0.55 s); heap peak 23 KB with RSA against 1 KB with ECDSA; a user root added by pasting its PEM is
  trusted at the next key exchange and survives a reboot; after removal and a reboot the same server is refused.
- **Host (2026-10-17):** an end-to-end test against `openssl s_server` with ECDSA and RSA hierarchies shows that a chain is
  accepted only through a root it was given, wherever that root stands in the list, and refused with a wrong name, no roots
  or an unrelated root; live key exchanges with PTB and Netnod pass with the RSA feature.
- Flash: 8 KB (Pico W) and 12 KB (Pico 2 W) at the end of flash are no longer program space; about 120 KB more flash on the
  Pico 2 W build for the RSA code (build, 2026-10-17).
- **Limits.** Netnod's NTP server is IPv6-only, which the clock cannot use; its key exchange works. System76's servers did
  not answer when tried. The Pico W's user-root path is the same code but has not run on a board for this change.
