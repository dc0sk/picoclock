#!/usr/bin/env python3
"""Sends a signed update file to the clock over the USB console (ADR-0006).

  upload_image.py FILE.pcu [--apply] [--port /dev/ttyACM0]

The clock checks length, chip, version (must be newer) and the Ed25519 signature before it stages anything; nothing is
installed until `update apply` is confirmed. With --apply this tool types the confirmation too (INSTALL), which restarts the
clock into the new image; the bootloader swaps back if the new image does not confirm itself.
"""
import glob, sys, time

import serial


def main(argv):
    apply = "--apply" in argv
    port = argv[argv.index("--port") + 1] if "--port" in argv else None
    files = [a for i, a in enumerate(argv) if not a.startswith("--") and (i == 0 or argv[i - 1] != "--port")]
    if len(files) != 1:
        raise SystemExit(__doc__)
    data = open(files[0], "rb").read()
    port = port or (glob.glob("/dev/serial/by-id/*picoclock*") or [None])[0]
    if not port:
        raise SystemExit("no picoclock console found")
    s = serial.Serial(port, 115200, timeout=2)
    time.sleep(0.5)
    s.write(b"\r\n")
    time.sleep(0.3)
    s.reset_input_buffer()

    def read_until(marker, secs):
        end, buf = time.time() + secs, b""
        while time.time() < end and marker.encode() not in buf:
            buf += s.read(256)
        return buf.decode(errors="replace")

    s.write(f"update upload {len(data)}\r\n".encode())
    out = read_until("ready: send the file now", 5)
    if "ready: send the file now" not in out:
        print(out.strip())
        raise SystemExit("the clock did not accept the upload")
    t0 = time.time()
    for i in range(0, len(data), 4096):
        s.write(data[i:i + 4096])
        s.flush()
    print(f"sent {len(data)} bytes in {time.time() - t0:.1f} s; the clock now checks it")
    out = read_until("picoclock> ", 60)
    print(out.strip())
    if "is staged" not in out:
        raise SystemExit(1)
    if apply:
        s.write(b"update apply\r\n")
        print(read_until("anything else cancels", 5).strip())
        s.write(b"INSTALL\r\n")
        print(read_until("restarts", 5).strip())


if __name__ == "__main__":
    main(sys.argv[1:])
