"""Draws the website's icons and social preview image from the app icon (scripts/make-icons.py).

Run with: python -I scripts/make-site-images.py
Writes site/static/favicon.ico (16, 32, 48), site/static/img/icon-{180,192,512}.png and
site/static/img/og.png (1200 x 630: paper, logo, name and one line). The preview text uses
Malgun Gothic (Windows); the results are committed, so the build needs no fonts.
"""

import importlib.util
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

sys.dont_write_bytecode = True  # importing make-icons.py must not leave __pycache__
ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("make_icons", ROOT / "scripts" / "make-icons.py")
ICONS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ICONS)

STATIC = ROOT / "site" / "static"
FONT_BOLD = Path("C:/Windows/Fonts/malgunbd.ttf")
FONT = Path("C:/Windows/Fonts/malgun.ttf")


def og_image():
    w, h = 1200, 630
    img = Image.new("RGBA", (w, h), ICONS.PAPER)
    logo = ICONS.app_icon(256)
    img.alpha_composite(logo, (96, (h - 256) // 2))
    d = ImageDraw.Draw(img)
    name = ImageFont.truetype(str(FONT_BOLD), 96)
    line = ImageFont.truetype(str(FONT), 40)
    x = 96 + 256 + 64
    d.text((x, 190), "light-view", font=name, fill=ICONS.INK)
    d.text((x, 330), "아이폰 사진(HEIC)도 바로 여는", font=line, fill=ICONS.INK)
    d.text((x, 390), "가벼운 무료 Windows 사진 뷰어", font=line, fill=ICONS.INK)
    return img.convert("RGB")


def main():
    (STATIC / "img").mkdir(parents=True, exist_ok=True)
    for size in (180, 192, 512):
        ICONS.app_icon(size).save(STATIC / "img" / f"icon-{size}.png")
    ico = [ICONS.app_icon(s) for s in (16, 32, 48)]
    ico[-1].save(STATIC / "favicon.ico", sizes=[(s, s) for s in (16, 32, 48)], append_images=ico[:-1])
    og_image().save(STATIC / "img" / "og.png", optimize=True)


if __name__ == "__main__":
    main()
