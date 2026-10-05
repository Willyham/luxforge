#!/usr/bin/env python3
"""Writes the expected values of Luxforge's Omarchy palette reader (`luxforge_core::theme::omarchy`).

Omarchy's resolver, `bin/omarchy-theme-color`, is a Bash 4 script (associative arrays) that runs
its colour mixes through awk; `bin/omarchy-theme-colors-from-alacritty` reads an `alacritty.toml`
through awk. The owner's Mac has Bash 3.2 only, so this script transcribes the Bash statements of
both, in their order, and runs the two scripts' own awk programs, cut from the scripts' text at the
pinned commit, through the system awk: every mix's rounding and every `alacritty.toml` parse is
Omarchy's own code. Bash's semantics are kept where they matter: an empty value is an unset one,
`${a:-b}` falls back on empty, and a line is split by `read` at its first `=`.

    python3 tools/omarchy_theme_parity.py OMARCHY_CLONE OUTPUT_JSON

reads the 22 built-in themes from OMARCHY_CLONE/themes (a clone checked out at the pinned commit)
and the synthetic fixtures beside the reader, and writes the fixture the reader's parity tests
compare against: for each theme the resolved value of every key `omarchy-theme-color --all` prints.
"""

import json
import os
import re
import subprocess
import sys

COMMIT = "035ce29f03bdd97a09af80ef5f2d22d7a98930d6"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TESTDATA = os.path.join(ROOT, "crates/luxforge-core/src/theme/omarchy/testdata")
# Every colour key `omarchy-theme-color --all` prints, in its order (`LC_ALL=C sort`). Beside these
# it prints `mode`, `theme_type` and any key it does not know.
KEYS = sorted(
    ["accent", "cursor", "muted", "selection", "selection_background", "selection_foreground",
     "orange", "brown", "purple", "bright_purple"]
    + [f"color{i}" for i in range(16)]
    + [p + n for p in ["", "bright_"] for n in ["red", "green", "yellow", "blue", "magenta", "cyan"]]
    + ["background", "dark_background", "darker_background", "lighter_background"]
    + ["bg", "dark_bg", "darker_bg", "lighter_bg"]
    + ["foreground", "dark_foreground", "light_foreground", "bright_foreground"]
    + ["fg", "dark_fg", "light_fg", "bright_fg"],
    key=str.encode,
)
BUNDLED = ["tokyo-night", "catppuccin", "catppuccin-latte", "gruvbox", "nord", "everforest"]


def awk_program(script, opening, closing):
    """The single-quoted awk program between `opening` and `closing` in a Bash script."""
    start = script.index(opening) + len(opening)
    end = script.index(closing, start)
    return script[start:end].replace("'\\''", "'")


