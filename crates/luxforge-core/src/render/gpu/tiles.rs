//! A pixel read and an export's output stage as the GPU draws them (`docs/design/gpu-first.md`,
//! stage 4): the plan of the one tile a read of a stage renders, and the plan of an export's output
//! stage in tiles, which the desktop's tile worker draws on a device of its own and reads back
//! ([`crate::tiles`]). Planned on that worker's side, never on the catalog owner: one compile and
//! one plan of the stack, `O(layers)`, and a window for each tile, `O(tiles × segments)`. No pixel
//! is read, and nothing here names a GPU crate.
//!
//! - **One plan, from the source.** A read of the output stage, and every tile of a stream, draws
//!   the whole stack's plan at the exact stage from the source, every layer in the CPU's shape, as
//!   each tile of the picture at rest does ([`super::RestTiles`]). A read of the stage layer `l`
//!   receives draws the plan of the layers before it, likewise, over the stage they produce: what
//!   the neutral picker, a colour-limited stroke's seed and `mask.sample-input` read. A RAW
//!   source's plan takes the linear path.
//! - **Anchored tiles.** A tile is a rectangle of its stage at full scale, drawn over the window of
//!   the source its halos read ([`WindowPlan::of_gpu_rect`]), that window's origin moved to the
//!   plan's anchor ([`GpuPlan::anchor`]), so every pixel of it is the pixel any other window of the
//!   stage draws, the photo surface's at 100% included: a read of the output stage is the byte on
//!   screen there, and a stream's tiles carry no seam. A stream's tiles are the picture at rest's
//!   at full scale ([`super::preview::plan_rest_tiles`]), row by row from the stage's origin, every one the
//!   same plan with only its rectangle and window varying. The picture at rest's own planner
//!   cannot be called for them, since it plans a Fit view's reduction and picks its side by the
//!   photo surface's budget, so the tiling is written again here from the same parts: the plan
//!   from the source, its anchor, the region planner's window of each tile.
//! - **Lights.** A spatial operation's global estimate, Dehaze's atmospheric light, is read from
//!   the light its plan's light link computes from the whole stage at full resolution
//!   ([`super::GpuLight`]), whatever window a tile reads: the plan carries its lights, as the
//!   picture at rest's does, and the tile worker's runner computes each once from the source
//!   before the tiles that read it. A light behind a spatial layer reads that layer's exact output
//!   over the whole stage, which only a staged sweep writes ([`super::GpuLightInput::Stage`]): an
//!   export reading one is streamed in staged sweeps ([`plan_stream_sweeps`]), its light reduced
//!   from the stage texture the sweep before it wrote, and a read's tile reads it as the tile
//!   worker's staged sweeps of the stack computed and kept it; an export or a read whose sweeps do
//!   not fit is the reference's (`light-stage`), never drawn with that layer left out.
//! - **The answer.** The output stage's codes are read back as the GPU's output quantizer gives
//!   them, the picture's own bytes. Every other read is read back as the linear values before that
//!   quantizer, and its codes are quantized from them by the core's own quantizer
//!   ([`TilePlan::answer`]), so a colour-limited stroke's seed is the code of the mask input the
//!   same tile gives. On the byte path the output stage and a stage handed to a spatial layer are
//!   encoded, so their linear values are clamped to `[0, 1]` as an encoded value is; a colour layer
//!   receives its colour run's unclamped value, and the linear path clamps nothing.
//! - **What the GPU cannot draw** answers its reason ([`TileFallback::Plan`]), and the reference
//!   renders the read or the export: the plan's own fallback (`pixel-stage`, `no-program`,
//!   `disabled-program`, ...), and as `unplannable` a stack that does not
//!   compile, a stack the window planner cannot cut and a source drafted under an approximate
//!   white balance, which the GPU source does not hold.
use super::{
    GpuAnchor, GpuAnswer, GpuFallback, GpuGeometry, GpuPlan, GpuPlanRequest, RestTile, anchored,
};
use crate::{
    BoundaryFormat, Error, Evaluation, PreviewSource, ProxyIdentity, Recipe,
    colour::srgb::quantize_pixel,
    editor::prefix,
    modules::{Region, Stage},
    render::{Compiled, window::WindowPlan},
    tiles::{MaskInputMode, ReadPixels, ReadStage, ReadValues, TileFallback, clipped},
};
use std::borrow::Cow;

/// The sides an export stream's tiles take, longest first. The desktop's tile worker draws a
/// stream at the first whose every tile its runner holds within its budget, less what a read
/// waiting beside the stream may take: fixed for the stream from these, the plan and the device's
/// own figures, never from what is in use.
pub const STREAM_TILE_SIDES: [u32; 3] = [2048, 1024, 512];

