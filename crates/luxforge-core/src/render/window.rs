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
//!   that crop, so its mask is read at the same stage pixel as before. A segment holding a
//!   positional unit (a finish-stage layer, whose units read the output coordinates they are handed)
//!   or a point replacement is never cut, and a stack that would need it has no window.
//! - **A resample** reads the previous segment's window: its continuous input coordinate is
//!   translated by the window's integer origin after the resample's own arithmetic, which is exact in
//!   `f64` (an integer subtracted from a non-negative coordinate below 2^52 is representable), so
//!   every tap is the same pixel with the same weight. The window holds every tap the output reads
//!   plus [`TAP_MARGIN`] pixels, or reaches the stage edge where a tap is clamped to it.
//! - **A spatial operation** runs over the previous segment's window as its own stage: the rectangle
//!   the next segment reads, grown by the operation's summed halo and clamped to the stage, with its
//!   origin moved down to the [`SPATIAL_TILE`] grid. Every unit is tile invariant over tiles anchored
//!   at the stage origin (the [`crate::modules::SpatialUnit`] contract), and a window whose origin is
//!   on that grid holds exactly the stage's own tiles, so every pixel the next segment reads is the
//!   value the whole stage gives it. Its radii are the ones it was compiled with, against the whole
//!   proxy stage. Its mask is compiled against the whole stage and read at the window's offset. What
//!   the window cannot hold is a whole-stage reduction, so an operation that prepares a global
//!   estimate is handed the estimate prepared for the **exact** stage, which the exact phase of the
//!   same job reads from the store; that is part of why a spatial proxy frame is approximate, which it
//!   already reports. An operation with an estimate behind another spatial operation would need the
//!   exact render of the first to prepare it, so such a stack has no window.
//!
//! Everything else — the colour arithmetic, the masks, the finish units of the last segment, the
//! quantization — is untouched, so a windowed proxy frame is byte for byte the proxy frame of the
//! whole proxy stage, which is itself the exact recipe over the exact downscale (for a stack without
//! a spatial operation; with one, the same frame as the whole stage rendered with the exact stage's
//! estimates). The tests below and in `preview::tests` prove both.

use super::{Compiled, Entry, ExactGeometry, Segment};
use crate::{
    Error,
    modules::{Global, Region, SPATIAL_TILE, SpatialOperation, Stage},
};
use std::sync::Arc;

/// Pixels kept beyond the taps a resample's output reads, on every side: a bilinear tap reads the
/// pixel at `floor(u - ½)` and the one after it, and the corners of an affine image bound every
/// interior coordinate only up to rounding, which this covers with room to spare.
pub(crate) const TAP_MARGIN: u32 = 2;

/// The estimates one spatial operation is handed instead of reducing its own stage.
pub(crate) type Globals = Arc<Vec<Option<Global>>>;

/// Where a stack compiled against a whole proxy stage is cut down to what its output reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowPlan {
    /// The rectangle of the whole proxy source the first segment reads.
    pub(crate) source: Region,
    /// Per segment, the rectangle of its output stage that is kept. The last segment's is always
    /// its whole output.
    outputs: Vec<Region>,
}

fn whole(stage: Stage) -> Region {
    Region {
        x0: 0,
        y0: 0,
        width: stage.width,
        height: stage.height,
    }
}

fn output_stage(segment: &Segment) -> Stage {
    Stage {
        width: segment.width,
        height: segment.height,
    }
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
        Some(Entry::Spatial { .. }) => output_stage(&segments[index - 1]),
    }
}

/// Whether any unit of `operation` prepares a global estimate from a reduction of its stage.
fn prepares_estimates(operation: &SpatialOperation) -> bool {
    operation
        .units()
        .iter()
        .any(|unit| unit.estimate_key().is_some())
}

