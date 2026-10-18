//! picoclock bootloader (ADR-0006): runs first, swaps a downloaded image into the active slot when the application marked one,
//! swaps the old image back if the new one did not confirm itself, and starts the active image. The watchdog runs while it
//! works on flash, so a hang resets the chip instead of leaving it stuck.
#![no_std]
#![no_main]

use core::cell::RefCell;

use cortex_m_rt::{entry, exception};
use embassy_boot_rp::*;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Duration;

#[cfg(feature = "rp2040")]
const FLASH_SIZE: usize = 2 * 1024 * 1024;
#[cfg(feature = "rp235x")]
const FLASH_SIZE: usize = 4 * 1024 * 1024;

#[entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());
    let flash = WatchdogFlash::<FLASH_SIZE>::start(p.FLASH, p.WATCHDOG, Duration::from_secs(8));
    let flash = Mutex::new(RefCell::new(flash));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active_offset = config.active.offset();
    let bl: BootLoader = BootLoader::prepare(config);

    unsafe { bl.load(embassy_rp::flash::FLASH_BASE as u32 + active_offset) }
}

#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".HardFault.user"))]
unsafe extern "C" fn HardFault() {
    cortex_m::peripheral::SCB::sys_reset();
}

#[exception]
unsafe fn DefaultHandler(_: i16) -> ! {
    cortex_m::peripheral::SCB::sys_reset();
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    cortex_m::asm::udf();
}
