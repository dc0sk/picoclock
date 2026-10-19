//! Stages an update file in the bootloader's download slot and, on the user's word, tells the bootloader to swap it in
//! (ADR-0006). The storage task owns the flash and runs these steps; the terminal (and later the web page) feeds it blocks.
//!
//! Nothing here trusts the file until `finish` has read it back from flash and checked the signature: the chip and the version
//! come from the info block inside the signed image, the length from the file, and an image that fails any check is refused
//! and never marked for the swap.

use core::cell::RefCell;

use defmt::{info, warn};
use embassy_boot_rp::{AlignedBuffer, BlockingFirmwareState, FirmwareUpdaterConfig, State};
use embassy_rp::flash::ERASE_SIZE;
use embassy_sync::blocking_mutex::{Mutex, raw::NoopRawMutex};
use picoclock_core::image::{self, Chip, Install, Refusal};
use picoclock_core::update::Version;
use sha2::{Digest, Sha512};

use crate::storage::Store;

/// Official builds demand a valid signature and a newer version; a build with the `custom-images` feature demands neither.
#[cfg(not(feature = "custom-images"))]
const POLICY: image::Policy = image::Policy::SIGNED;
#[cfg(feature = "custom-images")]
const POLICY: image::Policy = image::Policy::CUSTOM;

#[cfg(feature = "rp2040")]
const CHIP: Chip = Chip::Rp2040;
#[cfg(feature = "rp235x")]
const CHIP: Chip = Chip::Rp2350;

unsafe extern "C" {
    static __bootloader_dfu_start: u32;
    static __bootloader_active_start: u32;
    static __bootloader_active_end: u32;
}

/// Flash offset of the download slot and the length of the running image's slot (an image must fit it).
fn dfu_start() -> u32 {
    unsafe { &__bootloader_dfu_start as *const u32 as u32 }
}
fn active_len() -> usize {
    unsafe { (&__bootloader_active_end as *const u32 as u32 - &__bootloader_active_start as *const u32 as u32) as usize }
}

/// An update file being written into the download slot.
pub struct Stager {
    file_len: usize,
    image_len: usize,
    written: usize,
}

pub enum Failure {
    Refused(Refusal),
    Flash,
}

impl From<embassy_rp::flash::Error> for Failure {
    fn from(_: embassy_rp::flash::Error) -> Self {
        Failure::Flash
    }
}

/// Checks the announced size and prepares to receive.
pub fn begin(file_len: usize) -> Result<Stager, Refusal> {
    let image_len = image::image_len(file_len, active_len())?;
    Ok(Stager { file_len, image_len, written: 0 })
}

impl Stager {
    /// Writes the next block (4 KiB, the last one shorter) into the download slot.
    pub fn block(&mut self, flash: &mut Store, data: &[u8]) -> Result<(), Failure> {
        if data.is_empty() || data.len() > ERASE_SIZE || self.written + data.len() > self.file_len || (self.written % ERASE_SIZE != 0) {
            return Err(Failure::Flash);
        }
        let mut page = [0xFFu8; ERASE_SIZE];
        page[..data.len()].copy_from_slice(data);
        let at = dfu_start() + self.written as u32;
        flash.blocking_erase(at, at + ERASE_SIZE as u32)?;
        flash.blocking_write(at, &page)?;
        self.written += data.len();
        Ok(())
    }

