// Split toning on the GPU: the arithmetic of `SplitToning::apply_row` in src/lib.rs, reading the
// same nine words. The surface's prelude declares `lf_f32`; every name here starts `lf_module`.

const lf_module_luma: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

fn lf_module(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let highlights = vec3<f32>(lf_f32(words), lf_f32(words + 1u), lf_f32(words + 2u));
    let shadows = vec3<f32>(lf_f32(words + 3u), lf_f32(words + 4u), lf_f32(words + 5u));
    let pivot = lf_f32(words + 6u);
    let below = lf_f32(words + 7u);
    let above = lf_f32(words + 8u);
    let tone = sqrt(clamp(dot(lf_module_luma, rgb), 0.0, 1.0));
    var highlight: f32;
    if tone <= pivot {
        let t = tone * below;
        highlight = 0.5 * t * t;
    } else {
        let t = (1.0 - tone) * above;
        highlight = 1.0 - 0.5 * t * t;
    }
    let shadow = 1.0 - highlight;
    return rgb * (vec3<f32>(1.0) + highlight * highlights + shadow * shadows);
}
