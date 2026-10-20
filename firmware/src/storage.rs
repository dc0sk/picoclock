//! Configuration persistence in the last 4 KiB flash sector plus a backup sector (both excluded from the program image by
//! `memory-*.x`). The record carries a magic, length and CRC32, so erased or torn flash simply reads as "no config"; the
//! two copies are written one after the other (`picoclock_core::slots`), so a power cut during a save loses nothing.

use defmt::{info, warn};
use embassy_rp::flash::{ERASE_SIZE, Flash};
use embassy_rp::mode::Blocking;
use embassy_rp::Peri;
use embassy_rp::peripherals::FLASH;
use embassy_time::{Duration, with_timeout};
use picoclock_core::config::Config;

use crate::state;

#[cfg(all(feature = "rp2040", feature = "https"))]
compile_error!("the HTTPS records of storage.rs only fit the Pico 2 W layout (tools/combine_image.py check-layout)");

/// Pico W: 2 MiB, Pico 2 W: 4 MiB. The configuration lives in the last sector (memory-*.x keeps it free).
#[cfg(feature = "rp2040")]
const FLASH_SIZE: usize = 2 * 1024 * 1024;
#[cfg(feature = "rp235x")]
const FLASH_SIZE: usize = 4 * 1024 * 1024;
const OFFSET: u32 = (FLASH_SIZE - ERASE_SIZE) as u32;
/// The sector before it holds the HTTPS key and certificates (Pico 2 W only; `memory-rp235x.x` keeps it free).
#[cfg(feature = "https")]
const TLS_OFFSET: u32 = (FLASH_SIZE - 2 * ERASE_SIZE) as u32;
/// The sector for the NTS roots the user added (both chips): before the HTTPS sector on the Pico 2 W, before the settings on the Pico W.
#[cfg(feature = "https")]
const ROOTS_OFFSET: u32 = (FLASH_SIZE - 3 * ERASE_SIZE) as u32;
#[cfg(not(feature = "https"))]
const ROOTS_OFFSET: u32 = (FLASH_SIZE - 2 * ERASE_SIZE) as u32;
/// The second copy of the settings, below all the other records. Written before the primary sector, read when that is invalid.
#[cfg(feature = "https")]
const BACKUP_OFFSET: u32 = (FLASH_SIZE - 4 * ERASE_SIZE) as u32;
#[cfg(not(feature = "https"))]
const BACKUP_OFFSET: u32 = (FLASH_SIZE - 3 * ERASE_SIZE) as u32;
/// The second copies of the HTTPS record and of the NTS roots, and the trust record (revoked signing keys) with its second copy,
/// all in the free sectors that `memory-*.x` keeps in front of the settings (the "trust sector"). Nothing here moves with a
/// release: these sectors were reserved and empty.
#[cfg(feature = "https")]
const TLS_BACKUP_OFFSET: u32 = (FLASH_SIZE - 5 * ERASE_SIZE) as u32;
#[cfg(feature = "https")]
const ROOTS_BACKUP_OFFSET: u32 = (FLASH_SIZE - 6 * ERASE_SIZE) as u32;
#[cfg(feature = "https")]
const TRUST_OFFSET: u32 = (FLASH_SIZE - 7 * ERASE_SIZE) as u32;
#[cfg(feature = "https")]
const TRUST_BACKUP_OFFSET: u32 = (FLASH_SIZE - 8 * ERASE_SIZE) as u32;
#[cfg(not(feature = "https"))]
const ROOTS_BACKUP_OFFSET: u32 = (FLASH_SIZE - 4 * ERASE_SIZE) as u32;
#[cfg(not(feature = "https"))]
const TRUST_OFFSET: u32 = (FLASH_SIZE - 5 * ERASE_SIZE) as u32;
#[cfg(not(feature = "https"))]
const TRUST_BACKUP_OFFSET: u32 = (FLASH_SIZE - 6 * ERASE_SIZE) as u32;
const RECORD: usize = 2048;
/// Edits via buttons arrive in bursts; wait for this long without a new one before writing flash.
const QUIET: Duration = Duration::from_secs(3);

