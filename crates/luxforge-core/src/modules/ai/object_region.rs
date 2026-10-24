//! The removal region of an object artifact (`docs/design/ai-editing.md`, "The removal region"):
//! the prototype's region over a picked object's 8-bit alpha raster in place of the decoder's
//! prior, frozen in the `f64` reference `luxforge_reference::removal::object_region` and equal to
//! it exactly:
//!
//! ```text
//! texel    = max(W / width, H / height)                    -- one raster texel, in stage pixels
//! cap      = max(round(15 · sqrt(W · H / 1.5e6)), 1)       -- dilation and feather together
//! peak     = max over stage pixels p of alpha(p's centre)  -- the model kind's bilinear read
//! core     = { stage pixel p : alpha(p's centre) >= peak / 2 }
//! feather  = min(max(round(texel), 2), cap)                -- the soft outer edge, one texel
//! r        = clamp(round(texel / 2 + 0.04 · min(core width, core height)), 2, max(cap − feather, 2))
//! d(p)     = the Euclidean distance from p to the nearest core pixel, centre to centre
//! c(p)     = 1 where d <= r, else smooth(clamp((r + feather − d) / feather, 0, 1))
//! ```
//!
//! The core is the object at full coverage, as the decoder's core is: a removal needs the object's
//! extent, not its probability. An opaque object peaks at 1, so its core is where it reaches one
//! half; a small object the refinement left soft (a kayak a few texels long peaking at 130 of 255)
//! keeps the pixels where it reaches half its own peak rather than none. The artifact is never finer
//! than the stage it was selected on (the guide is the stage reduced), so a pixel centre lies within
//! half a texel of every texel and reads it at a weight of at least a quarter: any raster with a
//! non-zero texel, which the owner admitted, has a core. The dilation is a grayscale disc dilation
//! of that binary core, and the soft edge lies outside it, so nothing of the object is ever under
//! partial coverage.
//!
//! **Only over the raster's window.** The core is evaluated over the stage pixels whose bilinear
//! read reaches a non-zero texel ([`core_window`]), and the region over the core's bounds grown by
//! `r + feather`; nothing is allocated outside them. Distances are exact: the squared distance to
//! the core is an integer, found by a column pass and Felzenszwalb and Huttenlocher's lower
//! envelope per row in integer arithmetic, so the square root and the profile see exactly the
//! reference's numbers. `O(window)` time and memory.
use super::fill::Coverage;
use crate::{
    Error,
    mask::{nonzero_rect, raster_support, sample_alpha},
    modules::{Region, Stage},
};

/// The rows a pass works through between cancellation checks.
const ROWS_PER_CHECK: u32 = 64;

/// What a picked object's raster is, before its bytes are needed: its size and the rectangle of its
/// non-zero texels, `[i0, j0, i1, j1]` inclusive, `None` when every texel is zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObjectRaster {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) nonzero: Option<[u32; 4]>,
}

impl ObjectRaster {
    /// The raster of `codes`, `width × height` row-major: one scan.
    pub(crate) fn of(codes: &[u8], width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            nonzero: nonzero_rect(codes, width),
        }
    }

    /// `kx` and `ky` of the raster over `stage`: mask space to texel coordinates, as the model
    /// kind reads it.
    fn scale(self, stage: Stage) -> (f64, f64) {
        let kx = f64::from(self.width) * f64::from(stage.height) / f64::from(stage.width);
        (kx, f64::from(self.height))
    }

    /// One texel of the raster stretched over `stage`, in stage pixels, on the coarser axis.
    fn texel(self, stage: Stage) -> f64 {
        (f64::from(stage.width) / f64::from(self.width))
            .max(f64::from(stage.height) / f64::from(self.height))
    }
}

/// The most a region grows past its core, dilation and feather together: the 15 px precedent for
/// 1.5 MP, scaled with the frame's linear size, at least 1 px.
fn reach_cap(stage: Stage) -> u32 {
    (15.0 * (f64::from(stage.width) * f64::from(stage.height) / 1.5e6).sqrt())
        .round()
        .max(1.0) as u32
}

