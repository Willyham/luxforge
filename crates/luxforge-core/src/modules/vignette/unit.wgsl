// The vignette module's positional unit on the GPU, beside its CPU unit in `unit.rs`: the frozen
// mask geometry and amount equation at the output-stage pixel `pos` the CPU unit is handed. The
// CPU evaluates the mask in f64; the GPU has no f64, so the same expressions run in f32. The
// amount equation is the CPU's: a linear-light gain when darkening, and a lift in the analytically
// continued encoded domain when brightening, leaving a channel at or past encoded white as it is.
// A pixel whose mask is exactly 0 is returned untouched.
//
// Words:
//   0  the shape, an integer: 0 the ellipse family (roundness >= 0), 1 the superellipse family
//   1  the ellipse's column coefficient a, or the superellipse's exponent p
//   2  the ellipse's row coefficient b
//   3  half the stage's width
//   4  half the stage's height
//   5  r0, where the falloff starts
//   6  r1 - r0, its span
//   7  1 for the hard step r1 == r0, an integer, else 0
//   8  the branch, an integer: 0 darkens (amount < 0), 1 lifts
//   9  amount / 100
//   10 |amount| / 100
// Each value is computed in f64 as the CPU unit computes it and narrowed to f32.

fn lf_vignette_vignette_encode(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        return 12.92 * linear;
    }
    return 1.055 * pow(linear, 1.0f / 2.4f) - 0.055;
}

fn lf_vignette_vignette_decode(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

// |x|^p for x >= 0, zero at zero.
fn lf_vignette_vignette_power(x: f32, p: f32) -> f32 {
    if x == 0.0 {
        return 0.0;
    }
    return pow(x, p);
}

fn lf_vignette_vignette_mask(pos: vec2<f32>, words: u32) -> f32 {
    let half_w = lf_f32(words + 3u);
    let half_h = lf_f32(words + 4u);
    let u = (pos.x + 0.5 - half_w) / half_w;
    let v = (pos.y + 0.5 - half_h) / half_h;
    var r: f32;
    if lf_word(words) == 0u {
        r = sqrt(lf_f32(words + 1u) * u * u + lf_f32(words + 2u) * v * v);
    } else {
        let p = lf_f32(words + 1u);
        let sum = lf_vignette_vignette_power(abs(u), p) + lf_vignette_vignette_power(abs(v), p);
        r = lf_vignette_vignette_power(sum / 2.0, 1.0 / p);
    }
    let r0 = lf_f32(words + 5u);
    if lf_word(words + 7u) == 1u {
        return select(1.0, 0.0, r <= r0);
    }
    let t = clamp((r - r0) / lf_f32(words + 6u), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn lf_vignette_vignette_lift(channel: f32, lift: f32) -> f32 {
    let encoded = lf_vignette_vignette_encode(channel);
    if encoded < 1.0 {
        return lf_vignette_vignette_decode(encoded + lift * (1.0 - encoded));
    }
    return channel;
}

fn lf_vignette_vignette(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let mask = lf_vignette_vignette_mask(pos, words);
    if mask == 0.0 {
        return rgb;
    }
    if lf_word(words + 8u) == 0u {
        return rgb * (1.0 - lf_f32(words + 10u) * mask);
    }
    let lift = lf_f32(words + 9u) * mask;
    return vec3<f32>(
        lf_vignette_vignette_lift(rgb.x, lift),
        lf_vignette_vignette_lift(rgb.y, lift),
        lf_vignette_vignette_lift(rgb.z, lift),
    );
}
