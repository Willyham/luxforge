// The Presence module's Texture, Clarity and Dehaze on the GPU, beside the CPU units in
// `texture.rs`, `clarity.rs` and `dehaze.rs` and the filters they share in `filters.rs`, under the
// spatial convention (`render/gpu/spatial.rs`). The passes `gpu.rs` describes fill planes with the
// kernels below; each unit's apply then reads them at the pixel it draws. The operations are the
// CPU filters', in `f32`:
//
// - separable box means, the horizontal pass then the vertical, each a running sum along its run
//   of outputs, reseeded by a direct sum where each run starts (`lf_presence_sum_x`,
//   `lf_presence_sum_y`);
// - separable box minima, by direct comparison over the window (`lf_presence_min_x` and the
//   vertical minimum in `lf_presence_dehaze_transmission`);
// - the self-guided and guided filters' coefficients and smoothing, as finishes of the vertical
//   pass;
// - the 4x block reduction anchored at the stage origin, and the 16x one a global estimate is
//   taken from (`lf_presence_reduce`), and the bilinear upsample (`lf_presence_upsample`);
// - the compressive soft clip (`lf_presence_soft_clip`);
// - the atmospheric light, from the brightest dark-channel blocks (`lf_presence_atmosphere`).
//
// Every coefficient is a word the description writes from the CPU unit's own `f32`; the constants
// below are the shared colour equations, restated, and held to the CPU's bits by the core's tests.

const lf_presence_luma_r: f32 = 0.2126;
const lf_presence_luma_g: f32 = 0.7152;
const lf_presence_luma_b: f32 = 0.0722;
const lf_presence_near_black: f32 = 1e-6;
// Texture's fine smoother of the encoded luminance is held less this, so a half-float plane holds
// it about zero, where a half's step is two to four times finer over most of its range; the band
// adds it back before it takes the coarse smoother from it.
const lf_presence_fine_offset: f32 = 0.5;
// tanh is 1 to every bit an f32 holds past 9.01; the argument is held inside +-16 so no
// implementation is asked for an exponent it cannot represent.
const lf_presence_tanh_bound: f32 = 16.0;

// The sum's forms: what one tap of the horizontal pass adds.
const lf_presence_form_plane: u32 = 0u;
const lf_presence_form_square: u32 = 1u;
const lf_presence_form_encoded: u32 = 2u;
const lf_presence_form_guided: u32 = 3u;

// The vertical pass's finishes: what it stores from the window's mean.
const lf_presence_finish_mean: u32 = 0u;
const lf_presence_finish_self: u32 = 1u;
const lf_presence_finish_guided: u32 = 2u;
const lf_presence_finish_smooth_encoded: u32 = 3u;
const lf_presence_finish_smooth_plane: u32 = 4u;
const lf_presence_finish_band_encoded: u32 = 5u;

// The reductions' forms.
const lf_presence_reduce_encoded: u32 = 0u;
const lf_presence_reduce_dehaze: u32 = 1u;
const lf_presence_reduce_dark: u32 = 2u;
const lf_presence_reduce_none: u32 = 3u;

// Dehaze's apply modes.
const lf_presence_dehaze_add: u32 = 0u;
const lf_presence_dehaze_remove: u32 = 1u;
const lf_presence_dehaze_none: u32 = 2u;

// ---- The shared colour equations ---------------------------------------------------------------

fn lf_presence_encode(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        return 12.92 * linear;
    }
    return 1.055 * pow(linear, 1.0f / 2.4f) - 0.055;
}

