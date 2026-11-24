# Draws the app icon: a periodic-table element tile.
#   python make-icon.py <out_dir> [--preview] [--install]
# --install also writes every icon file of the repo (Tauri icons, icon.ico, icon.icns,
# window logo, extension and site icons) from the matching-size renders.
# Writes element-<size>.png for every size below (each size drawn on its own,
# so small sizes drop the fine print instead of shrinking it into noise).
import sys
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

# ---- the element (edit per app) ----
SYMBOL = "Co"
NAME = "Confluo"
NUMBER = "16"
MASS = "1024.00"
TOP = (0xee, 0x8a, 0x4c)
BOTTOM = (0xc2, 0x41, 0x0f)
# ------------------------------------

FONTS = Path("C:/Windows/Fonts")
BOLD = str(FONTS / "segoeuib.ttf")
SEMI = str(FONTS / "seguisb.ttf") if (FONTS / "seguisb.ttf").exists() else BOLD
SIZES = [16, 20, 24, 30, 32, 40, 44, 48, 50, 64, 71, 89, 107, 128, 142, 150, 256, 284, 310, 512, 1024]
SS = 4  # supersampling


def font(path, px):
    return ImageFont.truetype(path, max(1, round(px)))


def text_center(d, cx, cy, s, f, fill):
    l, t, r, b = d.textbbox((0, 0), s, font=f)
    d.text((cx - (l + r) / 2, cy - (t + b) / 2), s, font=f, fill=fill)


def render(size):
    n = size * SS
    small = size < 64
    full = size >= 256
    m = n * (0.03 if small else 0.06)          # outer margin
    w = n - 2 * m
    rad = w * (0.20 if small else 0.22)

    # gradient fill, diagonal
    g = 64
    grad = Image.new("RGBA", (g, g))
    gp = grad.load()
    for y in range(g):
        for x in range(g):
            t = (x + y) / (2 * (g - 1))
            gp[x, y] = tuple(round(TOP[i] + (BOTTOM[i] - TOP[i]) * t) for i in range(3)) + (255,)
    grad = grad.resize((n, n), Image.BICUBIC)
    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle([m, m, n - m, n - m], rad, fill=255)
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    img.paste(grad, (0, 0), mask)

    ov = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(ov)
    white = (255, 255, 255, 255)
    soft = (255, 255, 255, 200)
    cx = n / 2

    if not small:
        # inner tile outline
        i = w * 0.075
        d.rounded_rectangle([m + i, m + i, n - m - i, n - m - i], rad * 0.62,
                            outline=(255, 255, 255, 110), width=max(SS, round(w * 0.010)))

    if small:
        text_center(d, cx, m + w * 0.50, SYMBOL, font(BOLD, w * 0.60), white)
    elif not full:
        fn = font(SEMI, w * 0.15)
        d.text((m + w * 0.15, m + w * 0.13), NUMBER, font=fn, fill=soft)
        text_center(d, cx, m + w * 0.53, SYMBOL, font(BOLD, w * 0.46), white)
        text_center(d, cx, m + w * 0.79, NAME, font(SEMI, w * 0.12), soft)
    else:
        fn = font(SEMI, w * 0.11)
        d.text((m + w * 0.15, m + w * 0.13), NUMBER, font=fn, fill=soft)
        text_center(d, cx, m + w * 0.49, SYMBOL, font(BOLD, w * 0.42), white)
        text_center(d, cx, m + w * 0.725, NAME, font(SEMI, w * 0.095), white)
        text_center(d, cx, m + w * 0.815, MASS, font(SEMI, w * 0.068), (255, 255, 255, 165))

    img = Image.alpha_composite(img, ov)
    return img.resize((size, size), Image.LANCZOS)


def install(out):
    root = Path(__file__).resolve().parent.parent
    src = lambda s: Image.open(out / f"element-{s}.png").convert("RGBA")
    ic = root / "app/src-tauri/icons"
    named = {"32x32": 32, "64x64": 64, "128x128": 128, "128x128@2x": 256, "StoreLogo": 50, "icon": 512}
    for n in (30, 44, 71, 89, 107, 142, 150, 284, 310):
        named[f"Square{n}x{n}Logo"] = n
    for name, s in named.items():
        src(s).save(ic / f"{name}.png")
    ico = [16, 20, 24, 32, 40, 48, 64, 256]
    src(256).save(ic / "icon.ico", sizes=[(s, s) for s in ico],
                  append_images=[src(s) for s in ico if s != 256])
    src(1024).save(ic / "icon.icns", append_images=[src(s) for s in (16, 32, 64, 128, 256, 512)])
    src(1024).save(root / "app/app-icon.png")
    src(64).save(root / "app/src/mark.png")
    for s in (16, 32, 48, 128):
        src(s).save(root / f"extension/icon{s}.png")
    src(128).save(root / "site/icon.png")


def main():
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for s in SIZES:
        render(s).save(out / f"element-{s}.png")
    if "--install" in sys.argv:
        install(out)
    if "--preview" in sys.argv:
        show = [512, 128, 48, 32, 16]
        sheet = Image.new("RGBA", (sum(show) + 20 * len(show), 532), (240, 240, 240, 255))
        x = 10
        for s in show:
            im = Image.open(out / f"element-{s}.png")
            sheet.paste(im, (x, 10), im)
            x += s + 20
        sheet.save(out / "preview.png")


if __name__ == "__main__":
    main()
