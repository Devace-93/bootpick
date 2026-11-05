#!/usr/bin/env python3
"""Renders SVG/PNG icons and wallpapers into the BMP files bootpick loads at boot.

Usage: uv run --with pillow --with cairosvg tools/build-assets.py [out_dir]
"""
import io, os, struct, sys, glob
from PIL import Image
import cairosvg

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "esp", "EFI", "bootpick")
ICON_PX = 256

UI_ICONS = {
    # Monochrome masks (white on transparent); the app tints them with the theme colour.
    "ui_gear": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z'/><path d='M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z'/></svg>""",
    "ui_power": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M18.36 6.64a9 9 0 1 1-12.73 0'/><line x1='12' y1='2' x2='12' y2='12'/></svg>""",
    "ui_restart": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M3 12a9 9 0 1 0 3-6.7'/><polyline points='3 3 3 9 9 9'/></svg>""",
    "ui_firmware": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><rect x='5' y='5' width='14' height='14' rx='2'/><rect x='9' y='9' width='6' height='6'/><path d='M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3'/></svg>""",
    "ui_eye": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12Z'/><circle cx='12' cy='12' r='3'/></svg>""",
    "ui_eye_off": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M17.9 17.9A10 10 0 0 1 12 19c-6.5 0-10-7-10-7a18 18 0 0 1 5.1-5.9M9.9 5.2A9 9 0 0 1 12 5c6.5 0 10 7 10 7a18 18 0 0 1-2.2 3.2'/><path d='M14.1 14.1a3 3 0 1 1-4.2-4.2'/><line x1='2' y1='2' x2='22' y2='22'/></svg>""",
    "ui_check": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='3.5' stroke-linecap='round' stroke-linejoin='round'><polyline points='4 12.5 9.5 18 20 6.5'/></svg>""",
    "ui_up": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='3' stroke-linecap='round' stroke-linejoin='round'><polyline points='5 15 12 8 19 15'/></svg>""",
    "ui_down": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='3' stroke-linecap='round' stroke-linejoin='round'><polyline points='5 9 12 16 19 9'/></svg>""",
    "ui_left": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='3' stroke-linecap='round' stroke-linejoin='round'><polyline points='15 5 8 12 15 19'/></svg>""",
    "ui_right": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='3' stroke-linecap='round' stroke-linejoin='round'><polyline points='9 5 16 12 9 19'/></svg>""",
    "ui_edit": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M12 20h9'/><path d='M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z'/></svg>""",
}

OS_SVGS = {
    # Windows: four tiles, Microsoft blue.
    "os_win": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 128 128'><g fill='#0a84ff'><rect x='12' y='12' width='49' height='49' rx='4'/><rect x='67' y='12' width='49' height='49' rx='4'/><rect x='12' y='67' width='49' height='49' rx='4'/><rect x='67' y='67' width='49' height='49' rx='4'/></g></svg>""",
    # Generic UEFI / firmware entry.
    "os_uefi": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 128 128'><circle cx='64' cy='64' r='56' fill='#475569'/><text x='64' y='78' font-family='DejaVu Sans, sans-serif' font-weight='bold' font-size='34' fill='#f8fafc' text-anchor='middle'>EFI</text></svg>""",
    "os_network": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#38bdf8' stroke-width='1.8' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='10'/><path d='M2 12h20M12 2a15 15 0 0 1 0 20M12 2a15 15 0 0 0 0 20'/></svg>""",
    "vol_external": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#a3e635' stroke-width='1.8' stroke-linecap='round' stroke-linejoin='round'><rect x='3' y='7' width='18' height='10' rx='3'/><path d='M7 12h.01M11 12h2'/></svg>""",
    "vol_optical": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#c084fc' stroke-width='1.8'><circle cx='12' cy='12' r='10'/><circle cx='12' cy='12' r='3'/></svg>""",
}

