"""The Midtone mark: its colours, its geometry, the pixel cuts and the mixes.

Two quarter discs stand on one baseline, one on each bottom corner of a square, with radius 0.94 of
the square's side. Each is a histogram channel rising from one end of the tonal scale: shadows on
the left, highlights on the right. Where they overlap, their light adds: Blue and Chartreuse cover
all three channels, so the overlap is white, a pointed arch standing on the tonal axis.

Every shape is drawn with exact circular arcs. Nothing uses masks, filters or clip paths, so every
renderer draws it, macOS sips included.
"""

import math

# ---------------------------------------------------------------- colour
# The Develop histogram's fills as they appear on screen: each channel alone and each pair.
COLOURS = {
    "red": "#b64742",
    "chartreuse": "#9dc570",  # red and green
    "green": "#67c26a",
    "cyan": "#64adc8",  # green and blue
    "blue": "#4a75bf",
    "violet": "#8e7cc2",  # red and blue
}
LIGHT = "#eef5f9"  # all three channels: the arch
OVERLAP = "#81aec9"  # the histogram's all-channel fill: the one-colour accent
CANVAS, PANEL, MUTED, BRIGHT = "#19191b", "#202023", "#a8a8ae", "#f0f0f2"
PAPER, INK = "#f3f4f5", "#17181b"
SITE_PAPER, SITE_INK = "#f3f1ec", "#1a1916"  # luxforge.app's paper and ink

CHANNELS = {
    "red": (1, 0, 0), "green": (0, 1, 0), "blue": (0, 0, 1),
    "chartreuse": (1, 1, 0), "cyan": (0, 1, 1), "violet": (1, 0, 1),
}
NAME_OF = {v: k for k, v in CHANNELS.items()}

# On paper the fills deepen towards Ink so the pale ones hold their edge against the paper.
DEEPEN = {"red": 0.08, "chartreuse": 0.10, "green": 0.12, "cyan": 0.10, "blue": 0.08, "violet": 0.08}


def mix(a, b, t):
    pa = [int(a[i:i + 2], 16) for i in (1, 3, 5)]
    pb = [int(b[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * t):02x}" for x, y in zip(pa, pb))


def fill_on_paper(name):
    return mix(COLOURS[name], INK, DEEPEN[name])


def arch_of(a, b):
    """The arch's colour name: the union of the two corners' channels ("light" for all three)."""
    m = tuple(max(x, y) for x, y in zip(CHANNELS[a], CHANNELS[b]))
    return "light" if m == (1, 1, 1) else NAME_OF[m]


def colour(name, paper=False):
    if name == "light":
        return LIGHT
    return fill_on_paper(name) if paper else COLOURS[name]


# ---------------------------------------------------------------- the mixes
# House first, then the release order of the white mixes, then the three channel pairs.
HOUSE = ("blue", "chartreuse")
WHITE_MIXES = [("blue", "chartreuse"), ("red", "cyan"), ("violet", "green"),
               ("chartreuse", "cyan"), ("cyan", "violet"), ("violet", "chartreuse")]
CHANNEL_MIXES = [("red", "green"), ("green", "blue"), ("red", "blue")]
MIXES = WHITE_MIXES + CHANNEL_MIXES
# The website's sections and their mixes. The first screen and Plate I share the house mix.
SECTIONS = {
    "develop": ("blue", "chartreuse"),
    "performance": ("red", "cyan"),
    "agents": ("violet", "green"),
    "select": ("chartreuse", "cyan"),
    "masking": ("cyan", "violet"),
    "corrections": ("violet", "chartreuse"),
}
# The accent each mix lends its section: rules, key numbers and tabs, in its text-on-paper colour.
ACCENT = {
    ("blue", "chartreuse"): "blue", ("red", "cyan"): "red", ("violet", "green"): "green",
    ("chartreuse", "cyan"): "cyan", ("cyan", "violet"): "violet", ("violet", "chartreuse"): "chartreuse",
    ("red", "green"): "chartreuse", ("green", "blue"): "cyan", ("red", "blue"): "violet",
}


def slug(pair):
    return f"{pair[0]}-{pair[1]}"


def release_mix(version):
    """Each minor release takes the next white mix: 0.1 is the first, and the order wraps."""
    minor = int(version.split(".")[1])
    return WHITE_MIXES[(minor - 1) % len(WHITE_MIXES)]


# ---------------------------------------------------------------- numbers and files
def f(v):
    s = f"{v:.2f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def svg(w, h, body, title="Luxforge", head=""):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{f(w)}" height="{f(h)}" viewBox="0 0 {f(w)} {f(h)}" '
        f'role="img" aria-label="{title}"><title>{title}</title>{head}{body}</svg>\n'
    )


