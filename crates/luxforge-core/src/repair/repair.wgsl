// Clone and Heal on the GPU (docs/design/corrections-numerics.md, "GPU form"): one group of
// operations whose reads meet no earlier destination of the group, each solved in its own tiles of
// an atlas per level, in f32, the CPU reference's equations in its order.
//
// Atlas. Every level has atlas planes of 8 x 8 texel tiles, `atx` tiles across. An operation's
// level-k grid (its solve rectangle halved k times, `ceil`) is cut into 8 x 8 tiles; the ones that
// may hold a cell that is not outside are placed in the atlas, and a tile map per operation and
// level says where. A reverse table per level says which operation and grid tile each atlas tile
// holds. So any number of operations of any shape share one dispatch per level and pass.
//
// Planes hold rgba32float: rgb a value, a the class. Level 0: a = coverage (> 0) for an unknown,
// -1 for a fixed boundary cell, 0 outside. Coarser levels: 1 unknown, -1 fixed, 0 outside.
//
// Block, from `block`:
//   0 operations, 1 their records, 2 segments, 3 atlas tiles across, 4 the apply's cell index,
//   5 its cell side in stage pixels, 6 its columns, 7 its rows, 8 the stage's width,
//   9 its height (f32), 10 .. 10 + 16 each level's reverse table.
// An operation's record, 75 words: 0 kind (0 Clone, 1 Heal), 1 and 2 the source delta (i32),
// 3 the radius, 4 the feather's band, 5 opacity / 100 (f32, content heights), 6 its first
// segment, 7 its segments, 8 and 9 its solve rectangle's origin in stage pixels, 10 its levels,
// then per level k at 11 + 4k: the grid's width, height, tiles across and tile map's offset.
// A segment is five words, ax, ay, ex, ey, len2, as the mask brush's.
//
// A pass's words: 0 its level, 1 a red-black parity, 2 flags (1: read its values as zero, 2: it
// has a right-hand side, 4: it reads the group's input from the atlas plane at slot `flags >> 8`
// through the read table, 16: that plane is the step's input), 3 the group's block offset.

const lf_repair_tile: i32 = 8;
const lf_repair_none: u32 = 0xffffffffu;
const lf_repair_op_words: u32 = 75u;
const lf_repair_levels_at: u32 = 11u;
const lf_repair_reverse_at: u32 = 10u;

fn lf_repair_op(block: u32, op: u32) -> u32 {
    return block + lf_block_word(block + 1u) + op * lf_repair_op_words;
}

fn lf_repair_level(record: u32, level: u32) -> u32 {
    return record + lf_repair_levels_at + 4u * level;
}

// The operation and grid cell atlas texel `at` of `level` holds: (op, i, j, held).
fn lf_repair_owner(block: u32, level: u32, at: vec2<i32>) -> vec4<i32> {
    let atx = i32(lf_block_word(block + 3u));
    let tile = at / lf_repair_tile;
    let index = u32(tile.y * atx + tile.x);
    let packed = lf_block_word(block + lf_block_word(block + lf_repair_reverse_at + level) + index);
    if packed == lf_repair_none {
        return vec4<i32>(0, 0, 0, 0);
    }
    let op = packed >> 24u;
    let gx = i32(packed & 0xfffu);
    let gy = i32((packed >> 12u) & 0xfffu);
    let i = gx * lf_repair_tile + at.x % lf_repair_tile;
    let j = gy * lf_repair_tile + at.y % lf_repair_tile;
    let grid = lf_repair_level(lf_repair_op(block, op), level);
    if i >= i32(lf_block_word(grid)) || j >= i32(lf_block_word(grid + 1u)) {
        return vec4<i32>(0, 0, 0, 0);
    }
    return vec4<i32>(i32(op), i, j, 1);
}

