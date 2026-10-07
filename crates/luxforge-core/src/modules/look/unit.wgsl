// The RAW look's one fused pointwise unit on the GPU, beside its CPU unit in `unit.rs`: tone,
// chroma, path to white and amount, in the same f32 arithmetic and order as `Look::pixel`. The
// interpolant evaluation is the Tone curve's (`lf_curve_tone_curve_value`) with the look's tails,
// and the Oklab conversion is the one `crate::colour::oklab` holds, both restated here because a
// program declares every name it uses; the matrices are the same published digits, narrowed to
// f32 as that module narrows them. Nothing is clamped.
//
// Words: 0 the knot count n (2 to 24), an integer; 1 the curve's first value y_0; 2 its last value
// y_{n-1}; 3 its black level decode(y_0); 4 its first slope d_0; 5 the Oklab chroma gain; 6 the
// path to white's knee; 7 the amount a = amount / 100 (exactly 0 only in the GPU shape, where the
// unit is the identity).
// Block: the n knots x_i; then the n - 1 inverse widths 1 / h_i; then each segment's coefficients
// [c0, c1, c2, c3] in t, segment by segment, as the Tone curve lays them out. Every value is the
// f32 the CPU unit holds.

const lf_look_look_near_black: f32 = 1e-6;

// Linear sRGB to LMS.
const lf_look_look_m1_0 = vec3<f32>(0.4122214708, 0.5363325363, 0.0514459929);
const lf_look_look_m1_1 = vec3<f32>(0.2119034982, 0.6806995451, 0.1073969566);
const lf_look_look_m1_2 = vec3<f32>(0.0883024619, 0.2817188376, 0.6299787005);
// LMS' to Oklab.
const lf_look_look_m2_0 = vec3<f32>(0.2104542553, 0.7936177850, -0.0040720468);
const lf_look_look_m2_1 = vec3<f32>(1.9779984951, -2.4285922050, 0.4505937099);
const lf_look_look_m2_2 = vec3<f32>(0.0259040371, 0.7827717662, -0.8086757660);
// Oklab to LMS'.
const lf_look_look_m2_inv_0 = vec3<f32>(1.0, 0.3963377774, 0.2158037573);
const lf_look_look_m2_inv_1 = vec3<f32>(1.0, -0.1055613458, -0.0638541728);
const lf_look_look_m2_inv_2 = vec3<f32>(1.0, -0.0894841775, -1.2914855480);
// LMS to linear sRGB.
const lf_look_look_m1_inv_0 = vec3<f32>(4.0767416621, -3.3077115913, 0.2309699292);
const lf_look_look_m1_inv_1 = vec3<f32>(-1.2684380046, 2.6097574011, -0.3413193965);
const lf_look_look_m1_inv_2 = vec3<f32>(-0.0041960863, -0.7034186147, 1.7076147010);

fn lf_look_look_encode(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        return 12.92 * linear;
    }
    return 1.055 * pow(linear, 1.0f / 2.4f) - 0.055;
}

fn lf_look_look_decode(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

// T(x) under the look's tails: the first segment continued below 0, the last value held at and
// above the last knot, and otherwise the later segment whose left knot is at or below x, with t
// clamped so a collapsed segment keeps T within its knot values.
fn lf_look_look_value(x: f32, words: u32, block: u32) -> f32 {
    let n = lf_word(words);
    if x < 0.0 {
        return lf_f32(words + 1u) + lf_f32(words + 4u) * x;
    }
    if x >= lf_block_f32(block + n - 1u) {
        return lf_f32(words + 2u);
    }
    // The knots are non-decreasing, so the knots at or below x are a prefix of them.
    var below = 0u;
    for (var k = 0u; k < n; k++) {
        if lf_block_f32(block + k) <= x {
            below += 1u;
        }
    }
    let i = min(max(below, 1u) - 1u, n - 2u);
    let t = clamp((x - lf_block_f32(block + i)) * lf_block_f32(block + n + i), 0.0, 1.0);
    let c = block + 2u * n - 1u + 4u * i;
    return lf_block_f32(c)
        + t * (lf_block_f32(c + 1u) + t * (lf_block_f32(c + 2u) + t * lf_block_f32(c + 3u)));
}

// Step 1: encoded Rec. 709 luminance through the curve, reconstructed over its black level.
fn lf_look_look_tone(rgb: vec3<f32>, words: u32, block: u32) -> vec3<f32> {
    let l_in = 0.2126 * rgb.x + 0.7152 * rgb.y + 0.0722 * rgb.z;
    let l_out = lf_look_look_decode(lf_look_look_value(lf_look_look_encode(l_in), words, block));
    if abs(l_in) < lf_look_look_near_black {
        return rgb + vec3<f32>(l_out - l_in);
    }
    let black = lf_f32(words + 3u);
    if black == 0.0 {
        return rgb * (l_out / l_in);
    }
    return vec3<f32>(black) + rgb * ((l_out - black) / l_in);
}

// The matrix-vector product, each row summed left to right.
fn lf_look_look_rows(r0: vec3<f32>, r1: vec3<f32>, r2: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        r0.x * v.x + r0.y * v.y + r0.z * v.z,
        r1.x * v.x + r1.y * v.y + r1.z * v.z,
        r2.x * v.x + r2.y * v.y + r2.z * v.z,
    );
}

