//! The GPU plan: what the compiled evaluation answers for a gesture, from the input of the earliest
//! layer a draft changes to the terminal output, or the named reason the gesture takes the CPU path.
//!
//! The plan is the CPU render's own structure read as data, in `O(layers + units + components)`
//! on the catalog owner: it reads no pixel and holds nothing that scales with the image.
//!
//! - **The boundary.** The input of the named layer, which the preview worker renders once per
//!   draft and the surface holds ([`GpuBoundary`]). Everything before it is baked in.
//! - **Content operations**, over the boundary's texels in recipe order: every colour operation of
//!   the boundary's segment from the boundary on.
//! - **The geometry tail** from the boundary's stage to the output stage: the exact steps after the
//!   boundary and every resample and warp after them, as one [`GeometryMap`] ([`GpuGeometry`]).
//! - **Output operations**, over the output stage's pixels in recipe order: the colour operations
//!   of the last segment when a resample separates it from the boundary, the vignette among them.
//! - **Clipping marks**: the output codes the clipping overlay counts, as linear thresholds.
//!
//! Each operation carries the map from its pass's pixel to the coordinate its units address, which
//! is exactly the coordinate the CPU hands `PointwiseColor::apply_row`, and its mask's map to the
//! mask's own stage pixel. A masked operation is blended against its own input as the CPU blends it
//! (`colour_runs.rs`): `out = (1 − M)·in + M·units(in)` per channel in linear light, and `in`
//! itself wherever `M` is exactly zero, so a value the CPU never computes cannot reach the frame.
use super::grid::CoordinateGrid;
use super::program::{GpuDescription, GpuProgramKind};
use super::spatial::{self, GPU_CHAIN_APPLY_PLANES, GpuSpatial};
use crate::{
    ComponentMode, EffectStage, Error, ModuleRegistry, PreviewSource, ProxyPlan, Recipe,
    colour::srgb,
    mask_field::MaskSampling,
    modules::{ColorOperation, ExactGeometry, Global, Processing, Region, Stage},
    render::{
        Compiled, Entry, PixelDomain, RenderContext, RenderSource,
        byte::{self, Byte},
        context::EstimateKey,
        linear::{self, Linear},
        map::{Affine, GeometryMap, MappingShape, StageSize, WarpStep},
        pipeline::{SpatialEntry, input_prefix_key},
    },
};

/// What a GPU plan is asked for: the layer whose input it starts from, and the compilation the
/// CPU frame it previews comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuPlanRequest {
    /// The earliest layer the draft changes. The plan starts from its input.
    pub boundary: usize,
    /// The content stage the stack is compiled against: the proxy stage at Fit, the source's own
    /// stage at 100%. The plan addresses this whole stage; a boundary that holds a window of it
    /// offsets its texels by the window's origin.
    pub stage: Stage,
    /// The source's full content stage, which a proxy compilation resolves stage-relative
    /// payloads against.
    pub full: Stage,
    /// Whether the plan previews the proxy phase, whose masks take the thin-feature supersample.
    pub proxy: bool,
    /// Plan disabled programs as if they were enabled: what the qualification corpus asks, to judge
    /// a program against its limits before it is enabled. Never for a frame a person sees.
    pub qualifying: bool,
    /// The layer a gesture drafts, compiled in its GPU shape: every unit its module can hold, a
    /// neutral one as its own identity (`CompileStage::gpu_shape`). A field-patch module compiles
    /// only its non-neutral units, so without this a drag that leaves or returns to neutral would
    /// meet a new program sequence mid-gesture. `None` compiles every layer as the CPU does.
    pub drafted: Option<usize>,
    /// The frame previews a RAW photograph's linear path, which holds every stage boundary's frame
    /// unclamped in float, where the byte path clamps and quantizes it: a spatial operation's input
    /// and output, and a resample's input, are then not clamped.
    pub linear: bool,
}

impl GpuPlanRequest {
    /// A plan of the proxy phase at Fit, over a `proxy` stage of a `full` content stage.
    pub fn fit(boundary: usize, proxy: Stage, full: Stage) -> Self {
        Self {
            boundary,
            stage: proxy,
            full,
            proxy: true,
            qualifying: false,
            drafted: None,
            linear: false,
        }
    }

    /// A plan of the exact phase over the source's own `stage`, as at 100%.
    pub fn exact(boundary: usize, stage: Stage) -> Self {
        Self {
            boundary,
            stage,
            full: stage,
            proxy: false,
            qualifying: false,
            drafted: None,
            linear: false,
        }
    }

    /// The same request with disabled programs planned, for the qualification corpus.
    pub fn qualifying(mut self) -> Self {
        self.qualifying = true;
        self
    }

    /// The same request with layer `layer`, the one a gesture drafts, in its GPU shape
    /// ([`Self::drafted`]).
    pub fn drafted(mut self, layer: usize) -> Self {
        self.drafted = Some(layer);
        self
    }

    /// The same request for a frame of the RAW linear path.
    pub fn linear(mut self) -> Self {
        self.linear = true;
        self
    }
}

/// Where a plan finds the global estimates its spatial operations read ([`gpu_plan_with`]): the
/// render context whose store the CPU frames the plan previews fill, and the source those frames
/// were rendered from at the plan's stage. A unit's estimate is the store's when the store holds it
/// under the key a CPU frame of the same content at the same stage asks with; otherwise the GPU
/// takes it from the stage it holds and the frame is approximate.
#[derive(Clone, Copy)]
pub struct GpuEstimates<'a> {
    pub context: &'a RenderContext,
    pub source: EstimateSource<'a>,
}

