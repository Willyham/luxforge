"""Outlined type: a static instance cut from the variable font, shaped with HarfBuzz and drawn as
SVG path data, so no file needs the font installed."""

import tempfile
from pathlib import Path

import uharfbuzz as hb
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

FONTS = Path(__file__).resolve().parent / "fonts"
SOURCE = "SchibstedGrotesk[wght].ttf"
CACHE = Path(tempfile.gettempdir()) / "luxforge-brand-fonts"


def instance(wght):
    CACHE.mkdir(exist_ok=True)
    out = CACHE / f"SchibstedGrotesk-{wght}.ttf"
    if not out.exists():
        font = TTFont(FONTS / SOURCE)
        instantiateVariableFont(font, {"wght": wght}, inplace=True)
        font.save(out)
    return out


class Face:
    def __init__(self, wght):
        p = instance(wght)
        self.tt = TTFont(p)
        self.glyphs = self.tt.getGlyphSet()
        self.upem = self.tt["head"].unitsPerEm
        self.hb = hb.Font(hb.Face(hb.Blob.from_file_path(str(p))))
        self.order = self.tt.getGlyphOrder()
        self.cmap = self.tt.getBestCmap()

    def text(self, s, x, y, size, tracking=0.0):
        """Path data for s with its baseline at y, and its advance width."""
        buf = hb.Buffer()
        buf.add_str(s)
        buf.guess_segment_properties()
        hb.shape(self.hb, buf, {"kern": True, "liga": True})
        k = size / self.upem
        out, cx = [], x
        for info, pos in zip(buf.glyph_infos, buf.glyph_positions):
            pen = SVGPathPen(self.glyphs, ntos=lambda v: f"{v:.2f}".rstrip("0").rstrip("."))
            self.glyphs[self.order[info.codepoint]].draw(
                TransformPen(pen, (k, 0, 0, -k, cx + pos.x_offset * k, y - pos.y_offset * k)))
            out.append(pen.getCommands())
            cx += pos.x_advance * k + tracking * size
        return "".join(out), cx - x - tracking * size

    def width(self, s, size, tracking=0.0):
        return self.text(s, 0, 0, size, tracking)[1]

    def bounds(self, ch):
        b = BoundsPen(self.glyphs)
        self.glyphs[self.cmap[ord(ch)]].draw(b)
        return [v / self.upem for v in b.bounds]
