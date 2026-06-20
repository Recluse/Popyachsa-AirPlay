#!/usr/bin/env python3
"""Render the Popyachsa AirPlay logo — a clean, centered AirPlay glyph (display
rectangle outline + upward triangle) on an iOS-style rounded blue square.

Supersamples 4x for crisp anti-aliasing, then exports:
  icons/app.ico            (multi-size: 16..256)  — exe + About + installer
  ../landing/assets/icon.png (256)                — website
  ../landing/assets/og.png   (1200x630)           — social share card

Run:  python tools/gen-icon.py
"""
from PIL import Image, ImageDraw, ImageFont
from pathlib import Path

HERE = Path(__file__).resolve().parent
ICONS = HERE.parent / "icons"
LANDING = HERE.parent.parent / "landing" / "assets"

SS = 4               # supersample factor
N = 1024             # final master size
M = N * SS           # work size


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(len(a)))


def rounded_mask(size, radius):
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=255)
    return m


def make_master(bg_top, bg_bot):
    """Return an N x N RGBA logo with the given background gradient."""
    # Vertical gradient background.
    grad = Image.new("RGB", (1, M))
    for y in range(M):
        grad.putpixel((0, y), lerp(bg_top, bg_bot, y / (M - 1)))
    grad = grad.resize((M, M))
    bg = Image.new("RGBA", (M, M), (0, 0, 0, 0))
    bg.paste(grad, (0, 0), rounded_mask(M, int(0.225 * M)))

    # White AirPlay glyph, centered with even margins.
    g = Image.new("RGBA", (M, M), (0, 0, 0, 0))
    d = ImageDraw.Draw(g)
    white = (255, 255, 255, 255)

    cx = M // 2
    # Display rectangle (outline). Sized large (iOS-glyph margins) so the mark
    # reads at 16px and fills the rounded square.
    rw, rh = int(0.740 * M), int(0.470 * M)
    # rect_top == side margin (0.130) so top and side gaps match optically;
    # the bottom-heavy triangle then sits slightly above true center (balanced).
    rect_top = int(0.130 * M)
    rect_left = cx - rw // 2
    rect_right = cx + rw // 2
    rect_bot = rect_top + rh
    stroke = int(0.086 * M)
    d.rounded_rectangle([rect_left, rect_top, rect_right, rect_bot],
                        radius=int(0.072 * M), outline=white, width=stroke)

    # Upward triangle: base below the rectangle, apex inside it. Drawn in white
    # over the rectangle's bottom stroke so the union reads as one clean mark.
    tri_half = int(0.228 * M)
    base_y = int(0.860 * M)
    apex_y = int(0.330 * M)
    d.polygon([(cx - tri_half, base_y), (cx + tri_half, base_y), (cx, apex_y)],
              fill=white)

    out = Image.alpha_composite(bg, g)
    return out.resize((N, N), Image.LANCZOS)


def main():
    ICONS.mkdir(exist_ok=True)
    LANDING.mkdir(parents=True, exist_ok=True)

    blue_top = (0x2A, 0x9B, 0xFF)
    blue_bot = (0x0A, 0x84, 0xFF)
    master = make_master(blue_top, blue_bot)

    # app.ico (multi-size). Force BMP-format frames (not PNG): Windows Explorer
    # / GDI+ fail to decode PNG-compressed ICO frames at 256px, showing a blank
    # icon — BMP frames render everywhere (Explorer, NSIS installer, taskbar).
    sizes = [16, 32, 48, 64, 128, 256]
    master.save(ICONS / "app.ico", sizes=[(s, s) for s in sizes],
                bitmap_format="bmp")
    # landing png
    master.resize((256, 256), Image.LANCZOS).save(LANDING / "icon.png")
    print("wrote", ICONS / "app.ico", "and", LANDING / "icon.png")

    # og.png 1200x630 — icon lifted, title below.
    W, H = 1200, 630
    og = Image.new("RGB", (W, H), (0x0D, 0x0F, 0x14))
    # subtle blue glow band at top
    glow = Image.new("RGB", (1, H))
    for y in range(H):
        t = max(0.0, 1 - y / 300)
        glow.putpixel((0, y), lerp((0x0D, 0x0F, 0x14), (0x14, 0x2A, 0x4A), t))
    og.paste(glow.resize((W, H)), (0, 0))
    isz = 188
    icon = master.resize((isz, isz), Image.LANCZOS)
    og.paste(icon, ((W - isz) // 2, 96), icon)
    d = ImageDraw.Draw(og)

    def font(sz):
        for name in ("segoeuib.ttf", "seguisb.ttf", "arialbd.ttf"):
            try:
                return ImageFont.truetype("C:/Windows/Fonts/" + name, sz)
            except OSError:
                continue
        return ImageFont.load_default()

    def centered(text, y, f, fill):
        w = d.textlength(text, font=f)
        d.text(((W - w) / 2, y), text, font=f, fill=fill)

    centered("Popyachsa AirPlay", 320, font(64), (0xF2, 0xF2, 0xF7))
    centered("AirPlay receiver for Windows", 420, font(30), (0x9A, 0x9F, 0xAA))
    og.save(LANDING / "og.png")
    print("wrote", LANDING / "og.png")


if __name__ == "__main__":
    main()
