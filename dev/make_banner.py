"""Agent Signals repo banner, docs/banner.png: 1280x640 (also the GitHub social preview size), drawn
at 2x and scaled down. Uses icon.ico, docs/screenshot.png and Windows' Segoe UI. Run: python dev/make_banner.py"""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter, ImageFont

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "docs" / "banner.png"
S = 2
W, H = 1280 * S, 640 * S
BG = (18, 20, 28)
TEXT, MUTED = (255, 255, 255), (156, 163, 175)
ACCENT = (217, 119, 87)  # Claude's orange (#D97757) for "for Claude Code"
RED, YELLOW, GREEN, GRAY = (239, 68, 68), (250, 204, 21), (34, 160, 70), (107, 114, 128)

def font(name, size):
    return ImageFont.truetype(rf"C:\Windows\Fonts\{name}", size * S)

img = Image.new("RGB", (W, H), BG)

# Soft glows in the three signal colors behind the window.
glow = Image.new("RGB", (W, H), BG)
g = ImageDraw.Draw(glow)
for (x, y, r, c) in [(1180, 40, 220, RED), (820, 330, 200, YELLOW), (1220, 600, 240, GREEN)]:
    g.ellipse([(x - r) * S, (y - r) * S, (x + r) * S, (y + r) * S], fill=tuple(int(v * 0.45 + b * 0.55) for v, b in zip(c, BG)))
img = Image.blend(img, glow.filter(ImageFilter.GaussianBlur(120 * S)), 0.75)
d = ImageDraw.Draw(img)

# Left: icon, name, promise.
icon = Image.open(rf"{REPO}\icon.ico")
icon.size = (256, 256)
icon = icon.convert("RGBA").resize((104 * S, 104 * S), Image.LANCZOS)
img.paste(icon, (72 * S, 100 * S), icon)
d.text((72 * S, 222 * S), "Agent Signals", font=font("segoeuib.ttf", 64), fill=TEXT)
d.text((74 * S, 308 * S), "for Claude Code", font=font("segoeuib.ttf", 44), fill=ACCENT)
d.text((74 * S, 382 * S), "Every session in one small window.", font=font("segoeui.ttf", 28), fill=TEXT)
d.text((74 * S, 436 * S), "See who waits, works or is done. Hear it happen.", font=font("segoeui.ttf", 20), fill=MUTED)
d.text((74 * S, 464 * S), "Click to jump to its terminal, or reopen a closed one.", font=font("segoeui.ttf", 20), fill=MUTED)

# Status legend.
x = 76
f = font("seguisb.ttf", 18)
# The app's status dots: yellow waits for you, green works, red just finished, gray rests or is closed.
WORKING = (34, 197, 94)
for label, c in [("waiting", YELLOW), ("working", WORKING), ("finished", RED), ("idle or closed", GRAY)]:
    d.ellipse([x * S, 536 * S, (x + 14) * S, 550 * S], fill=c)
    d.text(((x + 22) * S, 530 * S), label, font=f, fill=MUTED)
    x += 22 + int(d.textlength(label, font=f) / S) + 30

# Right: the real window, top part, bleeding off the bottom edge.
shot = Image.open(rf"{REPO}\docs\screenshot.png").convert("RGBA")
shot = shot.crop((0, 0, shot.width, min(860, shot.height)))
scale = 1.02 * S
shot = shot.resize((int(shot.width * scale), int(shot.height * scale)), Image.LANCZOS)
px, py = 760 * S, 64 * S
mask = Image.new("L", shot.size, 0)
ImageDraw.Draw(mask).rounded_rectangle([0, 0, shot.width - 1, shot.height + 40 * S], radius=14 * S, fill=255)
shadow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
ImageDraw.Draw(shadow).rounded_rectangle([px, py + 14 * S, px + shot.width, py + shot.height + 60 * S], radius=18 * S, fill=(0, 0, 0, 170))
img = Image.alpha_composite(img.convert("RGBA"), shadow.filter(ImageFilter.GaussianBlur(26 * S)))
img.paste(shot, (px, py), mask)
ImageDraw.Draw(img).rounded_rectangle([px, py, px + shot.width - 1, py + shot.height + 40 * S], radius=14 * S, outline=(59, 66, 86), width=2 * S)

img.convert("RGB").resize((W // S, H // S), Image.LANCZOS).save(OUT, optimize=True)
print("saved", OUT)
