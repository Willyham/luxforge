// The colour mixer's one pointwise unit on the GPU, beside its CPU unit in `unit.rs`: one Oklab
// round trip per pixel, the hue warp, the chroma factor and the L gamma response evaluated once on
// the input pixel, in the same f32 arithmetic and order as `Mixer::apply_row`. The Oklab
// conversion is `crate::colour::oklab`'s, restated because a program declares every name it uses;
// its matrices are the same published digits, narrowed to f32 as that module narrows them.
//
// Words: 0 to 31 the hue warp's displacement cubic [c0, c1, c2, c3] per segment, in wheel order;
// 32 to 39 each range's chroma gain (saturation / 100 * k_s); 40 to 47 each range's luminance
// amount (luminance / 100). Each is the f32 the CPU unit holds.

// The frozen range centres and the gaps between them, in degrees: the f64 values the CPU unit
// narrows, written to every digit so they narrow to the same f32.
const lf_mixer_mixer_centres = array<f32, 8>(
    29.2338851923,
    52.984679594,
    109.7692320765,
    142.4953388878,
    194.768947932,
    264.0520206381,
    293.9376408145,
    328.3634179235,
);
const lf_mixer_mixer_gaps = array<f32, 8>(
    23.7507944017,
    56.784552482500004,
    32.726106811299985,
    52.273609044200015,
    69.28307270609997,
    29.88562017640004,
    34.42577710899997,
    60.87046726880004,
);
const lf_mixer_mixer_chroma_ramp_edge: f32 = 0.02;

// Linear sRGB to LMS.
const lf_mixer_mixer_m1_0 = vec3<f32>(0.4122214708, 0.5363325363, 0.0514459929);
const lf_mixer_mixer_m1_1 = vec3<f32>(0.2119034982, 0.6806995451, 0.1073969566);
const lf_mixer_mixer_m1_2 = vec3<f32>(0.0883024619, 0.2817188376, 0.6299787005);
// LMS' to Oklab.
const lf_mixer_mixer_m2_0 = vec3<f32>(0.2104542553, 0.7936177850, -0.0040720468);
const lf_mixer_mixer_m2_1 = vec3<f32>(1.9779984951, -2.4285922050, 0.4505937099);
const lf_mixer_mixer_m2_2 = vec3<f32>(0.0259040371, 0.7827717662, -0.8086757660);
// Oklab to LMS'.
const lf_mixer_mixer_m2_inv_0 = vec3<f32>(1.0, 0.3963377774, 0.2158037573);
const lf_mixer_mixer_m2_inv_1 = vec3<f32>(1.0, -0.1055613458, -0.0638541728);
const lf_mixer_mixer_m2_inv_2 = vec3<f32>(1.0, -0.0894841775, -1.2914855480);
// LMS to linear sRGB.
const lf_mixer_mixer_m1_inv_0 = vec3<f32>(4.0767416621, -3.3077115913, 0.2309699292);
const lf_mixer_mixer_m1_inv_1 = vec3<f32>(-1.2684380046, 2.6097574011, -0.3413193965);
const lf_mixer_mixer_m1_inv_2 = vec3<f32>(-0.0041960863, -0.7034186147, 1.7076147010);

// The matrix-vector product, each row summed left to right.
fn lf_mixer_mixer_rows(r0: vec3<f32>, r1: vec3<f32>, r2: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        r0.x * v.x + r0.y * v.y + r0.z * v.z,
        r1.x * v.x + r1.y * v.y + r1.z * v.z,
        r2.x * v.x + r2.y * v.y + r2.z * v.z,
    );
}

// The signed cube root, finite for every finite x. WGSL has no cbrt: the power function's
// estimate is refined by one Newton step, written so that no intermediate overflows.
fn lf_mixer_mixer_cbrt(x: f32) -> f32 {
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

fn lf_mixer_mixer_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = lf_mixer_mixer_rows(lf_mixer_mixer_m1_0, lf_mixer_mixer_m1_1, lf_mixer_mixer_m1_2, rgb);
    let root = vec3<f32>(
        lf_mixer_mixer_cbrt(lms.x),
        lf_mixer_mixer_cbrt(lms.y),
        lf_mixer_mixer_cbrt(lms.z),
    );
    return lf_mixer_mixer_rows(lf_mixer_mixer_m2_0, lf_mixer_mixer_m2_1, lf_mixer_mixer_m2_2, root);
}

