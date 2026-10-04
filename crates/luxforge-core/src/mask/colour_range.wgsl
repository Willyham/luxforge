// The colour range's coverage on the GPU, beside its CPU field in `range.rs`: the frozen falloff
// around the nearest sampled colour in the Oklab chromaticity plane, read from the input of the
// operation the mask modulates, in the same spelling and order, evaluated in f32. The Oklab
// conversion is restated here, as a program declares every name it uses; its matrices are the
// published digits the CPU holds, and the core's tests hold them to the CPU's f32 values. Only the
// a and b rows are evaluated: the frozen metric does not read L.
//
// Words: 0 the sample count (at most five; none selects nothing), 1 the radius the refine maps to,
// then each sample's Oklab (a, b), the f64 pair the CPU field holds, narrowed to f32.

// Linear sRGB to LMS.
const lf_mask_colour_range_m1_0 = vec3<f32>(0.4122214708, 0.5363325363, 0.0514459929);
const lf_mask_colour_range_m1_1 = vec3<f32>(0.2119034982, 0.6806995451, 0.1073969566);
const lf_mask_colour_range_m1_2 = vec3<f32>(0.0883024619, 0.2817188376, 0.6299787005);
// LMS' to Oklab a and b.
const lf_mask_colour_range_m2_1 = vec3<f32>(1.9779984951, -2.4285922050, 0.4505937099);
const lf_mask_colour_range_m2_2 = vec3<f32>(0.0259040371, 0.7827717662, -0.8086757660);
// The falloff's span, 1 - the plateau. Exactly one half, so dividing by it is exact.
const lf_mask_colour_range_span: f32 = 0.5;
// The most samples one component holds.
const lf_mask_colour_range_samples: u32 = 5u;

// The frozen easing, s*s*(3 - 2s), on s already clamped to [0, 1].
fn lf_mask_colour_range_smooth(s: f32) -> f32 {
    return s * s * (3.0 - 2.0 * s);
}

// One row of a matrix-vector product, summed left to right.
fn lf_mask_colour_range_row(row: vec3<f32>, v: vec3<f32>) -> f32 {
    return row.x * v.x + row.y * v.y + row.z * v.z;
}

// The signed cube root, finite for every finite x. WGSL has no cbrt: the power function's
// estimate is refined by one Newton step, written so that no intermediate overflows.
fn lf_mask_colour_range_cbrt(x: f32) -> f32 {
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
fn lf_mask_colour_range_ab(rgb: vec3<f32>) -> vec2<f32> {
    let lms = vec3<f32>(
        lf_mask_colour_range_row(lf_mask_colour_range_m1_0, rgb),
        lf_mask_colour_range_row(lf_mask_colour_range_m1_1, rgb),
        lf_mask_colour_range_row(lf_mask_colour_range_m1_2, rgb),
    );
    let root = vec3<f32>(
        lf_mask_colour_range_cbrt(lms.x),
        lf_mask_colour_range_cbrt(lms.y),
        lf_mask_colour_range_cbrt(lms.z),
    );
    return vec2<f32>(
        lf_mask_colour_range_row(lf_mask_colour_range_m2_1, root),
        lf_mask_colour_range_row(lf_mask_colour_range_m2_2, root),
    );
}

fn lf_mask_colour_range(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {
    let count = min(lf_word(words), lf_mask_colour_range_samples);
    if count == 0u {
        return 0.0;
    }
    let ab = lf_mask_colour_range_ab(rgb);
    // The nearest sample's squared distance, folded by min as the CPU folds it.
    var d2 = 0.0;
    for (var k = 0u; k < count; k = k + 1u) {
        let da = ab.x - lf_f32(words + 2u + 2u * k);
        let db = ab.y - lf_f32(words + 3u + 2u * k);
        let candidate = da * da + db * db;
        d2 = select(min(d2, candidate), candidate, k == 0u);
    }
    let r = sqrt(d2) / lf_f32(words + 1u);
    return lf_mask_colour_range_smooth(
        clamp((1.0 - r) / lf_mask_colour_range_span, 0.0, 1.0),
    );
}
