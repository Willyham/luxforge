use super::filters::{self, Geometry, Kernel};
use crate::{
    Cancel, Error,
    modules::{Global, Parallelism, Planes, PlanesMut, Region, SamplingScale, SpatialUnit, Stage},
    render::gpu::GpuSpatialUnit,
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
    /// Whether the blur's kernels are the guide's, as at Radius 1: the guide's smoothing, over the
    /// output grown by one, is then the blur as well, the same values at every pixel of the output.
    blur_is_guide: bool,
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
            blur_is_guide: kernels[0].same(&guide[0]) && kernels[1].same(&guide[1]),
            kernels,
            guide,
            halo,
        }
    }
}
impl Sharpen {
    /// The sharpened lightness at `(x, y)` from the planes of the input's lightness `l`, its blur
    /// and its guide: the residual against the blur cored, gated by the guide's central-difference
    /// gradient energy, scaled by the gain and soft-limited by `tanh` to the 3 x 3 extrema of `l`.
    pub(super) fn limited(
        &self,
        l: &[f32],
        blurred: &[f32],
        guide: &[f32],
        geometry: Geometry,
        x: i64,
        y: i64,
    ) -> f32 {
        let i = geometry.index(x, y);
        let residual = l[i] - blurred[i];
        let squared = residual * residual;
        let cored = if residual == 0.0 {
            0.0
        } else {
            residual * squared / (squared + self.theta_squared)
        };
        let dx = 0.5 * (geometry.sample(guide, x + 1, y) - geometry.sample(guide, x - 1, y));
        let dy = 0.5 * (geometry.sample(guide, x, y + 1) - geometry.sample(guide, x, y - 1));
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
        if limit == 0.0 {
            l[i]
        } else if proposed > hi {
            hi + limit * ((proposed - hi) / limit).tanh()
        } else if proposed < lo {
            lo + limit * ((proposed - lo) / limit).tanh()
        } else {
            proposed
        }
    }

    /// The gain, the coring threshold squared and the masking scale squared the unit holds.
    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn coefficients(&self) -> [f32; 3] {
        [self.gain, self.theta_squared, self.mask_squared]
    }

    /// The blur's and the guide's kernels the unit holds: what the frozen reference in
    /// `exactness.rs` runs.
    #[cfg(test)]
    pub(super) fn kernels(&self) -> (&[Kernel; 2], &[Kernel; 2]) {
        (&self.kernels, &self.guide)
    }

    /// The limiter over the rows of `out` from the planes of the input's lightness `l`, its blur
    /// and its guide, each held over `geometry.held`.
    pub(super) fn limiter<'a>(
        &'a self,
        l: &'a [f32],
        blurred: &'a [f32],
        guide: &'a [f32],
        geometry: Geometry,
        out: Region,
    ) -> Limiter<'a> {
        Limiter {
            unit: self,
            l,
            blurred,
            guide,
            geometry,
            out,
        }
    }

    /// The unit's last pass: each pixel of `output` reconstructed from its input, its Oklab `lab`
    /// (planar L, a and b over `geometry.held`) and the change of lightness the limiter gives it.
    /// A row's limited lightness waits in its red row until the row's pixels are reconstructed.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        lab: &[f32],
        blurred: &[f32],
        guide: &[f32],
        geometry: Geometry,
        parallelism: Parallelism,
        cancel: &Cancel,
    ) -> Result<(), Error> {
        let out = output.region();
        let len = geometry.held.pixels() as usize;
        let l = &lab[..len];
        let limiter = self.limiter(l, blurred, guide, geometry, out);
        output.for_rows(parallelism, |y, red, green, blue| {
            if cancel.is_cancelled() {
                return;
            }
            limiter.row(y, red);
            for column in 0..out.width as usize {
                let x = i64::from(out.x0) + column as i64;
                let y = i64::from(y);
                let i = geometry.index(x, y);
                let value = filters::reconstruct(
                    input.sample(x, y),
                    [l[i], lab[len + i], lab[2 * len + i]],
                    [red[column] - l[i], 0.0, 0.0],
                );
                [red[column], green[column], blue[column]] = value;
            }
        });
        cancel.check()
    }
}

/// Sharpening's limiter over the rows of one output rectangle.
#[derive(Clone, Copy)]
pub(super) struct Limiter<'a> {
    unit: &'a Sharpen,
    l: &'a [f32],
    blurred: &'a [f32],
    guide: &'a [f32],
    geometry: Geometry,
    out: Region,
}
impl Limiter<'_> {
    /// The limited lightness of stage row `y` over the output's columns, into `row`.
    pub(super) fn row(&self, y: u32, row: &mut [f32]) {
        for (column, value) in row.iter_mut().enumerate() {
            let x = i64::from(self.out.x0) + column as i64;
            *value = self.unit.limited(
                self.l,
                self.blurred,
                self.guide,
                self.geometry,
                x,
                i64::from(y),
            );
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
            guide,
            temporary,
            geometry,
            out.grown(1, stage),
            &self.guide,
            parallelism,
            cancel,
        )?;
        let guide = &*guide;
        let blurred = if self.blur_is_guide {
            guide
        } else {
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
            &*blurred
        };
        self.finish(
            input,
            output,
            lab,
            blurred,
            guide,
            geometry,
            parallelism,
            cancel,
        )
    }
    fn gpu(&self, _: Option<&Global>) -> Option<GpuSpatialUnit> {
        super::gpu::sharpen(
            &self.kernels,
            &self.guide,
            self.gain,
            self.theta_squared,
            self.mask_squared,
        )
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
