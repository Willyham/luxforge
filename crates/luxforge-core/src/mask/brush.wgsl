// The brush's coverage on the GPU, beside its CPU field in `brush.rs`: the frozen capsule profile,
// the maximum along each stroke (its nearest segment), the screen union across strokes in stored
// order with erase strokes taking away, and a stroke's optional colour limit, in the same spelling
// and order, evaluated in f32. A pixel tests exactly the segments the CPU's uniform grid index
// lists in its one cell, so its cost is the cell's occupancy, which the painted-stroke cap bounds.
//
// Words: 0 the stage's height H, 1 and 2 the grid's minimum corner (u0, v0), 3 the cell side,
// 4 the columns, 5 the rows (none for a component with no stroke), then the block offsets of
// 6 the cell table, 7 the entries and 8 the stroke records.
//
// The block, in the order a painted stroke grows it (its new segments are appended to the first
// part; everything after is the index the CPU rebuilds and the strokes' records):
// - the segments, in stored stroke order, five words each: ax, ay, ex, ey, len2;
// - the cell table, columns * rows + 1 words: cell k's entries are [table[k], table[k + 1]);
// - the entries, each (stroke << 24) | segment, in stored stroke order within a cell;
// - one record of seven words per stroke: R, band, flags (1 hard, 2 erase, 4 colour limit),
//   flow / 100, the seed's Oklab a and b, and the radius its refine maps to.
// Every f32 is the f64 term the CPU field holds, narrowed once.

// Linear sRGB to LMS, for a stroke limited to a colour.
const lf_mask_brush_m1_0 = vec3<f32>(0.4122214708, 0.5363325363, 0.0514459929);
const lf_mask_brush_m1_1 = vec3<f32>(0.2119034982, 0.6806995451, 0.1073969566);
const lf_mask_brush_m1_2 = vec3<f32>(0.0883024619, 0.2817188376, 0.6299787005);
// LMS' to Oklab a and b.
const lf_mask_brush_m2_1 = vec3<f32>(1.9779984951, -2.4285922050, 0.4505937099);
const lf_mask_brush_m2_2 = vec3<f32>(0.0259040371, 0.7827717662, -0.8086757660);
// The colour limit's falloff span, the colour range's own: exactly one half.
const lf_mask_brush_span: f32 = 0.5;
const lf_mask_brush_hard: u32 = 1u;
const lf_mask_brush_erase: u32 = 2u;
const lf_mask_brush_limited: u32 = 4u;
const lf_mask_brush_segment_words: u32 = 5u;
const lf_mask_brush_record_words: u32 = 7u;

// The frozen easing, s*s*(3 - 2s), on s already clamped to [0, 1].
fn lf_mask_brush_smooth(s: f32) -> f32 {
    return s * s * (3.0 - 2.0 * s);
}

// One row of a matrix-vector product, summed left to right.
fn lf_mask_brush_row(row: vec3<f32>, v: vec3<f32>) -> f32 {
    return row.x * v.x + row.y * v.y + row.z * v.z;
}

// The signed cube root, finite for every finite x. WGSL has no cbrt: the power function's
// estimate is refined by one Newton step, written so that no intermediate overflows.
fn lf_mask_brush_cbrt(x: f32) -> f32 {
    let a = abs(x);
    if a == 0.0 {
        return x;
    }
    var y = pow(a, 1.0 / 3.0);
    if y > 0.0 {
        y = (2.0 * y + a / (y * y)) / 3.0;
    }
    return select(y, -y, x < 0.0);
}

// The Oklab chromaticity (a, b) of one linear-sRGB pixel.
fn lf_mask_brush_ab(rgb: vec3<f32>) -> vec2<f32> {
    let lms = vec3<f32>(
        lf_mask_brush_row(lf_mask_brush_m1_0, rgb),
        lf_mask_brush_row(lf_mask_brush_m1_1, rgb),
        lf_mask_brush_row(lf_mask_brush_m1_2, rgb),
    );
    let root = vec3<f32>(
        lf_mask_brush_cbrt(lms.x),
        lf_mask_brush_cbrt(lms.y),
        lf_mask_brush_cbrt(lms.z),
    );
    return vec2<f32>(
        lf_mask_brush_row(lf_mask_brush_m2_1, root),
        lf_mask_brush_row(lf_mask_brush_m2_2, root),
    );
}

