"""Cuts the app icon and the two tray icons out of assets/src/icon-sheet.png.

Run from the repo root: python3 assets/make_icons.py [contact-sheet.png]
The large app icon frames are cut from the sheet (variation 08). The tray
icons follow the sheet's round pair but are redrawn, because 16 pixels cannot
carry the original detail.
"""
from PIL import Image, ImageChops, ImageDraw, ImageFilter

import sys

CONTACT_SHEET = sys.argv[1] if len(sys.argv) > 1 else "target/icons-contact-sheet.png"
SHEET = Image.open("assets/src/icon-sheet.png").convert("RGB")

# Boxes on the sheet: (left, top, right, bottom).
APP_TILE = (640, 416, 900, 668)    # 08, the cat looking over the keyboard
APP_ART = (662, 452, 880, 600)     # the same tile without the wordmark


def keyed(box):
    """Artwork on transparency: alpha is the brightness above the background."""
    art = SHEET.crop(box)
    r, g, b = art.split()
    brightness = ImageChops.lighter(ImageChops.lighter(r, g), b)
    alpha = brightness.point(lambda v: 0 if v < 40 else min(255, int((v - 40) * 255 / 150)))
    # Full-strength colour under the alpha, otherwise edges turn grey.
    solid = art.point(lambda v: min(255, int(v * 1.25)))
    solid.putalpha(alpha)
    return solid.crop(alpha.getbbox())


