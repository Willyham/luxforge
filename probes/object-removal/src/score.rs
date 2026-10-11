//! Scoring a removal against its ground truth.
//!
//! A removal replaces a **region** of an input image. Given the input, the result, the region and,
//! when there is one, the clean truth the region covered, the score reports:
//!
//! - PSNR and SSIM **inside the region** and in its **edge ring**, against the truth;
//! - the number of pixels **outside the region** that differ between result and input, which must
//!   be 0 (a pixel with coverage 0 is byte-identical by contract);
//! - LPIPS, which is `skipped` with its reason until weights are pinned.
//!
//! Definitions, all in 8-bit sRGB codes:
//!
//! - **Region**: pixels with non-zero coverage.
//! - **Edge ring**: the region's own pixels within `width` pixels (a square neighbourhood, so
//!   Chebyshev distance) of a pixel outside it. This is where the fill meets the photograph, so
//!   where a seam shows. The image border is not an edge: nothing meets it. Outside the region the
//!   result is the input, so the outside is checked for change rather than scored.
//! - **PSNR**: `10 log10(255² / MSE)` over every channel of every counted pixel, infinite when the
//!   two are identical.
//! - **SSIM**: Wang et al. (2004) with an 11×11 Gaussian window (σ = 1.5), K1 = 0.01, K2 = 0.03,
//!   L = 255 and population covariance, computed per channel with the image edge replicated, and
//!   averaged over the three channels and then over the counted pixels. Windows straddle the
//!   region's boundary and read the pixels on both sides.

use serde_json::{Value, json};

use crate::fill;
use crate::image::Rgb8;
use crate::mask::Mask;

/// The edge ring's default width in pixels.
pub const RING_WIDTH: u32 = 8;

const WINDOW: usize = 11;
const HALF: usize = WINDOW / 2;
const SIGMA: f64 = 1.5;
const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);

/// Why LPIPS is not reported.
pub const LPIPS_SKIPPED: &str = "no LPIPS weights are pinned: the backbone and linear heads, their size and SHA-256, \
     their licence and any restriction on the person's use of their results are not yet recorded";

/// PSNR and SSIM over one set of pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quality {
    pub pixels: u64,
    /// `f64::INFINITY` when the two are identical there.
    pub psnr_db: f64,
    pub ssim: f64,
}

/// The score of one removal.
#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    pub image: (u32, u32),
    pub region_pixels: u64,
    pub ring_width: u32,
    pub ring_pixels: u64,
    /// `None` without a ground truth.
    pub region: Option<Quality>,
    pub ring: Option<Quality>,
    /// Pixels with coverage 0 whose value differs between result and input.
    pub changed_outside: u64,
}

impl Score {
    /// Whether every pixel outside the region is byte-identical to the input: the one check that
    /// holds for every removal, with or without a truth.
    pub fn outside_unchanged(&self) -> bool {
        self.changed_outside == 0
    }

    pub fn to_json(&self) -> Value {
        let quality = |q: &Option<Quality>| match q {
            Some(q) => json!({
                "pixels": q.pixels,
                "psnr_db": number(q.psnr_db),
                "ssim": q.ssim,
            }),
            None => json!({"status": "skipped", "reason": "no ground truth was given"}),
        };
        json!({
            "image": [self.image.0, self.image.1],
            "region_pixels": self.region_pixels,
            "ring": {"width": self.ring_width, "pixels": self.ring_pixels},
            "region": quality(&self.region),
            "edge_ring": quality(&self.ring),
            "changed_pixels_outside_region": {
                "count": self.changed_outside,
                "required": 0,
                "pass": self.outside_unchanged(),
            },
            "lpips": {"status": "skipped", "reason": LPIPS_SKIPPED},
        })
    }
}

/// A JSON number, or the string `"infinity"` for an exact match.
fn number(v: f64) -> Value {
    if v.is_finite() {
        json!(v)
    } else {
        json!("infinity")
    }
}