/// The one tile a read of a stage renders on the GPU ([`plan_read`]).
#[derive(Clone, Debug, PartialEq)]
pub struct TilePlan {
    /// The stage read.
    pub stage: ReadStage,
    /// Its size: the output stage's, or the one the layers before the read layer produce.
    pub size: Stage,
    /// The plan of the stack, or of the layers before the read layer, from the source at the exact
    /// stage.
    pub plan: Box<GpuPlan>,
    /// The tile drawn: the rectangle asked for, clipped to the stage, and the window of the source
    /// it reads, anchored. Both are empty where the rectangle misses the stage, and the read reads
    /// nothing.
    pub tile: RestTile,
    /// The source the tile's boundary is cut from, and how its texels are held.
    pub source: ProxyIdentity,
    pub format: BoundaryFormat,
}

impl TilePlan {
    /// A lens warp's geometry tail, whose grid of the whole stage at one display pixel an output
    /// pixel the tile takes its part of; `None` for an affine or projective tail.
    pub fn warp(&self) -> Option<&GpuGeometry> {
        warp(&self.plan)
    }

    /// Whether the GPU reads the tile back as codes for a read of `values`: only for the output
    /// stage's codes, the picture's own bytes. Every other read is read back as the linear values
    /// before the output quantizer, which [`Self::answer`] answers it from.
    pub fn reads_codes(&self, values: ReadValues) -> bool {
        matches!((self.stage, values), (ReadStage::Output, ReadValues::Codes))
    }

    /// A read of `values` answered from `linear`, linear values the GPU read back for pixels of the
    /// tile, row by row: clamped to `[0, 1]` where the byte path encodes the stage — its output
    /// stage, and the stage it hands a spatial layer — and as codes quantized by the core's own
    /// output quantizer, so a stage's codes are always the codes of its linear values.
    pub fn answer(
        &self,
        values: ReadValues,
        linear: impl IntoIterator<Item = [f32; 3]>,
    ) -> ReadPixels {
        let encoded = !self.plan.linear
            && !matches!(
                self.stage,
                ReadStage::Before {
                    mode: MaskInputMode::ColourRun,
                    ..
                }
            );
        let linear = linear.into_iter().map(move |value| {
            if encoded {
                value.map(|channel| channel.clamp(0.0, 1.0))
            } else {
                value
            }
        });
        match values {
            ReadValues::Linear => ReadPixels::Linear(linear.collect()),
            ReadValues::Codes => ReadPixels::Codes(
                linear
                    .map(|value| {
                        let [red, green, blue] = quantize_pixel(value);
                        [red, green, blue, 255]
                    })
                    .collect(),
            ),
        }
    }
}

/// An export's output stage in tiles of one side at full scale ([`plan_stream`]).
#[derive(Clone, Debug, PartialEq)]
pub struct StreamPlan {
    /// The plan of the whole stack from the source at the exact stage: every tile's plan, which a
    /// tile's rectangle and window place.
    pub plan: Box<GpuPlan>,
    /// The tiles, row by row from the stage's origin: each its rectangle of the output stage, `side`
    /// pixels a side but at the stage's right and bottom edges, and the window of the source it
    /// reads, anchored.
    pub tiles: Vec<RestTile>,
    /// The output stage the tiles cover.
    pub output: Stage,
    pub side: u32,
    /// The source every tile's boundary is cut from, and how its texels are held.
    pub source: ProxyIdentity,
    pub format: BoundaryFormat,
}

impl StreamPlan {
    /// A lens warp's geometry tail, whose grid of the whole output stage at one display pixel an
    /// output pixel every tile takes its part of; `None` for an affine or projective tail.
    pub fn warp(&self) -> Option<&GpuGeometry> {
        warp(&self.plan)
    }

    /// The tiles of each band of the output stage, top to bottom: a band is one row of tiles,
    /// `side` rows tall but the last.
    pub fn bands(&self) -> impl Iterator<Item = &[RestTile]> {
        self.tiles
            .chunk_by(|tile, next| tile.rect.y0 == next.rect.y0)
    }
}

fn warp(plan: &GpuPlan) -> Option<&GpuGeometry> {
    plan.geometry.needs_grid().then_some(&plan.geometry)
}

/// The tile a read of `rect` of `stage` renders on the GPU: the stage's plan from the source, the
/// rectangle clipped to the stage, and the window of the source it reads, anchored ([`TilePlan`]).
/// The reason the reference renders the read instead, when the GPU cannot draw it. One compile of
/// the stack, or of the layers before the read layer, and one window: `O(layers + segments)`, no
/// pixel read.
pub fn plan_read(
    evaluation: &Evaluation,
    stage: ReadStage,
    rect: Region,
) -> Result<TilePlan, TileFallback> {
    let planned = Planned::of(evaluation, stage)?;
    let tile = planned.tile(rect, planned.plan.anchor())?;
    Ok(TilePlan {
        stage,
        size: planned.size,
        tile,
        source: planned.source.identity(),
        format: BoundaryFormat::of(planned.plan.linear),
        plan: planned.plan,
    })
}

/// `evaluation`'s output stage for an export in tiles of `side` pixels at full scale, row by row
/// ([`StreamPlan`]), or the reason the reference renders the export instead. One compile and one
/// plan of the stack and a window for each tile: `O(layers + tiles × segments)`, no pixel read.
pub fn plan_stream(evaluation: &Evaluation, side: u32) -> Result<StreamPlan, TileFallback> {
    PreparedStream::of(evaluation)?.stream(side)
}