/// The source a plan's CPU frames are rendered from, as the estimate store names it.
#[derive(Clone, Copy)]
pub enum EstimateSource<'a> {
    /// The source itself.
    Render(RenderSource<'a>),
    /// The proxy of a preview source at a plan, which the preview worker renders a Fit frame from:
    /// named by the source's identity and the plan alone, and never built, so a plan made on the
    /// catalog owner reads the store with no pixel work.
    Proxy {
        source: &'a PreviewSource,
        plan: ProxyPlan,
    },
    /// The source at its own whole `stage`, whose estimates the exact phase hands a windowed
    /// proxy's spatial operations, since a window cannot reduce its stage (`Render::render_proxy`):
    /// named with that stage's dimensions whatever stage the plan is drawn at.
    Whole {
        source: RenderSource<'a>,
        stage: Stage,
    },
}

impl<'a> From<RenderSource<'a>> for EstimateSource<'a> {
    fn from(source: RenderSource<'a>) -> Self {
        Self::Render(source)
    }
}

impl EstimateSource<'_> {
    /// Whether its frames take the linear path.
    fn linear(&self) -> bool {
        match self {
            Self::Render(source) | Self::Whole { source, .. } => {
                matches!(source, RenderSource::Linear { .. })
            }
            Self::Proxy { source, .. } => matches!(source, PreviewSource::Raw { .. }),
        }
    }

    /// The stage its estimates are named with: `stage`, the one the plan addresses, but for
    /// [`Self::Whole`]'s own.
    fn stage(&self, stage: Stage) -> Stage {
        match self {
            Self::Whole { stage, .. } => *stage,
            _ => stage,
        }
    }

    /// The fingerprint and the estimate prefix its pixel domain gives `prefix_hash`. A proxy keeps
    /// its source's fingerprint; a JPEG's is the plan's window at the source's orientation, a
    /// RAW's its derived development ([`crate::proxy::proxy_development`]) under an identity view
    /// and the settings a job renders it under.
    pub(crate) fn identity(&self, prefix_hash: &str) -> Result<(String, String), Error> {
        Ok(match *self {
            Self::Render(RenderSource::Byte(image))
            | Self::Whole {
                source: RenderSource::Byte(image),
                ..
            } => {
                let domain = Byte(image);
                (
                    domain.fingerprint().to_owned(),
                    input_prefix_key(&domain, prefix_hash).into_owned(),
                )
            }
            Self::Render(RenderSource::Linear { image, settings })
            | Self::Whole {
                source: RenderSource::Linear { image, settings },
                ..
            } => {
                let domain = Linear::new(image, settings)?;
                (
                    domain.fingerprint().to_owned(),
                    input_prefix_key(&domain, prefix_hash).into_owned(),
                )
            }
            Self::Proxy { source, plan } => {
                let (width, height) = plan.source_dimensions();
                match source {
                    PreviewSource::Jpeg(image) => (
                        image.fingerprint.clone(),
                        byte::estimate_prefix(prefix_hash, width, height, image.orientation),
                    ),
                    PreviewSource::Raw { image, settings } => (
                        image.fingerprint().to_owned(),
                        linear::estimate_prefix(
                            prefix_hash,
                            crate::proxy::proxy_development(image, plan),
                            ([0, 0, width, height], 1),
                            settings.white_balance,
                        ),
                    ),
                }
            }
        })
    }
}

impl GpuEstimates<'_> {
    /// The store's estimate for `key` over a spatial entry's `stage`, or `None` when it holds none.
    fn stored(
        &self,
        entry: &SpatialEntry,
        stage: Stage,
        key: &str,
    ) -> Result<Option<Global>, Error> {
        Ok(self.cached(entry.prefix_hash(), stage, key)?.flatten())
    }

    /// The store's entry for `key` over a `stage` behind the layers `prefix_hash` names: `None`
    /// when it holds none, `Some(None)` for a preparation that yielded none.
    fn cached(
        &self,
        prefix_hash: &str,
        stage: Stage,
        key: &str,
    ) -> Result<Option<Option<Global>>, Error> {
        let (fingerprint, prefix) = self.source.identity(prefix_hash)?;
        let stage = self.source.stage(stage);
        Ok(self.context.estimates().cached(&EstimateKey {
            fingerprint,
            prefix_hash: prefix,
            width: stage.width,
            height: stage.height,
            estimate: key.to_owned().into(),
        }))
    }

    /// Whether the store holds every global estimate `entry` prepares over its `stage`, so a frame
    /// of it reduces nothing. `O(units)`, and reads no pixel.
    pub(crate) fn holds(&self, entry: &SpatialEntry, stage: Stage) -> Result<bool, Error> {
        for unit in entry.operation.units() {
            if let Some(key) = unit.estimate_key()
                && self.cached(entry.prefix_hash(), stage, &key)?.is_none()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// What the stack a draft was opened over stored for one of the drafted stack's spatial layers:
/// the prefix hash its global estimates were stored under there. Where the store holds none for
/// the drafted stack, the plan reads the layer's estimates under this one instead, held for the
/// drag ([`GpuSpatial::held`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeldPrefix {
    /// The layer in the planned stack.
    pub(crate) layer: usize,
    pub(crate) prefix_hash: String,
}

/// What the compiled evaluation answers for a gesture.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuAnswer {
    Plan(Box<GpuPlan>),
    /// The gesture takes the CPU path, for this reason.
    Fallback(GpuFallback),
}

impl GpuAnswer {
    /// The plan, when there is one.
    pub fn plan(&self) -> Option<&GpuPlan> {
        match self {
            Self::Plan(plan) => Some(plan),
            Self::Fallback(_) => None,
        }
    }

    /// The reason there is no plan, when there is none.
    pub fn fallback(&self) -> Option<&GpuFallback> {
        match self {
            Self::Plan(_) => None,
            Self::Fallback(fallback) => Some(fallback),
        }
    }
}

