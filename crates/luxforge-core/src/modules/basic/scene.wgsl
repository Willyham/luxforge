// The Basic module's scene unit on the GPU, beside its CPU unit in `scene.rs`: each channel of
// linear sRGB shifted by its stops on the scene scale, `S(x) = log2(x) + lift(log2(x))`, and back,
// unclamped. Zero and negative values take the plain gain. The CPU unit evaluates in f64; this
// program in f32, held to it within the pointwise output limits.
//
// Words: 0 to 2 are red's, green's and blue's shift in stops, the CPU unit's narrowed to f32.

const lf_basic_scene_knots = array<f32, 33>(
    -9.0, -8.625, -8.25, -7.875, -7.5, -7.125, -6.75, -6.375, -6.0, -5.625, -5.25, -4.875, -4.5,
    -4.125, -3.75, -3.375, -3.0, -2.625, -2.25, -1.875, -1.5, -1.375, -1.25, -1.125, -1.0, -0.875,
    -0.75, -0.625, -0.5, -0.375, -0.25, -0.125, 0.0
);

const lf_basic_scene_lift = array<f32, 33>(
    0.0, 0.0044, 0.0299, 0.0461, 0.0306, -0.0078, -0.0605, -0.1373, -0.2248, -0.3224, -0.4384,
    -0.5707, -0.7072, -0.8218, -0.9321, -1.0384, -1.1378, -1.1815, -1.274, -1.3646, -1.4104,
    -1.4165, -1.4199, -1.4188, -1.4096, -1.3848, -1.3315, -1.2425, -1.1329, -0.9404, -0.566,
    -0.1909, 0.1841
);

fn lf_basic_scene_log(x: f32) -> f32 {
    let u = log2(x);
    if (u <= lf_basic_scene_knots[0]) {
        return u + lf_basic_scene_lift[0];
    }
    if (u >= lf_basic_scene_knots[32]) {
        return u + lf_basic_scene_lift[32];
    }
    var i = 0u;
    for (var k = 1u; k < 32u; k++) {
        if (lf_basic_scene_knots[k] <= u) {
            i = k;
        }
    }
    let t = (u - lf_basic_scene_knots[i]) / (lf_basic_scene_knots[i + 1u] - lf_basic_scene_knots[i]);
    return u + lf_basic_scene_lift[i] + t * (lf_basic_scene_lift[i + 1u] - lf_basic_scene_lift[i]);
}

fn lf_basic_scene_unlog(z: f32) -> f32 {
    let first = lf_basic_scene_knots[0] + lf_basic_scene_lift[0];
    let last = lf_basic_scene_knots[32] + lf_basic_scene_lift[32];
    if (z <= first) {
        return exp2(z - lf_basic_scene_lift[0]);
    }
    if (z >= last) {
        return exp2(z - lf_basic_scene_lift[32]);
    }
    var i = 0u;
    for (var k = 1u; k < 32u; k++) {
        if (lf_basic_scene_knots[k] + lf_basic_scene_lift[k] <= z) {
            i = k;
        }
    }
    let z0 = lf_basic_scene_knots[i] + lf_basic_scene_lift[i];
    let z1 = lf_basic_scene_knots[i + 1u] + lf_basic_scene_lift[i + 1u];
    let width = lf_basic_scene_knots[i + 1u] - lf_basic_scene_knots[i];
    return exp2(lf_basic_scene_knots[i] + (z - z0) / (z1 - z0) * width);
}

fn lf_basic_scene_shift(x: f32, stops: f32) -> f32 {
    if (x > 0.0) {
        return lf_basic_scene_unlog(lf_basic_scene_log(x) + stops);
    }
    return x * exp2(stops);
}

fn lf_basic_scene(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    return vec3<f32>(
        lf_basic_scene_shift(rgb.x, lf_f32(words)),
        lf_basic_scene_shift(rgb.y, lf_f32(words + 1u)),
        lf_basic_scene_shift(rgb.z, lf_f32(words + 2u)),
    );
}
