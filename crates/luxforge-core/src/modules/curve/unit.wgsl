// The Tone curve's one pointwise unit on the GPU, beside its CPU unit in `unit.rs`: encoded Rec.
// 709 luminance through the curve's f32 interpolant, reconstructed over the curve's black level,
// in the same f32 arithmetic and order as `ToneCurve::apply_row`. Nothing is clamped.
//
// Words: 0 the knot count n (2 to 16), an integer; 1 the curve's first value; 2 its last value;
// 3 its black level decode(y_0).
// Block: the n knots x_i; then the n - 1 inverse widths 1 / h_i; then each segment's coefficients
// [c0, c1, c2, c3] in t, segment by segment. Every value is the f32 the CPU unit holds.

const lf_curve_tone_curve_near_black: f32 = 1e-6;

fn lf_curve_tone_curve_encode(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        return 12.92 * linear;
    }
    return 1.055 * pow(linear, 1.0f / 2.4f) - 0.055;
}

fn lf_curve_tone_curve_decode(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

// C(x) by the design's five rules: the unit-slope tails, the flat holds, and otherwise the later
// segment whose left knot is at or below x, with t clamped so a collapsed segment keeps C within
// its knot values.
fn lf_curve_tone_curve_value(x: f32, words: u32, block: u32) -> f32 {
    let n = lf_word(words);
    if x < 0.0 {
        return lf_f32(words + 1u) + x;
    }
    if x > 1.0 {
        return lf_f32(words + 2u) + (x - 1.0);
    }
    if x <= lf_block_f32(block) {
        return lf_f32(words + 1u);
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
    let i = min(below - 1u, n - 2u);
    let t = clamp((x - lf_block_f32(block + i)) * lf_block_f32(block + n + i), 0.0, 1.0);
    let c = block + 2u * n - 1u + 4u * i;
    return lf_block_f32(c)
        + t * (lf_block_f32(c + 1u) + t * (lf_block_f32(c + 2u) + t * lf_block_f32(c + 3u)));
}

fn lf_curve_tone_curve(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let l_in = 0.2126 * rgb.x + 0.7152 * rgb.y + 0.0722 * rgb.z;
    let l_out = lf_curve_tone_curve_decode(
        lf_curve_tone_curve_value(lf_curve_tone_curve_encode(l_in), words, block),
    );
    if abs(l_in) < lf_curve_tone_curve_near_black {
        return rgb + vec3<f32>(l_out - l_in);
    }
    let black = lf_f32(words + 3u);
    if black == 0.0 {
        return rgb * (l_out / l_in);
    }
    return vec3<f32>(black) + rgb * ((l_out - black) / l_in);
}