fn lf_presence_decode(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

fn lf_presence_luma(rgb: vec3<f32>) -> f32 {
    return lf_presence_luma_r * rgb.x + lf_presence_luma_g * rgb.y + lf_presence_luma_b * rgb.z;
}

// Encoded luminance, the luminance units' working domain.
fn lf_presence_encoded(rgb: vec3<f32>) -> f32 {
    return lf_presence_encode(lf_presence_luma(rgb));
}

// The luminance-ratio reconstruction, with the additive rule near black.
fn lf_presence_reconstruct(rgb: vec3<f32>, l_in: f32, l_out: f32) -> vec3<f32> {
    if abs(l_in) < lf_presence_near_black {
        return rgb + vec3<f32>(l_out - l_in);
    }
    return rgb * (l_out / l_in);
}

// The compressive gain: `lim * tanh(raw / lim)` with `lim` the limit held inside the headroom,
// exactly zero for a zero excursion or no headroom.
fn lf_presence_soft_clip(raw: f32, encoded: f32, limit: f32) -> f32 {
    if raw == 0.0 {
        return 0.0;
    }
    var headroom = encoded;
    if raw > 0.0 {
        headroom = 1.0 - encoded;
    }
    let lim = min(limit, max(headroom, 0.0));
    if lim <= 0.0 {
        return 0.0;
    }
    return lim * tanh(clamp(raw / lim, -lf_presence_tanh_bound, lf_presence_tanh_bound));
}

// The largest f32, the start of a minimum over finite values.
fn lf_presence_largest() -> f32 {
    return bitcast<f32>(0x7f7fffffu);
}

// ---- Separable box means -----------------------------------------------------------------------

// One tap of the horizontal pass at texel `at`.
fn lf_presence_tap(form: u32, at: vec2<i32>) -> vec4<f32> {
    switch form {
        case 1u: {
            let value = lf_plane(0u, at).x;
            return vec4<f32>(value, value * value, 0.0, 0.0);
        }
        case 2u: {
            let encoded = lf_presence_encoded(lf_source(at));
            return vec4<f32>(encoded, encoded * encoded, 0.0, 0.0);
        }
        case 3u: {
            let pair = lf_plane(0u, at);
            return vec4<f32>(pair.x, pair.y, pair.x * pair.x, pair.x * pair.y);
        }
        default: {
            return lf_plane(0u, at);
        }
    }
}

// The texels the horizontal pass's input covers: the boundary for the encoded source, else its
// input plane.
fn lf_presence_extent(form: u32) -> vec2<i32> {
    if form == lf_presence_form_encoded {
        return lf_size();
    }
    return lf_plane_size(0u);
}

// The horizontal mean over 2r + 1 columns of each tap, edge-clamped, for `run` outputs from `at`:
// a direct sum where the run starts, then the entering tap added and the leaving one subtracted.
// Words: 0 the form, 1 r, 2 the run.
fn lf_presence_sum_x(at: vec2<i32>, words: u32, block: u32) {
    let form = lf_word(words);
    let r = i32(lf_word(words + 1u));
    let run = i32(lf_word(words + 2u));
    let n = f32(2 * r + 1);
    let end = min(at.x + run, lf_presence_extent(form).x);
    var sum = vec4<f32>(0.0);
    for (var dx = -r; dx <= r; dx++) {
        sum += lf_presence_tap(form, vec2<i32>(at.x + dx, at.y));
    }
    lf_store(at, sum / n);
    for (var x = at.x + 1; x < end; x++) {
        sum += lf_presence_tap(form, vec2<i32>(x + r, at.y))
            - lf_presence_tap(form, vec2<i32>(x - 1 - r, at.y));
        lf_store(vec2<i32>(x, at.y), sum / n);
    }
}

// What the vertical pass stores from the mean of its window at `at`.
fn lf_presence_finish(finish: u32, mean: vec4<f32>, at: vec2<i32>, eps: f32) -> vec4<f32> {
    switch finish {
        case 1u: {
            // The self-guided filter's coefficients: a = var / (var + eps), b = (1 - a) mean.
            let m = mean.x;
            let variance = max(mean.y - m * m, 0.0);
            let a = variance / (variance + eps);
            return vec4<f32>(a, (1.0 - a) * m, 0.0, 0.0);
        }
        case 2u: {
            // The guided filter's: a = cov / (var + eps), b = mean(I) - a mean(G).
            let mg = mean.x;
            let mi = mean.y;
            let variance = max(mean.z - mg * mg, 0.0);
            let covariance = mean.w - mg * mi;
            let a = covariance / (variance + eps);
            return vec4<f32>(a, mi - a * mg, 0.0, 0.0);
        }
        case 3u: {
            // q = mean(b) + mean(a) G, the guide the encoded source: Texture's fine smoother, held
            // less the offset for the band to add back.
            let guide = lf_presence_encoded(lf_source(at));
            return vec4<f32>(mean.y + mean.x * guide - lf_presence_fine_offset, 0.0, 0.0, 0.0);
        }
        case 4u: {
            // The same with the guide in plane 1.
            return vec4<f32>(mean.y + mean.x * lf_plane(1u, at).x, 0.0, 0.0, 0.0);
        }
        case 5u: {
            // Texture's band: the fine smoother in plane 1 less this, the coarse one, of the
            // encoded source, so the coarse smoother is never held.
            let guide = lf_presence_encoded(lf_source(at));
            let fine = lf_plane(1u, at).x + lf_presence_fine_offset;
            return vec4<f32>(fine - (mean.y + mean.x * guide), 0.0, 0.0, 0.0);
        }
        default: {
            return mean;
        }
    }
}

// The vertical mean over 2r + 1 rows of plane 0, a running sum along `run` outputs from `at`,
// finished and stored. Words: 0 the finish, 1 r, 2 the run, 3 eps.
fn lf_presence_sum_y(at: vec2<i32>, words: u32, block: u32) {
    let finish = lf_word(words);
    let r = i32(lf_word(words + 1u));
    let run = i32(lf_word(words + 2u));
    let eps = lf_f32(words + 3u);
    let n = f32(2 * r + 1);
    let end = min(at.y + run, lf_plane_size(0u).y);
    var sum = vec4<f32>(0.0);
    for (var dy = -r; dy <= r; dy++) {
        sum += lf_plane(0u, vec2<i32>(at.x, at.y + dy));
    }
    lf_store(at, lf_presence_finish(finish, sum / n, at, eps));
    for (var y = at.y + 1; y < end; y++) {
        sum += lf_plane(0u, vec2<i32>(at.x, y + r)) - lf_plane(0u, vec2<i32>(at.x, y - 1 - r));
        let here = vec2<i32>(at.x, y);
        lf_store(here, lf_presence_finish(finish, sum / n, here, eps));
    }
}

// ---- Separable box minima ----------------------------------------------------------------------

// The horizontal minimum over 2r + 1 columns of one channel of plane 0. Words: 0 the channel, 1 r.
fn lf_presence_min_x(at: vec2<i32>, words: u32, block: u32) {
    let channel = lf_word(words);
    let r = i32(lf_word(words + 1u));
    var value = lf_presence_largest();
    for (var dx = -r; dx <= r; dx++) {
        value = min(value, lf_plane(0u, vec2<i32>(at.x + dx, at.y))[channel]);
    }
    lf_store(at, vec4<f32>(value, 0.0, 0.0, 0.0));
}

// Dehaze's raw transmission and guide on its reduced grid: the vertical minimum over 2r + 1 rows
// of plane 0's horizontal minima is the dark channel, `t = 1 - omega dark`, and the guide is the
// encoded luminance of the reduced colour in plane 1. Stores `(guide, t)`. Words: 0 r, 1 omega.
fn lf_presence_dehaze_transmission(at: vec2<i32>, words: u32, block: u32) {
    let r = i32(lf_word(words));
    let omega = lf_f32(words + 1u);
    var dark = lf_presence_largest();
    for (var dy = -r; dy <= r; dy++) {
        dark = min(dark, lf_plane(0u, vec2<i32>(at.x, at.y + dy)).x);
    }
    let guide = lf_presence_encoded(lf_plane(1u, at).xyz);
    lf_store(at, vec4<f32>(guide, 1.0 - omega * dark, 0.0, 0.0));
}

// ---- Reductions --------------------------------------------------------------------------------

// The boundary texels `[x0, y0) .. [x1, y1)` reduced texel `at` of a plane reduced by `s` averages:
// its block of the stage, anchored at the stage origin, cut to the boundary.
fn lf_presence_block(at: vec2<i32>, s: i32) -> vec4<i32> {
    let origin = lf_origin();
    let first = origin / vec2<i32>(s) + at;
    let low = max(first * s - origin, vec2<i32>(0));
    let high = min((first + vec2<i32>(1)) * s - origin, lf_size());
    return vec4<i32>(low, high);
}

// The mean over one block of the unit's input, stored by form: the encoded luminance's mean
// (Clarity); the colour's mean beside its dark channel normalized by the atmospheric light in plane
// 0, `min_c clamp(mean_c / A_c, 0, 1)` (Dehaze); or the colour's mean beside its channel minimum,
// which the atmospheric light is chosen by (the global estimate); or nothing, where the light the
// estimate is for is stored. Words: 0 the form, 1 s.
fn lf_presence_reduce(at: vec2<i32>, words: u32, block: u32) {
    let form = lf_word(words);
    if form == lf_presence_reduce_none {
        return;
    }
    let s = i32(lf_word(words + 1u));
    let cut = lf_presence_block(at, s);
    var sum = vec3<f32>(0.0);
    var count = 0.0;
    for (var y = cut.y; y < cut.w; y++) {
        for (var x = cut.x; x < cut.z; x++) {
            let rgb = lf_source(vec2<i32>(x, y));
            if form == lf_presence_reduce_encoded {
                sum.x += lf_presence_encoded(rgb);
            } else {
                sum += rgb;
            }
            count += 1.0;
        }
    }
    let mean = sum / count;
    if form == lf_presence_reduce_encoded {
        lf_store(at, vec4<f32>(mean.x, 0.0, 0.0, 0.0));
    } else if form == lf_presence_reduce_dehaze {
        let normalized = clamp(mean / lf_plane(0u, vec2<i32>(0)).xyz, vec3<f32>(0.0), vec3<f32>(1.0));
        lf_store(at, vec4<f32>(mean, min(normalized.x, min(normalized.y, normalized.z))));
    } else {
        lf_store(at, vec4<f32>(mean, min(mean.x, min(mean.y, mean.z))));
    }
}

// ---- The atmospheric light ---------------------------------------------------------------------

// An f32's order as an unsigned key: larger keys for larger values, -0 below +0, as the CPU's
// total order sorts them.
fn lf_presence_key(value: f32) -> u32 {
    let bits = bitcast<u32>(value);
    if (bits & 0x80000000u) != 0u {
        return ~bits;
    }
    return bits | 0x80000000u;
}

// The sum of `lf_shared[0..256]` over the workgroup, which every lane reads.
fn lf_presence_total(lane: u32, value: f32) -> f32 {
    lf_shared[lane] = value;
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride = stride >> 1u) {
        if lane < stride {
            lf_shared[lane] += lf_shared[lane + stride];
        }
        workgroupBarrier();
    }
    let total = lf_shared[0];
    workgroupBarrier();
    return total;
}

