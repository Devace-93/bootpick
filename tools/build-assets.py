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

FA = os.path.join(ROOT, "assets", "fa")

# Monochrome UI glyphs (white on transparent); the app tints them with the theme.
UI_ICONS = {
    "ui_gear": "gear", "ui_power": "power-off", "ui_restart": "rotate-right",
    "ui_firmware": "microchip", "ui_eye": "eye", "ui_eye_off": "eye-slash",
    "ui_check": "check", "ui_up": "chevron-up", "ui_down": "chevron-down",
    "ui_left": "chevron-left", "ui_right": "chevron-right", "ui_edit": "pen",
    "ui_last": "clock-rotate-left", "ui_globe": "globe",
}

# Generic entry tiles: a white Font Awesome glyph on a coloured disc, so tools
# and unknown loaders share one visual style with the UI.
GENERIC_TILES = {
    "os_uefi": ("microchip", "#475569"),
    "os_unknown": ("circle-question", "#64748b"),
    "os_linux": ("linux", "#334155"),
    "os_systemd": ("list", "#0f766e"),
    "os_refind": ("list", "#7c3aed"),
    "os_network": ("network-wired", "#0369a1"),
    "tool_shell": ("terminal", "#4b5563"),
    "tool_memtest": ("memory", "#b45309"),
    "vol_external": ("usb", "#4d7c0f"),
    "vol_optical": ("compact-disc", "#6d28d9"),
}

# Windows keeps its own four-pane mark.
OS_SVGS = {
    "os_win": """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 128 128'><g fill='#0a84ff'><rect x='12' y='12' width='49' height='49' rx='4'/><rect x='67' y='12' width='49' height='49' rx='4'/><rect x='12' y='67' width='49' height='49' rx='4'/><rect x='67' y='67' width='49' height='49' rx='4'/></g></svg>""",
}


def fa_svg(name, fill="#ffffff"):
    text = open(os.path.join(FA, name + ".svg")).read()
    return text.replace("<path ", f"<path fill='{fill}' ", 1)


def fa_image(name, size, fill="#ffffff"):
    return svg_to_image(fa_svg(name, fill), size)


def generic_tile(glyph, colour, size):
    disc = f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><circle cx='50' cy='50' r='48' fill='{colour}'/></svg>"
    base = svg_to_image(disc, size)
    g = fit_square(fa_image(glyph, size), int(size * 0.5), 0.0)
    base.alpha_composite(g, ((size - g.width) // 2, (size - g.height) // 2))
    return base


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
    """Renders an SVG so it fits a size x size square, keeping its aspect ratio."""
    png = cairosvg.svg2png(bytestring=svg_text.encode(), output_width=size)
    img = Image.open(io.BytesIO(png)).convert("RGBA")
    if img.height > size:
        png = cairosvg.svg2png(bytestring=svg_text.encode(), output_height=size)
        img = Image.open(io.BytesIO(png)).convert("RGBA")
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    out.paste(img, ((size - img.width) // 2, (size - img.height) // 2), img)
    return out

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
    for name in os.listdir(icons):
        if name.endswith(".bmp"):
            os.remove(os.path.join(icons, name))
    for name, glyph in UI_ICONS.items():
        write_bmp(os.path.join(icons, name + ".bmp"), fit_square(fa_image(glyph, 128), 128, 0.0)); n += 1
    for name, (glyph, colour) in GENERIC_TILES.items():
        write_bmp(os.path.join(icons, name + ".bmp"), generic_tile(glyph, colour, ICON_PX)); n += 1
    for name, svg in OS_SVGS.items():
        write_bmp(os.path.join(icons, name + ".bmp"), fit_square(svg_to_image(svg, ICON_PX), ICON_PX, 0.0)); n += 1
    for path in glob.glob(os.path.join(ROOT, "assets", "svg", "*.svg")):
        name = os.path.splitext(os.path.basename(path))[0]
        img = svg_to_image(open(path).read(), ICON_PX)
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