class Resolver:
    def __init__(self, clone):
        with open(os.path.join(clone, "bin/omarchy-theme-color")) as f:
            self.mix_program = awk_program(
                f.read(),
                """awk -v start="$start" -v end="$end" -v amount="$amount" '""",
                "\n  '\n",
            )
        with open(os.path.join(clone, "bin/omarchy-theme-colors-from-alacritty")) as f:
            self.alacritty_program = awk_program(f.read(), "done < <(awk '", """' "$ALACRITTY_FILE")""")

    # mix_color. A mix of a missing colour is left missing: Omarchy's awk reads an empty string as
    # an awk-dependent colour (gawk mixes it as -17 per channel, BSD awk as 0), never a theme's.
    def mix(self, start, end, amount):
        if not start:
            return ""
        out = subprocess.run(
            ["awk", "-v", "start=" + start.lstrip("#"), "-v", "end=" + end.lstrip("#"),
             "-v", "amount=" + amount, self.mix_program],
            check=True, capture_output=True, text=True,
        ).stdout
        return out.rstrip("\n")

    # omarchy-theme-colors-from-alacritty: the colors.toml it writes, as text.
    def colors_from_alacritty(self, path):
        out = subprocess.run(["awk", self.alacritty_program, path], check=True,
                             capture_output=True, text=True).stdout
        colours = {}
        for line in out.splitlines():
            key, value = line.split("\t")
            colours[key] = value
        names = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"]
        c = {}
        for i, name in enumerate(names):
            c[i] = colours.get("colors.normal." + name, "")
        if any(not c[i] for i in range(8)):
            return None
        for i, name in enumerate(names):
            c[i + 8] = colours.get("colors.bright." + name, "") or c[i]
        background = colours.get("colors.primary.background", "")
        foreground = colours.get("colors.primary.foreground", "")
        selection_background = colours.get("colors.selection.background", "")
        background = background or c[0]
        foreground = foreground or c[7]
        c[0] = background
        c[7] = foreground
        selection_background = selection_background or foreground
        accent = c[4]
        lines = [f'accent = "{accent}"', f'selection = "{selection_background}"', "",
                 f'background = "{background}"', f'foreground = "{foreground}"', ""]
        lines += [f'color{i} = "{c[i]}"' for i in range(16)]
        return "\n".join(lines) + "\n"

    # parse_colors_file
    @staticmethod
    def parse(text):
        colours = {}
        # `read` drops a last line with no newline; every file here ends with one.
        for line in text.split("\n")[:-1]:
            key, _, value = line.partition("=")
            key = re.sub(r"[\"' ]", "", key)
            if not key or key.startswith("#"):
                continue
            if re.search(r"[\"']", value):
                value = re.split(r"[\"']", value, maxsplit=1)[1]
                value = re.split(r"[\"']", value, maxsplit=1)[0]
            else:
                value = value.strip(" \t\n\r\f\v")
            if not re.fullmatch(r"[A-Za-z0-9_-]+", key):
                continue
            if not re.fullmatch(r"[A-Za-z0-9#(),._+/% -]*", value):
                continue
            colours[key] = value
        return colours

    # resolve_theme_colors and resolve_theme_mode
    def resolve(self, text, light_mode):
        t = self.parse(text)
        g = lambda k: t.get(k, "")

        def alias(key, fallback):
            if not g(key):
                t[key] = g(fallback)

        def default(key, value):
            if not g(key):
                t[key] = value

        def first(*keys):
            for k in keys:
                if g(k):
                    return g(k)
            return ""

        legacy_palette_alias = {
            "background": "bg", "dark_background": "dark_bg", "darker_background": "darker_bg",
            "lighter_background": "lighter_bg", "foreground": "fg", "dark_foreground": "dark_fg",
            "light_foreground": "light_fg", "bright_foreground": "bright_fg",
        }
        for key, short in legacy_palette_alias.items():
            alias(key, short)

        default("background", g("color0"))
        default("foreground", g("color7"))
        if g("background"):
            t["color0"] = g("background")
        if g("foreground"):
            t["color7"] = g("foreground")

        legacy_alias = {
            "red": "color1", "green": "color2", "yellow": "color3", "blue": "color4",
            "magenta": "color5", "cyan": "color6", "bright_red": "color9",
            "bright_green": "color10", "bright_yellow": "color11", "bright_blue": "color12",
            "bright_magenta": "color13", "bright_cyan": "color14",
        }
        for key, ansi in legacy_alias.items():
            alias(key, ansi)
        alias("magenta", "purple")
        alias("bright_magenta", "bright_purple")

        default("light_foreground", first("color7", "foreground"))
        default("bright_foreground", first("color15", "foreground"))
        t["cursor"] = g("bright_foreground")
        default("lighter_background", first("color0", "background"))
        default("dark_foreground", first("color8", "foreground"))
        default("muted", first("color8", "dark_foreground"))
        default("selection", first("selection_background", "color8", "color0", "background"))
        default("selection_background", g("selection"))
        default("selection_foreground", g("bright_foreground"))
        default("orange", g("yellow"))
        if not g("brown"):
            t["brown"] = self.mix(g("orange"), "#000000", "50%")

        if not g("dark_background"):
            t["dark_background"] = self.mix(g("background"), "#000000", "25%")
        if not g("darker_background"):
            t["darker_background"] = self.mix(g("background"), "#000000", "50%")
        for name in ["red", "yellow", "green", "cyan", "blue", "magenta"]:
            if not g("bright_" + name):
                t["bright_" + name] = self.mix(g(name), "#ffffff", "20%")
        alias("purple", "magenta")
        alias("bright_purple", "bright_magenta")

        ansi_alias = {
            "color0": "background", "color1": "red", "color2": "green", "color3": "yellow",
            "color4": "blue", "color5": "magenta", "color6": "cyan", "color7": "foreground",
            "color8": "muted", "color9": "bright_red", "color10": "bright_green",
            "color11": "bright_yellow", "color12": "bright_blue", "color13": "bright_magenta",
            "color14": "bright_cyan", "color15": "bright_foreground",
        }
        for key, name in ansi_alias.items():
            alias(key, name)

        for key, short in legacy_palette_alias.items():
            if g(key):
                t[short] = g(key)

        default("mode", g("theme_type"))
        if not g("mode"):
            if light_mode:
                t["mode"] = "light"
            elif re.fullmatch(r"#[0-9A-Fa-f]{6}", g("background")):
                h = g("background")[1:]
                lum = int(h[0:2], 16) + int(h[2:4], 16) + int(h[4:6], 16)
                t["mode"] = "light" if lum > 382 else "dark"
            else:
                t["mode"] = "dark"
        t["theme_type"] = g("mode")
        return dict(sorted(t.items(), key=lambda kv: kv[0].encode()))


