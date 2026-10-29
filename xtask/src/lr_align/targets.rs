//! The rounds' synthetic targets: already-rendered sRGB pictures both editors read as they are,
//! with no base profile in between, so a variant's change is its setting alone
//! (`docs/design/lightroom-alignment.md`, "Test images").
//!
//! Every target is computed from integers, the one shared sRGB reference and a seeded generator,
//! so the same build writes the same pixels; each target's 8-bit RGB buffer is frozen by its
//! SHA-256 in this module's tests. Each carries its layout, the places a measurement reads, in the
//! round's manifest. Patches are large enough, with an inset margin, that a measurement averages
//! away JPEG quantization and never reads across a patch boundary.
use luxforge_reference::{colour, srgb};
use serde_json::{Value, json};

/// The targets, in manifest order.
pub const TARGETS: [&str; 8] = [
    "wedge",
    "patches",
    "surround",
    "edges",
    "edges-24mp",
    "flat",
    "greys",
    "grid",
];

/// One synthetic target: its 8-bit sRGB pixels, row-major RGB, and its layout.
pub struct Target {
    pub id: &'static str,
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    pub layout: Value,
}

impl Target {
    fn new(id: &'static str, width: u32, height: u32, background: u8) -> Self {
        Self {
            id,
            width,
            height,
            rgb: vec![background; (width * height * 3) as usize],
            layout: Value::Null,
        }
    }

    fn put(&mut self, x: u32, y: u32, code: [u8; 3]) {
        let at = ((y * self.width + x) * 3) as usize;
        self.rgb[at..at + 3].copy_from_slice(&code);
    }

    fn fill(&mut self, (x0, y0, width, height): (u32, u32, u32, u32), code: [u8; 3]) {
        for y in y0..y0 + height {
            for x in x0..x0 + width {
                self.put(x, y, code);
            }
        }
    }
}

/// The target named `id`, or `None` for a name that is not one.
pub fn target(id: &str) -> Option<Target> {
    Some(match id {
        "wedge" => wedge(),
        "patches" => patches(),
        "surround" => surround(),
        "edges" => edges("edges", 2),
        // The edge chart at two and a half times the side: whether a spatial setting's reach
        // follows the picture's size or stays a fixed number of pixels.
        "edges-24mp" => edges("edges-24mp", 5),
        "flat" => flat(),
        "greys" => greys(),
        "grid" => grid(),
        _ => return None,
    })
}

const WEDGE_PATCH: u32 = 48;
const WEDGE_INSET: u32 = 12;

/// A 256-step grey wedge, every 8-bit code once, on a 16 × 16 grid of 48-pixel patches in
/// boustrophedon order, so neighbours along a row differ by one code.
fn wedge() -> Target {
    let side = 16 * WEDGE_PATCH;
    let mut target = Target::new("wedge", side, side, 0);
    let mut patches = Vec::new();
    for code in 0..=255u32 {
        let row = code / 16;
        let column = if row % 2 == 0 {
            code % 16
        } else {
            15 - code % 16
        };
        let (x, y) = (column * WEDGE_PATCH, row * WEDGE_PATCH);
        let value = code as u8;
        target.fill((x, y, WEDGE_PATCH, WEDGE_PATCH), [value; 3]);
        patches.push(json!({"x": x, "y": y, "code": ([value; 3])}));
    }
    target.layout = json!({
        "kind": "patches", "patch": WEDGE_PATCH, "inset": WEDGE_INSET, "patches": patches,
        "describes": "every 8-bit grey code once, 0 to 255, boustrophedon by rows of 16",
    });
    target
}

const GRID_LIGHTNESS: [f64; 5] = [0.3, 0.45, 0.6, 0.75, 0.9];
const GRID_CHROMA: [f64; 4] = [0.03, 0.06, 0.10, 0.14];
const GRID_HUES: u32 = 24;
const GRID_PATCH: u32 = 40;
const GRID_INSET: u32 = 10;

/// The largest chroma at most `chroma`, at Oklab lightness `l` and hue `hue` (radians), whose
/// colour is inside the sRGB gamut, found by bisection; the hue and lightness are kept.
fn in_gamut(l: f64, chroma: f64, hue: f64) -> (f64, [f64; 3]) {
    let rgb = |c: f64| {
        colour::from_oklab(colour::Oklab {
            l,
            a: c * hue.cos(),
            b: c * hue.sin(),
        })
    };
    let inside = |c: f64| rgb(c).iter().all(|v| (0.0..=1.0).contains(v));
    if inside(chroma) {
        return (chroma, rgb(chroma));
    }
    let (mut low, mut high) = (0.0, chroma);
    for _ in 0..48 {
        let middle = 0.5 * (low + high);
        if inside(middle) {
            low = middle;
        } else {
            high = middle;
        }
    }
    (low, rgb(low))
}