/// The soft outer edge: one texel, at least 2 px, within the cap.
fn feather(texel: f64, cap: u32) -> u32 {
    texel.round().max(2.0).min(f64::from(cap)) as u32
}

/// The dilation of a core whose shorter side is `shorter` px: half a texel, the raster's positional
/// uncertainty, plus 4% of the core for anti-aliasing and colour bleeding into its surroundings; at
/// least 2 px and at most the cap less the feather (but never under 2 px). Monotone in `shorter`.
fn dilation(texel: f64, shorter: u32, cap: u32, feather: u32) -> u32 {
    let ceiling = cap.saturating_sub(feather).max(2);
    ((0.5 * texel + 0.04 * f64::from(shorter)).round() as u32).clamp(2, ceiling)
}

/// The stage pixels whose bilinear read can reach a non-zero texel, as the model kind bounds its
/// coverage ([`raster_support`]); `None` for an all-zero raster. Every pixel outside it has zero
/// alpha and is no core.
fn core_window(raster: ObjectRaster, stage: Stage) -> Option<Region> {
    let (kx, ky) = raster.scale(stage);
    let window = raster_support(raster.nonzero, kx, ky, stage);
    (!window.is_empty()).then_some(window)
}

/// Every stage pixel whose coverage may be above zero, from the raster's size and non-zero
/// rectangle alone: what the owner admits a request by before the bytes are read. The core lies
/// inside [`core_window`], so its shorter side is at most the window's and the dilation at most the
/// window's ([`dilation`] is monotone).
pub(crate) fn support(raster: ObjectRaster, stage: Stage) -> Region {
    let Some(window) = core_window(raster, stage) else {
        return Region::EMPTY;
    };
    let texel = raster.texel(stage);
    let cap = reach_cap(stage);
    let feather = feather(texel, cap);
    let r = dilation(texel, window.width.min(window.height), cap, feather);
    window.grown(r + feather, stage)
}

/// The frozen `smooth` of the mask brush's profile.
fn smooth(s: f64) -> f64 {
    s * s * (3.0 - 2.0 * s)
}

/// One boundary of the lower envelope: a rational `num / den` with `den > 0`, or an end.
#[derive(Clone, Copy)]
enum Boundary {
    Below,
    At(i64, i64),
    Above,
}

impl Boundary {
    /// Whether `num / den <= self`.
    fn at_most(num: i64, den: i64, boundary: Self) -> bool {
        match boundary {
            Self::Below => false,
            Self::Above => true,
            Self::At(n, d) => num * d <= n * den,
        }
    }

    /// Whether `self < x`.
    fn before(self, x: i64) -> bool {
        match self {
            Self::Below => true,
            Self::Above => false,
            Self::At(n, d) => n < x * d,
        }
    }
}

/// The squared distance along one row to the nearest of `sites`, each a column with the squared
/// vertical distance to its nearest core pixel, into `out`: the lower envelope of the parabolas
/// `(x − q)² + f(q)`, exactly, in integers.
fn envelope(sites: &[(i64, i64)], out: &mut [i64], v: &mut Vec<usize>, z: &mut Vec<Boundary>) {
    v.clear();
    z.clear();
    if sites.is_empty() {
        out.fill(i64::MAX);
        return;
    }
    v.push(0);
    z.push(Boundary::Below);
    z.push(Boundary::Above);
    for (index, &(q, fq)) in sites.iter().enumerate().skip(1) {
        loop {
            let (p, fp) = sites[*v.last().expect("one site at least")];
            let num = (fq + q * q) - (fp + p * p);
            let den = 2 * (q - p);
            let k = v.len() - 1;
            // The first site's left boundary is `Below`, so the envelope never empties.
            if Boundary::at_most(num, den, z[k]) {
                v.pop();
                // `z` keeps a stale last boundary, which the push below replaces.
                z.pop();
                continue;
            }
            z.pop();
            z.push(Boundary::At(num, den));
            z.push(Boundary::Above);
            v.push(index);
            break;
        }
    }
    let mut k = 0;
    for (x, slot) in out.iter_mut().enumerate() {
        let x = x as i64;
        while z[k + 1].before(x) {
            k += 1;
        }
        let (q, fq) = sites[v[k]];
        *slot = (x - q) * (x - q) + fq;
    }
}

