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
    /// and its guide, each read clamped to the stage and checked against the held rectangle through
    /// [`Geometry::index`] and [`Geometry::sample`]: [`Self::limit`] of the pixel's values. The
    /// limiter's path for the pixels within one of a stage edge, whose windows clamp.
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
        let at = |plane: &[f32], dx: i64, dy: i64| geometry.sample(plane, x + dx, y + dy);
        self.limit(
            l[i],
            blurred[i],
            [
                at(guide, 1, 0),
                at(guide, -1, 0),
                at(guide, 0, 1),
                at(guide, 0, -1),
            ],
            [-1, 0, 1].map(|dy| [-1, 0, 1].map(|dx| at(l, dx, dy))),
        )
    }

    /// The sharpened lightness of a pixel whose lightness is `centre` and blur `blurred`: the
    /// residual against the blur cored, gated by the gradient energy of the guide's central
    /// differences across its `[right, left, below, above]` neighbours, scaled by the gain and
    /// soft-limited by `tanh` to the extrema of `window`, the 3 x 3 of lightness around the pixel
    /// by rows from the top left, taken in that order.
    #[inline(always)]
    fn limit(&self, centre: f32, blurred: f32, guide: [f32; 4], window: [[f32; 3]; 3]) -> f32 {
        let residual = centre - blurred;
        let squared = residual * residual;
        let cored = if residual == 0.0 {
            0.0
        } else {
            residual * squared / (squared + self.theta_squared)
        };
        let [right, left, below, above] = guide;
        let dx = 0.5 * (right - left);
        let dy = 0.5 * (below - above);
        let energy = dx * dx + dy * dy;
        let mask = if self.mask_squared == 0.0 {
            1.0
        } else {
            energy / (energy + self.mask_squared)
        };
        let proposed = centre + self.gain * cored * mask;
        let (mut lo, mut hi) = (centre, centre);
        for row in window {
            for value in row {
                lo = lo.min(value);
                hi = hi.max(value);
            }
        }
        let limit = 0.04 * (hi - lo);
        if limit == 0.0 {
            centre
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
    /// and its guide, each held over `geometry.held`. The pixels of `out` one or more in from every
    /// stage edge, whose 3 x 3 windows no clamp moves, are its interior; their windows are checked
    /// against the held rectangle here, once.
    pub(super) fn limiter<'a>(
        &'a self,
        l: &'a [f32],
        blurred: &'a [f32],
        guide: &'a [f32],
        geometry: Geometry,
        out: Region,
    ) -> Limiter<'a> {
        let stage = geometry.stage;
        let (x0, x1) = (out.x0.max(1), out.x1().min(stage.width.saturating_sub(1)));
        let (y0, y1) = (out.y0.max(1), out.y1().min(stage.height.saturating_sub(1)));
        let interior = if x0 < x1 && y0 < y1 {
            let interior = Region {
                x0,
                y0,
                width: x1 - x0,
                height: y1 - y0,
            };
            // Growing the interior by one moves no edge onto the stage's.
            filters::guard(geometry.held, interior.grown(1, stage));
            interior
        } else {
            Region::EMPTY
        };
        Limiter {
            unit: self,
            l,
            blurred,
            guide,
            geometry,
            out,
            interior,
        }
    }

    /// The unit's last pass: each pixel of `output` reconstructed from its input, its Oklab `lab`
    /// (planar L, a and b over `geometry.held`) and the change of lightness the limiter gives it.
    /// A row's limited lightness waits in its red row until the row's pixels are reconstructed.
    /// A pixel's reconstruction reads its own place alone, which lies inside the stage, so it reads
    /// row slices of the planes and the input with no clamp, the columns checked once.
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
        if out.is_empty() {
            return cancel.check();
        }
        filters::guard(geometry.held, out);
        let first = (out.x0 - geometry.held.x0) as usize;
        let columns = filters::input_columns(input, out.x0, out.x1());
        let len = geometry.held.pixels() as usize;
        let (l, ab) = lab[..3 * len].split_at(len);
        let (a, b) = ab.split_at(len);
        let limiter = self.limiter(l, blurred, guide, geometry, out);
        output.for_rows(parallelism, |y, red, green, blue| {
            if cancel.is_cancelled() {
                return;
            }
            limiter.row(y, red);
            let (n, y) = (red.len(), i64::from(y));
            let l = geometry.span(l, y, first, n);
            let (a, b) = (geometry.span(a, y, first, n), geometry.span(b, y, first, n));
            let rgb = filters::input_row(input, y, &columns, n);
            let (green, blue) = (&mut green[..n], &mut blue[..n]);
            for column in 0..n {
                let value = filters::reconstruct(
                    [rgb[0][column], rgb[1][column], rgb[2][column]],
                    [l[column], a[column], b[column]],
                    [red[column] - l[column], 0.0, 0.0],
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
    /// The pixels of `out` whose 3 x 3 window lies inside the stage, or no pixel.
    interior: Region,
}
impl Limiter<'_> {
    /// The limited lightness of stage row `y` over the output's columns, into `row`. A row of the
    /// interior reads its columns there from row slices: rows `y - 1`, `y` and `y + 1` of the
    /// lightness and the guide and row `y` of the blur, each tap a slice as long as the run, with
    /// no clamp, index assertion or per-tap bounds check. The columns and rows within one of a
    /// stage edge go through the clamped and checked [`Sharpen::limited`]. Both compute each value
    /// with [`Sharpen::limit`].
    pub(super) fn row(&self, y: u32, row: &mut [f32]) {
        let Self {
            unit,
            l,
            blurred,
            guide,
            geometry,
            out,
            interior,
        } = *self;
        let (x0, x1) = (out.x0, out.x1());
        let (start, end) = if (interior.y0..interior.y1()).contains(&y) {
            (interior.x0, interior.x1())
        } else {
            (x1, x1)
        };
        let y = i64::from(y);
        for x in (x0..start).chain(end..x1) {
            row[(x - x0) as usize] = unit.limited(l, blurred, guide, geometry, i64::from(x), y);
        }
        if start < end {
            // The run's left, centre and right taps of rows `y - 1`, `y` and `y + 1` of the
            // lightness, and of the guide where the differences read it, each a slice `n` long
            // from the held column left of, at or right of the run's first, cut once a row.
            let n = (end - start) as usize;
            let first = (start - 1 - geometry.held.x0) as usize;
            let above = filters::taps(geometry.span(l, y - 1, first, n + 2), n);
            let middle = filters::taps(geometry.span(l, y, first, n + 2), n);
            let below = filters::taps(geometry.span(l, y + 1, first, n + 2), n);
            let across = filters::taps(geometry.span(guide, y, first, n + 2), n);
            let up = geometry.span(guide, y - 1, first + 1, n);
            let down = geometry.span(guide, y + 1, first + 1, n);
            let blurred = geometry.span(blurred, y, first + 1, n);
            let row = &mut row[(start - x0) as usize..][..n];
            for j in 0..n {
                row[j] = unit.limit(
                    middle[1][j],
                    blurred[j],
                    [across[2][j], across[0][j], down[j], up[j]],
                    [
                        [above[0][j], above[1][j], above[2][j]],
                        [middle[0][j], middle[1][j], middle[2][j]],
                        [below[0][j], below[1][j], below[2][j]],
                    ],
                );
            }
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
