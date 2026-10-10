// Clone and Heal on the GPU, beside the CPU reference units in `repair/` (`mod.rs`, `footprint.rs`,
// `heal.rs`), under the spatial convention (`render/gpu/spatial.rs`). The host's plan
// (`render/gpu/repair.rs`) describes one spatial step for a whole repair layer: the operations in
// ordered groups, each group a pass (Clone) or a chain of passes (Heal) from one state plane of
// the window into the next, and one apply that reads the last state plane. The arithmetic is the
// reference's, in `f32`:
//
// - coverage: the capsule profile of the distance to the operation's nearest segment, through the
//   same cell index the CPU footprint builds, times `opacity / 100` (`lf_repair_coverage`);
// - Clone: `(1 - a) * I(p) + a * I(p + delta)` where the source lies in the stage
//   (`lf_repair_clone`), for a group of operations no one of which reads what another writes;
// - Heal: the membrane in the sRGB-encoded domain over the operation's solve rectangle, its
//   levels anchored at the rectangle's origin and halved rounding up, started from the fixed
//   cells' means and solved by three V(2, 2) cycles of red-black Gauss-Seidel, every level's
//   classes, sums and order the CPU's; then the composite `D(E(S) + f)` blended by the coverage
//   (`lf_repair_heal_*`).
//
// The storage block (`block`) holds, from its start: the operation count, the stage's width and
// height, each operation's record offset; each record (`lf_repair_record`); the cell index's
// offsets and entries; and the segments in pixel units. A pass's words: its input (0 a state
// plane bound as plane 0, 1 the step's input through `lf_source`), then its own parameters.
// A level plane holds the membrane in `rgb` and the cell's class in `a`: 0 outside, 1 unknown,
// 2 fixed.

const lf_repair_outside: f32 = 0.0;
const lf_repair_unknown: f32 = 1.0;
const lf_repair_fixed: f32 = 2.0;

// ---- The block ---------------------------------------------------------------------------------

fn lf_repair_stage(block: u32) -> vec2<i32> {
    return vec2<i32>(i32(lf_block_word(block + 1u)), i32(lf_block_word(block + 2u)));
}

// The absolute index of operation `op`'s record.
fn lf_repair_record(block: u32, op: u32) -> u32 {
    return block + lf_block_word(block + 3u + op);
}

fn lf_repair_delta(record: u32) -> vec2<i32> {
    return vec2<i32>(bitcast<i32>(lf_block_word(record + 4u)), bitcast<i32>(lf_block_word(record + 5u)));
}

fn lf_repair_rect(record: u32) -> vec2<i32> {
    return vec2<i32>(i32(lf_block_word(record + 16u)), i32(lf_block_word(record + 17u)));
}

// Whether stage pixel `p`'s source lies inside the stage.
fn lf_repair_valid(block: u32, record: u32, p: vec2<i32>) -> bool {
    let s = p + lf_repair_delta(record);
    let stage = lf_repair_stage(block);
    return all(s >= vec2<i32>(0)) && all(s < stage);
}

// The frozen easing on `s` already clamped to `[0, 1]`.
fn lf_repair_smooth(s: f32) -> f32 {
    return s * s * (3.0 - 2.0 * s);
}