/// Why a gesture takes the CPU path, naming the layer that decided it. The first reason in recipe
/// order is the one answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuFallback {
    /// A pixel-stage layer is in the stack: its payload addresses content pixels, which no proxy
    /// stage has and no program replaces.
    PixelStage { layer: usize },
    /// The boundary layer is a source or geometry effect: its draft changes the content stage or
    /// the geometry itself, which a plan from its input cannot hold.
    BoundaryStage { layer: usize, stage: EffectStage },
    /// A spatial or restoration layer after the boundary that the plan cannot hold: a unit without
    /// a GPU program, or an exact step between the boundary and it or between it and the spatial
    /// layer before it.
    SpatialUnit { layer: usize },
    /// A spatial or restoration layer whose applies read more planes than a pass can bind beside
    /// its input ([`GPU_CHAIN_APPLY_PLANES`]).
    SpatialChain { layer: usize },
    /// A colour layer between two resamples after the boundary: its stage is neither the
    /// boundary's nor the output's, and a plan has only those two passes.
    BetweenResamples { layer: usize },
    /// A pointwise unit, or a mask component, without a GPU program.
    NoProgram { layer: usize, unit: String },
    /// A program that ships disabled, because it has not met its error limits.
    DisabledProgram { layer: usize, program: &'static str },
    /// The draft changes no layer of the stack yet, and drafts no layer of its own: there is
    /// nothing for a plan to start from, and its frame is the entry's.
    Unchanged,
    /// At a percentage zoom, the spatial layer's global estimate would be taken on the GPU from
    /// the visible region alone, where the exact visible region reads the whole stage's: Dehaze's
    /// atmospheric light, which misses the spatial limits there on the corpus.
    RegionEstimate { layer: usize },
    /// At a Fit proxy that holds a window of its stage, the spatial layer's global estimate would be
    /// taken on the GPU from the proxy, where the CPU's windowed proxy is handed the exact stage's:
    /// Dehaze's atmospheric light, which no proxy-scale estimate reproduces within the spatial
    /// limits on the corpus. The store holds none the drag may read: a colour drag under it, or a
    /// stack whose exact light no frame has stored yet.
    WindowEstimate { layer: usize },
    /// Planning a draft's GPU preview failed for this reason, which the CPU path answers in its
    /// own way.
    Unplannable(String),
}

impl GpuFallback {
    /// A stable kebab-case name for the reason, for session state and evidence.
    pub fn code(&self) -> &'static str {
        match self {
            Self::PixelStage { .. } => "pixel-stage",
            Self::BoundaryStage { .. } => "boundary-stage",
            Self::SpatialUnit { .. } => "spatial-unit",
            Self::SpatialChain { .. } => "spatial-chain",
            Self::BetweenResamples { .. } => "between-resamples",
            Self::NoProgram { .. } => "no-program",
            Self::DisabledProgram { .. } => "disabled-program",
            Self::Unchanged => "unchanged",
            Self::RegionEstimate { .. } => "region-estimate",
            Self::WindowEstimate { .. } => "window-estimate",
            Self::Unplannable(_) => "unplannable",
        }
    }

    /// The layer the reason names; `None` for [`Self::Unchanged`], which names none.
    pub fn layer(&self) -> Option<usize> {
        match self {
            Self::PixelStage { layer }
            | Self::BoundaryStage { layer, .. }
            | Self::SpatialUnit { layer }
            | Self::SpatialChain { layer }
            | Self::BetweenResamples { layer }
            | Self::NoProgram { layer, .. }
            | Self::DisabledProgram { layer, .. }
            | Self::RegionEstimate { layer }
            | Self::WindowEstimate { layer } => Some(*layer),
            Self::Unchanged | Self::Unplannable(_) => None,
        }
    }
}

impl std::fmt::Display for GpuFallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PixelStage { layer } => write!(f, "layer {layer} is a pixel-stage layer"),
            Self::BoundaryStage { layer, stage } => write!(
                f,
                "layer {layer} is a {} effect, whose draft the plan cannot start from",
                stage.as_str()
            ),
            Self::SpatialUnit { layer } => {
                write!(
                    f,
                    "layer {layer} is a spatial layer the GPU plan cannot hold"
                )
            }
            Self::SpatialChain { layer } => write!(
                f,
                "layer {layer} would chain more spatial planes than a GPU pass can bind"
            ),
            Self::BetweenResamples { layer } => {
                write!(f, "layer {layer} is a colour layer between two resamples")
            }
            Self::NoProgram { layer, unit } => {
                write!(f, "layer {layer} holds {unit}, which has no GPU program")
            }
            Self::DisabledProgram { layer, program } => {
                write!(f, "layer {layer} needs the disabled GPU program {program}")
            }
            Self::Unchanged => write!(f, "the draft changes no layer yet"),
            Self::RegionEstimate { layer } => write!(
                f,
                "layer {layer}'s global estimate would be taken from the visible region alone"
            ),
            Self::WindowEstimate { layer } => write!(
                f,
                "layer {layer}'s global estimate would be taken from a windowed proxy, which is \
                 handed the exact stage's"
            ),
            Self::Unplannable(reason) => {
                write!(f, "the GPU preview could not be planned: {reason}")
            }
        }
    }
}

/// An exact integer map from a pass's pixel `(x, y)` to the coordinate a program addresses:
/// `(a·x + b·y + tx, c·x + d·y + ty)`. The linear part is a signed permutation and every
/// coefficient an integer, so the map is exact in `f32` for every coordinate of an admissible
/// stage, and a program's `pos` is the integer the CPU unit is handed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GpuPosition {
    pub a: i64,
    pub b: i64,
    pub tx: i64,
    pub c: i64,
    pub d: i64,
    pub ty: i64,
}

impl GpuPosition {
    pub const IDENTITY: Self = Self {
        a: 1,
        b: 0,
        tx: 0,
        c: 0,
        d: 1,
        ty: 0,
    };

    /// The coordinate pixel `(x, y)` maps to.
    pub fn at(self, x: i64, y: i64) -> (i64, i64) {
        (
            self.a * x + self.b * y + self.tx,
            self.c * x + self.d * y + self.ty,
        )
    }

    /// A translation by `(x, y)`.
    pub(crate) fn translation(x: i64, y: i64) -> Self {
        Self {
            tx: x,
            ty: y,
            ..Self::IDENTITY
        }
    }

