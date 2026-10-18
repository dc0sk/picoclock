---
project: picoclock
doc: docs/design/adr/0006-firmware-update.md
status: ledger
last_updated: 2026-10-18
---

# 0006. Firmware update: an online check now, a signed install later

## Status

Step 1 (the check) is **Accepted** and delivered (REQ-UPD-01 to 03). Step 2 (the install) is **Proposed**, revised after the design review of
2026-10-18 (see "Revision after the design review"): the owner agreed on 2026-10-18 to the approach (check first, notify, install only on the user's request; GitHub releases as the source; a signing key
of their own in GnuPG; a one-time bootloader reflash on both boards), but nothing of it is built.

## Context

Until 0.2.0 the only way to a new firmware was a UF2 file copied to the board in BOOTSEL mode, or a debug probe. The owner asked
for an online check and an update function. The Pico has no bootloader today, and the clock sits in a case with only a USB
cable, so an update that goes wrong must be recoverable, and one that is offered must be authentic.

## Decision

**Step 1, the check (built).**
1. The clock asks `GET /repos/dc0sk/picoclock/releases/latest` on `api.github.com` over TLS 1.3, once a day (the first time 3 minutes
   after joining; a failure is retried after an hour, a rate limit after six hours) and on request (`update check`, a button on the
   web page's Status page). It reads the first 4 KB of the answer and takes `tag_name`; nothing else of the JSON is used.
2. The result (newer, up to date, ahead, no release yet, rate limited, failed) is shown in `status`, `update` and on the web page
   with the address of the releases page. The setting `update_check off` stops the daily check. **Nothing is downloaded or installed.**
3. The check trusts only the **USERTrust ECC Certification Authority** root (pinned, `UPDATE_ROOTS`), the root of GitHub's chain
   (Sectigo E36 under Root E46 under USERTrust ECC; all ECDSA, so the Pico W can verify it). The NTS roots and the roots the user added
   are not used for it. Like the NTS key exchange, the answer is only used after the slow certificate and signature checks passed.

**Step 2, the install (proposed).**
4. **Bootloader:** `embassy-boot-rp` (in the pinned Embassy, with examples for the RP2040 and the RP2350): two flash slots (active and
   download), a state sector, and a swap that survives a power cut. Both images fit: about 0.74 MB on the Pico W (2 MB flash) and
   1.33 MB on the Pico 2 W (4 MB flash), from the UF2 sizes of 2026-10-18. The settings sectors stay where they are (end of flash).
   The first bootloader is flashed once over USB (UF2) or the debug probe; a bootloader that does not start is still recoverable
   with BOOTSEL.
5. **Signature (option A of the discussion):** a raw **Ed25519 signature over the SHA-512 digest of the image**, made with the owner's
   GnuPG agent (`SIGKEY`, `SETHASH --hash=sha512`, `PKSIGN`), checked by Embassy's own verifier in the bootloader. Tested on
   2026-10-18 with a throwaway key: the agent's signature verifies over the digest with a standard Ed25519 library. A normal
   `gpg --detach-sign` signature may be published next to it for people; the clock ignores it, so there is no OpenPGP parser in the
   bootloader.
6. **Keys:** a dedicated signing key (fingerprint D49B00BC5555CB48D2DBCF4AA84FE2521C649368) and a backup key kept apart
   (908DDDFA33D6DCCC3E3BE550612218B1165CF3A1) were generated on 2026-10-18; both public keys go into the bootloader, so a lost or
   compromised key can be retired by a release signed with the other, without reflashing the bootloader. The private keys are in
   `local/` (git-ignored) until the owner imports them.
7. **Safeguards:** the install starts only on the user's request and is confirmed twice like the factory reset (terminal; web over
   HTTPS only, same-origin); the downloaded image is verified before it is marked for the swap and again by the bootloader; a
   version that is not newer than the running one is refused (rollback protection); the new image must confirm itself after
   starting (the watchdog already restarts a hung clock) or the bootloader swaps back.

## Revision after the design review (2026-10-18)

A review of step 2 against the code (Fable, read-only, model output checked against the repository by the author; image sizes
measured from the release ELFs with `size -A`) found problems in points 4 to 7. The owner decided on 2026-10-18 that **the Pico W
stays limited**. Points 4 to 7 stand except where this section says otherwise.

