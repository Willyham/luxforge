// The colour mixer's grading unit on the GPU, beside its CPU unit in `grade.rs`: one Oklab round
// trip per pixel, the tonal weights read once from the input lightness, the luminance responses and
// the enveloped tint, in the same f32 arithmetic and order as `Coefficients::apply`. The Oklab
// conversion is `crate::colour::oklab`'s, restated because a program declares every name it uses;
// its matrices are the same published digits, narrowed to f32 as that module narrows them.
//
// Words: 0 and 1 the shadow/midtone and midtone/highlight boundaries; 2 the tint weights' inverse
// width; 3 the luminance weights' inverse width; 4 to 11 the shadows, midtones, highlights and
// global (a, b) tints; 12 to 15 their luminance exponent amounts; 16 the black lift; 17 the white
// dim. Each is the f32 the CPU unit holds.

// Linear sRGB to LMS.
const lf_mixer_grade_m1_0 = vec3<f32>(0.4122214708, 0.5363325363, 0.0514459929);
const lf_mixer_grade_m1_1 = vec3<f32>(0.2119034982, 0.6806995451, 0.1073969566);
const lf_mixer_grade_m1_2 = vec3<f32>(0.0883024619, 0.2817188376, 0.6299787005);
// LMS' to Oklab.
const lf_mixer_grade_m2_0 = vec3<f32>(0.2104542553, 0.7936177850, -0.0040720468);
const lf_mixer_grade_m2_1 = vec3<f32>(1.9779984951, -2.4285922050, 0.4505937099);
const lf_mixer_grade_m2_2 = vec3<f32>(0.0259040371, 0.7827717662, -0.8086757660);
// Oklab to LMS'.
const lf_mixer_grade_m2_inv_0 = vec3<f32>(1.0, 0.3963377774, 0.2158037573);
const lf_mixer_grade_m2_inv_1 = vec3<f32>(1.0, -0.1055613458, -0.0638541728);
const lf_mixer_grade_m2_inv_2 = vec3<f32>(1.0, -0.0894841775, -1.2914855480);
// LMS to linear sRGB.
const lf_mixer_grade_m1_inv_0 = vec3<f32>(4.0767416621, -3.3077115913, 0.2309699292);
const lf_mixer_grade_m1_inv_1 = vec3<f32>(-1.2684380046, 2.6097574011, -0.3413193965);
const lf_mixer_grade_m1_inv_2 = vec3<f32>(-0.0041960863, -0.7034186147, 1.7076147010);

// The matrix-vector product, each row summed left to right.
fn lf_mixer_grade_rows(r0: vec3<f32>, r1: vec3<f32>, r2: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        r0.x * v.x + r0.y * v.y + r0.z * v.z,
        r1.x * v.x + r1.y * v.y + r1.z * v.z,
        r2.x * v.x + r2.y * v.y + r2.z * v.z,
    );
}

// The signed cube root, finite for every finite x. WGSL has no cbrt: the power function's
// estimate is refined by one Newton step, written so that no intermediate overflows.
fn lf_mixer_grade_cbrt(x: f32) -> f32 {
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

fn lf_mixer_grade_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = lf_mixer_grade_rows(lf_mixer_grade_m1_0, lf_mixer_grade_m1_1, lf_mixer_grade_m1_2, rgb);
    let root = vec3<f32>(
        lf_mixer_grade_cbrt(lms.x),
        lf_mixer_grade_cbrt(lms.y),
        lf_mixer_grade_cbrt(lms.z),
    );
    return lf_mixer_grade_rows(lf_mixer_grade_m2_0, lf_mixer_grade_m2_1, lf_mixer_grade_m2_2, root);
}

// An achromatic colour (a = b = 0) reconstructs to L^3 in all three channels, as the CPU's does.
fn lf_mixer_grade_from_oklab(lab: vec3<f32>) -> vec3<f32> {
    if lab.y == 0.0 && lab.z == 0.0 {
        return vec3<f32>(lab.x * lab.x * lab.x);
    }
    let root = lf_mixer_grade_rows(
        lf_mixer_grade_m2_inv_0,
        lf_mixer_grade_m2_inv_1,
        lf_mixer_grade_m2_inv_2,
        lab,
    );
    return lf_mixer_grade_rows(
        lf_mixer_grade_m1_inv_0,
        lf_mixer_grade_m1_inv_1,
        lf_mixer_grade_m1_inv_2,
        root * root * root,
    );
}

// The cubic smoothstep of x clamped to [0, 1].
fn lf_mixer_grade_smooth(x: f32) -> f32 {
    let c = clamp(x, 0.0, 1.0);
    return c * c * (3.0 - 2.0 * c);
}

// The shadow, midtone and highlight weights of t at one inverse width.
fn lf_mixer_grade_weights(t: f32, inverse_width: f32, words: u32) -> vec3<f32> {
    let rise = lf_mixer_grade_smooth((t - lf_f32(words)) * inverse_width + 0.5);
    let high = lf_mixer_grade_smooth((t - lf_f32(words + 1u)) * inverse_width + 0.5);
    return vec3<f32>(1.0 - rise, rise - high, high);
}

// The gamma 2^(-amount) on the [0, 1] part of l, any excess passed through.
fn lf_mixer_grade_gamma(l: f32, amount: f32) -> f32 {
    let core = clamp(l, 0.0, 1.0);
    var lifted = 0.0;
    if core > 0.0 {
        lifted = pow(core, exp2(-amount));
    }
    return lifted + (l - core);
}

fn lf_mixer_grade(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let lab = lf_mixer_grade_to_oklab(rgb);
    let t = clamp(lab.x, 0.0, 1.0);
    let w = lf_mixer_grade_weights(t, lf_f32(words + 2u), words);
    let v = lf_mixer_grade_weights(t, lf_f32(words + 3u), words);
    let amount = v.x * lf_f32(words + 12u) + v.y * lf_f32(words + 13u) + v.z * lf_f32(words + 14u);
    var l = lab.x;
    if amount != 0.0 {
        l = lf_mixer_grade_gamma(l, amount);
    }
    let lift = lf_f32(words + 16u);
    let dim = lf_f32(words + 17u);
    if lift != 0.0 || dim != 0.0 {
        let core = clamp(l, 0.0, 1.0);
        l = lift + (1.0 - lift - dim) * core + (l - core);
    }
    let global = lf_f32(words + 15u);
    if global != 0.0 {
        l = lf_mixer_grade_gamma(l, global);
    }
    let x = 2.0 * clamp(l, 0.0, 1.0) - 1.0;
    let x2 = x * x;
    let x4 = x2 * x2;
    let envelope = 1.0 - x4 * x4;
    let a = w.x * lf_f32(words + 4u) + w.y * lf_f32(words + 6u) + w.z * lf_f32(words + 8u)
        + lf_f32(words + 10u);
    let b = w.x * lf_f32(words + 5u) + w.y * lf_f32(words + 7u) + w.z * lf_f32(words + 9u)
        + lf_f32(words + 11u);
    return lf_mixer_grade_from_oklab(vec3<f32>(l, lab.y + envelope * a, lab.z + envelope * b));
}
