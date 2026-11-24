"""Regenerate the Windows icon set from assets/yamada-source.ico.

The original aspect ratio is preserved on square transparent canvases.
Run: python -I assets/make_icons.py
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]

src = Image.open(HERE / "yamada-source.ico").convert("RGBA")


def icon_at(size):
    margin = max(1, round(size / 16))
    scale = (size - 2 * margin) / max(src.size)
    width, height = (max(1, round(axis * scale)) for axis in src.size)
    art = src.resize((width, height), Image.LANCZOS)
    frame = Image.new("RGBA", (size, size))
    frame.alpha_composite(art, ((size - width) // 2, (size - height) // 2))
    return frame


frames = [icon_at(size) for size in SIZES]

frames[-1].save(HERE / "yamada.ico", format="ICO", sizes=[(s, s) for s in SIZES],
                append_images=frames[:-1])
frames[-1].save(HERE / "yamada-256.png")
print("wrote", HERE / "yamada.ico", "and", HERE / "yamada-256.png")

# ---- Installer wizard images (Inno Setup 6.7 accepts PNG; 100% and 200% DPI variants)
def font(size, weight="Regular"):
    return ImageFont.truetype(HERE / "fonts" / f"Poppins-{weight}.ttf", size)


for scale in (1, 2):
    s = 55 * scale
    icon_at(s).save(HERE / f"wizard-small-{s}.png")

    w, h = 164 * scale, 314 * scale
    panel = Image.new("RGBA", (w, h), (0x1C, 0x1C, 0x1E, 255))
    d = ImageDraw.Draw(panel)
    # traffic lights, like the app's title bar
    for i, col in enumerate([(0xFF, 0x5F, 0x57), (0xFE, 0xBC, 0x2E), (0x28, 0xC8, 0x40)]):
        cx, cy, r = (16 + i * 18) * scale, 16 * scale, 6 * scale
        d.ellipse((cx - r, cy - r, cx + r, cy + r), fill=col)
    ic = icon_at(96 * scale)
    panel.alpha_composite(ic, ((w - ic.width) // 2, 92 * scale))
    f1, f2 = font(17 * scale, "SemiBold"), font(10 * scale)
    for text, f, y, col in (("BesideME", f1, 204, (0xF2, 0xF2, 0xF7)),
                            ("New chapters, straight", f2, 232, (0x8E, 0x8E, 0x93)),
                            ("to your Discord.", f2, 247, (0x8E, 0x8E, 0x93))):
        tw = d.textlength(text, font=f)
        d.text(((w - tw) / 2, y * scale), text, font=f, fill=col)
    panel.save(HERE / f"wizard-large-{w}x{h}.png")
print("wrote installer wizard images")