pub type Store = Flash<'static, Blocking, FLASH_SIZE>;

pub fn new(flash: Peri<'static, FLASH>) -> Store {
    Flash::new_blocking(flash)
}

/// The settings sector and its backup, as `picoclock_core::slots` sees them.
struct Settings<'a>(&'a mut Store);

impl picoclock_core::slots::Sectors for Settings<'_> {
    type Error = embassy_rp::flash::Error;
    fn read(&mut self, slot: usize, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.0.blocking_read(slot_offset(slot), buf)
    }
    fn erase(&mut self, slot: usize) -> Result<(), Self::Error> {
        self.0.blocking_erase(slot_offset(slot), slot_offset(slot) + ERASE_SIZE as u32)
    }
    fn write(&mut self, slot: usize, data: &[u8]) -> Result<(), Self::Error> {
        self.0.blocking_write(slot_offset(slot), data)
    }
}

fn slot_offset(slot: usize) -> u32 {
    if slot == picoclock_core::slots::PRIMARY { OFFSET } else { BACKUP_OFFSET }
}

pub fn load(flash: &mut Store) -> Option<Config> {
    picoclock_core::slots::load(&mut Settings(flash), &mut [0u8; RECORD], Config::from_bytes)
}

/// A record kept in two sectors (`picoclock_core::slots`): the offsets of the primary and the backup sector.
struct Pair<'a>(&'a mut Store, u32, u32);

impl picoclock_core::slots::Sectors for Pair<'_> {
    type Error = embassy_rp::flash::Error;
    fn read(&mut self, slot: usize, buf: &mut [u8]) -> Result<(), Self::Error> {
        let at = self.at(slot);
        self.0.blocking_read(at, buf)
    }
    fn erase(&mut self, slot: usize) -> Result<(), Self::Error> {
        let at = self.at(slot);
        self.0.blocking_erase(at, at + ERASE_SIZE as u32)
    }
    fn write(&mut self, slot: usize, data: &[u8]) -> Result<(), Self::Error> {
        let at = self.at(slot);
        self.0.blocking_write(at, data)
    }
}

impl Pair<'_> {
    fn at(&self, slot: usize) -> u32 {
        if slot == picoclock_core::slots::PRIMARY { self.1 } else { self.2 }
    }
}

#[cfg(feature = "https")]
fn tls_pair(flash: &mut Store) -> Pair<'_> {
    Pair(flash, TLS_OFFSET, TLS_BACKUP_OFFSET)
}
fn roots_pair(flash: &mut Store) -> Pair<'_> {
    Pair(flash, ROOTS_OFFSET, ROOTS_BACKUP_OFFSET)
}
fn trust_pair(flash: &mut Store) -> Pair<'_> {
    Pair(flash, TRUST_OFFSET, TRUST_BACKUP_OFFSET)
}

#[cfg(feature = "https")]
pub fn load_tls(flash: &mut Store) -> Option<picoclock_core::tlsstore::Material> {
    picoclock_core::slots::load(&mut tls_pair(flash), &mut [0u8; picoclock_core::tlsstore::MAX_RECORD], picoclock_core::tlsstore::Material::decode)
}

#[cfg(feature = "https")]
fn save_tls(flash: &mut Store, m: &picoclock_core::tlsstore::Material) -> Result<(), embassy_rp::flash::Error> {
    let mut buf = [0xFFu8; picoclock_core::tlsstore::MAX_RECORD];
    m.encode(&mut buf).ok_or(embassy_rp::flash::Error::OutOfBounds)?;
    picoclock_core::slots::save(&mut tls_pair(flash), &buf)
}

