"""Draws icon.ico: a traffic light (the board's waiting / working / idle colors) on a dark tile.

python dev/make_icon.py        (Pillow; writes icon.ico next to Cargo.toml)
"""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter

RED, YELLOW, GREEN = (239, 68, 68), (250, 204, 21), (34, 197, 94)
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def tile(size: int) -> Image.Image:
    """One icon frame, drawn 4x larger and scaled down. Small frames get bigger lights and no glow."""
    s = size * 4
    small = size <= 24
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))

    # Background: rounded square, dark navy, lighter at the top.
    bg = Image.new("RGBA", (s, s))
    top, bottom = (36, 42, 60), (16, 19, 28)
    for y in range(s):
        t = y / (s - 1)
        bg.paste(tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3)) + (255,), (0, y, s, y + 1))
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, s - 1, s - 1), radius=int(s * 0.22), fill=255)
    img.paste(bg, (0, 0), mask)

    # Housing of the light.
    hw = s * (0.46 if small else 0.40)
    hh = s * (0.86 if small else 0.78)
    x0, y0 = (s - hw) / 2, (s - hh) / 2
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((x0, y0, x0 + hw, y0 + hh), radius=hw * 0.45, fill=(11, 13, 19, 255),
                        outline=None if small else (70, 78, 100, 255), width=max(1, int(s * 0.012)))

    # Lights: red (needs you), yellow (working), green (idle).
    r = hw * (0.36 if small else 0.32)
    cx = s / 2
    step = hh / 3
    for i, color in enumerate((RED, YELLOW, GREEN)):
        cy = y0 + step * (i + 0.5)
        if not small:
            glow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
            ImageDraw.Draw(glow).ellipse((cx - r * 1.35, cy - r * 1.35, cx + r * 1.35, cy + r * 1.35), fill=color + (150,))
            img.alpha_composite(glow.filter(ImageFilter.GaussianBlur(r * 0.45)))
        d = ImageDraw.Draw(img)
        d.ellipse((cx - r, cy - r, cx + r, cy + r), fill=color + (255,))
        if not small:
            # A soft highlight at the top left of each lens, blended over it.
            hl = r * 0.38
            hx, hy = cx - r * 0.32, cy - r * 0.34
            shine = Image.new("RGBA", (s, s), (0, 0, 0, 0))
            ImageDraw.Draw(shine).ellipse((hx - hl, hy - hl * 0.7, hx + hl, hy + hl * 0.7), fill=(255, 255, 255, 90))
            img.alpha_composite(shine.filter(ImageFilter.GaussianBlur(r * 0.06)))

    return img.resize((size, size), Image.LANCZOS)


if __name__ == "__main__":
    root = Path(__file__).resolve().parent.parent
    frames = [tile(n) for n in SIZES]
    frames[-1].save(root / "icon.ico", format="ICO", sizes=[(n, n) for n in SIZES], append_images=frames[:-1])
    print("icon.ico:", ", ".join(str(n) for n in SIZES))
