/* Pico W, 2 MiB flash, with the bootloader (bootloader/memory-rp2040.x has the whole map): this image runs from the ACTIVE
   slot, the bootloader holds boot2, so there is no BOOT2 region here. The last three 4 KiB sectors of the chip are kept free
   for the settings (last), the NTS roots and the settings backup (see storage.rs); the 12 KiB before them are the trust sector. */
MEMORY {
    FLASH : ORIGIN = 0x10011000, LENGTH = 976K
    RAM   : ORIGIN = 0x20000000, LENGTH = 264K
}

/* Offsets in flash, read by `FirmwareUpdaterConfig::from_linkerfile_blocking` (the state sector is where the bootloader keeps it) */
__bootloader_state_start = 0x10010000 - 0x10000000;
__bootloader_state_end = 0x10011000 - 0x10000000;
__bootloader_dfu_start = 0x10105000 - 0x10000000;
__bootloader_dfu_end = 0x101FA000 - 0x10000000;
