//! Hardware-independent logic for the Pico-Clock-Green firmware, kept separate so it can be unit-tested on the host.
#![cfg_attr(not(test), no_std)]

pub mod cli;

/// Software version, from the package version in `Cargo.toml` (the firmware package carries the same number; a test checks it).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY: &str = "https://github.com/dc0sk/picoclock";
pub const LICENSE: &str = "GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later)";

#[cfg(test)]
mod tests {
    // VERIFIES: REQ-CFG-06
    #[test]
    fn firmware_and_core_carry_the_same_version() {
        let firmware = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../firmware/Cargo.toml")).unwrap();
        let line = firmware.lines().find(|l| l.starts_with("version = ")).unwrap();
        assert_eq!(line, format!("version = \"{}\"", super::VERSION));
    }
}

pub mod config;
pub mod dcf77;
pub mod download;
pub mod entropy;
pub mod font;
pub mod geo;
pub mod http;
pub mod image;
pub mod morse;
pub mod netutil;
pub mod nts;
pub mod nts_ke;
pub mod frame;
pub mod rootstore;
pub mod sources;
pub mod slots;
pub mod sntp;
pub mod time;
pub mod update;
pub mod tlsstore;
pub mod trust;
pub mod tz;
pub mod ui;
pub mod tzdb;
pub mod x509;
