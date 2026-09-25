//! The host's compiled mask: what one pixel's coverage is, where coverage can be non-zero at all,
//! and the table of component kinds this build can evaluate.
//!
//! A mask is a host object in the recipe rather than a module's state
//! (`docs/design/masking.md`), so compiling one lives here beside the module registry and not inside
//! any module. A [`CompiledMask`] is built from a stored [`Mask`] and the [`Stage`] its layer
//! receives, and it is **pure**: coverage at a pixel is a function of that pixel's position, that
//! pixel's own value and the stored payloads, and of nothing else. That is what lets a rasterizing
//! pass and `render.sample` agree by construction rather than by care — both reach it through the
//! same call with the same arguments.
//!
//! **The pixel is the second argument, and it is the input of the operation the mask modulates**
//! (proposal P12 of `docs/design/range-study.md`, decided there). A geometric component ignores it
//! and is bit-identical across the change; a range selection is answerable from nothing else, and a
//! second entry point for it would double the contract for one argument. What the value-based kinds
//! then cost — a whole-stage rectangle, no thin-feature supersample, and a selection that moves when
//! a layer ahead of it is reordered — is stated in [`range`] and in the user guide rather than
//! hidden.
//!
//! Transcription. This file is the production transcription of the frozen mathematics in
//! `docs/design/mask-study.md` and of the independent `f64` reference at
//! `crates/luxforge-core/tests/reference/mask.rs`; the three must be read together, and every
//! expression on the per-pixel path is written in the same form and the same order as the
//! reference's, so coverage is not merely within tolerance of it but **bit-identical** for the same
//! payloads, stage and pixel. The study's rule that makes that checkable is that any value which
//! does not depend on the pixel may be precomputed and no arithmetic on the per-pixel path may be
//! rewritten: so `amount / 100`, the aspect ratio and each component's own compile-time terms are
//! hoisted here, and there is no fused multiply-add, no reassociated dot product, no precomputed
//! reciprocal in place of a division, no Horner-form [`smooth`], and no component inversion folded
//! into a falloff instead of the one `1 - c` the composition performs.
//!
//! Two spellings of the mask-space map are frozen and are not interchangeable. This unit uses the
//! **pixel-centre** spelling — `u = (px + 0.5) / H`, `v = (py + 0.5) / H` — because it answers for a
//! pixel of the stage it was compiled against; compiling a stored payload uses the **stored
//! position** spelling, `u = x · W/H`, `v = y`. They agree to `2.220e-16` and differ in their last
//! bits, so mixing them would cost bit-identity while staying within tolerance.
//!
//! Memory. Nothing here scales with the stage's area: a compiled mask holds one entry per component
//! and no mask plane is ever allocated, at any size. That is the point-query rule
//! (`docs/engineering/performance-rules.md`), not an optimization — a mask that could not be
//! answered for one pixel in bounded time would make a sampled byte unable to equal a rendered one.
use crate::{
    Component, ComponentMode, Error, ErrorKind, Mask, ParameterDescriptor,
    modules::{Region, Stage},
    path::StrokeTable,
};

mod brush;
#[cfg(test)]
mod command_contracts;
/// The `mask.*` host command family: what each command declares, does and labels.
pub mod commands;
mod linear;
mod parameters;
mod radial;
mod range;

pub use brush::{BrushStrokes, SEGMENTS_PER_PIXEL, STROKES_PER_COMPONENT};
pub use linear::{LinearGradient, POSITION_MAX, POSITION_MIN};
pub use radial::{ANGLE_MAX, ANGLE_MIN, FEATHER_MAX, FEATHER_MIN, RadialGradient};
pub use range::{
    ColourRange, FEATHER_DEFAULT, LEVEL_MAX, LEVEL_MIN, LuminanceRange, MAX_SAMPLES, PLATEAU,
    RADIUS_MAX, RADIUS_MIN, RANGE_FEATHER_MAX, RANGE_FEATHER_MIN, REFINE_DEFAULT, REFINE_MAX,
    REFINE_MIN, SAMPLE_MAX, SAMPLE_MIN, SPAN, refine_radius,
};

/// The payload field a sampling kind keeps its sampled colours in, the way
/// [`crate::path::STROKES_FIELD`] is the field a drawn kind keeps its stroke addresses in.
///
/// One spelling, read by the generated sample methods and written by every sampling kind's own
/// payload, so the command family can edit the list without knowing what else that payload holds.
pub const SAMPLES_FIELD: &str = "samples";

/// The component kind a stroke reaches: the one kind whose geometry is a drawn path rather than
/// declared numbers.
///
/// `mask.add-stroke` creates a component of this kind and appends to one, and refuses a component of
/// any other kind by name — it cannot be read from [`component_geometry_is_drawn`] alone, which says
/// *whether* a kind is drawn and not *which* kind a drawn path belongs to. It is the kind table's
/// own token, so the command family still spells no kind of its own.
pub const BRUSH: &str = brush::KIND;

/// The smallest legal stored distance, in mask-space units, where one unit is the content stage's
/// height. Every falloff divides by a stored distance, so this floor is what replaces a runtime
/// guard against a vanishing divisor: it bounds every such division by `1e4`. It is below one pixel
/// on every admissible stage — 0.4 px at 4000 px of height, 1.6 px at the 16384 px per-side limit —
/// so no distance a gesture can draw is excluded by it.
pub const DISTANCE_MIN: f64 = 1e-4;

/// The largest legal stored distance, in mask-space units. A distance of `sqrt((W/H)² + 1)` already
/// covers the whole stage from any point, so `64` covers every aspect ratio up to 63.99:1, far
/// beyond anything the per-side limit can produce as a photograph.
pub const DISTANCE_MAX: f64 = 64.0;

/// The frozen easing, `smooth(s) = s²(3 − 2s)`, on `s` the caller has already clamped to `[0, 1]`.
///
/// `smooth(0)` is exactly `0.0` and `smooth(1)` exactly `1.0` in `f64`, which is what lets a clamped
/// one-branch falloff reproduce its limits exactly instead of approaching them. It is the same
/// falloff the delivered vignette froze (written there as `3t² − 2t³`, the same polynomial), so the
/// editor has one falloff shape rather than two that differ for no reason a person could name. The
/// spelling is load-bearing: an algebraically equal rewrite, Horner's form included, is within
/// tolerance of the reference and not bit-identical to it.
fn smooth(s: f64) -> f64 {
    s * s * (3.0 - 2.0 * s)
}

