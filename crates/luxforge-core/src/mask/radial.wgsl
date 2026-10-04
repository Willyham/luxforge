// The radial gradient's coverage on the GPU, beside its CPU field in `radial.rs`: the frozen
// ellipse, inside selected, in the same spelling and order, evaluated in f32.
//
// Words: 0 the stage's height H, 1 cu, 2 cv, 3 cos(angle), 4 sin(angle), 5 radius_x, 6 radius_y,
// 7 r0 = 1 - feather / 100, 8 span = 1 - r0, 9 the hard edge (1 when span is exactly zero, so the
// span is never divided by). Each is the f64 term the CPU field holds, narrowed to f32.

// The frozen easing, s*s*(3 - 2s), on s already clamped to [0, 1].
fn lf_mask_radial_smooth(s: f32) -> f32 {
    return s * s * (3.0 - 2.0 * s);
}

fn lf_mask_radial(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {
    let height = lf_f32(words);
    let du = (pos.x + 0.5) / height - lf_f32(words + 1u);
    let dv = (pos.y + 0.5) / height - lf_f32(words + 2u);
    let ca = lf_f32(words + 3u);
    let sa = lf_f32(words + 4u);
    let a = ca * du + sa * dv;
    let b = -sa * du + ca * dv;
    let ax = a / lf_f32(words + 5u);
    let by = b / lf_f32(words + 6u);
    let r = sqrt(ax * ax + by * by);
    if lf_word(words + 9u) != 0u {
        return select(0.0, 1.0, r <= lf_f32(words + 7u));
    }
    return lf_mask_radial_smooth(clamp((1.0 - r) / lf_f32(words + 8u), 0.0, 1.0));
}