    /// An exact step's forward map, [`ExactGeometry::map`] without its range check.
    fn of(step: ExactGeometry) -> Self {
        Self {
            a: step.a,
            b: step.b,
            tx: step.tx,
            c: step.c,
            d: step.d,
            ty: step.ty,
        }
    }

    /// An exact step's inverse, [`ExactGeometry::unmap`]: its linear part is a signed permutation,
    /// whose inverse is its transpose.
    fn inverse(step: ExactGeometry) -> Self {
        Self {
            a: step.a,
            b: step.c,
            tx: -(step.a * step.tx + step.c * step.ty),
            c: step.b,
            d: step.d,
            ty: -(step.b * step.tx + step.d * step.ty),
        }
    }

    /// This map followed by `next`.
    pub(crate) fn then(self, next: Self) -> Self {
        Self {
            a: next.a * self.a + next.b * self.c,
            b: next.a * self.b + next.b * self.d,
            tx: next.a * self.tx + next.b * self.ty + next.tx,
            c: next.c * self.a + next.d * self.c,
            d: next.c * self.b + next.d * self.d,
            ty: next.c * self.tx + next.d * self.ty + next.ty,
        }
    }
}

/// One colour operation of a plan: one layer's units in order, the coordinate they address and the
/// mask the operation is blended by.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuOperation {
    /// The layer it was compiled from.
    pub layer: usize,
    /// Every unit's program and words, in evaluation order, with nothing clamped between them.
    pub units: Vec<GpuDescription>,
    /// From the pass's pixel to the `pos` every unit receives.
    pub position: GpuPosition,
    pub mask: Option<GpuMask>,
}

/// The mask one operation is blended by, as data: its components' programs and the composition the
/// CPU folds them with (`CompiledMask::coverage`), in `f32`:
///
/// ```text
/// m = 0
/// for component: c = coverage(pos, input); c = 1 − c if component.invert
///                m = max(m, c) | min(m, 1 − c) | min(m, c)   by its mode
/// m = 1 − m if invert
/// M = scale · m
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct GpuMask {
    /// From the pass's pixel to the pixel of the stage the mask was compiled against.
    pub position: GpuPosition,
    /// The rectangle of that stage outside which `M` is exactly zero.
    pub bounds: Region,
    /// The proxy's thin-feature rule fired: `M` is the mean of the composition at the four pixels
    /// `(2x + i, 2y + j)`, `i, j ∈ {0, 1}`, of a stage twice the size, which the components'
    /// words were compiled against.
    pub supersample: bool,
    pub components: Vec<GpuComponent>,
    /// The whole mask's inversion, `1 − m`.
    pub invert: bool,
    /// `amount / 100`, the final multiply.
    pub scale: f32,
}

/// One mask component of a plan: its program and how the composition folds it.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuComponent {
    /// The kind token it was compiled through.
    pub kind: &'static str,
    pub mode: ComponentMode,
    /// The component's own inversion, `1 − c`, before the composition.
    pub invert: bool,
    pub program: GpuDescription,
}

/// The texels a plan starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuBoundary {
    /// The layer whose input this is.
    pub layer: usize,
    /// The stage that layer receives.
    pub stage: Stage,
    /// A colour operation before the boundary, in the same segment, hands the layer its unclamped
    /// output, as one colour run: the boundary holds that `f32` value as it is, not an encoded
    /// frame of it. Otherwise the layer receives its segment's input, which is the source or the
    /// frame a stage boundary wrote.
    pub continues_run: bool,
}

/// The geometry tail from the boundary's stage to the output stage.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuGeometry {
    /// Content is the boundary's stage, output the output stage. Output pixel `(x, y)` reads the
    /// boundary at the continuous coordinate `(u, v)` the map gives its centre `(x + ½, y + ½)`,
    /// in pixel-edge coordinates: a bilinear blend in linear light of the texels at
    /// `floor(u − ½)`, `floor(v − ½)` and the ones after them, weighted by the fractions and
    /// clamped to [`Self::reads`].
    pub map: GeometryMap,
    /// The rectangle of the boundary the CPU's resample reads, whose edge it replicates: the frame
    /// the exact steps after the boundary keep.
    pub reads: Region,
    /// A stage boundary separates the content operations from the tail, so the CPU clamps and
    /// quantizes their result before it is resampled: the plan clamps it to `[0, 1]` there.
    pub clamps: bool,
}

impl GpuGeometry {
    /// The output stage.
    pub fn output(&self) -> StageSize {
        self.map.output
    }

    /// The output-to-boundary matrix `[a, b, c, d, e, f]`, `u = a·x + b·y + c`, `v = d·x + e·y + f`,
    /// when the tail is affine.
    pub fn affine(&self) -> Option<[f64; 6]> {
        match &self.map.mapping {
            MappingShape::Affine { inverse, .. } => Some(*inverse),
            MappingShape::Warp { .. } => None,
        }
    }

    /// The output-to-boundary homography `[a, b, c, d, e, f, g, h, i]` when the tail is a
    /// perspective warp with no lens distortion, alone or composed with the exact steps, crops and
    /// straightenings around it: `u = (a·x + b·y + c) / w`, `v = (d·x + e·y + f) / w` for
    /// `w = g·x + h·y + i`, which is the map's own steps multiplied together, with no grid between
    /// them. Scaled so `w` is one at the output stage's centre, and `None` unless `w` is positive at
    /// every corner of the output stage, so the map is finite across it.
    pub fn projective(&self) -> Option<[f64; 9]> {
        let MappingShape::Warp { steps, .. } = &self.map.mapping else {
            return None;
        };
        // `GeometryMap::content_at` applies the last step first, so the map is the first step's
        // matrix times the next's, and so on.
        let mut product = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        for step in steps {
            let step = step.homography()?;
            product = std::array::from_fn(|at| {
                let (row, column) = (at / 3, at % 3);
                (0..3)
                    .map(|k| product[row * 3 + k] * step[k * 3 + column])
                    .sum()
            });
        }
        let output = self.output();
        let (width, height) = (f64::from(output.width), f64::from(output.height));
        let w = |x: f64, y: f64| product[6] * x + product[7] * y + product[8];
        let centre = w(width / 2.0, height / 2.0);
        let finite = [(0.0, 0.0), (width, 0.0), (0.0, height), (width, height)]
            .iter()
            .all(|&(x, y)| w(x, y) > 0.0)
            && product.iter().all(|value| value.is_finite());
        (finite && centre.is_finite() && centre > 0.0).then(|| product.map(|value| value / centre))
    }

