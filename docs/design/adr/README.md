---
project: picoclock
doc: docs/design/adr/README.md
status: ledger
last_updated: 2026-10-17
---

# Architecture decision records

An ADR records one decision and why it was taken. They are numbered, never edited after acceptance and never deleted; a
reversed decision gets a new ADR that says which premise of the old one failed. Every empirical claim carries its
evidence tier and date, and every rejection is labelled measured or assumed.

| ADR                                           | Title                                                 | Status                  | Date       |
|-----------------------------------------------|-------------------------------------------------------|-------------------------|------------|
| [0001](0001-tls-client-for-nts.md)            | TLS 1.3 client for NTS: vendored embedded-tls, exporter patch, deferred checks | Accepted | 2026-10-16 |
| [0002](0002-https-server-stack.md)            | HTTPS server for the web page                         | Accepted | 2026-10-16 |
| [0003](0003-nts-rsa-chains-and-user-roots.md) | NTS: RSA chains on the Pico 2 W, user-added trust anchors | Accepted | 2026-10-17 |
| [0004](0004-time-source-selection-and-dcf77.md) | Time source selection (order, vote) and the DCF77 source | Accepted | 2026-10-18 |
| [0005](0005-dcf77-receiver-delay.md) | DCF77 receiver delay: manual and learned | Accepted | 2026-10-18 |
| [0006](0006-firmware-update.md) | Firmware update: an online check now, a signed install later | Step 1 accepted, step 2 proposed (revised) | 2026-10-18 |
