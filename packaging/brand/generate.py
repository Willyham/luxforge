"""Luxforge brand pack: the Midtone mark, the wordmark, lockups, the app icon, the mixes, the
website's icons and theme, and the cards. Edit this generator, not its output.

Usage: python3 -I packaging/brand/generate.py [output directory, default: this directory]
Needs fontTools 4.60.1 and uharfbuzz 0.51.1 (pip, in a virtual environment). Rasterizes the PNGs
with macOS sips, the rasterizer `cargo xtask icon` uses, so it runs on macOS.
"""

import json
import re
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.dont_write_bytecode = True  # keep the pack free of __pycache__

from midtone import (  # noqa: E402
    ACCENT, AXIS, BRIGHT, CANVAS, CHANNEL_MIXES, COLOURS, HOUSE, INK, LIGHT, MIXES, MUTED, OVERLAP,
    PANEL, PAPER, R0, SECTIONS, SITE_INK, SITE_PAPER, WHITE_MIXES, Frame, arch, arch_of, colour,
    contrast, draw_colour, draw_mono, draw_paper, f, fill_on_paper, path, quarter, release_mix,
    shoulder, slug, small, svg, text_on_dark, text_on_paper,
)
from typeset import Face  # noqa: E402

OUT = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else HERE
BOLD, MED, REG = Face(600), Face(500), Face(400)
TRACK = -0.02  # the wordmark's tracking, as a fraction of the size
ASC, DESC, XH = BOLD.bounds("l")[3], -BOLD.bounds("g")[1], BOLD.bounds("x")[3]
LOCK_K = 1.22  # lockup: the mark's height over the l's
SITE_INK_2 = "#5f5c55"  # luxforge.app's secondary text
GRAPHITE = ("#2a2a2e", "#121214")  # the app icon tile, top to bottom
SECTION_LABELS = {
    "develop": "First screen, Plate I", "performance": "Performance", "agents": "API and agents",
    "select": "Select", "masking": "Masking", "corrections": "Corrections",
}


def write(rel, text):
    p = OUT / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)
    return p


def label(face, s, x, y, size, fill, track=0.0, opacity=None):
    dd, w = face.text(s, x, y, size, track)
    op = f' fill-opacity="{opacity}"' if opacity else ""
    return f'<path d="{dd}" fill="{fill}"{op}/>', w


def mark_file(body_fn, w=100):
    """A mark on a tight canvas, w wide and R0 × w tall."""
    return svg(w, R0 * w, body_fn(Frame.at(0, 0, w)))


# ---------------------------------------------------------------- wordmark and lockups
def wordmark(x, base, size, fill):
    dd, w = BOLD.text("luxforge", x, base, size, TRACK)
    return path(dd, fill), w


def draw_mark(mode, fr, pair=HOUSE, ink="currentColor"):
    if mode == "dark":
        return draw_colour(fr, pair)
    if mode == "paper":
        return draw_paper(fr, pair)
    return draw_mono(fr, ink)


def lockup(x, base, size, mode, text):
    """The mark stands on the word's baseline, LOCK_K times the l's height, 0.62 x-heights away."""
    mh = LOCK_K * ASC * size
    mw = mh / R0
    m = draw_mark(mode, Frame.at(x, base - mh, mw), ink=text)
    gap = 0.62 * XH * size
    wb, ww = wordmark(x + mw + gap, base, size, text)
    return m + wb, mw + gap + ww, mh


def lockup_svg(mode, text, size=200):
    _, w, mh = lockup(0, 0, size, mode, text)
    top = max(mh, ASC * size)
    body, _, _ = lockup(0, top, size, mode, text)
    return svg(w, top + DESC * size, body)


def stacked_svg(mode, text, size=200):
    """The mark 42% of the word's width, centred, 0.55 x-heights above the l."""
    _, ww = wordmark(0, 0, size, text)
    mw = 0.42 * ww
    m = draw_mark(mode, Frame.at((ww - mw) / 2, 0, mw), ink=text)
    base = R0 * mw + 0.55 * XH * size + ASC * size
    wb, _ = wordmark(0, base, size, text)
    return svg(ww, base + DESC * size, m + wb)


