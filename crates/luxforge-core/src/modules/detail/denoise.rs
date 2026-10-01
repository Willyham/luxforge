use super::filters::{self, Geometry, Kernel};
use crate::{
    Cancel, Error,
    modules::{Global, Parallelism, Planes, PlanesMut, SamplingScale, SpatialUnit, Stage},
};

const BAND_NOISE: [f64; 4] = [0.8914, 0.1992, 0.0860, 0.0417];
const LUMINANCE_THRESHOLD: f64 = 0.10;
const CHROMA_THRESHOLD: f64 = 0.10;

#[derive(Debug)]
pub(super) struct Denoise {
    scale: SamplingScale,
    luminance: f64,
    colour: f64,
    luminance_detail: f32,
    colour_detail: f32,
    thresholds: Vec<[f32; 2]>,
    kernels: Vec<[Kernel; 2]>,
    halo: u32,
}
impl Denoise {
    pub fn new(
        luminance: f64,
        luminance_detail: f64,
        colour: f64,
        colour_detail: f64,
        scale: SamplingScale,
    ) -> Self {
        let levels = if colour == 0.0 { 3 } else { 4 };
        let mut kernels = filters::denoise_kernels(scale);
        kernels.truncate(levels);
        let halo = (0..2)
            .map(|axis| kernels.iter().map(|k| k[axis].radius).sum::<u32>() + 1)
            .max()
            .unwrap();
        debug_assert!(
            halo <= if scale.x == 1.0 && scale.y == 1.0 {
                filters::DENOISE_EXACT_HALO_MAX
            } else {
                filters::DENOISE_SAMPLED_HALO_MAX
            }
        );
        let thresholds = BAND_NOISE
            .iter()
            .enumerate()
            .take(levels)
            .map(|(j, k)| {
                [
                    if j < 3 {
                        (LUMINANCE_THRESHOLD * (luminance / 100.0).powf(1.5) * k) as f32
                    } else {
                        0.0
                    },
                    (CHROMA_THRESHOLD * (colour / 100.0).powf(1.5) * k) as f32,
                ]
            })
            .collect();
        Self {
            scale,
            luminance,
            colour,
            luminance_detail: (luminance_detail / 100.0) as f32,
            colour_detail: (colour_detail / 100.0) as f32,
            thresholds,
            kernels,
            halo,
        }
    }
}
impl SpatialUnit for Denoise {
    fn halo(&self, _: Stage) -> u32 {
        self.halo
    }
    fn scratch_bytes(&self, region: Stage) -> u64 {
        filters::scratch_bytes(region, 13)
    }
    fn apply(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        _: Option<&Global>,
        scratch: &mut [f32],
        parallelism: Parallelism,
    ) -> Result<(), Error> {
        self.apply_cancellable(input, output, None, scratch, parallelism, &Cancel::never())
    }
    fn apply_cancellable(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        _: Option<&Global>,
        scratch: &mut [f32],
        parallelism: Parallelism,
        cancel: &Cancel,
    ) -> Result<(), Error> {
        cancel.check()?;
        let stage = input.stage();
        let out = output.region();
        if out.is_empty() {
            return Ok(());
        }
        let held = out.grown(self.halo, stage);
        let geometry = Geometry { stage, held };
        let len = held.pixels() as usize;
        let mut scratch = scratch;
        let lab = filters::take(&mut scratch, 3 * len)?;
        let coarse = filters::take(&mut scratch, 3 * len)?;
        let next = filters::take(&mut scratch, 3 * len)?;
        let temporary = filters::take(&mut scratch, len)?;
        let delta = filters::take(&mut scratch, 3 * len)?;
        filters::lab(input, lab, geometry, parallelism, cancel)?;
        coarse.copy_from_slice(lab);
        delta.fill(0.0);
        let mut valid = held;
        for (kernels, thresholds) in self.kernels.iter().zip(&self.thresholds) {
            cancel.check()?;
            let next_valid = valid.shrunk(kernels[0].radius.max(kernels[1].radius), stage);
            for c in 0..3 {
                filters::smooth(
                    &coarse[c * len..(c + 1) * len],
                    &mut next[c * len..(c + 1) * len],
                    temporary,
                    geometry,
                    next_valid,
                    kernels,
                    parallelism,
                    cancel,
                )?;
            }
            for c in 0..3 {
                filters::rows(
                    &mut coarse[c * len..(c + 1) * len],
                    geometry,
                    next_valid,
                    parallelism,
                    cancel,
                    |y, row| {
                        for (column, value) in row.iter_mut().enumerate() {
                            let x = next_valid.x0 + column as u32;
                            *value -= next[c * len + geometry.index(i64::from(x), i64::from(y))];
                        }
                    },
                )?;
            }
            for c in 0..3 {
                let kind = usize::from(c != 0);
                let threshold = thresholds[kind];
                if threshold == 0.0 {
                    continue;
                }
                let detail = if kind == 0 {
                    self.luminance_detail
                } else {
                    self.colour_detail
                };
                filters::rows(
                    &mut delta[c * len..(c + 1) * len],
                    geometry,
                    out,
                    parallelism,
                    cancel,
                    |y, row| {
                        for (column, value) in row.iter_mut().enumerate() {
                            let x = i64::from(out.x0) + column as i64;
                            let y = i64::from(y);
                            let index = geometry.index(x, y);
                            let mut energy = 0.0;
                            for dy in -1..=1 {
                                for dx in -1..=1 {
                                    let i = geometry.index(x + dx, y + dy);
                                    if kind == 0 {
                                        energy += coarse[i] * coarse[i];
                                    } else {
                                        energy += coarse[len + i] * coarse[len + i]
                                            + coarse[2 * len + i] * coarse[2 * len + i];
                                    }
                                }
                            }
                            energy /= 9.0;
                            let protection =
                                energy / (energy + (3.0 * threshold) * (3.0 * threshold));
                            let effective = threshold * (1.0 - detail * protection);
                            let squared = if kind == 0 {
                                coarse[index] * coarse[index]
                            } else {
                                coarse[len + index] * coarse[len + index]
                                    + coarse[2 * len + index] * coarse[2 * len + index]
                            };
                            if squared != 0.0 {
                                let factor = (1.0 - effective * effective / squared).max(0.0);
                                let d = coarse[c * len + index];
                                *value += d * factor - d;
                            }
                        }
                    },
                )?;
            }
            for c in 0..3 {
                filters::rows(
                    &mut coarse[c * len..(c + 1) * len],
                    geometry,
                    next_valid,
                    parallelism,
                    cancel,
                    |y, row| {
                        for (column, value) in row.iter_mut().enumerate() {
                            *value = next[c * len
                                + geometry
                                    .index(i64::from(next_valid.x0) + column as i64, i64::from(y))];
                        }
                    },
                )?;
            }
            valid = next_valid;
            #[cfg(test)]
            filters::finished_level(cancel);
        }
        output.for_rows(parallelism, |y, red, green, blue| {
            if cancel.is_cancelled() {
                return;
            }
            for column in 0..out.width as usize {
                let x = i64::from(out.x0) + column as i64;
                let y = i64::from(y);
                let i = geometry.index(x, y);
                let value = filters::reconstruct(
                    input.sample(x, y),
                    [lab[i], lab[len + i], lab[2 * len + i]],
                    [delta[i], delta[len + i], delta[2 * len + i]],
                );
                [red[column], green[column], blue[column]] = value;
            }
        });
        cancel.check()
    }
    fn is_finite(&self) -> bool {
        self.thresholds.iter().flatten().all(|v| v.is_finite())
            && self.luminance_detail.is_finite()
            && self.colour_detail.is_finite()
    }
    fn describe(&self) -> String {
        format!(
            "detail denoise(luminance={}, detail={}, colour={}, detail={}, scale={:?}, halo={})",
            self.luminance,
            self.luminance_detail,
            self.colour,
            self.colour_detail,
            self.scale,
            self.halo
        )
    }
}