def on_square(art, size, pad, plate):
    """Centres the art on a square canvas, optionally on a dark rounded plate."""
    big = size * 4
    canvas = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    if plate:
        ImageDraw.Draw(canvas).rounded_rectangle(
            (0, 0, big - 1, big - 1), radius=big * 22 // 100, fill=(5, 7, 9, 255))
    room = big - 2 * pad * 4
    scale = min(room / art.width, room / art.height)
    fitted = art.resize((max(1, round(art.width * scale)), max(1, round(art.height * scale))), Image.LANCZOS)
    canvas.alpha_composite(fitted, ((big - fitted.width) // 2, (big - fitted.height) // 2))
    small = canvas.resize((size, size), Image.LANCZOS)
    return small.filter(ImageFilter.UnsharpMask(radius=0.6, percent=80)) if size <= 32 else small


def tile(size):
    big = SHEET.crop(APP_TILE).convert("RGBA")
    mask = Image.new("L", big.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, big.width - 1, big.height - 1), radius=big.width * 22 // 100, fill=255)
    big.putalpha(mask)
    return big.resize((size, size), Image.LANCZOS)


def save_ico(path, frames):
    frames = sorted(frames, key=lambda f: -f.width)
    frames[0].save(path, format="ICO", append_images=frames[1:], sizes=[(f.width, f.width) for f in frames])


LIME = (200, 240, 60, 255)
WHITE = (255, 255, 255, 255)
GREY = (150, 158, 164, 255)
DARK = (8, 11, 14, 255)


def drawn_tray(size, watching, halo=True):
    """The cat head behind the keyboard, redrawn for small sizes.

    The sheet artwork has more detail than 16 pixels can carry, so the tray
    icon is drawn here on a 16-unit grid: head, dark face, two eyes, the
    keyboard edge, and sparks while catguard is watching. A dark halo keeps
    it visible on a light taskbar.
    """
    ss = 16
    u = size * ss / 16
    img = Image.new("RGBA", (size * ss, size * ss), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    P = lambda *pts: [(x * u, y * u) for x, y in pts]
    body = WHITE if watching else GREY

    d.polygon(P((1.7, 9.0), (2.3, 1.6), (5.6, 4.9)), fill=body)      # left ear
    d.polygon(P((12.3, 9.0), (11.7, 1.6), (8.4, 4.9)), fill=body)    # right ear
    d.ellipse(P((1.6, 3.6), (12.4, 14.4)), fill=body)                # head
    d.ellipse(P((3.2, 6.4), (10.8, 12.6)), fill=DARK)                # face
    eye = LIME if watching else GREY
    d.ellipse(P((4.2, 7.6), (6.4, 9.4)), fill=eye)
    d.ellipse(P((7.6, 7.6), (9.8, 9.4)), fill=eye)
    d.rectangle(P((0, 10.6), (16, 16)), fill=(0, 0, 0, 0))           # cut the head at the keyboard
    d.rounded_rectangle(P((0.6, 10.8), (14.2, 12.6)), radius=0.6 * u, fill=body)
    for x in (1.6, 4.2, 6.8, 9.4):                                   # a row of keys
        d.rounded_rectangle(P((x, 13.6), (x + 2.0, 15.0)), radius=0.3 * u, fill=body)
    d.rounded_rectangle(P((12.0, 13.6), (13.2, 15.0)), radius=0.3 * u, fill=body)
    if watching:
        for a, b in (((13.0, 4.6), (14.6, 2.2)), ((13.8, 6.4), (15.7, 5.0)), ((14.2, 8.2), (15.8, 8.0))):
            d.line(P(a, b), fill=LIME, width=max(1, round(0.95 * u)))

    if halo:
        alpha = img.getchannel("A")
        spread = alpha.filter(ImageFilter.MaxFilter(2 * round(0.55 * u) + 1))
        under = Image.new("RGBA", img.size, DARK)
        under.putalpha(spread.point(lambda v: v * 200 // 255))
        under.alpha_composite(img)
        img = under
    return img.resize((size, size), Image.LANCZOS)


def drawn_ring(size, watching):
    """The round tray icon: a cat head in a ring. The ring carries the state,
    lime while catguard watches and grey while it is paused, because a ring
    is the one thing that still reads at 16 pixels."""
    ss = 16
    u = size * ss / 16
    img = Image.new("RGBA", (size * ss, size * ss), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    P = lambda *pts: [(x * u, y * u) for x, y in pts]
    ring = LIME if watching else GREY
    body = WHITE if watching else GREY

    d.ellipse(P((0.4, 0.4), (15.6, 15.6)), fill=DARK, outline=ring, width=max(1, round(1.5 * u)))
    d.polygon(P((3.9, 9.0), (4.3, 3.6), (7.0, 5.9)), fill=body)      # left ear
    d.polygon(P((12.1, 9.0), (11.7, 3.6), (9.0, 5.9)), fill=body)    # right ear
    d.ellipse(P((3.9, 5.2), (12.1, 12.4)), fill=body)                # head
    eye = LIME if watching else DARK
    if watching:
        d.ellipse(P((4.9, 7.4), (11.1, 11.4)), fill=DARK)            # face
    d.ellipse(P((5.6, 8.3), (7.5, 10.1)), fill=eye)
    d.ellipse(P((8.5, 8.3), (10.4, 10.1)), fill=eye)
    return img.resize((size, size), Image.LANCZOS)


def drawn_app(size):
    plate = on_square(Image.new("RGBA", (4, 4), (0, 0, 0, 0)), size, 0, plate=True)
    inner = drawn_tray(size - 2 * max(1, size // 8), watching=True, halo=False)
    plate.alpha_composite(inner, ((size - inner.width) // 2, (size - inner.height) // 2))
    return plate


art = keyed(APP_ART)

# Optical sizes: the wordmark is only legible from 128 px up. Below that the
# icon is the cat over the keyboard alone, redrawn at taskbar size.
save_ico("assets/catguard.ico",
         [tile(256), tile(128)]
         + [on_square(art, s, s // 12, plate=True) for s in (64, 48)]
         + [drawn_app(s) for s in (32, 24, 20, 16)])
tile(256).save("assets/logo.png")  # for the README
TRAY_SIZES = (48, 32, 24, 20, 16)
save_ico("assets/tray-active.ico", [drawn_ring(s, True) for s in TRAY_SIZES])
save_ico("assets/tray-paused.ico", [drawn_ring(s, False) for s in TRAY_SIZES])

# Contact sheet for a human to look at: every small frame at 1x and at 6x,
# on a dark and on a light taskbar colour.
sheet = Image.new("RGBA", (980, 470), (32, 32, 32, 255))
ImageDraw.Draw(sheet).rectangle((0, 300, 980, 470), fill=(238, 238, 238, 255))
x = 10
for frame in (tile(256), on_square(art, 64, 5, True), on_square(art, 48, 4, True)):
    sheet.alpha_composite(frame.resize((frame.width // 2, frame.height // 2)) if frame.width == 256 else frame, (x, 10))
    x += (128 if frame.width == 256 else frame.width) + 12
for y in (150, 320):
    x = 10
    for frame in (drawn_ring(16, True), drawn_ring(16, False), drawn_ring(24, True), drawn_ring(32, True),
                  drawn_ring(32, False), drawn_app(16), drawn_app(32)):
        sheet.alpha_composite(frame, (x, y))
        sheet.alpha_composite(frame.resize((96, 96), Image.NEAREST), (x + 36, y))
        x += 138
sheet.save(CONTACT_SHEET)