    /// Whether the tail is drawn through a coordinate grid ([`Self::grid`]): a lens warp's, which
    /// neither [`Self::affine`] nor [`Self::projective`] states exactly.
    pub fn needs_grid(&self) -> bool {
        self.affine().is_none() && self.projective().is_none()
    }

    /// A lens warp's coordinate grid over `region` of the output stage, within
    /// [`super::GRID_TOLERANCE_PX`] of the map, drawn at `magnification` display pixels per output
    /// pixel ([`CoordinateGrid::new`]). `None` for an affine or projective tail, whose matrix the
    /// surface evaluates exactly at every pixel. Once per draft, not per tick: the geometry does not
    /// change while a colour draft is open.
    pub fn grid(
        &self,
        region: Region,
        magnification: f64,
    ) -> Result<Option<CoordinateGrid>, Error> {
        if !self.needs_grid() {
            return Ok(None);
        }
        CoordinateGrid::new(&self.map, region, magnification).map(Some)
    }
}

/// The output codes the clipping overlay marks, as the linear values that reach them: a channel
/// below `shadow_below` quantizes to code 0 and one at or above `highlight_from` to code 255. Each is
/// the first `f32` in its code's exact interval, so comparing an `f32` against it agrees with the
/// CPU's quantizer for every `f32`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuClipping {
    pub shadow_below: f32,
    pub highlight_from: f32,
}

impl GpuClipping {
    fn of_output_codes() -> Self {
        // The first f32 at or above an f64 threshold.
        let first_at = |threshold: f64| {
            let narrowed = threshold as f32;
            if f64::from(narrowed) < threshold {
                narrowed.next_up()
            } else {
                narrowed
            }
        };
        Self {
            shadow_below: first_at(srgb::decode(0.5 / 255.0)),
            highlight_from: first_at(srgb::decode(254.5 / 255.0)),
        }
    }
}

/// The ordered GPU plan from a boundary to the terminal output.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuPlan {
    pub boundary: GpuBoundary,
    /// The plan previews a RAW photograph's linear path, whose boundary is held in `f32`
    /// ([`crate::BoundaryFormat::Float`]) and whose boundary and geometry frames stay unclamped;
    /// otherwise the byte path's, held in half floats.
    pub linear: bool,
    /// Over the boundary's texels, in recipe order.
    pub content: Vec<GpuOperation>,
    /// The spatial operations the content operations' frame enters, in recipe order, at the
    /// boundary's stage: each one's passes over its own input, the boundary through everything
    /// before it, then its applies, then the colour operations it holds after it
    /// ([`GpuSpatial::after`]).
    pub spatial: Vec<GpuSpatial>,
    pub geometry: GpuGeometry,
    /// Over the output stage's pixels, after the geometry tail, in recipe order.
    pub output: Vec<GpuOperation>,
    pub clipping: GpuClipping,
}

impl GpuPlan {
    /// Every colour operation in recipe order: content, those between spatial operations, then
    /// output, with the spatial operations among them.
    pub fn operations(&self) -> impl Iterator<Item = &GpuOperation> {
        self.content
            .iter()
            .chain(self.spatial.iter().flat_map(|spatial| &spatial.after))
            .chain(&self.output)
    }

    /// The frame is approximate beyond the GPU's arithmetic: a global estimate is taken on the GPU
    /// from the stage it holds, or held from the stack the draft was opened over, not read from
    /// the store under the drafted stack's own key.
    pub fn approximate(&self) -> bool {
        self.spatial
            .iter()
            .any(|spatial| spatial.estimated || spatial.held)
    }
}

/// The GPU plan of `recipe` for `request`, or the reason the gesture takes the CPU path.
///
/// Compiles the stack once, exactly as the CPU frame it previews is compiled, and walks the
/// compilation: `O(layers + units + components)` on the catalog owner, with no pixel read and
/// nothing allocated that scales with the image. A stack that does not compile, or a boundary
/// outside it, is the CPU path's own error.
pub fn gpu_plan(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
) -> Result<GpuAnswer, Error> {
    gpu_plan_with(registry, recipe, request, None)
}

/// [`gpu_plan`], with the estimate store a spatial operation's global estimates are read from
/// ([`GpuEstimates`]). Without one, every estimate is taken on the GPU and the plan says so
/// ([`GpuPlan::approximate`]). The lookup is the store's own, `O(entries)`, and reads no pixel. A
/// source on the linear path comes with a [`GpuPlanRequest::linear`] request.
pub fn gpu_plan_with(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
    estimates: Option<GpuEstimates<'_>>,
) -> Result<GpuAnswer, Error> {
    gpu_plan_holding(registry, recipe, request, estimates, &[])
}

/// [`gpu_plan_with`], a spatial layer's estimates that the store does not hold for `recipe` read
/// under `held`'s prefix for it instead ([`HeldPrefix`]).
pub(crate) fn gpu_plan_holding(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
    estimates: Option<GpuEstimates<'_>>,
    held: &[HeldPrefix],
) -> Result<GpuAnswer, Error> {
    if let Some(estimates) = &estimates
        && estimates.source.linear() != request.linear
    {
        return Err(Error::internal(
            "a GPU plan's request names one path and its estimates' source the other",
        ));
    }
    let Some(layer) = recipe.layers.get(request.boundary) else {
        return Err(Error::validation(format!(
            "layer {} is outside the {}-layer stack",
            request.boundary,
            recipe.layers.len()
        )));
    };
    let sampling = if request.proxy {
        MaskSampling::ThinFeature
    } else {
        MaskSampling::Point
    };
    let compiled = registry.compile_shaped(
        request.stage.width,
        request.stage.height,
        request.full.width,
        request.full.height,
        recipe,
        sampling,
        request.drafted,
    )?;
    compiled.gpu_plan(
        request.boundary,
        request.stage,
        registry.effect_stage(&layer.effect_id),
        Planning {
            qualifying: request.qualifying,
            linear: request.linear,
            estimates,
            held,
        },
    )
}

