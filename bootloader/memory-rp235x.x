/* Pico 2 W, 4 MiB flash. Offsets from the start of flash; the same numbers are in firmware/memory-rp235x.x.
     0x000000  bootloader (IMAGE_DEF in the first 4 KiB)  64 KiB
     0x010000  bootloader state                            4 KiB
     0x011000  ACTIVE: the running image                1984 KiB
     0x201000  DFU: the downloaded image (+ 4 KiB)      1988 KiB
     0x3F2000  trust sector (ADR-0006, point 10)          40 KiB
     0x3FC000  settings backup, TLS, NTS roots, settings  16 KiB (storage.rs)                                            */
MEMORY {
    FLASH : ORIGIN = 0x10000000, LENGTH = 64K
    BOOTLOADER_STATE : ORIGIN = 0x10010000, LENGTH = 4K
    ACTIVE : ORIGIN = 0x10011000, LENGTH = 1984K
    DFU : ORIGIN = 0x10201000, LENGTH = 1988K
    RAM : ORIGIN = 0x20000000, LENGTH = 512K
}

SECTIONS {
    /* Boot ROM info: goes after .vector_table to stay in the first 4 KiB, where the boot ROM looks for the IMAGE_DEF */
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH
} INSERT AFTER .vector_table;

_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS {
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH
} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(FLASH);
__bootloader_active_end = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(FLASH);
__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(FLASH);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH);