/// The rectangle of `input` the taps of `resample` read over the output rectangle `read`, with
/// [`TAP_MARGIN`] on every side, clamped to `input`, or `None` when a corner maps to a coordinate
/// that is not finite.
fn tap_region(resample: super::Resample, read: Region, input: Stage) -> Option<Region> {
    let mut low = [f64::INFINITY; 2];
    let mut high = [f64::NEG_INFINITY; 2];
    for (x, y) in [
        (read.x0, read.y0),
        (read.x1() - 1, read.y0),
        (read.x0, read.y1() - 1),
        (read.x1() - 1, read.y1() - 1),
    ] {
        let (u, v) = resample.input_at(x, y);
        for (axis, value) in [u, v].into_iter().enumerate() {
            let index = (value - 0.5).floor();
            if !index.is_finite() {
                return None;
            }
            low[axis] = low[axis].min(index);
            high[axis] = high[axis].max(index + 1.0);
        }
    }
    let margin = f64::from(TAP_MARGIN);
    let clamp = |value: f64, limit: u32| value.max(0.0).min(f64::from(limit)) as u32;
    let (x0, x1) = (
        clamp(low[0] - margin, input.width),
        clamp(high[0] + margin + 1.0, input.width),
    );
    let (y0, y1) = (
        clamp(low[1] - margin, input.height),
        clamp(high[1] + margin + 1.0, input.height),
    );
    (x1 > x0 && y1 > y0).then_some(Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

impl WindowPlan {
    /// The windows of `compiled`, a stack compiled against a whole proxy source of `source`
    /// dimensions, or `None` when the stack reads the whole source anyway or cannot be cut: a
    /// positional unit or a point replacement in a segment whose output would be cut, or a spatial
    /// operation with a global estimate behind another spatial operation. `O(segments)`, and reads
    /// no pixel.
    pub(crate) fn of(compiled: &Compiled, source: (u32, u32)) -> Option<Self> {
        let segments = &compiled.segments;
        let source = Stage {
            width: source.0,
            height: source.1,
        };
        let count = segments.len();
        let mut outputs = vec![whole(output_stage(&segments[count - 1])); count];
        // What the segment being walked must produce, in its output stage's coordinates.
        let mut needed = outputs[count - 1];
        let mut read_source = None;
        for index in (0..count).rev() {
            let segment = &segments[index];
            if needed.is_empty() {
                return None;
            }
            outputs[index] = needed;
            let cut = needed != whole(output_stage(segment));
            if cut && (segment.positional || segment.has_pixels) {
                return None;
            }
            let input = input_stage(segments, index, source);
            let read = segment.geometry.unmap_region(needed);
            if read.x1() > input.width || read.y1() > input.height {
                return None;
            }
            match &segment.entry {
                None => read_source = Some(read),
                Some(Entry::Spatial { operation, .. }) => {
                    if prepares_estimates(operation) && compiled.spatial_before(index) {
                        return None;
                    }
                    let grown = read.grown(operation.summed_halo(input), input);
                    let x0 = grown.x0 / SPATIAL_TILE * SPATIAL_TILE;
                    let y0 = grown.y0 / SPATIAL_TILE * SPATIAL_TILE;
                    needed = Region {
                        x0,
                        y0,
                        width: grown.x1() - x0,
                        height: grown.y1() - y0,
                    };
                }
                Some(Entry::Resample(resample)) => {
                    needed = tap_region(*resample, read, output_stage(&segments[index - 1]))?;
                }
            }
        }
        let read_source = read_source?;
        (read_source != whole(source)).then_some(Self {
            source: read_source,
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
        for index in 0..compiled.segments.len() {
            // The stage this segment reads before and after the cut, and where the kept part of it
            // lies in the whole stage.
            let whole_input = input_stage(&compiled.segments, index, source);
            let (window, input) = match &compiled.segments[index].entry {
                None => (Some(self.source), self.source),
                Some(Entry::Spatial { .. }) => {
                    let window = self.outputs[index - 1];
                    (Some(window), window)
                }
                Some(Entry::Resample(_)) => (None, whole(whole_input)),
            };
            if index > 0 {
                let previous = self.outputs[index - 1];
                let segment = &mut compiled.segments[index];
                match &mut segment.entry {
                    Some(Entry::Resample(_)) => {
                        segment.entry_origin = (previous.x0, previous.y0);
                    }
                    Some(Entry::Spatial {
                        operation,
                        globals: handed,
                        ..
                    }) if previous != whole(whole_input) => {
                        if let Some(mask) = operation.mask() {
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
                && window != whole(whole_input)
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
            if kept != whole(output_stage(segment)) {
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
        BASIC_EFFECT, Component, ComponentMode, CropPayload, EFFECT_FORMAT, Layer, LayerId,
        LinearImage, LinearSettings, Mask, ModuleRegistry, Orientation, PRESENCE_EFFECT,
        PreviewSource, ProxyBounds, ProxyPlan, RECIPE_FORMAT, Recipe, SnapshotId, SourceImage,
        VIGNETTE_EFFECT,
        render::{Cancel, Render, RenderOptions, render, testing::context},
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

    /// The exact compilation a preview job starts from.
    fn exact<'a>(
        registry: &ModuleRegistry,
        source: &'a PreviewSource,
        stack: &Recipe,
    ) -> Render<'a> {
        render(
            registry,
            source.input(),
            stack,
            RenderOptions::default(),
            context(),
        )
        .expect("the exact compilation")
    }

    /// What the preview worker does for one job after its exact compilation: the fitted plan, its
    /// window, the windowed proxy source and the frame rendered against it.
    fn windowed_frame(
        registry: &ModuleRegistry,
        source: &PreviewSource,
        exact: &Render<'_>,
        stack: &Recipe,
        bounds: ProxyBounds,
    ) -> (ProxyPlan, crate::Raster) {
        let plan = exact.proxy_plan(bounds).expect("a proxy is worthwhile");
        let plan = exact.proxy_window(registry, stack, plan);
        let proxy = source.proxy(plan).expect("the windowed proxy source");
        assert_eq!(proxy.dimensions(), plan.source_dimensions());
        let frame = exact
            .render_proxy(
                registry,
                proxy.input(),
                stack,
                plan,
                &Cancel::never(),
                context(),
            )
            .expect("the windowed compilation")
            .frame(SnapshotId::new())
            .expect("the windowed proxy frame");
        (plan, frame)
    }

    /// The whole proxy stage's frame: the exact recipe over the exact downscale, which is what the
    /// proxy phase has always been proven to be.
    fn whole_frame(
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
            context(),
        )
        .expect("the whole proxy stage")
        .frame(SnapshotId::new())
        .expect("the whole proxy frame")
    }

    /// Stacks without a spatial operation: the windowed proxy frame is byte for byte the frame of
    /// the whole proxy stage — the exact recipe over the exact downscale — for a straight and a
    /// straightened tight crop, behind an orientation, a masked colour layer and a finish layer,
    /// on both pixel domains.
    #[test]
    fn a_windowed_proxy_frame_is_the_whole_proxy_stages_frame() {
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
                let exact = exact(&registry, &source, stack);
                let (plan, frame) = windowed_frame(&registry, &source, &exact, stack, bounds);
                let window = plan.window.expect("a tight crop's proxy is windowed");
                assert!(
                    u64::from(window.width) * u64::from(window.height)
                        < u64::from(plan.width) * u64::from(plan.height) / 4,
                    "{domain} {name}: a {window:?} window of a {}x{} stage",
                    plan.width,
                    plan.height
                );
                let whole = whole_frame(&registry, &source, stack, plan);
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

    /// The proxy source of a cropped stack is bounded by the display bounds and the stated margin,
    /// however tight the crop: a straight crop's window is its output, and a straightened one's is
    /// the box its output reads plus the taps' margin.
    #[test]
    fn a_tight_crops_proxy_source_is_bounded_by_the_display_not_the_crop() {
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
                let exact = exact(&registry, &source, &stack);
                let plan = exact.proxy_plan(bounds).expect("a proxy is worthwhile");
                let plan = exact.proxy_window(&registry, &stack, plan);
                let (width, height) = plan.source_dimensions();
                // The output the proxy phase presents: the whole proxy stage's output stage.
                let whole = source.proxy(plan.whole()).unwrap();
                let (out_width, out_height) = render(
                    &registry,
                    whole.input(),
                    &stack,
                    RenderOptions::default(),
                    context(),
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
                            + 2 * (TAP_MARGIN + 2)
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
                let exact = exact(&registry, &source, stack);
                let (plan, frame) = windowed_frame(&registry, &source, &exact, stack, bounds);
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
                    context(),
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
