// The Detail module's noise reduction and capture sharpening on the GPU, beside the CPU units in
// `denoise.rs` and `sharpen.rs` and the passes they share in `filters.rs`, under the spatial
// convention (`render/gpu/spatial.rs`). The passes `gpu.rs` describes fill planes with the kernels
// below; each unit's apply then reads them at the pixel it draws. The operations are the CPU
// units', in `f32` and in the CPU's order:
//
// - the unit's input in Oklab (`lf_detail_lab`);
// - separable smoothing in difference form, the horizontal pass then the vertical, every tap's
//   offset and weight a word: the B3 a-trous levels at full resolution and the sampled Gaussians at
//   a proxy's scale for noise reduction, the blur and the guide for sharpening
//   (`lf_detail_smooth_x`, `lf_detail_smooth_y`);
// - one wavelet level's band, its 3 x 3 energy and the soft shrinkage that energy gates, added to
//   the change the levels before it accumulated (`lf_detail_shrink`);
// - the sharpening residual's coring, the guide's gradient-energy gate, the 3 x 3 extrema and the
//   tanh limiter (`lf_detail_sharpen_l`);
// - the reconstruction from Oklab, the change added and a near-neutral chroma snapped to grey
//   (`lf_detail_denoise`, `lf_detail_sharpen`).
//
// Every coefficient is a word the description writes from the CPU unit's own `f32`; the constants
// below are the shared colour equations and the unit's own snap, restated, and held to the CPU's
// bits by the core's tests.

// Linear sRGB to LMS.
const lf_detail_m1_0 = vec3<f32>(0.4122214708, 0.5363325363, 0.0514459929);
const lf_detail_m1_1 = vec3<f32>(0.2119034982, 0.6806995451, 0.1073969566);
const lf_detail_m1_2 = vec3<f32>(0.0883024619, 0.2817188376, 0.6299787005);
// LMS' to Oklab.
const lf_detail_m2_0 = vec3<f32>(0.2104542553, 0.7936177850, -0.0040720468);
const lf_detail_m2_1 = vec3<f32>(1.9779984951, -2.4285922050, 0.4505937099);
const lf_detail_m2_2 = vec3<f32>(0.0259040371, 0.7827717662, -0.8086757660);
// Oklab to LMS'.
const lf_detail_m2_inv_0 = vec3<f32>(1.0, 0.3963377774, 0.2158037573);
const lf_detail_m2_inv_1 = vec3<f32>(1.0, -0.1055613458, -0.0638541728);
const lf_detail_m2_inv_2 = vec3<f32>(1.0, -0.0894841775, -1.2914855480);
// LMS to linear sRGB.
const lf_detail_m1_inv_0 = vec3<f32>(4.0767416621, -3.3077115913, 0.2309699292);
const lf_detail_m1_inv_1 = vec3<f32>(-1.2684380046, 2.6097574011, -0.3413193965);
const lf_detail_m1_inv_2 = vec3<f32>(-0.0041960863, -0.7034186147, 1.7076147010);

// A reconstructed chroma within this of zero on both axes is drawn as an exact grey.
const lf_detail_neutral_chroma_snap: f32 = 1e-6;
// tanh is 1 to every bit an f32 holds past 9.01; the argument is held inside +-16 so no
// implementation is asked for an exponent it cannot represent.
const lf_detail_tanh_bound: f32 = 16.0;

// The smoothing passes' forms: every channel through one kernel (a level of noise reduction), or
// two kernels over one channel each (sharpening's blur into x and guide into y).
const lf_detail_form_every: u32 = 0u;
const lf_detail_form_pair: u32 = 1u;

// ---- The shared colour equations ---------------------------------------------------------------

// The matrix-vector product, each row summed left to right.
fn lf_detail_rows(r0: vec3<f32>, r1: vec3<f32>, r2: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        r0.x * v.x + r0.y * v.y + r0.z * v.z,
        r1.x * v.x + r1.y * v.y + r1.z * v.z,
        r2.x * v.x + r2.y * v.y + r2.z * v.z,
    );
}

// The signed cube root, finite for every finite x. WGSL has no cbrt: the power function's
// estimate is refined by one Newton step, written so that no intermediate overflows.
fn lf_detail_cbrt(x: f32) -> f32 {
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

fn lf_detail_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = lf_detail_rows(lf_detail_m1_0, lf_detail_m1_1, lf_detail_m1_2, rgb);
    let root = vec3<f32>(lf_detail_cbrt(lms.x), lf_detail_cbrt(lms.y), lf_detail_cbrt(lms.z));
    return lf_detail_rows(lf_detail_m2_0, lf_detail_m2_1, lf_detail_m2_2, root);
}

// An achromatic colour (a = b = 0) reconstructs to L^3 in all three channels, as the CPU's does.
fn lf_detail_from_oklab(lab: vec3<f32>) -> vec3<f32> {
    if lab.y == 0.0 && lab.z == 0.0 {
        return vec3<f32>(lab.x * lab.x * lab.x);
    }
    let root = lf_detail_rows(lf_detail_m2_inv_0, lf_detail_m2_inv_1, lf_detail_m2_inv_2, lab);
    return lf_detail_rows(
        lf_detail_m1_inv_0,
        lf_detail_m1_inv_1,
        lf_detail_m1_inv_2,
        root * root * root,
    );
}

