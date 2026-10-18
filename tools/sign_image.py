#!/usr/bin/env python3
"""Release tooling for signed firmware images (ADR-0006, step 2, build order 1).

  sign_image.py bin  ELF OUT.bin             raw image of an ELF (program headers, no objcopy needed)
  sign_image.py sign IMG.bin OUT.sig KEYGRIP-OR-FPR [--gnupghome DIR] [--pubkey HEX]
                                             Ed25519 signature over the SHA-512 digest of IMG.bin, made by gpg-agent
                                             (PKSIGN); with --pubkey the signature is verified before it is written
  sign_image.py verify IMG.bin IMG.sig PUBKEY-HEX
                                             exit 0 only if the signature is valid for that raw public key
  sign_image.py self-test                    throwaway key in a temporary GnuPG home: sign, verify, tamper, refuse

The signature is the 64 bytes r||s of PureEdDSA over the 64-byte SHA-512 digest of the image, the form Embassy's bootloader
verifier takes. The signing key is never read by this script: gpg-agent holds it and asks for the passphrase itself.
"""
import hashlib, os, re, shutil, subprocess, sys, tempfile

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey


def digest(path):
    h = hashlib.sha512()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.digest()


def verify(img, sig, pub_hex):
    if len(sig) != 64:
        return False
    try:
        Ed25519PublicKey.from_public_bytes(bytes.fromhex(pub_hex)).verify(sig, digest(img))
        return True
    except (InvalidSignature, ValueError):
        return False


def unescape(b):
    return re.sub(rb"%([0-9A-Fa-f]{2})", lambda m: bytes([int(m.group(1), 16)]), b)


def parse_sigval(data):
    """data: the agent's `D ` lines joined and unescaped, an S-expression (sig-val(eddsa(r ..)(s ..)))."""
    m = re.search(rb"\(1:r32:(.{32})\)\(1:s32:(.{32})\)", data, re.S)
    if not m:
        raise SystemExit("the agent's answer is not an Ed25519 signature")
    return m.group(1) + m.group(2)


def agent_sign(img, grip, home):
    env = dict(os.environ)
    if home:
        env["GNUPGHOME"] = home
    cmds = [f"SIGKEY {grip}", "SETKEYDESC picoclock%20firmware%20signing", f"SETHASH --hash=sha512 {digest(img).hex()}", "PKSIGN", "BYE"]
    out = subprocess.run(["gpg-connect-agent"], input="\n".join(cmds).encode(), env=env, capture_output=True).stdout
    for line in out.split(b"\n"):
        if line.startswith(b"ERR"):
            raise SystemExit("gpg-agent refused: " + line.decode(errors="replace"))
    data = unescape(b"".join(l[2:] for l in out.split(b"\n") if l.startswith(b"D ")))
    return parse_sigval(data)


def keygrip(fpr_or_grip, home):
    if re.fullmatch(r"[0-9A-Fa-f]{40}", fpr_or_grip) is None:
        raise SystemExit("give a 40-digit fingerprint or keygrip")
    env = dict(os.environ)
    if home:
        env["GNUPGHOME"] = home
    out = subprocess.run(["gpg", "--batch", "--with-keygrip", "-k", "--with-colons", fpr_or_grip], env=env, capture_output=True, text=True).stdout
    grips = [l.split(":")[9] for l in out.splitlines() if l.startswith("grp:")]
    return grips[0] if grips else fpr_or_grip  # a keygrip is passed through


def raw_pubkey(fpr, home):
    """The raw 32-byte Ed25519 public key (hex) of a key in the keyring, for --pubkey."""
    env = dict(os.environ)
    if home:
        env["GNUPGHOME"] = home
    out = subprocess.run(["gpg", "--batch", "--export", fpr], env=env, capture_output=True).stdout
    m = re.search(rb"\x40(.{32})", out, re.S)  # MPI 0x40 prefix + 32 bytes in an EdDSA public key packet
    return None if not m else m.group(1).hex()


