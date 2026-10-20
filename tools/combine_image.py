#!/usr/bin/env python3
"""The one-time image for the BOOTSEL drive (or a probe): bootloader + erased state sector + application in one UF2.

  combine_image.py BOOTLOADER.elf APP.elf OUT.uf2 rp2040|rp2350-arm-s      (also writes OUT.bin: the same flat image)
  combine_image.py check-layout            the four memory maps (bootloader and firmware, both chips) describe the same flash

The state sector between the two (where the bootloader keeps its swap progress) is written as 0xFF: an installation over an
older picoclock leaves that sector full of the old program, and a byte of it that happens to look like the bootloader's swap
mark would swap garbage into place. The layout numbers are the ones in bootloader/memory-*.x and firmware/memory-*.x; the
tool refuses an ELF that does not fit them.
"""
import struct, sys

import sign_image
from uf2_family import FAMILIES

FLASH = 0x10000000
LAYOUT = {  # chip: (state offset, active offset, active length)
    "rp2040": (0x10000, 0x11000, 976 * 1024),
    "rp2350-arm-s": (0x10000, 0x11000, 1984 * 1024),
}


def base_and_image(path):
    elf = open(path, "rb").read()
    phoff, = struct.unpack_from("<I", elf, 0x1C)
    phentsize, phnum = struct.unpack_from("<HH", elf, 0x2A)
    loads = [struct.unpack_from("<8I", elf, phoff + i * phentsize) for i in range(phnum)]
    base = min(p[3] for p in loads if p[0] == 1 and p[4])
    return base - FLASH, sign_image.elf_to_bin(elf)


def combine(boot_elf, app_elf, chip):
    state, active, active_len = LAYOUT[chip]
    b_off, boot = base_and_image(boot_elf)
    a_off, app = base_and_image(app_elf)
    if b_off != 0 or len(boot) > state:
        raise SystemExit(f"the bootloader must start at flash offset 0 and end before {state:#x} (starts {b_off:#x}, {len(boot)} bytes)")
    if a_off != active or len(app) > active_len:
        raise SystemExit(f"the application must start at {active:#x} and fit {active_len} bytes (starts {a_off:#x}, {len(app)} bytes)")
    img = bytearray(b"\xff" * (active + len(app)))
    img[:len(boot)] = boot
    img[active:] = app
    return bytes(img)


def uf2(img, family):
    img = img + b"\xff" * (-len(img) % 256)
    n = len(img) // 256
    out = bytearray()
    for i in range(n):
        out += struct.pack("<8I", 0x0A324655, 0x9E5D5157, 0x2000, FLASH + i * 256, 256, i, n, FAMILIES[family])
        out += img[i * 256:(i + 1) * 256] + b"\x00" * (476 - 256) + struct.pack("<I", 0x0AB16F30)
    return bytes(out)


# chip: (memory file suffix, flash size, bytes at the end of flash kept for the records of firmware/src/storage.rs: the settings,
# the NTS roots and the HTTPS key (Pico 2 W) with a second copy of each, and the trust record with its second copy)
CHIPS = {
    "rp2040": ("rp2040", 2048 * 1024, 24 * 1024),
    "rp2350-arm-s": ("rp235x", 4096 * 1024, 32 * 1024),
}


def regions(text):
    """name -> (origin, length) of the MEMORY block, numbers with K suffixes and simple `- 0x100` terms evaluated."""
    import re
    block = re.search(r"MEMORY\s*\{(.*?)\n\s*\}", re.sub(r"/\*.*?\*/", "", text, flags=re.S), re.S).group(1)
    out = {}
    for name, o, l in re.findall(r"(\w+)\s*:\s*ORIGIN\s*=\s*([^,]+),\s*LENGTH\s*=\s*([^\n]+)", block):
        def num(s):
            s = s.strip().rstrip(",")
            return eval(re.sub(r"(\d+)K", r"(\1*1024)", s), {}, {})
        out[name] = (num(o), num(l))
    return out


