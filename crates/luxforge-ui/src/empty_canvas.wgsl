// The empty Develop canvas's art: the stacked Luxforge lockup, the Midtone mark over the wordmark,
// drawn from the brand pack's own construction and outlines (`packaging/brand`). When the art first
// appears the mark's two channels rise from the tonal axis, shadows first, and where they overlap
// their light adds to the arch; the word fades in under them. Then the two channels sway a little,
// out of step, as a histogram does while the exposure moves, and the sway settles as the art comes
// to rest, so a frame at rest is the mark exactly. Every edge is antialiased from its exact
// distance. A pixel the lockup does not cover is left transparent, so the canvas's fill shows
// through it exactly.

struct Art {
    // The bounds' physical width and height, and 1 when the target stores linear light (an sRGB
    // format), 0 when it stores the colours' codes as they are.
    frame: vec4<f32>,
    // The lockup's top left, in physical pixels from the bounds' top left, its physical pixels a
    // lockup unit, and the tonal axis's height in units: the paper drawing's, 0 on dark grounds.
    lockup: vec4<f32>,
    // The mark at rest, in lockup units: its left corner's x, its baseline's y, the square's side
    // and the quarters' radius.
    mark: vec4<f32>,
    // The art's time in seconds, and how much it sways: 0 at rest to 1 at full speed.
    clock: vec4<f32>,
    // The shadows' channel, the highlights' and the arch where they overlap, which is transparent
    // on paper, where the light is the paper itself.
    left: vec4<f32>,
    right: vec4<f32>,
    arch: vec4<f32>,
    word: vec4<f32>,
};

const CONTOURS: u32 = 16u;
const SEGMENTS: u32 = 640u;

// The wordmark's outline in lockup units, flattened to line segments, contour by contour.
struct Word {
    // Each contour's bounds: its least x and y, then its greatest. An unused contour's are empty.
    bounds: array<vec4<f32>, CONTOURS>,
    // Each contour's first segment and its count of segments.
    ranges: array<vec4<u32>, CONTOURS>,
    // Each segment's start and end.
    segments: array<vec4<f32>, SEGMENTS>,
};

@group(0) @binding(0) var<uniform> art: Art;
@group(0) @binding(1) var<uniform> word: Word;

const TAU: f32 = 6.28318531;
// Each channel rises over this many seconds, the highlights this many after the shadows.
const RISE_TAKES: f32 = 1.2;
const RISE_LAG: f32 = 0.16;
// The word fades in from this time, over this many seconds.
const WORD_FROM: f32 = 0.6;
const WORD_TAKES: f32 = 0.9;
// The sway: its share of the radius, its period in seconds, how far the highlights' channel runs
// out of step with the shadows', and the seconds it takes to grow in once the channels have risen.
const SWAY: f32 = 0.035;
const SWAY_PERIOD: f32 = 9.0;
const SWAY_OFFSET: f32 = 2.1;
const SWAY_GROWS: f32 = 2.5;

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

