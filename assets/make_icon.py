"""Draws the DevHub icon: a mint hexagon outline with a >_ prompt inside.
Writes assets/devhub.ico (16..256 px) and assets/icon.png (512 px)."""
import math, sys, os
from PIL import Image, ImageDraw, ImageFilter

out_dir = sys.argv[1]
S = 1024                      # draw big, scale down for sharp small sizes
MINT = (92, 240, 186, 255)    # #5CF0BA, the app's accent
FILL = (8, 14, 12, 255)       # near-black, like the dashboard


def hexagon(cx, cy, r):
    # pointy-top hexagon
    return [(cx + r * math.cos(math.radians(a)), cy + r * math.sin(math.radians(a)))
            for a in range(-90, 270, 60)]


def draw(size_hint):
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    # small sizes get thicker strokes so they stay readable
    stroke = 70 if size_hint <= 32 else 52
    R = S * (0.455 if size_hint <= 32 else 0.42)
    corners = hexagon(S / 2, S / 2, R)
    # start and end mid-edge, so every corner gets a proper joint (no notch)
    mid = ((corners[0][0] + corners[1][0]) / 2, (corners[0][1] + corners[1][1]) / 2)
    pts = [mid] + corners[1:] + [corners[0]]

    # soft glow behind the outline (skipped for tiny sizes, where it just blurs)
    if size_hint >= 48:
        glow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
        g = ImageDraw.Draw(glow)
        g.line(pts + [mid], fill=(92, 240, 186, 110), width=stroke + 30, joint="curve")
        glow = glow.filter(ImageFilter.GaussianBlur(26))
        img = Image.alpha_composite(img, glow)

    d = ImageDraw.Draw(img)
    inner = hexagon(S / 2, S / 2, R - stroke / 2)
    d.polygon(inner, fill=FILL)
    d.line(pts + [mid], fill=MINT, width=stroke, joint="curve")

    # >  chevron
    w = 74 if size_hint <= 32 else 62
    cx, cy = S * 0.40, S * 0.50
    a = S * 0.13
    chev = [(cx - a * 0.75, cy - a), (cx + a * 0.55, cy), (cx - a * 0.75, cy + a)]
    d.line(chev, fill=MINT, width=w, joint="curve")
    for p in (chev[0], chev[2]):
        d.ellipse([p[0] - w / 2, p[1] - w / 2, p[0] + w / 2, p[1] + w / 2], fill=MINT)
    # _  cursor
    x0, y0 = S * 0.52, cy + a - w / 2
    d.rounded_rectangle([x0, y0, x0 + S * 0.20, y0 + w], radius=w / 2, fill=MINT)
    return img


os.makedirs(out_dir, exist_ok=True)
sizes = [16, 24, 32, 48, 64, 128, 256]
frames = [draw(s).resize((s, s), Image.LANCZOS) for s in sizes]
frames[-1].save(os.path.join(out_dir, "devhub.ico"), format="ICO",
                sizes=[(s, s) for s in sizes], append_images=frames[:-1])
draw(512).resize((512, 512), Image.LANCZOS).save(os.path.join(out_dir, "icon.png"))
# a strip of every size, for checking by eye
strip = Image.new("RGBA", (sum(sizes) + 10 * len(sizes), 256), (20, 20, 20, 255))
x = 0
for f in frames:
    strip.paste(f, (x, 256 - f.height), f)
    x += f.width + 10
strip.save(sys.argv[2])
print("ok")

