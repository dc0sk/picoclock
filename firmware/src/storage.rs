//! Configuration persistence in the last 4 KiB flash sector (excluded from the program image by `memory.x`).
//! The record carries a magic, length and CRC32, so erased or torn flash simply reads as "no config".

use defmt::{info, warn};
use embassy_rp::flash::{ERASE_SIZE, Flash};
use embassy_rp::mode::Blocking;
use embassy_rp::Peri;
use embassy_rp::peripherals::FLASH;
use embassy_time::{Duration, with_timeout};
use picoclock_core::config::Config;

use crate::state;

/// Pico W: 2 MiB, Pico 2 W: 4 MiB. The configuration lives in the last sector (memory-*.x keeps it free).
#[cfg(feature = "rp2040")]
const FLASH_SIZE: usize = 2 * 1024 * 1024;
#[cfg(feature = "rp235x")]
const FLASH_SIZE: usize = 4 * 1024 * 1024;
const OFFSET: u32 = (FLASH_SIZE - ERASE_SIZE) as u32;
/// The sector before it holds the HTTPS key and certificates (Pico 2 W only; `memory-rp235x.x` keeps it free).
#[cfg(feature = "https")]
const TLS_OFFSET: u32 = (FLASH_SIZE - 2 * ERASE_SIZE) as u32;
const RECORD: usize = 2048;
/// Edits via buttons arrive in bursts; wait for this long without a new one before writing flash.
const QUIET: Duration = Duration::from_secs(3);

pub type Store = Flash<'static, Blocking, FLASH_SIZE>;

pub fn new(flash: Peri<'static, FLASH>) -> Store {
    Flash::new_blocking(flash)
}

pub fn load(flash: &mut Store) -> Option<Config> {
    let mut buf = [0u8; RECORD];
    flash.blocking_read(OFFSET, &mut buf).ok()?;
    Config::from_bytes(&buf)
}

#[cfg(feature = "https")]
pub fn load_tls(flash: &mut Store) -> Option<picoclock_core::tlsstore::Material> {
    let mut buf = [0u8; picoclock_core::tlsstore::MAX_RECORD];
    flash.blocking_read(TLS_OFFSET, &mut buf).ok()?;
    picoclock_core::tlsstore::Material::decode(&buf)
}

#[cfg(feature = "https")]
fn save_tls(flash: &mut Store, m: &picoclock_core::tlsstore::Material) -> Result<(), embassy_rp::flash::Error> {
    let mut buf = [0xFFu8; picoclock_core::tlsstore::MAX_RECORD];
    m.encode(&mut buf).ok_or(embassy_rp::flash::Error::OutOfBounds)?;
    flash.blocking_erase(TLS_OFFSET, TLS_OFFSET + ERASE_SIZE as u32)?;
    flash.blocking_write(TLS_OFFSET, &buf)
}

fn save(flash: &mut Store, cfg: &Config) -> Result<(), embassy_rp::flash::Error> {
    let mut buf = [0xFFu8; RECORD];
    if cfg.to_bytes(&mut buf).is_err() {
        warn!("storage: config does not fit the record");
        return Err(embassy_rp::flash::Error::OutOfBounds);
    }
    // The CPU stalls while flash is erased (tens of ms); the display blinks once, which is acceptable.
    flash.blocking_erase(OFFSET, OFFSET + ERASE_SIZE as u32)?;
    flash.blocking_write(OFFSET, &buf)
}

#[embassy_executor::task]
pub async fn storage_task(mut flash: Store) -> ! {
    loop {
        #[cfg(feature = "https")]
        let mut now = match embassy_futures::select::select(state::SAVE_REQUEST.wait(), state::TLS_SAVE.wait()).await {
            embassy_futures::select::Either::First(now) => now,
            embassy_futures::select::Either::Second(()) => {
                // read back, like the settings: a silent write failure must not look like success
                match state::tls_material() {
                    Some(m) => match save_tls(&mut flash, &m) {
                        Ok(()) if load_tls(&mut flash).as_ref() == Some(&m) => info!("storage: certificate saved"),
                        Ok(()) => warn!("storage: certificate verify failed"),
                        Err(_) => warn!("storage: certificate write failed"),
                    },
                    None => warn!("storage: no certificate to save"),
                }
                continue;
            }
        };
        #[cfg(not(feature = "https"))]
        let mut now = state::SAVE_REQUEST.wait().await;
        while !now {
            match with_timeout(QUIET, state::SAVE_REQUEST.wait()).await {
                Ok(n) => now = n,
                Err(_) => break,
            }
        }
        let cfg = state::config_snapshot();
        match save(&mut flash, &cfg) {
            Ok(()) => {
                // Read back: a silent write failure must not be reported as saved.
                if load(&mut flash).as_ref() == Some(&cfg) {
                    state::with_runtime(|r| r.unsaved = false);
                    info!("storage: saved");
                } else {
                    warn!("storage: verify failed");
                }
            }
            Err(_) => warn!("storage: write failed"),
        }
    }
}
