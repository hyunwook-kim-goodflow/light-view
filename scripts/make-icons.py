"""Draws light-view's icons (logo A: photo frame + highlighter swash) into .ico files.

Run with: python -I scripts/make-icons.py
Writes crates/lv-app/res/light-view.ico (the app) and crates/lv-app/res/photo-file.ico
(photos opened with light-view). Each size is drawn at 8x and scaled down, so small
sizes stay crisp. Colours are the goodflow tokens: paper, ink, lime highlighter.
"""

from pathlib import Path

from PIL import Image, ImageDraw

PAPER = (0xFB, 0xFA, 0xF6, 255)
INK = (0x1C, 0x1B, 0x19, 255)
LIME = (0xD6, 0xF0, 0x4A, 255)
HAIRLINE = (0xC9, 0xC5, 0xBB, 255)
SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
SS = 8  # supersampling


def photo_glyph(d, s, x0, y0, w, h, stroke):
    """A framed photo (mountains + sun) in a w x h box at (x0, y0), units of s."""
    d.rounded_rectangle(
        [x0 * s, y0 * s, (x0 + w) * s, (y0 + h) * s],
        radius=5 * s * w / 38,
        outline=INK,
        width=round(stroke * s),
    )
    px = lambda u: x0 + u * w / 38  # noqa: E731
    py = lambda v: y0 + v * h / 32  # noqa: E731
    d.polygon(
        [(px(4) * s, py(28) * s), (px(15) * s, py(15) * s), (px(22) * s, py(22) * s),
         (px(27) * s, py(17) * s), (px(34) * s, py(28) * s)],
        fill=INK,
    )
    r = 3.5 * w / 38
    d.ellipse([(px(28) - r) * s, (py(8) - r) * s, (px(28) + r) * s, (py(8) + r) * s], fill=INK)


def app_icon(size):
    s = size * SS / 64
    img = Image.new("RGBA", (size * SS, size * SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # Tiny sizes: fill the whole square so the shape survives.
    pad = 1 if size <= 24 else 4
    d.rounded_rectangle([pad * s, pad * s, (64 - pad) * s, (64 - pad) * s], radius=14 * s, fill=PAPER,
                        outline=HAIRLINE if size >= 32 else None, width=max(1, round(0.6 * s)))
    d.polygon([(10 * s, 41 * s), (54 * s, 33 * s), (55 * s, 45 * s), (11 * s, 52 * s)], fill=LIME)
    stroke = 4 if size >= 32 else 5
    photo_glyph(d, s, 13, 15, 38, 32, stroke)
    return img.resize((size, size), Image.LANCZOS)


def file_icon(size):
    """A paper page with a folded corner, the photo glyph and the swash."""
    s = size * SS / 64
    img = Image.new("RGBA", (size * SS, size * SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    fold = 14
    page = [(10 * s, 3 * s), ((54 - fold) * s, 3 * s), (54 * s, (3 + fold) * s), (54 * s, 61 * s), (10 * s, 61 * s)]
    d.polygon(page, fill=PAPER, outline=INK, width=max(1, round(2 * s)))
    d.polygon([((54 - fold) * s, 3 * s), ((54 - fold) * s, (3 + fold) * s), (54 * s, (3 + fold) * s)],
              fill=HAIRLINE, outline=INK, width=max(1, round(2 * s)))
    d.polygon([(13 * s, 44 * s), (51 * s, 38 * s), (52 * s, 49 * s), (14 * s, 54 * s)], fill=LIME)
    photo_glyph(d, s, 16, 21, 32, 27, 3.5 if size >= 32 else 4.5)
    return img.resize((size, size), Image.LANCZOS)


def save_ico(draw, path):
    frames = [draw(n) for n in SIZES]
    path.parent.mkdir(parents=True, exist_ok=True)
    frames[-1].save(path, format="ICO", sizes=[(n, n) for n in SIZES], append_images=frames[:-1])
    frames[-1].save(path.with_suffix(".png"))


if __name__ == "__main__":
    root = Path(__file__).resolve().parent.parent
    save_ico(app_icon, root / "crates/lv-app/res/light-view.ico")
    save_ico(file_icon, root / "crates/lv-app/res/photo-file.ico")
    print("wrote", root / "crates/lv-app/res")