// How many of this lane's blocks have a key at or above `threshold`.
fn lf_presence_at_or_above(lane: u32, chunk: u32, n: u32, width: u32, threshold: u32) -> f32 {
    var count = 0.0;
    let end = min((lane + 1u) * chunk, n);
    for (var index = lane * chunk; index < end; index++) {
        let at = vec2<i32>(i32(index % width), i32(index / width));
        if lf_presence_key(lf_plane(0u, at).w) >= threshold {
            count += 1.0;
        }
    }
    return count;
}

// The atmospheric light from plane 0, the 16x reduction of the unit's input with each block's
// channel minimum beside its colour: the mean colour of the brightest `max(min_count, ceil(n /
// divisor))` blocks by that minimum, ties taken in row-major order, each channel floored. Every
// lane takes a contiguous run of blocks; the threshold is found by bisecting the keys, so the
// selection is the CPU's exactly. Stores `(A, 1)` at texel (0, 0). Words: 0 the divisor (the
// reciprocal of the selected fraction), 1 the minimum count, 2 the floor, 3 whether the light is
// given (1) or not (0), 4..6 the light given.
//
// A given light, the one the CPU stored, is written in place of one found: plane 0 holds nothing
// then, and the search runs over no blocks, because every lane must still reach each barrier.
fn lf_presence_atmosphere(at: vec2<i32>, words: u32, block: u32) {
    let lane = u32(at.x);
    let size = lf_plane_size(0u);
    let width = u32(size.x);
    let given = lf_word(words + 3u) == 1u;
    let n = select(width * u32(size.y), 0u, given);
    let divisor = lf_word(words);
    let wanted = clamp(max(lf_word(words + 1u), (n + divisor - 1u) / divisor), 1u, n);
    let chunk = (n + 255u) / 256u;
    // The largest key with at least `wanted` blocks at or above it.
    var low = 0u;
    var high = 0xffffffffu;
    for (var step = 0u; step < 32u; step++) {
        let mid = low + ((high - low) >> 1u) + ((high - low) & 1u);
        let count = lf_presence_total(lane, lf_presence_at_or_above(lane, chunk, n, width, mid));
        if low < high {
            if count >= f32(wanted) {
                low = mid;
            } else {
                high = mid - 1u;
            }
        }
    }
    // Every lane reaches each barrier: the sum is taken whatever the threshold, and only its
    // meaning depends on it. At the largest key nothing lies above.
    let top = low == 0xffffffffu;
    let next = low + select(1u, 0u, top);
    let above = select(
        lf_presence_total(lane, lf_presence_at_or_above(lane, chunk, n, width, next)),
        0.0,
        top,
    );
    let needed = f32(wanted) - above;
    // The ties at the threshold, taken from the lowest index: this lane's count of them, then how
    // many the lanes before it hold.
    var ties = 0.0;
    let end = min((lane + 1u) * chunk, n);
    for (var index = lane * chunk; index < end; index++) {
        let at = vec2<i32>(i32(index % width), i32(index / width));
        if lf_presence_key(lf_plane(0u, at).w) == low {
            ties += 1.0;
        }
    }
    lf_shared[lane] = ties;
    workgroupBarrier();
    var before = 0.0;
    for (var other = 0u; other < lane; other++) {
        before += lf_shared[other];
    }
    workgroupBarrier();
    var take = clamp(needed - before, 0.0, ties);
    var sum = vec3<f32>(0.0);
    for (var index = lane * chunk; index < end; index++) {
        let at = vec2<i32>(i32(index % width), i32(index / width));
        let texel = lf_plane(0u, at);
        let key = lf_presence_key(texel.w);
        if key > low {
            sum += texel.xyz;
        } else if key == low && take > 0.0 {
            sum += texel.xyz;
            take -= 1.0;
        }
    }
    let red = lf_presence_total(lane, sum.x);
    let green = lf_presence_total(lane, sum.y);
    let blue = lf_presence_total(lane, sum.z);
    let found = max(vec3<f32>(red, green, blue) / f32(wanted), vec3<f32>(lf_f32(words + 2u)));
    let light = select(
        found,
        vec3<f32>(lf_f32(words + 4u), lf_f32(words + 5u), lf_f32(words + 6u)),
        given,
    );
    if lane == 0u {
        lf_store(vec2<i32>(0), vec4<f32>(light, 1.0));
    }
}