    /// Reads the image back from flash, hashes it and applies the rules; returns the version and the number of the key that
    /// signed it. The caller keeps the result until the user says `update apply`.
    pub async fn finish(&self, flash: &mut Store) -> Result<(Version, u8), Failure> {
        if self.written != self.file_len {
            return Err(Failure::Refused(Refusal::BadLength));
        }
        let start = dfu_start();
        let mut hasher = Sha512::new();
        let mut buf = [0u8; ERASE_SIZE];
        let mut head = [0u8; image::SCAN];
        let mut at = 0usize;
        while at < self.image_len {
            let n = (self.image_len - at).min(ERASE_SIZE);
            flash.blocking_read(start + at as u32, &mut buf[..n])?;
            if at == 0 {
                head.copy_from_slice(&buf[..image::SCAN]);
            }
            hasher.update(&buf[..n]);
            at += n;
            embassy_futures::yield_now().await; // let the clock task and the watchdog feeder run during the 1 to 3 s of hashing
        }
        let digest: [u8; 64] = hasher.finalize().into();
        let mut sig = [0u8; image::SIGNATURE_LEN];
        flash.blocking_read(start + self.image_len as u32, &mut sig)?;

        // cheap rules first, the signature last (it takes the longest)
        let found = image::find_info(&head).ok_or(Failure::Refused(Refusal::NoInfo))?;
        image::check_info(&found, CHIP, image::RUNNING, POLICY).map_err(Failure::Refused)?;
        if !POLICY.require_signature {
            warn!("install: custom build, the signature is not checked");
            return Ok((found.version, image::UNSIGNED));
        }
        let key = image::verify(&image::KEYS, &digest, &sig).ok_or(Failure::Refused(Refusal::BadSignature))?;
        Ok((found.version, key as u8))
    }
}

/// Marks the staged image for the bootloader's swap at the next start. Refused while the running image has not confirmed itself
/// yet (the state would still say "swapped, not confirmed", and a second mark would lose the way back).
pub fn mark_swap(flash: &mut Store) -> Result<(), &'static str> {
    let shared = Mutex::<NoopRawMutex, _>::new(RefCell::new(&mut *flash));
    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(&shared, &shared);
    let mut magic = AlignedBuffer([0; 1]);
    let mut state = BlockingFirmwareState::from_config(config, &mut magic.0);
    match state.get_state() {
        Ok(State::Swap) => return Err("the running image has not confirmed itself yet (wait until 20 s after the start)"),
        Ok(_) => {}
        Err(_) => return Err("the bootloader state could not be read"),
    }
    state.mark_updated().map_err(|_| "the swap mark could not be written")
}

pub fn log_result(r: &Result<(Version, u8), Failure>) {
    match r {
        Ok((v, image::UNSIGNED)) => info!("install: {}.{}.{} staged, unsigned (custom build)", v.major, v.minor, v.patch),
        Ok((v, key)) => info!("install: {}.{}.{} staged, signed by key {}", v.major, v.minor, v.patch, key + 1),
        Err(Failure::Refused(why)) => warn!("install: refused: {}", why.message()),
        Err(Failure::Flash) => warn!("install: flash error"),
    }
}

pub fn install_of(r: &Result<(Version, u8), Failure>) -> Install {
    match r {
        Ok((v, key)) => Install::Staged(*v, *key),
        Err(Failure::Refused(why)) => Install::Refused(*why),
        Err(Failure::Flash) => Install::WriteFailed,
    }
}

/// Collects a file arriving in pieces of any size into 4 KiB blocks for the storage task, which writes each into the download slot
/// while the next one arrives. `remaining` counts the bytes of the file still to come; surplus bytes are ignored.
pub struct BlockSink {
    block: heapless::Vec<u8, 4096>,
    pub remaining: usize,
}

impl BlockSink {
    pub fn new(len: usize) -> Self {
        Self { block: heapless::Vec::new(), remaining: len }
    }

    pub async fn push(&mut self, mut data: &[u8]) {
        data = &data[..data.len().min(self.remaining)];
        while !data.is_empty() {
            let n = (self.block.capacity() - self.block.len()).min(data.len());
            let _ = self.block.extend_from_slice(&data[..n]);
            data = &data[n..];
            self.remaining -= n;
            if self.block.is_full() || self.remaining == 0 {
                crate::state::INSTALL_IN.send(crate::state::InstallMsg::Block(core::mem::take(&mut self.block))).await;
            }
        }
    }
}
