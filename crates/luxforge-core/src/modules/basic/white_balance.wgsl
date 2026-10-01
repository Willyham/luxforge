// The Basic module's white-balance unit on the GPU, beside its CPU unit in `white_balance.rs`: the
// one composite linear-sRGB 3 x 3 multiply, each row summed left to right as `matvec_f32` sums it,
// unclamped.
//
// Words: 0 to 8 are the composite matrix, row by row: the f32 values the CPU unit multiplies by,
// computed once in f64 and narrowed exactly as its own.
fn lf_basic_white_balance(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let r = vec3<f32>(lf_f32(words), lf_f32(words + 1u), lf_f32(words + 2u));
    let g = vec3<f32>(lf_f32(words + 3u), lf_f32(words + 4u), lf_f32(words + 5u));
    let b = vec3<f32>(lf_f32(words + 6u), lf_f32(words + 7u), lf_f32(words + 8u));
    return vec3<f32>(
        r.x * rgb.x + r.y * rgb.y + r.z * rgb.z,
        g.x * rgb.x + g.y * rgb.y + g.z * rgb.z,
        b.x * rgb.x + b.y * rgb.y + b.z * rgb.z,
    );
}