/// Mark in `core` (row-major over `window`) the stage pixels whose bilinear alpha is at least
/// `threshold`: their bounds, `None` for none, and the largest alpha met.
fn core_at(
    codes: &[u8],
    raster: ObjectRaster,
    stage: Stage,
    window: Region,
    threshold: f64,
    core: &mut [bool],
    checkpoint: &dyn Fn() -> Result<(), Error>,
) -> Result<(Option<Region>, f64), Error> {
    let h = f64::from(stage.height);
    let (kx, ky) = raster.scale(stage);
    let ww = window.width as usize;
    let mut peak = 0.0_f64;
    let (mut cx0, mut cy0, mut cx1, mut cy1) = (u32::MAX, u32::MAX, 0, 0);
    for row in 0..window.height {
        if row.is_multiple_of(ROWS_PER_CHECK) {
            checkpoint()?;
        }
        let py = window.y0 + row;
        let v = (f64::from(py) + 0.5) / h;
        for column in 0..window.width {
            let px = window.x0 + column;
            let u = (f64::from(px) + 0.5) / h;
            let alpha = sample_alpha(codes, raster.width, raster.height, kx, ky, u, v);
            peak = peak.max(alpha);
            if alpha >= threshold {
                core[row as usize * ww + column as usize] = true;
                cx0 = cx0.min(px);
                cy0 = cy0.min(py);
                cx1 = cx1.max(px);
                cy1 = cy1.max(py);
            }
        }
    }
    let bounds = (cx0 != u32::MAX).then(|| Region {
        x0: cx0,
        y0: cy0,
        width: cx1 - cx0 + 1,
        height: cy1 - cy0 + 1,
    });
    Ok((bounds, peak))
}