fn to_linear(code: vec3<f32>) -> vec3<f32> {
    let low = code / 12.92;
    let high = pow((code + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, code <= vec3<f32>(0.04045));
}

// A colour as the target stores it.
fn stored(colour: vec3<f32>) -> vec3<f32> {
    if art.frame.z > 0.5 {
        return to_linear(colour);
    }
    return colour;
}

// How far a channel has risen at `t`: 0 to 1, easing out.
fn rise(t: f32) -> f32 {
    let x = clamp(t / RISE_TAKES, 0.0, 1.0);
    return 1.0 - pow(1.0 - x, 3.0);
}

// The coverage of `u` by the quarter disc of `radius` standing on the baseline at `corner`, which
// is the square's left corner when `side` is 1 and its right when it is -1. `px` is a pixel in
// units.
fn quarter(u: vec2<f32>, corner: vec2<f32>, radius: f32, side: f32, px: f32) -> f32 {
    let d = max(max(length(u - corner) - radius, (corner.x - u.x) * side), u.y - corner.y);
    return clamp(0.5 - d / px, 0.0, 1.0) * clamp(radius / px, 0.0, 1.0);
}

// The coverage of `u` by the box from `low` to `high`.
fn box_coverage(u: vec2<f32>, low: vec2<f32>, high: vec2<f32>, px: f32) -> f32 {
    let d = max(low - u, u - high);
    return clamp(0.5 - max(d.x, d.y) / px, 0.0, 1.0);
}

// The coverage of `u` by the wordmark, filled by the nonzero rule.
fn wordmark(u: vec2<f32>, px: f32) -> f32 {
    var nearest = 1e30;
    var winding = 0;
    for (var c = 0u; c < CONTOURS; c = c + 1u) {
        let b = word.bounds[c];
        // A point outside a contour's bounds has no winding about it, and one more than a pixel
        // outside is too far from its edges to be touched by them.
        if any(u < b.xy - px) || any(u > b.zw + px) {
            continue;
        }
        let range = word.ranges[c];
        for (var i = range.x; i < range.x + range.y; i = i + 1u) {
            let s = word.segments[i];
            let a = s.xy;
            let ab = s.zw - a;
            let ap = u - a;
            let h = clamp(dot(ap, ab) / max(dot(ab, ab), 1e-12), 0.0, 1.0);
            let off = ap - ab * h;
            nearest = min(nearest, dot(off, off));
            let side = ab.x * ap.y - ab.y * ap.x;
            if a.y <= u.y {
                if s.w > u.y && side > 0.0 {
                    winding = winding + 1;
                }
            } else if s.w <= u.y && side < 0.0 {
                winding = winding - 1;
            }
        }
    }
    let distance = sqrt(nearest) / px;
    return clamp(0.5 + select(-distance, distance, winding != 0), 0.0, 1.0);
}

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4<f32> {
    let scale = art.lockup.z;
    let px = 1.0 / scale;
    let u = (in.uv * art.frame.xy - art.lockup.xy) / scale;
    let t = art.clock.x;
    let x0 = art.mark.x;
    let base = art.mark.y;
    let side = art.mark.z;
    let radius = art.mark.w;

    let sway = SWAY * art.clock.y * smoothstep(RISE_TAKES, RISE_TAKES + SWAY_GROWS, t);
    let phase = TAU * t / SWAY_PERIOD;
    let shadows = radius * rise(t) * (1.0 + sway * sin(phase));
    let highlights = radius * rise(t - RISE_LAG) * (1.0 + sway * sin(phase + SWAY_OFFSET));
    let l = quarter(u, vec2<f32>(x0, base), shadows, 1.0, px);
    let r = quarter(u, vec2<f32>(x0 + side, base), highlights, -1.0, px);

    // Premultiplied: each channel alone, and the arch where both are.
    let both = l * r;
    var colour = stored(art.left.rgb) * (l - both) + stored(art.right.rgb) * (r - both)
        + stored(art.arch.rgb) * art.arch.a * both;
    var alpha = l + r - 2.0 * both + art.arch.a * both;

    // On paper a tonal axis closes the arch, split between the two colours.
    let axis = art.lockup.w;
    if axis > 0.0 {
        let cover = box_coverage(u, vec2<f32>(x0, base - axis), vec2<f32>(x0 + side, base), px);
        let towards = clamp((u.x - (x0 + side * 0.5)) / px + 0.5, 0.0, 1.0);
        let hue = mix(stored(art.left.rgb), stored(art.right.rgb), towards);
        colour = hue * cover + colour * (1.0 - cover);
        alpha = cover + alpha * (1.0 - cover);
    }

    let lettered = wordmark(u, px) * smoothstep(WORD_FROM, WORD_FROM + WORD_TAKES, t) * art.word.a;
    colour = stored(art.word.rgb) * lettered + colour * (1.0 - lettered);
    alpha = lettered + alpha * (1.0 - lettered);
    return vec4<f32>(colour, alpha);
}