// The unit's output from its input `rgb`, the input's Oklab `lab` and the change in Oklab `delta`:
// the input itself where nothing changed, an exact grey where the changed chroma is within the snap
// of neutral, and the changed colour otherwise.
fn lf_detail_reconstruct(rgb: vec3<f32>, lab: vec3<f32>, delta: vec3<f32>) -> vec3<f32> {
    if all(delta == vec3<f32>(0.0)) {
        return rgb;
    }
    let changed = lab + delta;
    if abs(changed.y) <= lf_detail_neutral_chroma_snap
        && abs(changed.z) <= lf_detail_neutral_chroma_snap {
        return vec3<f32>(changed.x * changed.x * changed.x);
    }
    return lf_detail_from_oklab(changed);
}

// ---- The unit's input in Oklab -----------------------------------------------------------------

// Lightness is held less this in every plane the units smooth, so a half-float plane holds it
// about zero, where a half's step is finer: two to four times finer than at the lightness itself
// over most of its range. Every kernel that reads a plane takes differences of it — the
// smoothing's taps against the centre, a level's band, the residual, the gradient, the extrema
// and the change — and a smoothing's weights sum to one, so no value they compute depends on it.
const lf_detail_lightness_offset: f32 = 0.5;

// The Oklab of the unit's input at `at`: L less the offset, a and b, then zero.
fn lf_detail_lab(at: vec2<i32>, words: u32, block: u32) {
    let lab = lf_detail_to_oklab(lf_source(at));
    lf_store(at, vec4<f32>(lab.x - lf_detail_lightness_offset, lab.y, lab.z, 0.0));
}

// ---- Separable smoothing -----------------------------------------------------------------------

// One kernel at `slot` over plane 0 at `at` along `axis`, every channel: the centre plus each tap's
// weight times its difference from the centre, taps in stored order, so a constant plane is
// returned exactly. A slot is its tap count, then each tap's offset and weight.
fn lf_detail_kernel(slot: u32, at: vec2<i32>, axis: vec2<i32>) -> vec4<f32> {
    let centre = lf_plane(0u, at);
    var result = centre;
    let count = lf_word(slot);
    for (var tap = 0u; tap < count; tap++) {
        let offset = bitcast<i32>(lf_word(slot + 1u + 2u * tap));
        let weight = lf_f32(slot + 2u + 2u * tap);
        result += weight * (lf_plane(0u, at + axis * offset) - centre);
    }
    return result;
}

// The slot after the one at `slot`.
fn lf_detail_next_slot(slot: u32) -> u32 {
    return slot + 1u + 2u * lf_word(slot);
}

// One pass of a separable smoothing along `axis`. Words: 0 the form, then the kernel slots it
// reads. Every channel through the first kernel; or, for the pair form, the first kernel over
// channel x into x and the second over channel `second` into y.
fn lf_detail_smooth(at: vec2<i32>, words: u32, axis: vec2<i32>, second: u32) {
    let first = words + 1u;
    if lf_word(words) == lf_detail_form_every {
        lf_store(at, lf_detail_kernel(first, at, axis));
        return;
    }
    let blurred = lf_detail_kernel(first, at, axis).x;
    let guide = lf_detail_kernel(lf_detail_next_slot(first), at, axis)[second];
    lf_store(at, vec4<f32>(blurred, guide, 0.0, 0.0));
}

// The horizontal pass of plane 0, edge-clamped. Both of the pair form's kernels read channel x.
fn lf_detail_smooth_x(at: vec2<i32>, words: u32, block: u32) {
    lf_detail_smooth(at, words, vec2<i32>(1, 0), 0u);
}

// The vertical pass of plane 0, the horizontal pass's output, edge-clamped. The pair form's second
// kernel reads channel y, where the horizontal pass put its own.
fn lf_detail_smooth_y(at: vec2<i32>, words: u32, block: u32) {
    lf_detail_smooth(at, words, vec2<i32>(0, 1), 1u);
}

// ---- Noise reduction: one wavelet level --------------------------------------------------------

// One channel kind's garrote of band value `d` whose squared magnitude is `squared`, gated by the
// band's mean energy over 3 x 3: the change `d * factor - d`, or none where the magnitude is zero.
fn lf_detail_garrote(d: vec2<f32>, squared: f32, energy: f32, threshold: f32, detail: f32)
    -> vec2<f32> {
    let protection = energy / (energy + (3.0 * threshold) * (3.0 * threshold));
    let effective = threshold * (1.0 - detail * protection);
    if squared == 0.0 {
        return vec2<f32>(0.0);
    }
    let factor = max(1.0 - effective * effective / squared, 0.0);
    return d * factor - d;
}