/// Scores `result` against `input` inside `region`, and against `truth` when given.
pub fn score(
    input: &Rgb8,
    result: &Rgb8,
    region: &Mask,
    truth: Option<&Rgb8>,
    ring_width: u32,
) -> Result<Score, String> {
    let size = (input.width, input.height);
    let same = |name: &str, other: (u32, u32)| {
        if other == size {
            Ok(())
        } else {
            Err(format!(
                "the {name} is {}×{}, the input {}×{}",
                other.0, other.1, size.0, size.1
            ))
        }
    };
    same("result", (result.width, result.height))?;
    same("region", region.image)?;
    if let Some(t) = truth {
        same("truth", (t.width, t.height))?;
    }
    let ring_mask = ring(region, ring_width);
    let region_pixels = covered(region);
    let ring_pixels = covered(&ring_mask);
    let (region_q, ring_q) = match truth {
        Some(t) => (
            Some(quality(result, t, region)),
            Some(quality(result, t, &ring_mask)),
        ),
        None => (None, None),
    };
    Ok(Score {
        image: size,
        region_pixels,
        ring_width,
        ring_pixels,
        region: region_q,
        ring: ring_q,
        changed_outside: changed_outside(input, result, region),
    })
}

fn covered(mask: &Mask) -> u64 {
    mask.data.iter().filter(|&&v| v > 0).count() as u64
}

/// The pixels with coverage 0 whose value differs between `input` and `result`.
pub fn changed_outside(input: &Rgb8, result: &Rgb8, region: &Mask) -> u64 {
    let mut changed = 0;
    for y in 0..input.height {
        let row = y as usize * input.width as usize * 3;
        for x in 0..input.width {
            if region.get(x, y) != 0 {
                continue;
            }
            let i = row + x as usize * 3;
            if input.data[i..i + 3] != result.data[i..i + 3] {
                changed += 1;
            }
        }
    }
    changed
}

/// The region's edge ring: its pixels within `width` pixels (a square neighbourhood) of a pixel
/// outside it, at full coverage. The image border is not an edge.
pub fn ring(region: &Mask, width: u32) -> Mask {
    let image = region.image;
    let Some(bounds) = region.bounds() else {
        return Mask::empty(image);
    };
    // Everything beyond the bounds is outside the region, so a window of the bounds grown by the
    // ring's width holds every pixel the dilation of the outside can reach inside the region.
    let window = bounds.grown(width, image);
    let (w, h) = (window.width as usize, window.height as usize);
    let outside: Vec<bool> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| region.get(window.x + x as u32, window.y + y as u32) == 0)
        .collect();
    let near = fill::dilate(&outside, w, h, width as usize);
    let data = outside
        .iter()
        .zip(&near)
        .map(|(&out, &near)| if !out && near { 255 } else { 0 })
        .collect();
    Mask::new(image, window, data)
}

/// PSNR and SSIM of `a` against `b` over the pixels with non-zero coverage.
pub fn quality(a: &Rgb8, b: &Rgb8, mask: &Mask) -> Quality {
    Quality {
        pixels: covered(mask),
        psnr_db: psnr(a, b, mask),
        ssim: ssim(a, b, mask),
    }
}

/// PSNR over the pixels with non-zero coverage, every channel, in dB; infinite when identical.
/// An empty mask is infinite too, as nothing differs there.
pub fn psnr(a: &Rgb8, b: &Rgb8, mask: &Mask) -> f64 {
    let (mut se, mut n) = (0f64, 0u64);
    for y in mask.window.y..mask.window.y + mask.window.height {
        for x in mask.window.x..mask.window.x + mask.window.width {
            if mask.get(x, y) == 0 {
                continue;
            }
            let (p, q) = (a.pixel(x, y), b.pixel(x, y));
            for c in 0..3 {
                let d = p[c] as f64 - q[c] as f64;
                se += d * d;
            }
            n += 3;
        }
    }
    if se == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / (se / n as f64)).log10()
}