// The squared distance from (u, v) to the segment whose five words start at `at`, as the CPU's
// `Segment::distance2` spells it. A degenerate segment holds e = 0 and len2 = 1, so t is 0 and
// the distance is to its point, with no branch.
fn lf_mask_brush_distance2(at: u32, u: f32, v: f32) -> f32 {
    let ax = lf_block_f32(at);
    let ay = lf_block_f32(at + 1u);
    let ex = lf_block_f32(at + 2u);
    let ey = lf_block_f32(at + 3u);
    let wx = u - ax;
    let wy = v - ay;
    let t = clamp((wx * ex + wy * ey) / lf_block_f32(at + 4u), 0.0, 1.0);
    let dx = u - (ax + t * ex);
    let dy = v - (ay + t * ey);
    return dx * dx + dy * dy;
}

fn lf_mask_brush(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {
    let cols = lf_word(words + 4u);
    let rows = lf_word(words + 5u);
    if cols == 0u || rows == 0u {
        return 0.0;
    }
    let height = lf_f32(words);
    let u = (pos.x + 0.5) / height;
    let v = (pos.y + 0.5) / height;
    let side = lf_f32(words + 3u);
    let col = floor((u - lf_f32(words + 1u)) / side);
    let row = floor((v - lf_f32(words + 2u)) / side);
    if !(col >= 0.0 && col < f32(cols) && row >= 0.0 && row < f32(rows)) {
        return 0.0;
    }
    let cell = block + lf_word(words + 6u) + u32(row) * cols + u32(col);
    let entries = block + lf_word(words + 7u);
    let records = block + lf_word(words + 8u);
    var at = lf_block_word(cell);
    let end = lf_block_word(cell + 1u);
    var c = 0.0;
    var ab = vec2<f32>(0.0, 0.0);
    var ab_read = false;
    loop {
        if at >= end {
            break;
        }
        // One stroke's run of entries: its nearest listed segment.
        let stroke = lf_block_word(entries + at) >> 24u;
        var nearest2 = 0.0;
        var first = true;
        loop {
            if at >= end {
                break;
            }
            let entry = lf_block_word(entries + at);
            if (entry >> 24u) != stroke {
                break;
            }
            let segment = block + lf_mask_brush_segment_words * (entry & 0xffffffu);
            let d2 = lf_mask_brush_distance2(segment, u, v);
            nearest2 = select(min(nearest2, d2), d2, first);
            first = false;
            at = at + 1u;
        }
        // The capsule profile at the nearest distance, times the stroke's flow.
        let record = records + lf_mask_brush_record_words * stroke;
        let radius = lf_block_f32(record);
        let flags = lf_block_word(record + 2u);
        let d = sqrt(nearest2);
        var profile = 0.0;
        if (flags & lf_mask_brush_hard) != 0u {
            profile = select(0.0, 1.0, d <= radius);
        } else {
            profile = lf_mask_brush_smooth(clamp((radius - d) / lf_block_f32(record + 1u), 0.0, 1.0));
        }
        var s = profile * lf_block_f32(record + 3u);
        // A limited stroke's similarity to its seed is the last multiply.
        if (flags & lf_mask_brush_limited) != 0u {
            if !ab_read {
                ab = lf_mask_brush_ab(rgb);
                ab_read = true;
            }
            let da = ab.x - lf_block_f32(record + 4u);
            let db = ab.y - lf_block_f32(record + 5u);
            let r = sqrt(da * da + db * db) / lf_block_f32(record + 6u);
            s = s * lf_mask_brush_smooth(clamp((1.0 - r) / lf_mask_brush_span, 0.0, 1.0));
        }
        if (flags & lf_mask_brush_erase) != 0u {
            c = c * (1.0 - s);
        } else {
            c = c + (1.0 - c) * s;
        }
    }
    return c;
}