/// One entry of the host's component-kind table: the token a stored component carries, the parser
/// that turns that component's payload into geometry this build can evaluate, and the parameters
/// that payload's fields are declared as.
///
/// This is the **one** table describing a component kind. It is what makes retention of an unknown
/// kind work — a component's `kind` and `payload` are to a component what `effect_id` and `payload`
/// are to a layer, so a kind no entry here claims is refused by name and its bytes are left exactly
/// as they were read — and it is equally what makes a *known* kind reachable: the `mask.*` command
/// family generates `mask.create-<kind>`, `mask.add-<kind>` and `mask.set-<kind>` from these rows,
/// each declaring exactly the parameters [`ComponentKind::parameters`] returns. Registering a kind is
/// therefore sufficient to make it evaluable, and sufficient to make it creatable, addable and
/// patchable **when it declares geometry**; there is no second table to remember. A row whose
/// `parameters` is `None` declares none and generates none, which is what [`declared_geometry_kinds`]
/// filters on and what a brush is: registering is necessary, not sufficient, and the one kind that
/// shows the difference is the one whose shape is drawn rather than typed.
struct ComponentKind {
    kind: &'static str,
    parse: fn(&Component) -> Result<Geometry, Error>,
    /// The kind's declared geometry, from its own module beside its parser. `required` is false for
    /// the patch method, where every field is optional.
    ///
    /// `None` for a kind whose geometry is **drawn** rather than typed. A brush's geometry is a list
    /// of strokes captured by a gesture and stored by hash: there is no number a control could edit
    /// and no honest `mask.create-brush` to generate from an empty parameter list, so such a kind
    /// registers its parser and its evaluation here and brings its own commands instead. Every kind
    /// is evaluable; only the ones with declared geometry are generated over.
    parameters: Option<fn(bool) -> Vec<ParameterDescriptor>>,
    /// The colours this kind samples from the photograph, for a kind that holds a list of them.
    ///
    /// A list is the one shape the closed parameter vocabulary cannot declare, so a kind that holds
    /// one says so here and the command family generates `mask.add-<kind>-sample` and
    /// `mask.delete-<kind>-sample` over it — one swatch at a time, which is also how a person edits
    /// it. `None` for a kind that samples nothing, which is every geometric kind.
    samples: Option<ColourSamples>,
    /// Whether this kind's coverage is a function of the **pixel value** the masked operation
    /// receives rather than of the pixel's position.
    ///
    /// A geometric kind is `false`; the two range selections are `true`. It is declared here because
    /// four behaviours follow from it that a client has to be able to name before it draws a control:
    /// the conservative rectangle is the whole stage, no proxy frame is marked approximate, the
    /// coverage overlay reads the masked operation's own input rather than position alone, and what
    /// such a component selects moves when a layer ahead of it changes the operation's input. A panel
    /// reads this rather than matching on kind
    /// tokens of its own, so a kind registered later carries the same statement without the panel
    /// being edited.
    ///
    /// It is the *kind's* answer and not one component's: a brush is `false` here and still answers
    /// [`CompiledMask::reads_pixels`] in the affirmative when one of its strokes is held to a colour,
    /// because that is a property of the stroke and not of the kind.
    /// `a_value_based_kind_is_exactly_one_that_reads_the_pixel` checks the two against each other.
    value_based: bool,
    /// What this kind does **not** select, in its own terms, for a client to show where a person
    /// would otherwise assume otherwise.
    ///
    /// It sits in the table beside the parser for the same reason the declared parameters do: a kind
    /// knows what its own axis cannot tell apart, and a panel that kept the sentence instead would
    /// have to match on kind tokens. Empty for a position-based kind — a gradient and a brush select
    /// where they were drawn, and no value in the picture changes that. Each line is one caption, is
    /// phrased as a remedy rather than only as a warning, and quotes the
    /// `docs/design/range-study.md` figure it rests on. [`component_kind_limits`] appends the one line
    /// every value-based kind shares, so that sentence is written once and a kind registered later
    /// carries it without this table being edited.
    limits: &'static [&'static str],
}

/// The one limit every value-based kind has, whatever its axis: it reads the input of the operation
/// it modulates, which is what makes it move under a reordering and makes the 100% view the only
/// place it can be read exactly.
///
/// The overlay reads that same input — the mask's first bound layer's, once per display cell
/// ([proposal P16](../../../docs/design/range-study.md#proposals), decided and built) — so the
/// sentence no longer says there is none. It still says where the truth is, because at Fit the cell's
/// own pixel is a downscaled one and coverage of the average is not the average of coverages.
const VALUE_BASED_LIMIT: &str = "Read on this layer's own input, so a layer above it changes what this selects · the overlay reads that input too, and at Fit it reads a downscaled pixel: the 100% view is the truth";

/// How many colours one component of a sampling kind holds, and what one of them declares.
struct ColourSamples {
    /// The most colours one component of this kind holds. A product bound, refused by name.
    max: usize,
    /// What one sampled colour declares: the parameters `mask.add-<kind>-sample` takes.
    parameters: fn() -> Vec<ParameterDescriptor>,
}

/// Every component kind this build knows. A later kind — a brush, a range selection — is one more
/// entry with its own module beside `linear` and `radial`, and nothing else here changes.
const COMPONENT_KINDS: &[ComponentKind] = &[
    ComponentKind {
        kind: linear::KIND,
        parse: parse_linear,
        parameters: Some(linear::parameters),
        samples: None,
        value_based: false,
        limits: &[],
    },
    ComponentKind {
        kind: radial::KIND,
        parse: parse_radial,
        parameters: Some(radial::parameters),
        samples: None,
        value_based: false,
        limits: &[],
    },
    ComponentKind {
        kind: brush::KIND,
        parse: parse_brush,
        parameters: None,
        samples: None,
        value_based: false,
        limits: &[],
    },
    ComponentKind {
        kind: range::LUMINANCE_KIND,
        parse: parse_luminance_range,
        parameters: Some(range::luminance_parameters),
        samples: None,
        value_based: true,
        limits: range::LUMINANCE_LIMITS,
    },
    ComponentKind {
        kind: range::COLOUR_KIND,
        parse: parse_colour_range,
        parameters: Some(range::colour_parameters),
        samples: Some(ColourSamples {
            max: range::MAX_SAMPLES,
            parameters: range::colour_sample_parameters,
        }),
        value_based: true,
        limits: range::COLOUR_LIMITS,
    },
];

fn parse_luminance_range(component: &Component) -> Result<Geometry, Error> {
    range::parse_luminance(component).map(Geometry::LuminanceRange)
}

fn parse_colour_range(component: &Component) -> Result<Geometry, Error> {
    range::parse_colour(component).map(Geometry::ColourRange)
}

fn parse_linear(component: &Component) -> Result<Geometry, Error> {
    linear::parse(component).map(Geometry::Linear)
}

fn parse_radial(component: &Component) -> Result<Geometry, Error> {
    radial::parse(component).map(Geometry::Radial)
}

fn parse_brush(component: &Component) -> Result<Geometry, Error> {
    brush::parse(component).map(Geometry::Brush)
}

/// Whether this build can evaluate `kind`, which is the question the refusal below answers in the
/// negative. It reads the table rather than a second list, so the two cannot drift.
pub fn knows_component_kind(kind: &str) -> bool {
    COMPONENT_KINDS.iter().any(|entry| entry.kind == kind)
}

/// One kind's display name, as the host itself writes it into a component's name: `linear` reads
/// `Linear`, `luminance-range` reads `Luminance range`. It is the delivered sentence-case rule the
/// rest of the editor's titles take, so a two-word kind reads as a phrase and not as a proper noun.
/// A client offering the kinds names them with this rather than a table of its own, so what a button
/// says and what the committed component is called cannot disagree.
pub fn kind_title(kind: &str) -> String {
    crate::modules::title_case(kind)
}

/// Every component kind this build knows, in table order: every kind it can parse, evaluate, bound
/// and retain.
pub fn component_kinds() -> impl Iterator<Item = &'static str> {
    COMPONENT_KINDS.iter().map(|entry| entry.kind)
}