/// One exact source-derived plan, shared across strategy and tile-side selection for this stream.
/// Its evaluation shares the bound compilation/source; it belongs only to the active request,
/// never a desktop cache. A differently sampled, reduced or prefixed request prepares separately.
pub struct PreparedStream {
    evaluation: Evaluation,
    plan: Box<GpuPlan>,
    output: Stage,
    full: Stage,
}

impl PreparedStream {
    pub fn of(evaluation: &Evaluation) -> Result<Self, TileFallback> {
        let planned = Planned::of(evaluation, ReadStage::Output)?;
        Ok(Self {
            evaluation: evaluation.clone(),
            plan: planned.plan,
            output: planned.size,
            full: planned.full,
        })
    }

    /// Another tile grid from the same compilation and GPU plan; no source preparation or plan.
    pub fn stream(&self, side: u32) -> Result<StreamPlan, TileFallback> {
        let (output, side, anchor) = (self.output, side.max(1), self.plan.anchor());
        let compiled = self
            .evaluation
            .compiled()
            .map_err(|error| unplannable(error.detail))?;
        let mut tiles = Vec::with_capacity(
            output.width.div_ceil(side) as usize * output.height.div_ceil(side) as usize,
        );
        for y0 in (0..output.height).step_by(side as usize) {
            for x0 in (0..output.width).step_by(side as usize) {
                let rect = Region {
                    x0,
                    y0,
                    width: side.min(output.width - x0),
                    height: side.min(output.height - y0),
                };
                tiles.push(tile(compiled, self.full, output, rect, anchor)?);
            }
        }
        Ok(StreamPlan {
            tiles,
            output,
            side,
            source: self.evaluation.source().identity(),
            format: BoundaryFormat::of(self.plan.linear),
            plan: self.plan.clone(),
        })
    }

    /// Staging and, when chained, any required light sweeps selected from the same plan.
    pub fn strategy(
        &self,
        budget: u64,
        sides: &[u32],
    ) -> Result<(super::GpuStaging, Vec<super::GpuLightSweep>), TileFallback> {
        let request = self.request(budget, sides)?;
        let staging = super::sweeps::plan_sweeps(&request);
        let lights = if matches!(staging, super::GpuStaging::Chained(_)) {
            self.light_sweeps(budget, sides)?
        } else {
            Vec::new()
        };
        Ok((staging, lights))
    }

    pub fn light_sweeps(
        &self,
        budget: u64,
        sides: &[u32],
    ) -> Result<Vec<super::GpuLightSweep>, TileFallback> {
        let request = self.request(budget, sides)?;
        super::sweeps::plan_light_sweeps(&request).map_err(|chained| {
            TileFallback::Plan(
                super::preview::staged_light_fallback(&self.plan, &chained)
                    .unwrap_or_else(|| GpuFallback::Unplannable("no staged light".into())),
            )
        })
    }

    fn request<'a>(
        &'a self,
        budget: u64,
        sides: &'a [u32],
    ) -> Result<super::sweeps::SweepRequest<'a>, TileFallback> {
        Ok(super::sweeps::SweepRequest {
            compiled: self
                .evaluation
                .compiled()
                .map_err(|error| unplannable(error.detail))?,
            source: (self.full.width, self.full.height),
            plan: &self.plan,
            format: BoundaryFormat::of(self.plan.linear),
            sides,
            budget: Some(budget),
            order: super::sweeps::TileOrder::Rows,
        })
    }
}

/// `evaluation`'s output stage for an export in staged sweeps ([`super::GpuSweeps`]), each sweep at
/// the longest of [`STREAM_TILE_SIDES`] whose middle tile's slot and light links fit `budget`
/// beside the stage textures, its tiles row by row so the last sweep's bands are its rows; or why
/// the export is drawn chained ([`plan_stream`]'s tiles). `budget` is what the tile worker's tiles
/// may take on its device, its read reserve already set aside. The reason the reference renders
/// the export instead, as [`plan_stream`] names it. `O(layers + sweeps × sides × segments + tiles ×
/// segments)`, no pixel read.
pub fn plan_stream_sweeps(
    evaluation: &Evaluation,
    budget: u64,
) -> Result<super::GpuStaging, TileFallback> {
    plan_stream_sweeps_at(evaluation, budget, &STREAM_TILE_SIDES)
}

/// [`plan_stream_sweeps`] trying `sides`, longest first, in place of [`STREAM_TILE_SIDES`]: what
/// a test that draws a small stage in several tiles a sweep asks for.
pub fn plan_stream_sweeps_at(
    evaluation: &Evaluation,
    budget: u64,
    sides: &[u32],
) -> Result<super::GpuStaging, TileFallback> {
    PreparedStream::of(evaluation)?
        .strategy(budget, sides)
        .map(|(staging, _)| staging)
}

