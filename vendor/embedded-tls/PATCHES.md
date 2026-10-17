# Changes to the vendored embedded-tls

Upstream: https://github.com/embassy-rs/embedded-tls at commit `58126ee5c4` (2026-10-14), Apache-2.0, see `LICENSE`.
The 0.19.0 release on crates.io was not usable: its `webpki` feature depends on `rustls-webpki` 0.101, which pulls in
`ring`, and `ring` does not build for the RP2040 (`thumbv6m-none-eabi`). The main branch uses `rustls-webpki` 0.103
without a provider and `embassy-crypto` for the primitives.

Changes, kept small so they can be offered upstream:

1. `Cargo.toml`: the `embassy-crypto` dependency points at the Embassy revision the firmware already pins
   (`cbb7990a5cce4be3650d804f9733f4a01bb1a967`), because `embassy-crypto` declares `links` and two revisions cannot
   coexist. The dev-dependencies, tests and examples are not vendored.
2. `src/key_schedule.rs`: keeps `exp master` (RFC 8446 section 7.1) and adds `KeySchedule::export`, the TLS exporter
   of RFC 8446 section 7.5. `LABEL_BUFFER_LEN` grows by 40 because the NTS exporter label (30 bytes) does not fit the
   original 70-byte buffer.
3. `src/asynch.rs`: `TlsConnection::export_keying_material(label, context, out)` (the blocking API is not patched; nothing here uses it).

Verification: `core/tests/tls_exporter_openssl.rs` makes the client and `openssl s_server -keymatexport` derive the same
material from one handshake; `core/tests/nts_live.rs` (ignored by default) completes a real NTS-KE and an
authenticated NTP exchange, which only works if the context-bearing derivation is right.

Offered upstream on 2026-10-17 as https://github.com/embassy-rs/embedded-tls/pull/221 (parts 2 and 3, rewritten without the picoclock comments, with a rustls-based test). This copy stays until that is merged and released.
