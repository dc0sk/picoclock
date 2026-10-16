#!/usr/bin/env python3
"""Push the settings in local/wifi.conf to the clock over its USB console, save them and show the status.

usage: tools/provision.py [--port /dev/ttyACM2] [--file local/wifi.conf]
Up to 8 networks: wifi1_ssid/wifi1_password ... wifi8_*; they are tried in slot order
(wifi_ssid/wifi_password mean slot 1). Needs pyserial. Lines starting with '#' and lines with an empty value are skipped.
"""
import argparse, glob, sys, time
import serial

def find_port():
    for d in glob.glob("/sys/class/tty/ttyACM*"):
        try:
            if "picoclock" in open(d + "/device/../product").read():
                return "/dev/" + d.rsplit("/", 1)[1]
        except OSError:
            pass
    sys.exit("clock console not found; pass --port")

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port")
    ap.add_argument("--file", default="local/wifi.conf")
    a = ap.parse_args()
    lines = []
    for raw in open(a.file):
        raw = raw.rstrip("\n")
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        key, _, val = raw.partition(" ")
        if val.strip():
            lines.append((key, val.strip()))
    if not any(k in ("wifi_ssid", "wifi1_ssid") or (k.startswith("wifi") and k.endswith("_ssid")) for k, _ in lines):
        sys.exit(f"{a.file}: no wifi SSID set")
    s = serial.Serial(a.port or find_port(), 115200, timeout=1)
    time.sleep(1)
    s.write(b"\r\n"); time.sleep(0.5)

    def cmd(c, wait=0.6):
        s.reset_input_buffer()
        s.write((c + "\r\n").encode())
        time.sleep(wait)
        return s.read(4000).decode(errors="replace")

    for k, v in lines:
        out = cmd(f"set {k} {v}")
        print(f"set {k}:", "ok" if "ok" in out else out.strip())
    print(cmd("save", 2).strip())
    time.sleep(15)
    print(cmd("status", 1))

main()