// The atlas texel of cell (i, j) of `op`'s `level`, or (-1, -1) where its grid holds none.
fn lf_repair_texel(block: u32, op: u32, level: u32, i: i32, j: i32) -> vec2<i32> {
    let grid = lf_repair_level(lf_repair_op(block, op), level);
    if i < 0 || j < 0 || i >= i32(lf_block_word(grid)) || j >= i32(lf_block_word(grid + 1u)) {
        return vec2<i32>(-1, -1);
    }
    let across = i32(lf_block_word(grid + 2u));
    let tile = lf_block_word(block + lf_block_word(grid + 3u)
        + u32((j / lf_repair_tile) * across + i / lf_repair_tile));
    if tile == lf_repair_none {
        return vec2<i32>(-1, -1);
    }
    let atx = lf_block_word(block + 3u);
    return vec2<i32>(i32(tile % atx) * lf_repair_tile + i % lf_repair_tile,
        i32(tile / atx) * lf_repair_tile + j % lf_repair_tile);
}

// Plane `slot` at an atlas texel, outside where there is none.
fn lf_repair_read(slot: u32, texel: vec2<i32>) -> vec4<f32> {
    if texel.x < 0 {
        return vec4<f32>(0.0);
    }
    return lf_plane(slot, texel);
}

// The frozen easing.
fn lf_repair_smooth(s: f32) -> f32 {
    return s * s * (3.0 - 2.0 * s);
}

// Coverage at stage pixel `p`: the capsule profile at the nearest segment's distance, times the
// opacity, at the pixel's centre in mask space; zero where the source leaves the stage.
fn lf_repair_alpha(block: u32, record: u32, p: vec2<i32>) -> f32 {
    let width = i32(lf_block_word(block + 8u));
    let h = lf_block_f32(block + 9u);
    let delta = vec2<i32>(bitcast<i32>(lf_block_word(record + 1u)),
        bitcast<i32>(lf_block_word(record + 2u)));
    let s = p + delta;
    if p.x < 0 || p.y < 0 || p.x >= width || f32(p.y) >= h || s.x < 0 || s.y < 0
        || s.x >= width || f32(s.y) >= h {
        return 0.0;
    }
    let u = (f32(p.x) + 0.5) / h;
    let v = (f32(p.y) + 0.5) / h;
    let first = lf_block_word(block + 2u) + 5u * lf_block_word(record + 6u);
    let count = lf_block_word(record + 7u);
    var nearest = 3.0e38;
    for (var k = 0u; k < count; k = k + 1u) {
        let at = block + first + 5u * k;
        let ax = lf_block_f32(at);
        let ay = lf_block_f32(at + 1u);
        let ex = lf_block_f32(at + 2u);
        let ey = lf_block_f32(at + 3u);
        let wx = u - ax;
        let wy = v - ay;
        let t = clamp((wx * ex + wy * ey) / lf_block_f32(at + 4u), 0.0, 1.0);
        let dx = u - (ax + t * ex);
        let dy = v - (ay + t * ey);
        nearest = min(nearest, dx * dx + dy * dy);
    }
    let radius = lf_block_f32(record + 3u);
    let band = lf_block_f32(record + 4u);
    let d = sqrt(nearest);
    var profile = 0.0;
    if band == 0.0 {
        profile = select(0.0, 1.0, d <= radius);
    } else {
        profile = lf_repair_smooth(clamp((radius - d) / band, 0.0, 1.0));
    }
    return profile * lf_block_f32(record + 5u);
}

// The sRGB transfer function extended over the real line, its constants the core's `f32` encode's
// (held to it by the core's tests) and the decode's threshold and exponent.
const lf_repair_linear_end: f32 = 0.0031308;
const lf_repair_slope: f32 = 12.92;
const lf_repair_scale: f32 = 1.055;
const lf_repair_offset: f32 = 0.055;
const lf_repair_exponent: f32 = 1.0 / 2.4;
const lf_repair_decode_end: f32 = 0.04045;
const lf_repair_decode_exponent: f32 = 2.4;

