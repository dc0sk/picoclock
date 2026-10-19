#!/usr/bin/env bash
# Builds the release files for both boards into dist/ (ADR-0006).
#   tools/release.sh [KEYDIR]        KEYDIR holds secret-key.asc, passphrase.txt, fingerprint.txt (default local/firmware-signing)
# Per board: picoclock-<version>-<board>.uf2  first installation from the BOOTSEL drive (bootloader + application)
#            picoclock-<version>-<board>.pcu  the update file (application + Ed25519 signature) for upload or download
# and SHA256SUMS. The signature is made in a temporary GnuPG home (the owner's keyring is never touched), checked against the
# public key the firmware carries, and the finished .pcu is verified once more. Refuses to run on a modified tree: a release is
# something one can rebuild from its commit.
set -euo pipefail
cd "$(dirname "$0")/.."
KEY="${1:-local/firmware-signing}"
PUBKEY="${PICOCLOCK_PUBKEY:-347fe5f208d22b3d4fb3e040ef9760dda05d1fc3f5cfaa8b3a2ce56f77aca933}"
[ -z "$(git status --porcelain)" ] || { echo "the working tree is not clean: commit first"; exit 1; }
for f in secret-key.asc passphrase.txt fingerprint.txt; do [ -f "$KEY/$f" ] || { echo "missing $KEY/$f"; exit 1; }; done
VER="$(sed -n '0,/^version = "\(.*\)"/s//\1/p' firmware/Cargo.toml | head -1)"
[ -n "$VER" ] || { echo "no version found"; exit 1; }
echo "== release $VER from $(git rev-parse --short HEAD)"

WORK="$(mktemp -d)"
export GNUPGHOME="$WORK/gnupg"; mkdir -m 700 "$GNUPGHOME"
trap 'gpgconf --kill gpg-agent 2>/dev/null || true; rm -rf "$WORK"' EXIT
printf 'allow-preset-passphrase\nallow-loopback-pinentry\n' > "$GNUPGHOME/gpg-agent.conf"
gpg -q --batch --pinentry-mode loopback --passphrase-file "$KEY/passphrase.txt" --import "$KEY/secret-key.asc" >/dev/null 2>&1
GRIP="$(grep -m1 '^keygrip' "$KEY/fingerprint.txt" | awk '{print $2}')"
HEXPASS="$(python3 -c 'import sys;print(open(sys.argv[1],"rb").read().rstrip(b"\n").hex().upper())' "$KEY/passphrase.txt")"
gpg-connect-agent "PRESET_PASSPHRASE $GRIP -1 $HEXPASS" /bye >/dev/null 2>&1

rm -rf dist; mkdir dist
build() {  # board chip target features
  local board="$1" chip="$2" target="$3"; shift 3
  local rel="target/$target/release" name="picoclock-$VER-$board"
  echo "== $board"
  cargo build --release -p picoclock --target "$target" "$@"
  cargo build --release -p picoclock-bootloader --target "$target" "$@"
  python3 tools/combine_image.py "$rel/picoclock-bootloader" "$rel/picoclock" "dist/$name.uf2" "$chip"
  rm -f "dist/$name.bin"
  python3 tools/sign_image.py bin "$rel/picoclock" "$WORK/$board.bin"
  python3 tools/sign_image.py sign "$WORK/$board.bin" "$WORK/$board.sig" "$GRIP" --gnupghome "$GNUPGHOME" --pubkey "$PUBKEY"
  python3 tools/sign_image.py pack "$WORK/$board.bin" "$WORK/$board.sig" "dist/$name.pcu"
  python3 tools/sign_image.py verify "$WORK/$board.bin" "$WORK/$board.sig" "$PUBKEY"
}
build pico-w rp2040 thumbv6m-none-eabi
build pico2-w rp2350-arm-s thumbv8m.main-none-eabihf --no-default-features --features rp235x
rm -f dist/*.bin
( cd dist && sha256sum ./*.uf2 ./*.pcu | sed 's| \./| |' > SHA256SUMS )
echo "== dist/"; ls -l dist; cat dist/SHA256SUMS
git status --porcelain | grep . && { echo "the build changed tracked files"; exit 1; } || true