/// The 11-tap Gaussian, normalised to sum to 1.
fn kernel() -> [f64; WINDOW] {
    let mut k = [0.0; WINDOW];
    for (i, v) in k.iter_mut().enumerate() {
        let d = i as f64 - HALF as f64;
        *v = (-d * d / (2.0 * SIGMA * SIGMA)).exp();
    }
    let sum: f64 = k.iter().sum();
    k.map(|v| v / sum)
}

/// Gaussian-filters a `w × h` plane, replicating its edge.
fn blur(plane: &[f64], w: usize, h: usize, k: &[f64; WINDOW]) -> Vec<f64> {
    let mut rows = vec![0.0; w * h];
    for (src, out) in plane.chunks_exact(w).zip(rows.chunks_exact_mut(w)) {
        for (x, o) in out.iter_mut().enumerate() {
            *o = k
                .iter()
                .enumerate()
                .map(|(i, &t)| t * src[(x + i).saturating_sub(HALF).min(w - 1)])
                .sum();
        }
    }
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for (i, &t) in k.iter().enumerate() {
            let sy = (y + i).saturating_sub(HALF).min(h - 1);
            for (o, &v) in out[y * w..(y + 1) * w]
                .iter_mut()
                .zip(&rows[sy * w..(sy + 1) * w])
            {
                *o += t * v;
            }
        }
    }
    out
}

