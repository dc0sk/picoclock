---
project: picoclock
doc: docs/design/reviews/2026-10-20-full-code-review.md
status: ledger
last_updated: 2026-10-20
---

# Full-code review before the GPS phase (2026-10-20)

The owner's process rule (memory `time-source-process`, ADR-0004): a review of the complete code by Fable before the development phase
of each further time source. Three reviewers read the tree at commit `319bcb9` (branch `feat/hardening`) in parallel, read-only, each
with an area: **A** update and storage, **B** networking, time keeping and time sources, **C** web, terminal, configuration, UI and the
traceability tooling. Their reports are model output; every finding below was checked against the code by the author before it was
acted on (the reviewers' claims were right in every case that was checked; where a claim was only partly right it says so).

Disposition: **fixed** (in the same round), **documented** (the behaviour stays, the text says so), **GPS** (carried into the GPS phase,
recorded in `docs/design/architecture.md` section 10), **accepted** (judged not worth the change, reason given).

## Security and correctness

| # | Finding | Disposition |
|---|---|---|
| B1 | NTP seconds 0x8000_0000 to 0x83AA_7E7F (1968 to 1970) wrapped in the epoch subtraction and wrote year 1968 / BCD garbage into the RTC | **fixed**: `sntp::parse_response` refuses dates outside 2020 to 2099 (`SntpError::Implausible`); the source task ignores such a sample and `ui_task` refuses to write one (`time::plausible_unix`); test `a_time_outside_2020_to_2099_is_refused_never_wrapped` |
| A1 | A leaked key 1 could revoke key 2 and lock the owner out (the record survives a factory reset and BOOTSEL) | **fixed**: only a later key may revoke an earlier one (REQ-UPD-10 retired, REQ-UPD-11); `tools/release.sh` insists on the backup key for `PICOCLOCK_REVOKE=1`; ADR-0006 point 18 |
| C1 | `POST /upload` on the plain port checked the password before the "HTTPS only" rule: a password oracle (401 vs 403) | **fixed**: the transport rule comes first; `https_for_upload()` also treats "HTTPS on but still starting" as HTTPS |
| C2 | `web_enabled off` closed port 80 but not the HTTPS port | **fixed**: the HTTPS task serves only while `https` and `web_enabled`; `status` says "off (the web interface is switched off)" |
| C3 | DNS rebinding: Origin equal to Host is satisfied by an attacker's own name when no web password is set | **fixed**: a browser request (one with an Origin) must carry the clock's address or a name that starts with its own name (`http::host_is_own`, `browser_ok`); test `a_rebound_name_is_not_the_clock`. Scripts without an Origin are unaffected |
| C4 | Plain port served the whole interface while HTTPS was still starting (no valid time), and README/REQ-WEB-08 said it only redirects | **fixed**: with `http_redirect` on, the plain port answers 503 until HTTPS is up. **Documented**: with `http_redirect off` the plain port serves, as the user chose (README table) |
| B3 | The step limit confirms with a second reading of the *same* source, which a receiver that is consistently wrong (GPS week rollover) satisfies | **GPS**: needs confirmation by a different source or a date floor |
| B6 | DCF77 delay learning used NTP samples the vote had rejected (`Selector::recent`) | **fixed**: `recent_used` (only a sample whose verdict was *use*); an offset beyond `i32` is no reading instead of aliasing |
| A2 = C10 | Terminal upload dropped bytes when a USB packet straddled a 4 KiB block; file bytes in the packet of the command line were taken as keystrokes | **fixed**: the terminal uses `BlockSink` like the web path, the rest of the command's packet is carried into the file (except the LF of a CRLF) |
| A3 | `INSTALL_REPLY` is one signal for up to three conversations; `Apply` was not under the claim | **fixed** for the cases that matter: `Apply` takes the install claim, so uploads, downloads and an apply never overlap |
| A4 | The revocation record was written before the swap state was checked | **fixed**: `check_markable` first, then the record, then the mark |
| A5 | The confirmation was not bound to the image it was asked for | **fixed**: `Apply(version)`; the web confirm form carries the version in a hidden field; the storage task installs only a staged image of that version |
| A6 | The watchdog was armed after the radio start-up | **fixed**: armed right after the reset reason is read, before the radio |
| A8 | `find_revocation` could miss the block when the info block sat in the last 32 bytes of the scan window | **fixed**: `find_info` accepts a block only if both blocks fit in the window |
| A9 | `release.sh` could build a revoking release signed by the wrong key | **fixed** |
| A11 | The trust record and the TLS record shared the magic `PCT1` | **fixed**: `PTR1` (never released) |
| A12 | The download fed the watchdog unconditionally | **fixed**: `feed_watchdog_if_alive` |
| B5 | `clock_ms()` was stale for up to a second after a sample was applied | **fixed**: the just-applied sample stands in until the anchor catches up |
| B7 | A failed RTC write was not retried | **fixed**: up to three more tries on the next second |
| B12 | The plain-NTP origin nonce was the uptime shown by `status` | **fixed**: random (TRNG on the Pico 2 W) |
| B13 | `Rtc::set` wrapped outside 2000 to 2099 | **fixed** by the range guard before the write |
| B11 | A forged NTS NAK makes the clock run a key exchange (5 to 7 s stall on the RP2040) | **accepted**: unavoidable by RFC 8915 section 5.7, bounded by the sync interval and the three immediate re-asks; revisit if abuse is seen |
| B14 | `DeferredVerifier::finish` could pass the 8 s watchdog on the RP2040 with a contrived user-added root | **accepted**: contrived, not measured |
| B8 | Overlapping vote groups of equal power are decided by enumeration order | **GPS**: rule (best-ranked member) to be written and tested together with the third source; adding it now would be untested code |
| A13 | embassy-boot's `mark_booted` has a ~50 ms window in which a power cut reverts a good image | **documented** (ADR-0006 point 18), safe outcome |
| A7 | The layout check did not know the record sectors of `storage.rs`; nothing stopped `rp2040` + `https` | **fixed**: `check-layout` counts the sectors from `storage.rs` and fails if they reach the download slot (sabotage-tested); `compile_error!` for the feature pair |

## Robustness, correctness, documents

| # | Finding | Disposition |
|---|---|---|
| C5 | `help` was cut at 2000 bytes: 16 setting names never shown | **fixed**: 3000-byte terminal buffer (`cli::OUTPUT_MAX`), a test that `help` fits |
| C6 | "No JavaScript" in README and REQ-WEB-11 | **fixed**: REQ-WEB-11 retired for REQ-WEB-12 (progress bar excepted), README |
| C7 | README "Still open" list stale | **fixed** |
| C8, C9 | A displaced `// VERIFIES:` bound three requirements to a test that does not exercise them; `trace.py` binds to the next `fn`, and its self-tests were not in the gate | **fixed**: `trace.py` demands `#[test]` above the function (`UNBOUND-VERIFIES`, self-tested); both self-tests run in `tools/gate.sh` |
| C11 | `respond()` head buffer (256 bytes) overflowed on a long redirect | **fixed** (512) |
| C12 | `Config::set` trimmed passwords | **fixed**: passwords keep their spaces |
| C13 | Fahrenheit rounding off by one for small negative values | **fixed** (test) |
| C14 | Alarm and DST | **documented** (README) |
| C15 | The terminal takes ASCII only | **documented** (README) |
| A10 | ADR-0006 status said "nothing built"; REQ-UPD-04 says "without a stall" but the code checks the heartbeat once at 20 s; the stutter during a download also comes from flash erases | **fixed** (ADR status, README). **Accepted**: the wording of the ratified REQ-UPD-04 stays; what the code does is "no stall at the 20 s mark and no watchdog reset", which is what protects the user |
| B15 | `status` showed the offset of an earlier sample for a held-back one | **fixed** |
| B17, B18 | Duplicated and stale section in `architecture.md`; stale comments (`net.rs`, `main.rs`, `nts_ke.rs`, `crypto.rs`) | **fixed**; ADR-0004 point 8 records the step limit |
| B10, B16 | Flash saves from the source path stall the scan and a DCF77 pulse; every sample clones the config and logs | **GPS**: keep flash writes away from the PPS producer; rate-limit the producer |

## What the GPS phase has to settle (from B2, B3, B4, B8, B9, B10, B16, C16)

Recorded in `docs/design/architecture.md` section 10. In short: PPS edges stamped in an interrupt; a per-source keep rule and a
minimum RTC rewrite interval; confirmation of a far-away time by a different source; `Status.sources`, the "synced" line, the vote keys
and the web/terminal text written for two sources; the settings record has about 117 bytes of headroom (1731 of 2048 bytes in the
worst case); the vote tie rule; `fresh_for_ms` for GPS.

## Found sound (selected)

No path marks an unverified image for the swap; every flash record uses two sectors with a CRC and survives a cut at each step
(host-simulated); the memory maps and the combined image agree; the NTS and NTS-KE parsers bound every length and verify before use;
TLS 1.3 only; time arithmetic, the timezone rules and the DCF77 decoder; request parsing limits, HTML escaping of every echoed value,
constant-time password comparison, the two-step confirmations; no passwords in logs.
