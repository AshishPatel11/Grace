"""Makes assets/grace.ico: the app icon put inside grace.exe (see build.rs).

Same picture as `make_icon` in src/win.rs: an orange rounded square with a white ">_".
Each size is drawn on its own (not shrunk from a big one), so small sizes stay sharp.

Run:  python assets/make_icon.py   (needs Pillow: pip install pillow)
"""

import math
from pathlib import Path

from PIL import Image

SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
ORANGE = (0xD9, 0x77, 0x57)


def seg(x, y, ax, ay, bx, by):
    """Distance from (x, y) to the line segment a-b."""
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(x - ax - t * dx, y - ay - t * dy)


def draw(n):
    s = float(n)
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    px = img.load()
    for yi in range(n):
        for xi in range(n):
            x, y = xi + 0.5, yi + 0.5
            r = s * 0.22
            cx, cy = min(max(x, r), s - r), min(max(y, r), s - r)
            d = math.hypot(x - cx, y - cy) - r
            a_bg = max(0.0, min(1.0, 0.5 - d))
            if a_bg <= 0:
                continue
            w = s * 0.085
            d1 = min(seg(x, y, s * 0.27, s * 0.30, s * 0.47, s * 0.50), seg(x, y, s * 0.47, s * 0.50, s * 0.27, s * 0.70))
            d2 = seg(x, y, s * 0.53, s * 0.70, s * 0.76, s * 0.70)
            a_fg = max(0.0, min(1.0, w - min(d1, d2) + 0.5))
            rgb = tuple(round(c + (255 - c) * a_fg) for c in ORANGE)
            px[xi, yi] = rgb + (round(a_bg * 255),)
    return img


def main():
    out = Path(__file__).with_name("grace.ico")
    imgs = [draw(n) for n in SIZES]
    big = imgs[-1]
    big.save(out, format="ICO", sizes=[(n, n) for n in SIZES], append_images=imgs[:-1])
    print(f"wrote {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
