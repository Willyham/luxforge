// The luminance range's coverage on the GPU, beside its CPU field in `range.rs`: the frozen band on
// the histogram's own axis, read from the input of the operation the mask modulates, in the same
// spelling and order, evaluated in f32. The axis is Rec. 709 luminance encoded through the
// continued sRGB transfer function, unclamped; its constants are restated here, as a program
// declares every name it uses, and the core's tests hold them to the CPU's f32 values.
//
// Words: 0 lo, 1 hi, 2 lo_feather, 3 hi_feather, each the stored slider value over 100 as the CPU
// field holds it, narrowed to f32. A feather of exactly zero is the hard edge and is never divided
// by.

const lf_mask_luminance_range_luma = vec3<f32>(0.2126, 0.7152, 0.0722);
const lf_mask_luminance_range_linear_end: f32 = 0.0031308;
const lf_mask_luminance_range_slope: f32 = 12.92;
const lf_mask_luminance_range_scale: f32 = 1.055;
const lf_mask_luminance_range_offset: f32 = 0.055;
const lf_mask_luminance_range_exponent: f32 = 1.0 / 2.4;

// The frozen easing, s*s*(3 - 2s), on s already clamped to [0, 1].
fn lf_mask_luminance_range_smooth(s: f32) -> f32 {
    return s * s * (3.0 - 2.0 * s);
}

// The luminance axis: Rec. 709 luminance, each product summed left to right, through the continued
// sRGB encode, which is defined and increasing below black and past white.
fn lf_mask_luminance_range_axis(rgb: vec3<f32>) -> f32 {
    let w = lf_mask_luminance_range_luma;
    let y = w.x * rgb.x + w.y * rgb.y + w.z * rgb.z;
    if y <= lf_mask_luminance_range_linear_end {
        return lf_mask_luminance_range_slope * y;
    }
    return lf_mask_luminance_range_scale * pow(y, lf_mask_luminance_range_exponent)
        - lf_mask_luminance_range_offset;
}

fn lf_mask_luminance_range(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {
    let e = lf_mask_luminance_range_axis(rgb);
    let lo = lf_f32(words);
    let hi = lf_f32(words + 1u);
    let lo_feather = lf_f32(words + 2u);
    let hi_feather = lf_f32(words + 3u);
    var rise = 0.0;
    if lo_feather == 0.0 {
        rise = select(0.0, 1.0, e >= lo);
    } else {
        rise = lf_mask_luminance_range_smooth(clamp((e - lo) / lo_feather + 1.0, 0.0, 1.0));
    }
    var fall = 0.0;
    if hi_feather == 0.0 {
        fall = select(0.0, 1.0, e <= hi);
    } else {
        fall = lf_mask_luminance_range_smooth(clamp((hi - e) / hi_feather + 1.0, 0.0, 1.0));
    }
    return min(rise, fall);
}
