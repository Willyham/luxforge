//! A windowed proxy: the proxy of a cropped stack holds only the part of its stage the output reads.
//!
//! The proxy phase fits the recipe's *output* stage into the display bounds, so a tight crop raises
//! the proxy scale towards one and a whole-stage proxy towards the source's own size. Nothing
//! outside what the crop reads ever reaches the output, though, so a proxy compiled against the
//! whole proxy stage — the stage the proxy scale defines, which is what every normalized payload,
//! mask and spatial radius is resolved against — can be evaluated over a window of that stage
//! instead: the rectangle the output reads, walked back through every segment, plus the margins
//! each boundary needs.
//!
//! [`WindowPlan::of`] is that walk, over a compilation against the whole proxy stage, in
//! `O(segments)` with no pixel read. [`WindowPlan::apply`] rewrites the same compilation so that it
//! reads a source of the window's dimensions and keeps only each segment's window of its output:
//!
//! - **The source.** The first segment's composed geometry is preceded by the translation that
//!   places the window at its origin in the proxy stage, so every output pixel reads the source
//!   pixel it read before, now at the window's coordinates.
//! - **A segment whose output a boundary reads** keeps only the rectangle that boundary reads: an
//!   exact crop to it is appended to its operations and to its composed geometry. A masked colour
//!   operation maps its frame coordinate back through the exact steps after it, which now include
//!   that crop, so its mask is read at the same stage pixel as before. Positional colour and finish
//!   units receive the kept rectangle's whole-stage origin exactly once; point replacement remains
//!   a named fallback.
//! - **A resample** reads the previous segment's window: its continuous input coordinate is
//!   translated by the window's integer origin after the resample's own arithmetic, which is exact in
//!   `f64` (an integer subtracted from a non-negative coordinate below 2^52 is representable), so
//!   every tap is the same pixel with the same weight. The window is [`crate::modules::Resample::reads`]:
//!   every tap the output reads plus [`super::geometry::TAP_MARGIN`] pixels, clamped to the stage edge where
//!   a tap is clamped to it.
//! - **A spatial operation** runs over the previous segment's window as its own stage: the rectangle
//!   the next segment reads, grown by the operation's summed halo and clamped to the stage, with its
//!   origin moved down to the grid of the tiles the operation runs in
//!   ([`Tiling::Halo`], 512 or 1024 px by its summed halo). Every unit is tile invariant over tiles
//!   anchored at the stage origin (the [`crate::modules::SpatialUnit`] contract), and a window whose origin is
//!   on that grid holds exactly the stage's own tiles, so every pixel the next segment reads is the
//!   value the whole stage gives it. Its radii are the ones it was compiled with, against the whole
//!   proxy stage. Its mask is compiled against the whole stage and read at the window's offset. What
//!   the window cannot hold is a whole-stage reduction, so an operation that prepares a global
//!   estimate is handed the exact whole-stage estimate, including in the half-detail viewport.
//!   An operation with an estimate
//!   behind another spatial operation would need the first spatial stage's whole output to prepare
//!   it, so such a stack has no window.
//!
//! Everything else — the colour arithmetic, the masks, the finish units of the last segment, the
//! quantization — is untouched, so a windowed proxy frame is byte for byte the proxy frame of the
//! whole proxy stage, which is itself the exact recipe over the exact downscale for stacks without
//! a spatial operation. With a spatial operation, its exact whole-stage estimate is retained. The tests below
//! and in `preview::tests` prove these contracts.

use super::{Compiled, Entry, Segment, spatial::Tiling};
use crate::{
    Error,
    modules::{ExactGeometry, Global, Region, SpatialOperation, Stage},
};
use std::sync::Arc;

/// A reason a requested output rectangle cannot use a cut compilation. The caller can still
/// render the existing whole-frame path and report why it did so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegionFallback {
    Empty,
    TooLarge,
    PointReplacement,
    EstimateAfterSpatial,
    SegmentMismatch,
    ProxyCompileFailed,
    UnplannableGeometry,
    ProxyIneligible,
    ProxyUnavailable,
}

impl RegionFallback {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Empty => "the requested viewport lies outside the output stage",
            Self::TooLarge => "the viewport exceeds the 32 MiB region frame bound",
            Self::PointReplacement => "a point replacement cannot yet be cut to a viewport",
            Self::EstimateAfterSpatial => {
                "a global estimate behind an earlier spatial layer cannot yet be windowed"
            }
            Self::SegmentMismatch => "the scaled recipe has different stage boundaries",
            Self::ProxyCompileFailed => "the recipe cannot be compiled at half detail",
            Self::UnplannableGeometry => "the viewport cannot be mapped safely through the stack",
            Self::ProxyIneligible => "the recipe is not eligible for a scaled proxy",
            Self::ProxyUnavailable => "a half-scale proxy is not available for this stage",
        }
    }
}

/// The estimates one spatial operation is handed instead of reducing its own stage.
pub(crate) type Globals = Arc<Vec<Option<Global>>>;

/// Where a stack compiled against a whole proxy stage is cut down to what its output reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowPlan {
    /// The rectangle of the whole proxy source the first segment reads.
    pub(crate) source: Region,
    /// Per segment, the rectangle of its output stage that is kept.
    outputs: Vec<Region>,
}

/// The stage segment `index` reads: the source for the first, a resample's output stage, or the
/// stage a spatial operation receives and writes.
fn input_stage(segments: &[Segment], index: usize, source: Stage) -> Stage {
    match &segments[index].entry {
        None => source,
        Some(Entry::Resample(resample)) => Stage {
            width: resample.output_width,
            height: resample.output_height,
        },
        Some(Entry::Spatial { .. }) => segments[index - 1].stage(),
    }
}

/// Whether any unit of `operation` prepares a global estimate from a reduction of its stage.
fn prepares_estimates(operation: &SpatialOperation) -> bool {
    operation
        .units()
        .iter()
        .any(|unit| unit.estimate_key().is_some())
}

impl WindowPlan {
    /// The windows of `compiled`, a stack compiled against a whole proxy source of `source`
    /// dimensions, or `None` when the stack reads the whole source anyway or cannot be cut: a
    /// point replacement in a segment whose output would be cut, or a spatial operation with a
    /// global estimate behind another spatial operation. `O(segments)`, and reads
    /// no pixel.
    pub(crate) fn of(compiled: &Compiled, source: (u32, u32)) -> Option<Self> {
        let requested = Region::whole(compiled.segments.last()?.stage());
        let plan = Self::of_rect(compiled, source, requested).ok()?;
        (plan.source
            != Region::whole(Stage {
                width: source.0,
                height: source.1,
            }))
        .then_some(plan)
    }

