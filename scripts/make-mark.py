"""Draws docs/brand/kbl-mark.png from the main Felix logo.

    python3 scripts/make-mark.py <felix>/docs/assets/logo.PNG docs/brand/kbl-mark.png

Needs Pillow.
"""
import math, sys
from PIL import Image, ImageChops, ImageDraw, ImageFilter

S = 2                      # supersample
W, H = 1000 * S, 1000 * S
CX, CY, R = 500 * S, 440 * S, 400 * S
CYAN = (11, 165, 200, 255)      # hsl(192, 90%, 46%)
CYAN_L = (120, 225, 245, 255)
INK = (16, 18, 24, 255)
AMBER = (251, 191, 36, 255)
WHITE = (255, 255, 255, 255)

out = Image.new("RGBA", (W, H), (0, 0, 0, 0))

# Cat head from the main logo, outer ring erased, clipped to the disk.
logo = Image.open(sys.argv[1]).convert("RGBA")
head = logo.crop((140, 100, 360, 290))
bw = head.convert("L").point(lambda v: 0 if v < 128 else 255)
# The crop catches part of the logo's own ring; flood it away from the edges.
for x in range(bw.width):
    if bw.getpixel((x, 0)) == 0:
        ImageDraw.floodfill(bw, (x, 0), 255)
for y in range(0, 120):
    if bw.getpixel((0, y)) == 0:
        ImageDraw.floodfill(bw, (0, y), 255)
# Whiten what the flood removed; everything else keeps its grey edges.
gray = head.convert("L")
flooded = ImageChops.subtract(bw, gray.point(lambda v: 0 if v < 128 else 255))
gray = ImageChops.lighter(gray, flooded)
scale = 3.15 * S
gray = gray.resize((int(gray.width * scale), int(gray.height * scale)), Image.BICUBIC).filter(ImageFilter.GaussianBlur(1.2 * S))
# Steep curve: crisp black and white with a two pixel anti-aliased edge.
g = gray.point(lambda v: max(0, min(255, int((v - 128) * 6 + 128))))
head = Image.merge("RGBA", (g, g, g, Image.new("L", g.size, 255)))
disk = Image.new("L", (W, H), 0)
ImageDraw.Draw(disk).ellipse((CX - R + 30*S, CY - R + 30*S, CX + R - 30*S, CY + R - 30*S), fill=255)
layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
layer.paste(head, (CX - head.width // 2, CY - int(head.height * 0.56)))
white = Image.new("RGBA", (W, H), WHITE)
out.paste(white, (0, 0), disk)
out.paste(layer, (0, 0), Image.composite(layer.split()[3], Image.new("L", (W, H), 0), disk))

d = ImageDraw.Draw(out)
# Ring.
d.ellipse((CX - R, CY - R, CX + R, CY + R), outline=CYAN, width=34 * S)

# Bottom: a log bar with records, agents attached to it.
bar_y = 905 * S
bar_h = 34 * S
x0, x1 = 60 * S, 940 * S
d.rounded_rectangle((x0, bar_y, x1, bar_y + bar_h), radius=10 * S, fill=INK)
cell = (x1 - x0 - 16 * S) / 22
for i in range(22):
    cx = x0 + 8 * S + i * cell
    col = CYAN if i % 3 else CYAN_L
    if i == 15: col = AMBER
    d.rectangle((cx + 3 * S, bar_y + 8 * S, cx + cell - 3 * S, bar_y + bar_h - 8 * S), fill=col)

agents = [(185, 800, CYAN), (343, 770, CYAN), (500, 790, AMBER), (657, 770, CYAN), (815, 800, CYAN)]
size = 100 * S
# Links between neighbours and down to the bar, drawn first.
pts = [(ax * S, ay * S) for ax, ay, _ in agents]
for (ax, ay), (bx, by) in zip(pts, pts[1:]):
    d.line((ax, ay, bx, by), fill=INK, width=14 * S)
for ax, ay in pts:
    d.line((ax, ay, ax, bar_y), fill=INK, width=14 * S)
for (ax, ay, rim) in agents:
    ax, ay = ax * S, ay * S
    h = size // 2
    d.rounded_rectangle((ax - h - 8*S, ay - h - 8*S, ax + h + 8*S, ay + h + 8*S), radius=34 * S, fill=WHITE)
    d.rounded_rectangle((ax - h, ay - h, ax + h, ay + h), radius=28 * S, fill=INK, outline=rim, width=10 * S)
    # Antenna.
    d.line((ax, ay - h - 6*S, ax, ay - h - 30*S), fill=INK, width=8 * S)
    d.ellipse((ax - 11*S, ay - h - 46*S, ax + 11*S, ay - h - 24*S), fill=rim)
    # Eyes.
    e = 15 * S
    for ex in (ax - 24 * S, ax + 24 * S):
        d.ellipse((ex - e, ay - 8*S - e, ex + e, ay - 8*S + e), fill=rim)
    d.rounded_rectangle((ax - 26*S, ay + 26*S, ax + 26*S, ay + 34*S), radius=4*S, fill=rim)

# Small compass ticks like the siblings.
for ang in (0, 90, 270):
    a = math.radians(ang - 90)
    rx, ry = CX + math.cos(a) * (R + 45 * S), CY + math.sin(a) * (R + 45 * S)
    d.line((CX + math.cos(a) * (R + 30*S), CY + math.sin(a) * (R + 30*S), rx, ry), fill=CYAN, width=14 * S)

out = out.resize((W // S, H // S), Image.LANCZOS)
out = out.crop(out.getbbox())
out.save(sys.argv[2])
