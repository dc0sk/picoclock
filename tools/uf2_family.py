#!/usr/bin/env python3
"""UF2 family IDs: `elf2uf2-rs` stamps every UF2 with the RP2040 family ID, which the RP2350 boot ROM silently ignores
(the board stays in BOOTSEL and nothing says why). This sets or checks the family so the same file flows to either chip.

  tools/uf2_family.py set   IN.uf2 OUT.uf2 rp2350-arm-s      rewrite every block's family ID
  tools/uf2_family.py check FILE.uf2 rp2040|rp2350-arm-s     exit 1 unless every block carries that family

Family IDs (https://github.com/raspberrypi/picotool, uf2families.json): rp2040 0xe48bff56, rp2350-arm-s 0xe48bff59.
"""
import struct, sys

FAMILIES = {"rp2040": 0xE48BFF56, "rp2350-arm-s": 0xE48BFF59}
MAGIC = (0x0A324655, 0x9E5D5157)
FAMILY_FLAG = 0x2000


def blocks(data):
    if len(data) == 0 or len(data) % 512:
        raise SystemExit("not a UF2 file: size is not a multiple of 512")
    for i in range(len(data) // 512):
        b = data[i * 512:(i + 1) * 512]
        m0, m1, flags, _addr, _size, _num, total, fam = struct.unpack_from("<8I", b)
        if (m0, m1) != MAGIC or total != len(data) // 512:
            raise SystemExit(f"block {i}: not a UF2 block of this file")
        if not flags & FAMILY_FLAG:
            raise SystemExit(f"block {i}: no family ID flag")
        yield i, fam


def main(argv):
    if len(argv) >= 4 and argv[1] == "set" and len(argv) == 5 and argv[4] in FAMILIES:
        data = bytearray(open(argv[2], "rb").read())
        for i, _ in list(blocks(data)):
            struct.pack_into("<I", data, i * 512 + 28, FAMILIES[argv[4]])
        open(argv[3], "wb").write(data)
        print(f"{len(data) // 512} blocks set to {argv[4]} (0x{FAMILIES[argv[4]]:08x})")
        return 0
    if len(argv) == 4 and argv[1] == "check" and argv[3] in FAMILIES:
        data = open(argv[2], "rb").read()
        bad = [i for i, fam in blocks(data) if fam != FAMILIES[argv[3]]]
        if bad:
            print(f"{argv[2]}: {len(bad)} of {len(data) // 512} blocks do not carry {argv[3]} (0x{FAMILIES[argv[3]]:08x}); first: block {bad[0]}")
            return 1
        print(f"{argv[2]}: all {len(data) // 512} blocks carry {argv[3]}")
        return 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
