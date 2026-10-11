//! Antialiased bilinear resampling: a triangle kernel widened by the reduction factor, applied
//! separably. This is PIL's `BILINEAR` resize and torchvision's `resize(..., antialias=True)`,
//! which both segmenter references use, computed in `f32` without their intermediate 8-bit
//! rounding. Rows are split across threads, so a 60 MP reduction stays in the tens of ms.

use std::thread;

use crate::image::Rgb8;

/// The kernel taps for one axis: output `i` reads `count[i]` inputs from `start[i]`, weighted by
/// `weights[i * stride..]`.
struct Taps {
    start: Vec<usize>,
    count: Vec<usize>,
    stride: usize,
    weights: Vec<f32>,
}

/// PIL's `precompute_coeffs` for the bilinear (triangle, support 1) filter.
fn taps(input: usize, output: usize) -> Taps {
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
                *wk = (*wk as f64 / total) as f32;
            }
        }
        t.start.push(lo);
        t.count.push(n);
    }
    t
}

fn threads() -> usize {
    thread::available_parallelism().map_or(1, |n| n.get())
}

/// Runs `f(row_index, row)` over `rows` rows of `row_len` values, split across threads.
fn par_rows(out: &mut [f32], row_len: usize, f: impl Fn(usize, &mut [f32]) + Sync) {
    let rows = out.len() / row_len.max(1);
    let per = rows.div_ceil(threads()).max(1);
    thread::scope(|s| {
        for (chunk_index, chunk) in out.chunks_mut(per * row_len).enumerate() {
            let f = &f;
            s.spawn(move || {
                for (i, row) in chunk.chunks_mut(row_len).enumerate() {
                    f(chunk_index * per + i, row);
                }
            });
        }
    });
}

/// `image` resampled to `width × height`, as interleaved RGB `f32` codes in `0..=255`.
pub fn resample(image: &Rgb8, width: u32, height: u32) -> Vec<f32> {
    let (sw, sh) = (image.width as usize, image.height as usize);
    let (dw, dh) = (width as usize, height as usize);
    assert!(sw > 0 && sh > 0 && dw > 0 && dh > 0, "empty resample");
    let tx = taps(sw, dw);
    let ty = taps(sh, dh);

    // Horizontal pass over every source row, then the vertical pass per output row.
    let mut across = vec![0.0f32; sh * dw * 3];
    par_rows(&mut across, dw * 3, |y, out| {
        let src = &image.data[y * sw * 3..(y + 1) * sw * 3];
        for (x, px) in out.chunks_exact_mut(3).enumerate() {
            let w = &tx.weights[x * tx.stride..x * tx.stride + tx.count[x]];
            let base = tx.start[x] * 3;
            let (mut r, mut g, mut b) = (0.0f32, 0.0f32, 0.0f32);
            for (k, &wk) in w.iter().enumerate() {
                let i = base + k * 3;
                r += wk * src[i] as f32;
                g += wk * src[i + 1] as f32;
                b += wk * src[i + 2] as f32;
            }
            px[0] = r;
            px[1] = g;
            px[2] = b;
        }
    });

    let mut out = vec![0.0f32; dh * dw * 3];
    let row = dw * 3;
    par_rows(&mut out, row, |y, dst| {
        let w = &ty.weights[y * ty.stride..y * ty.stride + ty.count[y]];
        for (k, &wk) in w.iter().enumerate() {
            let src = &across[(ty.start[y] + k) * row..(ty.start[y] + k + 1) * row];
            for (d, s) in dst.iter_mut().zip(src) {
                *d += wk * s;
            }
        }
    });
    out
}

/// `resample` rounded to 8-bit codes.
pub fn resample_rgb8(image: &Rgb8, width: u32, height: u32) -> Rgb8 {
    let data = resample(image, width, height)
        .into_iter()
        .map(|v| v.round().clamp(0.0, 255.0) as u8)
        .collect();
    Rgb8::new(width, height, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Rgb8 {
        let mut data = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&f(x, y));
            }
        }
        Rgb8::new(w, h, data)
    }

    #[test]
    fn weights_sum_to_one_and_stay_in_bounds() {
        for (input, output) in [
            (6048, 512),
            (4024, 341),
            (7, 3),
            (3, 7),
            (512, 512),
            (10, 1),
        ] {
            let t = taps(input, output);
            for i in 0..output {
                assert!(t.start[i] + t.count[i] <= input);
                let sum: f32 = t.weights[i * t.stride..i * t.stride + t.count[i]]
                    .iter()
                    .sum();
                assert!((sum - 1.0).abs() < 1e-5, "{input}->{output} at {i}: {sum}");
            }
        }
    }

    #[test]
    fn a_flat_image_stays_flat() {
        let img = image(97, 61, |_, _| [10, 128, 250]);
        let out = resample(&img, 13, 9);
        for px in out.chunks_exact(3) {
            assert!((px[0] - 10.0).abs() < 1e-3);
            assert!((px[1] - 128.0).abs() < 1e-3);
            assert!((px[2] - 250.0).abs() < 1e-3);
        }
    }

    #[test]
    fn an_integer_reduction_matches_pil() {
        // A 2:1 reduction with PIL's bilinear kernel: support 2, taps at distances 0.25 and 0.75
        // from the output centre get 0.375 each, the two outer ones 0.125.
        let img = image(8, 1, |x, _| [(x * 10) as u8, 0, 0]);
        let out = resample(&img, 4, 1);
        // Output 1 centres on input 2.5: 0.125·10 + 0.375·20 + 0.375·30 + 0.125·40 = 25.
        assert!((out[3] - 25.0).abs() < 1e-4, "{}", out[3]);
        // Output 0 at the edge renormalizes over inputs 0..=2: (0.375·0 + 0.375·10 + 0.125·20) / 0.875.
        assert!((out[0] - 6.25 / 0.875).abs() < 1e-4, "{}", out[0]);
    }

    #[test]
    fn channels_and_rows_stay_in_place() {
        let img = image(40, 30, |x, y| [x as u8 * 6, y as u8 * 8, 77]);
        let out = resample_rgb8(&img, 20, 15);
        // Output (i, j) centres on input (2i + 0.5, 2j + 0.5): red 12i + 3, green 16j + 4.
        assert_eq!(out.pixel(5, 7), [63, 116, 77]);
    }

    #[test]
    fn identity_size_is_exact() {
        let img = image(9, 5, |x, y| [(x * 20) as u8, (y * 40) as u8, 3]);
        assert_eq!(resample_rgb8(&img, 9, 5), img);
    }
}
