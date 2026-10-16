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

const FLASH_SIZE: usize = 2 * 1024 * 1024;
const OFFSET: u32 = (FLASH_SIZE - ERASE_SIZE) as u32;
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
