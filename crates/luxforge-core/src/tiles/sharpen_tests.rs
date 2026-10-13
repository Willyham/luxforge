//! Output sharpening against a stepwise reference that computes each pixel on its own, from the
//! untouched input, with a two-dimensional sum: every medium and amount, 8 and 16 bits, band edges,
//! image borders and clamping.

use super::*;

/// `width` × `height` opaque RGBA codes from a fixed sequence, up to `max`.
fn codes(width: u32, height: u32, max: u32, seed: u64) -> Vec<u32> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..width as usize * height as usize)
        .flat_map(|_| {
            let mut next = || {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((state >> 33) % u64::from(max + 1)) as u32
            };
            [next(), next(), next(), max]
        })
        .collect()
}

fn pixels(codes: &[u32], sixteen: bool) -> Pixels {
    if sixteen {
        Pixels::Sixteen(codes.iter().map(|&code| code as u16).collect())
    } else {
        Pixels::Eight(codes.iter().map(|&code| code as u8).collect())
    }
}

fn values(pixels: &Pixels) -> Vec<u32> {
    match pixels {
        Pixels::Eight(samples) => samples.iter().map(|&code| u32::from(code)).collect(),
        Pixels::Sixteen(samples) => samples.iter().map(|&code| u32::from(code)).collect(),
    }
}

/// The sharpened output computed pixel by pixel from `input` alone: its luma, the Gaussian of
/// whole weights summed over both axes at once with clamped coordinates, the change rounded half
/// away from zero and limited so no channel clips. Written from the design, not the code.
fn stepwise(
    input: &[u32],
    (width, height): (u32, u32),
    kernel: &SharpenKernel,
    max: i128,
) -> Vec<u32> {
    let (w, h) = (width as i128, height as i128);
    let r = (kernel.weights.len() / 2) as i128;
    let at = |x: i128, y: i128| ((y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) * 4) as usize;
    let y_of = |i: usize| {
        2126 * i128::from(input[i])
            + 7152 * i128::from(input[i + 1])
            + 722 * i128::from(input[i + 2])
    };
    let mut out = input.to_vec();
    for y in 0..h {
        for x in 0..w {
            let mut blurred = 0i128;
            for dy in -r..=r {
                for dx in -r..=r {
                    let weight = i128::from(kernel.weights[(dy + r) as usize])
                        * i128::from(kernel.weights[(dx + r) as usize]);
                    blurred += weight * y_of(at(x + dx, y + dy));
                }
            }
            let own = at(x, y);
            let high = y_of(own) * 4096 * 4096 - blurred;
            let scaled = i128::from(kernel.percent) * high;
            let denominator = 100 * 10_000 * 4096 * 4096i128;
            let mut change = (scaled.abs() + denominator / 2) / denominator;
            if scaled < 0 {
                change = -change;
            }
            let rgb = [input[own], input[own + 1], input[own + 2]].map(i128::from);
            let low = -rgb.iter().min().unwrap();
            let high = max - rgb.iter().max().unwrap();
            let change = change.clamp(low, high);
            for channel in 0..3 {
                out[own + channel] = (rgb[channel] + change) as u32;
            }
        }
    }
    out
}

fn every_setting() -> impl Iterator<Item = Sharpen> {
    SharpenMedium::ALL.into_iter().flat_map(|medium| {
        SharpenAmount::ALL
            .into_iter()
            .map(move |amount| Sharpen { medium, amount })
    })
}

#[test]
fn every_medium_and_amount_matches_the_stepwise_reference_across_band_edges() {
    // 37 rows in bands of 8 cross four band edges; 300 ppi and 600 ppi give Matte and Glossy
    // their default and a wider radius.
    let size = (29, 37);
    for (sixteen, max) in [(false, 255u32), (true, 65_535)] {
        let input = codes(size.0, size.1, max, u64::from(max));
        for density in [None, Some(600)] {
            for sharpen in every_setting() {
                let kernel = OutputSharpen::new(sharpen, density).kernel();
                let mut output = pixels(&input, sixteen);
                sharpen_in_bands(&mut output, size, &kernel, 8, &Cancel::never()).unwrap();
                assert_eq!(
                    values(&output),
                    stepwise(&input, size, &kernel, i128::from(max)),
                    "{sharpen:?} at {density:?} ppi, 16-bit {sixteen}"
                );
            }
        }
    }
}

#[test]
fn the_band_size_and_the_pool_change_no_code() {
    let size = (64, 150);
    let input = codes(size.0, size.1, 255, 7);
    let kernel = OutputSharpen::new(
        Sharpen {
            medium: SharpenMedium::Matte,
            amount: SharpenAmount::High,
        },
        Some(720),
    )
    .kernel();
    let expected = stepwise(&input, size, &kernel, 255);
    for (band, pooled) in [
        (1, Some(false)),
        (3, Some(true)),
        (64, Some(false)),
        (500, Some(true)),
    ] {
        let previous = crate::render::parallel::force(pooled);
        let mut output = pixels(&input, false);
        sharpen_in_bands(&mut output, size, &kernel, band, &Cancel::never()).unwrap();
        crate::render::parallel::force(previous);
        assert_eq!(
            values(&output),
            expected,
            "bands of {band}, pooled {pooled:?}"
        );
    }
}

