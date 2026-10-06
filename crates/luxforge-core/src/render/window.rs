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
//!   (`Tiling::Halo`, 512 or 1024 px by its summed halo). Every unit is tile invariant over tiles
//!   anchored at the stage origin (the [`crate::modules::SpatialUnit`] contract), and a window whose origin is
//!   on that grid holds exactly the stage's own tiles, so every pixel the next segment reads is the
//!   value the whole stage gives it. Its radii are the ones it was compiled with, against the whole
//!   proxy stage. Its mask is compiled against the whole stage and read at the window's offset. What
//!   the window cannot hold is a whole-stage reduction, so an operation that prepares a global
//!   estimate is handed the exact whole-stage estimate, including in the half-detail viewport.
//!   An operation with an estimate behind another spatial operation needs that operation's whole
//!   output to prepare it, so the segments before it are kept whole and it reduces its own input
//!   as a frame does: such a stack's window starts after it.
//!
//! Everything else — the colour arithmetic, the masks, the finish units of the last segment, the
//! quantization — is untouched, so a windowed proxy frame is byte for byte the proxy frame of the
//! whole proxy stage, which is itself the exact recipe over the exact downscale for stacks without
//! a spatial operation. With a spatial operation, its exact whole-stage estimate is retained. The tests below
//! and in `preview::tests` prove these contracts.

use super::{Compiled, Segment};
use crate::{
    Error,
    modules::{ExactGeometry, Global, Region, Stage},
};
use std::sync::Arc;

/// Why a requested output rectangle cannot be cut out of a stack, which a GPU plan reports as
/// unplannable with this reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegionFallback {
    Empty,
    PointReplacement,
    UnplannableGeometry,
}

impl RegionFallback {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Empty => "the requested viewport lies outside the output stage",
            Self::PointReplacement => "a point replacement cannot yet be cut to a viewport",
            Self::UnplannableGeometry => "the viewport cannot be mapped safely through the stack",
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
    /// Per segment, what is cut ([`SegmentCut`]).
    cuts: Vec<SegmentCut>,
}

/// Where one segment of a [`WindowPlan`] is cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SegmentCut {
    /// The rectangle of its output stage that is kept.
    output: Region,
    /// The rectangle of the whole stage it receives that its kept output reads through its exact
    /// geometry: inside what its boundary's cut frame holds, which a spatial operation grows by
    /// its halo and the CPU's tile grid.
    read: Region,
}