# ---------------------------------------------------------------- app icon
def tile(uid="tile", x=100, y=100, s=824, rx=185):
    top, bottom = GRAPHITE
    k = s / 824
    return (
        f'<defs><linearGradient id="{uid}-g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{top}"/>'
        f'<stop offset="1" stop-color="{bottom}"/></linearGradient>'
        f'<linearGradient id="{uid}-e" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffffff" stop-opacity=".10"/>'
        f'<stop offset=".1" stop-color="#ffffff" stop-opacity="0"/></linearGradient></defs>'
        f'<rect x="{f(x)}" y="{f(y)}" width="{f(s)}" height="{f(s)}" rx="{f(rx)}" fill="url(#{uid}-g)"/>'
        f'<rect x="{f(x + 0.75 * k)}" y="{f(y + 0.75 * k)}" width="{f(s - 1.5 * k)}" height="{f(s - 1.5 * k)}" rx="{f(rx - 0.75 * k)}" '
        f'fill="url(#{uid}-e)" stroke="#ffffff" stroke-opacity=".08" stroke-width="{f(1.5 * k)}"/>'
    )


def centred_mark(cx, cy, w, pair=HOUSE, lift=0.01):
    """The colour mark centred on (cx, cy), lifted a hair because the notch lightens its top."""
    return draw_colour(Frame.at(cx - w / 2, cy - R0 * w / 2 - lift * w, w), pair)


def app_icon():
    return svg(1024, 1024, tile() + centred_mark(512, 512, 0.58 * 824))


def app_icon_small(size):
    """The 16 and 32 px app icons, cut on the pixel grid. At 16 the tile takes the whole canvas
    less a pixel and the mark grows to 10 of its 14 pixels, so the shoulders still show."""
    if size == 16:
        return svg(16, 16, tile("t16", 1, 1, 14, 3) + draw_colour(Frame.snapped(3, 13, 10)))
    return svg(32, 32, tile("t32", 3, 3, 26, 6) + draw_colour(Frame.snapped(8, 23, 16)))


def full_bleed(size, fill):
    """An opaque graphite square with the mark at `fill` of its width: the touch and maskable icons."""
    top, bottom = GRAPHITE
    bg = (f'<defs><linearGradient id="fb" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{top}"/>'
          f'<stop offset="1" stop-color="{bottom}"/></linearGradient></defs><rect width="{size}" height="{size}" fill="url(#fb)"/>')
    return svg(size, size, bg + centred_mark(size / 2, size / 2, fill * size))


# ---------------------------------------------------------------- construction
def construction():
    W = H = 640
    o, s = 120, 400
    fr = Frame.at(o, (H - R0 * s) / 2 - 20, s)
    b, r = fr.base, fr.r
    ln = f'stroke="{MUTED}" stroke-width="1.2" fill="none" stroke-opacity=".7"'
    body = f'<rect width="{W}" height="{H}" fill="{CANVAS}"/>' + draw_colour(fr)
    body += f'<rect x="{o}" y="{f(b - r)}" width="{s}" height="{f(r)}" {ln}/>'
    for cx in (o, o + s):
        body += f'<circle cx="{cx}" cy="{f(b)}" r="{f(r)}" {ln} stroke-dasharray="4 6"/>'
    ax, ay = fr.apex()
    body += f'<path d="M{f(o + s - r)},{f(b)}L{f(ax)},{f(ay)}L{f(o + r)},{f(b)}" {ln}/>'
    for cx, cy in ((o, b), (o + s, b), (ax, ay)):
        body += f'<circle cx="{f(cx)}" cy="{f(cy)}" r="4" fill="{BRIGHT}"/>'
    t, _ = label(MED, "SHADOWS", o, b + 34, 13, MUTED, 0.14)
    body += t
    tw = MED.width("HIGHLIGHTS", 13, 0.14)
    t, _ = label(MED, "HIGHLIGHTS", o + s - tw, b + 34, 13, MUTED, 0.14)
    body += t
    cap = f"radius {R0:.2f} of the side · apex {(b - ay) / s:.2f}"
    t, _ = label(MED, cap, o + s / 2 - MED.width(cap, 13, 0.04) / 2, b + 64, 13, MUTED, 0.04)
    body += t
    return svg(W, H, body)