/// The removal region of a `width × height` raster of `codes` over `stage`, over the core's bounds
/// grown by the dilation and feather and clamped to the stage, untightened; `None` when the raster
/// is zero at every stage pixel. `checkpoint` is called between bands of rows. A raster with no
/// opaque pixel is read twice: once to find its peak, once at half of it.
pub(crate) fn region(
    codes: &[u8],
    width: u32,
    height: u32,
    stage: Stage,
    checkpoint: &dyn Fn() -> Result<(), Error>,
) -> Result<Option<Coverage>, Error> {
    let raster = ObjectRaster::of(codes, width, height);
    let Some(window) = core_window(raster, stage) else {
        return Ok(None);
    };
    // The core over the window, as the model kind reads the raster: at one half, which is half
    // the peak when some pixel is opaque, and otherwise again at half the peak found.
    let (ww, wh) = (window.width as usize, window.height as usize);
    let mut core = vec![false; ww * wh];
    let (bounds, peak) = core_at(codes, raster, stage, window, 0.5, &mut core, checkpoint)?;
    let bounds = if peak < 1.0 && peak > 0.0 {
        core.fill(false);
        core_at(
            codes,
            raster,
            stage,
            window,
            peak / 2.0,
            &mut core,
            checkpoint,
        )?
        .0
    } else {
        bounds
    };
    let Some(bounds) = bounds else {
        return Ok(None);
    };
    let texel = raster.texel(stage);
    let cap = reach_cap(stage);
    let feather = feather(texel, cap);
    let r = dilation(texel, bounds.width.min(bounds.height), cap, feather);
    let out = bounds.grown(r + feather, stage);
    let (ow, oh) = (out.width as usize, out.height as usize);
    let is_core = |x: u32, y: u32| {
        window.contains(x, y) && core[(y - window.y0) as usize * ww + (x - window.x0) as usize]
    };

    // The vertical distance from each pixel to its column's nearest core pixel, down then up;
    // `u32::MAX` where the column holds none.
    let mut vertical = vec![u32::MAX; ow * oh];
    let mut last = vec![None::<u32>; ow];
    for row in 0..out.height {
        if row.is_multiple_of(ROWS_PER_CHECK) {
            checkpoint()?;
        }
        let y = out.y0 + row;
        for (column, seen) in last.iter_mut().enumerate() {
            let x = out.x0 + column as u32;
            if is_core(x, y) {
                *seen = Some(y);
            }
            if let Some(found) = *seen {
                vertical[row as usize * ow + column] = y - found;
            }
        }
    }
    last.fill(None);
    for row in (0..out.height).rev() {
        if row.is_multiple_of(ROWS_PER_CHECK) {
            checkpoint()?;
        }
        let y = out.y0 + row;
        for (column, seen) in last.iter_mut().enumerate() {
            let x = out.x0 + column as u32;
            if is_core(x, y) {
                *seen = Some(y);
            }
            if let Some(found) = *seen {
                let slot = &mut vertical[row as usize * ow + column];
                *slot = (*slot).min(found - y);
            }
        }
    }

    // Each row's squared distances, then the profile.
    let reach = f64::from(r + feather);
    let (radius, band) = (f64::from(r), f64::from(feather));
    let mut values = vec![0.0_f32; ow * oh];
    let mut sites = Vec::with_capacity(ow);
    let mut squared = vec![0_i64; ow];
    let (mut v, mut z) = (Vec::with_capacity(ow), Vec::with_capacity(ow + 1));
    for row in 0..oh {
        if (row as u32).is_multiple_of(ROWS_PER_CHECK) {
            checkpoint()?;
        }
        sites.clear();
        let line = &vertical[row * ow..(row + 1) * ow];
        sites.extend(
            line.iter()
                .enumerate()
                .filter(|(_, d)| **d != u32::MAX)
                .map(|(x, &d)| (x as i64, i64::from(d) * i64::from(d))),
        );
        envelope(&sites, &mut squared, &mut v, &mut z);
        for (value, &d2) in values[row * ow..(row + 1) * ow].iter_mut().zip(&squared) {
            if d2 == i64::MAX {
                continue;
            }
            let d = (d2 as f64).sqrt();
            *value = if d <= radius {
                1.0
            } else {
                smooth(((reach - d) / band).clamp(0.0, 1.0)) as f32
            };
        }
    }
    Coverage::new(out, values).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_reference::mask as reference;
    use std::sync::Arc;

    /// A `width × height` raster holding an anti-aliased ellipse centred at texel `(cx, cy)` with
    /// semi-axes `(rx, ry)` texels: opaque inside, falling to zero over about a texel and a half.
    fn ellipse(width: u32, height: u32, (cx, cy): (f64, f64), (rx, ry): (f64, f64)) -> Vec<u8> {
        let mut codes = Vec::with_capacity((width * height) as usize);
        for j in 0..height {
            for i in 0..width {
                let (dx, dy) = ((f64::from(i) - cx) / rx, (f64::from(j) - cy) / ry);
                let edge = (1.0 - (dx * dx + dy * dy).sqrt()) * rx.min(ry) / 1.5 + 0.5;
                codes.push((255.0 * edge.clamp(0.0, 1.0)).round() as u8);
            }
        }
        codes
    }

    /// Production's region of `codes` against the reference at every stage pixel, exactly; then
    /// that what production allocated is the reference's non-zero box and one ring of zeros past
    /// it, clamped to the frame, within what the owner admitted by.
    fn matches_reference(codes: Vec<u8>, width: u32, height: u32, stage: Stage) -> Region {
        let coverage = region(&codes, width, height, stage, &|| Ok(()))
            .unwrap()
            .expect("a core");
        let model = reference::Model {
            width,
            height,
            alpha: Arc::from(codes.clone()),
            threshold: 50.0,
            feather: 100.0,
        };
        let expected = luxforge_reference::removal::object_region(
            &model,
            &reference::Stage::new(stage.width, stage.height),
        );
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..stage.height {
            for x in 0..stage.width {
                let want = expected[(y * stage.width + x) as usize];
                assert_eq!(coverage.at(x, y), want as f32, "pixel ({x}, {y})");
                if want > 0.0 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        let nonzero = Region {
            x0,
            y0,
            width: x1 - x0 + 1,
            height: y1 - y0 + 1,
        };
        let bounds = coverage.bounds();
        assert_eq!(bounds, nonzero.grown(1, stage));
        let support = support(ObjectRaster::of(&codes, width, height), stage);
        assert!(
            support.x0 <= bounds.x0
                && support.y0 <= bounds.y0
                && support.x1() >= bounds.x1()
                && support.y1() >= bounds.y1(),
            "{support:?} does not hold {bounds:?}"
        );
        bounds
    }

    fn stage(width: u32, height: u32) -> Stage {
        Stage { width, height }
    }

    #[test]
    fn a_small_object_matches_its_reference_and_allocates_only_around_itself() {
        // Two pixels a texel: the dilation is its floor of 2 px and the feather 2 px.
        let s = stage(1200, 800);
        let bounds = matches_reference(ellipse(600, 400, (300.0, 150.0), (4.0, 3.0)), 600, 400, s);
        assert!(bounds.pixels() < 40 * 40, "{bounds:?}");
    }

    #[test]
    fn a_large_object_matches_its_reference() {
        // A core about 240 × 180 px: the dilation is half a texel plus 4% of 180, 8 px, and the
        // feather 2 px: the core grown by 10 px each side.
        let s = stage(1200, 800);
        let bounds =
            matches_reference(ellipse(600, 400, (250.0, 200.0), (60.0, 45.0)), 600, 400, s);
        assert_eq!((bounds.width, bounds.height), (260, 200), "{bounds:?}");
    }

    #[test]
    fn an_object_at_the_frame_edge_matches_its_reference_clamped_to_the_frame() {
        // A raster of another aspect, stretched over the frame: four pixels a texel across, 3.2
        // down. The object runs off the left and bottom edges.
        let s = stage(1200, 800);
        let bounds = matches_reference(ellipse(300, 250, (2.0, 240.0), (30.0, 40.0)), 300, 250, s);
        assert_eq!((bounds.x0, bounds.y1()), (0, 800), "{bounds:?}");
    }

    #[test]
    fn an_empty_raster_has_no_region_and_no_support() {
        let s = stage(64, 64);
        let empty = vec![0_u8; 16 * 16];
        assert!(region(&empty, 16, 16, s, &|| Ok(())).unwrap().is_none());
        assert!(support(ObjectRaster::of(&empty, 16, 16), s).is_empty());
    }

    #[test]
    fn a_soft_small_object_keeps_a_core_at_half_its_peak_within_what_was_admitted() {
        // A kayak a few texels long whose refined alpha peaks at 130 of 255: one half is reached
        // at no pixel centre between texels, so the core is where it reaches half its peak.
        let s = stage(1200, 800);
        let codes: Vec<u8> = ellipse(600, 400, (300.0, 200.0), (3.0, 1.2))
            .into_iter()
            .map(|code| (u32::from(code) * 130 / 255) as u8)
            .collect();
        assert_eq!(codes.iter().max(), Some(&130));
        let raster = ObjectRaster::of(&codes, 600, 400);
        let (kx, ky) = raster.scale(s);
        let peak = (0..s.height)
            .flat_map(|y| (0..s.width).map(move |x| (x, y)))
            .map(|(x, y)| {
                let (u, v) = ((f64::from(x) + 0.5) / 800.0, (f64::from(y) + 0.5) / 800.0);
                sample_alpha(&codes, 600, 400, kx, ky, u, v)
            })
            .fold(0.0, f64::max);
        assert!(peak < 0.5, "{peak}");
        assert!(!support(raster, s).is_empty(), "the owner admits it");
        let bounds = matches_reference(codes, 600, 400, s);
        assert!(bounds.pixels() < 40 * 40, "{bounds:?}");
    }

    #[test]
    fn a_cancelled_region_stops_at_its_first_check() {
        let codes = ellipse(64, 64, (32.0, 32.0), (10.0, 10.0));
        let cancelled = region(&codes, 64, 64, stage(256, 256), &|| {
            Err(Error::cancelled("stop"))
        });
        assert!(cancelled.is_err());
    }
}