/// The kinds whose geometry is a set of declared numbers, in table order. The command family
/// generates `mask.create-<kind>`, `mask.add-<kind>` and `mask.set-<kind>` from exactly this list,
/// and the panel generates their number fields from the same declarations — so what a client can
/// *type* is what a control can edit. A kind whose geometry is drawn is evaluable without being
/// generated over, and says so by declaring no parameters.
pub fn declared_geometry_kinds() -> impl Iterator<Item = &'static str> {
    COMPONENT_KINDS
        .iter()
        .filter(|entry| entry.parameters.is_some())
        .map(|entry| entry.kind)
}

/// Whether one command of `kind` can be created with no arguments at all, because every field its
/// geometry declares carries a default.
///
/// It is what lets a client offer a kind that has no canvas gesture: a gradient is *drawn* and a
/// range selection is *typed*, and a typed kind is created by a button and then edited through the
/// number fields its own declarations generate. A kind with a field and no default is neither, and
/// says so rather than putting up a button with nothing behind it.
pub fn component_geometry_is_defaulted(kind: &str) -> bool {
    component_parameters(kind, true).is_some_and(|parameters| {
        !parameters.is_empty()
            && parameters
                .iter()
                .all(|parameter| parameter.default.is_some())
    })
}

/// Whether `kind`'s coverage is a function of the pixel value the masked operation receives rather
/// than of the pixel's position.
///
/// A client asks this to say what such a component does *not* do before a person has drawn anything
/// with it: its coverage overlay is read on the masked operation's own input rather than on position
/// alone, so at Fit it is read on a downscaled pixel and the 100% view is the truth; its conservative
/// rectangle is the whole stage; and what it selects moves when a layer ahead of it changes the
/// operation's input. It reads the kind table, so
/// a kind registered later carries the same statement without a second list to keep in step, and it
/// is the kind's answer rather than one component's — a brush is position-based here and still reads
/// the pixel when one of its strokes is held to a colour.
pub fn component_kind_is_value_based(kind: &str) -> bool {
    COMPONENT_KINDS
        .iter()
        .any(|entry| entry.kind == kind && entry.value_based)
}

/// What `kind` does **not** select: the kind's own lines from the table, then the one line every
/// value-based kind shares.
///
/// Empty for a position-based kind and for a kind this build does not know — there is nothing honest
/// to say about a kind whose payload cannot be parsed, and such a component is already named as
/// unavailable. A client shows these where a person is choosing or editing a component of that kind,
/// which is where the assumption they correct is made.
pub fn component_kind_limits(kind: &str) -> Vec<&'static str> {
    let Some(entry) = COMPONENT_KINDS.iter().find(|entry| entry.kind == kind) else {
        return Vec::new();
    };
    let mut limits = entry.limits.to_vec();
    if entry.value_based {
        limits.push(VALUE_BASED_LIMIT);
    }
    limits
}

/// Whether this build draws `kind`'s geometry rather than declaring it as numbers. A drawn kind has
/// no generated geometry method and no number field, which is a fact a refusal has to be able to
/// state.
pub fn component_geometry_is_drawn(kind: &str) -> bool {
    COMPONENT_KINDS
        .iter()
        .any(|entry| entry.kind == kind && entry.parameters.is_none())
}

/// One kind's declared geometry parameters, or none when this build does not know the kind.
///
/// `required` is false for a patch method, where every field is optional and the ones a request
/// names are merged over the stored payload.
pub fn component_parameters(kind: &str, required: bool) -> Option<Vec<ParameterDescriptor>> {
    COMPONENT_KINDS
        .iter()
        .find(|entry| entry.kind == kind)
        .and_then(|entry| entry.parameters)
        .map(|parameters| parameters(required))
}

/// The kinds that sample colours from the photograph, in table order. The command family generates
/// `mask.add-<kind>-sample` and `mask.delete-<kind>-sample` from exactly this list, so a kind that
/// holds swatches becomes samplable by being registered.
pub fn sampling_kinds() -> impl Iterator<Item = &'static str> {
    COMPONENT_KINDS
        .iter()
        .filter(|entry| entry.samples.is_some())
        .map(|entry| entry.kind)
}

/// The most colours one component of `kind` holds, or none when the kind samples nothing.
pub fn component_sample_limit(kind: &str) -> Option<usize> {
    COMPONENT_KINDS
        .iter()
        .find(|entry| entry.kind == kind)
        .and_then(|entry| entry.samples.as_ref())
        .map(|samples| samples.max)
}

/// What one sampled colour of `kind` declares, or none when the kind samples nothing.
pub fn component_sample_parameters(kind: &str) -> Option<Vec<ParameterDescriptor>> {
    COMPONENT_KINDS
        .iter()
        .find(|entry| entry.kind == kind)
        .and_then(|entry| entry.samples.as_ref())
        .map(|samples| (samples.parameters)())
}

/// One component's stored geometry, validated but not yet bound to a stage. Everything checkable
/// without a stage is checked here: the kind is known, the payload has the shape its kind declares,
/// and every stored position is finite and inside the legal range. What needs a stage — an axis
/// length, which is a mask-space distance and therefore depends on the aspect ratio — is checked
/// when the component is compiled.
#[derive(Clone, Debug)]
enum Geometry {
    Linear(LinearGradient),
    Radial(RadialGradient),
    Brush(BrushStrokes),
    LuminanceRange(LuminanceRange),
    ColourRange(ColourRange),
}

impl Geometry {
    fn compile(
        self,
        stage: Stage,
        strokes: &StrokeTable,
        mask: &str,
        name: &str,
    ) -> Result<CompiledGeometry, Error> {
        match self {
            Self::Linear(gradient) => {
                linear::Compiled::new(gradient, stage, name).map(CompiledGeometry::Linear)
            }
            // Nothing about a radial's legality depends on the stage — a stored radius is a
            // mask-space distance as written, unlike an axis whose length is a projection through
            // the aspect ratio — so compiling one cannot fail and has nothing to name.
            Self::Radial(gradient) => Ok(CompiledGeometry::Radial(radial::Compiled::new(
                gradient, stage,
            ))),
            // A brush resolves its stroke references against the store the recipe was read from,
            // checks each radius against the study's distance rule and builds its grid index, and
            // every one of those can refuse — by name, and before any pixel is read.
            Self::Brush(held) => {
                brush::Compiled::new(&held, stage, strokes, mask, name).map(CompiledGeometry::Brush)
            }
            // Nothing about a range selection's legality depends on the stage: its numbers live on
            // an axis the picture defines and not on the frame, so the parser has already checked
            // everything there is to check and compiling one cannot fail.
            Self::LuminanceRange(band) => Ok(CompiledGeometry::LuminanceRange(
                range::CompiledLuminance::new(band),
            )),
            Self::ColourRange(colours) => Ok(CompiledGeometry::ColourRange(
                range::CompiledColour::new(&colours),
            )),
        }
    }
}

/// One component's geometry bound to a stage, with every term that does not depend on the pixel
/// already computed.
#[derive(Clone, Debug)]
enum CompiledGeometry {
    Linear(linear::Compiled),
    Radial(radial::Compiled),
    Brush(brush::Compiled),
    LuminanceRange(range::CompiledLuminance),
    ColourRange(range::CompiledColour),
}