8. **Pico W: no network install.** The release files are served from `release-assets.githubusercontent.com` (a redirect from
   `github.com`, checked 2026-10-18), whose chain is RSA (Let's Encrypt YR1 under ISRG Root X1). The Pico 2 W can pin ISRG Root X1
   (`core/roots/isrg-root-x1.der`); the Pico W has no RSA and could only fetch the image without authenticating the host. It will
   not do that. On the Pico W the signed image is installed by upload (web page or terminal); the Ed25519 signature is checked
   the same way. The update check keeps working on both boards. Fetching without authentication was rejected on policy
   (ASSUMED, not built).
9. **The version and the chip are inside the signed bytes.** A fixed-offset info block after the vector table (magic, chip id,
   version, image length) is covered by the signature; the application reads it from the download slot only after the signature
   verified. The rollback rule and "an RP2040 image on an RP2350" are decided on signed data, never on the release tag or the file
   name. Rollback protection lives in the application (the bootloader has no version logic).
10. **Revocation is an application rule.** The bootloader is never updated by the swap, so the two embedded keys cannot be retired
    there. The application keeps the key list and a revocation set in a small trust sector that a factory reset does not erase; a
    release signed with the other key can revoke the first. The bootloader check stays as the second line.
11. **Signature trailer:** the application writes length, 64-byte signature and key index in a trailer page of the download slot
    last; the bootloader reads it before it prepares the swap. Missing or bad: nothing is swapped. An interrupted download leaves
    the state at "boot", so the running image is untouched.
12. **Release files:** raw `.bin` images linked for the active slot are added next to the UF2 files (UF2 stays for the one-time
    BOOTSEL flash of bootloader plus application). A release script signs the SHA-512 digest through `gpg-agent` and verifies the
    result on the host before anything is published. The dedicated keys sign only images the owner built.
13. **Layout (first proposal):** Pico W (2 MiB): bootloader 64 KiB, state 4 KiB, active 976 KiB, download 980 KiB, trust sector
    12 KiB, settings as today. Pico 2 W (4 MiB): bootloader 64 KiB, state 4 KiB, active 1984 KiB, download 1988 KiB, trust sector,
    settings as today. The Pico W application is 751 KiB today, so about 225 KiB remain before the verifier (estimated 30 to 45
    KiB, not measured) is added; the Pico 2 W application is 1.29 MiB. The boot ROM's secure boot of the RP2350 is not used (its
    OTP settings cannot be undone).
14. **Boot confirmation** uses local health only (settings loaded, main loop running), never "WiFi joined", or a router outage
    would revert a good image. The watchdog is fed first thing in the application, since the bootloader's watchdog keeps running.

Build order: release tooling and host verifier; memory maps and bootloader; upload path into the download slot (Pico W and Pico 2
W, no network needed); streaming download with one redirect (Pico 2 W only); rollback rule and two-step confirmation; revocation.

## Alternatives considered

### A. Ask the API, not the `releases/latest` redirect page — chosen (assumed)
`github.com/<repo>/releases/latest` redirects to the tag and would need no JSON, but it is not a documented interface. The API is,
and its answer's first 4 KB contain the tag (measured on rust-lang/rust, 2026-10-18: the whole answer was 3.6 KB).

### B. Trust the NTS roots or all built-in roots for the check — REJECTED (assumed)
It would let every authority that NTS trusts, and any root the user added, vouch for the connection that announces a firmware.
The check cannot install anything, but a narrower trust costs one 659-byte file.

### C. A standard OpenPGP detached signature verified on the clock — REJECTED (assumed, not built)
Anyone could check it with `gpg --verify`, and one signature would serve both purposes, but the bootloader would need its own parser
for an untrusted format and Embassy's verifier could not be reused. Rejected on reasoning alone.

### D. A separate plain Ed25519 key outside GnuPG (minisign style) — REJECTED (assumed)
The simplest, but another key to protect; the owner already keeps GnuPG keys.

### E. Install by writing the running image from the application, without a bootloader — REJECTED (assumed)
A power cut or a bad download would leave a board that only BOOTSEL can revive, and the flash cannot be written while the code runs
from it without a RAM-resident copy routine; the swap with a state sector solves both.

## Consequences

- The clock now contacts GitHub (and so reveals its address to it) once a day unless `update_check` is off.
- The check works on both boards (all-ECDSA chain). The release-download hosts' certificate chains were not verified (an
  openssl check from a PC was inconclusive), so step 2 must test them, and the Pico W cannot verify RSA chains.
- The check was proved on the host against the real API (`core/tests/update_live.rs`) and on a Pico 2 W (VR-63). No release of the
  project exists yet, so the "newer version" path was exercised against another public repository.
- Step 2 changes the flash layout and needs a release process (build both images, sign, publish); neither exists yet.
