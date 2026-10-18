#!/bin/bash
# Regenerates the screenshots in docs/images/ (the six web pages and three terminal sessions) from made-up data
# (core/examples/demo, web_screens.rs, terminal_screens.rs). Needs chromium and python3 with Pillow. Nothing comes from a real clock.
set -euo pipefail
cd "$(dirname "$0")/.."
HOST="$(rustc -vV | sed -n 's/^host: //p')"
OUT="$(mktemp -d)"
cargo run -q -p picoclock-core --target "$HOST" --example web_screens -- "$OUT"
cargo run -q -p picoclock-core --target "$HOST" --example terminal_screens -- "$OUT"
mkdir -p docs/images

shoot() { # name width image
  # chromium aborts if TMPDIR is a long path (its profile socket path gets too long), hence TMPDIR=/tmp
  for try in 1 2 3; do
    (cd "$OUT" && env TMPDIR=/tmp chromium --headless --no-sandbox --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
      --window-size="$2,2600" --screenshot="$OUT/$1.png" "file://$OUT/$1.html" >/dev/null 2>&1) && break
    sleep 2
  done
  python3 - "$OUT/$1.png" "docs/images/$3.png" <<'PY'
import sys
from PIL import Image, ImageChops
im = Image.open(sys.argv[1]).convert("RGB")
bg = Image.new("RGB", im.size, im.getpixel((2, im.height - 2)))
box = ImageChops.difference(im, bg).getbbox()
bottom = min(im.height, (box[3] if box else im.height) + 16)
im.crop((0, 0, im.width, bottom)).save(sys.argv[2], optimize=True)
PY
}

for tab in status wifi time clock web security; do shoot "$tab" 620 "web-$tab"; done
for t in status config help; do shoot "terminal-$t" 900 "terminal-$t"; done
rm -rf "$OUT"
ls -l docs/images