// ---- The applies -------------------------------------------------------------------------------

// The bilinear upsample of a plane reduced by `s` at boundary texel `at`: the stage pixel's centre
// on the reduced grid, `(x + 0.5) / s - 0.5`, between the reduced texels around it, edge-clamped.
fn lf_presence_upsample(slot: u32, at: vec2<i32>, s: f32) -> f32 {
    let origin = lf_origin();
    let u = (vec2<f32>(origin + at) + 0.5) / s - 0.5;
    let low = floor(u);
    let f = u - low;
    let base = vec2<i32>(low) - origin / vec2<i32>(i32(s));
    let top = lf_plane(slot, base).x * (1.0 - f.x) + lf_plane(slot, base + vec2<i32>(1, 0)).x * f.x;
    let bottom = lf_plane(slot, base + vec2<i32>(0, 1)).x * (1.0 - f.x)
        + lf_plane(slot, base + vec2<i32>(1, 1)).x * f.x;
    return top * (1.0 - f.y) + bottom * f.y;
}

// Dehaze: the transmission refined on the reduced grid in plane `planes`, upsampled and held in
// [floor, 1], inverts the veil for a positive amount and deepens it for a negative one, with the
// atmospheric light in plane `planes + 1`, and leaves its input alone for an amount of 0. Words: 0
// the mode (add, remove or none), 1 the veil factor, 2 the transmission floor, 3 the reduction.
fn lf_presence_dehaze(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    let mode = lf_word(words);
    if mode == lf_presence_dehaze_none {
        return rgb;
    }
    let t = clamp(lf_presence_upsample(planes, at, lf_f32(words + 3u)), lf_f32(words + 2u), 1.0);
    let light = lf_plane(planes + 1u, vec2<i32>(0)).xyz;
    if mode == lf_presence_dehaze_remove {
        return (rgb - light) / t + light;
    }
    let veil = t * lf_f32(words + 1u);
    return veil * rgb + (1.0 - veil) * light;
}