def write_bmp(path, img):
    """32-bit BGRA BMP with a V4 header (BI_BITFIELDS) so alpha survives."""
    img = img.convert("RGBA")
    w, h = img.size
    px = img.tobytes("raw", "BGRA")
    stride = w * 4
    rows = [px[(h - 1 - y) * stride:(h - y) * stride] for y in range(h)]  # bottom-up
    data = b"".join(rows)
    dib = struct.pack("<IiiHHIIiiII", 108, w, h, 1, 32, 3, len(data), 2835, 2835, 0, 0)
    dib += struct.pack("<IIII", 0x00FF0000, 0x0000FF00, 0x000000FF, 0xFF000000)
    dib += b"sRGB" + b"\0" * 36 + struct.pack("<III", 0, 0, 0)
    assert len(dib) == 108
    header = struct.pack("<2sIHHI", b"BM", 14 + len(dib) + len(data), 0, 0, 14 + len(dib))
    with open(path, "wb") as f:
        f.write(header + dib + data)

def write_bmp24(path, img):
    img = img.convert("RGB")
    w, h = img.size
    stride = (w * 3 + 3) & ~3
    raw = img.tobytes("raw", "BGR")
    rows = [raw[(h - 1 - y) * w * 3:(h - y) * w * 3].ljust(stride, b"\0") for y in range(h)]
    data = b"".join(rows)
    dib = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(data), 2835, 2835, 0, 0)
    header = struct.pack("<2sIHHI", b"BM", 14 + 40 + len(data), 0, 0, 54)
    with open(path, "wb") as f:
        f.write(header + dib + data)

def svg_to_image(svg_text, size):
    png = cairosvg.svg2png(bytestring=svg_text.encode(), output_width=size, output_height=size)
    return Image.open(io.BytesIO(png)).convert("RGBA")

def fit_square(img, size, pad=0.06):
    """Scales an RGBA image to fit in a size x size square with padding, centred."""
    inner = int(size * (1 - 2 * pad))
    img = img.convert("RGBA")
    bbox = img.getbbox()
    if bbox:
        img = img.crop(bbox)
    r = min(inner / img.width, inner / img.height)
    img = img.resize((max(1, int(img.width * r)), max(1, int(img.height * r))), Image.LANCZOS)
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    out.paste(img, ((size - img.width) // 2, (size - img.height) // 2), img)
    return out

def main():
    icons = os.path.join(OUT, "icons")
    bgs = os.path.join(OUT, "backgrounds")
    os.makedirs(icons, exist_ok=True)
    os.makedirs(bgs, exist_ok=True)
    n = 0
    for name, svg in UI_ICONS.items():
        write_bmp(os.path.join(icons, name + ".bmp"), svg_to_image(svg, 128)); n += 1
    for name, svg in OS_SVGS.items():
        write_bmp(os.path.join(icons, name + ".bmp"), fit_square(svg_to_image(svg, ICON_PX), ICON_PX, 0.0)); n += 1
    for path in glob.glob(os.path.join(ROOT, "assets", "svg", "*.svg")):
        name = os.path.splitext(os.path.basename(path))[0]
        img = svg_to_image(open(path).read(), ICON_PX)
        write_bmp(os.path.join(icons, name + ".bmp"), fit_square(img, ICON_PX, 0.04)); n += 1
    for path in glob.glob(os.path.join(ROOT, "assets", "svg", "*.png")):
        name = os.path.splitext(os.path.basename(path))[0]
        if name == "os_win8":
            continue
        img = Image.open(path).convert("RGBA")
        write_bmp(os.path.join(icons, name + ".bmp"), fit_square(img, ICON_PX, 0.04)); n += 1
    print(f"{n} icons -> {icons}")
    m = 0
    for path in sorted(glob.glob(os.path.join(ROOT, "assets", "backgrounds", "*"))):
        name = os.path.splitext(os.path.basename(path))[0]
        img = Image.open(path).convert("RGB")
        tw, th = 1920, 1080
        r = max(tw / img.width, th / img.height)
        img = img.resize((int(img.width * r + 0.5), int(img.height * r + 0.5)), Image.LANCZOS)
        l = (img.width - tw) // 2; t = (img.height - th) // 2
        img = img.crop((l, t, l + tw, t + th))
        write_bmp24(os.path.join(bgs, name + ".bmp"), img); m += 1
    print(f"{m} backgrounds -> {bgs}")

if __name__ == "__main__":
    main()