    /// Plan a non-empty requested rectangle in the *uncut* output stage. Unlike the cropped-proxy
    /// convenience above, the final output may itself be cut even when the whole source is read.
    /// Its original stage coordinates are kept in `outputs` for positional finish units.
    pub(crate) fn of_rect(
        compiled: &Compiled,
        source: (u32, u32),
        requested: Region,
    ) -> Result<Self, RegionFallback> {
        let segments = &compiled.segments;
        let source = Stage {
            width: source.0,
            height: source.1,
        };
        let count = segments.len();
        if count == 0 || requested.is_empty() {
            return Err(RegionFallback::Empty);
        }
        let output = segments[count - 1].stage();
        if requested.x1() > output.width || requested.y1() > output.height {
            return Err(RegionFallback::UnplannableGeometry);
        }
        let mut outputs = vec![Region::whole(segments[count - 1].stage()); count];
        // What the segment being walked must produce, in its output stage's coordinates.
        let mut needed = requested;
        let mut read_source = None;
        for index in (0..count).rev() {
            let segment = &segments[index];
            if needed.is_empty() {
                return Err(RegionFallback::UnplannableGeometry);
            }
            outputs[index] = needed;
            let cut = needed != Region::whole(segment.stage());
            if cut && segment.has_pixels {
                return Err(RegionFallback::PointReplacement);
            }
            let input = input_stage(segments, index, source);
            let read = segment.geometry.unmap_region(needed);
            if read.x1() > input.width || read.y1() > input.height {
                return Err(RegionFallback::UnplannableGeometry);
            }
            match &segment.entry {
                None => read_source = Some(read),
                Some(Entry::Spatial { operation, .. }) => {
                    if prepares_estimates(operation) && compiled.spatial_before(index) {
                        return Err(RegionFallback::EstimateAfterSpatial);
                    }
                    let grown = read.grown(operation.summed_halo(input), input);
                    let tile = Tiling::Halo.tile(operation, input);
                    let x0 = grown.x0 / tile * tile;
                    let y0 = grown.y0 / tile * tile;
                    needed = Region {
                        x0,
                        y0,
                        width: grown.x1() - x0,
                        height: grown.y1() - y0,
                    };
                }
                Some(Entry::Resample(resample)) => {
                    needed = resample
                        .reads((0, 0), read, segments[index - 1].stage())
                        .ok_or(RegionFallback::UnplannableGeometry)?;
                }
            }
        }
        Ok(Self {
            source: read_source.ok_or(RegionFallback::UnplannableGeometry)?,
            outputs,
        })
    }