/// Light sweeps for one exact request, without keeping any stage texture.
pub fn plan_stream_light_sweeps(
    evaluation: &Evaluation,
    budget: u64,
    sides: &[u32],
) -> Result<Vec<super::GpuLightSweep>, TileFallback> {
    PreparedStream::of(evaluation)?.light_sweeps(budget, sides)
}

/// A stage of an evaluation planned for the GPU: its compilation, the plan of it from the source,
/// its size and the source's.
struct Planned<'a> {
    compiled: Cow<'a, Compiled>,
    plan: Box<GpuPlan>,
    size: Stage,
    /// The source's full content stage, which every window is a rectangle of.
    full: Stage,
    source: &'a PreviewSource,
}

/// The reference renders it, for `reason`, under the plan's `unplannable`.
fn unplannable(reason: impl Into<String>) -> TileFallback {
    TileFallback::Plan(GpuFallback::Unplannable(reason.into()))
}

impl<'a> Planned<'a> {
    /// `stage` of `evaluation`: the stack's own compilation and plan for the output stage, or the
    /// layers before the read layer compiled once and planned, as the GPU plan compiles them, with
    /// the lights it reads.
    fn of(evaluation: &'a Evaluation, stage: ReadStage) -> Result<Self, TileFallback> {
        #[cfg(test)]
        preparation_count::note();
        let source = evaluation.source();
        if source.approximate_white_balance() {
            return Err(unplannable(
                "the source is drafted under an approximate white balance, which the GPU source \
                 does not hold",
            ));
        }
        let (registry, recipe) = (evaluation.registry(), evaluation.recipe());
        let (width, height) = source.dimensions();
        let full = Stage { width, height };
        let refused = |error: Error| unplannable(error.detail);
        let (compiled, before) = match stage {
            ReadStage::Output => (Cow::Borrowed(evaluation.compiled().map_err(refused)?), None),
            ReadStage::Before { layer, .. } => {
                let before = Recipe {
                    layers: prefix(&recipe.layers, layer).map_err(refused)?.to_vec(),
                    ..recipe.clone()
                };
                let compiled = registry.compile(width, height, &before).map_err(refused)?;
                (Cow::Owned(compiled), Some(before))
            }
        };
        let linear = matches!(source, PreviewSource::Raw { .. });
        let request = GpuPlanRequest::exact(0, full).from_source();
        let request = if linear { request.linear() } else { request };
        let planned = before.as_ref().unwrap_or(recipe);
        let answer = compiled
            .gpu_plan(
                0,
                full,
                None,
                super::plan::Planning {
                    linear,
                    source: true,
                    ..super::plan::Planning::default()
                },
            )
            .map_err(refused)?;
        let plan = match super::plan::with_lights(registry, planned, request, &compiled, answer) {
            Ok(GpuAnswer::Plan(plan)) => plan,
            Ok(GpuAnswer::Fallback(reason)) => return Err(TileFallback::Plan(reason)),
            Err(error) => return Err(refused(error)),
        };
        Ok(Self {
            size: compiled.stage(),
            compiled,
            plan,
            full,
            source,
        })
    }

    /// The tile of `rect`: the rectangle clipped to the stage, and the window of the source it
    /// reads, as the region planner plans a GPU region from the source, moved to `anchor`. Empty,
    /// window and all, where the rectangle misses the stage.
    fn tile(&self, rect: Region, anchor: GpuAnchor) -> Result<RestTile, TileFallback> {
        tile(&self.compiled, self.full, self.size, rect, anchor)
    }
}

fn tile(
    compiled: &Compiled,
    full: Stage,
    size: Stage,
    rect: Region,
    anchor: GpuAnchor,
) -> Result<RestTile, TileFallback> {
    let rect = clipped(rect, size);
    if rect.is_empty() {
        return Ok(RestTile {
            rect,
            window: Region::EMPTY,
        });
    }
    let windows =
        WindowPlan::of_gpu_rect(compiled, (full.width, full.height), rect).map_err(|reason| {
            unplannable(format!(
                "the tile at ({}, {}): {}",
                rect.x0,
                rect.y0,
                reason.reason()
            ))
        })?;
    Ok(RestTile {
        rect,
        window: anchored(windows.reads(0), anchor),
    })
}

