// One level of a photograph's mip chain: every texel of the level being written is the mean of the
// 2 x 2 texels of the level above it that it covers. The source is read through an sRGB-typed view
// of the photograph's texture, so `textureLoad` returns linear light, and the target is an
// sRGB-typed view of the next level, so the mean is encoded on the way out: the chain is averaged in
// linear light, never in encoded codes. A texel past the source's edge, when an odd side leaves the
// last row or column unpaired, repeats the edge texel.

@group(0) @binding(0) var source: texture_2d<f32>;

// One triangle that covers the whole target: (-1, -1), (3, -1), (-1, 3).
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let last = vec2<i32>(textureDimensions(source, 0)) - vec2<i32>(1, 1);
    let origin = vec2<i32>(floor(position.xy)) * 2;
    let a = textureLoad(source, min(origin, last), 0);
    let b = textureLoad(source, min(origin + vec2<i32>(1, 0), last), 0);
    let c = textureLoad(source, min(origin + vec2<i32>(0, 1), last), 0);
    let d = textureLoad(source, min(origin + vec2<i32>(1, 1), last), 0);
    return (a + b + c + d) * 0.25;
}