# ---------------------------------------------------------------- the mixes sheet
def mixes_sheet():
    W, cw, gap, x0 = 1640, 240, 20, 60
    th = 190  # tile height
    body = f'<rect width="{W}" height="1360" fill="{CANVAS}"/>'
    t, _ = label(BOLD, "Midtone mixes", x0, 96, 44, BRIGHT, -0.02)
    body += t
    rules = [
        "Any two colours can stand in the corners; their light adds, and the arch is the mix.",
        "A white arch means shipped. Blue and Chartreuse is the house mix, for identity, and is never remixed.",
        "The white mixes mark the website's sections and, in this order, the minor releases. A coloured arch means not white yet: unfinished.",
    ]
    for i, line in enumerate(rules):
        t, _ = label(REG, line, x0, 140 + i * 28, 18, MUTED)
        body += t

    def cell(x, y, pair, lines):
        out = f'<rect x="{x}" y="{y}" width="{cw}" height="{th}" rx="6" fill="{PANEL}"/>'
        out += f'<rect x="{x}" y="{y + th + 8}" width="{cw}" height="{th}" rx="6" fill="{SITE_PAPER}"/>'
        w = 128
        out += draw_colour(Frame.at(x + (cw - w) / 2, y + (th - R0 * w) / 2, w), pair)
        out += draw_paper(Frame.at(x + (cw - w) / 2, y + th + 8 + (th - R0 * w) / 2, w), pair)
        ty = y + 2 * th + 40
        t, _ = label(MED, slug(pair), x, ty, 16, BRIGHT)
        out += t
        for k, line in enumerate(lines):
            t, _ = label(REG, line, x, ty + 24 + k * 21, 14, MUTED)
            out += t
        return out

    def heading(s, y):
        return label(MED, s.upper(), x0, y, 13, MUTED, 0.14)[0]

    body += heading("White arch: shipped", 270)
    by_pair = {p: n for n, p in SECTIONS.items()}
    for i, pair in enumerate(WHITE_MIXES):
        lines = [("House · " if pair == HOUSE else "") + f"Release 0.{i + 1}", SECTION_LABELS[by_pair[pair]]]
        body += cell(x0 + i * (cw + gap), 290, pair, lines)
    body += heading("Not white yet: unfinished", 820)
    for i, pair in enumerate(CHANNEL_MIXES):
        lines = [f"{arch_of(*pair).capitalize()} arch", "Pre-releases, experiments"]
        body += cell(x0 + i * (cw + gap), 840, pair, lines)
    note = ("Photo mixes take their radii and corners from a photograph.", "They are for per-photo cards, the site hero and",
            "release art, never for identity.")
    for i, line in enumerate(note):
        t, _ = label(REG, line, x0 + 3 * (cw + gap) + 20, 880 + i * 24, 16, MUTED)
        body += t
    return svg(W, 1360, body)


# ---------------------------------------------------------------- web
def favicon_svg():
    """Adaptive: the paper cut on light tabs, the colour cut on dark ones. Drawn on the 16 px grid,
    so it also lands on whole pixels at 32 and 48."""
    style = ("<style>.d{display:none}@media (prefers-color-scheme:dark){.l{display:none}.d{display:inline}}</style>")
    body = f'<g class="l">{small(16, "paper")}</g><g class="d">{small(16, "colour")}</g>'
    return svg(16, 16, body, head=style)


def inline_svgs():
    """One Midtone each for dark grounds and for paper, coloured by the --mix-* properties that
    brand.css sets for each .mix-* class, with the house mix as the fallback."""
    fr = Frame.at(0, 0, 100)
    a, b = HOUSE

    def st(var, fallback, stroke=False):
        s = f"fill:var({var},{fallback})"
        if stroke:
            s += f";stroke:var({var},{fallback});stroke-width:.4"
        return f' style="{s}"'

    head = '<svg class="midtone" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 94" role="img" aria-label="Luxforge">'
    dark = (head + path(quarter(fr, "l"), "", st("--mix-a", COLOURS[a])).replace(' fill=""', "")
            + path(quarter(fr, "r"), "", st("--mix-b", COLOURS[b])).replace(' fill=""', "")
            + path(arch(fr), "", st("--mix-arch", LIGHT)).replace(' fill=""', "") + "</svg>\n")
    th, half = AXIS * fr.r, 50
    pa, pb = fill_on_paper(a), fill_on_paper(b)
    paper = (head
             + f'<path d="M0,{f(fr.base - th)}h{half}v{f(th)}h-{half}Z"{st("--mix-a-fill", pa)}/>'
             + f'<path d="M{half},{f(fr.base - th)}h{half}v{f(th)}h-{half}Z"{st("--mix-b-fill", pb)}/>'
             + f'<path d="{shoulder(fr, "l")}"{st("--mix-a-fill", pa)}/>'
             + f'<path d="{shoulder(fr, "r")}"{st("--mix-b-fill", pb)}/>'
             + f'<path d="{arch(fr)}"{st("--mix-arch-fill", "none", stroke=True)}/>'
             + "</svg>\n")
    return dark, paper