impl CompiledGeometry {
    /// The component's own falloff at a mask-space point, before its inversion and before the
    /// composition. This is the per-pixel path: it is the reference's spelling, in the reference's
    /// order.
    fn coverage(&self, u: f64, v: f64, rgb: [f64; 3]) -> f64 {
        match self {
            // A geometric component ignores `rgb`. That is the whole of what proposal P12 costs
            // them: the same expressions on the same `(u, v)`, so their coverage is bit-identical
            // across the change and the frozen references still hold bit for bit.
            Self::Linear(linear) => linear.coverage(u, v),
            Self::Radial(radial) => radial.coverage(u, v),
            // A brush ignores `rgb` for every stroke that is not limited to a colour, and a limited
            // one multiplies its own coverage by the similarity frozen in
            // `docs/design/mask-study.md#the-colour-constraint`.
            Self::Brush(brush) => brush.coverage(u, v, rgb),
            // A value-based component ignores the position instead.
            Self::LuminanceRange(band) => band.coverage(rgb),
            Self::ColourRange(colours) => colours.coverage(rgb),
        }
    }

    /// Whether this component's coverage depends on the pixel's value rather than on its position.
    /// It is what makes a mask's bounds the whole stage, what decides whether the coverage overlay
    /// needs the masked operation's input, and what a client is told so it can say the 100% view is
    /// the truth for such a selection.
    fn reads_pixels(&self) -> bool {
        match self {
            Self::Linear(_) | Self::Radial(_) => false,
            // A brush reads pixels exactly when one of its strokes is limited to a colour. That is
            // the whole of what the colour-constrained brush costs the rest of the mask, and it is
            // the range selections' cost too: the overlay has to be handed the masked operation's
            // input before it can draw such a mask, and what the stroke paints moves when a layer
            // ahead of the masked one changes that input.
            Self::Brush(brush) => brush.reads_pixels(),
            Self::LuminanceRange(_) | Self::ColourRange(_) => true,
        }
    }

    /// A conservative pixel rectangle of this component's own support: outside it the component's
    /// coverage, already carrying `inverted`, is exactly zero.
    fn support(&self, stage: Stage, inverted: bool) -> Region {
        match self {
            Self::Linear(linear) => linear.support(stage, inverted),
            Self::Radial(radial) => radial.support(stage, inverted),
            Self::Brush(brush) => brush.support(stage, inverted),
            // Stated, not guessed: a value-based component's coverage can be non-zero at any pixel
            // of the frame, drawn or inverted, so no rectangle smaller than the stage is correct.
            Self::LuminanceRange(_) | Self::ColourRange(_) => range::value_support(stage),
        }
    }

    /// The smallest feature this component draws at `stage`, in that stage's pixels.
    fn feature_px(&self, stage: Stage) -> f64 {
        match self {
            Self::Linear(linear) => linear.feature_px(stage),
            Self::Radial(radial) => radial.feature_px(stage),
            Self::Brush(brush) => brush.feature_px(stage),
            Self::LuminanceRange(_) | Self::ColourRange(_) => range::value_feature_px(stage),
        }
    }
}

/// One compiled component: its mode, its own inversion and its stage-bound geometry.
#[derive(Clone, Debug)]
struct CompiledComponent {
    mode: ComponentMode,
    invert: bool,
    geometry: CompiledGeometry,
}

impl CompiledComponent {
    /// This component's coverage with its inversion applied. The inversion is the one `1 - c` the
    /// composition performs, never a sign flip or a swapped clamp inside the falloff: those are
    /// within tolerance of the reference and not bit-identical to it.
    fn coverage(&self, u: f64, v: f64, rgb: [f64; 3]) -> f64 {
        let c = self.geometry.coverage(u, v, rgb);
        if self.invert { 1.0 - c } else { c }
    }
}

/// A mask compiled against the stage its layer receives.
///
/// Pure: [`CompiledMask::coverage`] reads nothing but its own compiled terms, the position it is
/// asked about and that pixel's own value, so the rasterizing pass and `render.sample` cannot
/// disagree. Bounded by the component count and never by the stage: no mask plane is allocated at
/// any size.
#[derive(Clone, Debug)]
pub struct CompiledMask {
    /// The stage this mask was compiled against, part of the identity of the compiled thing because
    /// two masks with equal payloads on different stages are different coverage fields.
    stage: Stage,
    /// `f64::from(stage.height)`: the single divisor of the pixel-centre spelling, hoisted so the
    /// per-pixel path converts nothing it does not have to.
    height: f64,
    /// `amount / 100`, hoisted. It is applied as the study's final multiply and is never collapsed
    /// into a scale applied earlier in the fold.
    scale: f64,
    invert: bool,
    components: Vec<CompiledComponent>,
    bounds: Region,
}

