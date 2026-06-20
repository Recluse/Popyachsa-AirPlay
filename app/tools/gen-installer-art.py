#!/usr/bin/env python3
"""Generate NSIS MUI2 branding bitmaps from the app icon.

  installer/art/welcome.bmp  164x314  — left sidebar on Welcome/Finish pages
  installer/art/header.bmp   150x57   — top-right header on inner pages

NSIS wants 24-bit BMPs (no alpha), so we composite the icon onto a solid/
gradient dark background and save as BMP.
"""
from PIL import Image, ImageDraw, ImageFont
from pathlib import Path

HERE = Path(__file__).resolve().parent
ICON = HERE.parent / "icons" / "app.ico"
ART = HERE.parent / "installer" / "art"
ART.mkdir(parents=True, exist_ok=True)

BG_TOP = (0x16, 0x1B, 0x26)
BG_BOT = (0x0B, 0x0D, 0x12)
BLUE = (0x0A, 0x84, 0xFF)
DIM = (0x9A, 0x9F, 0xAA)
WHITE = (0xF2, 0xF2, 0xF7)


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def vgrad(w, h, top, bot):
    g = Image.new("RGB", (1, h))
    for y in range(h):
        g.putpixel((0, y), lerp(top, bot, y / (h - 1)))
    return g.resize((w, h))


def load_icon(size):
    im = Image.open(ICON)
    im.size = (256, 256)
    im = im.convert("RGBA").resize((size, size), Image.LANCZOS)
    return im


def font(sz, bold=True):
    for n in (("segoeuib.ttf", "seguisb.ttf") if bold else ("segoeui.ttf",)):
        try:
            return ImageFont.truetype("C:/Windows/Fonts/" + n, sz)
        except OSError:
            pass
    return ImageFont.load_default()


# Welcome / finish sidebar.
w, h = 164, 314
img = vgrad(w, h, BG_TOP, BG_BOT)
d = ImageDraw.Draw(img)
isz = 96
icon = load_icon(isz)
img.paste(icon, ((w - isz) // 2, 54), icon)
def centered(text, y, f, fill):
    tw = d.textlength(text, font=f)
    d.text(((w - tw) / 2, y), text, font=f, fill=fill)
centered("Popyachsa", 168, font(19), WHITE)
centered("AirPlay", 190, font(19), WHITE)
centered("for Windows", 218, font(12, False), DIM)
d.rectangle([0, h - 4, w, h], fill=BLUE)
img.save(ART / "welcome.bmp")

# Inner-page header (icon + wordmark on the right).
w, h = 150, 57
img = Image.new("RGB", (w, h), (0xFF, 0xFF, 0xFF))  # MUI header bg is light
d = ImageDraw.Draw(img)
isz = 40
icon = load_icon(isz)
img.paste(icon, (w - isz - 8, (h - isz) // 2), icon)
img.save(ART / "header.bmp")

print("wrote", ART / "welcome.bmp", "and", ART / "header.bmp")