// The coverage at stage pixel `p`: zero outside the support and in an empty cell, otherwise the
// profile of the nearest listed segment's distance, in pixels, times the amount.
fn lf_repair_coverage(block: u32, record: u32, p: vec2<i32>) -> f32 {
    let low = vec2<i32>(i32(lf_block_word(record + 6u)), i32(lf_block_word(record + 7u)));
    let high = vec2<i32>(i32(lf_block_word(record + 8u)), i32(lf_block_word(record + 9u)));
    if any(p < low) || any(p >= high) {
        return 0.0;
    }
    let side = i32(lf_block_word(record + 10u));
    let columns = u32(lf_block_word(record + 11u));
    let cell = u32((p.y - low.y) / side) * columns + u32((p.x - low.x) / side);
    let offsets = block + lf_block_word(record + 13u);
    let entries = block + lf_block_word(record + 14u);
    let segments = block + lf_block_word(record + 15u);
    let first = lf_block_word(offsets + cell);
    let last = lf_block_word(offsets + cell + 1u);
    if first == last {
        return 0.0;
    }
    let u = f32(p.x) + 0.5;
    let v = f32(p.y) + 0.5;
    var nearest = 3.4e38;
    for (var at = first; at < last; at++) {
        let s = segments + 5u * lf_block_word(entries + at);
        let ax = lf_block_f32(s);
        let ay = lf_block_f32(s + 1u);
        let ex = lf_block_f32(s + 2u);
        let ey = lf_block_f32(s + 3u);
        let len2 = lf_block_f32(s + 4u);
        let wx = u - ax;
        let wy = v - ay;
        let t = clamp((wx * ex + wy * ey) / len2, 0.0, 1.0);
        let dx = u - (ax + t * ex);
        let dy = v - (ay + t * ey);
        nearest = min(nearest, dx * dx + dy * dy);
    }
    let d = sqrt(nearest);
    let radius = lf_block_f32(record + 1u);
    let band = lf_block_f32(record + 2u);
    var profile = 0.0;
    if band == 0.0 {
        profile = select(0.0, 1.0, d <= radius);
    } else {
        profile = lf_repair_smooth(clamp((radius - d) / band, 0.0, 1.0));
    }
    return profile * lf_block_f32(record + 3u);
}

// The repaired coverage: the coverage where the source is valid, else zero.
fn lf_repair_alpha(block: u32, record: u32, p: vec2<i32>) -> f32 {
    if !lf_repair_valid(block, record, p) {
        return 0.0;
    }
    return lf_repair_coverage(block, record, p);
}

// ---- The state planes --------------------------------------------------------------------------

// The pass's input state at window texel `at`: plane 0, or the step's input.
fn lf_repair_input(at: vec2<i32>, words: u32) -> vec3<f32> {
    if lf_word(words) == 1u {
        return lf_source(at);
    }
    return lf_plane(0u, at).rgb;
}

// One group of Clone operations over the state: words `[input, first, count]`.
fn lf_repair_clone(at: vec2<i32>, words: u32, block: u32) {
    let p = lf_origin() + at;
    var value = lf_repair_input(at, words);
    let first = lf_word(words + 1u);
    let count = lf_word(words + 2u);
    for (var k = 0u; k < count; k++) {
        let record = lf_repair_record(block, first + k);
        let a = lf_repair_alpha(block, record, p);
        if a > 0.0 {
            let s = lf_repair_input(at + lf_repair_delta(record), words);
            value = (1.0 - a) * value + a * s;
        }
    }
    lf_store(at, vec4<f32>(value, 1.0));
}

// The step's apply: the last state plane at the pixel.
fn lf_repair_apply(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    return lf_plane(planes, at).rgb;
}

// ---- Heal ----------------------------------------------------------------------------------------