impl CompiledMask {
    /// Compile `mask` against the stage its layer receives.
    ///
    /// Cost is `O(components)` and independent of the stage's size: each component parses its own
    /// payload, computes the handful of terms that do not depend on a pixel, and contributes its
    /// conservative rectangle in closed form. Nothing here reads a pixel or allocates a frame.
    ///
    /// Every refusal names what it refused: a component kind this build does not know is
    /// `incompatible`, and a payload that is malformed, out of range or geometrically degenerate is
    /// `validation` naming the component and the field.
    ///
    /// `strokes` is the recipe's resolved stroke table, which the drawn kinds read and the typed
    /// kinds ignore. It is a parameter rather than something a component carries because the strokes
    /// are the *recipe's*: a component holds addresses and never coordinates, so the table is what
    /// turns those addresses into geometry, and a reference it cannot answer refuses here — with the
    /// store's own message, before any pixel is read.
    pub fn new(mask: &Mask, stage: Stage, strokes: &StrokeTable) -> Result<Self, Error> {
        if stage.width == 0 || stage.height == 0 {
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "mask {} cannot be compiled against an empty {}x{} stage",
                    mask.name, stage.width, stage.height
                ),
            ));
        }
        let mut components = Vec::with_capacity(mask.components.len());
        for component in &mask.components {
            let geometry =
                parse_component(component)?.compile(stage, strokes, &mask.name, &component.name)?;
            components.push(CompiledComponent {
                mode: component.mode,
                invert: component.invert,
                geometry,
            });
        }
        let scale = mask.amount / 100.0;
        let bounds = compose_bounds(&components, stage, mask.invert, scale);
        Ok(Self {
            stage,
            height: f64::from(stage.height),
            scale,
            invert: mask.invert,
            components,
            bounds,
        })
    }

    /// The composed coverage `M` at the centre of stage pixel `(x, y)`, in `f64`.
    ///
    /// The **pixel-centre** spelling of mask space, then the frozen Zadeh fold, in the reference's
    /// order:
    ///
    /// ```text
    /// u = (px + 0.5) / H
    /// v = (py + 0.5) / H
    /// m = 0
    /// for component in components:
    ///     c = the component's falloff at (u, v)
    ///     c = 1 - c                       if component.invert
    ///     m = max(m, c)                   if mode == add
    ///     m = min(m, 1 - c)               if mode == subtract
    ///     m = min(m, c)                   if mode == intersect
    /// m = 1 - m                           if mask.invert
    /// M = (amount / 100) * m
    /// ```
    ///
    /// `max` and `min` introduce no rounding at all, so the only rounding the algebra itself
    /// contributes is one `1 - c` per inversion or subtraction. A mask with no components composes
    /// to `m = 0`: an empty mask selects nothing rather than everything.
    ///
    /// This is the `f64` field [`Self::evaluate`] narrows, and it is what the study's reference is
    /// compared against bit for bit. A pixel outside the compiled stage is not a special case: the
    /// field is total, and callers address their own stage.
    pub fn coverage(&self, x: u32, y: u32, rgb: [f64; 3]) -> f64 {
        let u = (f64::from(x) + 0.5) / self.height;
        let v = (f64::from(y) + 0.5) / self.height;
        let mut m: f64 = 0.0;
        for component in &self.components {
            let c = component.coverage(u, v, rgb);
            m = match component.mode {
                ComponentMode::Add => m.max(c),
                ComponentMode::Subtract => m.min(1.0 - c),
                ComponentMode::Intersect => m.min(c),
            };
        }
        let m = if self.invert { 1.0 - m } else { m };
        self.scale * m
    }

    /// The composed coverage at stage pixel `(x, y)`, narrowed to the `f32` a blend multiplies by.
    /// The narrowing is the last step, after the whole `f64` fold, so the value a masked run uses is
    /// the nearest `f32` to the frozen field rather than the product of an `f32` composition.
    pub fn evaluate(&self, x: u32, y: u32, rgb: [f32; 3]) -> f32 {
        self.coverage(
            x,
            y,
            [f64::from(rgb[0]), f64::from(rgb[1]), f64::from(rgb[2])],
        ) as f32
    }

    /// Whether any component of this mask reads the pixel's value rather than its position.
    ///
    /// A caller decides from this whether it needs a pixel at all: the coverage overlay reads the
    /// masked operation's input once per display cell for a mask that answers yes, and refuses the
    /// grid when no operation is bound to read one from, because the honest answer for such a mask is
    /// not a grid of zeros. It is also what a panel reads to say that a range selection is evaluated
    /// on what the current view can see and that the 100% view is the truth. `O(components)` and
    /// reads nothing.
    pub fn reads_pixels(&self) -> bool {
        self.components
            .iter()
            .any(|component| component.geometry.reads_pixels())
    }

    /// A conservative rectangle of the compiled stage outside which coverage is **exactly** zero.
    ///
    /// This is what makes a small mask cheap on a large frame: a colour run skips those spans and a
    /// spatial tiling copies those tiles. It is composed the way coverage is, one component at a
    /// time, from the fact that the algebra's `max` only grows the support and its `min` only
    /// shrinks it — so an `add` unions the rectangles, a `subtract` leaves the rectangle alone and
    /// an `intersect` intersects them. A whole-mask inversion makes coverage non-zero almost
    /// everywhere, so the rectangle becomes the whole stage; an `amount` of exactly zero makes it
    /// empty, because the final multiply is then exactly `0.0` at every pixel.
    pub fn bounds(&self) -> Region {
        self.bounds
    }

    /// The smallest feature this mask draws at `stage`, in that stage's pixels: the narrowest
    /// transition any one component contributes. The proxy path compares it against two pixels to
    /// decide whether evaluating the mask at proxy size aliases.
    ///
    /// `stage` is a parameter rather than the compiled stage because the question is asked *about* a
    /// stage — usually a proxy of this one — before a mask is compiled against it, and the stored
    /// geometry is normalized, so the same payloads answer for any stage. A mask with no components
    /// draws no feature at all and answers `f32::INFINITY`: there is nothing for a pixel grid to
    /// miss. Cost is `O(components)`.
    pub fn min_feature_px(&self, stage: Stage) -> f32 {
        let mut smallest = f64::INFINITY;
        for component in &self.components {
            smallest = smallest.min(component.geometry.feature_px(stage));
        }
        smallest as f32
    }

    /// The stage this mask was compiled against.
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// How many components this mask composes, which is the whole of its per-pixel cost.
    pub fn components(&self) -> usize {
        self.components.len()
    }
}

/// Parse one stored component through the host's kind table.
///
/// A kind no entry claims is `incompatible`, naming the kind as stored. It is not a validation
/// error: the stack is well formed and this build simply cannot draw part of it, which is a reason
/// to refuse the stack whole and keep every byte, not to render something else.
fn parse_component(component: &Component) -> Result<Geometry, Error> {
    let entry = COMPONENT_KINDS
        .iter()
        .find(|entry| entry.kind == component.kind)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Incompatible,
                format!("unknown mask component {}", component.kind),
            )
        })?;
    (entry.parse)(component)
}

/// Every component of one stored mask, parsed by the kind table without being bound to a stage.
///
/// This is the stage-free half of [`CompiledMask::new`], for the recipe validation that runs before
/// any stage is known: it is what makes an unknown kind or a malformed payload fail compiling,
/// rendering, sampling and planning alike instead of only at the catalog boundary. Cost is
/// `O(components)` and it reads no pixels.
pub fn validate_component_kinds(mask: &Mask) -> Result<(), Error> {
    for component in &mask.components {
        parse_component(component)?;
    }
    Ok(())
}

/// The empty rectangle: the support of `m = 0`, which is what the composition starts from.
fn empty_region() -> Region {
    Region {
        x0: 0,
        y0: 0,
        width: 0,
        height: 0,
    }
}

fn whole_stage(stage: Stage) -> Region {
    Region {
        x0: 0,
        y0: 0,
        width: stage.width,
        height: stage.height,
    }
}

/// The composed conservative rectangle, folded in the same order coverage is.
fn compose_bounds(
    components: &[CompiledComponent],
    stage: Stage,
    invert: bool,
    scale: f64,
) -> Region {
    let mut bounds = empty_region();
    for component in components {
        let support = component.geometry.support(stage, component.invert);
        bounds = match component.mode {
            // max(m, c) can only grow the support.
            ComponentMode::Add => union(bounds, support),
            // min(m, 1 - c) can only shrink it, and where it shrinks depends on the component, so
            // the rectangle already in hand stays as it is.
            ComponentMode::Subtract => bounds,
            // min(m, c) is zero wherever either side is.
            ComponentMode::Intersect => intersection(bounds, support),
        };
    }
    if invert {
        // (1 - m) is non-zero wherever m < 1, which no rectangle usefully bounds.
        bounds = whole_stage(stage);
    }
    if scale == 0.0 {
        // The final multiply is exactly `0.0 * m` at every pixel, m being finite everywhere.
        bounds = empty_region();
    }
    bounds
}