#[cfg(test)]
mod preparation_count {
    use std::cell::Cell;
    thread_local! { static PREPARED: Cell<u64> = const { Cell::new(0) }; }
    pub(super) fn note() {
        PREPARED.with(|count| count.set(count.get() + 1));
    }
    pub(super) fn take() -> u64 {
        PREPARED.with(|count| count.replace(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_plan;
    use crate::{
        AssetId, BASIC_EFFECT, CropPayload, DETAIL_EFFECT, EntryId, HistoryEntry, Layer,
        LinearSettings, MIXER_EFFECT, ModuleRegistry, PRESENCE_EFFECT, ProxyBounds, RenderContext,
        Snapshot, VIGNETTE_EFFECT, WhiteBalanceApproximation,
        render::tests::{gradient, varied},
    };
    use serde_json::json;
    use std::sync::Arc;

    /// `recipe` over `source`, bound for evaluation as the catalog owner binds a saved entry's
    /// stack, on `context`.
    fn evaluation(context: &RenderContext, source: &PreviewSource, recipe: &Recipe) -> Evaluation {
        let asset_id = AssetId::new();
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset_id.clone(),
            sequence: 1,
            action_id: "test".into(),
            label: "Test".into(),
            parameters: json!({}),
            actor: "test".into(),
            timestamp_ms: 0,
            request_id: None,
            base_revision: 0,
            result_revision: 1,
            snapshot: Snapshot::original(asset_id),
            undo_parent: None,
            restore_target: None,
        };
        Evaluation::new(
            Arc::new(ModuleRegistry::builtin()),
            context.clone(),
            source.clone(),
            entry,
            recipe.clone(),
            None,
        )
    }

    /// `recipe` over `source` on a context of its own.
    fn stored(source: &PreviewSource, recipe: &Recipe) -> Evaluation {
        evaluation(&RenderContext::new(), source, recipe)
    }

    fn recipe_of(layers: Vec<Layer>) -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            ..Recipe::default()
        }
    }

    fn basic() -> Layer {
        Layer::new(BASIC_EFFECT, json!({"exposure": 0.4, "contrast": 20.0}))
    }

    fn mixer() -> Layer {
        Layer::new(
            MIXER_EFFECT,
            json!({"red-hue": 20.0, "blue-luminance": -15.0}),
        )
    }

    /// Presence with no Dehaze, whose plan reads no light.
    fn presence() -> Layer {
        Layer::new(PRESENCE_EFFECT, json!({"texture": 30.0, "clarity": 35.0}))
    }

    fn crop() -> Layer {
        Layer::crop(CropPayload {
            angle: 4.0,
            x: 0.15,
            y: 0.1,
            width: 0.7,
            height: 0.75,
        })
    }