/// Mean SSIM of `a` against `b` over the pixels with non-zero coverage (see the module notes);
/// 1 for an empty mask, as nothing differs there.
pub fn ssim(a: &Rgb8, b: &Rgb8, mask: &Mask) -> f64 {
    let Some(bounds) = mask.bounds() else {
        return 1.0;
    };
    // The window holds every pixel a counted pixel's SSIM window reads; where it is clamped to the
    // image, the image edge is replicated, as the image's own edge is.
    let win = bounds.grown(HALF as u32, mask.image);
    let (w, h) = (win.width as usize, win.height as usize);
    let k = kernel();
    let mut total = 0.0;
    for c in 0..3 {
        let plane = |img: &Rgb8| -> Vec<f64> {
            (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| img.pixel(win.x + x as u32, win.y + y as u32)[c] as f64)
                .collect()
        };
        let (pa, pb) = (plane(a), plane(b));
        let square = |p: &[f64]| p.iter().map(|v| v * v).collect::<Vec<_>>();
        let cross: Vec<f64> = pa.iter().zip(&pb).map(|(x, y)| x * y).collect();
        let (ma, mb) = (blur(&pa, w, h, &k), blur(&pb, w, h, &k));
        let (saa, sbb, sab) = (
            blur(&square(&pa), w, h, &k),
            blur(&square(&pb), w, h, &k),
            blur(&cross, w, h, &k),
        );
        let mut sum = 0.0;
        for y in 0..h {
            for x in 0..w {
                if mask.get(win.x + x as u32, win.y + y as u32) == 0 {
                    continue;
                }
                let i = y * w + x;
                let (va, vb, cov) = (
                    saa[i] - ma[i] * ma[i],
                    sbb[i] - mb[i] * mb[i],
                    sab[i] - ma[i] * mb[i],
                );
                sum += (2.0 * ma[i] * mb[i] + C1) * (2.0 * cov + C2)
                    / ((ma[i] * ma[i] + mb[i] * mb[i] + C1) * (va + vb + C2));
            }
        }
        total += sum;
    }
    total / (3.0 * covered(mask) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask::RectPx;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> RectPx {
        RectPx {
            x,
            y,
            width,
            height,
        }
    }

    /// A region of full coverage over `r` in an image of `size`.
    fn region_of(size: (u32, u32), r: RectPx) -> Mask {
        Mask::new(size, r, vec![255; (r.width * r.height) as usize])
    }

    fn flat(w: u32, h: u32, v: u8) -> Rgb8 {
        Rgb8::new(w, h, vec![v; (w * h * 3) as usize])
    }

    /// The patterned pair the golden values below were computed for.
    fn pattern(w: u32, h: u32) -> (Rgb8, Rgb8) {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for y in 0..h {
            for x in 0..w {
                for c in 0..3 {
                    let v = (x * 37 + y * 91 + x * y * 13 + c * 50) % 256;
                    let d = ((x * 5 + y * 3 + c) % 17) as i32 - 8;
                    a.push(v as u8);
                    b.push((v as i32 + d).clamp(0, 255) as u8);
                }
            }
        }
        (Rgb8::new(w, h, a), Rgb8::new(w, h, b))
    }

    #[test]
    fn psnr_of_a_uniform_error_is_known() {
        // An error of 5 codes everywhere: MSE 25, so 10 log10(255² / 25) = 20 log10(51).
        let region = region_of((8, 8), rect(2, 2, 4, 4));
        let p = psnr(&flat(8, 8, 105), &flat(8, 8, 100), &region);
        assert!((p - 20.0 * 51f64.log10()).abs() < 1e-12, "{p}");
        assert!((p - 34.151_4).abs() < 1e-4, "{p}");
    }

    #[test]
    fn psnr_counts_only_the_region_and_is_infinite_when_identical() {
        let (a, mut b) = (flat(8, 8, 100), flat(8, 8, 100));
        let region = region_of((8, 8), rect(2, 2, 4, 4));
        assert_eq!(psnr(&a, &b, &region), f64::INFINITY);
        // An error outside the region does not enter.
        b.data[0] = 0;
        assert_eq!(psnr(&a, &b, &region), f64::INFINITY);
        // One channel of one pixel off by 16 inside: MSE = 256 / (16 pixels * 3 channels).
        let i = (3 * 8 + 3) * 3;
        b.data[i] = 84;
        let p = psnr(&a, &b, &region);
        let expected = 10.0 * (255.0f64 * 255.0 / (256.0 / 48.0)).log10();
        assert!((p - expected).abs() < 1e-12, "{p} against {expected}");
    }

    #[test]
    fn ssim_of_identical_images_is_one() {
        let (a, _) = pattern(24, 20);
        let region = region_of((24, 20), rect(6, 5, 8, 7));
        assert!((ssim(&a, &a, &region) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn ssim_of_two_flat_images_is_the_luminance_term() {
        // With no variance or covariance the structure term is C2 / C2, so SSIM is
        // (2 ab + C1) / (a² + b² + C1) with C1 = (0.01 * 255)².
        let region = region_of((20, 20), rect(4, 4, 8, 8));
        let s = ssim(&flat(20, 20, 100), &flat(20, 20, 120), &region);
        let expected = (2.0 * 100.0 * 120.0 + C1) / (100.0 * 100.0 + 120.0 * 120.0 + C1);
        assert!((expected - 24_006.502_5 / 24_406.502_5).abs() < 1e-12);
        assert!((s - expected).abs() < 1e-9, "{s} against {expected}");
    }

    #[test]
    fn ssim_matches_an_independent_implementation_on_a_pattern() {
        // Values from a direct per-pixel NumPy implementation of the definition in the module notes
        // (11×11 Gaussian, σ 1.5, replicated edge, population covariance, mean over three channels
        // and the counted pixels) on `pattern(24, 20)`: `scripts/ssim_golden.py`.
        let (a, b) = pattern(24, 20);
        let region = region_of((24, 20), rect(6, 5, 8, 7));
        let ring = ring(&region, 2);
        let q = quality(&a, &b, &region);
        let r = quality(&a, &b, &ring);
        assert_eq!((q.pixels, r.pixels), (56, 44));
        assert!((q.psnr_db - GOLDEN.0).abs() < 1e-9, "{}", q.psnr_db);
        assert!((q.ssim - GOLDEN.1).abs() < 1e-9, "{}", q.ssim);
        assert!((r.psnr_db - GOLDEN.2).abs() < 1e-9, "{}", r.psnr_db);
        assert!((r.ssim - GOLDEN.3).abs() < 1e-9, "{}", r.ssim);
    }

    /// Region PSNR and SSIM, ring PSNR and SSIM, from the independent implementation.
    const GOLDEN: (f64, f64, f64, f64) = (
        34.409_138_527_233_96,
        0.997_853_599_000_020_1,
        34.421_525_258_362_33,
        0.997_841_929_544_704_7,
    );

    #[test]
    fn ssim_reads_across_the_region_boundary() {
        // Only the counted pixels average, but their windows read the pixels around them: the same
        // pixels score differently once the surroundings differ.
        let (a, b) = pattern(24, 20);
        let region = region_of((24, 20), rect(10, 8, 2, 2));
        let with = ssim(&a, &b, &region);
        let mut b_outside = b.clone();
        for y in 3..16 {
            for x in 5..17 {
                if !region.window.contains(x, y) {
                    let i = ((y * 24 + x) * 3) as usize;
                    b_outside.data[i..i + 3].copy_from_slice(&a.data[i..i + 3]);
                }
            }
        }
        assert!(ssim(&a, &b_outside, &region) > with);
    }

    #[test]
    fn the_ring_is_the_regions_pixels_near_its_outside() {
        let region = region_of((30, 30), rect(10, 10, 10, 8));
        let r = ring(&region, 2);
        // A 10 × 8 rectangle less its (10 − 4) × (8 − 4) core.
        assert_eq!(covered(&r), 10 * 8 - 6 * 4);
        for y in 0..30 {
            for x in 0..30 {
                let inside = region.get(x, y) != 0;
                let core = (12..18).contains(&x) && (12..16).contains(&y);
                let expected = inside && !core;
                assert_eq!(r.get(x, y) != 0, expected, "({x}, {y})");
            }
        }
    }

    #[test]
    fn the_ring_width_is_a_square_neighbourhood() {
        let region = region_of((30, 30), rect(10, 10, 10, 10));
        // Width 1: the one-pixel frame.
        assert_eq!(covered(&ring(&region, 1)), 100 - 64);
        // A width of at least half the region's size leaves no core: the ring is the region.
        assert_eq!(covered(&ring(&region, 5)), 100);
        assert_eq!(covered(&ring(&region, 8)), 100);
        // Width 0 reaches nothing outside.
        assert_eq!(covered(&ring(&region, 0)), 0);
    }

    #[test]
    fn the_image_border_is_not_an_edge() {
        // A region in the top-left corner meets the border on two sides: only its other two sides
        // have a ring, 6 × 6 less the 4 × 4 corner that is 2 away from nothing outside.
        let region = region_of((10, 10), rect(0, 0, 6, 6));
        let r = ring(&region, 2);
        assert_eq!(covered(&r), 36 - 16);
        assert_eq!(r.get(0, 0), 0);
        assert_eq!(r.get(3, 0), 0);
        assert_eq!(r.get(5, 0), 255);
        assert_eq!(r.get(0, 5), 255);
        assert_eq!(r.get(3, 3), 0, "2 away from nothing outside");
        assert_eq!(r.get(4, 4), 255);
    }

    #[test]
    fn the_ring_of_a_soft_or_holed_region_counts_any_coverage_as_inside() {
        // A region with a hole: the hole's rim is ring too, since the hole is outside the region.
        let mut data = vec![255u8; 100];
        data[5 * 10 + 5] = 0;
        let region = Mask::new((10, 10), rect(0, 0, 10, 10), data);
        let r = ring(&region, 1);
        assert_eq!(covered(&r), 8);
        // Coverage of 1 is inside.
        let faint = Mask::new((10, 10), rect(2, 2, 1, 1), vec![1]);
        assert_eq!(covered(&ring(&faint, 3)), 1);
        assert_eq!(covered(&ring(&Mask::empty((10, 10)), 3)), 0);
    }

    #[test]
    fn outside_changes_are_counted_and_inside_changes_are_not() {
        let input = flat(10, 10, 50);
        let region = region_of((10, 10), rect(2, 2, 4, 4));
        let mut result = input.clone();
        // Inside: any change is allowed.
        result.data[(3 * 10 + 3) * 3] = 200;
        assert_eq!(changed_outside(&input, &result, &region), 0);
        // Outside: a change in one channel of one pixel counts once, however many channels.
        let i = (8 * 10 + 8) * 3;
        result.data[i] = 51;
        assert_eq!(changed_outside(&input, &result, &region), 1);
        result.data[i + 1] = 52;
        result.data[i + 2] = 53;
        result.data[0] = 0;
        assert_eq!(changed_outside(&input, &result, &region), 2);
        // A pixel of coverage 1 is inside.
        let faint = Mask::new((10, 10), rect(8, 8, 1, 1), vec![1]);
        assert_eq!(changed_outside(&input, &result, &faint), 2);
    }

    #[test]
    fn the_score_reports_every_field_and_fails_an_outside_change() {
        let (clean, _) = pattern(24, 20);
        let region = region_of((24, 20), rect(6, 5, 8, 7));
        // The input is the clean image with the region replaced; the result a perfect removal.
        let mut input = clean.clone();
        for y in 5..12 {
            for x in 6..14 {
                let i = ((y * 24 + x) * 3) as usize;
                input.data[i..i + 3].copy_from_slice(&[255, 0, 0]);
            }
        }
        let s = score(&input, &clean, &region, Some(&clean), 2).unwrap();
        assert!(s.outside_unchanged());
        let region_q = s.region.unwrap();
        assert_eq!(region_q.psnr_db, f64::INFINITY);
        assert!((region_q.ssim - 1.0).abs() < 1e-12);
        assert_eq!((s.region_pixels, s.ring_pixels), (56, 44));
        let j = s.to_json();
        assert_eq!(j["region"]["psnr_db"], "infinity");
        assert_eq!(j["changed_pixels_outside_region"]["count"], 0);
        assert_eq!(j["changed_pixels_outside_region"]["pass"], true);
        assert_eq!(j["lpips"]["status"], "skipped");
        assert!(j["lpips"]["reason"].as_str().unwrap().contains("weights"));

        // A result that touches a pixel outside the region fails the one required check.
        let mut bad = clean.clone();
        bad.data[0] ^= 1;
        let s = score(&input, &bad, &region, Some(&clean), 2).unwrap();
        assert_eq!(s.changed_outside, 1);
        assert_eq!(s.to_json()["changed_pixels_outside_region"]["pass"], false);
    }

    #[test]
    fn without_a_truth_only_the_outside_check_runs() {
        let input = flat(10, 10, 50);
        let region = region_of((10, 10), rect(2, 2, 4, 4));
        let s = score(&input, &input, &region, None, 2).unwrap();
        assert!(s.region.is_none() && s.ring.is_none());
        let j = s.to_json();
        assert_eq!(j["region"]["status"], "skipped");
        assert_eq!(j["edge_ring"]["status"], "skipped");
        assert_eq!(j["changed_pixels_outside_region"]["pass"], true);
    }

    #[test]
    fn mismatched_sizes_are_refused() {
        let region = region_of((10, 10), rect(2, 2, 4, 4));
        let e = score(&flat(10, 10, 0), &flat(10, 12, 0), &region, None, 2).unwrap_err();
        assert!(e.contains("result"), "{e}");
        let e = score(
            &flat(10, 10, 0),
            &flat(10, 10, 0),
            &region,
            Some(&flat(9, 10, 0)),
            2,
        )
        .unwrap_err();
        assert!(e.contains("truth"), "{e}");
        let wrong = region_of((12, 12), rect(2, 2, 4, 4));
        let e = score(&flat(10, 10, 0), &flat(10, 10, 0), &wrong, None, 2).unwrap_err();
        assert!(e.contains("region"), "{e}");
    }
}
