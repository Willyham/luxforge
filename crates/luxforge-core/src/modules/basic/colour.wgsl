// The Basic module's colour unit on the GPU, beside its CPU unit in `colour.rs`: Vibrance and
// Saturation fused into one gain on Oklab a and b, with L untouched and one round trip through
// Oklab. The conversion is the one `crate::colour::oklab` holds, restated here because a program
// declares every name it uses; its matrices are the same published digits, narrowed to f32 as
// that module narrows them. Nothing is clamped.
//
// Words: 0 vibrance / 100 (exactly 0 when Vibrance is neutral, which skips the per-pixel weight),
// 1 Saturation's gain 1 + saturation / 100. Each is the f32 the CPU unit holds.

// Linear sRGB to LMS.
const lf_basic_colour_adjust_m1_0 = vec3<f32>(0.4122214708, 0.5363325363, 0.0514459929);
const lf_basic_colour_adjust_m1_1 = vec3<f32>(0.2119034982, 0.6806995451, 0.1073969566);
const lf_basic_colour_adjust_m1_2 = vec3<f32>(0.0883024619, 0.2817188376, 0.6299787005);
// LMS' to Oklab.
const lf_basic_colour_adjust_m2_0 = vec3<f32>(0.2104542553, 0.7936177850, -0.0040720468);
const lf_basic_colour_adjust_m2_1 = vec3<f32>(1.9779984951, -2.4285922050, 0.4505937099);
const lf_basic_colour_adjust_m2_2 = vec3<f32>(0.0259040371, 0.7827717662, -0.8086757660);
// Oklab to LMS'.
const lf_basic_colour_adjust_m2_inv_0 = vec3<f32>(1.0, 0.3963377774, 0.2158037573);
const lf_basic_colour_adjust_m2_inv_1 = vec3<f32>(1.0, -0.1055613458, -0.0638541728);
const lf_basic_colour_adjust_m2_inv_2 = vec3<f32>(1.0, -0.0894841775, -1.2914855480);
// LMS to linear sRGB.
const lf_basic_colour_adjust_m1_inv_0 = vec3<f32>(4.0767416621, -3.3077115913, 0.2309699292);
const lf_basic_colour_adjust_m1_inv_1 = vec3<f32>(-1.2684380046, 2.6097574011, -0.3413193965);
const lf_basic_colour_adjust_m1_inv_2 = vec3<f32>(-0.0041960863, -0.7034186147, 1.7076147010);

// The vibrance weight's constants, as `colour.rs` narrows them.
const lf_basic_colour_adjust_chroma_reference: f32 = 0.32;
const lf_basic_colour_adjust_chroma_low: f32 = 0.10;
const lf_basic_colour_adjust_chroma_high: f32 = 0.70;
const lf_basic_colour_adjust_skin_centre: f32 = 55.0;
const lf_basic_colour_adjust_skin_half_width: f32 = 35.0;
const lf_basic_colour_adjust_skin_protection: f32 = 0.6;
const lf_basic_colour_adjust_chroma_epsilon: f32 = 1e-4;

// The matrix-vector product, each row summed left to right.
fn lf_basic_colour_adjust_rows(r0: vec3<f32>, r1: vec3<f32>, r2: vec3<f32>, v: vec3<f32>)
    -> vec3<f32> {
    return vec3<f32>(
        r0.x * v.x + r0.y * v.y + r0.z * v.z,
        r1.x * v.x + r1.y * v.y + r1.z * v.z,
        r2.x * v.x + r2.y * v.y + r2.z * v.z,
    );
}

// The signed cube root, finite for every finite x. WGSL has no cbrt: the power function's
// estimate is refined by one Newton step, written so that no intermediate overflows.
fn lf_basic_colour_adjust_cbrt(x: f32) -> f32 {
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

fn lf_basic_colour_adjust_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = lf_basic_colour_adjust_rows(
        lf_basic_colour_adjust_m1_0,
        lf_basic_colour_adjust_m1_1,
        lf_basic_colour_adjust_m1_2,
        rgb,
    );
    let root = vec3<f32>(
        lf_basic_colour_adjust_cbrt(lms.x),
        lf_basic_colour_adjust_cbrt(lms.y),
        lf_basic_colour_adjust_cbrt(lms.z),
    );
    return lf_basic_colour_adjust_rows(
        lf_basic_colour_adjust_m2_0,
        lf_basic_colour_adjust_m2_1,
        lf_basic_colour_adjust_m2_2,
        root,
    );
}

// An achromatic colour (a = b = 0) reconstructs to L^3 in all three channels, as the CPU's does.
fn lf_basic_colour_adjust_from_oklab(lab: vec3<f32>) -> vec3<f32> {
    if lab.y == 0.0 && lab.z == 0.0 {
        return vec3<f32>(lab.x * lab.x * lab.x);
    }
    let root = lf_basic_colour_adjust_rows(
        lf_basic_colour_adjust_m2_inv_0,
        lf_basic_colour_adjust_m2_inv_1,
        lf_basic_colour_adjust_m2_inv_2,
        lab,
    );
    return lf_basic_colour_adjust_rows(
        lf_basic_colour_adjust_m1_inv_0,
        lf_basic_colour_adjust_m1_inv_1,
        lf_basic_colour_adjust_m1_inv_2,
        root * root * root,
    );
}

fn lf_basic_colour_adjust_smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// w(C, h) = w_c(C) * w_h(h), in [0, 1].
fn lf_basic_colour_adjust_weight(chroma: f32, hue_degrees: f32) -> f32 {
    let normalized = max(chroma / lf_basic_colour_adjust_chroma_reference, 0.0);
    let chroma_weight = 1.0 - lf_basic_colour_adjust_smoothstep(
        lf_basic_colour_adjust_chroma_low,
        lf_basic_colour_adjust_chroma_high,
        normalized,
    );
    let delta = hue_degrees - lf_basic_colour_adjust_skin_centre;
    var response = 0.0;
    if abs(delta) < lf_basic_colour_adjust_skin_half_width {
        response = max(
            cos(delta / lf_basic_colour_adjust_skin_half_width * 1.5707963267948966),
            0.0,
        );
    }
    return chroma_weight * (1.0 - lf_basic_colour_adjust_skin_protection * response);
}

fn lf_basic_colour_adjust(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let lab = lf_basic_colour_adjust_to_oklab(rgb);
    let vibrance = lf_f32(words);
    var vibrance_k = 1.0;
    if vibrance != 0.0 {
        let chroma = sqrt(lab.y * lab.y + lab.z * lab.z);
        var weight = 1.0;
        if chroma >= lf_basic_colour_adjust_chroma_epsilon {
            weight = lf_basic_colour_adjust_weight(chroma, atan2(lab.z, lab.y) * 57.29577951308232);
        }
        vibrance_k = 1.0 + vibrance * weight;
    }
    let k = vibrance_k * lf_f32(words + 1u);
    return lf_basic_colour_adjust_from_oklab(vec3<f32>(lab.x, lab.y * k, lab.z * k));
}