def elf_to_bin(elf):
    """The loadable segments of a 32-bit little-endian ELF as one flat image from the lowest load address (gaps 0xFF, like
    flash). Pure Python: no objcopy, whose installed copies differ between machines."""
    import struct
    if elf[:4] != b"\x7fELF" or elf[4] != 1 or elf[5] != 1:
        raise SystemExit("not a 32-bit little-endian ELF")
    phoff, = struct.unpack_from("<I", elf, 0x1C)
    phentsize, phnum = struct.unpack_from("<HH", elf, 0x2A)
    segs = []
    for i in range(phnum):
        ptype, off, _v, paddr, filesz, _m, _f, _a = struct.unpack_from("<8I", elf, phoff + i * phentsize)
        if ptype == 1 and filesz:
            segs.append((paddr, elf[off:off + filesz]))
    if not segs:
        raise SystemExit("no loadable segments")
    base = min(p for p, _ in segs)
    img = bytearray(b"\xff" * (max(p + len(d) for p, d in segs) - base))
    for p, d in segs:
        img[p - base:p - base + len(d)] = d
    return bytes(img)


def self_test():
    d = tempfile.mkdtemp(prefix="signtest")
    try:
        os.chmod(d, 0o700)
        env = dict(os.environ, GNUPGHOME=d)
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "test <t@example.invalid>", "ed25519", "sign", "1d"], env=env, capture_output=True, check=True)
        fpr = [l.split(":")[9] for l in subprocess.run(["gpg", "--batch", "-k", "--with-colons"], env=env, capture_output=True, text=True).stdout.splitlines() if l.startswith("fpr:")][0]
        img = os.path.join(d, "image.bin")
        with open(img, "wb") as f:
            f.write(os.urandom(100_000))
        grip = keygrip(fpr, d)
        sig = agent_sign(img, grip, d)
        pub = raw_pubkey(fpr, d)
        assert pub and len(sig) == 64, "no public key or short signature"
        assert verify(img, sig, pub), "a good signature was refused"
        with open(img, "r+b") as f:
            f.seek(5000)
            b = f.read(1)
            f.seek(5000)
            f.write(bytes([b[0] ^ 1]))
        assert not verify(img, sig, pub), "a tampered image was accepted"
        assert not verify(img, sig[:63], pub), "a short signature was accepted"
        assert not verify(img, bytes(64), pub), "a zero signature was accepted"
        print("self-test: ok (sign, verify, tampered image refused, short and zero signatures refused)")
    finally:
        subprocess.run(["gpgconf", "--homedir", d, "--kill", "gpg-agent"], capture_output=True)
        shutil.rmtree(d, ignore_errors=True)


def main(a):
    if a[:1] == ["self-test"]:
        return self_test()
    if a[:1] == ["bin"] and len(a) == 3:
        img = elf_to_bin(open(a[1], "rb").read())
        open(a[2], "wb").write(img)
        print(f"{a[2]}: {len(img)} bytes")
        return
    if a[:1] == ["verify"] and len(a) == 4:
        ok = verify(a[1], open(a[2], "rb").read(), a[3])
        print("signature ok" if ok else "SIGNATURE INVALID")
        sys.exit(0 if ok else 1)
    if a[:1] == ["sign"] and len(a) >= 4:
        home = a[a.index("--gnupghome") + 1] if "--gnupghome" in a else None
        pub = a[a.index("--pubkey") + 1] if "--pubkey" in a else None
        sig = agent_sign(a[1], keygrip(a[3], home), home)
        if pub and not verify(a[1], sig, pub):
            raise SystemExit("the signature does not verify against --pubkey: nothing written")
        open(a[2], "wb").write(sig)
        print(f"{a[2]}: 64 bytes" + (", verified" if pub else ", NOT verified (no --pubkey)"))
        return
    raise SystemExit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