pub fn load_roots(flash: &mut Store) -> Option<picoclock_core::rootstore::Roots> {
    picoclock_core::slots::load(&mut roots_pair(flash), &mut [0u8; picoclock_core::rootstore::MAX_RECORD], picoclock_core::rootstore::Roots::decode)
}

fn save_roots(flash: &mut Store, roots: &picoclock_core::rootstore::Roots) -> Result<(), embassy_rp::flash::Error> {
    let mut buf = [0xFFu8; picoclock_core::rootstore::MAX_RECORD];
    roots.encode(&mut buf).ok_or(embassy_rp::flash::Error::OutOfBounds)?;
    picoclock_core::slots::save(&mut roots_pair(flash), &buf)
}

/// The revoked signing keys (bitmask); nothing is revoked when no record exists.
pub fn load_trust(flash: &mut Store) -> picoclock_core::trust::Trust {
    picoclock_core::slots::load(&mut trust_pair(flash), &mut [0u8; 64], picoclock_core::trust::Trust::decode).unwrap_or_default()
}

pub fn save_trust(flash: &mut Store, t: &picoclock_core::trust::Trust) -> Result<(), embassy_rp::flash::Error> {
    let mut buf = [0xFFu8; 64];
    buf[..picoclock_core::trust::RECORD_LEN].copy_from_slice(&t.encode());
    picoclock_core::slots::save(&mut trust_pair(flash), &buf)?;
    if load_trust(flash) == *t { Ok(()) } else { Err(embassy_rp::flash::Error::OutOfBounds) }
}

/// Copies a record that only one of its two sectors holds (written before the second one existed, or torn) to the other.
fn heal(flash: &mut Store) {
    use picoclock_core::slots::heal;
    #[cfg(feature = "https")]
    if let Ok(true) = heal(&mut tls_pair(flash), &mut [0u8; picoclock_core::tlsstore::MAX_RECORD], picoclock_core::tlsstore::Material::decode) {
        info!("storage: certificate copy restored");
    }
    if let Ok(true) = heal(&mut roots_pair(flash), &mut [0u8; picoclock_core::rootstore::MAX_RECORD], picoclock_core::rootstore::Roots::decode) {
        info!("storage: NTS roots copy restored");
    }
    if let Ok(true) = heal(&mut trust_pair(flash), &mut [0u8; 64], picoclock_core::trust::Trust::decode) {
        info!("storage: trust record copy restored");
    }
    if let Ok(true) = heal(&mut Settings(flash), &mut [0u8; RECORD], Config::from_bytes) {
        info!("storage: settings copy restored");
    }
}

/// Pending until the HTTPS certificate must be saved; never, in a build without HTTPS.
async fn tls_request() {
    #[cfg(feature = "https")]
    state::TLS_SAVE.wait().await;
    #[cfg(not(feature = "https"))]
    core::future::pending::<()>().await
}

fn save(flash: &mut Store, cfg: &Config) -> Result<(), embassy_rp::flash::Error> {
    let mut buf = [0xFFu8; RECORD];
    if cfg.to_bytes(&mut buf).is_err() {
        warn!("storage: config does not fit the record");
        return Err(embassy_rp::flash::Error::OutOfBounds);
    }
    // The CPU stalls while flash is erased (tens of ms per sector, two sectors); the display blinks once, which is acceptable.
    picoclock_core::slots::save(&mut Settings(flash), &buf)
}

