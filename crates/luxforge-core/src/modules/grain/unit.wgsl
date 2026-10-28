// The grain module's positional unit on the GPU, beside its CPU unit in `unit.rs`: the frozen hash,
// value noise, field and application of `docs/design/grain-study.md` at the stage pixel `pos` the
// CPU unit is handed. The integer parts (the hash, the lattice coordinate and the lattice values)
// are exact `u32` arithmetic, bit for bit the CPU's; everything after is the same f32 arithmetic in
// the same order as `Grain::apply_row`. The compile is the CPU's: the program receives its steps
// and narrowed coefficients and never recomputes them, so a window, a tile and a reduced stage
// address the field the CPU addresses at the same stage pixel. Amount 0 (a neutral draft's GPU
// shape), a pixel at or below black and a pixel with a channel at or above white are returned
// untouched.
//
// Words:
//   0  the fine component's step across, a u32: lattice cells per stage pixel, times 2^16
//   1  its step down
//   2  the coarse component's step across
//   3  its step down
//   4  the clump component's step across
//   5  its step down
//   6  fine_gain
//   7  coarse_gain, 0 at Roughness 0
//   8  clump_gain, 0 at Roughness 0
//   9  4 beta, the tone weight's amplitude; 0 is the exact identity
//  10  0.5 * 1.4 * variance, the mean correction's coefficient
// Each coefficient is computed in f64 as the CPU unit's compile computes it and narrowed to the
// f32 that unit multiplies by.

// Each component's key, `mix(SEED + component)` for the frozen seed 0x4C584752; a test holds them
// to the CPU's `component_key`.
const lf_grain_film_key_fine: u32 = 0xa99c2dfcu;
const lf_grain_film_key_coarse: u32 = 0x80d15decu;
const lf_grain_film_key_clump: u32 = 0x4ac8a78eu;

const lf_grain_film_field_clamp: f32 = 4.0;
const lf_grain_film_curvature_offset: f32 = 0.055;
const lf_grain_film_near_black: f32 = 1e-6;

// The 32-bit finaliser ("lowbias32"), in u32 wrapping arithmetic only.
fn lf_grain_film_mix(value: u32) -> u32 {
    var x = value;
    x ^= x >> 16u;
    x = x * 0x7feb352du;
    x ^= x >> 15u;
    x = x * 0x846ca68bu;
    x ^= x >> 16u;
    return x;
}

// A hash's lattice value: the sum of its two 16-bit halves, centred, in (-1, 1). Exact in f32.
fn lf_grain_film_value(hash: u32) -> f32 {
    return f32((hash & 0xffffu) + (hash >> 16u) + 1u) / 65536.0 - 1.0;
}

// A stage pixel index's lattice cell and 16-bit fraction for a step of at most 2^16: the exact
// product `index * step` split without overflowing u32.
fn lf_grain_film_lattice(index: u32, step: u32) -> vec2<u32> {
    let high = index >> 16u;
    let product = (index & 0xffffu) * step;
    return vec2<u32>(high * step + (product >> 16u), product & 0xffffu);
}

// The smoothstep weights (w0, w1) at a 16-bit fraction.
fn lf_grain_film_weights(fraction: u32) -> vec2<f32> {
    let tau = f32(fraction) / 65536.0;
    let w1 = tau * tau * (3.0 - 2.0 * tau);
    return vec2<f32>(1.0 - w1, w1);
}

// One component's value noise at stage pixel `at`: (raw, norm), `raw` the smooth interpolation of
// its cell's four lattice values and `norm` the root sum of the squared weights.
fn lf_grain_film_noise(key: u32, at: vec2<u32>, step_x: u32, step_y: u32) -> vec2<f32> {
    let lx = lf_grain_film_lattice(at.x, step_x);
    let ly = lf_grain_film_lattice(at.y, step_y);
    let i = lx.x;
    let j = ly.x;
    let row0 = lf_grain_film_mix(j + key);
    let row1 = lf_grain_film_mix(j + 1u + key);
    let v00 = lf_grain_film_value(lf_grain_film_mix(i + row0));
    let v01 = lf_grain_film_value(lf_grain_film_mix(i + row1));
    let v10 = lf_grain_film_value(lf_grain_film_mix(i + 1u + row0));
    let v11 = lf_grain_film_value(lf_grain_film_mix(i + 1u + row1));
    let wx = lf_grain_film_weights(lx.y);
    let wy = lf_grain_film_weights(ly.y);
    let norm_y = wy.x * wy.x + wy.y * wy.y;
    let top = wx.x * v00 + wx.y * v10;
    let bottom = wx.x * v01 + wx.y * v11;
    let raw = wy.x * top + wy.y * bottom;
    let norm = sqrt((wx.x * wx.x + wx.y * wx.y) * norm_y);
    return vec2<f32>(raw, norm);
}

// The clamped field value G at stage pixel `at`.
fn lf_grain_film_field(at: vec2<u32>, words: u32) -> f32 {
    let fine = lf_grain_film_noise(lf_grain_film_key_fine, at, lf_word(words), lf_word(words + 1u));
    var f = lf_f32(words + 6u) * (fine.x / fine.y);
    let coarse_gain = lf_f32(words + 7u);
    if coarse_gain != 0.0 {
        let coarse = lf_grain_film_noise(
            lf_grain_film_key_coarse,
            at,
            lf_word(words + 2u),
            lf_word(words + 3u),
        );
        f += coarse_gain * (coarse.x / coarse.y);
    }
    let clump_gain = lf_f32(words + 8u);
    if clump_gain != 0.0 {
        let clump = lf_grain_film_noise(
            lf_grain_film_key_clump,
            at,
            lf_word(words + 4u),
            lf_word(words + 5u),
        );
        f *= sqrt(1.0 + clump_gain * clump.x);
    }
    return clamp(f, -lf_grain_film_field_clamp, lf_grain_film_field_clamp);
}

fn lf_grain_film_encode(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        return 12.92 * linear;
    }
    return 1.055 * pow(linear, 1.0f / 2.4f) - 0.055;
}

fn lf_grain_film_decode(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

fn lf_grain_film(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let four_beta = lf_f32(words + 9u);
    if four_beta == 0.0 {
        return rgb;
    }
    let l = 0.2126 * rgb.x + 0.7152 * rgb.y + 0.0722 * rgb.z;
    let m = max(max(rgb.x, rgb.y), rgb.z);
    if l <= 0.0 || m >= 1.0 {
        return rgb;
    }
    // `pos` is an integer stage pixel held exactly in f32; rounding rather than truncating keeps
    // the hash's integer position should a map ever hand it a value just below that integer.
    let g = lf_grain_film_field(vec2<u32>(round(pos)), words);
    let e = lf_grain_film_encode(l);
    let e_top = lf_grain_film_encode(l / m);
    let t = e / e_top;
    let s = four_beta * t * (1.0 - t) * e_top;
    let bias = lf_f32(words + 10u) * s * s / (e + lf_grain_film_curvature_offset);
    let l_out = lf_grain_film_decode(e + s * g - bias);
    if abs(l) < lf_grain_film_near_black {
        return rgb + vec3<f32>(l_out - l);
    }
    return rgb * (l_out / l);
}
