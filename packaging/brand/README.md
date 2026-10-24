# Brand

This folder holds Luxforge's mark, wordmark and lockups, the app icon, the mixes, the website's
icons and theme, and the cards. They are original project art under GPL-3.0-or-later, like the rest
of the project.

The wordmark and all text are outlines of Schibsted Grotesk 1.100, which is under the SIL Open Font
License (`fonts/OFL.txt`). A logo drawn from outlines is artwork, not Font Software, so it needs no
further notice.

`generate.py` draws every file here from the mark's construction and the font's outlines. Edit the
generator, not its output.

## The mark: Midtone

Two quarter discs stand on one baseline, one on each bottom corner of a square. Each is a histogram
channel rising from one end of the tonal scale: shadows on the left, highlights on the right. Where
the two overlap their light adds. Blue and Chartreuse cover all three channels between them, so the
overlap is white: a pointed arch standing on the tonal axis, like the histogram of a well-exposed
photograph.

- **Construction** (`mark/construction.svg`):
  - Each quarter's radius is 0.94 of the square's side.
  - The mark is 1 wide and 0.94 tall, and the arch's apex is at 0.80.
  - Colour to white is about 45:55.
  - Every shape is an exact circular arc. Nothing uses masks, filters or clip paths, so every
    renderer draws the mark, sips included.
- **On dark grounds** (`mark/mark.svg`): both channels are whole, and the arch is drawn on top in
  Light.
- **On paper** (`mark/mark-paper.svg`): the light is the paper itself.
  - The shoulders are cut away from the arch, in their on-paper fills.
  - A tonal axis closes the arch: 4.5% of the mark's height, split between the two colours.
  - It holds on both #f3f4f5 and the website's warmer #f3f1ec.
- **One colour** (`mark/mark-mono.svg`, `currentColor`): the arch and the two shoulders are parted
  by a constant gap of 4.5% of the side along both arcs.

The primary mark is two colours plus Light. The full palette lives in the mixes.

### Size rule

- **Colour and paper:** use the drawing above 32 px. At 32 px and below, use the pixel cuts:
  `mark-32.svg` and `mark-paper-32.svg` from 17 to 32 px, and the `-16` files at 16 px.
  - The square sits on whole pixels.
  - The radius is chosen so the apex falls on a pixel row.
- **One colour:** use the gapped mark at 32 px and up. `mark-mono-32.svg` is its pixel cut, with
  2 px gaps.
- **Below 32 px,** use the filled-notch cut, `mark-mono-16.svg`. Its notch is filled, so the two
  1 px gaps meet at the apex as one arch line instead of crossing into an X.

## The wordmark

`luxforge` in lower case, Schibsted Grotesk SemiBold, tracked in by 2% of the size. Its pointed
joins and flat terminals sit with the arch.

- Use the name **Luxforge**, capitalized, in running text.
- The wordmark is always the drawn file (`wordmark/`), never retyped.
- `-light` is for dark grounds, `-ink` for light ones, and `-mono` is `currentColor`.

## Lockups

- **Horizontal** (`lockup/lockup-horizontal-*.svg`):
  - The mark stands on the word's baseline, 1.22 times the height of the l, because the notch
    lightens its top.
  - The gap between mark and word is 0.62 of an x-height.
- **Stacked** (`lockup/lockup-stacked-*.svg`): the mark is 42% of the word's width, centred, 0.55
  of an x-height above the word.
- **Versions:** `-dark` is the colour mark with bright text, `-paper` is the paper mark with ink
  text, and `-mono` is all `currentColor`.

Keep clear space of a quarter of the mark's width on every side. Do not set the horizontal lockup
smaller than 20 px tall.

## Colour

The colours are the Develop histogram's: its red, green and blue fills and the colours their
overlaps make, as they appear on screen over the editor's canvas.

- **Fill on paper:** the base colour deepened 8–12% towards Ink. Use it for shapes, such as the
  paper mark.
- **Text on paper:** the least change towards Ink that reaches at least 4.5:1 on both #f3f1ec and
  #f3f4f5.