    /// A JPEG and a developed RAW, `width` × `height`.
    fn sources(width: u32, height: u32) -> [(&'static str, PreviewSource); 2] {
        [
            ("JPEG", PreviewSource::Jpeg(gradient(width, height))),
            (
                "RAW",
                PreviewSource::Raw {
                    image: varied(width, height),
                    settings: LinearSettings::default(),
                },
            ),
        ]
    }

    /// Stacks through Presence and Detail, through a straightened crop, of colour alone, and none.
    fn stacks() -> Vec<(&'static str, Recipe)> {
        vec![
            (
                "Detail, Basic and Presence",
                recipe_of(vec![
                    Layer::new(DETAIL_EFFECT, json!({"sharpening": 40.0})),
                    basic(),
                    presence(),
                ]),
            ),
            (
                "Basic, the mixer and a straightened crop",
                recipe_of(vec![basic(), mixer(), crop()]),
            ),
            ("Basic", recipe_of(vec![basic()])),
            ("nothing", recipe_of(Vec::new())),
        ]
    }

    fn planned(answer: Result<GpuAnswer, Error>) -> Box<GpuPlan> {
        match answer.unwrap() {
            GpuAnswer::Plan(plan) => plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        }
    }

    /// Side retries and strategy/light choices must not recompile or replan a source. Check
    /// both counters and compare each candidate's windows/order to a fresh exact request.
    #[test]
    fn a_prepared_stream_reuses_one_compilation_and_plan_across_candidates() {
        for (_, source) in sources(157, 101) {
            for (_, recipe) in stacks().into_iter().chain(std::iter::once((
                "staged light",
                recipe_of(vec![
                    Layer::new(DETAIL_EFFECT, json!({"sharpening": 40.0})),
                    Layer::new(PRESENCE_EFFECT, json!({"dehaze": 30.0})),
                ]),
            ))) {
                let evaluation = stored(&source, &recipe);
                crate::modules::stack_compiles::take();
                preparation_count::take();
                let prepared = PreparedStream::of(&evaluation).unwrap();
                let candidates: Vec<_> = [64, 32, 16]
                    .into_iter()
                    .map(|side| prepared.stream(side).unwrap())
                    .collect();
                let _ = prepared.strategy(2 << 30, &[64, 32, 16]).unwrap();
                // A tiny budget exercises the chained/refusal choice, without another plan.
                let _ = prepared.strategy(1, &[64, 32, 16]);
                let _ = prepared.light_sweeps(2 << 30, &[64, 32, 16]).unwrap();
                assert_eq!(
                    crate::modules::stack_compiles::take(),
                    0,
                    "the evaluation already owns its exact compilation"
                );
                assert_eq!(preparation_count::take(), 1);
                for candidate in candidates {
                    assert_eq!(candidate, plan_stream(&evaluation, candidate.side).unwrap());
                }
            }
        }
    }

    /// A read's tile is its rectangle clipped to the stage, over the window of the source the
    /// region planner gives that rectangle, its origin on the plan's anchor and inside the source;
    /// every read of a stage draws the stack's one plan from the source; and a rectangle past the
    /// stage plans an empty tile, which draws nothing.
    #[test]
    fn a_read_plans_the_tile_of_its_rectangle_over_an_anchored_window() {
        for (domain, source) in sources(157, 101) {
            let (width, height) = source.dimensions();
            for (stack, recipe) in stacks() {
                let what = format!("{domain}, {stack}");
                let evaluation = stored(&source, &recipe);
                let request = GpuPlanRequest::exact(0, Stage { width, height }).from_source();
                let request = match &source {
                    PreviewSource::Raw { .. } => request.linear(),
                    PreviewSource::Jpeg(_) => request,
                };
                let plan = planned(gpu_plan(&ModuleRegistry::builtin(), &recipe, request));
                let compiled = evaluation.compiled().unwrap();
                let output = compiled.stage();
                for rect in [
                    Region {
                        x0: 40,
                        y0: 30,
                        width: 17,
                        height: 17,
                    },
                    Region {
                        x0: 0,
                        y0: 0,
                        width: 9,
                        height: 5,
                    },
                    Region {
                        x0: output.width - 4,
                        y0: output.height - 3,
                        width: 17,
                        height: 17,
                    },
                ] {
                    let at = format!("{what}, {rect:?}");
                    let read = plan_read(&evaluation, ReadStage::Output, rect).unwrap();
                    assert_eq!(read.plan, plan, "{at}: the stack's plan from the source");
                    assert_eq!(read.size, output, "{at}");
                    assert_eq!(read.tile.rect, clipped(rect, output), "{at}");
                    let anchor = plan.anchor();
                    let window = WindowPlan::of_gpu_rect(compiled, (width, height), read.tile.rect)
                        .unwrap()
                        .reads(0);
                    assert_eq!(read.tile.window, anchored(window, anchor), "{at}");
                    let held = read.tile.window;
                    assert_eq!(held.x0 % anchor.multiple.0, 0, "{at}: anchored across");
                    assert_eq!(held.y0 % anchor.multiple.1, 0, "{at}: anchored down");
                    assert!(
                        held.x1() <= width && held.y1() <= height && !held.is_empty(),
                        "{at}: inside the source"
                    );
                    assert_eq!(read.source, source.identity(), "{at}");
                    assert_eq!(
                        read.format,
                        BoundaryFormat::of(matches!(source, PreviewSource::Raw { .. })),
                        "{at}"
                    );
                }
                let past = plan_read(
                    &evaluation,
                    ReadStage::Output,
                    Region {
                        x0: output.width + 3,
                        y0: 0,
                        width: 4,
                        height: 4,
                    },
                )
                .unwrap();
                assert!(past.tile.rect.is_empty(), "{what}: nothing of the stage");
                assert!(past.tile.window.is_empty(), "{what}: no window to draw");
            }
        }
    }

    /// A read of the stage a layer receives plans the layers before it, over the stage they
    /// produce — a crop before it shrinks the stage, none leaves the source's — through the same
    /// compile the GPU plan makes; before the first layer it plans the empty stack over the source,
    /// and past the stack it answers why it cannot.
    #[test]
    fn a_prefix_read_plans_the_stack_before_its_layer() {
        let registry = ModuleRegistry::builtin();
        for (domain, source) in sources(157, 101) {
            let (width, height) = source.dimensions();
            let recipe = recipe_of(vec![
                basic(),
                presence(),
                crop(),
                Layer::new(VIGNETTE_EFFECT, json!({"amount": -40.0})),
            ]);
            let evaluation = stored(&source, &recipe);
            for layer in 0..=recipe.layers.len() {
                let what = format!("{domain}, before layer {layer}");
                let stage = ReadStage::Before {
                    layer,
                    mode: MaskInputMode::Boundary,
                };
                let read = plan_read(
                    &evaluation,
                    stage,
                    Region {
                        x0: 20,
                        y0: 20,
                        width: 5,
                        height: 5,
                    },
                )
                .unwrap();
                let before = recipe_of(recipe.layers[..layer].to_vec());
                let request = GpuPlanRequest::exact(0, Stage { width, height }).from_source();
                let request = match &source {
                    PreviewSource::Raw { .. } => request.linear(),
                    PreviewSource::Jpeg(_) => request,
                };
                assert_eq!(
                    read.plan,
                    planned(gpu_plan(&registry, &before, request)),
                    "{what}: the plan of the layers before it"
                );
                let size = registry.compile(width, height, &before).unwrap().stage();
                assert_eq!(read.size, size, "{what}");
                if layer >= 3 {
                    assert!(
                        size.width < width && size.height < height,
                        "{what}: the crop's stage"
                    );
                } else {
                    assert_eq!(size, Stage { width, height }, "{what}: the source's stage");
                }
            }
            let past = plan_read(
                &evaluation,
                ReadStage::Before {
                    layer: recipe.layers.len() + 1,
                    mode: MaskInputMode::Boundary,
                },
                Region {
                    x0: 0,
                    y0: 0,
                    width: 1,
                    height: 1,
                },
            );
            assert_eq!(
                past.unwrap_err().code(),
                "unplannable",
                "{domain}: past the stack"
            );
        }
    }

    /// A read of the output stage, a read of the stage after Dehaze and a stream carry the light
    /// the Presence layer reads, planned from the source over the whole stage at full resolution
    /// as the picture at rest's is; a read of the stage before Presence reads no light.
    #[test]
    fn a_read_or_a_stream_that_reads_a_light_carries_it() {
        for (domain, source) in sources(97, 61) {
            let dehaze = Layer::new(
                PRESENCE_EFFECT,
                json!({"texture": 30.0, "clarity": 35.0, "dehaze": 20.0}),
            );
            let recipe = recipe_of(vec![basic(), dehaze, mixer()]);
            let after = ReadStage::Before {
                layer: 2,
                mode: MaskInputMode::ColourRun,
            };
            let before = ReadStage::Before {
                layer: 1,
                mode: MaskInputMode::Boundary,
            };
            let rect = Region {
                x0: 10,
                y0: 10,
                width: 3,
                height: 3,
            };
            let evaluation = stored(&source, &recipe);
            let whole = Stage {
                width: 97,
                height: 61,
            };
            let lit = |plan: &GpuPlan| {
                plan.lights.len() == 1
                    && plan.lights[0].layer == 1
                    && plan.lights[0].stage == whole
                    && plan.lights[0].over_source()
            };
            for stage in [ReadStage::Output, after] {
                let read = plan_read(&evaluation, stage, rect).unwrap();
                assert!(lit(&read.plan), "{domain}: {stage:?}");
            }
            assert!(lit(&plan_stream(&evaluation, 32).unwrap().plan), "{domain}");
            let read = plan_read(&evaluation, before, rect).unwrap();
            assert!(!read.plan.reads_lights(), "{domain}");
        }
    }

    /// A stream is the picture at rest's tiles at full scale: the same plan and, tile for tile,
    /// the same rectangles and anchored windows the picture at rest plans at that side, the stream
    /// row by row over the whole output stage where the picture at rest draws them by shape, each
    /// tile the window a read of its rectangle plans; its bands are its rows.
    #[test]
    fn a_stream_is_the_picture_at_rests_tiles_at_full_scale() {
        let bounds = ProxyBounds {
            width: 64,
            height: 64,
        };
        for (domain, source) in sources(157, 101) {
            for (stack, recipe) in stacks() {
                let what = format!("{domain}, {stack}");
                let evaluation = stored(&source, &recipe);
                for side in [48, 2048] {
                    let stream = plan_stream(&evaluation, side).unwrap();
                    let rest = super::super::plan_rest_tiles(
                        &evaluation,
                        bounds,
                        super::super::RestSizing::Side(side),
                    )
                    .unwrap()
                    .expect("tiles at bounds smaller than the stage")
                    .unwrap();
                    assert_eq!(stream.plan, rest.plan, "{what}: one plan");
                    let mut rows = rest.tiles.clone();
                    rows.sort_by_key(|tile| (tile.rect.y0, tile.rect.x0));
                    assert_eq!(stream.tiles, rows, "{what}: the same tiles");
                    assert_eq!(stream.output, rest.output, "{what}");
                    assert_eq!(
                        (&stream.source, stream.format),
                        (&rest.source, rest.format),
                        "{what}"
                    );
                    let covered: u64 = stream.tiles.iter().map(|tile| tile.rect.pixels()).sum();
                    assert_eq!(covered, Region::whole(stream.output).pixels(), "{what}");
                    let mut row = 0;
                    for band in stream.bands() {
                        assert!(band.iter().all(|tile| tile.rect.y0 == row), "{what}");
                        assert_eq!(band.first().unwrap().rect.x0, 0, "{what}");
                        let width: u32 = band.iter().map(|tile| tile.rect.width).sum();
                        assert_eq!(width, stream.output.width, "{what}: a band is whole rows");
                        row += band[0].rect.height;
                    }
                    assert_eq!(
                        row, stream.output.height,
                        "{what}: the bands cover the stage"
                    );
                    for tile in &stream.tiles {
                        let read = plan_read(&evaluation, ReadStage::Output, tile.rect).unwrap();
                        assert_eq!(read.tile, *tile, "{what}: {:?}", tile.rect);
                    }
                }
            }
        }
    }

    /// The answer's values: the output stage's codes are read back as codes and nothing else is;
    /// on the byte path the output stage and the boundary a spatial layer receives are clamped to
    /// `[0, 1]` and a colour run is not, on the linear path nothing is; and codes are always the
    /// core quantizer's of the linear values answered.
    #[test]
    fn a_read_answers_its_values_as_its_stage_holds_them() {
        let values = [[-0.25_f32, 0.5, 1.75], [0.002, 0.999, 0.4]];
        let quantized: Vec<[u8; 4]> = values
            .iter()
            .map(|value| {
                let [red, green, blue] = quantize_pixel(value.map(|c| c.clamp(0.0, 1.0)));
                [red, green, blue, 255]
            })
            .collect();
        let clamped: Vec<[f32; 3]> = values
            .iter()
            .map(|value| value.map(|c| c.clamp(0.0, 1.0)))
            .collect();
        for (domain, source) in sources(31, 23) {
            let raw = matches!(source, PreviewSource::Raw { .. });
            let recipe = recipe_of(vec![basic(), mixer()]);
            let evaluation = stored(&source, &recipe);
            let rect = Region {
                x0: 1,
                y0: 1,
                width: 2,
                height: 1,
            };
            for (stage, clamps) in [
                (ReadStage::Output, !raw),
                (
                    ReadStage::Before {
                        layer: 1,
                        mode: MaskInputMode::Boundary,
                    },
                    !raw,
                ),
                (
                    ReadStage::Before {
                        layer: 1,
                        mode: MaskInputMode::ColourRun,
                    },
                    false,
                ),
            ] {
                let what = format!("{domain}, {stage:?}");
                let read = plan_read(&evaluation, stage, rect).unwrap();
                assert_eq!(
                    read.reads_codes(ReadValues::Codes),
                    stage == ReadStage::Output,
                    "{what}"
                );
                assert!(!read.reads_codes(ReadValues::Linear), "{what}");
                let expected = if clamps {
                    clamped.clone()
                } else {
                    values.to_vec()
                };
                assert_eq!(
                    read.answer(ReadValues::Linear, values),
                    ReadPixels::Linear(expected),
                    "{what}"
                );
                assert_eq!(
                    read.answer(ReadValues::Codes, values),
                    ReadPixels::Codes(quantized.clone()),
                    "{what}"
                );
            }
        }
    }

    /// A source drafted under an approximate white balance is the reference's to read and to
    /// export: the GPU source holds the development's own planes, not the approximation.
    #[test]
    fn a_drafted_white_balance_is_left_to_the_reference() {
        let balance = WhiteBalanceApproximation::from_matrix([
            [1.21, -0.11, -0.02],
            [-0.06, 1.08, -0.02],
            [0.01, -0.13, 1.12],
        ])
        .unwrap();
        let source = PreviewSource::Raw {
            image: varied(31, 23),
            settings: LinearSettings {
                white_balance: Some(balance),
            },
        };
        let evaluation = evaluation(&RenderContext::new(), &source, &recipe_of(vec![basic()]));
        let rect = Region {
            x0: 0,
            y0: 0,
            width: 1,
            height: 1,
        };
        assert_eq!(
            plan_read(&evaluation, ReadStage::Output, rect)
                .unwrap_err()
                .code(),
            "unplannable"
        );
        assert_eq!(
            plan_stream(&evaluation, 16).unwrap_err().code(),
            "unplannable"
        );
    }
    /// Integrated qualification only: warmed exact evaluation, no source preparation or GPU.
    /// Exercise all candidate sides plus strategy/light choices, retaining every timing sample.
    #[test]
    #[ignore = "integrated before/after measurement on a quiet host, 24/60 MP allocations"]
    fn code_structure_stream_setup_measurement() {
        use luxforge_testbase::Distribution;
        use std::{hint::black_box, time::Instant};
        for (width, height) in [(6_000, 4_000), (9_504, 6_336)] {
            for (domain, source) in sources(width, height) {
                let recipe = recipe_of(vec![
                    Layer::new(DETAIL_EFFECT, json!({"sharpening": 40.0})),
                    basic(),
                    Layer::new(
                        PRESENCE_EFFECT,
                        json!({"texture": 30.0, "clarity": 35.0, "dehaze": 30.0}),
                    ),
                ]);
                let evaluation = stored(&source, &recipe);
                evaluation.compiled().unwrap();
                let setup = || {
                    let prepared = PreparedStream::of(&evaluation).unwrap();
                    for side in STREAM_TILE_SIDES {
                        black_box(prepared.stream(side).unwrap());
                    }
                    black_box(
                        prepared
                            .strategy((2 << 30) - (256 << 20), &STREAM_TILE_SIDES)
                            .unwrap(),
                    );
                    black_box(
                        prepared
                            .light_sweeps((2 << 30) - (256 << 20), &STREAM_TILE_SIDES)
                            .unwrap(),
                    );
                };
                setup(); // Warm planner/library allocation paths; source is already developed.
                let mut times = Vec::new();
                let mut plans = Vec::new();
                let mut compiles = Vec::new();
                for _ in 0..30 {
                    preparation_count::take();
                    crate::modules::stack_compiles::take();
                    let started = Instant::now();
                    setup();
                    times.push(started.elapsed().as_secs_f64() * 1e3);
                    plans.push(preparation_count::take());
                    compiles.push(crate::modules::stack_compiles::take());
                }
                let wall = Distribution::of(times).unwrap();
                println!(
                    "STRUCTURE_SETUP {}",
                    json!({
                        "domain":domain,"dimensions":[width,height],"samples":wall.count,
                        "wall_ms":{"p50":wall.p50,"p95":wall.p95,"all":wall.samples},
                        "plan_preparations":plans,"stack_compiles":compiles,
                        "scope":"all three candidate grids plus strategy and light selection; prepared exact evaluation; no pixels read or GPU work"
                    })
                );
            }
        }
    }
}
