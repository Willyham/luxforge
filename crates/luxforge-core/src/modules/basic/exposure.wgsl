// The Basic module's exposure unit on the GPU, beside its CPU unit in `exposure.rs`: every
// linear-light channel times 2^EV, unclamped.
//
// Words: 0 is the gain, 2^EV computed in f64 and narrowed to f32 exactly as the CPU unit's.
fn lf_basic_exposure(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    return rgb * lf_f32(words);
}