// The sRGB transfer, extended over the real line, as the CPU's `srgb::encode` and `decode`.
fn lf_repair_encode(linear: vec3<f32>) -> vec3<f32> {
    let low = 12.92 * linear;
    let high = 1.055 * pow(max(linear, vec3<f32>(0.0031308)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, linear <= vec3<f32>(0.0031308));
}

fn lf_repair_decode(encoded: vec3<f32>) -> vec3<f32> {
    let low = encoded / 12.92;
    let high = pow((max(encoded, vec3<f32>(0.04045)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, encoded <= vec3<f32>(0.04045));
}

// A Heal pass's words: `[input, op, w, h, a, b, c, d]`, the level's own size `(w, h)` and two more
// sizes or flags the pass names.
fn lf_repair_size(words: u32, at: u32) -> vec2<i32> {
    return vec2<i32>(i32(lf_word(words + at)), i32(lf_word(words + at + 1u)));
}

// The level's cell `at`, or an outside cell past its size.
fn lf_repair_cell(slot: u32, at: vec2<i32>, size: vec2<i32>) -> vec4<f32> {
    if any(at < vec2<i32>(0)) || any(at >= size) {
        return vec4<f32>(0.0);
    }
    return lf_plane(slot, at);
}

// Level 0: each cell's class, and a fixed cell's boundary difference `E(I(q)) - E(S(q))`.
// Words `[input, op, w, h]`; the state is plane 0 or the step's input.
fn lf_repair_heal_setup(at: vec2<i32>, words: u32, block: u32) {
    let size = lf_repair_size(words, 2u);
    if any(at >= size) {
        lf_store(at, vec4<f32>(0.0));
        return;
    }
    let record = lf_repair_record(block, lf_word(words + 1u));
    let p = lf_repair_rect(record) + at;
    if lf_repair_alpha(block, record, p) > 0.0 {
        lf_store(at, vec4<f32>(0.0, 0.0, 0.0, lf_repair_unknown));
        return;
    }
    let stage = lf_repair_stage(block);
    if lf_repair_valid(block, record, p) {
        let adjacent = (p.x > 0 && lf_repair_alpha(block, record, p - vec2<i32>(1, 0)) > 0.0)
            || (p.x + 1 < stage.x && lf_repair_alpha(block, record, p + vec2<i32>(1, 0)) > 0.0)
            || (p.y > 0 && lf_repair_alpha(block, record, p - vec2<i32>(0, 1)) > 0.0)
            || (p.y + 1 < stage.y && lf_repair_alpha(block, record, p + vec2<i32>(0, 1)) > 0.0);
        if adjacent {
            let texel = p - lf_origin();
            let i = lf_repair_encode(lf_repair_input(texel, words));
            let s = lf_repair_encode(lf_repair_input(texel + lf_repair_delta(record), words));
            lf_store(at, vec4<f32>(i - s, lf_repair_fixed));
            return;
        }
    }
    lf_store(at, vec4<f32>(0.0));
}

// A coarser level from the finer, plane 0: fixed if a child is, with the mean of its fixed
// children (the start's climb), else unknown if a child is, else outside. Words
// `[input, op, w, h, fine w, fine h]`.
fn lf_repair_heal_coarse(at: vec2<i32>, words: u32, block: u32) {
    let size = lf_repair_size(words, 2u);
    if any(at >= size) {
        lf_store(at, vec4<f32>(0.0));
        return;
    }
    let fine = lf_repair_size(words, 4u);
    var sum = vec3<f32>(0.0);
    var fixed = 0u;
    var unknown = false;
    for (var k = 0; k < 4; k++) {
        let child = lf_repair_cell(0u, 2 * at + vec2<i32>(k % 2, k / 2), fine);
        if child.a == lf_repair_fixed {
            sum += child.rgb;
            fixed += 1u;
        } else if child.a == lf_repair_unknown {
            unknown = true;
        }
    }
    if fixed > 0u {
        lf_store(at, vec4<f32>(sum / f32(fixed), lf_repair_fixed));
    } else if unknown {
        lf_store(at, vec4<f32>(0.0, 0.0, 0.0, lf_repair_unknown));
    } else {
        lf_store(at, vec4<f32>(0.0));
    }
}

// The start's descent: an unknown cell of plane 0 takes its parent's value in plane 1.
// Words `[input, op, w, h, coarse w, coarse h]`.
fn lf_repair_heal_descend(at: vec2<i32>, words: u32, block: u32) {
    let size = lf_repair_size(words, 2u);
    var cell = lf_repair_cell(0u, at, size);
    if cell.a == lf_repair_unknown {
        cell = vec4<f32>(lf_repair_cell(1u, at / 2, lf_repair_size(words, 4u)).rgb, cell.a);
    }
    lf_store(at, cell);
}

// The sum of `at`'s non-outside neighbours' values, `-x, +x, -y, +y`, added to `start`, and
// their count.
fn lf_repair_neighbours(at: vec2<i32>, size: vec2<i32>, start: vec3<f32>) -> vec4<f32> {
    var sum = start;
    var count = 0.0;
    var around = array<vec2<i32>, 4>(vec2<i32>(-1, 0), vec2<i32>(1, 0), vec2<i32>(0, -1), vec2<i32>(0, 1));
    for (var k = 0; k < 4; k++) {
        let n = lf_repair_cell(0u, at + around[k], size);
        if n.a != lf_repair_outside {
            sum += n.rgb;
            count += 1.0;
        }
    }
    return vec4<f32>(sum, count);
}

// The right-hand side at `at` of a level whose right-hand side plane is `slot`, or zero.
fn lf_repair_rhs(at: vec2<i32>, size: vec2<i32>, words: u32, slot: u32) -> vec3<f32> {
    if lf_word(words + 4u) == 0u {
        return vec3<f32>(0.0);
    }
    return lf_repair_cell(slot, at, size).rgb;
}

// Half a red-black iteration: the unknown cells of parity `p` from their neighbours and the
// right-hand side (plane 1, when the level has one). Words `[input, op, w, h, has rhs, p]`.
fn lf_repair_heal_sweep(at: vec2<i32>, words: u32, block: u32) {
    let size = lf_repair_size(words, 2u);
    var cell = lf_repair_cell(0u, at, size);
    let parity = lf_word(words + 5u);
    if cell.a == lf_repair_unknown && u32(at.x + at.y) % 2u == parity {
        let around = lf_repair_neighbours(at, size, lf_repair_rhs(at, size, words, 1u));
        if around.w > 0.0 {
            cell = vec4<f32>(around.xyz / around.w, cell.a);
        }
    }
    lf_store(at, cell);
}

// The coarse right-hand side: at each coarse unknown cell (plane 1), the sum over its unknown
// children of the fine residual `rhs - (count f - neighbours)` (plane 0 the fine values, plane 2
// its right-hand side, when it has one). Words `[input, op, w, h, has rhs, -, coarse w, coarse h]`.
fn lf_repair_heal_restrict(at: vec2<i32>, words: u32, block: u32) {
    let coarse = lf_repair_size(words, 6u);
    if lf_repair_cell(1u, at, coarse).a != lf_repair_unknown {
        lf_store(at, vec4<f32>(0.0));
        return;
    }
    let size = lf_repair_size(words, 2u);
    var sum = vec3<f32>(0.0);
    for (var k = 0; k < 4; k++) {
        let child = 2 * at + vec2<i32>(k % 2, k / 2);
        let cell = lf_repair_cell(0u, child, size);
        if cell.a == lf_repair_unknown {
            let around = lf_repair_neighbours(child, size, vec3<f32>(0.0));
            let rhs = lf_repair_rhs(child, size, words, 2u);
            sum += rhs - (around.w * cell.rgb - around.xyz);
        }
    }
    lf_store(at, vec4<f32>(sum, 0.0));
}

// A coarse level's values cleared, its classes kept. Words `[input, op, w, h]`.
fn lf_repair_heal_clear(at: vec2<i32>, words: u32, block: u32) {
    let cell = lf_repair_cell(0u, at, lf_repair_size(words, 2u));
    lf_store(at, vec4<f32>(0.0, 0.0, 0.0, cell.a));
}

// The coarse correction: an unknown fine cell whose parent (plane 1) is unknown adds its value.
// Words `[input, op, w, h, coarse w, coarse h]`.
fn lf_repair_heal_prolong(at: vec2<i32>, words: u32, block: u32) {
    let size = lf_repair_size(words, 2u);
    var cell = lf_repair_cell(0u, at, size);
    if cell.a == lf_repair_unknown {
        let parent = lf_repair_cell(1u, at / 2, lf_repair_size(words, 4u));
        if parent.a == lf_repair_unknown {
            cell = vec4<f32>(cell.rgb + parent.rgb, cell.a);
        }
    }
    lf_store(at, cell);
}

// The composite over the state: `(1 - a) I + a D(E(S) + f)` at each unknown cell of the solve
// rectangle (the finest level, plane `slot`), the state elsewhere. Words `[input, op, w, h, slot]`;
// the state is plane 0 or the step's input.
fn lf_repair_heal_composite(at: vec2<i32>, words: u32, block: u32) {
    let record = lf_repair_record(block, lf_word(words + 1u));
    let p = lf_origin() + at;
    var value = lf_repair_input(at, words);
    let cell = lf_repair_cell(lf_word(words + 4u), p - lf_repair_rect(record), lf_repair_size(words, 2u));
    if cell.a == lf_repair_unknown {
        let s = lf_repair_input(at + lf_repair_delta(record), words);
        let healed = lf_repair_decode(lf_repair_encode(s) + cell.rgb);
        let a = lf_repair_alpha(block, record, p);
        value = (1.0 - a) * value + a * healed;
    }
    lf_store(at, vec4<f32>(value, 1.0));
}
