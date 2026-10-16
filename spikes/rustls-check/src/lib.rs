#![no_std]
extern crate alloc;
use alloc::sync::Arc;
use rustls::server::{ServerConfig, UnbufferedServerConnection};

pub fn make(cfg: Arc<ServerConfig>) -> UnbufferedServerConnection {
    UnbufferedServerConnection::new(cfg).unwrap()
}

fn no_rng(_buf: &mut [u8]) -> Result<(), getrandom::Error> {
    Err(getrandom::Error::UNSUPPORTED)
}
getrandom::register_custom_getrandom!(no_rng);