// One level of the a-trous decomposition: the band `coarse - next` (planes 0 and 1), its energy
// over the 3 x 3 neighbourhood (of L, and of a and b together), and the soft shrinkage of each
// channel kind with a threshold, added to the change the levels before accumulated in plane 2,
// which the first level, with none before it, does not read. Words: 0 whether a level came before
// (1) or not (0), 1 the luminance threshold, 2 the chroma threshold, 3 the luminance detail, 4 the
// colour detail.
fn lf_detail_shrink(at: vec2<i32>, words: u32, block: u32) {
    var luminance = 0.0;
    var chroma = 0.0;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let near = at + vec2<i32>(dx, dy);
            let d = lf_plane(0u, near).xyz - lf_plane(1u, near).xyz;
            luminance += d.x * d.x;
            chroma += d.y * d.y + d.z * d.z;
        }
    }
    luminance /= 9.0;
    chroma /= 9.0;
    let d = lf_plane(0u, at).xyz - lf_plane(1u, at).xyz;
    var delta = vec3<f32>(0.0);
    if lf_word(words) == 1u {
        delta = lf_plane(2u, at).xyz;
    }
    let luminance_threshold = lf_f32(words + 1u);
    if luminance_threshold != 0.0 {
        delta.x += lf_detail_garrote(
            vec2<f32>(d.x, 0.0),
            d.x * d.x,
            luminance,
            luminance_threshold,
            lf_f32(words + 3u),
        ).x;
    }
    let chroma_threshold = lf_f32(words + 2u);
    if chroma_threshold != 0.0 {
        let change = lf_detail_garrote(
            d.yz,
            d.y * d.y + d.z * d.z,
            chroma,
            chroma_threshold,
            lf_f32(words + 4u),
        );
        delta.y += change.x;
        delta.z += change.y;
    }
    lf_store(at, vec4<f32>(delta, 0.0));
}

// ---- Capture sharpening ------------------------------------------------------------------------

// The sharpened lightness's change at `at`: plane 0 the unit's Oklab (L in x), plane 1 the blurred
// L and the guide (x and y). The residual against the blur is cored, gated by the guide's
// central-difference gradient energy, scaled, and soft-limited by tanh to the 3 x 3 extrema of L.
// Words: 0 the gain, 1 the coring threshold squared, 2 the masking scale squared (0 admits every
// edge).
fn lf_detail_sharpen_l(at: vec2<i32>, words: u32, block: u32) {
    let gain = lf_f32(words);
    let theta_squared = lf_f32(words + 1u);
    let mask_squared = lf_f32(words + 2u);
    let l = lf_plane(0u, at).x;
    let residual = l - lf_plane(1u, at).x;
    let squared = residual * residual;
    var cored = 0.0;
    if residual != 0.0 {
        // Where the squared residual is flushed to zero with no coring threshold, the quotient is
        // its limit, the residual itself, where the CPU holds the subnormal square.
        let denominator = squared + theta_squared;
        cored = select(residual * squared / denominator, residual, denominator == 0.0);
    }
    let dx = 0.5 * (lf_plane(1u, at + vec2<i32>(1, 0)).y - lf_plane(1u, at - vec2<i32>(1, 0)).y);
    let dy = 0.5 * (lf_plane(1u, at + vec2<i32>(0, 1)).y - lf_plane(1u, at - vec2<i32>(0, 1)).y);
    let energy = dx * dx + dy * dy;
    var mask = 1.0;
    if mask_squared != 0.0 {
        mask = energy / (energy + mask_squared);
    }
    let proposed = l + gain * cored * mask;
    var lo = l;
    var hi = l;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let value = lf_plane(0u, at + vec2<i32>(x, y)).x;
            lo = min(lo, value);
            hi = max(hi, value);
        }
    }
    let limit = 0.04 * (hi - lo);
    var limited = proposed;
    if limit == 0.0 {
        limited = l;
    } else if proposed > hi {
        limited = hi + limit * tanh(min((proposed - hi) / limit, lf_detail_tanh_bound));
    } else if proposed < lo {
        limited = lo + limit * tanh(max((proposed - lo) / limit, -lf_detail_tanh_bound));
    }
    lf_store(at, vec4<f32>(limited - l, 0.0, 0.0, 0.0));
}

// ---- The applies -------------------------------------------------------------------------------

// Noise reduction: the change every level accumulated, in plane `planes`, added to the input's
// Oklab.
fn lf_detail_denoise(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    let delta = lf_plane(planes, at).xyz;
    if all(delta == vec3<f32>(0.0)) {
        return rgb;
    }
    return lf_detail_reconstruct(rgb, lf_detail_to_oklab(rgb), delta);
}

// Capture sharpening: the change of lightness alone, in plane `planes`, added to the input's L.
fn lf_detail_sharpen(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    let change = lf_plane(planes, at).x;
    if change == 0.0 {
        return rgb;
    }
    return lf_detail_reconstruct(rgb, lf_detail_to_oklab(rgb), vec3<f32>(change, 0.0, 0.0));
}
