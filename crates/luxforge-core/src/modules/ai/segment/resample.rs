//! Antialiased bilinear resampling: a triangle kernel widened by the reduction factor, applied
//! separably. This is PIL's `BILINEAR` resize and torchvision's `resize(..., antialias=True)`,
//! which the segmenter's reference uses, computed in `f32` without their intermediate 8-bit
//! rounding (`probes/object-removal/src/segment/resample.rs`). It reduces the at most 1024 px
//! analysis input to the encoder's 512 px, so it runs on the calling lane alone.

/// The kernel taps for one axis: output `i` reads `count[i]` inputs from `start[i]`, weighted by
/// `weights[i * stride..]`.
pub(super) struct Taps {
    pub(super) start: Vec<usize>,
    pub(super) count: Vec<usize>,
    pub(super) stride: usize,
    pub(super) weights: Vec<f32>,
}

/// PIL's `precompute_coeffs` for the bilinear (triangle, support 1) filter.
pub(super) fn taps(input: usize, output: usize) -> Taps {
    let scale = input as f64 / output as f64;
    let filter_scale = scale.max(1.0);
    let support = filter_scale;
    let stride = support.ceil() as usize * 2 + 1;
    let mut t = Taps {
        start: Vec::with_capacity(output),
        count: Vec::with_capacity(output),
        stride,
        weights: vec![0.0; output * stride],
    };
    for i in 0..output {
        let centre = (i as f64 + 0.5) * scale;
        let lo = ((centre - support + 0.5) as isize).max(0) as usize;
        let hi = ((centre + support + 0.5) as isize).min(input as isize) as usize;
        let n = hi - lo;
        let w = &mut t.weights[i * stride..i * stride + n];
        let mut total = 0.0;
        for (k, wk) in w.iter_mut().enumerate() {
            let d = ((lo + k) as f64 - centre + 0.5) / filter_scale;
            let v = (1.0 - d.abs()).max(0.0);
            *wk = v as f32;
            total += v;
        }
        if total > 0.0 {
            for wk in w.iter_mut() {
                *wk = (f64::from(*wk) / total) as f32;
            }
        }
        t.start.push(lo);
        t.count.push(n);
    }
    t
}

/// Interleaved RGB `f32` codes of `source` size resampled to `size`.
pub(crate) fn resample(rgb: &[f32], source: (u32, u32), size: (u32, u32)) -> Vec<f32> {
    let (sw, sh) = (source.0 as usize, source.1 as usize);
    let (dw, dh) = (size.0 as usize, size.1 as usize);
    debug_assert!(sw > 0 && sh > 0 && dw > 0 && dh > 0, "empty resample");
    debug_assert_eq!(rgb.len(), sw * sh * 3);
    let tx = taps(sw, dw);
    let ty = taps(sh, dh);
    // Horizontal pass over every source row, then the vertical pass per output row.
    let mut across = vec![0.0_f32; sh * dw * 3];
    for (y, out) in across.chunks_exact_mut(dw * 3).enumerate() {
        let src = &rgb[y * sw * 3..(y + 1) * sw * 3];
        for (x, px) in out.chunks_exact_mut(3).enumerate() {
            let w = &tx.weights[x * tx.stride..x * tx.stride + tx.count[x]];
            let base = tx.start[x] * 3;
            let (mut r, mut g, mut b) = (0.0_f32, 0.0_f32, 0.0_f32);
            for (k, &wk) in w.iter().enumerate() {
                let i = base + k * 3;
                r += wk * src[i];
                g += wk * src[i + 1];
                b += wk * src[i + 2];
            }
            px[0] = r;
            px[1] = g;
            px[2] = b;
        }
    }
    let row = dw * 3;
    let mut out = vec![0.0_f32; dh * row];
    for (y, dst) in out.chunks_exact_mut(row).enumerate() {
        let w = &ty.weights[y * ty.stride..y * ty.stride + ty.count[y]];
        for (k, &wk) in w.iter().enumerate() {
            let src = &across[(ty.start[y] + k) * row..(ty.start[y] + k + 1) * row];
            for (d, s) in dst.iter_mut().zip(src) {
                *d += wk * s;
            }
        }
    }
    out
}
