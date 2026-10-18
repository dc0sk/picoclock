MEMORY {
    /*
     * The Pico 2 W has 4 MiB of flash and the bootloader (bootloader/memory-rp235x.x has the whole map): this image runs from
     * the ACTIVE slot. The last four 4 KiB sectors are kept free (see storage.rs): the last one holds the saved
     * configuration, the one before it the HTTPS key and certificate chain, the one before that the NTS roots the user
     * added, and the one before that the backup copy of the configuration. The 40 KiB before them are the trust sector.
     */
    FLASH : ORIGIN = 0x10011000, LENGTH = 1984K
    /*
     * RAM consists of 8 banks, SRAM0-SRAM7, with a striped mapping.
     * This is usually good for performance, as it distributes load on
     * those banks evenly.
     */
    RAM : ORIGIN = 0x20000000, LENGTH = 512K
    /*
     * RAM banks 8 and 9 use a direct mapping. They can be used to have
     * memory areas dedicated for some specific job, improving predictability
     * of access times.
     * Example: Separate stacks for core0 and core1.
     */
    SRAM8 : ORIGIN = 0x20080000, LENGTH = 4K
    SRAM9 : ORIGIN = 0x20081000, LENGTH = 4K
}

/* Offsets in flash, read by `FirmwareUpdaterConfig::from_linkerfile_blocking` (the state sector is where the bootloader keeps it) */
__bootloader_state_start = 0x10010000 - 0x10000000;
__bootloader_state_end = 0x10011000 - 0x10000000;
__bootloader_dfu_start = 0x10201000 - 0x10000000;
__bootloader_dfu_end = 0x103F2000 - 0x10000000;

SECTIONS {
    /* ### Boot ROM info
     *
     * Goes after .vector_table, to keep it in the first 4K of flash
     * where the Boot ROM (and picotool) can find it
     */
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH

} INSERT AFTER .vector_table;

/* move .text to start /after/ the boot info */
_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS {
    /* ### Picotool 'Binary Info' Entries
     *
     * Picotool looks through this block (as we have pointers to it in our
     * header) to find interesting information.
     */
    .bi_entries : ALIGN(4)
    {
        /* We put this in the header */
        __bi_entries_start = .;
        /* Here are the entries */
        KEEP(*(.bi_entries));
        /* Keep this block a nice round size */
        . = ALIGN(4);
        /* We put this in the header */
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;

SECTIONS {
    /* ### Boot ROM extra info
     *
     * Goes after everything in our program, so it can contain a signature.
     */
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH

} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);
