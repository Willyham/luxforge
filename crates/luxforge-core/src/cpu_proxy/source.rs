//! Building a proxy source: the prepared source reduced to the proxy stage by the view's area
//! average (`crate::proxy`), a JPEG's through the reference's own reduction and a RAW's planes
//! through their view.

use crate::{
    Cancel, Error, LinearImage, PreviewSource, ProxyPlan,
    proxy::{BAND_BYTES, BoxDownscale, check_plan, downscale_jpeg, float_values},
};
use rayon::prelude::*;

impl PreviewSource {
    /// This source's pixels under the evaluation settings of `job`. A cached proxy is keyed by its
    /// pixels alone — a RAW source's developed planes and view — while its `LinearSettings`
    /// (the exposure a RAW development layer asks for) belong to the recipe being rendered, so a
    /// cache hit takes the pixels from the cache and the settings from the job that is rendering.
    /// A JPEG carries no settings and is returned as it is.
    pub(crate) fn with_settings_of(&self, job: &PreviewSource) -> PreviewSource {
        match (self, job) {
            (Self::Raw { image, .. }, Self::Raw { settings, .. }) => Self::Raw {
                image: image.clone(),
                settings: *settings,
            },
            _ => self.clone(),
        }
    }

    /// Downscale this source to the plan's dimensions with a separable area average, keeping only
    /// the plan's window when it has one. A windowed proxy's pixels are the whole downscale's
    /// pixels in that window, bit for bit: each is the same weights over the same source samples in
    /// the same order, and only the source rows and columns the window covers are read.
    ///
    /// This is frame work: it runs on the caller's thread and puts its bands of output rows on the
    /// shared Rayon pool from the proxy pass's threshold of source pixels read, each worker holding
    /// one band's intermediate of about 1 MiB. Never call it on the catalog owner thread.
    pub fn proxy(&self, plan: ProxyPlan) -> Result<PreviewSource, Error> {
        self.proxy_cancellable(plan, &Cancel::never())
    }

    /// The same bounded proxy build, with a checkpoint in every horizontal and vertical row.
    /// Interactive jobs pass their `abandoned` token here, including cache misses.
    pub(crate) fn proxy_cancellable(
        &self,
        plan: ProxyPlan,
        cancel: &Cancel,
    ) -> Result<PreviewSource, Error> {
        self.proxy_in_bands(plan, cancel, BAND_BYTES)
    }

    /// The proxy build with bands of about `band_bytes` of intermediate each. The pixels do not
    /// depend on the band size ([`BoxDownscale`]); the tests pass small bands to prove it.
    pub(super) fn proxy_in_bands(
        &self,
        plan: ProxyPlan,
        cancel: &Cancel,
        band_bytes: usize,
    ) -> Result<PreviewSource, Error> {
        cancel.check()?;
        let (source_width, source_height) = self.dimensions();
        check_plan(plan, source_width, source_height)?;
        match self {
            Self::Jpeg(image) => Ok(PreviewSource::Jpeg(downscale_jpeg(
                image, plan, cancel, band_bytes,
            )?)),
            Self::Raw { image, settings } => Ok(PreviewSource::Raw {
                image: downscale_linear(image, plan, cancel, band_bytes)?,
                settings: *settings,
            }),
        }
    }
}

/// One band of output rows of the three planes, as the zipped chunk iterators hand it over: the
/// band index and its red, green and blue slices. The serial and parallel iterators yield the same
/// shape, so one closure serves both.
type PlanarBand<'a> = (usize, ((&'a mut [f32], &'a mut [f32]), &'a mut [f32]));

/// The area average of a prepared RAW source's planes, read through its view.
///
/// The result is a smaller [`LinearImage`] with the same fingerprint and an identity view: the
/// crop and orientation of the input view are resolved by the averaging itself, so the proxy is
/// upright content with nothing left to map. Values stay unbounded linear f32, so no clipping or
/// transfer function is introduced anywhere on this path.
fn downscale_linear(
    image: &LinearImage,
    plan: ProxyPlan,
    cancel: &Cancel,
    band_bytes: usize,
) -> Result<LinearImage, Error> {
    let (width, height) = plan.source_dimensions();
    let reader = image.reader();
    let (source_width, source_height) = reader.dimensions();
    let plane_values = float_values(width, height, "proxy linear source")?;
    let plane_len = plane_values / 3;
    // Inside the view by construction: the coverage never leaves the source.
    let read_row = |y: u32| {
        let reader = &reader;
        move |x: u32| reader.pixel(x, y).unwrap_or([0.0; 3])
    };
    let downscale = BoxDownscale::new(source_width, source_height, plan, band_bytes)?;
    let mut planes = vec![0f32; plane_values];
    {
        let (red, rest) = planes.split_at_mut(plane_len);
        let (green, blue) = rest.split_at_mut(plane_len);
        let row = width as usize;
        let pass = |rows: &mut Vec<f32>, (band, ((red, green), blue)): PlanarBand<'_>| {
            downscale.band(band, rows, cancel, &read_row, |y, x, sum| {
                red[y * row + x] = sum[0] as f32;
                green[y * row + x] = sum[1] as f32;
                blue[y * row + x] = sum[2] as f32;
            })
        };
        let band_len = downscale.band_rows * row;
        if downscale.parallel {
            red.par_chunks_mut(band_len)
                .zip(green.par_chunks_mut(band_len))
                .zip(blue.par_chunks_mut(band_len))
                .enumerate()
                .try_for_each_init(Vec::new, pass)?;
        } else {
            let mut rows = Vec::new();
            red.chunks_mut(band_len)
                .zip(green.chunks_mut(band_len))
                .zip(blue.chunks_mut(band_len))
                .enumerate()
                .try_for_each(|band| pass(&mut rows, band))?;
        }
    }

    // Every value is a weighted mean, with weights summing to one, of the finite values of a source
    // that was scanned or validated when it was built, so none can be non-finite and the
    // constructor's scan of the whole proxy is skipped.
    debug_assert!(
        planes.iter().all(|value| value.is_finite()),
        "a proxy of finite planes is finite"
    );
    Ok(
        LinearImage::from_validated_planes(width, height, planes, image.fingerprint().to_owned())?
            .with_development(proxy_development(image, plan)),
    )
}

/// The development a linear proxy of `image` at `plan` is: derived from `image`'s development and
/// view and the plan's stage and window, which decide its pixels exactly, so a proxy is named the
/// same whenever it is built and anything keyed by it finds it again. The top bit keeps it apart
/// from every adopted development, which counts up from one.
fn proxy_development(image: &LinearImage, plan: ProxyPlan) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    (image.development(), image.view(), plan.width, plan.height).hash(&mut hasher);
    plan.window
        .map(|window| (window.x, window.y, window.width, window.height))
        .hash(&mut hasher);
    hasher.finish() | 1 << 63
}
