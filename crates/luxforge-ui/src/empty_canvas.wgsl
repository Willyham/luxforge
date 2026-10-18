// The empty Develop canvas's art: a lens, seen from the front, stopping down through its f-stops,
// with a tunnel of film running away through its aperture, aperture-shaped bokeh drifting behind it
// and the word LUXFORGE under it, which a glint crosses now and then. Every shade is a level on one of two ramps drawn from the theme —
// a neutral one towards the theme's brightest text and one towards its accent — with the canvas's
// own ground at level 2, and each art pixel is an ordered-dithered choice between two levels.
// Ground pixels are left transparent, so the canvas's fill shows through them exactly.

struct Art {
    // The bounds' physical width and height, the physical size of one art pixel, and 1 when the
    // target stores linear light (an sRGB format), 0 when it stores the theme's codes as they are.
    frame: vec4<f32>,
    // The lens's centre, in physical pixels from the bounds' top left, its radius in physical
    // pixels, and the art's time in seconds.
    lens: vec4<f32>,
    // The word's top left, in art pixels from the bounds' top left, and the art pixels a side of
    // one of its font pixels.
    word: vec4<f32>,
    ground: vec4<f32>,
    accent: vec4<f32>,
    ink: vec4<f32>,
    // Each letter's 5×7 bits, in order, two words to a letter: rows 0 to 3, then rows 4 to 6, five
    // bits a row.
    letters: array<vec4<u32>, 4>,
};

@group(0) @binding(0) var<uniform> art: Art;

const TAU: f32 = 6.28318531;
const PI: f32 = 3.14159265;
const BLADES: f32 = 7.0;
// The level the canvas's ground sits at on both ramps.
const GROUND: f32 = 2.0;
// The aperture's radius wide open, at f/1.4, in lens radii.
const WIDE_OPEN: f32 = 0.6;
// The word's letters, and its width in font pixels: five a letter and one between letters.
const LETTERS: u32 = 8u;
const WORD_WIDTH: f32 = 47.0;
// A glint crosses the word once every this many seconds, and takes this many to cross.
const GLINT_EVERY: f32 = 6.0;
const GLINT_TAKES: f32 = 1.4;

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle that covers the viewport, which the toolkit sets to the widget's bounds.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> Varyings {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: Varyings;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

// A level on a ramp: 0 to 7, the ground at 2, and whether it is on the accent's ramp.
struct Shade {
    level: f32,
    accent: f32,
};

fn hash(n: f32) -> f32 {
    return fract(sin(n * 127.1 + 311.7) * 43758.5453);
}

// The 4×4 Bayer threshold of an art pixel, in (0, 1).
fn bayer(q: vec2<u32>) -> f32 {
    let x = q.x & 3u;
    let y = q.y & 3u;
    let a = x ^ y;
    let v = ((a & 1u) << 3u) | ((y & 1u) << 2u) | (a & 2u) | ((y & 2u) >> 1u);
    return (f32(v) + 0.5) / 16.0;
}

// The colour of `level` on its ramp, in the theme's own codes.
fn ramp(level: u32, accent: bool) -> vec3<f32> {
    let ground = art.ground.rgb;
    let hue = select(art.ink.rgb, art.accent.rgb, accent);
    switch level {
        case 0u: { return mix(ground, vec3<f32>(0.0), 0.62); }
        case 1u: { return mix(ground, vec3<f32>(0.0), 0.32); }
        case 2u: { return ground; }
        case 3u: { return mix(ground, hue, 0.24); }
        case 4u: { return mix(ground, hue, 0.48); }
        case 5u: { return mix(ground, hue, 0.74); }
        case 6u: { return hue; }
        default: { return mix(hue, vec3<f32>(1.0), 0.6); }
    }
}

