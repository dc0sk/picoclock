/* Pico W, 2 MiB flash. Offsets from the start of flash; the same numbers are in firmware/memory-rp2040.x.
     0x000000  boot2 (256 B) + bootloader                 64 KiB
     0x010000  bootloader state                            4 KiB
     0x011000  ACTIVE: the running image                 976 KiB
     0x105000  DFU: the downloaded image (+ 4 KiB)       980 KiB
     0x1FA000  trust sector (ADR-0006, point 10)          12 KiB
     0x1FD000  settings backup, NTS roots, settings       12 KiB (storage.rs)                                            */
MEMORY {
    BOOT2 : ORIGIN = 0x10000000, LENGTH = 0x100
    FLASH : ORIGIN = 0x10000100, LENGTH = 64K - 0x100
    BOOTLOADER_STATE : ORIGIN = 0x10010000, LENGTH = 4K
    ACTIVE : ORIGIN = 0x10011000, LENGTH = 976K
    DFU : ORIGIN = 0x10105000, LENGTH = 980K
    RAM : ORIGIN = 0x20000000, LENGTH = 264K
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(BOOT2);
__bootloader_active_end = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(BOOT2);
__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOT2);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOT2);