fn union(a: Region, b: Region) -> Region {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let x0 = a.x0.min(b.x0);
    let y0 = a.y0.min(b.y0);
    let x1 = a.x1().max(b.x1());
    let y1 = a.y1().max(b.y1());
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

fn intersection(a: Region, b: Region) -> Region {
    let x0 = a.x0.max(b.x0);
    let y0 = a.y0.max(b.y0);
    let x1 = a.x1().min(b.x1());
    let y1 = a.y1().min(b.y1());
    if x1 <= x0 || y1 <= y0 {
        return empty_region();
    }
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

/// The conservative pixel rectangle of the half-plane `inside(x, y) > 0`, where `inside` is an
/// affine function of the real pixel indices and the pixels of interest are the integer points of
/// `[0, W-1] x [0, H-1]`.
///
/// Closed form, so a component's rectangle costs `O(1)` rather than a scan of the stage's side: an
/// affine function over a rectangle takes its extremes at the corners, so the clipped region is the
/// convex hull of the corners that satisfy it and the crossings on the edges between a corner that
/// does and one that does not.
///
/// Two deliberate slacks make the result conservative against its own arithmetic, which is what
/// "exactly zero outside" demands of a rectangle computed in floating point. The clip is taken at a
/// level slightly below zero, scaled to the magnitudes involved, so a pixel whose exact value is
/// just negative is kept rather than excluded; and the rectangle is then grown by one pixel on every
/// side. A non-finite value anywhere answers the whole stage, which is always a correct rectangle.
fn half_plane_bounds(stage: Stage, inside: impl Fn(f64, f64) -> f64) -> Region {
    let x_max = f64::from(stage.width - 1);
    let y_max = f64::from(stage.height - 1);
    let corners = [(0.0, 0.0), (x_max, 0.0), (x_max, y_max), (0.0, y_max)];
    let values = corners.map(|(x, y)| inside(x, y));
    if values.iter().any(|value| !value.is_finite()) {
        return whole_stage(stage);
    }
    // The level the clip is taken at: a few ulps of the largest magnitude the corners produced, so
    // it is a slack in the arithmetic rather than a fixed coverage threshold.
    let magnitude = values
        .iter()
        .fold(0.0f64, |worst, value| worst.max(value.abs()));
    let level = -16.0 * f64::EPSILON * magnitude;
    if values.iter().all(|value| *value <= level) {
        return empty_region();
    }
    if values.iter().all(|value| *value > level) {
        return whole_stage(stage);
    }
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut extend = |x: f64, y: f64| {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    };
    for index in 0..4 {
        let (x0, y0) = corners[index];
        let (x1, y1) = corners[(index + 1) % 4];
        let (v0, v1) = (values[index], values[(index + 1) % 4]);
        if v0 > level {
            extend(x0, y0);
        }
        if (v0 > level) != (v1 > level) {
            // The edge crosses the clip level exactly once, the function being affine along it.
            let s = (v0 - level) / (v0 - v1);
            extend(x0 + s * (x1 - x0), y0 + s * (y1 - y0));
        }
    }
    let grow_low = |value: f64, limit: u32| -> u32 {
        let index = value.floor() as i64 - 1;
        index.clamp(0, i64::from(limit)) as u32
    };
    let grow_high = |value: f64, limit: u32| -> u32 {
        let index = value.floor() as i64 + 2;
        index.clamp(0, i64::from(limit)) as u32
    };
    let x0 = grow_low(min_x, stage.width);
    let y0 = grow_low(min_y, stage.height);
    let x1 = grow_high(max_x, stage.width);
    let y1 = grow_high(max_y, stage.height);
    if x1 <= x0 || y1 <= y0 {
        return empty_region();
    }
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ComponentId;
    use serde_json::json;

    /// The pixel value a geometric component is handed and ignores (proposal P12 of
    /// `docs/design/range-study.md`): the masks here hold gradients and their coverage is a
    /// function of position alone, so the value is arbitrary and the same at every call.
    const ANY_PIXEL: [f64; 3] = [0.25, 0.5, 0.75];

    fn stage(width: u32, height: u32) -> Stage {
        Stage { width, height }
    }

    fn component(
        name: &str,
        mode: ComponentMode,
        kind: &str,
        payload: serde_json::Value,
    ) -> Component {
        Component::new(name, mode, kind, payload)
    }

    fn gradient(x0: f64, y0: f64, x1: f64, y1: f64) -> serde_json::Value {
        json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1})
    }

    fn mask_of(components: Vec<Component>) -> Mask {
        let mut mask = Mask::new("Mask 1");
        mask.components = components;
        mask
    }

    /// One vertical gradient over a square stage: coverage is exactly zero at and behind `p0`,
    /// exactly one at and beyond `p1`, and the frozen easing in between.
    #[test]
    fn a_linear_gradient_is_exact_at_both_ends_of_its_axis() {
        let mask = mask_of(vec![component(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            gradient(0.5, 0.25, 0.5, 0.75),
        )]);
        let compiled =
            CompiledMask::new(&mask, stage(400, 400), &crate::path::StrokeTable::default())
                .unwrap();
        // p0 sits at v = 0.25, which is pixel row 99.5; row 99 is behind it and row 300 beyond p1.
        assert_eq!(compiled.coverage(200, 0, ANY_PIXEL), 0.0);
        assert_eq!(compiled.coverage(200, 99, ANY_PIXEL), 0.0);
        assert_eq!(compiled.coverage(200, 300, ANY_PIXEL), 1.0);
        assert_eq!(compiled.coverage(200, 399, ANY_PIXEL), 1.0);
        // The midpoint of the axis is v = 0.5, pixel row 199.5: smooth(0.5) is exactly 0.5.
        assert_eq!(
            compiled.coverage(200, 199, ANY_PIXEL) + compiled.coverage(200, 200, ANY_PIXEL),
            1.0
        );
        // A gradient has no width: the field is constant across the axis.
        for x in [0u32, 137, 399] {
            assert_eq!(
                compiled.coverage(x, 150, ANY_PIXEL),
                compiled.coverage(200, 150, ANY_PIXEL)
            );
        }
    }

    /// The whole-mask `amount` is the study's final multiply, and `invert` is applied before it.
    #[test]
    fn amount_and_invert_apply_at_the_whole_mask_level() {
        let components = vec![component(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            gradient(0.5, 0.25, 0.5, 0.75),
        )];
        let mut mask = mask_of(components);
        mask.amount = 50.0;
        let compiled =
            CompiledMask::new(&mask, stage(400, 400), &crate::path::StrokeTable::default())
                .unwrap();
        assert_eq!(compiled.coverage(200, 300, ANY_PIXEL), 0.5);
        mask.invert = true;
        let inverted =
            CompiledMask::new(&mask, stage(400, 400), &crate::path::StrokeTable::default())
                .unwrap();
        assert_eq!(inverted.coverage(200, 300, ANY_PIXEL), 0.0);
        assert_eq!(inverted.coverage(200, 0, ANY_PIXEL), 0.5);
        // An amount of exactly zero is exactly zero coverage, inverted or not, and nothing to draw.
        mask.amount = 0.0;
        let silent =
            CompiledMask::new(&mask, stage(400, 400), &crate::path::StrokeTable::default())
                .unwrap();
        assert_eq!(silent.coverage(200, 0, ANY_PIXEL), 0.0);
        assert!(silent.bounds().is_empty());
    }

    /// An empty component list composes to `m = 0`: an empty mask selects nothing rather than
    /// everything, its rectangle is empty and it draws no feature for a proxy pixel to miss.
    #[test]
    fn an_empty_mask_selects_nothing() {
        let mask = mask_of(Vec::new());
        let compiled =
            CompiledMask::new(&mask, stage(64, 48), &crate::path::StrokeTable::default()).unwrap();
        for y in 0..48 {
            for x in 0..64 {
                assert_eq!(compiled.coverage(x, y, ANY_PIXEL), 0.0);
            }
        }
        assert!(compiled.bounds().is_empty());
        assert_eq!(compiled.min_feature_px(stage(64, 48)), f32::INFINITY);
        assert_eq!(compiled.components(), 0);
    }

    /// `subtract` and `intersect` are the frozen `min` forms, and a subtract only ever removes
    /// coverage the adds already placed.
    #[test]
    fn composition_is_the_frozen_zadeh_algebra() {
        let mask = mask_of(vec![
            component(
                "Linear 1",
                ComponentMode::Add,
                "linear",
                gradient(0.5, 0.25, 0.5, 0.75),
            ),
            component(
                "Linear 2",
                ComponentMode::Subtract,
                "linear",
                gradient(0.25, 0.5, 0.75, 0.5),
            ),
        ]);
        let compiled =
            CompiledMask::new(&mask, stage(400, 400), &crate::path::StrokeTable::default())
                .unwrap();
        for (x, y) in [(20u32, 380u32), (200, 300), (380, 380), (100, 100)] {
            let first = compiled.coverage(x, y, ANY_PIXEL);
            // Duplicating the whole list changes nothing, bit for bit: the algebra is idempotent
            // and add is order independent.
            let mut doubled = mask.clone();
            doubled.components = mask
                .components
                .iter()
                .flat_map(|component| {
                    let mut copy = component.clone();
                    copy.id = ComponentId::new();
                    copy.name = format!("{} copy", component.name);
                    [component.clone(), copy]
                })
                .collect();
            let twice = CompiledMask::new(
                &doubled,
                stage(400, 400),
                &crate::path::StrokeTable::default(),
            )
            .unwrap();
            assert_eq!(twice.coverage(x, y, ANY_PIXEL), first, "at {x},{y}");
        }
    }

    /// The kind table refuses a kind it does not claim by name, with the `incompatible` kind, and
    /// reads nothing of the payload it could not understand.
    #[test]
    fn an_unknown_component_kind_is_refused_by_name() {
        assert!(knows_component_kind("linear"));
        assert!(knows_component_kind("radial"));
        assert!(knows_component_kind("brush"));
        assert!(knows_component_kind("luminance-range"));
        assert!(knows_component_kind("colour-range"));
        // Every kind the masking design names is delivered, so the kind this build does not claim
        // is one no design names — which is exactly the case retention exists for.
        assert!(!knows_component_kind("future-kind"));
        let mask = mask_of(vec![component(
            "Future 1",
            ComponentMode::Add,
            "future-kind",
            json!({"nested": {"points": [[0.25, 0.5]]}, "flag": true}),
        )]);
        let error = CompiledMask::new(&mask, stage(64, 48), &crate::path::StrokeTable::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(
            error.to_string(),
            "incompatible: unknown mask component future-kind"
        );
        let table = validate_component_kinds(&mask).unwrap_err();
        assert_eq!(
            table.to_string(),
            "incompatible: unknown mask component future-kind"
        );
        // The stored component is untouched by the refusal: the table parses, it does not rewrite.
        assert_eq!(mask.components[0].kind, "future-kind");
        assert_eq!(mask.components[0].payload["flag"], json!(true));
    }

    /// Every message a malformed or illegal linear payload produces, in full.
    #[test]
    fn linear_validation_errors_name_the_field() {
        let cases = [
            (
                gradient(0.0, 0.0, 0.0, 0.0),
                ErrorKind::Validation,
                "validation: component Linear 1 linear axis length must be within 1e-4..=64 \
                 mask-space units on a 400x400 stage",
            ),
            (
                gradient(-1.5, 0.0, 0.5, 0.5),
                ErrorKind::Validation,
                "validation: component Linear 1 linear x0 must be a number within -1..=2",
            ),
            (
                gradient(0.0, 2.5, 0.5, 0.5),
                ErrorKind::Validation,
                "validation: component Linear 1 linear y0 must be a number within -1..=2",
            ),
            (
                json!({"x0": 0.0, "y0": 0.0, "x1": 1.0}),
                ErrorKind::Validation,
                "validation: component Linear 1 has an invalid linear payload: missing field `y1`",
            ),
            (
                json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0, "angle": 4.0}),
                ErrorKind::Validation,
                "validation: component Linear 1 has an invalid linear payload: unknown field \
                 `angle`, expected one of `x0`, `y0`, `x1`, `y1`",
            ),
        ];
        for (payload, kind, message) in cases {
            let mask = mask_of(vec![component(
                "Linear 1",
                ComponentMode::Add,
                "linear",
                payload,
            )]);
            let error =
                CompiledMask::new(&mask, stage(400, 400), &crate::path::StrokeTable::default())
                    .unwrap_err();
            assert_eq!(error.kind, kind);
            assert_eq!(error.to_string(), message);
        }
    }

    /// An axis so long that its mask-space length passes the ceiling is refused by the same rule
    /// that refuses a zero-length one, because the axis length is itself a stored distance.
    #[test]
    fn the_axis_length_is_bounded_at_both_ends() {
        let mask = mask_of(vec![component(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            gradient(-1.0, 0.0, 2.0, 0.0),
        )]);
        // Three stage widths of horizontal axis on a 16384x100 stage is 491.52 mask-space units.
        let error = CompiledMask::new(
            &mask,
            stage(16384, 100),
            &crate::path::StrokeTable::default(),
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .contains("axis length must be within 1e-4..=64"),
            "{error}"
        );
        // The same payload on a square stage is three units, which is legal.
        assert!(
            CompiledMask::new(&mask, stage(400, 400), &crate::path::StrokeTable::default()).is_ok()
        );
    }

    /// A stage with no pixels is refused rather than divided by.
    #[test]
    fn an_empty_stage_is_refused() {
        let mask = mask_of(Vec::new());
        let error = CompiledMask::new(&mask, stage(0, 48), &crate::path::StrokeTable::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.to_string(),
            "validation: mask Mask 1 cannot be compiled against an empty 0x48 stage"
        );
    }

    /// The minimum feature is the ramp width in pixels, and it is the number of rows the transition
    /// actually occupies — counted, not argued. One mask-space unit is the stage's height on both
    /// axes, so a diagonal axis measures its own length and not its projection.
    #[test]
    fn min_feature_px_is_the_measured_ramp_width() {
        let stage = stage(600, 400);
        let mask = mask_of(vec![component(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            gradient(0.5, 0.25, 0.5, 0.75),
        )]);
        let compiled =
            CompiledMask::new(&mask, stage, &crate::path::StrokeTable::default()).unwrap();
        // The axis is half the stage's height in mask-space units: 0.5 * 400 = 200 px.
        assert_eq!(compiled.min_feature_px(stage), 200.0);
        let partial = (0..stage.height)
            .filter(|y| {
                let c = compiled.coverage(300, *y, ANY_PIXEL);
                c > 0.0 && c < 1.0
            })
            .count();
        assert!(
            (199..=200).contains(&partial),
            "the ramp occupied {partial} rows against a stated 200"
        );
        // A diagonal axis across the same stage: du = 0.5 * 1.5, dv = 0.5, so the length is
        // sqrt(0.5625 + 0.25) = 0.9013878... units, or 360.555 px — the axis's own length, not its
        // projection, because one mask-space unit is the stage's height on both axes.
        let diagonal = mask_of(vec![component(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            gradient(0.25, 0.25, 0.75, 0.75),
        )]);
        let compiled =
            CompiledMask::new(&diagonal, stage, &crate::path::StrokeTable::default()).unwrap();
        let expected = ((0.5 * 1.5f64).powi(2) + 0.5f64.powi(2)).sqrt() * 400.0;
        assert!(
            (f64::from(compiled.min_feature_px(stage)) - expected).abs() < 1e-3,
            "{} against {expected}",
            compiled.min_feature_px(stage)
        );
        // The smallest feature of several components is the narrowest of them.
        let mixed = mask_of(vec![
            component(
                "Linear 1",
                ComponentMode::Add,
                "linear",
                gradient(0.5, 0.25, 0.5, 0.75),
            ),
            component(
                "Linear 2",
                ComponentMode::Add,
                "linear",
                gradient(0.5, 0.5, 0.5, 0.55),
            ),
        ]);
        let compiled =
            CompiledMask::new(&mixed, stage, &crate::path::StrokeTable::default()).unwrap();
        assert!((f64::from(compiled.min_feature_px(stage)) - 20.0).abs() < 1e-9);
    }

    /// `bounds` is a rectangle of the compiled stage and it excludes what a half-plane excludes: a
    /// gradient with both endpoints in the lower half of the frame leaves the rows above `p0` out.
    #[test]
    fn bounds_excludes_the_half_plane_behind_p0() {
        let stage = stage(200, 200);
        let mask = mask_of(vec![component(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            gradient(0.5, 0.6, 0.5, 0.9),
        )]);
        let compiled =
            CompiledMask::new(&mask, stage, &crate::path::StrokeTable::default()).unwrap();
        let bounds = compiled.bounds();
        assert_eq!(bounds.x0, 0);
        assert_eq!(bounds.width, 200);
        // p0 is at v = 0.6, row 119.5, and the rectangle starts one pixel before row 119.
        assert_eq!(bounds.y0, 118);
        assert_eq!(bounds.y1(), 200);
        // An intersect narrows it; a subtract leaves it as it was.
        let mut narrowed = mask.clone();
        narrowed.components.push(component(
            "Linear 2",
            ComponentMode::Intersect,
            "linear",
            gradient(0.6, 0.5, 0.9, 0.5),
        ));
        let compiled =
            CompiledMask::new(&narrowed, stage, &crate::path::StrokeTable::default()).unwrap();
        assert_eq!(compiled.bounds().y0, 118);
        assert_eq!(compiled.bounds().x0, 118);
        let mut unchanged = mask.clone();
        unchanged.components.push(component(
            "Linear 3",
            ComponentMode::Subtract,
            "linear",
            gradient(0.6, 0.5, 0.9, 0.5),
        ));
        let compiled =
            CompiledMask::new(&unchanged, stage, &crate::path::StrokeTable::default()).unwrap();
        assert_eq!(compiled.bounds(), bounds);
        // A whole-mask inversion is non-zero almost everywhere, so no rectangle bounds it.
        let mut inverted = mask.clone();
        inverted.invert = true;
        let compiled =
            CompiledMask::new(&inverted, stage, &crate::path::StrokeTable::default()).unwrap();
        assert_eq!(compiled.bounds(), whole_stage(stage));
    }

    /// An inverted component's support is the other half-plane: coverage is `1` where the component
    /// itself was `0`, so the rectangle must cover the rows in front of `p1` instead.
    #[test]
    fn an_inverted_component_bounds_the_other_half_plane() {
        let stage = stage(200, 200);
        let mut mask = mask_of(vec![component(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            gradient(0.5, 0.1, 0.5, 0.4),
        )]);
        mask.components[0].invert = true;
        let compiled =
            CompiledMask::new(&mask, stage, &crate::path::StrokeTable::default()).unwrap();
        // p1 is at v = 0.4, which is row 79.5: above it the inverted component is non-zero, and from
        // row 80 down the original was exactly 1 so the inversion is exactly 0. The rectangle ends
        // one pixel past the crossing row.
        assert_eq!(compiled.bounds().y0, 0);
        assert_eq!(compiled.bounds().y1(), 81);
        assert_eq!(compiled.coverage(100, 80, ANY_PIXEL), 0.0);
        assert_eq!(compiled.coverage(100, 199, ANY_PIXEL), 0.0);
        assert_eq!(compiled.coverage(100, 0, ANY_PIXEL), 1.0);
    }

    /// The cost of compiling a mask against component count and against stage size. Ignored by
    /// default because it is a measurement, not a pass/fail property: run it with
    /// `cargo test --release --package luxforge-core --lib
    /// mask::tests::compile_cost_against_component_count_and_stage_size -- --ignored --nocapture`.
    #[test]
    #[ignore = "a recorded measurement, not an assertion"]
    fn compile_cost_against_component_count_and_stage_size() {
        let payload = gradient(0.2, 0.2, 0.8, 0.8);
        for count in [1usize, 4, 16, 32] {
            let mask = mask_of(
                (0..count)
                    .map(|index| {
                        component(
                            &format!("Linear {}", index + 1),
                            ComponentMode::Add,
                            "linear",
                            payload.clone(),
                        )
                    })
                    .collect(),
            );
            for (width, height) in [(480u32, 320u32), (6000, 4000), (9504, 6336)] {
                let stage = stage(width, height);
                let rounds = 1000;
                let started = std::time::Instant::now();
                for _ in 0..rounds {
                    std::hint::black_box(
                        CompiledMask::new(&mask, stage, &crate::path::StrokeTable::default())
                            .unwrap(),
                    );
                }
                let elapsed = started.elapsed();
                println!(
                    "{count:2} components, {width}x{height}: {:.3} us/compile",
                    elapsed.as_secs_f64() * 1e6 / f64::from(rounds)
                );
            }
        }
        // The per-pixel field for context, single threaded, at one component and at the 32-component
        // limit. What a masked run costs is the masked-primitive task's measurement; this is the cost
        // of the coverage field alone, over every pixel of a 24 MP frame.
        let stage = stage(6000, 4000);
        for count in [1usize, 32] {
            let mask = mask_of(
                (0..count)
                    .map(|index| {
                        component(
                            &format!("Linear {}", index + 1),
                            ComponentMode::Add,
                            "linear",
                            payload.clone(),
                        )
                    })
                    .collect(),
            );
            let compiled =
                CompiledMask::new(&mask, stage, &crate::path::StrokeTable::default()).unwrap();
            let started = std::time::Instant::now();
            let mut total = 0.0;
            for y in 0..stage.height {
                for x in 0..stage.width {
                    total += compiled.coverage(x, y, ANY_PIXEL);
                }
            }
            std::hint::black_box(total);
            let elapsed = started.elapsed();
            println!(
                "{count:2} components over 6000x4000: {:.1} ms ({:.2} ns/pixel)",
                elapsed.as_secs_f64() * 1000.0,
                elapsed.as_secs_f64() * 1e9 / (f64::from(stage.width) * f64::from(stage.height))
            );
        }
    }
}