def check_layout():
    import os, re
    here = os.path.dirname(os.path.abspath(__file__))
    bad = []
    for chip, (suffix, size, settings) in CHIPS.items():
        boot = regions(open(f"{here}/../bootloader/memory-{suffix}.x").read())
        app_text = open(f"{here}/../firmware/memory-{suffix}.x").read()
        app = regions(app_text)
        b = lambda n: (boot[n][0] - FLASH, boot[n][1])
        state, active, dfu = b("BOOTLOADER_STATE"), b("ACTIVE"), b("DFU")
        flash = b("FLASH")
        if (b("BOOT2")[0] if "BOOT2" in boot else flash[0]) != 0:
            bad.append(f"{chip}: bootloader does not start at flash offset 0")
        boot_end = flash[0] + flash[1]
        if boot_end > state[0]:
            bad.append(f"{chip}: bootloader area ends at {boot_end:#x}, after the state sector at {state[0]:#x}")
        if state[0] + state[1] != active[0]:
            bad.append(f"{chip}: the state sector is not directly below the active slot")
        if (app["FLASH"][0] - FLASH, app["FLASH"][1]) != active:
            bad.append(f"{chip}: firmware FLASH {app['FLASH']} is not the bootloader's ACTIVE {active}")
        if dfu[0] != active[0] + active[1] or dfu[1] < active[1] + 4096:
            bad.append(f"{chip}: the download slot must follow the active slot and be at least one page longer")
        if (active[0] + active[1] + 4096 > dfu[0]) and dfu[0] != active[0] + active[1]:
            bad.append(f"{chip}: slots overlap")
        if dfu[0] + dfu[1] + settings > size:
            bad.append(f"{chip}: the download slot runs into the settings sectors at the end of flash")
        syms = {m[0]: eval(m[1], {}, {}) for m in re.findall(r"(__bootloader_(?:state|dfu)_(?:start|end))\s*=\s*([^;]+);", app_text)}
        want = {"__bootloader_state_start": state[0], "__bootloader_state_end": state[0] + state[1],
                "__bootloader_dfu_start": dfu[0], "__bootloader_dfu_end": dfu[0] + dfu[1]}
        if syms != want:
            bad.append(f"{chip}: firmware linker symbols {syms} differ from the bootloader's map {want}")
        if LAYOUT[chip] != (state[0], active[0], active[1]):
            bad.append(f"{chip}: LAYOUT in this tool {LAYOUT[chip]} differs from the maps {(state[0], active[0], active[1])}")
        # the record sectors of storage.rs must not reach into the download slot: the lowest one counted from the end of flash
        storage = open(f"{here}/../firmware/src/storage.rs").read()
        https = chip == "rp2350-arm-s"
        lowest, cfg = 1, None
        for line in storage.splitlines():
            if line.strip().startswith("#[cfg("):
                cfg = line.strip()
            m = re.match(r"const \w+_OFFSET: u32 = \(FLASH_SIZE - (?:(\d+) \* )?ERASE_SIZE\) as u32;", line)
            if m:
                applies = True
                if cfg and 'feature = "https"' in cfg:
                    applies = https if "not(" not in cfg else not https
                if applies:
                    lowest = max(lowest, int(m.group(1) or 1))
            if not line.strip().startswith("#["):
                cfg = None
        records = lowest * 4096
        if records != settings:
            bad.append(f"{chip}: storage.rs uses {lowest} sectors at the end of flash, this tool expects {settings // 4096}")
        if size - records < dfu[0] + dfu[1]:
            bad.append(f"{chip}: the record sectors of storage.rs ({lowest}) reach into the download slot")
        print(f"{chip}: state {state[0]:#x}, active {active[0]:#x}+{active[1] // 1024}K, download {dfu[0]:#x}+{dfu[1] // 1024}K, "
              f"{lowest} record sectors, {(size - records - dfu[0] - dfu[1]) // 1024}K free between")
    if bad:
        raise SystemExit("layout problems:\n  " + "\n  ".join(bad))
    print("layout: ok")


if __name__ == "__main__":
    if sys.argv[1:] == ["check-layout"]:
        sys.exit(check_layout() or 0)
    if len(sys.argv) != 5 or sys.argv[4] not in LAYOUT:
        raise SystemExit(__doc__)
    img = combine(sys.argv[1], sys.argv[2], sys.argv[4])
    open(sys.argv[3], "wb").write(uf2(img, sys.argv[4]))
    open(sys.argv[3].removesuffix(".uf2") + ".bin", "wb").write(img)
    print(f"{sys.argv[3]}: {len(img)} bytes of flash, {len(img) // 256} blocks, family {sys.argv[4]}")
