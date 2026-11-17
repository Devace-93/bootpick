#!/usr/bin/env python3
"""Draws the bootpick logo (the shapes of site/icon.svg) and writes
assets/bootpick.ico, the icon of the Windows program.

    uv run --with pillow tools/make-ico.py
"""
import os
from PIL import Image, ImageDraw

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
S = 8  # drawn at 8x 128 and scaled down


def draw():
    n = 128 * S
    # Background: vertical gradient inside a rounded square.
    grad = Image.new("RGBA", (n, n))
    top, bottom = (0x1E, 0x29, 0x3B), (0x0B, 0x12, 0x20)
    px = grad.load()
    for y in range(n):
        t = min(max((y / S - 6) / 116, 0), 1)
        c = tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)) + (255,)
        for x in range(n):
            px[x, y] = c
    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle((6 * S, 6 * S, 122 * S, 122 * S), 26 * S, fill=255)
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    img.paste(grad, (0, 0), mask)
    # Tiles: the first one lit, the others faint.
    tiles = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(tiles)
    for x, y, fill in [(24, 30, (0x38, 0xBD, 0xF8, 255)), (70, 30, (0xF1, 0xF5, 0xF9, 64)), (24, 72, (0xF1, 0xF5, 0xF9, 64)), (70, 72, (0xF1, 0xF5, 0xF9, 64))]:
        d.rounded_rectangle((x * S, y * S, (x + 34) * S, (y + 34) * S), 8 * S, fill=fill)
    img = Image.alpha_composite(img, tiles)
    # The pointer.
    arrow = [(84, 78), (84, 112), (92, 104), (98, 116), (104, 113), (98, 101), (109, 101)]
    pts = [(x * S, y * S) for x, y in arrow]
    d = ImageDraw.Draw(img)
    d.polygon(pts, fill=(255, 255, 255, 255))
    d.line(pts + pts[:2], fill=(0x0B, 0x12, 0x20, 255), width=3 * S, joint="curve")
    d.polygon([(x * S + (S if x < 100 else -S), y * S) for x, y in arrow], fill=(255, 255, 255, 255))
    return img


if __name__ == "__main__":
    out = os.path.join(ROOT, "assets", "bootpick.ico")
    draw().save(out, sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)], bitmap_format="bmp")
    print(out)
