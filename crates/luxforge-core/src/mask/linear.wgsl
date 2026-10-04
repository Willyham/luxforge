// The linear gradient's coverage on the GPU, beside its CPU field in `linear.rs`: the frozen ramp
// along the axis from p0 to p1, in the same spelling and order, evaluated in f32.
//
// Words: 0 the stage's height H, 1 u0, 2 v0, 3 du, 4 dv, 5 l2 = du*du + dv*dv, each the f64 term
// the CPU field holds, narrowed to f32. `pos` is the pixel of the stage the words were compiled
// against; mask space is its pixel centre over H on both axes.

// The frozen easing, s*s*(3 - 2s), on s already clamped to [0, 1].
fn lf_mask_linear_smooth(s: f32) -> f32 {
    return s * s * (3.0 - 2.0 * s);
}

fn lf_mask_linear(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {
    let height = lf_f32(words);
    let u = (pos.x + 0.5) / height;
    let v = (pos.y + 0.5) / height;
    let projected = (u - lf_f32(words + 1u)) * lf_f32(words + 3u)
        + (v - lf_f32(words + 2u)) * lf_f32(words + 4u);
    return lf_mask_linear_smooth(clamp(projected / lf_f32(words + 5u), 0.0, 1.0));
}