#[test]
fn images_smaller_than_the_kernel_clamp_to_their_borders() {
    let kernel = OutputSharpen::new(
        Sharpen {
            medium: SharpenMedium::Matte,
            amount: SharpenAmount::High,
        },
        Some(720),
    )
    .kernel();
    assert!(kernel.radius() >= 7, "{}", kernel.radius());
    for size in [(1, 1), (1, 9), (9, 1), (2, 3), (5, 4)] {
        let input = codes(size.0, size.1, 255, u64::from(size.0 * 31 + size.1));
        let mut output = pixels(&input, false);
        sharpen(&mut output, size, &kernel, &Cancel::never()).unwrap();
        assert_eq!(
            values(&output),
            stepwise(&input, size, &kernel, 255),
            "{size:?}"
        );
    }
    // One pixel is its own blur: nothing changes.
    let one = codes(1, 1, 255, 3);
    let mut output = pixels(&one, false);
    sharpen(&mut output, (1, 1), &kernel, &Cancel::never()).unwrap();
    assert_eq!(values(&output), one);
}

#[test]
fn a_step_edge_overshoots_by_the_exact_amount_and_flat_areas_keep_their_codes() {
    // A vertical edge from 100 to 140 grey across a 12 × 4 image, sharpened for Screen Standard:
    // flat columns away from it keep their codes; the two either side overshoot symmetrically.
    let kernel = OutputSharpen::new(
        Sharpen {
            medium: SharpenMedium::Screen,
            amount: SharpenAmount::Standard,
        },
        None,
    )
    .kernel();
    assert_eq!(kernel.weights, [11, 678, 2718, 678, 11], "σ 0.6, r 2");
    assert_eq!(kernel.percent, 70);
    let input: Vec<u32> = (0..4)
        .flat_map(|_| {
            (0..12).flat_map(|x| {
                if x < 6 {
                    [100, 100, 100, 255]
                } else {
                    [140, 140, 140, 255]
                }
            })
        })
        .collect();
    let mut output = pixels(&input, false);
    sharpen(&mut output, (12, 4), &kernel, &Cancel::never()).unwrap();
    let row: Vec<u32> = values(&output)[..12 * 4]
        .chunks(4)
        .map(|pixel| pixel[0])
        .collect();
    // Column 5's blur takes 689 / 4096 of the step: 0.7 × 40 × 689 / 4096 = 4.71, so it falls
    // to 95 and column 6 rises to 145; column 4 takes 11 / 4096, too little to round to a code.
    assert_eq!(
        row,
        [100, 100, 100, 100, 100, 95, 145, 140, 140, 140, 140, 140]
    );
}

#[test]
fn luminance_only_sharpening_keeps_every_chroma_on_saturated_edges() {
    // Saturated reds, greens, blues and their mixes against black, white and grey, where a
    // per-channel sharpen would clip and shift hue: every pixel's channel differences, and so
    // its chroma, are unchanged, and the luma still moves wherever there is room.
    let colours = [
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [255, 255, 0],
        [0, 0, 0],
        [255, 255, 255],
        [128, 128, 128],
        [200, 30, 60],
    ];
    let size = (24u32, 24u32);
    let input: Vec<u32> = (0..size.1)
        .flat_map(|y| {
            (0..size.0).flat_map(move |x| {
                let [r, g, b] = colours[((x / 3 + y / 3) % colours.len() as u32) as usize];
                [r, g, b, 255]
            })
        })
        .collect();
    for sharpen in every_setting() {
        let kernel = OutputSharpen::new(sharpen, Some(600)).kernel();
        let mut output = pixels(&input, false);
        super::sharpen(&mut output, size, &kernel, &Cancel::never()).unwrap();
        let output = values(&output);
        let mut moved = 0;
        for (before, after) in input.chunks(4).zip(output.chunks(4)) {
            let difference = |p: &[u32]| [p[0] as i64 - p[1] as i64, p[1] as i64 - p[2] as i64];
            assert_eq!(
                difference(before),
                difference(after),
                "{sharpen:?}: {before:?} → {after:?}"
            );
            if before != after {
                moved += 1;
            }
        }
        assert!(moved > 0, "{sharpen:?} changed nothing");
        // Pure red, already at the gamut's edge on two channels, cannot move at all.
        let red = input
            .chunks(4)
            .zip(output.chunks(4))
            .find(|(before, _)| before[..3] == [255, 0, 0]);
        let (before, after) = red.unwrap();
        assert_eq!(before, after);
    }
}