def path(dd, fill, extra=""):
    return f'<path d="{dd}" fill="{fill}"{extra}/>'


# ---------------------------------------------------------------- geometry
R0 = 0.94  # quarter radius as a fraction of the square's side
MONO_GAP = 0.045  # the one-colour parting, as a fraction of the side
AXIS = 0.045  # the paper mark's tonal axis, as a fraction of the mark's height


class Frame:
    """One mark: left corner x0 on the baseline, side S, quarter radius r."""

    def __init__(self, x0, base, S, r):
        self.x0, self.base, self.S, self.r = x0, base, S, r

    @classmethod
    def at(cls, x, y, w):
        """A mark w wide with its top-left at (x, y); it is R0 × w tall."""
        return cls(x, y + R0 * w, w, R0 * w)

    @classmethod
    def snapped(cls, x0, base, S):
        """A mark on the pixel grid: the square on whole pixels, and the radius chosen so the apex
        falls on a pixel row; the shoulder tops then land within a fraction of a pixel of one."""
        half = S / 2
        h = round(math.sqrt((R0 * S) ** 2 - half * half))
        return cls(x0, base, S, math.sqrt(half * half + h * h))

    @classmethod
    def pixels(cls, size):
        """The 16 and 32 px cuts."""
        return cls.snapped(*{16: (1, 15, 14), 32: (2, 29, 28)}[size])

    def apex(self, r=None):
        r = r or self.r
        return self.x0 + self.S / 2, self.base - math.sqrt(r * r - self.S * self.S / 4)


def quarter(fr, side):
    """The quarter disc on the left ("l") or right ("r") corner."""
    x0, b, S, r = fr.x0, fr.base, fr.S, fr.r
    if side == "l":
        return f"M{f(x0)},{f(b)}V{f(b - r)}A{f(r)},{f(r)} 0 0 1 {f(x0 + r)},{f(b)}Z"
    xr = x0 + S
    return f"M{f(xr)},{f(b)}H{f(xr - r)}A{f(r)},{f(r)} 0 0 1 {f(xr)},{f(b - r)}Z"


def arch(fr, r=None):
    """The overlap of the two quarters (radius r, by default the mark's): a pointed arch."""
    r = r or fr.r
    x0, b, S = fr.x0, fr.base, fr.S
    ax, ay = fr.apex(r)
    return (f"M{f(x0 + S - r)},{f(b)}A{f(r)},{f(r)} 0 0 1 {f(ax)},{f(ay)}"
            f"A{f(r)},{f(r)} 0 0 1 {f(x0 + r)},{f(b)}Z")


def shoulder(fr, side, h=0.0):
    """A quarter minus the other corner's disc grown by h: the part of a channel outside the
    overlap, parted from it by h."""
    x0, b, S, r = fr.x0, fr.base, fr.S, fr.r
    R = r + h
    px = (r * r - R * R + S * S) / (2 * S)
    py = math.sqrt(r * r - px * px)
    if side == "l":
        return (f"M{f(x0)},{f(b)}V{f(b - r)}A{f(r)},{f(r)} 0 0 1 {f(x0 + px)},{f(b - py)}"
                f"A{f(R)},{f(R)} 0 0 0 {f(x0 + S - R)},{f(b)}Z")
    xr = x0 + S
    return (f"M{f(xr)},{f(b)}V{f(b - r)}A{f(r)},{f(r)} 0 0 0 {f(xr - px)},{f(b - py)}"
            f"A{f(R)},{f(R)} 0 0 1 {f(x0 + R)},{f(b)}Z")


