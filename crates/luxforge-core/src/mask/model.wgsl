// The model selection's coverage on the GPU, beside its CPU field in `model.rs`: the stored alpha
// raster read bilinearly with the edges clamped, then thresholded and feathered by the frozen
// easing, in the same spelling and order, evaluated in f32.
//
// Words: 0 the stage's height H, 1 kx = width * H / W, 2 ky = height, 3 t = threshold / 100, 4 lo,
// 5 span, each the f64 term the CPU field holds narrowed to f32; 6 the hard edge (1 when span is
// exactly zero, so it is never divided by); 7 the raster's width and 8 its height, in texels.
// Block: the raster's codes packed four to a word, code k at bits 8 * (k % 4) of word k / 4.

// The frozen easing, s*s*(3 - 2s), on s already clamped to [0, 1].
fn lf_mask_model_smooth(s: f32) -> f32 {
    return s * s * (3.0 - 2.0 * s);
}

// One code of the raster, as a float in 0..=255.
fn lf_mask_model_code(block: u32, width: u32, i: u32, j: u32) -> f32 {
    let k = j * width + i;
    return f32((lf_block_word(block + k / 4u) >> (8u * (k % 4u))) & 0xffu);
}

fn lf_mask_model(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {
    let height = lf_f32(words);
    let u = (pos.x + 0.5) / height;
    let v = (pos.y + 0.5) / height;
    let width = lf_word(words + 7u);
    let rows = lf_word(words + 8u);
    let ax = u * lf_f32(words + 1u) - 0.5;
    let ay = v * lf_f32(words + 2u) - 0.5;
    let x0 = floor(ax);
    let y0 = floor(ay);
    let fx = ax - x0;
    let fy = ay - y0;
    let last_x = i32(width) - 1;
    let last_y = i32(rows) - 1;
    let i0 = u32(clamp(i32(x0), 0, last_x));
    let i1 = u32(clamp(i32(x0) + 1, 0, last_x));
    let j0 = u32(clamp(i32(y0), 0, last_y));
    let j1 = u32(clamp(i32(y0) + 1, 0, last_y));
    let a00 = lf_mask_model_code(block, width, i0, j0);
    let a10 = lf_mask_model_code(block, width, i1, j0);
    let a01 = lf_mask_model_code(block, width, i0, j1);
    let a11 = lf_mask_model_code(block, width, i1, j1);
    let top = a00 + fx * (a10 - a00);
    let bottom = a01 + fx * (a11 - a01);
    let a = (top + fy * (bottom - top)) / 255.0;
    if lf_word(words + 6u) != 0u {
        return select(0.0, 1.0, a >= lf_f32(words + 3u));
    }
    return lf_mask_model_smooth(clamp((a - lf_f32(words + 4u)) / lf_f32(words + 5u), 0.0, 1.0));
}