def read_theme(resolver, folder):
    colors = os.path.join(folder, "colors.toml")
    alacritty = os.path.join(folder, "alacritty.toml")
    light_mode = os.path.isfile(os.path.join(folder, "light.mode"))
    if os.path.isfile(colors):
        with open(colors) as f:
            text = f.read()
    else:
        text = resolver.colors_from_alacritty(alacritty)
    return text, resolver.resolve(text, light_mode)


def main():
    clone, output = sys.argv[1], sys.argv[2]
    with open(os.path.join(clone, ".git/HEAD")) as f:
        head = f.read().strip()
    if head != COMMIT:
        sys.exit(f"{clone} is at {head}, not the pinned {COMMIT}")
    resolver = Resolver(clone)
    folders = []
    builtin = os.path.join(clone, "themes")
    for slug in sorted(os.listdir(builtin)):
        folders.append((slug, "omarchy", os.path.join(builtin, slug)))
    for name in sorted(os.listdir(TESTDATA)):
        if os.path.isdir(os.path.join(TESTDATA, name)):
            folders.append((name, "synthetic", os.path.join(TESTDATA, name)))
    themes = []
    for slug, source, folder in folders:
        text, resolved = read_theme(resolver, folder)
        resolved = {k: v for k, v in resolved.items() if v}
        mode = resolved.pop("mode")
        assert resolved.pop("theme_type") == mode
        entry = {
            "slug": slug,
            "source": source,
            "mode": mode,
            "values": [resolved.pop(k, None) for k in KEYS],
            # The rest of what `--all` prints: the keys the resolver does not know, verbatim.
            "other": resolved,
        }
        if source == "omarchy" and slug not in BUNDLED:
            # The palette as Omarchy's parser read it, so the test can write a minimal copy.
            entry["palette"] = [f"{k}={v}" for k, v in Resolver.parse(text).items()]
        themes.append(entry)
    with open(output, "w") as f:
        f.write('{"omarchy_commit": "%s",\n"keys": %s,\n"themes": [\n' % (COMMIT, json.dumps(KEYS)))
        f.write(",\n".join(json.dumps(theme) for theme in themes))
        f.write("\n]}\n")


if __name__ == "__main__":
    main()