// Texture: the band between the fine and coarse self-guided smoothers of the encoded luminance,
// held in plane `planes`, scaled, soft-clipped and reapplied. Words: 0 the gain, 1 the limit.
fn lf_presence_texture(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    let encoded = lf_presence_encoded(rgb);
    let band = lf_plane(planes, at).x;
    let delta = lf_presence_soft_clip(lf_f32(words) * band, encoded, lf_f32(words + 1u));
    if delta == 0.0 {
        return rgb;
    }
    return lf_presence_reconstruct(rgb, lf_presence_luma(rgb), lf_presence_decode(encoded + delta));
}

// Clarity: the residual against the base smoothed on the reduced grid in plane `planes`,
// upsampled, scaled, soft-clipped and reapplied. Words: 0 the gain, 1 the limit, 2 the reduction.
fn lf_presence_clarity(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    let encoded = lf_presence_encoded(rgb);
    let residual = encoded - lf_presence_upsample(planes, at, lf_f32(words + 2u));
    let delta = lf_presence_soft_clip(lf_f32(words) * residual, encoded, lf_f32(words + 1u));
    if delta == 0.0 {
        return rgb;
    }
    return lf_presence_reconstruct(rgb, lf_presence_luma(rgb), lf_presence_decode(encoded + delta));
}
