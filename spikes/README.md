# Spikes

Throwaway experiments that answer a design question with a measurement. They are separate Cargo workspaces, not part of
the product, and are not run by `tools/gate.sh`. Results are recorded in `docs/design/adr/` and
`docs/verification-log.md`.

- `crypto-bench/`: times SHA-256, AES-GCM, AES-SIV, ECDSA P-256, ECDH P-256 and X25519 on an RP2040 (flash it, read the
  RTT log with `defmt-print`; it replaces the clock firmware until the product image is flashed again).
- `rustls-check/`: compile check only. `cargo build --release --target thumbv8m.main-none-eabihf --lib` works;
  `--target thumbv6m-none-eabi` fails (no atomic compare-and-swap).
- `rustls-server-host/`: `rustls` with the RustCrypto provider as a TLS 1.3 server on the host, to try with `curl` and
  `openssl s_client` (usage in `src/main.rs`; it needs a certificate and a PKCS#8 key, for example from `openssl`).
