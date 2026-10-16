//! Hardware-independent logic for the Pico-Clock-Green firmware, kept separate so it can be unit-tested on the host.
#![cfg_attr(not(test), no_std)]

pub mod cli;
pub mod config;
pub mod font;
pub mod geo;
pub mod http;
pub mod nts;
pub mod nts_ke;
pub mod frame;
pub mod sntp;
pub mod time;
pub mod tz;
pub mod ui;
pub mod tzdb;