/// An Oklab lightness × chroma × hue grid of flat patches: five lightnesses, four chromas and 24
/// hues 15° apart, one row of hues per lightness and chroma. A colour outside the sRGB gamut keeps
/// its lightness and hue at the largest chroma inside it, recorded as clipped.
fn patches() -> Target {
    let rows = (GRID_LIGHTNESS.len() * GRID_CHROMA.len()) as u32;
    let mut target = Target::new("patches", GRID_HUES * GRID_PATCH, rows * GRID_PATCH, 0);
    let mut patches = Vec::new();
    let mut row = 0;
    for l in GRID_LIGHTNESS {
        for chroma in GRID_CHROMA {
            for step in 0..GRID_HUES {
                let degrees = f64::from(step) * 15.0;
                let (reached, rgb) = in_gamut(l, chroma, degrees.to_radians());
                let code = rgb.map(srgb::code);
                let (x, y) = (step * GRID_PATCH, row * GRID_PATCH);
                target.fill((x, y, GRID_PATCH, GRID_PATCH), code);
                patches.push(json!({
                    "x": x, "y": y, "code": code,
                    "oklab": {"l": l, "chroma": chroma, "hue": degrees},
                    "clipped": reached < chroma,
                }));
            }
            row += 1;
        }
    }
    target.layout = json!({
        "kind": "patches", "patch": GRID_PATCH, "inset": GRID_INSET, "patches": patches,
        "describes": "Oklab lightness 0.3..0.9 × chroma 0.03..0.14 × 24 hues; a clipped patch is at the largest in-gamut chroma of its lightness and hue",
    });
    target
}

const SURROUNDS: [u8; 3] = [16, 118, 240];
const SURROUNDED: [u8; 5] = [40, 80, 128, 176, 224];
const CELL: u32 = 320;
const CENTRE: u32 = 80;

/// The same five grey patches on a dark, a mid and a light surround: a pointwise setting renders
/// each patch alike on every surround, a local one does not.
fn surround() -> Target {
    let mut target = Target::new(
        "surround",
        CELL * SURROUNDED.len() as u32,
        CELL * SURROUNDS.len() as u32,
        0,
    );
    let mut patches = Vec::new();
    for (row, surround) in SURROUNDS.iter().enumerate() {
        for (column, value) in SURROUNDED.iter().enumerate() {
            let (x, y) = (column as u32 * CELL, row as u32 * CELL);
            target.fill((x, y, CELL, CELL), [*surround; 3]);
            let offset = (CELL - CENTRE) / 2;
            target.fill((x + offset, y + offset, CENTRE, CENTRE), [*value; 3]);
            patches.push(json!({
                "x": x + offset, "y": y + offset, "code": ([value; 3]), "surround": surround,
                "cell": [x, y, CELL, CELL],
            }));
        }
    }
    target.layout = json!({
        "kind": "patches", "patch": CENTRE, "inset": 16, "patches": patches,
        "describes": "grey patches 40..224 centred in 320-pixel cells of surround 16, 118 and 240",
    });
    target
}

/// SplitMix64: the seeded generator the noise panels draw from, the same on every host.
struct Generator(u64);