NAMES = list(COLOURS) + ["overlap"]


def colour_table():
    rows = {}
    for n in NAMES:
        base = OVERLAP if n == "overlap" else COLOURS[n]
        fill = base if n == "overlap" else fill_on_paper(n)
        tp, td = text_on_paper(n), text_on_dark(n)
        rows[n] = {
            "base": base, "fill_on_paper": fill, "text_on_paper": tp, "text_on_dark": td,
            "ratios": {
                "base_on_canvas": round(contrast(base, CANVAS), 2),
                "text_on_paper_on_site_paper": round(contrast(tp, SITE_PAPER), 2),
                "text_on_paper_on_paper": round(contrast(tp, PAPER), 2),
                "text_on_dark_on_canvas": round(contrast(td, CANVAS), 2),
                "fill_on_paper_on_site_paper": round(contrast(fill, SITE_PAPER), 2),
            },
        }
    return rows


NEUTRALS = {"light": LIGHT, "canvas": CANVAS, "panel": PANEL, "muted": MUTED, "bright": BRIGHT,
            "paper": PAPER, "ink": INK, "site-paper": SITE_PAPER, "site-ink": SITE_INK}


def mix_props(pair):
    a, b = pair
    ar = arch_of(a, b)
    acc = ACCENT[pair]
    return {
        "--mix-a": f"var(--lf-{a})", "--mix-b": f"var(--lf-{b})",
        "--mix-arch": "var(--lf-light)" if ar == "light" else f"var(--lf-{ar})",
        "--mix-a-fill": f"var(--lf-{a}-fill)", "--mix-b-fill": f"var(--lf-{b}-fill)",
        "--mix-arch-fill": "none" if ar == "light" else f"var(--lf-{ar}-fill)",
        "--mix-accent": f"var(--lf-{acc}-text)", "--mix-accent-dark": f"var(--lf-{acc}-text-dark)",
    }


def brand_css(table):
    lines = ["/* Luxforge brand colours and mixes. Generated by packaging/brand/generate.py: edit that, not this. */",
             ":root{"]
    for n, row in table.items():
        lines.append(f"  --lf-{n}:{row['base']}; --lf-{n}-fill:{row['fill_on_paper']}; "
                     f"--lf-{n}-text:{row['text_on_paper']}; --lf-{n}-text-dark:{row['text_on_dark']};")
    lines.append("  " + " ".join(f"--lf-{k}:{v};" for k, v in NEUTRALS.items()))
    lines.append("  /* the house mix, until a .mix-* class says otherwise */")
    lines.append("  " + ";".join(f"{k}:{v}" for k, v in mix_props(HOUSE).items()) + ";")
    lines.append("}")
    for pair in MIXES:
        lines.append(f".mix-{slug(pair)}{{" + ";".join(f"{k}:{v}" for k, v in mix_props(pair).items()) + "}")
    lines.append(".midtone{display:block;width:100%;height:auto}")
    return "\n".join(lines) + "\n"


def tokens(table):
    return {
        "colours": table,
        "neutrals": NEUTRALS,
        "mixes": {
            slug(p): {"a": p[0], "b": p[1], "arch": arch_of(*p), "accent": ACCENT[p],
                      "white": arch_of(*p) == "light", "house": p == HOUSE,
                      "release": (f"0.{WHITE_MIXES.index(p) + 1}" if p in WHITE_MIXES else None),
                      "section": next((n for n, q in SECTIONS.items() if q == p), None)}
            for p in MIXES
        },
        "release_order": [slug(p) for p in WHITE_MIXES],
        "geometry": {"radius": R0, "mono_gap": 0.045, "paper_axis": AXIS},
    }