fn lf_repair_encode(x: vec3<f32>) -> vec3<f32> {
    let power = lf_repair_scale * pow(max(x, vec3<f32>(0.0)), vec3<f32>(lf_repair_exponent))
        - lf_repair_offset;
    return select(power, lf_repair_slope * x, x <= vec3<f32>(lf_repair_linear_end));
}

fn lf_repair_decode(y: vec3<f32>) -> vec3<f32> {
    let power = pow((max(y, vec3<f32>(0.0)) + lf_repair_offset) / lf_repair_scale,
        vec3<f32>(lf_repair_decode_exponent));
    return select(power, y / lf_repair_slope, y <= vec3<f32>(lf_repair_decode_end));
}

// The read set (corrections.md, "Rendering"). A step holds every rectangle its operations read
// in atlas planes of its own: its input, which the surface cuts from the photograph's source, and
// the states chained groups leave, all at the same placements. The read table is the step's block
// from `base`:
//   0 rectangles, 1 one where the step's input is clamped to [0, 1] as the CPU quantizes it,
//   2 .. 6 the stage's index: its cell side in pixels, columns, rows and offset from `base`,
//   6 .. 10 the atlas's index, likewise,
//   10 + 6k rectangle k: x0, y0, width and height in stage pixels, the atlas texel of its first.
// An index lists, per cell, the rectangles that meet it there: cells + 1 offsets from the index's
// own start, then the lists.
const lf_repair_rects_at: u32 = 10u;

// The rectangle holding `q` of the stage (`at` 2) or of the atlas (`at` 6), or -1.
fn lf_repair_find(base: u32, q: vec2<i32>, at: u32) -> i32 {
    let side = i32(lf_block_word(base + at));
    let columns = i32(lf_block_word(base + at + 1u));
    let rows = i32(lf_block_word(base + at + 2u));
    if q.x < 0 || q.y < 0 || q.x / side >= columns || q.y / side >= rows {
        return -1;
    }
    let index = base + lf_block_word(base + at + 3u);
    let cell = u32((q.y / side) * columns + q.x / side);
    let last = lf_block_word(index + cell + 1u);
    let held = select(0u, 4u, at == 6u);
    for (var e = lf_block_word(index + cell); e < last; e = e + 1u) {
        let rect = lf_block_word(index + e);
        let record = base + lf_repair_rects_at + 6u * rect;
        let low = vec2<i32>(i32(lf_block_word(record + held)), i32(lf_block_word(record + held + 1u)));
        let size = vec2<i32>(i32(lf_block_word(record + 2u)), i32(lf_block_word(record + 3u)));
        if all(q >= low) && all(q < low + size) {
            return i32(rect);
        }
    }
    return -1;
}

// From a rectangle's stage origin to its atlas placement, the sign of `to_atlas`.
fn lf_repair_shift(base: u32, rect: i32) -> vec2<i32> {
    let record = base + lf_repair_rects_at + 6u * u32(rect);
    return vec2<i32>(i32(lf_block_word(record + 4u)), i32(lf_block_word(record + 5u)))
        - vec2<i32>(i32(lf_block_word(record)), i32(lf_block_word(record + 1u)));
}

// The atlas texel holding stage pixel `p`, or (-1, -1) where the read set holds none.
fn lf_repair_atlas(base: u32, p: vec2<i32>) -> vec2<i32> {
    let rect = lf_repair_find(base, p, 2u);
    if rect < 0 {
        return vec2<i32>(-1, -1);
    }
    return p + lf_repair_shift(base, rect);
}

// The stage pixel atlas texel `t` holds, or (-1, -1) for a texel between its rectangles.
fn lf_repair_stage(base: u32, t: vec2<i32>) -> vec2<i32> {
    let rect = lf_repair_find(base, t, 6u);
    if rect < 0 {
        return vec2<i32>(-1, -1);
    }
    return t - lf_repair_shift(base, rect);
}