fn to_linear(code: vec3<f32>) -> vec3<f32> {
    let low = code / 12.92;
    let high = pow((code + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, code <= vec3<f32>(0.04045));
}

// How far the iris is stopped down: 0 wide open (f/1.4) to 5 (f/8), holding at each stop and then
// clicking to the next, down and back up again.
fn stop(t: f32) -> f32 {
    let x = t * 0.42;
    let step = floor(x) + smoothstep(0.62, 1.0, fract(x));
    let m = step - 10.0 * floor(step / 10.0);
    return select(m, 10.0 - m, m > 5.0);
}

// The aperture's radius in lens radii: each stop halves its area.
fn aperture(t: f32) -> f32 {
    return WIDE_OPEN * pow(0.5, stop(t) * 0.5);
}

// The blades turn as they close.
fn blade_turn(a: f32) -> f32 {
    return 0.35 + 1.1 * (1.0 - a / WIDE_OPEN);
}

// The signed distance from `v` to an aperture of circumradius `radius` turned by `turn`: a
// heptagon, rounder the wider it is open, as rounded blades make it. Negative inside.
fn aperture_distance(v: vec2<f32>, radius: f32, turn: f32, round: f32) -> f32 {
    let r = length(v);
    let sector = TAU / BLADES;
    let angle = atan2(v.y, v.x) - turn;
    let local = angle - sector * floor(angle / sector) - sector * 0.5;
    let polygon = r * cos(local) - radius * cos(PI / BLADES);
    return mix(polygon, r - radius, round);
}

fn roundness(a: f32) -> f32 {
    return mix(0.1, 0.55, a / WIDE_OPEN);
}

// Whether the word's font pixel at column `f.x`, row `f.y` is lit.
fn letter_lit(f: vec2<f32>) -> bool {
    if f.x < 0.0 || f.y < 0.0 || f.y >= 7.0 || f.x >= WORD_WIDTH {
        return false;
    }
    let letter = u32(f.x) / 6u;
    let column = u32(f.x) % 6u;
    if column >= 5u || letter >= LETTERS {
        return false;
    }
    let pair = art.letters[letter / 2u];
    let row = u32(f.y);
    var bits: u32;
    if letter % 2u == 0u {
        bits = select(pair.x, pair.y, row >= 4u);
    } else {
        bits = select(pair.z, pair.w, row >= 4u);
    }
    let bit = (row % 4u) * 5u + column;
    return ((bits >> bit) & 1u) == 1u;
}

// One frame of the film: a tiny photograph at `uv` (0 to 1 across and along the frame), one of a
// sunset, mountains or a colour chart, chosen by the frame's `id`.
fn photograph(uv: vec2<f32>, id: f32) -> f32 {
    let kind = floor(hash(id) * 3.0);
    if kind < 1.0 {
        // A sun setting over water.
        let horizon = 0.42;
        let sun = length((uv - vec2<f32>(0.62, 0.58)) * vec2<f32>(1.0, 1.4));
        if uv.y > horizon {
            if sun < 0.13 {
                return 7.0;
            }
            return 3.0 + 3.2 * (1.0 - (uv.y - horizon) / (1.0 - horizon)) + 0.9 * exp(-sun * 6.0);
        }
        let ripple = step(0.5, fract(uv.y * 14.0 + sin(uv.x * 9.0 + id)));
        let glitter = select(0.0, 3.0, abs(uv.x - 0.62) < 0.08 + 0.12 * (horizon - uv.y));
        return 2.2 + ripple * (0.8 + glitter);
    }
    if kind < 2.0 {
        // Mountains under a pale sky.
        let ridge = 0.38 + 0.22 * abs(sin(uv.x * 4.0 + id)) + 0.07 * sin(uv.x * 19.0 + id * 3.0);
        if uv.y > ridge {
            return 5.6 - uv.y * 1.2;
        }
        let snow = select(0.0, 3.2, uv.y > ridge - 0.06);
        return 1.6 + snow;
    }
    // A colour chart: four rows of six patches.
    let square = floor(uv * vec2<f32>(6.0, 4.0));
    let inside = fract(uv * vec2<f32>(6.0, 4.0));
    if any(inside < vec2<f32>(0.12)) {
        return 0.8;
    }
    return 1.4 + 5.4 * hash(square.x * 7.0 + square.y * 31.0 + id);
}

// The film lining the tunnel: `across` (0 to 1) runs across one strip, `along` along all of them,
// and `strip` names the strip.
fn film(across: f32, along: f32, strip: f32) -> f32 {
    if across < 0.03 || across > 0.97 {
        return 0.5;
    }
    if across < 0.15 || across > 0.85 {
        let band = select(across - 0.03, across - 0.85, across > 0.5) / 0.12;
        let hole = abs(band - 0.5) < 0.3 && abs(fract(along * 4.0) - 0.5) < 0.22;
        return select(4.6, 0.6, hole);
    }
    if across < 0.17 || across > 0.83 {
        return 4.6;
    }
    let frame = fract(along);
    if frame < 0.07 {
        return 3.4;
    }
    let uv = vec2<f32>((across - 0.17) / 0.66, (frame - 0.07) / 0.93);
    return photograph(uv, floor(along) + strip * 17.0);
}

// What the aperture shows: a tunnel lined with film, rushing away, its far end a light.
fn tunnel(p: vec2<f32>, a: f32, edge: f32, t: f32) -> Shade {
    let drift = vec2<f32>(sin(t * 0.37), cos(t * 0.29)) * 0.12 * a;
    let v = p - drift;
    let rho = max(length(v), 1e-3);
    let depth = 0.3 / rho + t * 0.85;
    let around = atan2(v.y, v.x) / TAU + 0.5 + t * 0.012 + 0.03 * sin(depth * 0.4 + t * 0.3);
    let strips = 5.0;
    let wall = film(fract(around * strips), depth * 0.8, floor(around * strips));
    // Further down the tunnel is darker, and the blades shade the aperture's rim.
    let fog = clamp(rho / WIDE_OPEN * 1.8, 0.0, 1.0);
    var level = mix(0.0, wall, fog * fog);
    level = level * smoothstep(0.0, 0.05, edge) + 0.6 * (1.0 - smoothstep(0.0, 0.05, edge));
    level += 6.0 * exp(-rho * rho / (0.0018 + 0.004 * a));
    return Shade(level, 1.0);
}

// The lens seen from the front, `p` in lens radii from its centre, y up: the barrel's rings, the
// iris and what its aperture shows, and a faint halo around it.
fn lens(p: vec2<f32>, t: f32, behind: Shade) -> Shade {
    let r = length(p);
    if r > 1.3 {
        return behind;
    }
    let toward = p / max(r, 1e-4);
    // Lit from the top left.
    let light = dot(toward, vec2<f32>(-0.55, 0.835));
    if r > 1.0 {
        let glow = 0.7 * exp(-(r - 1.0) * 16.0);
        return Shade(max(behind.level, GROUND) + glow, 1.0);
    }
    let angle = atan2(p.y, p.x);
    // The focus ring turns as focus is pulled, back and forth.
    let focus = 0.4 * sin(t * 0.21);
    if r > 0.985 {
        return Shade(3.6 + 1.7 * light, 0.0);
    }
    if r > 0.93 {
        let ridge = fract((angle + focus) * 108.0 / TAU);
        return Shade(select(2.5, 1.0, ridge < 0.5) + 0.9 * light, 0.0);
    }
    if r > 0.915 {
        // The index mark at the top, over a dark groove.
        if abs(angle - PI * 0.5) < 0.035 {
            return Shade(6.2, 1.0);
        }
        return Shade(0.5, 0.0);
    }
    if r > 0.8 {
        // The distance scale: a tick every 6°, a long one every 30°, turning with the focus ring.
        let u = (angle + focus) * 60.0 / TAU;
        let tick = fract(u) < 0.14;
        let major = fract(u / 5.0) < 0.028;
        if (major && r > 0.82 && r < 0.9) || (tick && r > 0.855 && r < 0.9) {
            return Shade(5.4, 0.0);
        }
        return Shade(1.4 + 0.5 * light, 0.0);
    }
    if r > 0.785 {
        // The ring in the theme's accent that marks a fine lens.
        return Shade(5.6 + 0.8 * light, 1.0);
    }
    if r > 0.74 {
        // The inner bevel, sloping inwards, so lit on the side away from the light.
        return Shade(2.4 - 1.3 * light, 0.0);
    }
    // The iris.
    let a = aperture(t);
    let turn = blade_turn(a);
    let d = aperture_distance(p, a, turn, roundness(a));
    if d < 0.0 {
        return tunnel(p, a, -d, t);
    }
    // Each blade's visible edge carries on straight from one side of the aperture, so the edge
    // leaving the aperture at radius `r` has turned `acos(apothem / r)` from that side's middle.
    let sector = TAU / BLADES;
    let apothem = a * cos(PI / BLADES);
    let swept = acos(clamp(apothem / max(r, apothem), -1.0, 1.0));
    let w = (angle - turn - swept + sector * 0.5) / sector;
    let face = fract(w);
    var level = 1.3 + 1.2 * (1.0 - face) + 0.35 * light;
    // The edge of the blade above catches the light, a constant width whatever the radius.
    if face * sector * r < 0.014 {
        level = 4.3;
    }
    // The aperture's cut edge.
    if d < 0.016 {
        level = 4.8;
    }
    // The barrel's lip shades the blades' outer ends.
    level = level - 1.1 * smoothstep(0.6, 0.74, r);
    return Shade(level, 0.0);
}

// Out-of-focus highlights drifting up behind the lens, each the aperture's own shape at the
// aperture's own turn and growing and shrinking with it, as bokeh does.
fn bokeh(c: vec2<f32>, t: f32) -> Shade {
    let size = art.frame.xy;
    let radius = art.lens.z;
    let a = aperture(t);
    let turn = blade_turn(a);
    let scale = mix(0.35, 1.0, a / WIDE_OPEN);
    var level = GROUND;
    for (var i = 0; i < 18; i = i + 1) {
        let fi = f32(i);
        let h1 = hash(fi);
        let h2 = hash(fi + 17.3);
        let h3 = hash(fi + 41.1);
        let rise = 0.006 + 0.012 * h3;
        let centre = vec2<f32>(
            (h1 + 0.02 * sin(t * 0.3 + fi)) * size.x,
            (1.15 - fract(h2 + t * rise) * 1.3) * size.y,
        );
        let disc = (0.08 + 0.22 * h3) * radius * scale;
        let v = (c - centre) * vec2<f32>(1.0, -1.0);
        if length(v) > disc * 1.1 {
            continue;
        }
        let d = aperture_distance(v, disc, turn, roundness(a));
        if d < 0.0 {
            // Brighter at the rim, as a real highlight's onion ring is.
            let rim = smoothstep(-0.25 * disc, 0.0, d);
            level += (0.4 + 0.55 * rim) * (0.55 + 0.45 * h1);
        }
    }
    return Shade(level, 1.0);
}

// The word under the lens, chrome from the ink above to the accent below, over a drop
// shadow, with a glint leaning like `/` crossing it every GLINT_EVERY seconds. `q` is the art
// pixel.
fn wordmark(q: vec2<f32>, t: f32, behind: Shade) -> Shade {
    let f = floor((q - art.word.xy) / art.word.z);
    if letter_lit(f) {
        // Chrome: the ink darkening down the top three rows, then the accent below them.
        var lit = Shade(5.6 - f.y * 0.4, 0.0);
        if f.y >= 3.0 {
            lit = Shade(6.6 - f.y * 0.2, 1.0);
        }
        let sweep = fract(t / GLINT_EVERY) * GLINT_EVERY / GLINT_TAKES;
        if sweep >= 1.0 {
            return lit;
        }
        let across = f.x - (6.0 - f.y) * 0.6;
        let off = abs(across - mix(-8.0, WORD_WIDTH + 4.0, sweep));
        if off < 2.4 {
            return Shade(mix(7.6, lit.level, smoothstep(1.0, 2.4, off)), select(lit.accent, 0.0, off < 1.6));
        }
        return lit;
    }
    if letter_lit(f - 1.0) {
        return Shade(0.2, 0.0);
    }
    return behind;
}

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4<f32> {
    let size = art.frame.xy;
    let cell = art.frame.z;
    let t = art.lens.w;
    let at = in.uv * size;
    let q = floor(at / cell);
    let c = (q + 0.5) * cell;
    let p = (c - art.lens.xy) / art.lens.z * vec2<f32>(1.0, -1.0);
    var shade = bokeh(c, t);
    shade = lens(p, t, shade);
    shade = wordmark(q, t, shade);
    // The art fades into the ground towards the bounds' edges.
    let edge = min(min(c.x, size.x - c.x), min(c.y, size.y - c.y)) / (12.0 * cell);
    let level = mix(GROUND, shade.level, clamp(edge, 0.0, 1.0));
    let chosen = u32(clamp(floor(level + bayer(vec2<u32>(q))), 0.0, 7.0));
    if chosen == 2u {
        return vec4<f32>(0.0);
    }
    var colour = ramp(chosen, shade.accent > 0.5);
    // A scanline across the bottom quarter of each art pixel's row, where an art pixel is large
    // enough to hold one.
    if cell >= 4.0 && fract(at.y / cell) >= 0.75 {
        colour = colour * 0.86;
    }
    if art.frame.w > 0.5 {
        colour = to_linear(colour);
    }
    return vec4<f32>(colour, 1.0);
}
