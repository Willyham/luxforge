//! Independent f64 Detail equations. No production implementation is imported.
//! See `docs/design/detail-study.md` for mappings, support and measured qualification limits.

use crate::colour::{self, Oklab};

pub const DETAIL_DENOISE_HALO_MAX: u32 = 32;
pub const DETAIL_SHARPEN_HALO_MAX: u32 = 16;
pub const NEUTRAL_CHROMA_SNAP: f64 = 1e-6;
pub const BAND_NOISE: [f64; 4] = [0.8914, 0.1992, 0.0860, 0.0417];
pub const LUMINANCE_THRESHOLD: f64 = 0.10;
pub const CHROMA_THRESHOLD: f64 = 0.10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub sharpening: f64,
    pub radius: f64,
    pub sharpen_detail: f64,
    pub sharpen_masking: f64,
    pub luminance: f64,
    pub luminance_detail: f64,
    pub colour: f64,
    pub colour_detail: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            sharpening: 0.0,
            radius: 1.0,
            sharpen_detail: 25.0,
            sharpen_masking: 0.0,
            luminance: 0.0,
            luminance_detail: 50.0,
            colour: 0.0,
            colour_detail: 50.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[f64; 3]>,
}

impl Image {
    pub fn new(width: usize, height: usize, pixels: Vec<[f64; 3]>) -> Self {
        assert!(width > 0 && height > 0);
        assert_eq!(pixels.len(), width * height);
        Self {
            width,
            height,
            pixels,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Kernel {
    pub taps: Vec<(isize, f64)>,
    pub radius: usize,
}

impl Kernel {
    pub fn b3(spacing: usize) -> Self {
        Self {
            taps: [
                (-2, 1.0 / 16.0),
                (-1, 4.0 / 16.0),
                (1, 4.0 / 16.0),
                (2, 1.0 / 16.0),
            ]
            .into_iter()
            .map(|(i, weight)| (i * spacing as isize, weight))
            .collect(),
            radius: spacing * 2,
        }
    }

    pub fn gaussian(sigma: f64) -> Self {
        let radius = (3.0 * sigma).ceil().max(1.0) as usize;
        let weights: Vec<f64> = (-(radius as isize)..=radius as isize)
            .map(|i| {
                if sigma * sigma == 0.0 {
                    f64::from(i == 0)
                } else {
                    (-(i as f64).powi(2) / (2.0 * sigma * sigma)).exp()
                }
            })
            .collect();
        let sum: f64 = weights.iter().sum();
        Self {
            taps: (-(radius as isize)..=radius as isize)
                .filter(|i| *i != 0)
                .map(|i| (i, weights[(i + radius as isize) as usize] / sum))
                .collect(),
            radius,
        }
    }
}

/// Separable difference form, with fixed ascending noncentral tap order.
pub fn smooth(source: &[f64], width: usize, height: usize, x: &Kernel, y: &Kernel) -> Vec<f64> {
    let mut horizontal = vec![0.0; source.len()];
    let mut result = vec![0.0; source.len()];
    for row in 0..height {
        for column in 0..width {
            let at = row * width + column;
            let center = source[at];
            let mut value = center;
            for &(offset, weight) in &x.taps {
                let sample = (column as isize + offset).clamp(0, width as isize - 1) as usize;
                value += weight * (source[row * width + sample] - center);
            }
            horizontal[at] = value;
        }
    }
    for row in 0..height {
        for column in 0..width {
            let at = row * width + column;
            let center = horizontal[at];
            let mut value = center;
            for &(offset, weight) in &y.taps {
                let sample = (row as isize + offset).clamp(0, height as isize - 1) as usize;
                value += weight * (horizontal[sample * width + column] - center);
            }
            result[at] = value;
        }
    }
    result
}

fn sample(plane: &[f64], width: usize, height: usize, x: isize, y: isize) -> f64 {
    plane
        [y.clamp(0, height as isize - 1) as usize * width + x.clamp(0, width as isize - 1) as usize]
}

fn lab_planes(image: &Image) -> [Vec<f64>; 3] {
    let mut planes = std::array::from_fn(|_| Vec::with_capacity(image.pixels.len()));
    for &pixel in &image.pixels {
        let lab = colour::to_oklab(pixel);
        planes[0].push(lab.l);
        planes[1].push(lab.a);
        planes[2].push(lab.b);
    }
    planes
}

fn reconstruct(input: [f64; 3], lab: [f64; 3], delta: [f64; 3]) -> [f64; 3] {
    if delta.iter().all(|v| *v == 0.0) {
        return input;
    }
    let [l, a, b] = std::array::from_fn(|c| lab[c] + delta[c]);
    if a.abs() <= NEUTRAL_CHROMA_SNAP && b.abs() <= NEUTRAL_CHROMA_SNAP {
        [l * l * l; 3]
    } else {
        colour::from_oklab(Oklab { l, a, b })
    }
}

pub fn denoise_halo(scale: [f64; 2], chroma_levels: usize) -> u32 {
    if scale == [1.0, 1.0] {
        return (2 * ((1 << chroma_levels) - 1) + 1) as u32;
    }
    scale
        .into_iter()
        .map(|s| {
            (0..chroma_levels)
                .map(|j| Kernel::gaussian((1 << j) as f64 * s).radius)
                .sum::<usize>()
                + 1
        })
        .max()
        .unwrap() as u32
}

pub fn sharpen_halo(radius: f64, scale: [f64; 2]) -> u32 {
    scale
        .into_iter()
        .map(|s| {
            Kernel::gaussian(radius * s)
                .radius
                .max(Kernel::gaussian(s).radius + 1)
                .max(1)
        })
        .max()
        .unwrap() as u32
}

pub fn denoise(image: &Image, params: Params, scale: [f64; 2], chroma_levels: usize) -> Image {
    if params.luminance == 0.0 && params.colour == 0.0 {
        return image.clone();
    }
    let (width, height) = (image.width, image.height);
    let original = lab_planes(image);
    let mut coarse = original.clone();
    let mut delta: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; image.pixels.len()]);
    for (j, &noise) in BAND_NOISE.iter().enumerate().take(chroma_levels) {
        let spacing = 1 << j;
        let kernels = if scale == [1.0, 1.0] {
            [Kernel::b3(spacing), Kernel::b3(spacing)]
        } else {
            [
                Kernel::gaussian(spacing as f64 * scale[0]),
                Kernel::gaussian(spacing as f64 * scale[1]),
            ]
        };
        let next: [Vec<f64>; 3] =
            std::array::from_fn(|c| smooth(&coarse[c], width, height, &kernels[0], &kernels[1]));
        let band: [Vec<f64>; 3] =
            std::array::from_fn(|c| coarse[c].iter().zip(&next[c]).map(|(a, b)| a - b).collect());
        let thresholds = [
            if j < 3 {
                LUMINANCE_THRESHOLD * (params.luminance / 100.0).powf(1.5) * noise
            } else {
                0.0
            },
            CHROMA_THRESHOLD * (params.colour / 100.0).powf(1.5) * noise,
        ];
        for y in 0..height {
            for x in 0..width {
                let at = y * width + x;
                for (kind, &threshold) in thresholds.iter().enumerate() {
                    if threshold == 0.0 {
                        continue;
                    }
                    let mut energy = 0.0;
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            if kind == 0 {
                                energy += sample(
                                    &band[0],
                                    width,
                                    height,
                                    x as isize + dx,
                                    y as isize + dy,
                                )
                                .powi(2);
                            } else {
                                energy += sample(
                                    &band[1],
                                    width,
                                    height,
                                    x as isize + dx,
                                    y as isize + dy,
                                )
                                .powi(2)
                                    + sample(
                                        &band[2],
                                        width,
                                        height,
                                        x as isize + dx,
                                        y as isize + dy,
                                    )
                                    .powi(2);
                            }
                        }
                    }
                    energy /= 9.0;
                    let protection = energy / (energy + (3.0 * threshold).powi(2));
                    let detail = if kind == 0 {
                        params.luminance_detail
                    } else {
                        params.colour_detail
                    } / 100.0;
                    let effective = threshold * (1.0 - detail * protection);
                    let magnitude_squared = if kind == 0 {
                        band[0][at].powi(2)
                    } else {
                        band[1][at].powi(2) + band[2][at].powi(2)
                    };
                    if magnitude_squared == 0.0 {
                        continue;
                    }
                    let factor = (1.0 - effective.powi(2) / magnitude_squared).max(0.0);
                    let channels = if kind == 0 { 0..1 } else { 1..3 };
                    for c in channels {
                        delta[c][at] += band[c][at] * factor - band[c][at];
                    }
                }
            }
        }
        coarse = next;
    }
    Image::new(
        width,
        height,
        image
            .pixels
            .iter()
            .enumerate()
            .map(|(at, &pixel)| {
                reconstruct(
                    pixel,
                    std::array::from_fn(|c| original[c][at]),
                    std::array::from_fn(|c| delta[c][at]),
                )
            })
            .collect(),
    )
}