impl Generator {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A uniform value in (0, 1].
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) + 1) as f64 / (1u64 << 53) as f64
    }

    /// A standard normal value, by the Box–Muller transform.
    fn normal(&mut self) -> f64 {
        let (u, v) = (self.unit(), self.unit());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

fn round_code(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// The coverage of the half-plane `n · (p − c) > 0` over a pixel, by 4 × 4 supersampling.
fn half_plane(x: u32, y: u32, centre: (f64, f64), normal: (f64, f64)) -> f64 {
    let mut inside = 0;
    for sy in 0..4 {
        for sx in 0..4 {
            let px = f64::from(x) + (f64::from(sx) + 0.5) / 4.0;
            let py = f64::from(y) + (f64::from(sy) + 0.5) / 4.0;
            if normal.0 * (px - centre.0) + normal.1 * (py - centre.1) > 0.0 {
                inside += 1;
            }
        }
    }
    f64::from(inside) / 16.0
}

const EDGE_DARK: u8 = 48;
const EDGE_LIGHT: u8 = 208;
const SOFT_DARK: u8 = 112;
const SOFT_LIGHT: u8 = 144;
const MID: u8 = 128;
const NOISE_SIGMA: f64 = 6.0;
/// Degrees the slanted edge leans from vertical.
const SLANT: f64 = 5.0;

/// The spatial chart, 2400 × 1600 at `scale` 2 and in proportion at others: a zone plate on the
/// left square, from flat at its centre to the Nyquist frequency at its rim, and on the right two
/// columns of panels: vertical and horizontal step edges at high and low contrast, a slanted edge
/// and thin lines, and luminance and colour noise of a known deviation on mid grey.
fn edges(id: &'static str, scale: u32) -> Target {
    // Lengths are in units of half the base chart, so scale 2 is 2400 × 1600 and 5 is 6000 × 4000.
    let unit = |length: u32| length * scale / 2;
    let (width, height) = (unit(2400), unit(1600));
    let mut target = Target::new(id, width, height, MID);
    // The zone plate: phase π r² / (2R) has frequency r / (2R), 0.5 cycles per pixel at r = R.
    let square = unit(1600);
    let radius = f64::from(unit(760));
    let centre = f64::from(square) / 2.0;
    for y in 0..square {
        for x in 0..square {
            let (dx, dy) = (f64::from(x) + 0.5 - centre, f64::from(y) + 0.5 - centre);
            let r2 = dx * dx + dy * dy;
            if r2 <= radius * radius {
                let value =
                    f64::from(MID) + 96.0 * (std::f64::consts::PI * r2 / (2.0 * radius)).cos();
                target.put(x, y, [round_code(value); 3]);
            }
        }
    }
    let panel = unit(400);
    let left = square;
    let panels = [
        ("vertical-edge", 0, 0, EDGE_DARK, EDGE_LIGHT),
        ("horizontal-edge", 1, 0, EDGE_DARK, EDGE_LIGHT),
        ("vertical-edge-soft", 0, 1, SOFT_DARK, SOFT_LIGHT),
        ("horizontal-edge-soft", 1, 1, SOFT_DARK, SOFT_LIGHT),
        ("slanted-edge", 0, 2, EDGE_DARK, EDGE_LIGHT),
    ];
    let mut layout = Vec::new();
    for (name, column, row, dark, light) in panels {
        let (x0, y0) = (left + column * panel, row * panel);
        let middle = (
            f64::from(x0) + f64::from(panel) / 2.0,
            f64::from(y0) + f64::from(panel) / 2.0,
        );
        let normal = match name {
            "horizontal-edge" | "horizontal-edge-soft" => (0.0, 1.0),
            "slanted-edge" => (SLANT.to_radians().cos(), -SLANT.to_radians().sin()),
            _ => (1.0, 0.0),
        };
        for y in y0..y0 + panel {
            for x in x0..x0 + panel {
                let cover = half_plane(x, y, middle, normal);
                let value = f64::from(dark) + cover * f64::from(light - dark);
                target.put(x, y, [round_code(value); 3]);
            }
        }
        layout.push(json!({
            "name": name, "rect": [x0, y0, panel, panel], "dark": dark, "light": light,
            "edge_through": [middle.0, middle.1], "edge_normal": [normal.0, normal.1],
        }));
    }
    // Thin dark lines, one pixel wide at the base scale, every 16 base pixels.
    let (x0, y0) = (left + panel, 2 * panel);
    let (line, spacing) = (unit(1).max(1), unit(16));
    for y in y0..y0 + panel {
        for x in x0..x0 + panel {
            let on = (x - x0) % spacing < line;
            target.put(x, y, [if on { EDGE_DARK } else { MID }; 3]);
        }
    }
    layout.push(json!({
        "name": "lines", "rect": [x0, y0, panel, panel], "dark": EDGE_DARK, "light": MID,
        "line_width": line, "spacing": spacing,
    }));
    // Noise of deviation NOISE_SIGMA codes on mid grey: the same value on every channel, then an
    // independent value per channel.
    let mut generator = Generator(0x4c75_7866_6f72_6765 ^ u64::from(scale));
    for (name, column, chroma) in [("luminance-noise", 0, false), ("colour-noise", 1, true)] {
        let (x0, y0) = (left + column * panel, 3 * panel);
        for y in y0..y0 + panel {
            for x in x0..x0 + panel {
                let code = if chroma {
                    [0; 3].map(|_| round_code(f64::from(MID) + NOISE_SIGMA * generator.normal()))
                } else {
                    [round_code(f64::from(MID) + NOISE_SIGMA * generator.normal()); 3]
                };
                target.put(x, y, code);
            }
        }
        layout.push(json!({
            "name": name, "rect": [x0, y0, panel, panel], "mean": MID, "sigma": NOISE_SIGMA,
            "per_channel": chroma,
        }));
    }
    target.layout = json!({
        "kind": "spatial",
        "zone_plate": {"centre": [centre, centre], "radius": radius, "amplitude": 96, "mean": MID,
                        "phase": "pi * r^2 / (2 * radius), r from the pixel centre",
                        "nyquist_at": "radius"},
        "panels": layout,
        "describes": "zone plate, step edges at high and low contrast, a 5-degree slanted edge, thin lines, and seeded luminance and colour noise",
    });
    target
}

/// A flat mid-grey field at 3:2, for the vignette and the mask falloffs.
fn flat() -> Target {
    let mut target = Target::new("flat", 1500, 1000, MID);
    target.layout = json!({
        "kind": "flat", "code": ([MID; 3]),
        "describes": "one grey code over the whole frame",
    });
    target
}

/// The flat greys' codes: encoded tones of about 0.1, 0.25, 0.4, 0.5, 0.7 and 0.9.
const GREYS: [u8; 6] = [26, 64, 102, 128, 179, 230];
const GREY_CELL: u32 = 640;
const GREY_INSET: u32 = 64;
/// The side of the square windows grain is read in, a power of two for its spectrum: 2 × 2 of them
/// fill each grey's cell inside its inset.
pub const GREY_WINDOW: u32 = 256;

/// Six flat greys in a 3 × 2 grid of 640-pixel cells, for grain: its strength per tone, the size
/// of its features and how unevenly it gathers, each read in windows inset from a cell's edges. A
/// patch layout, so a colour measure reads each grey's interior too.
fn greys() -> Target {
    let columns = 3;
    let mut target = Target::new(
        "greys",
        GREY_CELL * columns,
        GREY_CELL * GREYS.len() as u32 / columns,
        0,
    );
    let mut patches = Vec::new();
    for (index, code) in GREYS.iter().enumerate() {
        let (x, y) = (
            index as u32 % columns * GREY_CELL,
            index as u32 / columns * GREY_CELL,
        );
        target.fill((x, y, GREY_CELL, GREY_CELL), [*code; 3]);
        patches.push(json!({"x": x, "y": y, "code": ([code; 3])}));
    }
    target.layout = json!({
        "kind": "patches", "patch": GREY_CELL, "inset": GREY_INSET, "window": GREY_WINDOW,
        "patches": patches,
        "describes": "six flat greys, codes 26 to 230, in 640-pixel cells; grain is read in 2 x 2 windows of 256 pixels inside each cell's 64-pixel inset",
    });
    target
}

const DOT_SPACING: u32 = 50;
const DOT_RADIUS: f64 = 5.0;
const MARKER_RADIUS: f64 = 9.0;
const DOT: u8 = 30;
const PAPER: u8 = 200;

/// A grid of dark dots on light grey at 3:2, for crop, straighten and perspective: 30 × 20 dots
/// every 50 pixels from 25, each antialiased by 4 × 4 supersampling, and a larger marker dot at
/// the second column of the second row so a reflection or quarter turn is visible.
fn grid() -> Target {
    let (columns, rows) = (30u32, 20u32);
    let mut target = Target::new("grid", columns * DOT_SPACING, rows * DOT_SPACING, PAPER);
    let marker = (1, 1);
    for row in 0..rows {
        for column in 0..columns {
            let radius = if (column, row) == marker {
                MARKER_RADIUS
            } else {
                DOT_RADIUS
            };
            let cx = f64::from(column * DOT_SPACING + DOT_SPACING / 2);
            let cy = f64::from(row * DOT_SPACING + DOT_SPACING / 2);
            let reach = radius.ceil() as u32 + 1;
            let (x0, y0) = (cx as u32 - reach, cy as u32 - reach);
            for y in y0..=cy as u32 + reach {
                for x in x0..=cx as u32 + reach {
                    let mut inside = 0;
                    for sy in 0..4 {
                        for sx in 0..4 {
                            let px = f64::from(x) + (f64::from(sx) + 0.5) / 4.0 - cx;
                            let py = f64::from(y) + (f64::from(sy) + 0.5) / 4.0 - cy;
                            if px * px + py * py <= radius * radius {
                                inside += 1;
                            }
                        }
                    }
                    if inside > 0 {
                        let cover = f64::from(inside) / 16.0;
                        let value = f64::from(PAPER) - cover * f64::from(PAPER - DOT);
                        target.put(x, y, [round_code(value); 3]);
                    }
                }
            }
        }
    }
    target.layout = json!({
        "kind": "dots", "columns": columns, "rows": rows, "spacing": DOT_SPACING,
        "first_centre": [DOT_SPACING / 2, DOT_SPACING / 2], "radius": DOT_RADIUS,
        "marker": {"column": marker.0, "row": marker.1, "radius": MARKER_RADIUS},
        "dot": DOT, "paper": PAPER,
        "describes": "dot centres at (25 + 50 column, 25 + 50 row) in pixel-edge coordinates",
    });
    target
}