- **Text on dark:** the base colour, lightened towards Bright only where it falls below 4.5:1 on
  Canvas (#19191b).

| Name | Base | Fill on paper | Text on paper | On #f3f1ec | On #f3f4f5 | Text on dark | On #19191b |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Red | `#b64742` | `#a9433f` | `#b64742` | 4.69:1 | 4.80:1 | `#c26965` | 4.60:1 (base 3.32:1) |
| Chartreuse | `#9dc570` | `#90b468` | `#5e7448` | 4.57:1 | 4.69:1 | `#9dc570` | 8.91:1 |
| Green | `#67c26a` | `#5dae61` | `#457948` | 4.56:1 | 4.67:1 | `#67c26a` | 7.94:1 |
| Cyan | `#64adc8` | `#5c9eb7` | `#467385` | 4.59:1 | 4.71:1 | `#64adc8` | 6.98:1 |
| Blue | `#4a75bf` | `#466eb2` | `#456db0` | 4.57:1 | 4.69:1 | `#5c83c5` | 4.60:1 (base 3.84:1) |
| Violet | `#8e7cc2` | `#8474b5` | `#73659c` | 4.57:1 | 4.68:1 | `#8e7cc2` | 4.86:1 |
| Overlap | `#81aec9` | `#81aec9` | `#567082` | 4.61:1 | 4.73:1 | `#81aec9` | 7.38:1 |

| Neutral | Hex | Use |
| --- | --- | --- |
| Light | `#eef5f9` | The arch: all three channels |
| Canvas | `#19191b` | The darkest ground, the editor's canvas |
| Panel | `#202023` | Secondary dark ground |
| Bright | `#f0f0f2` | Text on dark grounds (15.4:1 on Canvas) |
| Muted | `#a8a8ae` | Secondary text on dark grounds (7.4:1) |
| Paper | `#f3f4f5` | Light ground |
| Ink | `#17181b` | Text on light grounds (16.1:1 on Paper) |

The website keeps its own paper (`#f3f1ec`) and ink (`#1a1916`). `web/brand.css` and
`web/tokens.json` carry every value above, with the computed contrast ratios in the tokens.

## Mixes

Any two colours can stand in the mark's corners, and the rule decides the arch: their light adds.
Name a mix plainly by its pair, left colour first (`red-cyan`), in file names and CSS. Do not give
mixes other names.

1. **House mix: Blue and Chartreuse, with the Light arch.** It is always used for identity: the app
   icon, the favicon, the website's header and footer lockup, the docs and the README banner. It is
   never remixed.
2. **Section mixes.** On the website, each section gets one white-arch mix. It is used for the
   section's small mark and for its accent: rules, key numbers and tabs, in the accent colour's
   text-on-paper value.

   | Section | Mix | Accent |
   | --- | --- | --- |
   | First screen, Plate I (Develop) | `blue-chartreuse` (house) | Blue |
   | Performance | `red-cyan` | Red |
   | API and agents | `violet-green` | Green |
   | Select | `chartreuse-cyan` | Cyan |
   | Masking | `cyan-violet` | Violet |
   | Corrections | `violet-chartreuse` | Chartreuse |

3. **Not white yet.** The three channel pairs mean unfinished: pre-releases, experiments and the
   website's "In the developer" items. White arches mean shipped.

   | Mix | Arch and accent |
   | --- | --- |
   | `red-green` | Chartreuse |
   | `green-blue` | Cyan |
   | `red-blue` | Violet |

   On paper a coloured arch is filled in its own on-paper colour, because only white light is the
   paper.
4. **Releases.** Each minor release takes the next white mix, in this order, then wraps:

   | Release | Mix |
   | --- | --- |
   | 0.1 | `blue-chartreuse` |
   | 0.2 | `red-cyan` |
   | 0.3 | `violet-green` |
   | 0.4 | `chartreuse-cyan` |
   | 0.5 | `cyan-violet` |
   | 0.6 | `violet-chartreuse` |

5. **Photo mixes.** A photo mix takes its radii and corner colours from a photograph. Use photo
   mixes for per-photo social cards, the website's hero and release art, never for identity. The
   generator does not draw them.

`mixes/mixes.svg` shows all nine mixes with these rules. Each mix has three files:

- `mixes/<pair>.svg`: the mark for dark grounds;
- `mixes/<pair>-paper.svg`: the paper mark;
- `mixes/<pair>-paper-32.svg`: the 32 px paper cut.

## The website

Everything the site needs is in `web/`, ready to copy:

| File | Goes to | Use |
| --- | --- | --- |
| `favicon.svg` | `site/img/favicon.svg` (and `site/favicon.svg`) | Adaptive favicon: the paper cut on light tabs, the colour cut on dark ones (`prefers-color-scheme`). Drawn on the 16 px grid, so it is sharp at 16, 32 and 48 |
| `favicon.ico` | `site/favicon.ico` | 16, 32 and 48 px paper cuts, for browsers without SVG favicons |
| `apple-touch-icon.png` | `site/apple-touch-icon.png` (and `site/img/`) | 180 × 180, opaque full-bleed graphite; iOS rounds it |
| `icon-192.png`, `icon-512.png`, `icon-maskable-512.png` | `site/img/` | Manifest icons. The maskable icon keeps the mark inside the 80% safe circle |
| `site.webmanifest` | `site/site.webmanifest` | Names the icons above at `/img/` |
| `brand.css` | `site/brand.css`, linked before the page's own styles | The palette as `--lf-*` properties, and one `.mix-<pair>` class per mix |
| `tokens.json` | for tools | The same values, the mixes, the release order and the contrast ratios |
| `lockup-header.svg` | `site/img/` | The paper lockup in the site's ink, for the header and footer |
| `sections/<section>.svg`, `sections/<section>-32.svg` | `site/img/sections/` | Each section's mark on paper, drawn and as the 32 px cut |
| `midtone-inline.svg`, `midtone-inline-paper.svg` | pasted into the HTML | One mark for dark grounds and one for paper, coloured by the `.mix-*` class around them |
| `og-default.svg`, `og-default.png` | `site/og-default.png` | 1200 × 630 link preview on the site's paper, in the house mix, for pages without their own image |

Head tags:

```html
<link rel="icon" href="favicon.ico" sizes="32x32">
<link rel="icon" href="img/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="apple-touch-icon.png">
<link rel="manifest" href="site.webmanifest">
<meta name="theme-color" content="#f3f1ec">
<link rel="stylesheet" href="brand.css">
```

A `.mix-<pair>` class sets the following properties:

- `--mix-a`, `--mix-b` and `--mix-arch`: the corners and the arch on dark grounds;
- `--mix-a-fill`, `--mix-b-fill` and `--mix-arch-fill`: the same on paper, where a white arch is
  `none`;
- `--mix-accent` and `--mix-accent-dark`: the accent's text colour on paper and on dark grounds.

`:root` holds the house mix. To remix a section, put the class on the section and paste the
inline mark inside it:

```html
<section id="speed" class="mix-red-cyan">
  <!-- web/midtone-inline-paper.svg, pasted -->
  <svg class="midtone" viewBox="0 0 100 94" role="img" aria-label="Luxforge">…</svg>
  <h2>Fast where you feel it.</h2>
</section>
<style>#speed h2 { border-top: 2px solid var(--mix-accent) }</style>
```

## App icon

`app-icon/app-icon.svg` is the application icon:

- a 1024 canvas with an 824 tile of radius 185, on a graphite gradient;
- the colour mark at 58% of the tile's width, lifted a hair.

`app-icon-32.svg` and `app-icon-16.svg` are pixel cuts for the two smallest sizes, where the scaled
drawing loses the mark. At 16 the mark grows to 10 of the tile's 14 pixels, so both shoulders still
show. `app-icon/png/` holds 1024, 512 and 256 px renders for documents.

The generator also writes the three as `packaging/icon/luxforge.svg`, `luxforge-32.svg` and
`luxforge-16.svg`, the sources of the icon the app bundles; [`packaging/icon`](../icon/README.md)
describes how the shipped sizes are made.

## Cards

- `cards/release-0.1.svg` and `.png`: a 1280 × 640 release splash, drawn in the release's mix with
  the house lockup. `release_splash()` in the generator makes the next one.
- `cards/readme-banner.svg` and `.png`: a 1600 × 400 README header in the house mix.

## Regenerating

From the repository root, on macOS:

```sh
python3 -I packaging/brand/generate.py
```

The generator needs fontTools 4.60.1 and uharfbuzz 0.51.1 (pip, in a virtual environment). It
rasterizes the PNGs with macOS `sips`, the same rasterizer `cargo xtask icon` uses, and writes the
ICO itself. Pass a directory to write somewhere else; only a run in place writes the application
icon's sources. After they change, run `tools/cargo-cached xtask icon` to regenerate the ICNS and
the Linux PNGs.