pub fn sharpen(image: &Image, params: Params, scale: [f64; 2]) -> Image {
    if params.sharpening == 0.0 {
        return image.clone();
    }
    let (width, height) = (image.width, image.height);
    let planes = lab_planes(image);
    let l = &planes[0];
    let blurred = smooth(
        l,
        width,
        height,
        &Kernel::gaussian(params.radius * scale[0]),
        &Kernel::gaussian(params.radius * scale[1]),
    );
    let guide = smooth(
        l,
        width,
        height,
        &Kernel::gaussian(scale[0]),
        &Kernel::gaussian(scale[1]),
    );
    let theta = 0.01 * (1.0 - params.sharpen_detail / 100.0).powi(2);
    let masking = (params.sharpen_masking / 100.0 * 0.05).powi(2);
    let mut output = image.clone();
    for y in 0..height {
        for x in 0..width {
            let at = y * width + x;
            let residual = l[at] - blurred[at];
            let cored = if residual == 0.0 {
                0.0
            } else {
                residual * residual.powi(2) / (residual.powi(2) + theta.powi(2))
            };
            let dx = 0.5
                * (sample(&guide, width, height, x as isize + 1, y as isize)
                    - sample(&guide, width, height, x as isize - 1, y as isize));
            let dy = 0.5
                * (sample(&guide, width, height, x as isize, y as isize + 1)
                    - sample(&guide, width, height, x as isize, y as isize - 1));
            let e = dx * dx + dy * dy;
            let mask = if masking == 0.0 {
                1.0
            } else {
                e / (e + masking)
            };
            let proposed = l[at] + params.sharpening / 100.0 * cored * mask;
            let (mut lo, mut hi) = (l[at], l[at]);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let v = sample(l, width, height, x as isize + dx, y as isize + dy);
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            let limit = 0.04 * (hi - lo);
            let limited = if limit == 0.0 {
                l[at]
            } else if proposed > hi {
                hi + limit * ((proposed - hi) / limit).tanh()
            } else if proposed < lo {
                lo + limit * ((proposed - lo) / limit).tanh()
            } else {
                proposed
            };
            output.pixels[at] = reconstruct(
                image.pixels[at],
                [planes[0][at], planes[1][at], planes[2][at]],
                [limited - l[at], 0.0, 0.0],
            );
        }
    }
    output
}

pub fn apply(image: &Image, params: Params, scale: [f64; 2]) -> Image {
    sharpen(&denoise(image, params, scale, 4), params, scale)
}