/// The stage segment `index` reads: the source for the first, or the whole stage the boundary
/// entering it produces ([`super::Entry::stage`]).
fn input_stage(segments: &[Segment], index: usize, source: Stage) -> Stage {
    match &segments[index].entry {
        None => source,
        Some(entry) => entry.stage(segments[index - 1].stage()),
    }
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
    /// Its original stage coordinates are kept in its cuts for positional finish units.
    pub(crate) fn of_rect(
        compiled: &Compiled,
        source: (u32, u32),
        requested: Region,
    ) -> Result<Self, RegionFallback> {
        Self::walk(compiled, source, requested, None)
    }

    /// [`Self::of_rect`] for a GPU preview's region whose boundary lies in segment `boundary`: the
    /// CPU renders the boundary through the segments up to it, which are planned as the CPU cuts
    /// them, and the GPU evaluates every boundary after it, each of whose windows is its output's
    /// grown by its halo alone ([`super::Entry::plan_gpu_window`]). A spatial step evaluates every
    /// pixel of the window it holds, so it needs no tile grid, which is the CPU's. Only
    /// [`Self::reads`] of segments up to `boundary` and [`Self::apply_through`] to it are the
    /// CPU's own cuts. A global estimate behind an earlier spatial layer is no refusal here: the
    /// boundary's render reads one before the boundary from the estimate store alone
    /// (`Render::region_boundary`), and the plan carries its own for those after it.
    pub(crate) fn of_gpu_rect(
        compiled: &Compiled,
        source: (u32, u32),
        requested: Region,
        boundary: usize,
    ) -> Result<Self, RegionFallback> {
        Self::walk(compiled, source, requested, Some(boundary))
    }

    /// The walk both plans share, back from the output: the entry of a segment after `gpu_after`
    /// is planned for the GPU, every other as the CPU cuts it.
    fn walk(
        compiled: &Compiled,
        source: (u32, u32),
        requested: Region,
        gpu_after: Option<usize>,
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
        let whole = Region::whole(segments[count - 1].stage());
        let mut cuts = vec![
            SegmentCut {
                output: whole,
                read: whole,
            };
            count
        ];
        // What the segment being walked must produce, in its output stage's coordinates.
        let mut needed = requested;
        let mut read_source = None;
        for index in (0..count).rev() {
            let segment = &segments[index];
            if needed.is_empty() {
                return Err(RegionFallback::UnplannableGeometry);
            }
            cuts[index].output = needed;
            let cut = needed != Region::whole(segment.stage());
            if cut && segment.has_pixels {
                return Err(RegionFallback::PointReplacement);
            }
            let input = input_stage(segments, index, source);
            let read = segment.geometry.unmap_region(needed);
            if read.x1() > input.width || read.y1() > input.height {
                return Err(RegionFallback::UnplannableGeometry);
            }
            cuts[index].read = read;
            match &segment.entry {
                None => read_source = Some(read),
                Some(entry) if gpu_after.is_some_and(|boundary| index > boundary) => {
                    needed = entry.plan_gpu_window(read, segments[index - 1].stage())?;
                }
                // An estimate behind an earlier spatial layer reads that layer's whole output, so
                // every segment before it is kept whole, a GPU preview's boundary's too.
                Some(entry) => {
                    needed = entry.plan_window(
                        read,
                        segments[index - 1].stage(),
                        compiled.spatial_before(index),
                    )?;
                }
            }
        }
        Ok(Self {
            source: read_source.ok_or(RegionFallback::UnplannableGeometry)?,
            cuts,
        })
    }

    /// The rectangle of the whole stage segment `segment` receives that its kept output reads: the
    /// source's window for the first segment, and for a later one the part of its boundary's cut
    /// frame inside that boundary's own margins, which a GPU preview's region boundary in it holds
    /// ([`Self::of_gpu_rect`]).
    pub(crate) fn reads(&self, segment: usize) -> Region {
        self.cuts[segment].read
    }

    /// [`Self::reads`], when it is less than the whole stage segment `segment` of `compiled`
    /// receives from a source of `source` dimensions; `None` when the segment reads all of it.
    pub(crate) fn received_cut(
        &self,
        compiled: &Compiled,
        source: (u32, u32),
        segment: usize,
    ) -> Option<Region> {
        let whole = input_stage(
            compiled.segments.get(..=segment)?,
            segment,
            Stage {
                width: source.0,
                height: source.1,
            },
        );
        let received = self.reads(segment);
        (received != Region::whole(whole)).then_some(received)
    }

    /// Rewrite `compiled`, the stack this plan was made from, to read a source of the window's
    /// dimensions and keep only each segment's window. `globals(index)` answers the estimates the
    /// spatial operation entering segment `index` is handed when its stage is cut and it prepares
    /// one; it is asked nothing otherwise.
    pub(crate) fn apply(
        &self,
        compiled: Compiled,
        source: (u32, u32),
        globals: impl FnMut(usize) -> Result<Vec<Option<Global>>, Error>,
    ) -> Result<Compiled, Error> {
        let last = compiled.segments.len().saturating_sub(1);
        self.apply_through(compiled, source, last, globals)
    }

    /// [`Self::apply`] to segments `0..=last` alone, the rest left as they were compiled: what a
    /// render that stops at segment `last` reads, such as a GPU preview's region boundary
    /// ([`Self::of_gpu_rect`]), whose later segments the GPU evaluates and no CPU frame of the cut
    /// compilation reaches. No estimate is asked for a segment past `last`.
    pub(crate) fn apply_through(
        &self,
        mut compiled: Compiled,
        source: (u32, u32),
        last: usize,
        mut globals: impl FnMut(usize) -> Result<Vec<Option<Global>>, Error>,
    ) -> Result<Compiled, Error> {
        let source = Stage {
            width: source.0,
            height: source.1,
        };
        if self.cuts.len() != compiled.segments.len() || last >= compiled.segments.len() {
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
        for (index, whole_input) in full_inputs.iter().copied().enumerate().take(last + 1) {
            // The window of the whole stage this segment reads: the source's, or the one its
            // boundary's cut frame holds ([`super::Entry::cut`]), read from the geometry the
            // segment composed before it is rebased below.
            let segment = &mut compiled.segments[index];
            let input = match &mut segment.entry {
                None => self.source,
                Some(entry) => {
                    let read = segment.geometry.unmap_region(self.cuts[index].output);
                    entry.cut(read, self.cuts[index - 1].output, whole_input, || {
                        globals(index)
                    })?
                }
            };
            if input != Region::whole(whole_input) {
                // Place the window at its origin in the whole stage, ahead of everything the
                // segment composed: an integer translation, so the composition stays exact.
                let place = ExactGeometry::crop(
                    -i64::from(input.x0),
                    -i64::from(input.y0),
                    whole_input.width,
                    whole_input.height,
                );
                segment.geometry = place.then(segment.geometry);
            }
            let kept = self.cuts[index].output;
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
        render::{Entry, Render, RenderContext, RenderOptions, render},
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
            width: 60,
            height: 44,
        };
        for (domain, source) in [("jpeg", jpeg(600, 400)), ("raw", raw(600, 400))] {
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

    /// A straightened crop behind a lens or perspective warp is fused into the warp's resample,
    /// whose segment then writes the crop's stage: its windowed proxy is cut as any other crop's,
    /// byte for byte the whole proxy stage's frame, tight or as wide as a 16:9 fit at 7°.
    #[test]
    fn a_crop_fused_into_a_warp_has_a_windowed_proxy() {
        let context = RenderContext::new();
        let registry = crate::render::warp_tests::registry();
        let lens = layer(
            "luxforge.lens.distortion",
            json!({"terms": [-0.079, 0.0, 0.0]}),
            None,
        );
        let perspective = layer(
            crate::PERSPECTIVE_EFFECT,
            json!({"horizontal": 20, "vertical": -10}),
            None,
        );
        let basic = layer(BASIC_EFFECT, json!({"exposure": 0.5, "contrast": 25}), None);
        let wide = Layer::crop(crate::render::tests::fitted_crop(
            600,
            400,
            7.0,
            [0.0, 0.0, 1.0, 1.0],
        ));
        let tight = crop(7.0, 0.42, 0.47, 0.14, 0.12);
        let bounds = ProxyBounds {
            width: 60,
            height: 44,
        };
        for (domain, source) in [("jpeg", jpeg(600, 400)), ("raw", raw(600, 400))] {
            for (name, warps) in [
                ("lens", vec![lens.clone()]),
                ("perspective", vec![perspective.clone()]),
                (
                    "lens and perspective",
                    vec![lens.clone(), perspective.clone()],
                ),
            ] {
                for (fit, crop) in [("tight", &tight), ("wide", &wide)] {
                    let mut layers = vec![basic.clone()];
                    layers.extend(warps.iter().cloned());
                    layers.push(crop.clone());
                    let stack = recipe(layers, Vec::new());
                    let exact = exact(&context, &registry, &source, &stack);
                    let fused = exact.compiled.segments.last().expect("a segment");
                    assert!(
                        fused.geometry.is_identity(fused.width, fused.height),
                        "{domain} {name} {fit}: the fused segment's geometry is its own stage's"
                    );
                    let (plan, frame) =
                        windowed_frame(&context, &registry, &source, &exact, &stack, bounds);
                    let window = plan
                        .window
                        .expect("a straightened crop's proxy is windowed");
                    if fit == "tight" {
                        assert!(
                            u64::from(window.width) * u64::from(window.height)
                                < u64::from(plan.width) * u64::from(plan.height) / 4,
                            "{domain} {name}: a {window:?} window of a {}x{} stage",
                            plan.width,
                            plan.height
                        );
                    }
                    let whole = whole_frame(&context, &registry, &source, &stack, plan);
                    assert_eq!(
                        (frame.width, frame.height),
                        (whole.width, whole.height),
                        "{domain} {name} {fit}"
                    );
                    assert!(
                        frame.rgba == whole.rgba,
                        "{domain} {name} {fit}: the windowed proxy frame differs from the whole \
                         stage's"
                    );
                }
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
        let source = jpeg(600, 400);
        let stack = recipe(vec![crop(7.0, 0.42, 0.47, 0.14, 0.12)], Vec::new());
        let exact = exact(&context, &registry, &source, &stack);
        let stage = exact.proxy_window(
            &registry,
            &stack,
            exact
                .proxy_plan(ProxyBounds {
                    width: 60,
                    height: 44,
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
            let actual = cut.to_output(x, y).unwrap();
            let expected = whole
                .to_output(x + f64::from(window.x), y + f64::from(window.y))
                .unwrap();
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
        let source = jpeg(600, 400);
        let bounds = ProxyBounds {
            width: 48,
            height: 36,
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
        // The proxy stage must be wider than one 512 px spatial tile for a window of its tiles to
        // be smaller than the stage.
        let bounds = ProxyBounds {
            width: 120,
            height: 80,
        };
        for (domain, source) in [("jpeg", jpeg(1200, 800)), ("raw", raw(1200, 800))] {
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
                        plan.width,
                        plan.height,
                        stack,
                        crate::mask_field::MaskSampling::ThinFeature,
                    )
                    .unwrap();
                for (segment, entry) in compiled.segments.iter_mut().enumerate() {
                    if let Some(Entry::Spatial(spatial)) = &mut entry.entry {
                        spatial.globals = Some(Arc::new(exact.spatial_globals(segment).unwrap()));
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

    /// A window runs each spatial operation over its window as its own stage, whose edge blocks
    /// are not the whole stage's, so it neither reads nor fills the store of reduced planes: not
    /// in a context that holds nothing, and not in one that holds the whole proxy stage's planes,
    /// where it writes the same bytes and leaves every counter as it was.
    #[test]
    fn a_windowed_render_neither_reads_nor_fills_the_reduced_planes() {
        let registry = ModuleRegistry::builtin();
        let bounds = ProxyBounds {
            width: 120,
            height: 80,
        };
        for presence in [json!({"dehaze": 30}), json!({"clarity": 45})] {
            let stack = recipe(
                vec![
                    layer(PRESENCE_EFFECT, presence.clone(), None),
                    crop(0.0, 0.72, 0.7, 0.16, 0.16),
                ],
                Vec::new(),
            );
            for (domain, source) in [("jpeg", jpeg(1200, 800)), ("raw", raw(1200, 800))] {
                let case = format!("{presence} {domain}");
                let empty = RenderContext::new();
                let exact_render = exact(&empty, &registry, &source, &stack);
                let (plan, cold) =
                    windowed_frame(&empty, &registry, &source, &exact_render, &stack, bounds);
                assert!(plan.window.is_some(), "{case}: a tight crop is windowed");
                let counts = empty.reduced().counts();
                assert_eq!(
                    (
                        counts.render_hits + counts.render_misses,
                        counts.tile_hits + counts.tile_misses,
                        counts.publishes,
                    ),
                    (0, 0, 0),
                    "{case}: the window neither looks nor fills"
                );

                let held = RenderContext::new();
                whole_frame(&held, &registry, &source, &stack, plan);
                let before = held.reduced().counts();
                assert_eq!(before.publishes, 1, "{case}: the whole proxy stage fills");
                let exact_render = exact(&held, &registry, &source, &stack);
                let (_, warm) =
                    windowed_frame(&held, &registry, &source, &exact_render, &stack, bounds);
                assert_eq!(held.reduced().counts(), before, "{case}: nothing read");
                assert_eq!(warm.rgba, cold.rgba, "{case}");
            }
        }
    }
}