// The signed cube root, finite for every finite x. WGSL has no cbrt: the power function's
// estimate is refined by one Newton step, written so that no intermediate overflows.
fn lf_look_look_cbrt(x: f32) -> f32 {
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

fn lf_look_look_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = lf_look_look_rows(lf_look_look_m1_0, lf_look_look_m1_1, lf_look_look_m1_2, rgb);
    let root = vec3<f32>(
        lf_look_look_cbrt(lms.x),
        lf_look_look_cbrt(lms.y),
        lf_look_look_cbrt(lms.z),
    );
    return lf_look_look_rows(lf_look_look_m2_0, lf_look_look_m2_1, lf_look_look_m2_2, root);
}

// An achromatic colour (a = b = 0) reconstructs to L^3 in all three channels, as the CPU's does.
fn lf_look_look_from_oklab(lab: vec3<f32>) -> vec3<f32> {
    if lab.y == 0.0 && lab.z == 0.0 {
        return vec3<f32>(lab.x * lab.x * lab.x);
    }
    let root = lf_look_look_rows(
        lf_look_look_m2_inv_0,
        lf_look_look_m2_inv_1,
        lf_look_look_m2_inv_2,
        lab,
    );
    return lf_look_look_rows(
        lf_look_look_m1_inv_0,
        lf_look_look_m1_inv_1,
        lf_look_look_m1_inv_2,
        root * root * root,
    );
}

// Step 2: Oklab a and b scaled by the gain at constant L; nothing at a gain of 1.
fn lf_look_look_chroma(rgb: vec3<f32>, gain: f32) -> vec3<f32> {
    if gain == 1.0 {
        return rgb;
    }
    let lab = lf_look_look_to_oklab(rgb);
    return lf_look_look_from_oklab(vec3<f32>(lab.x, lab.y * gain, lab.z * gain));
}

// Step 3: a colour whose largest channel m passes the knee k mixed toward the grey of its own
// luminance until its largest channel is k + (1 - k)(1 - e^{-(m - k)/(1 - k)}).
fn lf_look_look_white(rgb: vec3<f32>, knee: f32) -> vec3<f32> {
    let m = max(max(rgb.x, rgb.y), rgb.z);
    if m <= knee {
        return rgb;
    }
    let y = 0.2126 * rgb.x + 0.7152 * rgb.y + 0.0722 * rgb.z;
    if m <= y {
        return rgb;
    }
    let room = 1.0 - knee;
    let shoulder = knee + room * (1.0 - exp(-(m - knee) / room));
    let t = max((shoulder - y) / (m - y), 0.0);
    return vec3<f32>(y) + t * (rgb - vec3<f32>(y));
}

fn lf_look_look(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let amount = lf_f32(words + 7u);
    if amount == 0.0 {
        return rgb;
    }
    let toned = lf_look_look_tone(rgb, words, block);
    let coloured = lf_look_look_chroma(toned, lf_f32(words + 5u));
    let white = lf_look_look_white(coloured, lf_f32(words + 6u));
    if amount == 1.0 {
        return white;
    }
    return rgb + amount * (white - rgb);
}