/// Erases the settings, the user's NTS roots and (Pico 2 W) the HTTPS key and certificate, then restarts. Only the erased
/// sectors decide what the next boot sees: no record means defaults, no roots, and a new self-signed certificate. If an erase
/// fails the clock is left running with what it has (`show` and `root` tell), because a restart would hide the failure.
async fn factory_reset(flash: &mut Store) {
    // the web page's answer is still on its way out; give it time to leave before the clock goes away
    embassy_time::Timer::after_millis(1500).await;
    warn!("storage: factory reset");
    let mut ok = true;
    // both copies of the settings: the backup would otherwise bring them back at the next start
    ok &= picoclock_core::slots::erase_all(&mut Settings(flash)).is_ok();
    // the trust record (revoked signing keys) is deliberately kept: a revoked key must not come back
    ok &= picoclock_core::slots::erase_all(&mut roots_pair(flash)).is_ok();
    #[cfg(feature = "https")]
    {
        ok &= picoclock_core::slots::erase_all(&mut tls_pair(flash)).is_ok();
        ok &= load_tls(flash).is_none();
    }
    ok &= load(flash).is_none() && load_roots(flash).is_none();
    if ok {
        state::forget_watchdog_marker();
        cortex_m::peripheral::SCB::sys_reset();
    }
    warn!("storage: factory reset failed; not restarting");
}

/// Tells the bootloader that this image works (ADR-0006, point 14): if it was just swapped in and never says so, the bootloader
/// swaps the old image back at the next start. "Works" is local only: the clock task has run for 20 s without a stall. WiFi
/// is not part of it, or a router outage would revert a good image. A fresh install (no swap yet) is marked the same way,
/// which writes the same magic again and is harmless.
async fn confirm_boot_signal() {
    loop {
        embassy_time::Timer::after_secs(20).await;
        let age_ms = (embassy_time::Instant::now().as_millis() as u32).wrapping_sub(state::HEARTBEAT_MS.load(core::sync::atomic::Ordering::Relaxed));
        if age_ms < 5_000 {
            break;
        }
    }
}

fn confirm_boot(flash: &mut Store) {
    use core::cell::RefCell;
    use embassy_boot_rp::{AlignedBuffer, BlockingFirmwareState, FirmwareUpdaterConfig};
    use embassy_sync::blocking_mutex::{Mutex, raw::NoopRawMutex};
    let shared = Mutex::<NoopRawMutex, _>::new(RefCell::new(&mut *flash));
    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(&shared, &shared);
    let mut magic = AlignedBuffer([0; 1]);
    let mut state = BlockingFirmwareState::from_config(config, &mut magic.0);
    match state.mark_booted() {
        Ok(()) => info!("boot: image confirmed"),
        Err(_) => warn!("boot: could not confirm the image"),
    }
}

/// One message of the install conversation (`crate::install`): the staged image waits here for `Apply`.
async fn install_step(flash: &mut Store, msg: state::InstallMsg, stager: &mut Option<crate::install::Stager>, staged: &mut Option<crate::install::Checked>) {
    use crate::install::{self, Failure};
    use picoclock_core::image::Install;
    use state::InstallMsg::*;
    match msg {
        Begin(len) => {
            *staged = None;
            *stager = None;
            match install::begin(len) {
                Ok(s) => {
                    *stager = Some(s);
                    state::set_install(Install::Receiving);
                    state::INSTALL_REPLY.signal(true);
                }
                Err(why) => {
                    warn!("install: refused: {}", why.message());
                    state::set_install(Install::Refused(why));
                    state::INSTALL_REPLY.signal(false);
                }
            }
        }
        Block(data) => {
            if let Some(s) = stager.as_mut() {
                if s.block(flash, &data).is_err() {
                    warn!("install: flash write failed");
                    state::set_install(Install::WriteFailed);
                    *stager = None;
                }
            }
        }
        Finish => {
            let result = match stager.take() {
                Some(s) => s.finish(flash).await,
                None => Err(Failure::Flash),
            };
            install::log_result(&result);
            *staged = result.as_ref().ok().copied();
            state::set_install(install::install_of(&result));
            state::INSTALL_REPLY.signal(result.is_ok());
        }
        Abort => {
            *stager = None;
            *staged = None;
            state::set_install(Install::Idle);
        }
        Apply(shown) => {
            let Some(checked) = *staged else {
                state::INSTALL_REPLY.signal(false);
                return;
            };
            // the user confirmed the image that was shown to them; a different one may have been staged since
            if checked.version != shown {
                warn!("install: the staged image is not the one that was confirmed");
                state::INSTALL_REPLY.signal(false);
                return;
            }
            if let Err(why) = install::check_markable(flash) {
                warn!("install: {}", why);
                state::INSTALL_REPLY.signal(false);
                return;
            }
            {
                // The revocation comes first: if the record cannot be written the image is not installed (a swap without it
                // would leave the key the image revokes trusted). It stays recorded even if the new image does not confirm.
                if checked.revokes_more {
                    let trust = picoclock_core::trust::Trust { revoked: checked.revoked };
                    if save_trust(flash, &trust).is_err() {
                        warn!("install: the trust record could not be written; not installing");
                        state::INSTALL_REPLY.signal(false);
                        return;
                    }
                    state::with_runtime(|r| r.revoked_keys = checked.revoked);
                    info!("install: signing keys revoked: {}", checked.revoked);
                }
                match install::mark_swap(flash) {
                    Ok(()) => {
                        info!("install: swap marked; restarting");
                        embassy_time::Timer::after_millis(300).await;
                        state::forget_watchdog_marker();
                        cortex_m::peripheral::SCB::sys_reset();
                    }
                    Err(why) => {
                        warn!("install: {}", why);
                        state::INSTALL_REPLY.signal(false);
                    }
                }
            }
        }
    }
}