def manifest():
    return json.dumps({
        "name": "Luxforge", "short_name": "Luxforge", "start_url": "/", "display": "browser",
        "background_color": SITE_PAPER, "theme_color": SITE_PAPER,
        "icons": [
            {"src": "/img/icon-192.png", "sizes": "192x192", "type": "image/png"},
            {"src": "/img/icon-512.png", "sizes": "512x512", "type": "image/png"},
            {"src": "/img/icon-maskable-512.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable"},
        ],
    }, indent=2) + "\n"


# ---------------------------------------------------------------- cards
def og_default():
    W, H = 1200, 630
    body = f'<rect width="{W}" height="{H}" fill="{SITE_PAPER}"/>'
    w = 360
    body += draw_paper(Frame.at(W - 110 - w, (H - R0 * w) / 2, w))
    wb, _ = wordmark(96, 300, 128, SITE_INK)
    body += wb
    for i, line in enumerate(("A fast, free photo editor", "you can take apart.")):
        t, _ = label(REG, line, 100, 380 + i * 46, 38, SITE_INK)
        body += t
    t, _ = label(MED, "OPEN SOURCE  ·  macOS AND LINUX  ·  LUXFORGE.APP", 100, 534, 15, SITE_INK_2, 0.12)
    body += t
    return svg(W, H, body)


def release_splash(version, subtitle):
    W, H = 1280, 640
    pair = release_mix(version)
    body = f'<rect width="{W}" height="{H}" fill="{CANVAS}"/>'
    w = 420
    body += draw_colour(Frame.at(W - 120 - w, (H - R0 * w) / 2, w), pair)
    lk, _, _ = lockup(100, 150, 44, "dark", BRIGHT)
    body += lk  # the lockup is identity, so it stays in the house mix
    t, _ = label(BOLD, version, 94, 400, 200, BRIGHT, -0.03)
    body += t
    t, _ = label(REG, subtitle, 102, 470, 32, BRIGHT, 0, 0.88)
    body += t
    cap = f"{pair[0].upper()} AND {pair[1].upper()}  ·  THE {version} RELEASE MIX"
    t, _ = label(MED, cap, 102, 540, 14, MUTED, 0.14)
    body += t
    return svg(W, H, body)


def readme_banner():
    W, H = 1600, 400
    body = f'<rect width="{W}" height="{H}" fill="{CANVAS}"/>'
    size = 120
    _, w, mh = lockup(0, 0, size, "dark", BRIGHT)
    lk, _, _ = lockup((W - w) / 2, (H + mh) / 2, size, "dark", BRIGHT)  # the mark centred
    return svg(W, H, body + lk)


# ---------------------------------------------------------------- rasters
def raster(svg_path, png_path, w, h=None):
    """Render an SVG at w × h pixels with sips."""
    h = h or w
    src = svg_path.read_text()
    head, rest = src.split(">", 1)
    head = re.sub(r'width="[^"]*"', f'width="{w}"', head, count=1)
    head = re.sub(r'height="[^"]*"', f'height="{h}"', head, count=1)
    with tempfile.TemporaryDirectory() as tmp:
        t = Path(tmp) / "in.svg"
        t.write_text(head + ">" + rest)
        png_path.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["sips", "-s", "format", "png", str(t), "--out", str(png_path)], check=True, capture_output=True)
    return png_path


def ico(pngs):
    """PNG-in-ICO: a directory of (size, PNG bytes) entries."""
    head = struct.pack("<HHH", 0, 1, len(pngs))
    entries, data = b"", b""
    offset = 6 + 16 * len(pngs)
    for size, blob in pngs:
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(blob), offset + len(data))
        data += blob
    return head + entries + data