    /// Rewrite `compiled`, the stack this plan was made from, to read a source of the window's
    /// dimensions and keep only each segment's window. `globals(index)` answers the estimates the
    /// spatial operation entering segment `index` is handed when its stage is cut and it prepares
    /// one; it is asked nothing otherwise.
    pub(crate) fn apply(
        &self,
        mut compiled: Compiled,
        source: (u32, u32),
        mut globals: impl FnMut(usize) -> Result<Vec<Option<Global>>, Error>,
    ) -> Result<Compiled, Error> {
        let source = Stage {
            width: source.0,
            height: source.1,
        };
        if self.outputs.len() != compiled.segments.len() {
            return Err(Error::internal(
                "a proxy window was planned for another compilation",
            ));
        }
        // Each input stage must be read from the uncut compilation. Earlier iterations shrink
        // preceding segments, which would otherwise make a spatial mask appear already windowed
        // when its old stage and the preceding cut happen to share an origin.
        let full_inputs: Vec<_> = (0..compiled.segments.len())
            .map(|index| input_stage(&compiled.segments, index, source))
            .collect();
        for (index, whole_input) in full_inputs.iter().copied().enumerate() {
            // The stage this segment reads before and after the cut, and where the kept part of it
            // lies in the whole stage.
            // A resample's entry is itself a pixel-producing boundary. Keep only the part of its
            // full output stage that this segment's exact geometry reads, before rebasing that
            // geometry below. Otherwise a small viewport still materializes the whole crop.
            let entry_window = matches!(compiled.segments[index].entry, Some(Entry::Resample(_)))
                .then(|| {
                    compiled.segments[index]
                        .geometry
                        .unmap_region(self.outputs[index])
                });
            let (window, input) = match &compiled.segments[index].entry {
                None => (Some(self.source), self.source),
                Some(Entry::Spatial { .. }) => {
                    let window = self.outputs[index - 1];
                    (Some(window), window)
                }
                Some(Entry::Resample(_)) => {
                    let read = entry_window.expect("resample entry has a planned read");
                    (Some(read), read)
                }
            };
            if index > 0 {
                let previous = self.outputs[index - 1];
                let segment = &mut compiled.segments[index];
                match &mut segment.entry {
                    Some(Entry::Resample(_)) => {
                        segment.entry_origin = (previous.x0, previous.y0);
                        segment.entry_window = entry_window;
                    }
                    Some(Entry::Spatial {
                        operation,
                        globals: handed,
                        ..
                    }) => {
                        if previous != Region::whole(whole_input)
                            && let Some(mask) = operation.mask()
                        {
                            let windowed = mask.windowed(previous);
                            *operation = operation.clone().with_mask(windowed);
                        }
                        if prepares_estimates(operation) {
                            *handed = Some(Arc::new(globals(index)?));
                        }
                    }
                    _ => {}
                }
            }
            let segment = &mut compiled.segments[index];
            if let Some(window) = window
                && window != Region::whole(whole_input)
            {
                // Place the window at its origin in the whole stage, ahead of everything the
                // segment composed: an integer translation, so the composition stays exact.
                let place = ExactGeometry::crop(
                    -i64::from(window.x0),
                    -i64::from(window.y0),
                    whole_input.width,
                    whole_input.height,
                );
                segment.geometry = place.then(segment.geometry);
            }
            let kept = self.outputs[index];
            if kept != Region::whole(segment.stage()) {
                segment.output_origin = (kept.x0, kept.y0);
                let cut = ExactGeometry::crop(
                    i64::from(kept.x0),
                    i64::from(kept.y0),
                    kept.width,
                    kept.height,
                );
                segment.geometry = segment.geometry.then(cut);
                segment
                    .operations
                    .push(crate::modules::Processing::ExactGeometry(cut));
                segment.width = kept.width;
                segment.height = kept.height;
            }
            if !segment.geometry.reads_inside(input.width, input.height) {
                return Err(Error::internal(format!(
                    "a proxy window's segment {index} reads outside its {}x{} input",
                    input.width, input.height
                )));
            }
        }
        Ok(compiled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BASIC_EFFECT, Cancel, Component, ComponentMode, CropPayload, EFFECT_FORMAT, Layer, LayerId,
        LinearImage, LinearSettings, Mask, ModuleRegistry, Orientation, PRESENCE_EFFECT,
        PreviewSource, ProxyBounds, ProxyPlan, RECIPE_FORMAT, Recipe, SnapshotId, SourceImage,
        VIGNETTE_EFFECT,
        render::{Render, RenderContext, RenderOptions, render},
    };
    use serde_json::{Value, json};

    fn layer(effect: &str, payload: Value, mask: Option<&Mask>) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: effect.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            mask: mask.map(|mask| mask.id.clone()),
            artifacts: Vec::new(),
        }
    }

    fn crop(angle: f64, x: f64, y: f64, width: f64, height: f64) -> Layer {
        Layer::crop(CropPayload {
            angle,
            x,
            y,
            width,
            height,
        })
    }

    fn gradient(name: &str) -> Mask {
        let mut mask = Mask::new(name);
        let component = mask.next_component_name("linear");
        mask.components.push(Component::new(
            component,
            ComponentMode::Add,
            "linear",
            json!({"x0": 0.3, "y0": 0.2, "x1": 0.8, "y1": 0.9}),
        ));
        mask
    }

    fn recipe(layers: Vec<Layer>, masks: Vec<Mask>) -> Recipe {
        Recipe {
            format: RECIPE_FORMAT,
            layers,
            masks,
            ..Recipe::default()
        }
    }

    /// A photo-like pattern with detail at every scale, so a pixel read from the wrong place, a
    /// tap with the wrong weight or an estimate from the wrong stage changes bytes.
    fn value(x: u32, y: u32, channel: u32) -> f64 {
        let (x, y) = (f64::from(x), f64::from(y));
        let wave = (x * 0.013 + y * 0.007 + f64::from(channel)).sin() * 0.3
            + (x * 0.21 - y * 0.17 * f64::from(channel + 1)).cos() * 0.15;
        (0.45 + wave + ((x * 7.0 + y * 3.0) % 11.0) / 60.0).clamp(0.0, 1.0)
    }

    fn jpeg(width: u32, height: u32) -> PreviewSource {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                for channel in 0..3 {
                    rgba.push((value(x, y, channel) * 255.0).round() as u8);
                }
                rgba.push(255);
            }
        }
        PreviewSource::Jpeg(SourceImage {
            width,
            height,
            rgba: rgba.into(),
            fingerprint: "sha256:window-fixture".into(),
            orientation: 1,
            capture: Default::default(),
        })
    }

    fn raw(width: u32, height: u32) -> PreviewSource {
        let mut planes = Vec::with_capacity((width * height * 3) as usize);
        for channel in 0..3 {
            for y in 0..height {
                for x in 0..width {
                    planes.push((value(x, y, channel) * 1.4 - 0.05) as f32);
                }
            }
        }
        PreviewSource::Raw {
            image: LinearImage::with_fingerprint(width, height, planes, "sha256:window-raw")
                .expect("an image"),
            settings: LinearSettings::default(),
        }
    }

    /// The exact compilation a preview job starts from, in the test's `context`.
    fn exact<'a>(
        context: &'a RenderContext,
        registry: &ModuleRegistry,
        source: &'a PreviewSource,
        stack: &Recipe,
    ) -> Render<'a> {
        render(
            registry,
            source.input(),
            stack,
            RenderOptions::default(),
            context,
        )
        .expect("the exact compilation")
    }

    /// What the preview worker does for one job after its exact compilation: the fitted plan, its
    /// window, the windowed proxy source and the frame rendered against it.
    fn windowed_frame(
        context: &RenderContext,
        registry: &ModuleRegistry,
        source: &PreviewSource,
        exact: &Render<'_>,
        stack: &Recipe,
        bounds: ProxyBounds,
    ) -> (ProxyPlan, crate::Raster) {
        let plan = exact.proxy_plan(bounds).expect("a proxy is worthwhile");
        let stage = exact.proxy_window(registry, stack, plan);
        let plan = stage.plan();
        let proxy = source.proxy(plan).expect("the windowed proxy source");
        assert_eq!(proxy.dimensions(), plan.source_dimensions());
        let frame = exact
            .render_proxy(proxy.input(), stage, &Cancel::never(), context)
            .expect("the windowed compilation")
            .frame(SnapshotId::new())
            .expect("the windowed proxy frame");
        (plan, frame)
    }

    /// The whole proxy stage's frame: the exact recipe over the exact downscale, which is what the
    /// proxy phase has always been proven to be.
    fn whole_frame(
        context: &RenderContext,
        registry: &ModuleRegistry,
        source: &PreviewSource,
        stack: &Recipe,
        plan: ProxyPlan,
    ) -> crate::Raster {
        let whole = source.proxy(plan.whole()).expect("the whole downscale");
        render(
            registry,
            whole.input(),
            stack,
            RenderOptions::proxy(&Cancel::never()),
            context,
        )
        .expect("the whole proxy stage")
        .frame(SnapshotId::new())
        .expect("the whole proxy frame")
    }

    fn cropped_bytes(frame: &crate::Raster, rect: Region) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(rect.pixels() as usize * 4);
        for y in rect.y0..rect.y1() {
            let at = ((u64::from(y) * u64::from(frame.width) + u64::from(rect.x0)) * 4) as usize;
            bytes.extend_from_slice(&frame.rgba[at..at + rect.width as usize * 4]);
        }
        bytes
    }

    /// Render the uncut half stage with an estimate computed in an independent exact context.
    /// This reference does not use either production region's source window or cut compilation.
    fn whole_half_frame_with_exact_estimate(
        render: &Render<'_>,
        registry: &ModuleRegistry,
        proxy: &PreviewSource,
        stack: &Recipe,
        plan: crate::ProxyRegionPlan,
        context: &crate::RenderContext,
    ) -> crate::Raster {
        let mut compiled = registry
            .compile_sampled(
                plan.proxy.width,
                plan.proxy.height,
                stack,
                crate::mask_field::MaskSampling::ThinFeature,
            )
            .unwrap();
        for (index, segment) in compiled.segments.iter_mut().enumerate() {
            if let Some(Entry::Spatial { globals, .. }) = &mut segment.entry {
                *globals = Some(Arc::new(render.spatial_globals(index).unwrap()));
            }
        }
        Render::compiled(
            proxy.input(),
            compiled,
            RenderOptions::proxy(&Cancel::never()),
            context,
        )
        .unwrap()
        .frame(SnapshotId::new())
        .unwrap()
    }

    #[test]
    fn exact_viewport_matches_whole_buffer_on_both_domains_and_complex_stacks() {
        let registry = ModuleRegistry::builtin();
        let mask = gradient("viewport mask");
        let stacks = [
            recipe(
                vec![
                    layer(
                        BASIC_EFFECT,
                        json!({"exposure": 0.35, "contrast": 20}),
                        None,
                    ),
                    layer(VIGNETTE_EFFECT, json!({"amount": -63}), None),
                ],
                Vec::new(),
            ),
            recipe(
                vec![
                    Layer::orientation(Orientation {
                        mirror: true,
                        turns: 1,
                    }),
                    layer(BASIC_EFFECT, json!({"exposure": -0.4}), Some(&mask)),
                    crop(6.0, 0.12, 0.14, 0.72, 0.70),
                    layer(VIGNETTE_EFFECT, json!({"amount": -50}), None),
                ],
                vec![mask.clone()],
            ),
            recipe(
                vec![
                    layer(BASIC_EFFECT, json!({"exposure": 0.2}), None),
                    layer(
                        PRESENCE_EFFECT,
                        json!({"clarity": 35, "dehaze": 24}),
                        Some(&mask),
                    ),
                    crop(0.0, 0.1, 0.1, 0.8, 0.8),
                    layer(VIGNETTE_EFFECT, json!({"amount": -45}), None),
                ],
                vec![mask.clone()],
            ),
        ];
        let oriented_raw = match raw(192, 144) {
            PreviewSource::Raw { image, settings } => PreviewSource::Raw {
                image: image.with_view([16, 12, 140, 110], 6).unwrap(),
                settings,
            },
            _ => unreachable!(),
        };
        for (domain, source) in [
            ("byte", jpeg(192, 144)),
            ("raw", raw(192, 144)),
            ("raw-oriented-view", oriented_raw),
        ] {
            for (case, stack) in stacks.iter().enumerate() {
                let region_context = crate::RenderContext::new();
                let region_render = render(
                    &registry,
                    source.input(),
                    stack,
                    RenderOptions::default(),
                    &region_context,
                )
                .expect("one compilation");
                let (width, height) = region_render.stage();
                let whole_context = crate::RenderContext::new();
                let whole = render(
                    &registry,
                    source.input(),
                    stack,
                    RenderOptions::default(),
                    &whole_context,
                )
                .expect("independent whole compilation")
                .frame(SnapshotId::new())
                .expect("whole reference");
                let mut regions = Vec::new();
                for requested in [
                    Region {
                        x0: 0,
                        y0: 0,
                        width: width.min(43),
                        height: height.min(27),
                    },
                    Region {
                        x0: width / 3,
                        y0: height / 4,
                        width: width / 2,
                        height: height / 2,
                    },
                    Region {
                        x0: width.saturating_sub(19),
                        y0: height.saturating_sub(13),
                        width: 40,
                        height: 30,
                    },
                ] {
                    let crate::RegionRenderOutcome::Rendered(region) = region_render
                        .region(SnapshotId::new(), requested)
                        .expect("region evaluation")
                    else {
                        panic!("{domain} case {case}: eligible region declined");
                    };
                    assert_eq!(region.stage, crate::StageSize { width, height });
                    assert_eq!(region.rect, region.full_rect);
                    regions.push(region);
                }
                for region in regions {
                    assert_eq!(
                        region.raster.rgba.as_slice(),
                        cropped_bytes(&whole, region.rect),
                        "{domain} case {case} at {:?}",
                        region.rect
                    );
                }
            }
        }
    }

    #[test]
    fn a_half_detail_region_is_the_same_scaled_recipe_at_the_same_pixels() {
        let registry = ModuleRegistry::builtin();
        let context = RenderContext::new();
        let mask = gradient("half-detail cut mask");
        let stack = recipe(
            vec![
                Layer::orientation(Orientation {
                    mirror: true,
                    turns: 1,
                }),
                layer(BASIC_EFFECT, json!({"exposure": 0.3}), Some(&mask)),
                layer(PRESENCE_EFFECT, json!({"clarity": 24}), Some(&mask)),
                crop(4.0, 0.1, 0.1, 0.8, 0.8),
                layer(VIGNETTE_EFFECT, json!({"amount": -65}), None),
            ],
            vec![mask],
        );
        for (domain, source) in [("byte", jpeg(321, 241)), ("raw", raw(321, 241))] {
            let full = exact(&context, &registry, &source, &stack);
            let (width, height) = full.stage();
            for requested in [
                Region {
                    x0: 0,
                    y0: 0,
                    width: 55,
                    height: 43,
                },
                Region {
                    x0: 37,
                    y0: 25,
                    width: width / 3,
                    height: height / 3,
                },
                Region {
                    x0: width - 47,
                    y0: height - 39,
                    width: 47,
                    height: 39,
                },
            ] {
                let plan = full
                    .plan_proxy_region(&registry, &stack, requested)
                    .expect("half plan");
                let proxy_source = source.proxy(plan.proxy).expect("bounded proxy");
                let crate::RegionRenderOutcome::Rendered(region) = full
                    .render_proxy_region(
                        &registry,
                        proxy_source.input(),
                        &stack,
                        plan,
                        SnapshotId::new(),
                        &context,
                    )
                    .expect("half region")
                else {
                    panic!("{domain}: half region declined")
                };
                let whole_proxy_source = source
                    .proxy(plan.proxy.whole())
                    .expect("whole reference source");
                let reference = render(
                    &registry,
                    whole_proxy_source.input(),
                    &stack,
                    RenderOptions::proxy(&Cancel::never()),
                    &context,
                )
                .unwrap()
                .frame(SnapshotId::new())
                .unwrap();
                assert_eq!(
                    region.raster.rgba.as_slice(),
                    cropped_bytes(&reference, region.rect),
                    "{domain} at {requested:?}"
                );
                assert!(region.approximation.reduced_detail);
                assert_eq!(region.full_rect, requested);
            }
        }
    }

    #[test]
    fn sub_megapixel_heavy_colour_pool_matches_serial_regions_on_both_domains() {
        // The whole stage is below the ordinary colour pass's pool threshold but above the
        // heavy-colour threshold; every quarter is below both and uses the serial row path.
        let (width, height) = (299, 277);
        assert!(u64::from(width) * u64::from(height) < luxforge_raw::PARALLEL_COLOUR_PIXELS);
        assert!(u64::from(width) * u64::from(height) >= luxforge_raw::PARALLEL_HEAVY_COLOUR_PIXELS);
        let registry = ModuleRegistry::builtin();
        let stack = recipe(
            vec![layer(
                BASIC_EFFECT,
                json!({
                    "exposure": 0.4, "contrast": 23, "highlights": -17, "shadows": 21,
                    "whites": -12, "blacks": 9, "temperature": 13, "tint": -8,
                    "vibrance": 16, "saturation": 11
                }),
                None,
            )],
            Vec::new(),
        );
        let units: usize = registry
            .compile(width, height, &stack)
            .unwrap()
            .segments
            .iter()
            .flat_map(|segment| &segment.operations)
            .filter_map(|operation| match operation {
                crate::modules::Processing::Color(colour) => Some(colour.len()),
                _ => None,
            })
            .sum();
        assert!(
            units >= 3,
            "the whole case must take the heavy-colour pool path"
        );
        for (domain, source) in [("byte", jpeg(width, height)), ("raw", raw(width, height))] {
            let context = crate::RenderContext::new();
            let render = render(
                &registry,
                source.input(),
                &stack,
                RenderOptions::default(),
                &context,
            )
            .unwrap();
            let whole = render.frame(SnapshotId::new()).unwrap();
            for (y0, y1) in [(0, height / 2), (height / 2, height)] {
                for (x0, x1) in [(0, width / 2), (width / 2, width)] {
                    let rect = Region {
                        x0,
                        y0,
                        width: x1 - x0,
                        height: y1 - y0,
                    };
                    assert!(rect.pixels() < luxforge_raw::PARALLEL_HEAVY_COLOUR_PIXELS);
                    let crate::RegionRenderOutcome::Rendered(part) =
                        render.region(SnapshotId::new(), rect).unwrap()
                    else {
                        panic!("{domain}: serial quarter region declined")
                    };
                    assert_eq!(
                        part.raster.rgba.as_slice(),
                        cropped_bytes(&whole, rect),
                        "{domain}: heavy colour pool differs at quarter {rect:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn rotated_resample_entry_keeps_only_the_viewport_on_both_domains() {
        let registry = ModuleRegistry::builtin();
        let stack = recipe(vec![crop(5.0, 0.15, 0.15, 0.7, 0.6)], Vec::new());
        for (domain, source) in [("byte", jpeg(1000, 800)), ("raw", raw(1000, 800))] {
            let context = crate::RenderContext::new();
            let render = render(
                &registry,
                source.input(),
                &stack,
                RenderOptions::default(),
                &context,
            )
            .unwrap();
            let (width, height) = render.stage();
            let virtual_pixels = u64::from(width) * u64::from(height);
            let compiled = registry
                .compile(source.dimensions().0, source.dimensions().1, &stack)
                .unwrap();
            let whole = render.frame(SnapshotId::new()).unwrap();
            for (y0, y1) in [(0, height / 2), (height / 2, height)] {
                for (x0, x1) in [(0, width / 2), (width / 2, width)] {
                    let rect = Region {
                        x0,
                        y0,
                        width: x1 - x0,
                        height: y1 - y0,
                    };
                    let plan = WindowPlan::of_rect(&compiled, source.dimensions(), rect).unwrap();
                    let cut = plan
                        .apply(compiled.clone(), source.dimensions(), |_| {
                            unreachable!("this crop has no global estimates")
                        })
                        .unwrap();
                    let resample = &cut.segments[1];
                    let entry = resample.entry_window.expect("resample entry window");
                    assert_eq!(entry.pixels(), rect.pixels());
                    assert!(entry.pixels() < virtual_pixels);
                    assert_eq!(
                        crate::Raster::expected_len(entry.width, entry.height).unwrap(),
                        (rect.pixels() * 4) as usize,
                        "{domain}: entry allocation must track the viewport, not the virtual crop"
                    );
                    let region_context = crate::RenderContext::new();
                    let region_render = crate::render::render(
                        &registry,
                        source.input(),
                        &stack,
                        RenderOptions::default(),
                        &region_context,
                    )
                    .unwrap();
                    let crate::RegionRenderOutcome::Rendered(part) =
                        region_render.region(SnapshotId::new(), rect).unwrap()
                    else {
                        panic!("{domain}: serial cropped region declined")
                    };
                    let peak = region_context.resample_peak_bytes();
                    if domain == "byte" {
                        assert_eq!(peak, rect.pixels() * 4);
                    } else {
                        assert!(peak > 0, "RAW resample tap block must be counted");
                        assert!(
                            peak <= super::super::linear::TAP_BLOCK_PIXELS * 48,
                            "RAW resample tap block must stay bounded: {peak}"
                        );
                        assert!(
                            peak < virtual_pixels * 24,
                            "RAW region must not allocate the whole virtual crop: {peak}"
                        );
                    }
                    assert_eq!(
                        part.raster.rgba.as_slice(),
                        cropped_bytes(&whole, rect),
                        "{domain}: bounded resample differs at quarter {rect:?}"
                    );
                }
            }
        }
    }

    /// Native diagnostic for moving 60 MP Basic + 7° crop. The caller supplies the generated
    /// JPEG path; decode and fixture preparation are outside every reported phase. This is a
    /// measurement, not a timing gate: host load and proxy-cache reuse are recorded separately.
    #[test]
    #[ignore = "requires LUXFORGE_VIEWPORT_PROFILE_SOURCE and a native release test run"]
    fn measure_moving_rotated_crop_viewport_phases() {
        use std::time::Instant;
        let path =
            std::env::var("LUXFORGE_VIEWPORT_PROFILE_SOURCE").expect("photo-sized JPEG path");
        let source = PreviewSource::Jpeg(crate::open_source(std::path::Path::new(&path)).unwrap());
        assert_eq!(source.dimensions(), (10_000, 6_000));
        let registry = ModuleRegistry::builtin();
        let crop = crop(
            7.0,
            0.08614317111317343,
            0.15430785840108324,
            0.827650075401078,
            0.6913884170455039,
        );
        let full = recipe(
            vec![
                layer(
                    BASIC_EFFECT,
                    json!({
                        "blacks": 15.0, "contrast": 25.0, "exposure": 0.5,
                        "highlights": -30.0, "saturation": 15.0, "shadows": 30.0,
                        "temperature": 20.0, "tint": -10.0, "vibrance": 30.0,
                        "whites": -15.0
                    }),
                    None,
                ),
                crop.clone(),
            ],
            Vec::new(),
        );
        let crop_only = recipe(vec![crop], Vec::new());
        let context = crate::RenderContext::new();
        let requests = [
            Region {
                x0: 1144,
                y0: 3531,
                width: 901,
                height: 833,
            },
            Region {
                x0: 1497,
                y0: 3347,
                width: 901,
                height: 833,
            },
            Region {
                x0: 4674,
                y0: 1691,
                width: 901,
                height: 833,
            },
        ];
        for request in requests {
            let compile_start = Instant::now();
            let exact = render(
                &registry,
                source.input(),
                &full,
                RenderOptions::default(),
                &context,
            )
            .unwrap();
            let plan = exact.plan_proxy_region(&registry, &full, request).unwrap();
            let compile_ms = compile_start.elapsed().as_secs_f64() * 1000.0;
            let source_start = Instant::now();
            let proxy = source
                .proxy_cancellable(plan.proxy, &Cancel::never())
                .unwrap();
            let source_ms = source_start.elapsed().as_secs_f64() * 1000.0;
            let render_start = Instant::now();
            let crate::RegionRenderOutcome::Rendered(_) = exact
                .render_proxy_region(
                    &registry,
                    proxy.input(),
                    &full,
                    plan,
                    SnapshotId::new(),
                    &context,
                )
                .unwrap()
            else {
                panic!("full stack viewport declined")
            };
            let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
            let crop_exact = render(
                &registry,
                source.input(),
                &crop_only,
                RenderOptions::default(),
                &context,
            )
            .unwrap();
            let crop_start = Instant::now();
            let crate::RegionRenderOutcome::Rendered(_) = crop_exact
                .render_proxy_region(
                    &registry,
                    proxy.input(),
                    &crop_only,
                    plan,
                    SnapshotId::new(),
                    &context,
                )
                .unwrap()
            else {
                panic!("crop-only viewport declined")
            };
            let crop_ms = crop_start.elapsed().as_secs_f64() * 1000.0;
            eprintln!(
                "moving-crop region={request:?} proxy_window={:?} half_output={:?} compile_plan_ms={compile_ms:.3} source_build_ms={source_ms:.3} full_render_ms={render_ms:.3} crop_only_render_ms={crop_ms:.3}",
                plan.proxy.window, plan.output
            );
        }
    }

    #[test]
    fn a_raw_window_composes_every_source_orientation_without_copying_planes() {
        let mut planes = Vec::new();
        for channel in 0..3 {
            for y in 0..9 {
                for x in 0..11 {
                    planes.push((channel * 1000 + y * 11 + x) as f32);
                }
            }
        }
        let base = LinearImage::with_fingerprint(11, 9, planes, "raw-view").unwrap();
        for orientation in 1..=8 {
            let viewed = base.with_view([2, 1, 7, 6], orientation).unwrap();
            let region = Region {
                x0: 1,
                y0: 1,
                width: viewed.width() - 2,
                height: viewed.height() - 2,
            };
            let cut = viewed.window(region).unwrap();
            assert_eq!((cut.width(), cut.height()), (region.width, region.height));
            assert_eq!(cut.development(), viewed.development());
            assert!(std::ptr::eq(
                cut.planes().as_ptr(),
                viewed.planes().as_ptr()
            ));
            for y in 0..region.height {
                for x in 0..region.width {
                    assert_eq!(
                        cut.reader().pixel(x, y),
                        viewed.reader().pixel(x + region.x0, y + region.y0),
                        "orientation {orientation} at {x},{y}",
                    );
                }
            }
        }
    }

    #[test]
    fn a_spatial_viewport_crossing_the_tile_grid_keeps_whole_stage_pixels() {
        let registry = ModuleRegistry::builtin();
        let mask = gradient("wide spatial mask");
        let stack = recipe(
            vec![
                layer(BASIC_EFFECT, json!({"exposure": 0.25}), None),
                layer(
                    PRESENCE_EFFECT,
                    json!({"clarity": 40, "dehaze": 28}),
                    Some(&mask),
                ),
                layer(VIGNETTE_EFFECT, json!({"amount": -55}), None),
            ],
            vec![mask],
        );
        // At 1800 px the summed halo is small and the operation runs in 512 px tiles; at 4400 px
        // the same payload's halo passes the bound and it runs in 1024 px tiles. Either way the
        // window's origin is on its own tile grid and the output crosses one of its seams.
        for (width, x0, expected) in [
            (1800, 880, luxforge_raw::SPATIAL_TILE),
            (4400, 1880, luxforge_raw::SPATIAL_WIDE_TILE),
        ] {
            spatial_viewport_case(&registry, &stack, width, x0, expected);
        }
    }

    fn spatial_viewport_case(
        registry: &ModuleRegistry,
        stack: &Recipe,
        width: u32,
        x0: u32,
        expected: u32,
    ) {
        let requested = Region {
            x0,
            y0: 19,
            width: 420,
            height: 85,
        };
        let compiled = registry
            .compile_sampled(width, 128, stack, crate::mask_field::MaskSampling::Point)
            .unwrap();
        let plan = WindowPlan::of_rect(&compiled, (width, 128), requested).unwrap();
        let tile = compiled
            .segments
            .iter()
            .find_map(|segment| match &segment.entry {
                Some(Entry::Spatial { operation, .. }) => {
                    Some(Tiling::Halo.tile(operation, Stage { width, height: 128 }))
                }
                _ => None,
            })
            .expect("a spatial segment");
        assert_eq!(tile, expected, "{width} px: the operation's tile");
        assert!(
            plan.source.x0 >= tile,
            "the source window must have a nonzero tile origin"
        );
        assert_eq!(
            plan.source.x0 % tile,
            0,
            "on the grid of the {tile} px tiles"
        );
        assert!(
            requested.x0 < 2 * tile && requested.x1() > 2 * tile,
            "the output crosses a tile seam"
        );
        for (domain, source) in [("byte", jpeg(width, 128)), ("raw", raw(width, 128))] {
            let region_context = crate::RenderContext::new();
            let region_render = render(
                registry,
                source.input(),
                stack,
                RenderOptions::default(),
                &region_context,
            )
            .unwrap();
            let whole_context = crate::RenderContext::new();
            let whole = render(
                registry,
                source.input(),
                stack,
                RenderOptions::default(),
                &whole_context,
            )
            .unwrap()
            .frame(SnapshotId::new())
            .unwrap();
            let crate::RegionRenderOutcome::Rendered(region) =
                region_render.region(SnapshotId::new(), requested).unwrap()
            else {
                panic!("{domain}, {width} px: region declined")
            };
            assert_eq!(
                region.raster.rgba.as_slice(),
                cropped_bytes(&whole, requested),
                "{domain}, {width} px"
            );
        }
    }

    #[test]
    fn pixel_replacements_and_global_estimates_after_spatial_are_named_fallbacks() {
        let registry = ModuleRegistry::developer();
        let context = RenderContext::new();
        let source = jpeg(96, 64);
        let requested = Region {
            x0: 20,
            y0: 10,
            width: 30,
            height: 20,
        };
        let pixel = recipe(vec![Layer::pixel(30, 20, [2, 3, 4])], Vec::new());
        let render = exact(&context, &registry, &source, &pixel);
        assert!(matches!(
            render.region(SnapshotId::new(), requested).unwrap(),
            crate::RegionRenderOutcome::Declined(RegionFallback::PointReplacement)
        ));
        let mask = gradient("second Presence target");
        let chained = recipe(
            vec![
                layer(PRESENCE_EFFECT, json!({"clarity": 20}), None),
                layer(PRESENCE_EFFECT, json!({"dehaze": 25}), Some(&mask)),
            ],
            vec![mask],
        );
        let render = exact(&context, &registry, &source, &chained);
        assert!(matches!(
            render.region(SnapshotId::new(), requested).unwrap(),
            crate::RegionRenderOutcome::Declined(RegionFallback::EstimateAfterSpatial)
        ));
        assert_eq!(
            render
                .plan_proxy_region(&registry, &chained, requested)
                .unwrap_err(),
            RegionFallback::EstimateAfterSpatial
        );
    }

    #[test]
    fn exact_estimates_obey_the_eight_entry_store_bound() {
        let registry = ModuleRegistry::builtin();
        let source = jpeg(96, 64);
        let context = crate::RenderContext::new();
        let mut first_keys = Vec::new();
        for step in 0..9 {
            let stack = recipe(
                vec![
                    layer(
                        BASIC_EFFECT,
                        json!({"exposure": f64::from(step) * 0.1}),
                        None,
                    ),
                    layer(PRESENCE_EFFECT, json!({"dehaze": 30}), None),
                ],
                Vec::new(),
            );
            let render = render(
                &registry,
                source.input(),
                &stack,
                RenderOptions::default(),
                &context,
            )
            .unwrap();
            render.spatial_globals(1).unwrap();
            if step == 0 {
                first_keys = context.estimates().keys();
                assert_eq!(first_keys.len(), 1);
            }
        }
        let keys = context.estimates().keys();
        assert_eq!(keys.len(), crate::modules::ESTIMATE_STORE_ENTRIES);
        assert!(
            first_keys.iter().all(|key| !keys.contains(key)),
            "the oldest exact estimate should be evicted"
        );
    }

    #[test]
    fn half_detail_dehaze_overlap_is_identical_across_pans() {
        let registry = ModuleRegistry::builtin();
        let stack = recipe(
            vec![
                layer(BASIC_EFFECT, json!({"exposure": 0.3}), None),
                layer(PRESENCE_EFFECT, json!({"clarity": 20, "dehaze": 32}), None),
            ],
            Vec::new(),
        );
        for (domain, source) in [("byte", jpeg(900, 180)), ("raw", raw(900, 180))] {
            let context = crate::RenderContext::new();
            let exact = render(
                &registry,
                source.input(),
                &stack,
                RenderOptions::default(),
                &context,
            )
            .unwrap();
            let mut regions = Vec::new();
            for request in [
                Region {
                    x0: 100,
                    y0: 20,
                    width: 450,
                    height: 120,
                },
                Region {
                    x0: 350,
                    y0: 20,
                    width: 450,
                    height: 120,
                },
            ] {
                let plan = exact.plan_proxy_region(&registry, &stack, request).unwrap();
                let proxy = source.proxy(plan.proxy).unwrap();
                let crate::RegionRenderOutcome::Rendered(frame) = exact
                    .render_proxy_region(
                        &registry,
                        proxy.input(),
                        &stack,
                        plan,
                        SnapshotId::new(),
                        &context,
                    )
                    .unwrap()
                else {
                    panic!("{domain}: region declined")
                };
                assert!(frame.approximation.reduced_detail);
                let reference_context = crate::RenderContext::new();
                let reference_exact = render(
                    &registry,
                    source.input(),
                    &stack,
                    RenderOptions::default(),
                    &reference_context,
                )
                .unwrap();
                let whole_proxy = source.proxy(plan.proxy.whole()).unwrap();
                let whole_estimate_proxy = whole_half_frame_with_exact_estimate(
                    &reference_exact,
                    &registry,
                    &whole_proxy,
                    &stack,
                    plan,
                    &reference_context,
                );
                assert_eq!(
                    frame.raster.rgba.as_slice(),
                    cropped_bytes(&whole_estimate_proxy, frame.rect),
                    "{domain}: production half viewport must use exact globals"
                );
                regions.push(frame);
            }
            let left = &regions[0];
            let right = &regions[1];
            let overlap = Region {
                x0: left.rect.x0.max(right.rect.x0),
                y0: left.rect.y0.max(right.rect.y0),
                width: left.rect.x1().min(right.rect.x1()) - left.rect.x0.max(right.rect.x0),
                height: left.rect.y1().min(right.rect.y1()) - left.rect.y0.max(right.rect.y0),
            };
            assert!(!overlap.is_empty());
            let region_bytes = |frame: &crate::RegionFrame| {
                let local = Region {
                    x0: overlap.x0 - frame.rect.x0,
                    y0: overlap.y0 - frame.rect.y0,
                    width: overlap.width,
                    height: overlap.height,
                };
                cropped_bytes(&frame.raster, local)
            };
            assert_eq!(
                region_bytes(left),
                region_bytes(right),
                "{domain}: pan changed overlapping pixels"
            );
            assert_eq!(
                context.estimates().keys().len(),
                1,
                "{domain}: one exact estimate identity serves both pan rectangles"
            );
        }
    }

    #[test]
    fn raw_estimate_keys_separate_view_development_and_white_balance_approximation() {
        let registry = ModuleRegistry::builtin();
        let stack = recipe(
            vec![layer(PRESENCE_EFFECT, json!({"dehaze": 30}), None)],
            Vec::new(),
        );
        let PreviewSource::Raw { image, .. } = raw(96, 64) else {
            unreachable!()
        };
        let context = crate::RenderContext::new();
        let source = |image: &LinearImage, settings: LinearSettings| {
            render(
                &registry,
                crate::RenderSource::Linear { image, settings },
                &stack,
                RenderOptions::default(),
                &context,
            )
            .unwrap()
            .spatial_globals(1)
            .unwrap();
        };
        source(&image, LinearSettings::default());
        let balance = crate::WhiteBalanceApproximation::from_matrix([
            [1.08, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 0.94],
        ])
        .unwrap();
        source(
            &image,
            LinearSettings {
                white_balance: Some(balance),
            },
        );
        let oriented = image.with_view([4, 3, 80, 55], 6).unwrap();
        source(&oriented, LinearSettings::default());
        let redeveloped =
            LinearImage::with_fingerprint(96, 64, image.planes().to_vec(), image.fingerprint())
                .unwrap();
        source(&redeveloped, LinearSettings::default());
        let keys = context.estimates().keys();
        assert_eq!(
            keys.len(),
            4,
            "each RAW view/development/WB state gets an exact estimate"
        );
    }

    /// Stacks without a spatial operation: the windowed proxy frame is byte for byte the frame of
    /// the whole proxy stage — the exact recipe over the exact downscale — for a straight and a
    /// straightened tight crop, behind an orientation, a masked colour layer and a finish layer,
    /// on both pixel domains.
    #[test]
    fn a_windowed_proxy_frame_is_the_whole_proxy_stages_frame() {
        let context = RenderContext::new();
        let registry = ModuleRegistry::builtin();
        let mask = gradient("Mask 1");
        let stacks = [
            (
                "straight crop behind an orientation",
                recipe(
                    vec![
                        Layer::orientation(Orientation {
                            mirror: true,
                            turns: 1,
                        }),
                        layer(BASIC_EFFECT, json!({"exposure": 0.4, "contrast": 25}), None),
                        crop(0.0, 0.61, 0.17, 0.13, 0.2),
                    ],
                    Vec::new(),
                ),
            ),
            (
                "straightened crop under a masked colour layer",
                recipe(
                    vec![
                        layer(BASIC_EFFECT, json!({"exposure": -0.3}), None),
                        layer(BASIC_EFFECT, json!({"saturation": 40}), Some(&mask)),
                        crop(4.5, 0.42, 0.47, 0.14, 0.12),
                    ],
                    vec![mask.clone()],
                ),
            ),
            (
                "straightened crop behind an orientation, then a vignette",
                recipe(
                    vec![
                        Layer::orientation(Orientation {
                            mirror: false,
                            turns: 3,
                        }),
                        layer(BASIC_EFFECT, json!({"highlights": -40}), Some(&mask)),
                        crop(-7.0, 0.45, 0.4, 0.12, 0.16),
                        layer(VIGNETTE_EFFECT, json!({"amount": -50}), None),
                    ],
                    vec![mask.clone()],
                ),
            ),
        ];
        let bounds = ProxyBounds {
            width: 150,
            height: 110,
        };
        for (domain, source) in [("jpeg", jpeg(1500, 1000)), ("raw", raw(1500, 1000))] {
            for (name, stack) in &stacks {
                let exact = exact(&context, &registry, &source, stack);
                let (plan, frame) =
                    windowed_frame(&context, &registry, &source, &exact, stack, bounds);
                let window = plan.window.expect("a tight crop's proxy is windowed");
                assert!(
                    u64::from(window.width) * u64::from(window.height)
                        < u64::from(plan.width) * u64::from(plan.height) / 4,
                    "{domain} {name}: a {window:?} window of a {}x{} stage",
                    plan.width,
                    plan.height
                );
                let whole = whole_frame(&context, &registry, &source, stack, plan);
                assert_eq!(
                    (frame.width, frame.height),
                    (whole.width, whole.height),
                    "{domain} {name}"
                );
                assert!(
                    frame.rgba == whole.rgba,
                    "{domain} {name}: the windowed proxy frame differs from the whole stage's"
                );
            }
        }
    }

    #[test]
    fn a_cold_windowed_fit_proxy_uses_its_own_cancel_token_for_dehaze() {
        let registry = ModuleRegistry::builtin();
        let source = jpeg(600, 400);
        let stack = recipe(
            vec![
                layer(PRESENCE_EFFECT, json!({"dehaze": 28}), None),
                crop(0.0, 0.3, 0.3, 0.4, 0.4),
            ],
            Vec::new(),
        );
        let context = crate::RenderContext::new();
        let exact_cancel = Cancel::new();
        let exact = render(
            &registry,
            source.input(),
            &stack,
            RenderOptions::exact(&exact_cancel),
            &context,
        )
        .unwrap();
        let stage = exact.proxy_window(
            &registry,
            &stack,
            exact
                .proxy_plan(ProxyBounds {
                    width: 120,
                    height: 80,
                })
                .unwrap(),
        );
        let plan = stage.plan();
        assert!(
            plan.window.is_some(),
            "the proxy must need a cold exact estimate"
        );
        let proxy = source.proxy(plan).unwrap();
        exact_cancel.cancel();
        let proxy_cancel = Cancel::new();
        let frame = exact
            .render_proxy(proxy.input(), stage, &proxy_cancel, &context)
            .unwrap()
            .frame(SnapshotId::new())
            .unwrap();
        assert!(frame.width > 0 && frame.height > 0);
    }

    #[test]
    fn windowed_proxy_transform_places_rotated_entry_origin_once() {
        let registry = ModuleRegistry::builtin();
        let context = RenderContext::new();
        let source = jpeg(1500, 1000);
        let stack = recipe(vec![crop(7.0, 0.42, 0.47, 0.14, 0.12)], Vec::new());
        let exact = exact(&context, &registry, &source, &stack);
        let stage = exact.proxy_window(
            &registry,
            &stack,
            exact
                .proxy_plan(ProxyBounds {
                    width: 150,
                    height: 110,
                })
                .unwrap(),
        );
        let plan = stage.plan();
        let window = plan.window.expect("tight rotated crop has a source window");
        let cut_source = source.proxy(plan).unwrap();
        let whole_source = source.proxy(plan.whole()).unwrap();
        let cut = exact
            .render_proxy(cut_source.input(), stage, &Cancel::never(), &context)
            .unwrap()
            .transform()
            .unwrap();
        let whole = render(
            &registry,
            whole_source.input(),
            &stack,
            RenderOptions::proxy(&Cancel::never()),
            &context,
        )
        .unwrap()
        .transform()
        .unwrap();
        assert_eq!(cut.output, whole.output);
        let map =
            |m: [f64; 6], x: f64, y: f64| (m[0] * x + m[1] * y + m[2], m[3] * x + m[4] * y + m[5]);
        for (x, y) in [
            (0.5, 0.5),
            (
                f64::from(window.width) / 2.0,
                f64::from(window.height) / 2.0,
            ),
            (
                f64::from(window.width) - 0.5,
                f64::from(window.height) - 0.5,
            ),
        ] {
            let actual = map(cut.forward, x, y);
            let expected = map(
                whole.forward,
                x + f64::from(window.x),
                y + f64::from(window.y),
            );
            assert!((actual.0 - expected.0).abs() < 1e-9, "x at {x},{y}");
            assert!((actual.1 - expected.1).abs() < 1e-9, "y at {x},{y}");
        }
    }

    /// The proxy source of a cropped stack is bounded by the display bounds and the stated margin,
    /// however tight the crop: a straight crop's window is its output, and a straightened one's is
    /// the box its output reads plus the taps' margin.
    #[test]
    fn a_tight_crops_proxy_source_is_bounded_by_the_display_not_the_crop() {
        let context = RenderContext::new();
        let registry = ModuleRegistry::builtin();
        let source = jpeg(1500, 1000);
        let bounds = ProxyBounds {
            width: 120,
            height: 90,
        };
        for tightness in [0.5, 0.25, 0.12, 0.09] {
            for angle in [0.0, 6.0] {
                let stack = recipe(
                    vec![crop(
                        angle,
                        0.5 - tightness / 2.0,
                        0.5 - tightness / 2.0,
                        tightness,
                        tightness,
                    )],
                    Vec::new(),
                );
                let exact = exact(&context, &registry, &source, &stack);
                let plan = exact.proxy_plan(bounds).expect("a proxy is worthwhile");
                let plan = exact.proxy_window(&registry, &stack, plan).plan();
                let (width, height) = plan.source_dimensions();
                // The output the proxy phase presents: the whole proxy stage's output stage.
                let whole = source.proxy(plan.whole()).unwrap();
                let (out_width, out_height) = render(
                    &registry,
                    whole.input(),
                    &stack,
                    RenderOptions::default(),
                    &context,
                )
                .unwrap()
                .stage();
                assert!(out_width <= bounds.width && out_height <= bounds.height);
                if angle == 0.0 {
                    assert_eq!(
                        (width, height),
                        (out_width, out_height),
                        "{tightness} straight: the window is the crop"
                    );
                } else {
                    // The box a rotated rectangle covers, plus each tap pair and its margin.
                    let (sin, cos) = angle.to_radians().sin_cos();
                    let reach = |a: u32, b: u32| {
                        (f64::from(a) * cos + f64::from(b) * sin).ceil() as u32
                            + 2 * (super::super::geometry::TAP_MARGIN + 2)
                    };
                    assert!(
                        width <= reach(out_width, out_height)
                            && height <= reach(out_height, out_width),
                        "{tightness} at {angle}: a {width}x{height} window for a \
                         {out_width}x{out_height} output"
                    );
                }
            }
        }
    }

    /// With a spatial operation the window holds the stage's own tiles around what the crop reads,
    /// so the windowed frame is byte for byte the whole proxy stage rendered with the estimates
    /// the window is handed — the exact stage's — on both pixel domains, masked and not. That is
    /// the approximation a spatial proxy already reports, and no other.
    #[test]
    fn a_windowed_spatial_proxy_is_the_whole_stage_with_the_exact_estimates() {
        let context = RenderContext::new();
        let registry = ModuleRegistry::builtin();
        let mask = gradient("Mask 1");
        let presence = json!({"clarity": 45, "dehaze": 30, "texture": 25});
        let stacks = [
            recipe(
                vec![
                    layer(BASIC_EFFECT, json!({"exposure": 0.2}), None),
                    layer(PRESENCE_EFFECT, presence.clone(), None),
                    crop(0.0, 0.72, 0.7, 0.16, 0.16),
                ],
                Vec::new(),
            ),
            recipe(
                vec![
                    layer(BASIC_EFFECT, json!({"contrast": 20}), Some(&mask)),
                    layer(PRESENCE_EFFECT, presence.clone(), Some(&mask)),
                    crop(3.0, 0.62, 0.66, 0.15, 0.15),
                ],
                vec![mask.clone()],
            ),
        ];
        let bounds = ProxyBounds {
            width: 240,
            height: 160,
        };
        for (domain, source) in [("jpeg", jpeg(2400, 1600)), ("raw", raw(2400, 1600))] {
            for (index, stack) in stacks.iter().enumerate() {
                let exact = exact(&context, &registry, &source, stack);
                let (plan, frame) =
                    windowed_frame(&context, &registry, &source, &exact, stack, bounds);
                let window = plan.window.expect("a tight crop's proxy is windowed");
                assert!(
                    window.width < plan.width && window.height < plan.height,
                    "{domain} {index}: {window:?} of {}x{}",
                    plan.width,
                    plan.height
                );
                // The whole proxy stage, with every spatial operation handed the exact stage's
                // estimates, as the window's operations are.
                let whole = source.proxy(plan.whole()).unwrap();
                let mut compiled = registry
                    .compile_sampled(
                        plan.width,
                        plan.height,
                        stack,
                        crate::mask_field::MaskSampling::ThinFeature,
                    )
                    .unwrap();
                for (segment, entry) in compiled.segments.iter_mut().enumerate() {
                    if let Some(Entry::Spatial { globals, .. }) = &mut entry.entry {
                        *globals = Some(Arc::new(exact.spatial_globals(segment).unwrap()));
                    }
                }
                let reference = Render::compiled(
                    whole.input(),
                    compiled,
                    RenderOptions::proxy(&Cancel::never()),
                    &context,
                )
                .unwrap();
                assert!(reference.approximation().spatial);
                let reference = reference.frame(SnapshotId::new()).unwrap();
                assert_eq!(
                    (frame.width, frame.height),
                    (reference.width, reference.height)
                );
                assert!(
                    frame.rgba == reference.rgba,
                    "{domain} {index}: the windowed spatial proxy differs from the whole stage"
                );
            }
        }
    }
}