// An achromatic colour (a = b = 0) reconstructs to L^3 in all three channels, as the CPU's does.
fn lf_mixer_mixer_from_oklab(lab: vec3<f32>) -> vec3<f32> {
    if lab.y == 0.0 && lab.z == 0.0 {
        return vec3<f32>(lab.x * lab.x * lab.x);
    }
    let root = lf_mixer_mixer_rows(
        lf_mixer_mixer_m2_inv_0,
        lf_mixer_mixer_m2_inv_1,
        lf_mixer_mixer_m2_inv_2,
        lab,
    );
    return lf_mixer_mixer_rows(
        lf_mixer_mixer_m1_inv_0,
        lf_mixer_mixer_m1_inv_1,
        lf_mixer_mixer_m1_inv_2,
        root * root * root,
    );
}

// A difference of two angles in [0, 360), or an atan2 hue in (-180, 180], wrapped to [0, 360).
fn lf_mixer_mixer_wrap(degrees: f32) -> f32 {
    if degrees < 0.0 {
        return degrees + 360.0;
    }
    return degrees;
}

// The Oklab L response for a weighted amount m: a gamma on the [0, 1] part of L, any excess
// passed through.
fn lf_mixer_mixer_luminance(l: f32, amount: f32) -> f32 {
    let gamma = exp2(-amount);
    let core = clamp(l, 0.0, 1.0);
    var lifted = 0.0;
    if core > 0.0 {
        lifted = pow(core, gamma);
    }
    return lifted + (l - core);
}

fn lf_mixer_mixer(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let lab = lf_mixer_mixer_to_oklab(rgb);
    if lab.y == 0.0 && lab.z == 0.0 {
        return lf_mixer_mixer_from_oklab(lab);
    }
    // The chroma ramp: 0 on the achromatic axis, smoothstep to 1 at the ramp's edge.
    let chroma = sqrt(lab.y * lab.y + lab.z * lab.z);
    let ramp_t = clamp(chroma / lf_mixer_mixer_chroma_ramp_edge, 0.0, 1.0);
    let ramp = ramp_t * ramp_t * (3.0 - 2.0 * ramp_t);
    let hue = lf_mixer_mixer_wrap(atan2(lab.z, lab.y) * 57.29577951308232);
    // The segment: the centre most recently passed going counter-clockwise, by argmin over the
    // wrapped distances, and the fraction across its gap.
    var lower = 0u;
    var distance = 3.4028234663852886e38;
    for (var range = 0u; range < 8u; range++) {
        let candidate = lf_mixer_mixer_wrap(hue - lf_mixer_mixer_centres[range]);
        if candidate < distance {
            distance = candidate;
            lower = range;
        }
    }
    let t = clamp(distance / lf_mixer_mixer_gaps[lower], 0.0, 1.0);
    let upper = (lower + 1u) % 8u;
    let warp = words + 4u * lower;
    let rotation = ramp * (lf_f32(warp) + t * (lf_f32(warp + 1u)
        + t * (lf_f32(warp + 2u) + t * lf_f32(warp + 3u))));
    let weight = 0.5 * (1.0 + cos(3.141592653589793 * t));
    let other = 1.0 - weight;
    let saturation = weight * lf_f32(words + 32u + lower) + other * lf_f32(words + 32u + upper);
    var factor = 1.0 + ramp * saturation;
    if saturation < 0.0 {
        factor = 1.0 + saturation;
    }
    factor = max(factor, 0.0);
    let amount = ramp * (weight * lf_f32(words + 40u + lower) + other * lf_f32(words + 40u + upper));
    var a = factor * lab.y;
    var b = factor * lab.z;
    if rotation != 0.0 {
        let radians = rotation * 0.017453292519943295;
        let s = sin(radians);
        let c = cos(radians);
        a = factor * (lab.y * c - lab.z * s);
        b = factor * (lab.y * s + lab.z * c);
    }
    var l = lab.x;
    if amount != 0.0 {
        l = lf_mixer_mixer_luminance(lab.x, amount);
    }
    return lf_mixer_mixer_from_oklab(vec3<f32>(l, a, b));
}
