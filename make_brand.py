#!/usr/bin/env python3
"""Generate all Watermark Remover brand art from one vector sparkle:
   SVG (icon + sparkle), the macOS app icon PNG, web favicons, and the OG card.
Run:  python3 make_brand.py
"""
import os, math
from PIL import Image, ImageDraw, ImageFont, ImageFilter

ROOT = os.path.dirname(os.path.abspath(__file__))
PUB = os.path.join(ROOT, "public")
ASSETS = os.path.join(ROOT, "desktop", "assets")
os.makedirs(PUB, exist_ok=True)
os.makedirs(ASSETS, exist_ok=True)

# ── palette ──
BG_TOP, BG_BOT = (19, 28, 46), (7, 11, 20)      # dark navy gradient
SPK_TOP, SPK_BOT = (255, 255, 255), (121, 180, 255)  # white → light blue
BORDER = (60, 90, 150)

# ── geometry (1024 reference) ──
# A 4-point sparkle: tips at ±R on the axes, sides bowed inward via cubic
# Béziers whose control points sit near the centre (offset c).
def sparkle_path_d(cx, cy, R, c):
    return (f"M {cx} {cy-R} "
            f"C {cx} {cy-c} {cx+c} {cy} {cx+R} {cy} "
            f"C {cx+c} {cy} {cx} {cy+c} {cx} {cy+R} "
            f"C {cx} {cy+c} {cx-c} {cy} {cx-R} {cy} "
            f"C {cx-c} {cy} {cx} {cy-c} {cx} {cy-R} Z")

def _cubic(p0, c1, c2, p3, n):
    pts = []
    for i in range(n):
        t = i / n
        mt = 1 - t
        x = mt**3*p0[0] + 3*mt*mt*t*c1[0] + 3*mt*t*t*c2[0] + t**3*p3[0]
        y = mt**3*p0[1] + 3*mt*mt*t*c1[1] + 3*mt*t*t*c2[1] + t**3*p3[1]
        pts.append((x, y))
    return pts

def sparkle_points(cx, cy, R, c, n=48):
    segs = [
        ((cx, cy-R), (cx, cy-c), (cx+c, cy), (cx+R, cy)),
        ((cx+R, cy), (cx+c, cy), (cx, cy+c), (cx, cy+R)),
        ((cx, cy+R), (cx, cy+c), (cx-c, cy), (cx-R, cy)),
        ((cx-R, cy), (cx-c, cy), (cx, cy-c), (cx, cy-R)),
    ]
    out = []
    for s in segs:
        out += _cubic(*s, n)
    return out

def vgrad(w, h, top, bot):
    g = Image.new("RGB", (w, h))
    px = g.load()
    for y in range(h):
        f = y / max(1, h - 1)
        col = tuple(int(top[i] + (bot[i] - top[i]) * f) for i in range(3))
        for x in range(w):
            px[x, y] = col
    return g

# ── raster icon (square, transparent corners) ──
def render_icon(px, margin_frac, radius_frac, R_frac=350/1024, c_frac=120/1024,
                border=True):
    SS = 4
    W = px * SS
    cx = cy = W / 2
    img = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    m = margin_frac * W
    rad = radius_frac * W
    # squircle background
    mask = Image.new("L", (W, W), 0)
    ImageDraw.Draw(mask).rounded_rectangle([m, m, W-m, W-m], radius=rad, fill=255)
    img.paste(vgrad(W, W, BG_TOP, BG_BOT), (0, 0), mask)
    if border:
        ImageDraw.Draw(img).rounded_rectangle(
            [m, m, W-m, W-m], radius=rad, outline=BORDER + (150,), width=max(2, int(W*0.004)))
    # sparkle
    pts = sparkle_points(cx, cy, R_frac*W, c_frac*W)
    smask = Image.new("L", (W, W), 0)
    ImageDraw.Draw(smask).polygon(pts, fill=255)
    img.paste(vgrad(W, W, SPK_TOP, SPK_BOT), (0, 0), smask)
    return img.resize((px, px), Image.LANCZOS)

# ── SVG writers ──
def write_svg(path, margin, radius, R, c, size=1024, border=True):
    cx = cy = size // 2
    b = (f'<rect x="{margin}" y="{margin}" width="{size-2*margin}" height="{size-2*margin}" '
         f'rx="{radius}" fill="url(#bg)" '
         + (f'stroke="#3c5a96" stroke-opacity="0.55" stroke-width="4"/>' if border else '/>'))
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" width="{size}" height="{size}">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#131c2e"/><stop offset="1" stop-color="#070b14"/>
    </linearGradient>
    <linearGradient id="spk" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#79b4ff"/>
    </linearGradient>
  </defs>
  {b}
  <path d="{sparkle_path_d(cx, cy, R, c)}" fill="url(#spk)"/>
