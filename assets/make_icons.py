"""Regenerate the Windows icon set from assets/yamada-source.ico.

The source is a single 22x32 image. It is centred on a transparent square
canvas (keeping its aspect ratio) and resampled to the standard Windows icon
sizes. Run: python -I assets/make_icons.py
"""
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
SIZES = [16, 24, 32, 48, 64, 128, 256]

src = Image.open(HERE / "yamada-source.ico").convert("RGBA")
side = max(src.size)
square = Image.new("RGBA", (side, side), (0, 0, 0, 0))
square.paste(src, ((side - src.width) // 2, (side - src.height) // 2))

frames = []
for size in SIZES:
    # Downscale smoothly; upscale with a two-step resample to limit blockiness.
    if size <= side:
        frames.append(square.resize((size, size), Image.LANCZOS))
    else:
        mid = square.resize((side * 4, side * 4), Image.NEAREST)
        frames.append(mid.resize((size, size), Image.LANCZOS))

frames[-1].save(HERE / "yamada.ico", format="ICO", sizes=[(s, s) for s in SIZES],
                append_images=frames[:-1])
frames[-1].save(HERE / "yamada-256.png")
print("wrote", HERE / "yamada.ico", "and", HERE / "yamada-256.png")

# ---- Installer wizard images (Inno Setup 6.7 accepts PNG; 100% and 200% DPI variants)
from PIL import ImageDraw, ImageFont

def font(size):
    for name in ("segoeuib.ttf", "segoeui.ttf", "arial.ttf"):
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            continue
    return ImageFont.load_default()

def icon_at(size):
    mid = square.resize((side * 8, side * 8), Image.NEAREST)
    return mid.resize((size, size), Image.LANCZOS)

def rounded_icon(size, radius):
    ic = icon_at(size)
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, size - 1, size - 1), radius, fill=255)
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    out.paste(ic, (0, 0), mask)
    return out

for scale in (1, 2):
    s = 55 * scale
    rounded_icon(s, 10 * scale).save(HERE / f"wizard-small-{s}.png")

    w, h = 164 * scale, 314 * scale
    panel = Image.new("RGBA", (w, h), (0x1C, 0x1C, 0x1E, 255))
    d = ImageDraw.Draw(panel)
    # traffic lights, like the app's title bar
    for i, col in enumerate([(0xFF, 0x5F, 0x57), (0xFE, 0xBC, 0x2E), (0x28, 0xC8, 0x40)]):
        cx, cy, r = (16 + i * 18) * scale, 16 * scale, 6 * scale
        d.ellipse((cx - r, cy - r, cx + r, cy + r), fill=col)
    ic = rounded_icon(96 * scale, 18 * scale)
    panel.paste(ic, ((w - ic.width) // 2, 92 * scale), ic)
    f1, f2 = font(17 * scale), font(10 * scale)
    for text, f, y, col in (("Manga Watcher", f1, 204, (0xF2, 0xF2, 0xF7)),
                            ("New chapters, straight", f2, 232, (0x8E, 0x8E, 0x93)),
                            ("to your Discord.", f2, 247, (0x8E, 0x8E, 0x93))):
        tw = d.textlength(text, font=f)
        d.text(((w - tw) / 2, y * scale), text, font=f, fill=col)
    panel.save(HERE / f"wizard-large-{w}x{h}.png")
print("wrote installer wizard images")
