#!/usr/bin/env python3
"""Renders site/og-image.png: site/og-image.svg plus the menu screenshot.
Usage: uv run --with pillow --with cairosvg tools/build-og.py"""
import io, os
import cairosvg
from PIL import Image, ImageDraw
SITE = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "site")
base = Image.open(io.BytesIO(cairosvg.svg2png(url=os.path.join(SITE, "og-image.svg")))).convert("RGBA")
shot = Image.open(os.path.join(SITE, "img", "menu.webp")).convert("RGBA")
w = 540
shot = shot.resize((w, w * shot.height // shot.width), Image.LANCZOS)
mask = Image.new("L", shot.size, 0)
ImageDraw.Draw(mask).rounded_rectangle((0, 0, shot.width - 1, shot.height - 1), 18, fill=255)
x, y = 1200 - w - 48, (630 - shot.height) // 2
shadow = Image.new("RGBA", base.size, (0, 0, 0, 0))
ImageDraw.Draw(shadow).rounded_rectangle((x + 8, y + 12, x + w + 8, y + shot.height + 12), 18, fill=(0, 0, 0, 110))
base = Image.alpha_composite(base, shadow)
base.paste(shot, (x, y), mask)
base.convert("RGB").save(os.path.join(SITE, "og-image.png"), optimize=True)
print("site/og-image.png")