# ---------------------------------------------------------------- main
def main():
    # the mark
    write("mark/mark.svg", mark_file(draw_colour))
    write("mark/mark-paper.svg", mark_file(draw_paper))
    write("mark/mark-mono.svg", mark_file(draw_mono))
    for size in (32, 16):
        write(f"mark/mark-{size}.svg", svg(size, size, small(size, "colour")))
        write(f"mark/mark-paper-{size}.svg", svg(size, size, small(size, "paper")))
        write(f"mark/mark-mono-{size}.svg", svg(size, size, small(size, "mono")))
    write("mark/construction.svg", construction())

    # the wordmark and lockups
    for name, fill in (("light", BRIGHT), ("ink", INK), ("mono", "currentColor")):
        _, w = wordmark(0, 0, 200, fill)
        write(f"wordmark/wordmark-{name}.svg", svg(w, (ASC + DESC) * 200, wordmark(0, ASC * 200, 200, fill)[0]))
    for mode, text in (("dark", BRIGHT), ("paper", INK), ("mono", "currentColor")):
        write(f"lockup/lockup-horizontal-{mode}.svg", lockup_svg(mode, text))
        write(f"lockup/lockup-stacked-{mode}.svg", stacked_svg(mode, text))

    # the app icon
    icon = write("app-icon/app-icon.svg", app_icon())
    for size in (1024, 512, 256):
        raster(icon, OUT / f"app-icon/png/app-icon-{size}.png", size)
    for size in (32, 16):
        write(f"app-icon/app-icon-{size}.svg", app_icon_small(size))
    if OUT == HERE:  # the application icon's sources; `cargo xtask icon` renders the ICNS and PNGs
        write("../icon/luxforge.svg", app_icon())
        for size in (32, 16):
            write(f"../icon/luxforge-{size}.svg", app_icon_small(size))

    # the mixes
    for pair in MIXES:
        s = slug(pair)
        write(f"mixes/{s}.svg", mark_file(lambda fr, p=pair: draw_colour(fr, p)))
        write(f"mixes/{s}-paper.svg", mark_file(lambda fr, p=pair: draw_paper(fr, p)))
        write(f"mixes/{s}-paper-32.svg", svg(32, 32, small(32, "paper", pair)))
    write("mixes/mixes.svg", mixes_sheet())

    # the website
    write("web/favicon.svg", favicon_svg())
    with tempfile.TemporaryDirectory() as tmp:
        t = Path(tmp)
        (t / "16.svg").write_text(svg(16, 16, small(16, "paper")))
        (t / "32.svg").write_text(svg(32, 32, small(32, "paper")))
        blobs = [(16, raster(t / "16.svg", t / "16.png", 16).read_bytes()),
                 (32, raster(t / "32.svg", t / "32.png", 32).read_bytes()),
                 (48, raster(t / "16.svg", t / "48.png", 48).read_bytes())]
        (OUT / "web/favicon.ico").write_bytes(ico(blobs))
        (t / "touch.svg").write_text(full_bleed(180, 0.58))
        raster(t / "touch.svg", OUT / "web/apple-touch-icon.png", 180)
        (t / "maskable.svg").write_text(full_bleed(512, 0.55))  # inside the 80% safe circle
        raster(t / "maskable.svg", OUT / "web/icon-maskable-512.png", 512)
    raster(icon, OUT / "web/icon-192.png", 192)
    raster(icon, OUT / "web/icon-512.png", 512)
    write("web/site.webmanifest", manifest())
    table = colour_table()
    write("web/brand.css", brand_css(table))
    write("web/tokens.json", json.dumps(tokens(table), indent=2) + "\n")
    dark, paper = inline_svgs()
    write("web/midtone-inline.svg", dark)
    write("web/midtone-inline-paper.svg", paper)
    write("web/lockup-header.svg", lockup_svg("paper", SITE_INK))
    for name, pair in SECTIONS.items():
        write(f"web/sections/{name}.svg", mark_file(lambda fr, p=pair: draw_paper(fr, p)))
        write(f"web/sections/{name}-32.svg", svg(32, 32, small(32, "paper", pair)))
    og = write("web/og-default.svg", og_default())
    raster(og, OUT / "web/og-default.png", 1200, 630)

    # the cards
    rel = write("cards/release-0.1.svg", release_splash("0.1", "A first preview"))
    raster(rel, OUT / "cards/release-0.1.png", 1280, 640)
    ban = write("cards/readme-banner.svg", readme_banner())
    raster(ban, OUT / "cards/readme-banner.png", 1600, 400)

    for n, row in table.items():
        r = row["ratios"]
        print(f"{n:11} {row['base']} fill {row['fill_on_paper']} text {row['text_on_paper']} "
              f"({r['text_on_paper_on_site_paper']}:1 site paper, {r['text_on_paper_on_paper']}:1 paper) "
              f"dark {row['text_on_dark']} ({r['text_on_dark_on_canvas']}:1; base {r['base_on_canvas']}:1)")


if __name__ == "__main__":
    main()