// Atlas texel `t` of the plane a pass's `flags` name: flag 4, bound at slot `flags >> 8`; flag 16,
// the step's input, clamped where the table says. Zero where `t` is no texel of the atlas.
fn lf_repair_held(flags: u32, base: u32, t: vec2<i32>) -> vec3<f32> {
    if t.x < 0 {
        return vec3<f32>(0.0);
    }
    let value = lf_plane((flags >> 8u) & 0xffu, t).rgb;
    if (flags & 16u) != 0u && lf_block_word(base + 1u) == 1u {
        return clamp(value, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return value;
}

// The group's input at stage pixel `p`: with flag 4 of the pass's flags, the atlas plane they name
// through the read table at `base` — the step's input or the state a chained group before it left;
// without, for one group drawn alone over a boundary that holds everything it reads, `lf_source`.
fn lf_repair_input(p: vec2<i32>, words: u32, base: u32) -> vec3<f32> {
    let flags = lf_word(words + 2u);
    if (flags & 4u) != 0u {
        return lf_repair_held(flags, base, lf_repair_atlas(base, p));
    }
    return lf_source(p - lf_origin());
}

// The state chained groups left at stage pixel `p`, the atlas plane at `slot`: `rgb`, the step's
// input, where the read set holds no texel of `p`, which no operation writes.
fn lf_repair_state(rgb: vec3<f32>, p: vec2<i32>, base: u32, slot: u32) -> vec3<f32> {
    let t = lf_repair_atlas(base, p);
    if t.x < 0 {
        return rgb;
    }
    return lf_plane(slot, t).rgb;
}

// Level 0: each cell's class, and a fixed cell's encoded boundary difference.
fn lf_repair_gather(at: vec2<i32>, words: u32, base: u32) {
    let block = base + lf_word(words + 3u);
    let owner = lf_repair_owner(block, 0u, at);
    if owner.w == 0 {
        lf_store(at, vec4<f32>(0.0));
        return;
    }
    let record = lf_repair_op(block, u32(owner.x));
    let p = vec2<i32>(i32(lf_block_word(record + 8u)) + owner.y,
        i32(lf_block_word(record + 9u)) + owner.z);
    let a = lf_repair_alpha(block, record, p);
    if a > 0.0 {
        lf_store(at, vec4<f32>(0.0, 0.0, 0.0, a));
        return;
    }
    let delta = vec2<i32>(bitcast<i32>(lf_block_word(record + 1u)),
        bitcast<i32>(lf_block_word(record + 2u)));
    let width = i32(lf_block_word(block + 8u));
    let height = i32(lf_block_f32(block + 9u));
    let s = p + delta;
    let valid = s.x >= 0 && s.y >= 0 && s.x < width && s.y < height;
    let heal = lf_block_word(record) == 1u;
    if heal && valid && (lf_repair_alpha(block, record, p + vec2<i32>(-1, 0)) > 0.0
        || lf_repair_alpha(block, record, p + vec2<i32>(1, 0)) > 0.0
        || lf_repair_alpha(block, record, p + vec2<i32>(0, -1)) > 0.0
        || lf_repair_alpha(block, record, p + vec2<i32>(0, 1)) > 0.0) {
        let d = lf_repair_encode(lf_repair_input(p, words, base))
            - lf_repair_encode(lf_repair_input(s, words, base));
        lf_store(at, vec4<f32>(d, -1.0));
        return;
    }
    lf_store(at, vec4<f32>(0.0));
}

// Level k from level k - 1 (slot 0): fixed with the mean of its fixed children if any child is
// fixed, else unknown at zero if any child is, else outside.
fn lf_repair_coarse(at: vec2<i32>, words: u32, base: u32) {
    let block = base + lf_word(words + 3u);
    let level = lf_word(words);
    let owner = lf_repair_owner(block, level, at);
    if owner.w == 0 {
        lf_store(at, vec4<f32>(0.0));
        return;
    }
    var sum = vec3<f32>(0.0);
    var fixed = 0.0;
    var unknown = false;
    for (var c = 0; c < 4; c = c + 1) {
        let child = lf_repair_texel(block, u32(owner.x), level - 1u, 2 * owner.y + c % 2,
            2 * owner.z + c / 2);
        let v = lf_repair_read(0u, child);
        if v.w < 0.0 {
            sum = sum + v.xyz;
            fixed = fixed + 1.0;
        } else if v.w > 0.0 {
            unknown = true;
        }
    }
    if fixed > 0.0 {
        lf_store(at, vec4<f32>(sum / fixed, -1.0));
    } else if unknown {
        lf_store(at, vec4<f32>(0.0, 0.0, 0.0, 1.0));
    } else {
        lf_store(at, vec4<f32>(0.0));
    }
}

// The start of level k (slot 0 its classes and means): an unknown takes its parent's value in
// level k + 1's start (slot 1).
fn lf_repair_start(at: vec2<i32>, words: u32, base: u32) {
    let block = base + lf_word(words + 3u);
    let level = lf_word(words);
    let v = lf_plane(0u, at);
    let owner = lf_repair_owner(block, level, at);
    if owner.w == 0 || v.w <= 0.0 {
        lf_store(at, v);
        return;
    }
    let parent = lf_repair_texel(block, u32(owner.x), level + 1u, owner.y / 2, owner.z / 2);
    lf_store(at, vec4<f32>(lf_repair_read(1u, parent).xyz, v.w));
}

// A cell's value and class: slot 0 (value and class), or zero with slot 1's class.
fn lf_repair_value(texel: vec2<i32>, zero: bool) -> vec4<f32> {
    if texel.x < 0 {
        return vec4<f32>(0.0);
    }
    if zero {
        return vec4<f32>(0.0, 0.0, 0.0, lf_plane(1u, texel).w);
    }
    return lf_plane(0u, texel);
}

// One red-black half iteration on a(f) = rhs at level k: slot 0 the values, slot 1 the
// right-hand side and classes (coarse levels).
fn lf_repair_relax(at: vec2<i32>, words: u32, base: u32) {
    let block = base + lf_word(words + 3u);
    let level = lf_word(words);
    let parity = i32(lf_word(words + 1u));
    let flags = lf_word(words + 2u);
    let zero = (flags & 1u) != 0u;
    let owner = lf_repair_owner(block, level, at);
    let here = lf_repair_value(select(vec2<i32>(-1, -1), at, owner.w != 0), zero);
    if owner.w == 0 || here.w <= 0.0 || (owner.y + owner.z) % 2 != parity {
        lf_store(at, here);
        return;
    }
    var sum = vec3<f32>(0.0);
    if (flags & 2u) != 0u {
        sum = lf_plane(1u, at).xyz;
    }
    var count = 0.0;
    let op = u32(owner.x);
    var steps = array<vec2<i32>, 4>(vec2<i32>(-1, 0), vec2<i32>(1, 0), vec2<i32>(0, -1),
        vec2<i32>(0, 1));
    for (var n = 0; n < 4; n = n + 1) {
        let v = lf_repair_value(lf_repair_texel(block, op, level, owner.y + steps[n].x,
            owner.z + steps[n].y), zero);
        if v.w != 0.0 {
            sum = sum + v.xyz;
            count = count + 1.0;
        }
    }
    if count > 0.0 {
        lf_store(at, vec4<f32>(sum / count, here.w));
    } else {
        lf_store(at, here);
    }
}

// Level k + 1's right-hand side: each unknown coarse cell (slot 2 the coarse classes) the sum of
// its unknown children's residuals rhs - (count f - sum of neighbours) at level k (slot 0 the
// values, slot 1 the right-hand side when the words' flags say it has one).
fn lf_repair_restrict(at: vec2<i32>, words: u32, base: u32) {
    let block = base + lf_word(words + 3u);
    let level = lf_word(words);
    let has_rhs = (lf_word(words + 2u) & 2u) != 0u;
    let owner = lf_repair_owner(block, level + 1u, at);
    let kind = lf_plane(2u, at).w;
    if owner.w == 0 || kind <= 0.0 {
        lf_store(at, vec4<f32>(0.0, 0.0, 0.0, kind));
        return;
    }
    let op = u32(owner.x);
    var steps = array<vec2<i32>, 4>(vec2<i32>(-1, 0), vec2<i32>(1, 0), vec2<i32>(0, -1),
        vec2<i32>(0, 1));
    var sum = vec3<f32>(0.0);
    for (var c = 0; c < 4; c = c + 1) {
        let i = 2 * owner.y + c % 2;
        let j = 2 * owner.z + c / 2;
        let texel = lf_repair_texel(block, op, level, i, j);
        let f = lf_repair_read(0u, texel);
        if f.w <= 0.0 {
            continue;
        }
        var neighbours = vec3<f32>(0.0);
        var count = 0.0;
        for (var n = 0; n < 4; n = n + 1) {
            let v = lf_repair_read(0u, lf_repair_texel(block, op, level, i + steps[n].x,
                j + steps[n].y));
            if v.w != 0.0 {
                neighbours = neighbours + v.xyz;
                count = count + 1.0;
            }
        }
        var rhs = vec3<f32>(0.0);
        if has_rhs {
            rhs = lf_plane(1u, texel).xyz;
        }
        sum = sum + (rhs - (count * f.xyz - neighbours));
    }
    lf_store(at, vec4<f32>(sum, kind));
}

// Level k's values (slot 0) corrected by level k + 1's (slot 1) where both are unknown.
fn lf_repair_correct(at: vec2<i32>, words: u32, base: u32) {
    let block = base + lf_word(words + 3u);
    let level = lf_word(words);
    let v = lf_plane(0u, at);
    let owner = lf_repair_owner(block, level, at);
    if owner.w == 0 || v.w <= 0.0 {
        lf_store(at, v);
        return;
    }
    let parent = lf_repair_read(1u,
        lf_repair_texel(block, u32(owner.x), level + 1u, owner.y / 2, owner.z / 2));
    if parent.w > 0.0 {
        lf_store(at, vec4<f32>(v.xyz + parent.xyz, v.w));
    } else {
        lf_store(at, v);
    }
}

// Level 0's repaired value and coverage (slot 0 the solved correction): Clone's source, or Heal's
// decode(encode(source) + f).
fn lf_repair_composite(at: vec2<i32>, words: u32, base: u32) {
    let block = base + lf_word(words + 3u);
    let owner = lf_repair_owner(block, 0u, at);
    let v = lf_plane(0u, at);
    if owner.w == 0 || v.w <= 0.0 {
        lf_store(at, vec4<f32>(0.0));
        return;
    }
    let record = lf_repair_op(block, u32(owner.x));
    let p = vec2<i32>(i32(lf_block_word(record + 8u)) + owner.y,
        i32(lf_block_word(record + 9u)) + owner.z);
    let delta = vec2<i32>(bitcast<i32>(lf_block_word(record + 1u)),
        bitcast<i32>(lf_block_word(record + 2u)));
    var healed = lf_repair_input(p + delta, words, base);
    if lf_block_word(record) == 1u {
        healed = lf_repair_decode(lf_repair_encode(healed) + v.xyz);
    }
    lf_store(at, vec4<f32>(healed, v.w));
}

// The group's repaired value over `rgb` at stage pixel `p` where an operation covers it, blended
// by its coverage, read from the composite plane at `slot`; `rgb` elsewhere. The cell index lists,
// per cell of the stage, the operations whose solve rectangle meets it.
fn lf_repair_over(rgb: vec3<f32>, p: vec2<i32>, block: u32, slot: u32) -> vec3<f32> {
    let side = i32(lf_block_word(block + 5u));
    let columns = i32(lf_block_word(block + 6u));
    let rows = i32(lf_block_word(block + 7u));
    let cell = p / side;
    if p.x < 0 || p.y < 0 || cell.x >= columns || cell.y >= rows {
        return rgb;
    }
    let index = block + lf_block_word(block + 4u);
    let k = u32(cell.y * columns + cell.x);
    let first = lf_block_word(index + k);
    let last = lf_block_word(index + k + 1u);
    for (var e = first; e < last; e = e + 1u) {
        let op = lf_block_word(index + e);
        let record = lf_repair_op(block, op);
        let i = p.x - i32(lf_block_word(record + 8u));
        let j = p.y - i32(lf_block_word(record + 9u));
        let texel = lf_repair_texel(block, op, 0u, i, j);
        if texel.x < 0 {
            continue;
        }
        let v = lf_plane(slot, texel);
        if v.w > 0.0 {
            return (1.0 - v.w) * rgb + v.w * v.xyz;
        }
    }
    return rgb;
}

// The frame's step of one group alone: its repaired value over the step's input.
fn lf_repair_apply(rgb: vec3<f32>, at: vec2<i32>, words: u32, base: u32, planes: u32) -> vec3<f32> {
    return lf_repair_over(rgb, at + lf_origin(), base + lf_word(words + 3u), planes);
}

// The frame's step of a chain of groups: the last group's repaired value over the state the
// groups before it left (plane `planes`; the composite at `planes + 1`).
fn lf_repair_apply_over(rgb: vec3<f32>, at: vec2<i32>, words: u32, base: u32, planes: u32) -> vec3<f32> {
    let p = at + lf_origin();
    let state = lf_repair_state(rgb, p, base, planes);
    return lf_repair_over(state, p, base + lf_word(words + 3u), planes + 1u);
}

// A chained group's output over the atlas: at each texel of the read set its repaired value over
// the plane the pass's flags name (the input or a state), the composite at slot
// `lf_word(words + 4u)`.
fn lf_repair_blend(at: vec2<i32>, words: u32, base: u32) {
    let p = lf_repair_stage(base, at);
    if p.x < 0 {
        lf_store(at, vec4<f32>(0.0));
        return;
    }
    let block = base + lf_word(words + 3u);
    let held = lf_repair_held(lf_word(words + 2u), base, at);
    lf_store(at, vec4<f32>(lf_repair_over(held, p, block, lf_word(words + 4u)), 1.0));
}

// A frozen patch (corrections.md, "Frozen patches"): immutable pixels composited over the stage
// at their rectangle. Its words are resident below the step's block base: record word 10 counts
// every resident word, so they start at `base - lf_block_word(record + 10u)`. Record: 0..4 the
// stage pixels it may write [x0, y0, x1, y1), 4 1 when its words are reduced box means (four f32
// a pixel: the coverage-weighted colour and the coverage) and 0 when they are its own half floats
// (RGBA, two words a pixel), 5 its words' offset among the resident words, 6 their row stride in
// pixels, 7 and 8 the stage pixel of their first, 9 opacity / 100.

// One IEEE half float, the low 16 bits of `h`, exactly as `f32`; infinity for a half's infinity
// or NaN, which the finite check then refuses.
fn lf_repair_half(h: u32) -> f32 {
    let sign = (h >> 15u) & 1u;
    let exponent = (h >> 10u) & 0x1fu;
    let mantissa = h & 0x3ffu;
    var value: f32;
    if exponent == 0u {
        value = f32(mantissa) * 5.9604645e-8;
    } else if exponent == 31u {
        value = bitcast<f32>(0x7f800000u);
    } else {
        value = bitcast<f32>(((exponent + 112u) << 23u) | (mantissa << 13u));
    }
    return select(value, -value, sign == 1u);
}

// Two halves of one word, the low first.
fn lf_repair_halves(word: u32) -> vec2<f32> {
    return vec2<f32>(lf_repair_half(word & 0xffffu), lf_repair_half(word >> 16u));
}

fn lf_repair_patch_finite(v: vec4<f32>) -> bool {
    return all(v == v) && all(abs(v) <= vec4<f32>(65504.0));
}

// The patch's value over `rgb` at stage pixel `p`: `rgb + c (patch - rgb)`, c = coverage times
// the opacity, at the stage it was placed in; `rgb + o (P - A rgb)` over a reduced stage's box;
// `rgb` itself outside the patch or where it covers nothing.
fn lf_repair_patch_over(rgb: vec3<f32>, p: vec2<i32>, base: u32, record: u32) -> vec3<f32> {
    let lo = vec2<i32>(i32(lf_block_word(record)), i32(lf_block_word(record + 1u)));
    let hi = vec2<i32>(i32(lf_block_word(record + 2u)), i32(lf_block_word(record + 3u)));
    if any(p < lo) || any(p >= hi) {
        return rgb;
    }
    let data = base - lf_block_word(record + 10u) + lf_block_word(record + 5u);
    let stride = lf_block_word(record + 6u);
    let origin = vec2<i32>(i32(lf_block_word(record + 7u)), i32(lf_block_word(record + 8u)));
    let t = vec2<u32>(p - origin);
    let amount = lf_block_f32(record + 9u);
    let at = t.y * stride + t.x;
    if lf_block_word(record + 4u) == 0u {
        let rg = lf_repair_halves(lf_block_word(data + 2u * at));
        let ba = lf_repair_halves(lf_block_word(data + 2u * at + 1u));
        let texel = vec4<f32>(rg, ba);
        if !lf_repair_patch_finite(texel) || !(texel.w > 0.0) {
            return rgb;
        }
        let c = min(texel.w, 1.0) * amount;
        return rgb + c * (texel.xyz - rgb);
    }
    let w = data + 4u * at;
    let mean = vec4<f32>(lf_block_f32(w), lf_block_f32(w + 1u), lf_block_f32(w + 2u),
        lf_block_f32(w + 3u));
    if !(mean.w > 0.0) {
        return rgb;
    }
    return rgb + amount * (mean.xyz - mean.w * rgb);
}

// The frame's step when a patch is its last operation and nothing before it was chained: the
// patch over the step's input.
fn lf_repair_patch_apply(rgb: vec3<f32>, at: vec2<i32>, words: u32, base: u32, planes: u32) -> vec3<f32> {
    return lf_repair_patch_over(rgb, at + lf_origin(), base, base + lf_word(words + 3u));
}

// The frame's step when a patch is its last operation after chained ones: the patch over the
// state they left (plane `planes`).
fn lf_repair_patch_apply_over(rgb: vec3<f32>, at: vec2<i32>, words: u32, base: u32, planes: u32) -> vec3<f32> {
    let p = at + lf_origin();
    let state = lf_repair_state(rgb, p, base, planes);
    return lf_repair_patch_over(state, p, base, base + lf_word(words + 3u));
}

// A chained patch's output over the atlas: the patch over the plane the pass's flags name, the
// step's input or a state, at each texel of the read set.
fn lf_repair_patch_blend(at: vec2<i32>, words: u32, base: u32) {
    let p = lf_repair_stage(base, at);
    if p.x < 0 {
        lf_store(at, vec4<f32>(0.0));
        return;
    }
    let held = lf_repair_held(lf_word(words + 2u), base, at);
    lf_store(at, vec4<f32>(lf_repair_patch_over(held, p, base, base + lf_word(words + 3u)), 1.0));
}
