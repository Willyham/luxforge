// The Basic module's Tone unit on the GPU, beside its CPU unit in `tone.rs`: the frozen global
// luminance curve, Whites/Blacks, then Shadows, then Highlights, then Contrast, on the pixel's
// encoded Rec. 709 luminance, with RGB reconstructed by the luminance ratio. The same f32
// arithmetic in the same order as `Tone::apply_row`; nothing is clamped but the blend weight's own
// input.
//
// Words:
//   0  Contrast's branch, an integer: 0 the identity, 1 the positive S-curve, 2 its reflection
//   1  alpha
//   2  g(0)
//   3  1 / (g(1) - g(0))
//   4  kappa, negative Contrast's reflection weight
//   5  Shadows' e^-k
//   6  Highlights' e^-k
//   7  the black point after the crossing-prevention clamp
//   8  1 / (white point - black point)
// Each is the f32 the CPU unit holds, computed once in f64 and narrowed exactly as its own.

const lf_basic_tone_near_black: f32 = 1e-6;

// The logistic's exponent is held within +-80 so `exp` is never asked for a value it cannot
// represent: past that bound the logistic is 0 or 1 to every bit an f32 holds, as the CPU's
// `exp`, which overflows to infinity there, gives it.
const lf_basic_tone_exponent_bound: f32 = 80.0;

fn lf_basic_tone_encode(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        return 12.92 * linear;
    }
    return 1.055 * pow(linear, 1.0f / 2.4f) - 0.055;
}

fn lf_basic_tone_decode(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

// B_k(x) = x / (x + (1 - x) e^-k) on (0, 1), passed straight through outside it.
fn lf_basic_tone_odds_bias(x: f32, exp_neg_k: f32) -> f32 {
    if x <= 0.0 || x >= 1.0 {
        return x;
    }
    return x / (x + (1.0 - x) * exp_neg_k);
}

fn lf_basic_tone_blend_weight(x: f32) -> f32 {
    let complement = 1.0 - clamp(x, 0.0, 1.0);
    return complement * complement;
}

fn lf_basic_tone_curve(input: f32, words: u32) -> f32 {
    // Whites and Blacks.
    var x = (input - lf_f32(words + 7u)) * lf_f32(words + 8u);
    // Shadows.
    let shadows = lf_basic_tone_blend_weight(x);
    x = shadows * lf_basic_tone_odds_bias(x, lf_f32(words + 5u)) + (1.0 - shadows) * x;
    // Highlights, mirrored about the midpoint.
    let mirrored = 1.0 - x;
    let highlights = lf_basic_tone_blend_weight(mirrored);
    let inner = highlights * lf_basic_tone_odds_bias(mirrored, lf_f32(words + 6u))
        + (1.0 - highlights) * mirrored;
    x = 1.0 - inner;
    // Contrast.
    let branch = lf_word(words);
    if branch == 0u {
        return x;
    }
    let exponent = clamp(
        -lf_f32(words + 1u) * (x - 0.5),
        -lf_basic_tone_exponent_bound,
        lf_basic_tone_exponent_bound,
    );
    let g = 1.0 / (1.0 + exp(exponent));
    let s = (g - lf_f32(words + 2u)) * lf_f32(words + 3u);
    if branch == 1u {
        return s;
    }
    return x - lf_f32(words + 4u) * (s - x);
}

fn lf_basic_tone(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let l_in = 0.2126 * rgb.x + 0.7152 * rgb.y + 0.0722 * rgb.z;
    let l_out = lf_basic_tone_decode(lf_basic_tone_curve(lf_basic_tone_encode(l_in), words));
    if abs(l_in) < lf_basic_tone_near_black {
        return rgb + vec3<f32>(l_out - l_in);
    }
    return rgb * (l_out / l_in);
}