/// How one plan is walked: whether disabled programs are planned, which path's quantization the
/// frame previews, and where stored estimates come from.
#[derive(Clone, Copy, Default)]
pub(crate) struct Planning<'a> {
    pub(crate) qualifying: bool,
    pub(crate) linear: bool,
    pub(crate) estimates: Option<GpuEstimates<'a>>,
    /// Where the stack the draft was opened over stored a spatial layer's estimates, read when
    /// the store holds none for the planned stack.
    pub(crate) held: &'a [HeldPrefix],
}

/// One step's verdict: a part of the plan, or the reason there is none.
type Planned<T> = Result<T, GpuFallback>;

impl Compiled {
    /// The GPU plan from the input of layer `boundary`, whose effect is at `stage`, of this
    /// compilation against a `content` stage: [`gpu_plan`] after its compile. Reads the
    /// compilation's structure only; an uncut compilation, since the plan addresses whole stages.
    pub(crate) fn gpu_plan(
        &self,
        boundary: usize,
        content: Stage,
        stage: Option<EffectStage>,
        planning: Planning<'_>,
    ) -> Result<GpuAnswer, Error> {
        match self.walk(boundary, content, stage, planning)? {
            Ok(plan) => Ok(GpuAnswer::Plan(Box::new(plan))),
            Err(fallback) => Ok(GpuAnswer::Fallback(fallback)),
        }
    }

