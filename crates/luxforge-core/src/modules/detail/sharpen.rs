use super::filters::{self, Geometry, Kernel};
use crate::{
    Cancel, Error,
    modules::{Global, Parallelism, Planes, PlanesMut, SamplingScale, SpatialUnit, Stage},
};

#[derive(Debug)]
pub(super) struct Sharpen {
    scale: SamplingScale,
    amount: f64,
    radius: f64,
    detail: f64,
    masking: f64,
    gain: f32,
    theta_squared: f32,
    mask_squared: f32,
    kernels: [Kernel; 2],
    guide: [Kernel; 2],
    halo: u32,
}
impl Sharpen {
    pub fn new(amount: f64, radius: f64, detail: f64, masking: f64, scale: SamplingScale) -> Self {
        let kernels = [
            Kernel::gaussian(radius * scale.x),
            Kernel::gaussian(radius * scale.y),
        ];
        let guide = [Kernel::gaussian(scale.x), Kernel::gaussian(scale.y)];
        let halo = (0..2)
            .map(|c| kernels[c].radius.max(guide[c].radius + 1))
            .max()
            .unwrap();
        debug_assert!(halo <= filters::SHARPEN_HALO_MAX);
        Self {
            scale,
            amount,
            radius,
            detail,
            masking,
            gain: (amount / 100.0) as f32,
            theta_squared: (0.01 * (1.0 - detail / 100.0).powi(2)).powi(2) as f32,
            mask_squared: (masking / 100.0 * 0.05).powi(2) as f32,
            kernels,
            guide,
            halo,
        }
    }
}
impl SpatialUnit for Sharpen {
    fn halo(&self, _: Stage) -> u32 {
        self.halo
    }
    fn scratch_bytes(&self, region: Stage) -> u64 {
        filters::scratch_bytes(region, 6)
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
        let temporary = filters::take(&mut scratch, len)?;
        let blurred = filters::take(&mut scratch, len)?;
        let guide = filters::take(&mut scratch, len)?;
        filters::lab(input, lab, geometry, parallelism, cancel)?;
        let l = &lab[..len];
        filters::smooth(
            l,
            blurred,
            temporary,
            geometry,
            out,
            &self.kernels,
            parallelism,
            cancel,
        )?;
        filters::smooth(
            l,
            guide,
            temporary,
            geometry,
            out.grown(1, stage),
            &self.guide,
            parallelism,
            cancel,
        )?;
        output.for_rows(parallelism, |y, red, green, blue| {
            if cancel.is_cancelled() {
                return;
            }
            for column in 0..out.width as usize {
                let x = i64::from(out.x0) + column as i64;
                let y = i64::from(y);
                let i = geometry.index(x, y);
                let residual = l[i] - blurred[i];
                let squared = residual * residual;
                let cored = if residual == 0.0 {
                    0.0
                } else {
                    residual * squared / (squared + self.theta_squared)
                };
                let dx =
                    0.5 * (geometry.sample(guide, x + 1, y) - geometry.sample(guide, x - 1, y));
                let dy =
                    0.5 * (geometry.sample(guide, x, y + 1) - geometry.sample(guide, x, y - 1));
                let energy = dx * dx + dy * dy;
                let mask = if self.mask_squared == 0.0 {
                    1.0
                } else {
                    energy / (energy + self.mask_squared)
                };
                let proposed = l[i] + self.gain * cored * mask;
                let (mut lo, mut hi) = (l[i], l[i]);
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let value = geometry.sample(l, x + dx, y + dy);
                        lo = lo.min(value);
                        hi = hi.max(value);
                    }
                }
                let limit = 0.04 * (hi - lo);
                let limited = if limit == 0.0 {
                    l[i]
                } else if proposed > hi {
                    hi + limit * ((proposed - hi) / limit).tanh()
                } else if proposed < lo {
                    lo + limit * ((proposed - lo) / limit).tanh()
                } else {
                    proposed
                };
                let value = filters::reconstruct(
                    input.sample(x, y),
                    [l[i], lab[len + i], lab[2 * len + i]],
                    [limited - l[i], 0.0, 0.0],
                );
                [red[column], green[column], blue[column]] = value;
            }
        });
        cancel.check()
    }
    fn is_finite(&self) -> bool {
        self.gain.is_finite() && self.theta_squared.is_finite() && self.mask_squared.is_finite()
    }
    fn describe(&self) -> String {
        format!(
            "detail sharpen(amount={}, radius={}, detail={}, masking={}, scale={:?}, halo={})",
            self.amount, self.radius, self.detail, self.masking, self.scale, self.halo
        )
    }
}