#[embassy_executor::task]
pub async fn storage_task(mut flash: Store) -> ! {
    heal(&mut flash);
    // the install conversation runs beside the 20-second wait for the boot confirmation, so an upload is never held up by it
    let mut stager: Option<crate::install::Stager> = None;
    let mut staged: Option<crate::install::Checked> = None;
    {
        use embassy_futures::select::{Either, select};
        let confirm = confirm_boot_signal();
        let mut confirm = core::pin::pin!(confirm);
        loop {
            match select(&mut confirm, state::INSTALL_IN.receive()).await {
                Either::First(()) => break,
                Either::Second(msg) => install_step(&mut flash, msg, &mut stager, &mut staged).await,
            }
        }
    }
    confirm_boot(&mut flash);
    loop {
        use embassy_futures::select::{Either, Either4, select, select4};
        let ev = select(state::INSTALL_IN.receive(), select4(state::SAVE_REQUEST.wait(), tls_request(), state::ROOTS_SAVE.wait(), state::FACTORY_RESET.wait())).await;
        let ev = match ev {
            Either::First(msg) => {
                install_step(&mut flash, msg, &mut stager, &mut staged).await;
                continue;
            }
            Either::Second(ev) => ev,
        };
        let mut now = match ev {
            Either4::First(now) => now,
            Either4::Fourth(()) => {
                factory_reset(&mut flash).await;
                continue;
            }
            Either4::Second(()) => {
                // read back, like the settings: a silent write failure must not look like success
                #[cfg(feature = "https")]
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
            Either4::Third(()) => {
                let roots = state::roots_snapshot();
                match save_roots(&mut flash, &roots) {
                    Ok(()) if load_roots(&mut flash).as_ref() == Some(&roots) => info!("storage: NTS roots saved"),
                    Ok(()) => warn!("storage: NTS roots verify failed"),
                    Err(_) => warn!("storage: NTS roots write failed"),
                }
                continue;
            }
        };
        while !now {
            match with_timeout(QUIET, state::SAVE_REQUEST.wait()).await {
                Ok(n) => now = n,
                Err(_) => break,
            }
        }
        let cfg = state::persisted_snapshot();
        match save(&mut flash, &cfg) {
            Ok(()) => {
                // Read back: a silent write failure must not be reported as saved.
                if load(&mut flash).as_ref() == Some(&cfg) {
                    state::refresh_unsaved();
                    info!("storage: saved");
                } else {
                    warn!("storage: verify failed");
                }
            }
            Err(_) => warn!("storage: write failed"),
        }
    }
}