    fn walk(
        &self,
        boundary: usize,
        content: Stage,
        stage: Option<EffectStage>,
        planning: Planning<'_>,
    ) -> Result<Planned<GpuPlan>, Error> {
        let qualifying = planning.qualifying;
        let &(first, start) = self
            .layers
            .get(boundary)
            .ok_or_else(|| Error::internal(format!("layer {boundary} has no compiled position")))?;
        // A stack-level reason first: a pixel-stage layer anywhere.
        for (index, segment) in self.segments.iter().enumerate() {
            if let Some(operation) = segment
                .operations
                .iter()
                .position(|operation| matches!(operation, Processing::PointReplace { .. }))
            {
                return Ok(Err(GpuFallback::PixelStage {
                    layer: self.layer_at(index, operation),
                }));
            }
        }
        if let Some(stage @ (EffectStage::Source | EffectStage::Geometry)) = stage {
            return Ok(Err(GpuFallback::BoundaryStage {
                layer: boundary,
                stage,
            }));
        }
        let last = self.segments.len() - 1;
        let segment = &self.segments[first];
        // The stage the boundary layer receives: its segment's input through the exact steps
        // before it. The steps after it carry its texels to the frame the segment writes.
        let input = match (first, &segment.entry) {
            (0, _) | (_, None) => content,
            (_, Some(entry)) => entry.stage(self.segments[first - 1].stage()),
        };
        let mut before = ExactGeometry::identity(input.width, input.height);
        for operation in &segment.operations[..start] {
            if let Processing::ExactGeometry(step) = operation {
                before = before.then(*step);
            }
        }
        let received = Stage {
            width: before.output_width,
            height: before.output_height,
        };
        let mut after = ExactGeometry::identity(received.width, received.height);
        for operation in &segment.operations[start..] {
            if let Processing::ExactGeometry(step) = operation {
                after = after.then(*step);
            }
        }
        let origin = |segment: &crate::render::Segment| {
            GpuPosition::translation(
                i64::from(segment.output_origin.0),
                i64::from(segment.output_origin.1),
            )
        };

        // Content: the boundary segment's colour operations from the boundary on. Their units
        // address the frame the segment writes, plus its origin in an uncut stage; a mask, the
        // stage its layer received, which is the boundary's through the exact steps between.
        let units = GpuPosition::of(after).then(origin(segment));
        let mut between = ExactGeometry::identity(received.width, received.height);
        let mut content_operations = Vec::new();
        for (index, operation) in segment.operations.iter().enumerate().skip(start) {
            match operation {
                Processing::ExactGeometry(step) => between = between.then(*step),
                Processing::Color(colour) => {
                    let layer = self.layer_at(first, index);
                    match plan_operation(
                        layer,
                        colour,
                        units,
                        GpuPosition::of(between),
                        qualifying,
                    )? {
                        Ok(operation) => content_operations.push(operation),
                        Err(fallback) => return Ok(Err(fallback)),
                    }
                }
                // Refused above; a spatial operation, resample or warp is an entry, never an
                // operation.
                Processing::PointReplace { .. }
                | Processing::Spatial(_)
                | Processing::Resample(_)
                | Processing::Warp(_) => {}
            }
        }

        // The spatial operations the boundary segment's frame enters, in order, at that frame's
        // stage, while it is the boundary's own texels: nothing but colour lies between them. Each
        // spatial segment's colour operations run on its output before the next one enters; a
        // spatial segment with exact steps of its own ends the chain, and a spatial entry after it
        // has no pass.
        let mut spatial: Vec<GpuSpatial> = Vec::new();
        let mut index = first + 1;
        while let Some(Some(Entry::Spatial(entry))) =
            self.segments.get(index).map(|next| &next.entry)
        {
            let layer = self.entry_layer(index);
            let previous = &self.segments[index - 1];
            let exact = |segment: &crate::render::Segment| {
                segment
                    .operations
                    .iter()
                    .any(|operation| matches!(operation, Processing::ExactGeometry(_)))
            };
            if after != ExactGeometry::identity(received.width, received.height)
                || (index > first + 1 && (exact(previous) || previous.stage() != received))
            {
                return Ok(Err(GpuFallback::SpatialUnit { layer }));
            }
            // The colour operations of the spatial segment before this one run between the two.
            if let Some(before) = spatial.last_mut() {
                for (position, operation) in previous.operations.iter().enumerate() {
                    if let Processing::Color(colour) = operation {
                        let at = ExactGeometry::identity(received.width, received.height);
                        match plan_operation(
                            self.layer_at(index - 1, position),
                            colour,
                            origin(previous),
                            GpuPosition::inverse(at),
                            qualifying,
                        )? {
                            Ok(operation) => before.after.push(operation),
                            Err(fallback) => return Ok(Err(fallback)),
                        }
                    }
                }
            }
            match plan_spatial(layer, entry, received, planning)? {
                Ok(planned) => {
                    // Each operation is a link of its own on the surface, which binds only its
                    // own applies' planes.
                    let planes = planned
                        .applies
                        .iter()
                        .map(|apply| apply.planes.len())
                        .sum::<usize>();
                    if planes > GPU_CHAIN_APPLY_PLANES {
                        return Ok(Err(GpuFallback::SpatialChain { layer }));
                    }
                    spatial.push(planned);
                }
                Err(fallback) => return Ok(Err(fallback)),
            }
            index += 1;
        }
        let content_end = first + spatial.len();

        // Every later segment: a resample or warp joins the tail; a spatial entry past the chain
        // has no pass. Colour in the last one runs over the output stage; in a chained spatial
        // segment before the last it ran between two spatial operations; anywhere else it has no
        // pass.
        let mut output_operations = Vec::new();
        for index in (first + 1..=last).filter(|index| *index >= content_end) {
            let segment = &self.segments[index];
            if index > content_end
                && let Some(Entry::Spatial(_)) = &segment.entry
            {
                return Ok(Err(GpuFallback::SpatialUnit {
                    layer: self.entry_layer(index),
                }));
            }
            for (position, operation) in segment.operations.iter().enumerate() {
                let Processing::Color(colour) = operation else {
                    continue;
                };
                let layer = self.layer_at(index, position);
                if index != last {
                    return Ok(Err(GpuFallback::BetweenResamples { layer }));
                }
                // A mask is read through the exact steps after the operation, backwards, as the
                // CPU's placement reads it.
                let mut suffix = ExactGeometry::identity(segment.width, segment.height);
                for operation in segment.operations[position + 1..].iter().rev() {
                    if let Processing::ExactGeometry(step) = operation {
                        suffix = step.then(suffix);
                    }
                }
                match plan_operation(
                    layer,
                    colour,
                    origin(segment),
                    GpuPosition::inverse(suffix),
                    qualifying,
                )? {
                    Ok(operation) => output_operations.push(operation),
                    Err(fallback) => return Ok(Err(fallback)),
                }
            }
        }

        // The tail: the exact steps after the boundary, then each later segment's entry and
        // geometry, as `transform_of` composes a whole stack's.
        let mut steps = Vec::new();
        let push_exact = |steps: &mut Vec<WarpStep>, step: ExactGeometry| -> Result<(), Error> {
            let inverse = Affine::from_exact(step).invert()?;
            if inverse != Affine::IDENTITY {
                steps.push(WarpStep::Affine(inverse.0));
            }
            Ok(())
        };
        push_exact(&mut steps, after)?;
        for segment in &self.segments[first + 1..] {
            if let Some(entry) = &segment.entry {
                entry.mapping_steps(&mut steps);
            }
            push_exact(&mut steps, segment.geometry)?;
        }
        let size = |stage: Stage| StageSize {
            width: stage.width,
            height: stage.height,
        };
        let map = GeometryMap::from_steps(size(received), size(self.stage()), steps)?;
        let reads = after.unmap_region(Region::whole(segment.stage()));
        // A layer that opens the next segment's stage boundary receives the frame this segment
        // wrote, which the CPU quantized: no colour run continues into it.
        let opens_boundary = start == segment.operations.len()
            && self
                .segments
                .get(first + 1)
                .is_some_and(|next| next.entry.is_some())
            && self.entry_layer(first + 1) == boundary;
        let continues_run = !opens_boundary
            && segment.operations[..start]
                .iter()
                .any(|operation| matches!(operation, Processing::Color(_)));
        Ok(Ok(GpuPlan {
            boundary: GpuBoundary {
                layer: boundary,
                stage: received,
                continues_run,
            },
            linear: planning.linear,
            content: content_operations,
            spatial,
            geometry: GpuGeometry {
                map,
                reads,
                clamps: last > content_end && !planning.linear,
            },
            output: output_operations,
            clipping: GpuClipping::of_output_codes(),
        }))
    }

    /// Every colour operation of this compilation by the layer it was compiled from, for the
    /// reference executor, which runs a shipped program through its CPU unit.
    #[cfg(test)]
    pub(super) fn colour_operations(&self) -> std::collections::BTreeMap<usize, ColorOperation> {
        let mut operations = std::collections::BTreeMap::new();
        for (index, segment) in self.segments.iter().enumerate() {
            for (position, operation) in segment.operations.iter().enumerate() {
                if let Processing::Color(colour) = operation {
                    operations.insert(self.layer_at(index, position), colour.clone());
                }
            }
        }
        operations
    }

    /// Every spatial operation of this compilation by the layer it was compiled from, for the
    /// reference executor, which runs a spatial step through its CPU units.
    #[cfg(test)]
    pub(super) fn spatial_operations(
        &self,
    ) -> std::collections::BTreeMap<usize, crate::modules::SpatialOperation> {
        let mut operations = std::collections::BTreeMap::new();
        for (index, segment) in self.segments.iter().enumerate().skip(1) {
            if let Some(Entry::Spatial(entry)) = &segment.entry {
                operations.insert(self.entry_layer(index), entry.operation.clone());
            }
        }
        operations
    }

