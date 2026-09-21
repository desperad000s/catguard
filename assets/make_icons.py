"""Renders every icon from one drawing, so that the exe, the tray, the lock
window and the app all show the same cat.

Run from the repo root: python3 assets/make_icons.py [contact-sheet.png]
Needs rsvg-convert and Pillow.

The mark is the cat from variation 08 of assets/src/icon-sheet.png, redrawn
as vectors with the same head the app draws in ui/index.html. The sheet's
bitmap does not survive 16 pixels, and cutting different sizes from different
places is how the exe ended up with three different logos.
"""
import io
import subprocess
import sys

from PIL import Image

CONTACT_SHEET = sys.argv[1] if len(sys.argv) > 1 else "target/icons-contact-sheet.png"
LIME, WHITE, GREY, PLATE = "#c8f03c", "#f2f5f3", "#8f9aa1", "#05070a"

# The head from ui/index.html: 236 wide, 142 high, its chin on y = 0.
HEAD = """
  <path fill="{body}" d="M-118 0C-118-62-96-100-92-142L-46-104C-30-110 30-110 46-104L92-142C96-100 118-62 118 0Z"/>
  <path fill="{plate}" d="M-84 0C-84-40-52-66 0-66 52-66 84-40 84 0Z"/>
  <ellipse fill="{eye}" cx="-36" cy="-26" rx="20" ry="15" transform="rotate(12 -36 -26)"/>
  <ellipse fill="{eye}" cx="36" cy="-26" rx="20" ry="15" transform="rotate(-12 36 -26)"/>
  {pupils}
"""
PUPILS = f'<ellipse fill="{PLATE}" cx="-36" cy="-26" rx="5.5" ry="12"/><ellipse fill="{PLATE}" cx="36" cy="-26" rx="5.5" ry="12"/>'


def app_svg():
    """The cat looks over the keyboard, on the black plate of the logo."""
    keys = "".join(
        f'<rect x="{46 + col * 28 + row * 7}" y="{166 + row * 24}" width="20" height="16" rx="4" fill="{WHITE}"/>'
        for row in range(2) for col in range(6 - row))
    head = HEAD.format(body=WHITE, plate=PLATE, eye=LIME, pupils=PUPILS)
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">
  <rect width="256" height="256" rx="56" fill="{PLATE}"/>
  <g transform="translate(128 150) scale(0.74)">{head}</g>
  <rect x="30" y="148" width="196" height="76" rx="16" fill="{PLATE}" stroke="{WHITE}" stroke-width="7"/>
  {keys}
  <g stroke="{LIME}" stroke-width="8" stroke-linecap="round"><path d="M212 62l14-20M224 90l22-10M226 120l22 2"/></g>
</svg>"""


def tray_svg(watching):
    """The same head in a ring. The ring carries the state."""
    ring, body = (LIME, WHITE) if watching else (GREY, GREY)
    head = HEAD.format(body=body, plate=PLATE, eye=LIME if watching else GREY, pupils=PUPILS if watching else "")
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">
  <circle cx="128" cy="128" r="114" fill="{PLATE}" stroke="{ring}" stroke-width="24"/>
  <g transform="translate(128 184) scale(0.72)">{head}</g>
</svg>"""


def render(svg, size):
    png = subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size)], input=svg.encode(), capture_output=True, check=True).stdout
    return Image.open(io.BytesIO(png)).convert("RGBA")


def save_ico(path, svg, sizes):
    frames = [render(svg, s) for s in sizes]
    frames[0].save(path, format="ICO", append_images=frames[1:], sizes=[(s, s) for s in sizes])


save_ico("assets/catguard.ico", app_svg(), (256, 128, 64, 48, 32, 24, 20, 16))
save_ico("assets/tray-active.ico", tray_svg(True), (48, 32, 24, 20, 16))
save_ico("assets/tray-paused.ico", tray_svg(False), (48, 32, 24, 20, 16))
render(app_svg(), 256).save("assets/logo.png")  # for the README
open("assets/src/icon.svg", "w").write(app_svg())

# Contact sheet for a human to look at: small frames at 1x and magnified, on
# a dark and on a light taskbar colour.
sheet = Image.new("RGBA", (1000, 470), (32, 32, 32, 255))
sheet.paste((238, 238, 238, 255), (0, 300, 1000, 470))
sheet.alpha_composite(render(app_svg(), 128), (10, 10))
for x, size in ((150, 64), (226, 48), (286, 32), (330, 16)):
    sheet.alpha_composite(render(app_svg(), size), (x, 10))
for y in (150, 320):
    x = 10
    for svg, size in ((tray_svg(True), 16), (tray_svg(False), 16), (tray_svg(True), 32), (tray_svg(False), 32), (app_svg(), 16), (app_svg(), 32)):
        frame = render(svg, size)
        sheet.alpha_composite(frame, (x, y))
        sheet.alpha_composite(frame.resize((112, 112), Image.NEAREST), (x + 40, y))
        x += 162
sheet.save(CONTACT_SHEET)