#[test]
fn the_radius_follows_the_medium_and_the_density() {
    let sigma = |medium, density| {
        OutputSharpen::new(
            Sharpen {
                medium,
                amount: SharpenAmount::Standard,
            },
            density,
        )
        .sigma()
    };
    assert_eq!(sigma(SharpenMedium::Screen, Some(72)), 0.6);
    assert_eq!(sigma(SharpenMedium::Screen, Some(600)), 0.6);
    assert_eq!(sigma(SharpenMedium::Glossy, None), 300.0 / 360.0);
    assert_eq!(sigma(SharpenMedium::Matte, None), 300.0 / 240.0);
    assert_eq!(sigma(SharpenMedium::Matte, Some(150)), 0.625);
    assert_eq!(
        sigma(SharpenMedium::Glossy, Some(72)),
        0.5,
        "held to the smallest"
    );
    assert_eq!(
        sigma(SharpenMedium::Matte, Some(2400)),
        3.0,
        "held to the largest"
    );
    for medium in SharpenMedium::ALL {
        let percents = SharpenAmount::ALL.map(|amount| amount_percent(medium, amount));
        assert!(
            percents[0] < percents[1] && percents[1] < percents[2],
            "{medium:?}"
        );
    }
    for amount in SharpenAmount::ALL {
        assert!(
            amount_percent(SharpenMedium::Matte, amount)
                > amount_percent(SharpenMedium::Glossy, amount)
        );
    }
    for sigma in [0.5, 0.6, 0.833, 1.25, 3.0] {
        let kernel = SharpenKernel::gaussian(sigma, 100);
        assert_eq!(kernel.weights.iter().sum::<i64>(), WEIGHT_SUM, "{sigma}");
        assert_eq!(kernel.radius(), (3.0 * sigma).ceil() as u32);
        assert!(
            kernel
                .weights
                .iter()
                .zip(kernel.weights.iter().rev())
                .all(|(a, b)| a == b)
        );
    }
}

#[test]
fn a_cancelled_sharpen_stops_before_its_first_band() {
    let cancel = Cancel::new();
    cancel.cancel();
    let input = codes(8, 8, 255, 1);
    let mut output = pixels(&input, false);
    let kernel = SharpenKernel::gaussian(0.6, 70);
    let error = sharpen(&mut output, (8, 8), &kernel, &cancel).unwrap_err();
    assert_eq!(error.kind.code(), "cancelled");
    assert_eq!(values(&output), input);
}

/// `input` of a `width` × `height` output pushed through a [`SharpenStream`] in bands of the row
/// counts `cuts` gives in turn, the bands it hands on joined; each handed on in order, every row
/// but the radius's above the newest band's last at once.
fn streamed(
    input: &Pixels,
    (width, height): (u32, u32),
    kernel: &SharpenKernel,
    cuts: &[u32],
) -> Vec<u32> {
    let sixteen = input.depth() == Depth::Sixteen;
    let mut stream = SharpenStream::new(kernel.clone(), (width, height), input.depth());
    let stride = width as usize * 4;
    let all = values(input);
    let (mut y, mut handed, mut out) = (0u32, 0u32, Vec::new());
    for &cut in cuts.iter().cycle() {
        if y >= height {
            break;
        }
        let rows = cut.min(height - y);
        let band = pixels(
            &all[y as usize * stride..(y + rows) as usize * stride],
            sixteen,
        );
        for (count, ready) in stream.push(rows, band, &Cancel::never()).unwrap() {
            handed += count;
            out.extend(values(&ready));
        }
        y += rows;
        let waiting = if y == height {
            0
        } else {
            kernel.radius().min(y)
        };
        assert_eq!(handed, y - waiting, "after row {y}");
    }
    out
}

/// A stream of bands of any height — one row, fewer rows than the radius, several groups, an
/// uneven last — hands on exactly the whole frame sharpened, at 8 and 16 bits, at the narrowest
/// and widest radius, and on images smaller than the kernel.
#[test]
fn a_streamed_output_is_sharpened_exactly_as_the_whole_frame() {
    let cuts: [&[u32]; 4] = [&[1], &[2, 5, 1], &[7, 64, 3], &[200]];
    for (width, height, seed) in [(37, 150, 3), (5, 4, 4), (1, 9, 5)] {
        for sixteen in [false, true] {
            let max = if sixteen { 65_535 } else { 255 };
            let input = pixels(&codes(width, height, max, seed), sixteen);
            for sigma in [0.6, 3.0] {
                let kernel = SharpenKernel::gaussian(sigma, 170);
                let mut whole = input.clone();
                sharpen(&mut whole, (width, height), &kernel, &Cancel::never()).unwrap();
                for cut in cuts {
                    assert_eq!(
                        streamed(&input, (width, height), &kernel, cut),
                        values(&whole),
                        "{width}x{height} at {sigma}, sixteen {sixteen}, bands {cut:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_streamed_band_of_another_depth_or_past_the_output_is_refused() {
    let kernel = SharpenKernel::gaussian(0.6, 70);
    let mut stream = SharpenStream::new(kernel.clone(), (4, 4), Depth::Eight);
    let sixteen = pixels(&codes(4, 2, 65_535, 1), true);
    let error = stream.push(2, sixteen, &Cancel::never()).unwrap_err();
    assert_eq!(error.kind.code(), "internal");
    let mut stream = SharpenStream::new(kernel, (4, 4), Depth::Eight);
    let five = pixels(&codes(4, 5, 255, 1), false);
    assert!(stream.push(5, five, &Cancel::never()).is_err());
}