    /// The layer whose compiled operation is operation `operation` of segment `segment`: the last
    /// layer that began at or before it, since a layer compiles to at most one operation and a
    /// neutral one begins where the next does.
    fn layer_at(&self, segment: usize, operation: usize) -> usize {
        self.layers
            .partition_point(|&start| start <= (segment, operation))
            .saturating_sub(1)
    }

    /// The layer whose stage boundary is segment `segment`'s entry: the one that began at the end
    /// of the segment before it.
    fn entry_layer(&self, segment: usize) -> usize {
        self.layer_at(segment - 1, self.segments[segment - 1].operations.len())
    }

    /// The layer of the first spatial operation a boundary in segment `first` is rendered through
    /// that lies behind an earlier spatial operation and prepares a global estimate `estimates`
    /// does not hold: a region's boundary reads such an estimate from the store alone, since no
    /// window can reduce its stage (`Render::region_boundary`). `None` when there is none.
    /// `O(segments + units)`, and reads no pixel.
    pub(crate) fn unheld_estimate(
        &self,
        first: usize,
        estimates: &GpuEstimates<'_>,
    ) -> Result<Option<usize>, Error> {
        for index in 1..=first.min(self.segments.len() - 1) {
            let Some(Entry::Spatial(entry)) = &self.segments[index].entry else {
                continue;
            };
            if entry.prepares_estimates()
                && self.spatial_before(index)
                && !estimates.holds(entry, self.segments[index - 1].stage())?
            {
                return Ok(Some(self.entry_layer(index)));
            }
        }
        Ok(None)
    }
}

/// One colour operation of layer `layer` as plan data: every unit's program, the map to the
/// coordinate its units address and, for a masked one, the map to its mask's stage.
fn plan_operation(
    layer: usize,
    operation: &ColorOperation,
    position: GpuPosition,
    mask_position: GpuPosition,
    qualifying: bool,
) -> Result<Planned<GpuOperation>, Error> {
    let admit = |description: &GpuDescription, kind: GpuProgramKind| -> Result<bool, Error> {
        if description.program.kind != kind || !description.well_formed() {
            return Err(Error::internal(format!(
                "GPU program {} is described as a {kind:?} with {} words, against its {:?} and {}",
                description.program.entry,
                description.words.len(),
                description.program.kind,
                description.program.words
            )));
        }
        Ok(description.program.enabled || qualifying)
    };
    let mut units = Vec::with_capacity(operation.len());
    for unit in operation.units() {
        let Some(description) = unit.gpu() else {
            return Ok(Err(GpuFallback::NoProgram {
                layer,
                unit: unit.describe(),
            }));
        };
        if !admit(&description, GpuProgramKind::Colour)? {
            return Ok(Err(GpuFallback::DisabledProgram {
                layer,
                program: description.program.entry,
            }));
        }
        units.push(description);
    }
    let mask = match operation.mask() {
        None => None,
        Some(field) => {
            let mut mask = match field.gpu() {
                Ok(mask) => mask,
                Err(kind) => {
                    return Ok(Err(GpuFallback::NoProgram {
                        layer,
                        unit: format!("a {kind} mask component"),
                    }));
                }
            };
            for component in &mask.components {
                if !admit(&component.program, GpuProgramKind::Coverage)? {
                    return Ok(Err(GpuFallback::DisabledProgram {
                        layer,
                        program: component.program.program.entry,
                    }));
                }
            }
            mask.position = mask_position.then(mask.position);
            Some(mask)
        }
    };
    Ok(Ok(GpuOperation {
        layer,
        units,
        position,
        mask,
    }))
}

/// The spatial entry of layer `layer`, at the `stage` of the frame it reads, as plan data: every
/// unit's description, each global estimate the store holds for it, and its mask.
fn plan_spatial(
    layer: usize,
    entry: &SpatialEntry,
    stage: Stage,
    planning: Planning<'_>,
) -> Result<Planned<GpuSpatial>, Error> {
    let operation = &entry.operation;
    let mut units = Vec::with_capacity(operation.len());
    let mut held = false;
    for unit in operation.units() {
        let global = match (unit.estimate_key(), &planning.estimates) {
            (Some(key), Some(estimates)) => match estimates.stored(entry, stage, &key)? {
                Some(global) => Some(global),
                None => match planning
                    .held
                    .iter()
                    .find(|prefix| prefix.layer == layer)
                    .filter(|_| unit.holds_restored_estimate())
                {
                    Some(prefix) => {
                        let global = estimates
                            .cached(&prefix.prefix_hash, stage, &key)?
                            .flatten();
                        held |= global.is_some();
                        global
                    }
                    None => None,
                },
            },
            _ => None,
        };
        let Some(description) = unit.gpu(global.as_ref()) else {
            return Ok(Err(GpuFallback::SpatialUnit { layer }));
        };
        if let Some(fallback) = spatial::admit(layer, &description, planning.qualifying)? {
            return Ok(Err(fallback));
        }
        units.push(description);
    }
    let mask = match operation.mask() {
        None => None,
        Some(field) => {
            let mask = match field.gpu() {
                Ok(mask) => mask,
                Err(kind) => {
                    return Ok(Err(GpuFallback::NoProgram {
                        layer,
                        unit: format!("a {kind} mask component"),
                    }));
                }
            };
            for component in &mask.components {
                let program = component.program.program;
                if program.kind != GpuProgramKind::Coverage || !component.program.well_formed() {
                    return Err(Error::internal(format!(
                        "GPU program {} is described as a coverage component against its {:?}",
                        program.entry, program.kind
                    )));
                }
                if !program.enabled && !planning.qualifying {
                    return Ok(Err(GpuFallback::DisabledProgram {
                        layer,
                        program: program.entry,
                    }));
                }
            }
            // The operation opens its segment at the stage its layer received, which is the
            // mask's own: the boundary's texels are the mask's pixels.
            Some(mask)
        }
    };
    let mut composed = spatial::compose(layer, units, !planning.linear, mask)?;
    composed.halos = operation.halos(stage);
    composed.held = held;
    Ok(Ok(composed))
}
