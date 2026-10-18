#!/bin/bash
# Regenerates the web interface screenshots in docs/images/ from made-up data (core/examples/web_screens.rs).
# Needs chromium and python3 with Pillow. Nothing comes from a real clock.
set -euo pipefail
cd "$(dirname "$0")/.."
HOST="$(rustc -vV | sed -n 's/^host: //p')"
OUT="$(mktemp -d)"
cargo run -q -p picoclock-core --target "$HOST" --example web_screens -- "$OUT"
mkdir -p docs/images
for tab in status wifi time clock web security; do
  # chromium aborts if TMPDIR is a long path (its profile socket path gets too long), hence TMPDIR=/tmp
  for try in 1 2 3; do
    (cd "$OUT" && env TMPDIR=/tmp chromium --headless --no-sandbox --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
      --window-size=620,2600 --screenshot="$OUT/$tab.png" "file://$OUT/$tab.html" >/dev/null 2>&1) && break
    sleep 2
  done
  python3 - "$OUT/$tab.png" "docs/images/web-$tab.png" <<'PY'
import sys
from PIL import Image, ImageChops
im = Image.open(sys.argv[1]).convert("RGB")
bg = Image.new("RGB", im.size, im.getpixel((2, im.height - 2)))
box = ImageChops.difference(im, bg).getbbox()
bottom = min(im.height, (box[3] if box else im.height) + 16)
im.crop((0, 0, im.width, bottom)).save(sys.argv[2], optimize=True)
PY
done
rm -rf "$OUT"
ls -l docs/images