</svg>
'''
    with open(path, "w") as f:
        f.write(svg)

def write_sparkle_svg(path, size=1024):
    cx = cy = size // 2
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" width="{size}" height="{size}">
  <defs><linearGradient id="spk" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#79b4ff"/>
  </linearGradient></defs>
  <path d="{sparkle_path_d(cx, cy, 350, 120)}" fill="url(#spk)"/>
</svg>
'''
    with open(path, "w") as f:
        f.write(svg)

# ── OG card 1200×630 ──
def font(sz, bold=False):
    cands = (["/System/Library/Fonts/Supplemental/Arial Bold.ttf",
              "/Library/Fonts/Arial Bold.ttf",
              "/System/Library/Fonts/Helvetica.ttc"] if bold else
             ["/System/Library/Fonts/Supplemental/Arial.ttf",
              "/Library/Fonts/Arial.ttf",
              "/System/Library/Fonts/Helvetica.ttc"])
    for c in cands:
        if os.path.exists(c):
            try:
                return ImageFont.truetype(c, sz)
            except Exception:
                pass
    return ImageFont.load_default()

def render_og(path):
    W, H = 1200, 630
    img = vgrad(W, H, (14, 22, 34), (7, 11, 20)).convert("RGBA")
    # soft glow behind the sparkle
    glow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse([40, 110, 500, 520], fill=(60, 110, 200, 90))
    img = Image.alpha_composite(img, glow.filter(ImageFilter.GaussianBlur(70)))
    # sparkle (rendered big, transparent bg, then pasted)
    SP = 400
    spk = Image.new("RGBA", (SP*4, SP*4), (0, 0, 0, 0))
    pts = sparkle_points(SP*4/2, SP*4/2, 350/1024*SP*4, 120/1024*SP*4)
    sm = Image.new("L", spk.size, 0)
    ImageDraw.Draw(sm).polygon(pts, fill=255)
    spk.paste(vgrad(*spk.size, SPK_TOP, SPK_BOT), (0, 0), sm)
    spk = spk.resize((SP, SP), Image.LANCZOS)
    img.alpha_composite(spk, (80, 115))
    d = ImageDraw.Draw(img)
    x, right = 520, 1150
    # auto-fit the title to the text column
    tsize = 86
    while tsize > 40 and d.textlength("Watermark Remover", font=font(tsize, True)) > right - x:
        tsize -= 2
    d.text((x, 170), "Watermark Remover", font=font(tsize, True), fill=(236, 240, 247))
    d.text((x, 292), "Erase the Gemini watermark — instantly,", font=font(29), fill=(154, 166, 178))
    d.text((x, 332), "right in your browser. No upload, no sign-up.", font=font(29), fill=(154, 166, 178))
    d.text((x, 420), "watermarkremover.jderrick.app", font=font(29, True), fill=(121, 180, 255))
    img.convert("RGB").save(path)

# ── build everything ──
# app icon (macOS): ~9% margin, squircle radius ~20%
render_icon(1024, 96/1024, 205/1024).save(os.path.join(ASSETS, "icon-1024.png"))
write_svg(os.path.join(ASSETS, "icon.svg"), 96, 205, 350, 120)
write_sparkle_svg(os.path.join(ASSETS, "sparkle.svg"))

# web favicons: fill more of the frame so it reads at 16px
write_svg(os.path.join(PUB, "favicon.svg"), 40, 232, 365, 125)
fav = lambda px: render_icon(px, 40/1024, 232/1024, R_frac=365/1024, c_frac=125/1024, border=False)
fav(16).save(os.path.join(PUB, "favicon-16.png"))
fav(32).save(os.path.join(PUB, "favicon-32.png"))
fav(180).save(os.path.join(PUB, "apple-touch-icon.png"))
fav(192).save(os.path.join(PUB, "icon-192.png"))
fav(512).save(os.path.join(PUB, "icon-512.png"))
write_sparkle_svg(os.path.join(PUB, "sparkle.svg"))

render_og(os.path.join(PUB, "og.png"))

print("Generated:")
for p in ["desktop/assets/icon-1024.png", "desktop/assets/icon.svg", "desktop/assets/sparkle.svg",
          "public/favicon.svg", "public/favicon-16.png", "public/favicon-32.png",
          "public/apple-touch-icon.png", "public/icon-192.png", "public/icon-512.png",
          "public/sparkle.svg", "public/og.png"]:
    fp = os.path.join(ROOT, p)
    print(f"  {p:34} {os.path.getsize(fp):>8} bytes")