def notch_filled(fr, h):
    """The square silhouette with the notch filled, less the arch grown by h: both shoulders as
    one piece, for the small one-colour cut."""
    x0, b, S, r = fr.x0, fr.base, fr.S, fr.r
    R = r + h
    ax, ay = fr.apex(R)
    return (f"M{f(x0)},{f(b)}V{f(b - r)}H{f(x0 + S)}V{f(b)}H{f(x0 + R)}"
            f"A{f(R)},{f(R)} 0 0 0 {f(ax)},{f(ay)}A{f(R)},{f(R)} 0 0 0 {f(x0 + S - R)},{f(b)}Z")


# ---------------------------------------------------------------- drawings
def draw_colour(fr, pair=HOUSE):
    """On dark grounds: both channels whole, the arch on top in the colour their light makes."""
    a, b = pair
    return (path(quarter(fr, "l"), colour(a)) + path(quarter(fr, "r"), colour(b))
            + path(arch(fr), colour(arch_of(a, b))))


def draw_paper(fr, pair=HOUSE, axis=None):
    """On paper: white light is the paper itself. The shoulders are cut away from the arch and a
    thin tonal axis, split between the two colours, closes it. A coloured arch (a channel pair)
    is filled in its own colour."""
    a, b = pair
    ca, cb = colour(a, True), colour(b, True)
    th = axis if axis is not None else AXIS * fr.r
    half = fr.S / 2
    out = (path(f"M{f(fr.x0)},{f(fr.base - th)}h{f(half)}v{f(th)}h{f(-half)}Z", ca)
           + path(f"M{f(fr.x0 + half)},{f(fr.base - th)}h{f(half)}v{f(th)}h{f(-half)}Z", cb))
    ar = arch_of(a, b)
    if ar == "light":
        return out + path(shoulder(fr, "l"), ca) + path(shoulder(fr, "r"), cb)
    return out + path(quarter(fr, "l"), ca) + path(quarter(fr, "r"), cb) + path(arch(fr), colour(ar, True))


def draw_mono(fr, fill="currentColor", gap=None):
    """One colour: the arch and the two shoulders, parted by a constant gap along both arcs."""
    h = (gap if gap is not None else MONO_GAP * fr.S) / 2
    return path(shoulder(fr, "l", h) + shoulder(fr, "r", h) + arch(fr, fr.r - h), fill)


def draw_mono_filled(fr, fill="currentColor", gap=1.0):
    """The small one-colour cut: the notch filled, so the two partings meet at the apex as one
    arch line instead of crossing into an X."""
    h = gap / 2
    return path(notch_filled(fr, h) + arch(fr, fr.r - h), fill)


def small(size, kind, pair=HOUSE, fill="currentColor"):
    """The pixel cuts: "colour", "paper" or "mono" (gapped at 32, notch filled at 16)."""
    fr = Frame.pixels(size)
    if kind == "colour":
        return draw_colour(fr, pair)
    if kind == "paper":
        return draw_paper(fr, pair, axis=1.0 if size == 16 else 2.0)
    if kind == "mono":
        return draw_mono(fr, fill, gap=2.0) if size == 32 else draw_mono_filled(fr, fill, gap=1.0)
    raise ValueError(kind)


# ---------------------------------------------------------------- contrast
def luminance(hexc):
    def lin(c):
        c /= 255
        return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4

    r, g, b = (int(hexc[i:i + 2], 16) for i in (1, 3, 5))
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)


def contrast(a, b):
    la, lb = sorted((luminance(a), luminance(b)), reverse=True)
    return (la + 0.05) / (lb + 0.05)


def text_variant(base, grounds, toward, target=4.55):
    """The least change towards `toward` that reaches the target contrast on every ground: WCAG's
    4.5:1 with a little margin, so no tool that rounds reports it below."""
    for i in range(101):
        c = mix(base, toward, i / 100)
        if all(contrast(c, g) >= target for g in grounds):
            return c
    raise ValueError(base)


def text_on_paper(name):
    base = OVERLAP if name == "overlap" else COLOURS[name]
    return text_variant(base, (SITE_PAPER, PAPER), INK)


def text_on_dark(name):
    base = OVERLAP if name == "overlap" else COLOURS[name]
    return text_variant(base, (CANVAS,), BRIGHT)
