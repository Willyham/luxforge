//! The `mask.*` host command family: what each command declares, what it does to a recipe's mask
//! table, and the history label it commits.
//!
//! Masks are host commands in their own namespace, as `history.*` and `version.*` are, and not a
//! tool module (`docs/design/masking.md#host-commands`). The reason is structural: a module commits
//! *layers* through [`crate::ActionPlan`] and must never rewrite the recipe, while every one of these
//! commands rewrites the mask table beside the layers. What they share with a module is everything
//! else, and deliberately the *same* code rather than a parallel copy of it:
//!
//! - Each command declares an [`ActionDescriptor`] with [`ParameterDescriptor`]s of the same closed
//!   [`crate::ParameterKind`] set a module may declare, so [`check_parameters`] is the one and only
//!   path a value takes to be validated, whatever client sent it, and a refusal reads the same.
//! - [`controls`] declares the panel widgets over those same parameters with the same [`Control`]
//!   type, so there is one control-generation path and a client renders a mask's number fields,
//!   toggles and mode selector with the widgets it already has.
//! - `schema.list` lists every command from this table through [`MaskCommand::schema`], so discovery
//!   and dispatch cannot drift apart.
//! - A command commits through the delivered [`crate::EditorService::commit_snapshot`] path, so the
//!   mutation envelope, the input-hash deduplication, the revision check, the single history entry
//!   and the single immutable snapshot are the delivered ones and not a second implementation.
//! - A gesture drafts through the delivered `draft.begin` / `draft.set` / `draft.commit` lifecycle
//!   with its existing conflict, Discard and Reapply behaviour; see [`MaskTarget`] for the one thing
//!   a draft needed that a declared parameter cannot carry.
//!
//! What no declared parameter kind can express is an identity or a person's free text: the kinds are
//! numbers, integers, enums, colours, booleans and curves. So the mask a command addresses, the
//! component inside it and the name a rename sets travel in the request's envelope beside
//! `asset_id` and `mutation`, as [`MaskTarget`], and only geometry, amounts, modes and flags are
//! declared parameters. That is why the masking design needs no string parameter kind.
//!
//! **The geometry methods are generated per component kind**, as `edit.<action>` and `query.<id>`
//! already are: `mask.create-linear`, `mask.add-radial`, `mask.set-linear` and so on, each declaring
//! exactly the parameters its own kind's module declares. One `mask.create` carrying a `kind` and a
//! union of every kind's fields cannot be declared honestly — the closed parameter vocabulary has no
//! way to say "these parameters when the kind is linear, those when it is radial", so `schema.list`
//! would advertise a radius on a linear gradient and a client would have to read prose to know
//! better. Generating from [`super::COMPONENT_KINDS`] also means a kind becomes creatable, addable
//! and patchable by being *registered*, rather than by someone remembering a second table: this
//! module declares no geometry of its own and knows no kind by name.
use super::SAMPLES_FIELD;
use super::{
    BRUSH, DISTANCE_MIN, REFINE_DEFAULT, REFINE_MAX, REFINE_MIN, component_geometry_is_drawn,
    component_parameters, component_sample_limit, component_sample_parameters,
    declared_geometry_kinds, knows_component_kind, sampling_kinds,
};
use crate::{
    ActionDescriptor, CanvasInteraction, ChoiceStyle, Component, ComponentId, ComponentMode,
    Control, Error, ErrorKind, Layer, LayerId, Mask, MaskId, ModuleRegistry, MutationResult,
    NumberStyle, ParameterDescriptor, ParameterKind, Recipe,
    model::{COMPONENTS_PER_MASK, MASKS_PER_RECIPE},
    path::{self, POINTS_PER_STROKE, SIZE_MAX, Stroke, StrokeId},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::LazyLock;

/// The read-only methods of the family.
pub const LIST: &str = "mask.list";

/// `mask.sample-input`: the **pixel the operation a mask modulates receives**, at one content
/// position, in linear sRGB.
///
/// It is the host side of the canvas pick for every value-based part of a mask — a colour range's
/// swatches today — and it exists because the client must not read that colour itself. A range
/// selection is evaluated on the operation's *input*, while the frame a client can see holds that
/// operation's *output*, so a colour decoded from the picture would be a different colour and the
/// selection would not be the one the person picked (proposal P17 of `docs/design/range-study.md`).
/// The host computes it from the same point sample a module's query uses, so it costs one
/// `O(layers)` evaluation and rasterizes nothing.
///
/// Read-only in every sense: it writes no entry, emits no event and touches no session state.
pub const SAMPLE_INPUT: &str = "mask.sample-input";

/// The brush's two commands: the **only** way a stroke reaches a mask.
///
/// A brush component's geometry is drawn rather than typed, so it declares no parameters and
/// generates no `mask.create-brush`, `mask.add-brush` or `mask.set-brush`. These two are declared
/// here instead, by hand and not from the kind table, because what they carry is a *path* and a
/// stroke's own settings rather than a kind's geometry fields.
pub const ADD_STROKE: &str = "mask.add-stroke";
pub const DELETE_STROKE: &str = "mask.delete-stroke";

/// What a generated geometry method does to a component list. The kind it does it to is the other
/// half of [`GeometryMethod`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryOp {
    /// `mask.create-<kind>`: a new mask whose first component is an `add` component of that kind.
    Create,
    /// `mask.add-<kind>`: a second, third, … component, with its mode given explicitly.
    Add,
    /// `mask.set-<kind>`: a field patch over one component's geometry.
    Set,
}

impl GeometryOp {
    /// The method-name stem this operation takes, before the kind it is generated for.
    fn stem(self) -> &'static str {
        match self {
            Self::Create => "mask.create",
            Self::Add => "mask.add",
            Self::Set => "mask.set",
        }
    }

    fn all() -> [Self; 3] {
        [Self::Create, Self::Add, Self::Set]
    }
}

/// What a generated sample method does to one component's list of sampled colours. The kind it does
/// it to is the other half of [`SampleMethod`].
///
/// A sampled colour cannot be a declared parameter of the geometry methods: the closed parameter
/// vocabulary has numbers, integers, enums, colours, booleans and curves and no *list* of any of
/// them. So a kind that holds swatches says how many and what one declares in the host's kind table,
/// and these two methods edit that list one swatch at a time — which is also how a person edits it,
/// a pick at a time and a removal at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleOp {
    /// `mask.add-<kind>-sample`: one more sampled colour, up to the kind's declared limit.
    Add,
    /// `mask.delete-<kind>-sample`: the sample at one index, removed on its own.
    Delete,
}

impl SampleOp {
    fn method(self, kind: &str) -> String {
        match self {
            Self::Add => format!("mask.add-{kind}-sample"),
            Self::Delete => format!("mask.delete-{kind}-sample"),
        }
    }

    fn all() -> [Self; 2] {
        [Self::Add, Self::Delete]
    }
}

/// The identity of one generated sample method: what it does, and the one component kind it does it
/// to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleMethod {
    pub op: SampleOp,
    pub kind: &'static str,
}

/// The identity of one generated geometry method: what it does, and the one component kind it does
/// it to. A command carrying this declares exactly that kind's parameters and refuses a component of
/// any other kind by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryMethod {
    pub op: GeometryOp,
    pub kind: &'static str,
}

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

/// A component kind this build cannot evaluate, in the spelling
/// [`super::validate_component_kinds`] already uses, because it is the same fact: a payload the host
/// retains and cannot read. `Incompatible` and not `Validation` — the stored stack is well formed and
/// this build simply cannot draw part of it.
fn unknown_kind(kind: &str) -> Error {
    Error::new(
        ErrorKind::Incompatible,
        format!("unknown mask component {kind}"),
    )
}

/// The host-owned envelope of one mask command: the objects it addresses and the display name a
/// rename sets.
///
/// These are not declared parameters and cannot be. The closed parameter-kind set carries numbers,
/// integers, enums, colours, booleans and curves — no identity and no free text — so a mask
/// identity travels beside `asset_id` exactly as `asset_id` itself does, and a client that can read
/// `mask.list` can address anything in it. It is also what a drafted gesture carries in
/// [`crate::Draft::target`]: the handle drag edits *that* component, and which component it is
/// cannot live in the drafted fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The stroke `mask.delete-stroke` removes, by its content address. It is an identity like the
    /// two above and travels for the same reason: no declared parameter kind carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<StrokeId>,
}

/// Whether one envelope field belongs to a command, and whether it must be there.
///
/// Every command but one wants a fixed envelope, so [`Need::Required`] and [`Need::None`] are what
/// almost all of them declare. `mask.add-stroke` is the exception, and deliberately: no mask draws a
/// new one, a mask without a component puts a new brush on it, and both together append a stroke to
/// that brush. Those are three history entries with three labels and **one** command, because a
/// person painting does not choose between three gestures — they put the brush down, and where it
/// lands decides which of the three that stroke was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// The command refuses this field by name.
    None,
    /// The command reads it when it is there and does something else when it is not.
    Optional,
    /// The command refuses its absence by name.
    Required,
}

impl Need {
    /// This command reads the field at all, which is what a client filling an envelope asks.
    pub fn wanted(self) -> bool {
        self != Self::None
    }

    fn required(self) -> bool {
        self == Self::Required
    }

    /// The declaration a `(bool, bool, bool)` in the command table spells: required, or refused.
    fn of(required: bool) -> Self {
        if required { Self::Required } else { Self::None }
    }
}

/// One declared `mask.*` method.
#[derive(Debug)]
pub struct MaskCommand {
    /// The method name, which is also the durable action identity a history entry stores. It carries
    /// a dot, which [`crate::valid_name`] forbids inside an action identity, so a module action and a
    /// mask command cannot collide however either grows; [`crate::ModuleRegistry::register`] checks
    /// that rather than assuming it.
    pub method: &'static str,
    pub mutates: bool,
    pub needs_mask: Need,
    pub needs_component: Need,
    pub needs_name: Need,
    pub needs_stroke: Need,
    /// Set on the generated geometry methods and on no other command: it is what says which kind's
    /// parameters this method declares and which kind's components it may touch.
    pub geometry: Option<GeometryMethod>,
    /// Set on the generated sample methods and on no other command.
    pub samples: Option<SampleMethod>,
    pub action: ActionDescriptor,
}

impl MaskCommand {
    /// The envelope fields this command requires and refuses, checked before anything is planned so
    /// a malformed request never reaches a recipe. The wording matches the delivered envelope
    /// refusal, because it is the same kind of missing field.
    pub fn checked_target(&self, target: &MaskTarget) -> Result<(), Error> {
        for (present, needed, field) in [
            (target.mask.is_some(), self.needs_mask, "mask"),
            (
                target.component.is_some(),
                self.needs_component,
                "component",
            ),
            (target.name.is_some(), self.needs_name, "name"),
            (target.stroke.is_some(), self.needs_stroke, "stroke"),
        ] {
            if needed.required() && !present {
                return Err(validation(format!(
                    "missing required field {field} for {}",
                    self.method
                )));
            }
            if present && !needed.wanted() {
                return Err(validation(format!(
                    "unknown field {field} for {}",
                    self.method
                )));
            }
        }
        Ok(())
    }

    /// This command's entry in `schema.list`, in the shape a generated action method uses: the
    /// envelope first, then the command's own declared parameters, then the descriptors themselves so
    /// a client generates its controls from the same declaration the host validates against.
    pub fn schema(&self) -> Value {
        let mut required = vec![json!("asset_id")];
        let mut optional = Map::new();
        if self.mutates {
            required.push(json!("mutation"));
        } else {
            optional.insert(
                "entry_id".to_owned(),
                json!("entry to ask about; default the session's selection"),
            );
        }
        for (needed, field, meaning) in [
            (self.needs_mask, "mask", "the mask this command addresses"),
            (
                self.needs_component,
                "component",
                "the component inside that mask",
            ),
            (self.needs_name, "name", "the display name to set"),
            (
                self.needs_stroke,
                "stroke",
                "the stroke's content address, as its component's strokes field lists it",
            ),
        ] {
            match needed {
                Need::Required => {
                    required.push(json!(field));
                    optional.remove(field);
                }
                // An envelope field a command *may* carry is listed where a client looks for what it
                // may send, beside the optional parameters, with what leaving it out means.
                Need::Optional => {
                    optional.insert(field.to_owned(), json!(meaning));
                }
                Need::None => {}
            }
        }
        for parameter in &self.action.parameters {
            if !self.action.patch && parameter.required && parameter.default.is_none() {
                required.push(json!(parameter.name));
            } else {
                optional.insert(parameter.name.clone(), json!(parameter.notes));
            }
        }
        json!({
            "mutates": self.mutates,
            "patch": self.action.patch,
            "required": required,
            "optional": optional,
            "notes": self.action.notes,
            "parameters": self.action.parameters,
        })
    }
}

/// Every declared command, in the order the design's method table lists them.
pub fn all() -> &'static [MaskCommand] {
    &COMMANDS
}

/// The command one method name declares, or none. The same lookup discovery, dispatch, drafting and
/// the registry's collision check all use, so the four cannot drift.
pub fn find(method: &str) -> Option<&'static MaskCommand> {
    COMMANDS.iter().find(|command| command.method == method)
}

/// The generated sample command one operation on one sampling kind declares, or none when this
/// build does not sample that kind.
pub fn sample(op: SampleOp, kind: &str) -> Option<&'static MaskCommand> {
    COMMANDS.iter().find(|command| {
        command
            .samples
            .is_some_and(|samples| samples.op == op && samples.kind == kind)
    })
}

/// The generated command one operation on one component kind declares, or none when this build does
/// not know the kind.
///
/// A client that has drawn a gradient knows what it did — create, add or patch — and which kind it
/// drew, and needs the method name for that pair. Spelling it out client-side would be a second copy
/// of the generation rule in [`geometry_commands`]; this is the same table, read by the same key.
pub fn geometry(op: GeometryOp, kind: &str) -> Option<&'static MaskCommand> {
    COMMANDS.iter().find(|command| {
        command
            .geometry
            .is_some_and(|geometry| geometry.op == op && geometry.kind == kind)
    })
}

/// The canvas picks the host declares for its own commands: one per sampling kind, generated from
/// the kind table exactly as that kind's two sample methods are.
///
/// This is the host's side of the delivered pick machinery, in the delivered type
/// ([`CanvasInteraction`]) with the delivered meaning, because a mask is a host object and no module
/// declares one — so a pick that fills part of a mask had no way to be expressed until the
/// interaction admitted a host target (proposal P17 of `docs/design/range-study.md`). Each entry runs
/// [`SAMPLE_INPUT`] at the picked content pixel and submits the `r`, `g` and `b` it answers with to
/// `mask.add-<kind>-sample`, whose own declared parameters carry exactly those names — so the
/// delivered "every top-level number field of the result whose name is a parameter of the action"
/// rule needs no exception and the client maps nothing.
///
/// **The colour is never the client's.** It is read by the host from the stage the masked layer
/// receives and travels straight back into a host command; nothing on the way can decode a pixel,
/// because nothing on the way has one. That is the whole of what this declaration buys.
///
/// The mode a pick belongs to is its **action's** method name, which is unique by construction and is
/// what a client sets to enter the mode — there is no second table of mode names. No shortcut is
/// declared: a mask's pick is offered beside the component it fills, the way a module's picker control
/// is, and the canvas mode strip lists no pick mode at all.
pub fn canvas() -> &'static [CanvasInteraction] {
    &CANVAS
}

static CANVAS: LazyLock<Vec<CanvasInteraction>> = LazyLock::new(|| {
    sampling_kinds()
        .filter_map(|kind| {
            let add = sample(SampleOp::Add, kind)?;
            Some(CanvasInteraction::SampleApply {
                query: SAMPLE_INPUT.to_owned(),
                x: "x".to_owned(),
                y: "y".to_owned(),
                action: add.method.to_owned(),
                title: format!("Pick {}", spoken(kind)),
                shortcut: None,
            })
        })
        .collect()
});

/// The canvas pick whose mode a client is in, or none when that mode is not one of the host's.
pub fn canvas_pick(mode: &str) -> Option<&'static CanvasInteraction> {
    CANVAS.iter().find(|pick| match pick {
        CanvasInteraction::SampleApply { action, .. } => action == mode,
        CanvasInteraction::PointPick { .. } | CanvasInteraction::CropFrame { .. } => false,
    })
}

/// The panel widgets of the mask commands, over the parameters those commands declare.
///
/// The same [`Control`] vocabulary a module declares, so a client generates a gradient's endpoint
/// fields, the whole-mask amount, the three-way mode selector and the two inversion toggles with the
/// widgets it already has, and no client invents an operation of its own. Layout — which row a
/// control sits in — belongs to the Masks panel and not here.
pub fn controls() -> &'static [Control] {
    &CONTROLS
}

/// What one command does to a stack.
#[derive(Debug)]
pub(crate) enum MaskOutcome {
    /// The command changed nothing: a drag returned to its start, a value was set to what it already
    /// was. No entry is written, exactly as a module's [`crate::ActionPlan::NoOp`].
    NoOp,
    Change(MaskChange),
}

/// The resulting stack, the label the entry stores, and what the command says it touched.
#[derive(Debug)]
pub(crate) struct MaskChange {
    pub recipe: Recipe,
    pub label: String,
    pub mask: Option<MaskId>,
    pub component: Option<ComponentId>,
    pub removed_layers: Vec<RemovedLayer>,
}

/// One layer a destructive command removed, named by the module that provided its effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovedLayer {
    pub id: LayerId,
    pub effect: String,
    /// The provider's title, such as `Basic`, or none when no provider declares the effect. It is
    /// what the history label names, so a destructive delete reads as a person would say it.
    pub title: Option<String>,
}

/// What one mask command answers with: the delivered mutation result, flattened so `outcome`,
/// `revision` and `deduplicated` read exactly where every other mutation puts them, plus what this
/// command changed.
///
/// A no-op reports the envelope alone. Nothing durable was written, so there is nothing to read back
/// on a retry, and a retried request must answer identically to the call it retries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskCommandResult {
    #[serde(flatten)]
    pub mutation: MutationResult,
    /// The history label this command committed, which is the durable record of what it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentId>,
    /// The layers a `mask.delete` removed. A destructive command says what it removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_layers: Vec<RemovedLayer>,
}

impl MaskCommandResult {
    /// A result carrying the envelope and nothing else: a no-op, and a retry of one.
    pub(crate) fn plain(mutation: MutationResult) -> Self {
        Self {
            mutation,
            label: None,
            mask: None,
            component: None,
            removed_layers: Vec::new(),
        }
    }
}

/// The report of a command read back from the entry it wrote, so a deduplicated retry answers
/// identically to the call it retries rather than approximately.
///
/// Everything is recovered from durable data. The label is what the entry stores; a mask or component
/// the request named is in the entry's parameters; and one the *command* minted — a created mask, a
/// duplicate, an appended component — is recovered by comparing the entry's stack with its parent's,
/// which is the same comparison that recovers the layers a delete removed. A minted identity cannot
/// be in the request's parameters: they are the deduplication identity, and a fresh identity there
/// would make every attempt hash differently and never deduplicate at all.
pub(crate) fn report_of(
    mutation: MutationResult,
    entry: &crate::HistoryEntry,
    parent: Option<&crate::HistoryEntry>,
    registry: &ModuleRegistry,
) -> MaskCommandResult {
    let named = |field: &str| {
        entry
            .parameters
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let before = parent.map(|parent| &parent.snapshot.recipe);
    let after = &entry.snapshot.recipe;
    // Which identities an entry minted is a property of the command that wrote it, read from the
    // same table dispatch reads, so a generated method needs no row of its own here.
    let minted =
        find(&entry.action_id).and_then(|command| command.geometry.map(|geometry| geometry.op));
    let (mask, component) = match (minted, entry.action_id.as_str()) {
        // A new mask and the one component it was created with.
        (Some(GeometryOp::Create), _) => {
            let added = added_mask(after, before);
            (
                added.map(|mask| mask.id.clone()),
                added
                    .and_then(|mask| mask.components.first())
                    .map(|component| component.id.clone()),
            )
        }
        // The mask is the one the request named; the component is the one it appended.
        (Some(GeometryOp::Add), _) => {
            let mask = named("mask").and_then(|id| MaskId::parse(id).ok());
            let component = mask
                .as_ref()
                .and_then(|id| added_component(after, before, id))
                .map(|component| component.id.clone());
            (mask, component)
        }
        // The copy, not the source the request named.
        (_, "mask.duplicate") => (added_mask(after, before).map(|mask| mask.id.clone()), None),
        // One stroke is one of three edits, and which one it was is written in the envelope the
        // entry stored: no mask means it drew one, a mask alone means it put a brush on that mask,
        // and both mean it appended to that brush. The minted identities are recovered the same way
        // the generated methods' are, by comparing the entry's stack with its parent's.
        (_, ADD_STROKE) => match (
            named("mask").and_then(|id| MaskId::parse(id).ok()),
            named("component").and_then(|id| ComponentId::parse(id).ok()),
        ) {
            (None, _) => {
                let added = added_mask(after, before);
                (
                    added.map(|mask| mask.id.clone()),
                    added
                        .and_then(|mask| mask.components.first())
                        .map(|component| component.id.clone()),
                )
            }
            (Some(mask), None) => {
                let component =
                    added_component(after, before, &mask).map(|component| component.id.clone());
                (Some(mask), component)
            }
            (mask, component) => (mask, component),
        },
        _ => (
            named("mask").and_then(|id| MaskId::parse(id).ok()),
            named("component").and_then(|id| ComponentId::parse(id).ok()),
        ),
    };
    let removed = parent.map_or_else(Vec::new, |parent| {
        parent
            .snapshot
            .recipe
            .layers
            .iter()
            .filter(|layer| {
                !entry
                    .snapshot
                    .recipe
                    .layers
                    .iter()
                    .any(|kept| kept.id == layer.id)
            })
            .map(|layer| removed_layer(layer, registry))
            .collect()
    });
    MaskCommandResult {
        mutation,
        label: Some(entry.label.clone()),
        mask,
        component,
        removed_layers: removed,
    }
}

/// The mask an entry's stack carries and its parent's did not.
fn added_mask<'a>(after: &'a Recipe, before: Option<&Recipe>) -> Option<&'a Mask> {
    after.masks.iter().find(|mask| match before {
        Some(before) => !before.masks.iter().any(|had| had.id == mask.id),
        None => true,
    })
}

/// The component one mask gained between an entry's parent and the entry.
fn added_component<'a>(
    after: &'a Recipe,
    before: Option<&Recipe>,
    mask: &MaskId,
) -> Option<&'a Component> {
    let had = before
        .and_then(|before| before.masks.iter().find(|candidate| &candidate.id == mask))
        .map(|mask| &mask.components);
    after
        .masks
        .iter()
        .find(|candidate| &candidate.id == mask)?
        .components
        .iter()
        .find(|component| match had {
            Some(had) => !had.iter().any(|kept| kept.id == component.id),
            None => true,
        })
}

/// `mask.list`: every mask of one stack with its components, its values and the layers bound to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskListing {
    pub entry_id: crate::EntryId,
    pub masks: Vec<MaskReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskReport {
    pub id: MaskId,
    /// Position in the mask list, which is the order masked layers of one effect are evaluated in.
    pub index: usize,
    pub name: String,
    pub amount: f64,
    pub invert: bool,
    pub components: Vec<ComponentReport>,
    pub layers: Vec<MaskedLayer>,
}

impl Eq for MaskReport {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentReport {
    pub id: ComponentId,
    pub index: usize,
    pub name: String,
    pub mode: ComponentMode,
    pub invert: bool,
    pub kind: String,
    pub payload: Value,
    /// Whether this build can evaluate the kind. A stored kind it cannot is reported here and kept
    /// byte for byte, exactly as a layer whose effect has no provider is reported and kept.
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskedLayer {
    pub id: LayerId,
    pub effect: String,
    pub module: Option<String>,
    pub title: Option<String>,
}

/// The listing of one stored stack. Read-only in every sense: it reads the snapshot it was handed
/// and touches nothing.
pub(crate) fn listing(
    entry_id: crate::EntryId,
    recipe: &Recipe,
    registry: &ModuleRegistry,
) -> MaskListing {
    let masks = recipe
        .masks
        .iter()
        .enumerate()
        .map(|(index, mask)| MaskReport {
            id: mask.id.clone(),
            index,
            name: mask.name.clone(),
            amount: mask.amount,
            invert: mask.invert,
            components: mask
                .components
                .iter()
                .enumerate()
                .map(|(index, component)| ComponentReport {
                    id: component.id.clone(),
                    index,
                    name: component.name.clone(),
                    mode: component.mode,
                    invert: component.invert,
                    kind: component.kind.clone(),
                    payload: component.payload.clone(),
                    available: knows_component_kind(&component.kind),
                })
                .collect(),
            layers: recipe
                .layers
                .iter()
                .filter(|layer| layer.mask.as_ref() == Some(&mask.id))
                .map(|layer| {
                    let descriptor = registry
                        .effect(&layer.effect_id)
                        .map(|(module, _)| module.descriptor());
                    MaskedLayer {
                        id: layer.id.clone(),
                        effect: layer.effect_id.clone(),
                        module: descriptor.map(|descriptor| descriptor.id.clone()),
                        title: descriptor.map(|descriptor| descriptor.title.clone()),
                    }
                })
                .collect(),
        })
        .collect();
    MaskListing { entry_id, masks }
}

/// The parameters one command stores on its history entry, which are also its deduplication
/// identity: its declared parameters plus the envelope fields that say *which* objects it addressed.
///
/// The envelope belongs in the identity: `mask.set-invert {invert: true}` on two different masks are
/// two different requests, and a shared request id with different input must be a `conflict` rather
/// than a silently reused result.
pub(crate) fn stored_parameters(
    checked: &Map<String, Value>,
    target: &MaskTarget,
) -> Map<String, Value> {
    let mut stored = checked.clone();
    if let Some(mask) = &target.mask {
        stored.insert("mask".to_owned(), json!(mask.as_str()));
    }
    if let Some(component) = &target.component {
        stored.insert("component".to_owned(), json!(component.as_str()));
    }
    if let Some(name) = &target.name {
        stored.insert("name".to_owned(), json!(name));
    }
    if let Some(stroke) = &target.stroke {
        stored.insert("stroke".to_owned(), json!(stroke.as_str()));
    }
    stored
}

/// What one command would do to this stack: the whole family's behaviour in one place.
///
/// Pure — it reads the recipe it is handed and returns a new one — so the same function answers a
/// commit and a drafted preview, and a drafted gesture therefore previews exactly the stack
/// committing it would write. It reads no pixels and costs `O(masks + components + layers)`.
///
/// `seed` is the pixel a limited stroke was seeded on, as the three sRGB codes the host read at the
/// layer and position [`colour_limit_request`] named. It arrives as an argument rather than being read here because this
/// function reads no pixels — that is what lets a drafted gesture preview exactly the stack its
/// release commits, through the same call.
pub(crate) fn plan(
    command: &MaskCommand,
    recipe: &Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
    registry: &ModuleRegistry,
    seed: Option<[u8; 3]>,
) -> Result<MaskOutcome, Error> {
    command.checked_target(target)?;
    let mut next = recipe.clone();
    // Each arm produces the base label, whether that label already names the mask, and what the
    // command touched. The mask prefix is applied once, below, so there is one label rule.
    //
    // A generated geometry method is dispatched by what it does and the kind it does it to, never by
    // its spelling: `mask.create-radial` reaches the same three arms `mask.create-linear` does, and a
    // kind registered later reaches them without this function learning its name.
    let (base, names_mask, mask_id, component_id, removed) = match (
        command.geometry,
        command.samples,
    ) {
        (Some(geometry), _) => plan_geometry(geometry, &mut next, target, parameters)?,
        (_, Some(samples)) => plan_sample(samples, &mut next, target, parameters)?,
        _ => match command.method {
            "mask.delete" => {
                let index = mask_index(&next, required_mask(target)?)?;
                let mask = next.masks.remove(index);
                // Deleting a mask deletes the layers bound to it. It is destructive, so the label and
                // the result both name what went with it.
                let removed: Vec<RemovedLayer> = next
                    .layers
                    .iter()
                    .filter(|layer| layer.mask.as_ref() == Some(&mask.id))
                    .map(|layer| removed_layer(layer, registry))
                    .collect();
                next.layers
                    .retain(|layer| layer.mask.as_ref() != Some(&mask.id));
                let base = match spoken_titles(&removed) {
                    Some(titles) => format!("Delete {} with {titles}", mask.name),
                    None => format!("Delete {}", mask.name),
                };
                (base, true, Some(mask.id), None, removed)
            }
            "mask.rename" => {
                let index = mask_index(&next, required_mask(target)?)?;
                let name = target
                    .name
                    .clone()
                    .ok_or_else(|| validation("missing required field name for mask.rename"))?;
                let previous = std::mem::replace(&mut next.masks[index].name, name.clone());
                next.masks[index].validate()?;
                let id = next.masks[index].id.clone();
                (
                    format!("Rename {previous} to {name}"),
                    true,
                    Some(id),
                    None,
                    Vec::new(),
                )
            }
            "mask.duplicate" => {
                if next.masks.len() >= MASKS_PER_RECIPE {
                    return Err(Error::new(
                        ErrorKind::ResourceLimit,
                        format!(
                            "recipe already has {MASKS_PER_RECIPE} masks; the limit is {MASKS_PER_RECIPE} masks per recipe"
                        ),
                    ));
                }
                let index = mask_index(&next, required_mask(target)?)?;
                let source = next.masks[index].clone();
                let mut copy = source.clone();
                copy.id = MaskId::new();
                copy.name = next_mask_name(&next);
                // New identities, the same geometry and the same spent ordinals: the copy's next linear
                // component is `Linear 2`, because `Linear 1` already names one of its components.
                for component in &mut copy.components {
                    component.id = ComponentId::new();
                }
                copy.validate()?;
                let id = copy.id.clone();
                next.masks.insert(index + 1, copy);
                // A mask without its adjustments is not a useful copy, so the layers bound to the source
                // are copied with it, each with a new identity and bound to the copy.
                //
                // The copies are legal because `single_layer` is per *target* and the global layer and
                // each mask are distinct targets (`docs/design/masking.md`, "How a mask reaches an
                // effect"): a second masked Basic layer bound to a different mask is a second target, not
                // an ambiguous duplicate, and `ModuleRegistry::compile_layers` checks exactly that pair.
                //
                // They are placed by the ordering rule rather than sorted into it afterwards. The copy
                // sits at `index + 1`, immediately after its source, so a copied layer placed immediately
                // after the layer it was copied from is already after the global layer of its effect and
                // already in mask order among the masked layers of that effect — the two clauses of the
                // rule, satisfied by construction and inside the source layer's own stage region.
                let copied = next
                    .layers
                    .iter()
                    .filter(|layer| layer.mask.as_ref() == Some(&source.id))
                    .count();
                if copied > 0 {
                    let mut layers = Vec::with_capacity(next.layers.len() + copied);
                    for layer in &next.layers {
                        layers.push(layer.clone());
                        if layer.mask.as_ref() == Some(&source.id) {
                            layers.push(Layer {
                                id: LayerId::new(),
                                mask: Some(id.clone()),
                                ..layer.clone()
                            });
                        }
                    }
                    next.layers = layers;
                }
                (
                    format!("Duplicate {}", source.name),
                    true,
                    Some(id),
                    None,
                    Vec::new(),
                )
            }
            "mask.set-amount" => {
                let index = mask_index(&next, required_mask(target)?)?;
                let amount = number(parameters, "amount")?;
                next.masks[index].amount = amount;
                next.masks[index].validate()?;
                let id = next.masks[index].id.clone();
                (
                    format!("Amount {amount}"),
                    false,
                    Some(id),
                    None,
                    Vec::new(),
                )
            }
            "mask.set-invert" => {
                let index = mask_index(&next, required_mask(target)?)?;
                let invert = boolean(parameters, "invert")?;
                next.masks[index].invert = invert;
                let id = next.masks[index].id.clone();
                (
                    inversion_label(invert).to_owned(),
                    false,
                    Some(id),
                    None,
                    Vec::new(),
                )
            }
            "mask.reorder" => {
                let index = mask_index(&next, required_mask(target)?)?;
                let to = position(parameters, "index", next.masks.len(), "masks")?;
                let mask = next.masks.remove(index);
                let (id, name) = (mask.id.clone(), mask.name.clone());
                next.masks.insert(to, mask);
                // Masked layers of one effect are evaluated in their masks' order, so moving a mask
                // moves them with it, in this one transaction, and nothing else moves.
                //
                // The rule lives beside the placement rule it is the other half of, in the registry, and
                // this is its one call site. There is no second re-sort here: an earlier copy in this
                // module predated the placement rule and permuted only the positions masked layers
                // already held, which left a masked layer that should have followed a *global* layer of
                // its effect where it was.
                registry.sort_masked_layers(&mut next.layers, &next.masks);
                (
                    format!("Move {name} to {}", to + 1),
                    true,
                    Some(id),
                    None,
                    Vec::new(),
                )
            }
            "mask.set-component-mode" => {
                let (mask_index, index) = component_at(&next, target)?;
                let mode = mode(parameters)?;
                let mask = &mut next.masks[mask_index];
                mask.components[index].mode = mode;
                let base = format!("{} {}", mask.components[index].name, mode.as_str());
                let component_id = mask.components[index].id.clone();
                // The first component of a mask is always add, so promoting one to subtract or
                // intersect is refused here with the model's own reason.
                mask.validate()?;
                let id = mask.id.clone();
                (base, false, Some(id), Some(component_id), Vec::new())
            }
            "mask.set-component-invert" => {
                let (mask_index, index) = component_at(&next, target)?;
                let invert = boolean(parameters, "invert")?;
                let mask = &mut next.masks[mask_index];
                mask.components[index].invert = invert;
                let base = format!(
                    "{} {}",
                    mask.components[index].name,
                    inversion_label(invert).to_lowercase()
                );
                let component_id = mask.components[index].id.clone();
                let id = mask.id.clone();
                (base, false, Some(id), Some(component_id), Vec::new())
            }
            "mask.delete-component" => {
                let (mask_index, index) = component_at(&next, target)?;
                let mask = &mut next.masks[mask_index];
                // A mask never exists empty from a command, so its last component is not deletable:
                // deleting the mask is the command that removes it, and it says what it removed.
                if mask.components.len() == 1 {
                    return Err(validation(format!(
                        "mask {} has one component; delete the mask rather than its last component",
                        mask.name
                    )));
                }
                let removed = mask.components.remove(index);
                // Removing the leading add component of a mask whose next component subtracts leaves a
                // mask that cannot be read; it is refused with the model's reason rather than promoted.
                mask.validate()?;
                let id = mask.id.clone();
                (
                    format!("Delete {}", removed.name),
                    false,
                    Some(id),
                    Some(removed.id),
                    Vec::new(),
                )
            }
            ADD_STROKE => plan_add_stroke(&mut next, target, parameters, seed)?,
            DELETE_STROKE => plan_delete_stroke(&mut next, target)?,
            "mask.reorder-component" => {
                let (mask_index, index) = component_at(&next, target)?;
                let mask = &mut next.masks[mask_index];
                let to = position(parameters, "index", mask.components.len(), "components")?;
                let component = mask.components.remove(index);
                let (component_id, name) = (component.id.clone(), component.name.clone());
                mask.components.insert(to, component);
                mask.validate()?;
                let id = mask.id.clone();
                (
                    format!("Move {name}"),
                    false,
                    Some(id),
                    Some(component_id),
                    Vec::new(),
                )
            }
            other => {
                return Err(validation(format!("{other} changes no mask")));
            }
        },
    };
    // Nothing changed: the value was already that, or a drag returned to where it began. No entry
    // and no event, exactly as a module's no-op.
    if next == *recipe {
        return Ok(MaskOutcome::NoOp);
    }
    let label = mask_label(base, names_mask, mask_id.as_ref(), &next);
    Ok(MaskOutcome::Change(MaskChange {
        recipe: next,
        label,
        mask: mask_id,
        component: component_id,
        removed_layers: removed,
    }))
}

/// What one command touched: the base history label, whether that label already names its mask, the
/// mask and component it addressed, and the layers it removed.
type Planned = (
    String,
    bool,
    Option<MaskId>,
    Option<ComponentId>,
    Vec<RemovedLayer>,
);

/// The three generated geometry methods, over whichever kind the method was generated for.
///
/// Nothing here names a component kind. `geometry.kind` came from the host's kind table when the
/// method was generated, the parameters the request carries were already validated against that
/// kind's own declarations by the generic check, and the payload is built from the names of those
/// same declarations — so a kind registered later reaches all three arms unchanged.
fn plan_geometry(
    geometry: GeometryMethod,
    next: &mut Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
) -> Result<Planned, Error> {
    let kind = geometry.kind;
    match geometry.op {
        GeometryOp::Create => {
            if next.masks.len() >= MASKS_PER_RECIPE {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "recipe already has {MASKS_PER_RECIPE} masks; the limit is {MASKS_PER_RECIPE} masks per recipe"
                    ),
                ));
            }
            let mut mask = Mask::new(next_mask_name(next));
            // The ordinal comes from the mask and is spent there, so it is never reused.
            let name = mask.next_component_name(kind);
            // The first component of a mask is always `add`: there is nothing yet to subtract from
            // or intersect with, so this command declares no mode at all and a request that sends
            // one is refused by the generic parameter check.
            let component = Component::new(
                name,
                ComponentMode::Add,
                kind,
                geometry_payload(kind, parameters)?,
            );
            let component_id = component.id.clone();
            let mask_id = mask.id.clone();
            mask.components.push(component);
            mask.validate()?;
            next.masks.push(mask);
            Ok((
                format!("Add {}", spoken(kind)),
                false,
                Some(mask_id),
                Some(component_id),
                Vec::new(),
            ))
        }
        GeometryOp::Add => {
            let index = mask_index(next, required_mask(target)?)?;
            let mask = &mut next.masks[index];
            if mask.components.len() >= COMPONENTS_PER_MASK {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "mask {} has {COMPONENTS_PER_MASK} components; the limit is {COMPONENTS_PER_MASK} components per mask",
                        mask.name
                    ),
                ));
            }
            let mode = mode(parameters)?;
            let name = mask.next_component_name(kind);
            let component = Component::new(name, mode, kind, geometry_payload(kind, parameters)?);
            let component_id = component.id.clone();
            mask.components.push(component);
            // The first component of a mask is always add, so a subtract or an intersect arriving at
            // an empty mask is refused here with the model's own reason rather than silently
            // creating a selection of nothing.
            mask.validate()?;
            let base = if mode == ComponentMode::Add {
                format!("Add {}", spoken(kind))
            } else {
                format!("Add {} {}", mode.as_str(), spoken(kind))
            };
            let id = mask.id.clone();
            Ok((base, false, Some(id), Some(component_id), Vec::new()))
        }
        GeometryOp::Set => {
            let (mask_index, index) = component_at(next, target)?;
            let mask = &mut next.masks[mask_index];
            let component = &mut mask.components[index];
            // A component whose kind this build cannot evaluate is `incompatible` and not a mismatch:
            // there is no generated method to point at, the stack is well formed, and the honest
            // answer is that this build cannot read that component at all. It is the same refusal
            // rendering gives, in the same spelling.
            if !knows_component_kind(&component.kind) {
                return Err(unknown_kind(&component.kind));
            }
            // One *known* kind's patch may not reach another known kind's component. Both are named,
            // because a client that picked the wrong generated method has to be told which one to
            // use.
            if component.kind != kind {
                // A kind whose geometry is drawn has no patch method to point at, so the refusal
                // says what is true of it rather than naming a command that does not exist.
                if component_geometry_is_drawn(&component.kind) {
                    return Err(validation(format!(
                        "component {} is a {} component, whose geometry is drawn rather than \
                         patched",
                        component.name, component.kind
                    )));
                }
                return Err(validation(format!(
                    "component {} is a {} component; patch it with mask.set-{}",
                    component.name, component.kind, component.kind
                )));
            }
            let mut payload = component
                .payload
                .as_object()
                .cloned()
                .ok_or_else(|| unknown_kind(&component.kind))?;
            for (field, value) in parameters {
                payload.insert(field.clone(), canonical(value));
            }
            component.payload = Value::Object(payload);
            // A later edit to a component names that component, so the row says which one it was.
            let base = format!("Update {}", component.name);
            let component_id = component.id.clone();
            mask.validate()?;
            let id = mask.id.clone();
            Ok((base, false, Some(id), Some(component_id), Vec::new()))
        }
    }
}

/// `mask.add-stroke`: one painted stroke, and whichever of the three edits its envelope says it is.
///
/// The stroke is captured **before** anything is decided, so a malformed path refuses without having
/// touched the mask table, and it is put in the recipe's own stroke table under its content address.
/// **No coordinate is written into a component payload**: the payload carries the reserved `strokes`
/// field holding addresses, which is what keeps one entry per stroke from copying every earlier
/// stroke of that component into every later entry.
///
/// Capture is the host's, not the desktop's. The desktop decimates before it posts, on the same
/// grid at the same tolerance, and that is idempotent — so an agent that posts a raw path and a hand
/// that drew one reach the same stored bytes, the same address and the same coverage.
fn plan_add_stroke(
    next: &mut Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
    seed: Option<[u8; 3]>,
) -> Result<Planned, Error> {
    let stroke = captured_stroke(parameters)?;
    // A limit is stored with the stroke, from the colour the host read where the stroke began: never
    // from the request, and never read again when the picture is drawn.
    let stroke = match (boolean(parameters, "limit_to_colour")?, seed) {
        (false, _) => stroke,
        (true, Some(seed)) => stroke.with_colour_limit(path::ColourLimit::sampled(
            seed,
            number(parameters, "colour_refine")?,
        )?),
        (true, None) => {
            return Err(validation(
                "a stroke limited to a colour needs the pixel the masked operation receives, and \
                 none was read",
            ));
        }
    };
    let id = next.strokes.insert(stroke);
    // The mode belongs to a component, and only a stroke that makes one may carry it. Appending to a
    // component that already exists is refused rather than silently ignoring the mode, and the
    // refusal names the command that does change one.
    let declared_mode = || -> Result<(), Error> {
        if parameters.contains_key("mode") {
            return Err(validation(
                "a stroke appended to an existing component takes no mode; change a component's \
                 mode with mask.set-component-mode",
            ));
        }
        Ok(())
    };
    match (&target.mask, &target.component) {
        // The first stroke of a session: a mask, a brush component and the stroke, as one entry.
        (None, _) => {
            declared_mode()?;
            if next.masks.len() >= MASKS_PER_RECIPE {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "recipe already has {MASKS_PER_RECIPE} masks; the limit is {MASKS_PER_RECIPE} masks per recipe"
                    ),
                ));
            }
            let mut mask = Mask::new(next_mask_name(next));
            let name = mask.next_component_name(BRUSH);
            // The first component of a mask is always add, exactly as `mask.create-<kind>` makes it.
            let component = Component::new(name, ComponentMode::Add, BRUSH, strokes_payload(&[id]));
            let component_id = component.id.clone();
            let mask_id = mask.id.clone();
            mask.components.push(component);
            mask.validate()?;
            next.masks.push(mask);
            Ok((
                format!("Add {}", spoken(BRUSH)),
                false,
                Some(mask_id),
                Some(component_id),
                Vec::new(),
            ))
        }
        // A further brush on a mask that exists, in the mode the gesture chose before it started.
        (Some(_), None) => {
            let index = mask_index(next, required_mask(target)?)?;
            let mode = optional_mode(parameters)?.unwrap_or(ComponentMode::Add);
            let mask = &mut next.masks[index];
            if mask.components.len() >= COMPONENTS_PER_MASK {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "mask {} has {COMPONENTS_PER_MASK} components; the limit is {COMPONENTS_PER_MASK} components per mask",
                        mask.name
                    ),
                ));
            }
            let name = mask.next_component_name(BRUSH);
            let component = Component::new(name, mode, BRUSH, strokes_payload(&[id]));
            let component_id = component.id.clone();
            mask.components.push(component);
            mask.validate()?;
            let base = if mode == ComponentMode::Add {
                format!("Add {}", spoken(BRUSH))
            } else {
                format!("Add {} {}", mode.as_str(), spoken(BRUSH))
            };
            let mask_id = mask.id.clone();
            Ok((base, false, Some(mask_id), Some(component_id), Vec::new()))
        }
        // Every later stroke on that component: one entry each, named for the component, which is
        // what makes undo walk back one stroke at a time with no second history model.
        (Some(_), Some(_)) => {
            declared_mode()?;
            let (mask_index, index) = component_at(next, target)?;
            let mask = &mut next.masks[mask_index];
            let mask_name = mask.name.clone();
            let component = &mut mask.components[index];
            strokes_reach(component)?;
            let mut held = path::references(
                &component.payload,
                &format!("component {} of mask {mask_name}", component.name),
            )?;
            held.push(id);
            component.payload = strokes_payload(&held);
            let base = format!("Update {}", component.name);
            let component_id = component.id.clone();
            mask.validate()?;
            let mask_id = mask.id.clone();
            Ok((base, false, Some(mask_id), Some(component_id), Vec::new()))
        }
    }
}

/// `mask.delete-stroke`: a **forward edit**, not an undo.
///
/// It removes one stroke and appends one entry, so a stroke made ten entries ago goes while
/// everything after it stays; `history.undo` still walks entries, and the two never mean the same
/// thing. Deleting is well defined because the fold is over the stored order: the remaining strokes
/// produce, bit for bit, the field they would have produced had the deleted one never been made.
///
/// A component holding the same address twice holds two strokes with the same content — painting one
/// path twice builds up, and the second pass is a second object — so this removes the **first** of
/// them, which is the one earliest in the fold.
fn plan_delete_stroke(next: &mut Recipe, target: &MaskTarget) -> Result<Planned, Error> {
    let wanted = target
        .stroke
        .as_ref()
        .ok_or_else(|| validation("missing required field stroke"))?
        .clone();
    let (mask_index, index) = component_at(next, target)?;
    let mask = &mut next.masks[mask_index];
    let mask_name = mask.name.clone();
    let component = &mut mask.components[index];
    strokes_reach(component)?;
    let mut held = path::references(
        &component.payload,
        &format!("component {} of mask {mask_name}", component.name),
    )?;
    let Some(at) = held.iter().position(|held| held == &wanted) else {
        return Err(validation(format!(
            "component {} holds no stroke {wanted}",
            component.name
        )));
    };
    // A component with no stroke covers nothing and is not a thing a person drew, so the last stroke
    // is removed by removing the component — the same rule, and the same wording, that keeps a mask
    // from existing empty.
    if held.len() == 1 {
        return Err(validation(format!(
            "stroke {wanted} is the only stroke of {}; delete the component instead",
            component.name
        )));
    }
    held.remove(at);
    component.payload = strokes_payload(&held);
    let base = format!("Delete a stroke from {}", component.name);
    let component_id = component.id.clone();
    mask.validate()?;
    let mask_id = mask.id.clone();
    Ok((base, false, Some(mask_id), Some(component_id), Vec::new()))
}

/// The component a stroke may reach: one whose geometry is drawn as a path.
///
/// A component of a kind this build cannot evaluate is `incompatible`, exactly as rendering it is; a
/// known kind whose geometry is declared numbers is a `validation` refusal naming the patch method
/// that does edit it, because a client that picked the wrong command has to be told the right one.
fn strokes_reach(component: &Component) -> Result<(), Error> {
    if !knows_component_kind(&component.kind) {
        return Err(unknown_kind(&component.kind));
    }
    if component.kind != BRUSH {
        return Err(validation(format!(
            "component {} is a {} component, whose geometry is declared rather than drawn; patch it \
             with mask.set-{}",
            component.name, component.kind, component.kind
        )));
    }
    Ok(())
}

/// A brush component's stored payload: the host's reserved `strokes` field holding its strokes in
/// order by content address, and nothing else.
fn strokes_payload(strokes: &[StrokeId]) -> Value {
    json!({ path::STROKES_FIELD: strokes })
}

/// Which layer's input a mask's value-based parts read, and the refusal when there is none.
///
/// **The rule, stated once for everything that reads a pixel through a mask:** the pixel is taken at
/// the stage the **first layer bound to this mask in evaluation order** receives. A mask several
/// layers at different stages share therefore has one answer and not several, and that answer is the
/// earliest operation the mask modulates — the one its components were drawn against first.
///
/// A mask no layer is bound to is **refused by name** rather than answered from the source or from
/// the finished frame. It has no operation to be the input of: a colour sampled anywhere else would
/// be in a different domain from the one the selection is evaluated in, and the
/// [range study](../../../docs/design/range-study.md) measures what that costs — a `+0.75 EV` layer
/// ahead of a band takes a sky from fully selected to not selected at all. Refusing says so; seeding
/// from the wrong stage would silently select nothing.
pub(crate) fn input_layer_index(recipe: &Recipe, mask: &MaskId) -> Result<usize, Error> {
    let name = recipe
        .masks
        .iter()
        .find(|held| &held.id == mask)
        .map(|held| held.name.clone())
        .unwrap_or_else(|| mask.as_str().to_owned());
    recipe
        .layers
        .iter()
        .position(|layer| layer.mask.as_ref() == Some(mask))
        .ok_or_else(|| {
            validation(format!(
                "no layer is bound to mask {name}, and reading the pixel an operation receives \
                 needs an operation; apply an adjustment through {name} first"
            ))
        })
}

/// What one `mask.add-stroke` needs read before it can be planned, when it asks for a colour limit:
/// the layer whose input to read, the position to read it at, and the refine to compile with.
///
/// It is answered here, beside the command that asks for it, and the pixel itself is read by the
/// host — the split is deliberate. **The rule** (which layer, and what a stroke starting outside the
/// picture means) belongs with the command family; **the pixel** belongs to the editor, which is the
/// only thing that can evaluate one. So a client cannot supply a colour at any point: the request
/// carries a flag and a path, and the colour that ends up stored is the one the photograph has at
/// the position the stroke started from.
///
/// The position is the stroke's **stored** first position, not the raw one posted, so a hand-drawn
/// path and an agent's raw copy of it seed from the same pixel exactly as they store the same bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LimitRequest {
    /// The layer whose input stage the seed is read from, by [`input_layer_index`].
    pub layer: usize,
    /// The stroke's first stored position, in the content stage's normalized coordinates.
    pub x: f64,
    pub y: f64,
    pub refine: f64,
}

/// Whether this request asks for a colour limit, and everything the host needs to seed it.
pub(crate) fn colour_limit_request(
    command: &MaskCommand,
    recipe: &Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
) -> Result<Option<LimitRequest>, Error> {
    if command.method != ADD_STROKE || !boolean(parameters, "limit_to_colour")? {
        return Ok(None);
    }
    let Some(mask) = target.mask.as_ref() else {
        return Err(validation(
            "a stroke that draws a new mask cannot be limited to a colour: the limit reads the pixel \
             the operation the mask modulates receives, and a new mask is bound to no layer yet. \
             Paint the mask, apply an adjustment through it, then limit the strokes after that",
        ));
    };
    let layer = input_layer_index(recipe, mask)?;
    // The stored first position, which is what makes the seed a property of the stroke the store
    // holds rather than of the raw path one client happened to post.
    let stroke = captured_stroke(parameters)?;
    let [x, y] = stroke
        .points()
        .next()
        .ok_or_else(|| validation("a stroke has no position to read a colour at"))?;
    Ok(Some(LimitRequest {
        layer,
        x,
        y,
        refine: number(parameters, "colour_refine")?,
    }))
}

/// The stroke one `mask.add-stroke` posted, captured on the stored grid.
///
/// The generic parameter check has already refused a path that is not a list of in-range positions
/// and a setting outside its declared range; what happens here is the capture itself — snapping,
/// decimation and the per-stroke bound — which is [`crate::path`]'s contract and not a second copy
/// of it.
fn captured_stroke(parameters: &Map<String, Value>) -> Result<Stroke, Error> {
    Stroke::capture(
        &points(parameters, "points")?,
        number(parameters, "size")?,
        number(parameters, "feather")?,
        number(parameters, "flow")?,
        boolean(parameters, "erase")?,
    )
}

/// A checked `points` parameter as the pairs it declares.
fn points(parameters: &Map<String, Value>, name: &str) -> Result<Vec<[f64; 2]>, Error> {
    let listed = parameters
        .get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| validation(format!("missing required parameter {name}")))?;
    listed
        .iter()
        .map(|point| {
            let pair = point.as_array().ok_or_else(|| {
                validation(format!(
                    "parameter {name} must be a list of [x, y] positions"
                ))
            })?;
            match (
                pair.first().and_then(Value::as_f64),
                pair.get(1).and_then(Value::as_f64),
            ) {
                (Some(x), Some(y)) if pair.len() == 2 => Ok([x, y]),
                _ => Err(validation(format!(
                    "parameter {name} must be a list of [x, y] positions"
                ))),
            }
        })
        .collect()
}

/// The mode a request carried, or none when it carried none. Unlike [`mode`], absence is an answer
/// rather than a refusal: the one command that declares an optional mode does something different
/// without it.
fn optional_mode(parameters: &Map<String, Value>) -> Result<Option<ComponentMode>, Error> {
    if !parameters.contains_key("mode") {
        return Ok(None);
    }
    mode(parameters).map(Some)
}

/// The two generated sample methods, over whichever sampling kind the method was generated for.
///
/// Nothing here names a component kind either: `samples.kind` came from the host's kind table when
/// the method was generated, the parameters were validated against that kind's own sample
/// declarations by the generic check, and the list is read and written through the reserved
/// `samples` field every sampling kind's payload carries.
fn plan_sample(
    samples: SampleMethod,
    next: &mut Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
) -> Result<Planned, Error> {
    let kind = samples.kind;
    let limit = component_sample_limit(kind).ok_or_else(|| unknown_kind(kind))?;
    let (mask_index, index) = component_at(next, target)?;
    let mask = &mut next.masks[mask_index];
    let component = &mut mask.components[index];
    if !knows_component_kind(&component.kind) {
        return Err(unknown_kind(&component.kind));
    }
    if component.kind != kind {
        return Err(validation(format!(
            "component {} is a {} component and holds no sampled colours of a {kind}",
            component.name, component.kind
        )));
    }
    let mut payload = component
        .payload
        .as_object()
        .cloned()
        .ok_or_else(|| unknown_kind(&component.kind))?;
    let mut stored: Vec<Value> = payload
        .get(SAMPLES_FIELD)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let base = match samples.op {
        SampleOp::Add => {
            let picked = Value::Array(
                component_sample_parameters(kind)
                    .ok_or_else(|| unknown_kind(kind))?
                    .iter()
                    .map(|declared| {
                        parameters
                            .get(&declared.name)
                            .map(canonical)
                            .ok_or_else(|| {
                                validation(format!(
                                    "missing required parameter {} for a {kind} sample",
                                    declared.name
                                ))
                            })
                    })
                    .collect::<Result<Vec<Value>, Error>>()?,
            );
            // Sampling a colour the component already holds is a no-op, because the selection folds
            // its samples by nearest and a duplicate changes no pixel's coverage. Storing it anyway
            // would spend one of the component's few swatches on nothing.
            if !stored.contains(&picked) {
                if stored.len() >= limit {
                    return Err(Error::new(
                        ErrorKind::ResourceLimit,
                        format!(
                            "component {} already holds {limit} sampled colours; the limit is \
                             {limit} per {kind} component",
                            component.name
                        ),
                    ));
                }
                stored.push(picked);
            }
            format!("Sample {}", component.name)
        }
        SampleOp::Delete => {
            let at = position(parameters, "index", stored.len(), "sampled colours")?;
            stored.remove(at);
            format!("Remove a sample from {}", component.name)
        }
    };
    payload.insert(SAMPLES_FIELD.to_owned(), Value::Array(stored));
    component.payload = Value::Object(payload);
    let component_id = component.id.clone();
    mask.validate()?;
    let id = mask.id.clone();
    Ok((base, false, Some(id), Some(component_id), Vec::new()))
}

/// The one label rule: the command's own text, prefixed with the mask's name whenever the resulting
/// stack carries more than one mask, because a history list shared with every other module cannot
/// afford `Update Linear 1` alone.
///
/// It is evaluated on the *resulting* mask table, so the mask a `mask.create` just made is counted;
/// and a command whose text already names its mask — rename, duplicate, reorder and the destructive
/// delete — is not prefixed with it twice.
fn mask_label(base: String, names_mask: bool, mask: Option<&MaskId>, resulting: &Recipe) -> String {
    if names_mask || resulting.masks.len() < 2 {
        return base;
    }
    match mask.and_then(|id| resulting.masks.iter().find(|mask| &mask.id == id)) {
        Some(mask) => format!("{} · {base}", mask.name),
        None => base,
    }
}

/// `Inverted` and `Not inverted`, one spelling for both levels of inversion.
fn inversion_label(invert: bool) -> &'static str {
    if invert { "Inverted" } else { "Not inverted" }
}

/// A kind as a label says it: `linear`, `luminance range`. Lower case, because the design's history
/// rows read `Add brush` and `Add subtract brush`.
fn spoken(kind: &str) -> String {
    kind.replace('-', " ")
}

/// The provider titles of the layers a destructive command removed, in stack order and without
/// repeating a title: `Basic, Presence`.
fn spoken_titles(removed: &[RemovedLayer]) -> Option<String> {
    let mut titles: Vec<&str> = Vec::new();
    for layer in removed {
        let title = layer.title.as_deref().unwrap_or(layer.effect.as_str());
        if !titles.contains(&title) {
            titles.push(title);
        }
    }
    if titles.is_empty() {
        None
    } else {
        Some(titles.join(", "))
    }
}

fn removed_layer(layer: &Layer, registry: &ModuleRegistry) -> RemovedLayer {
    RemovedLayer {
        id: layer.id.clone(),
        effect: layer.effect_id.clone(),
        title: registry
            .effect(&layer.effect_id)
            .map(|(module, _)| module.descriptor().title.clone()),
    }
}

/// The lowest unused default mask name, so `Mask 1` freed by a delete is available again while a
/// name a person typed is never taken.
fn next_mask_name(recipe: &Recipe) -> String {
    (1..=MASKS_PER_RECIPE + 1)
        .map(|ordinal| format!("Mask {ordinal}"))
        .find(|candidate| !recipe.masks.iter().any(|mask| &mask.name == candidate))
        .unwrap_or_else(|| format!("Mask {}", recipe.masks.len() + 1))
}

fn required_mask(target: &MaskTarget) -> Result<&MaskId, Error> {
    target
        .mask
        .as_ref()
        .ok_or_else(|| validation("missing required field mask"))
}

fn mask_index(recipe: &Recipe, id: &MaskId) -> Result<usize, Error> {
    recipe
        .masks
        .iter()
        .position(|mask| &mask.id == id)
        .ok_or_else(|| validation(format!("unknown mask {id}")))
}

/// The mask and the component one component command addresses.
fn component_at(recipe: &Recipe, target: &MaskTarget) -> Result<(usize, usize), Error> {
    let mask = mask_index(recipe, required_mask(target)?)?;
    let id = target
        .component
        .as_ref()
        .ok_or_else(|| validation("missing required field component"))?;
    let index = recipe.masks[mask]
        .components
        .iter()
        .position(|component| &component.id == id)
        .ok_or_else(|| {
            validation(format!(
                "mask {} has no component {id}",
                recipe.masks[mask].name
            ))
        })?;
    Ok((mask, index))
}

/// The declared payload fields of one component kind: the names of the parameters that kind's own
/// module declares, so a geometry parameter and a stored field are one spelling because they are one
/// declaration.
fn geometry_fields(kind: &str) -> Option<Vec<String>> {
    component_parameters(kind, true).map(|parameters| {
        parameters
            .into_iter()
            .map(|parameter| parameter.name)
            .collect()
    })
}

/// The stored payload of a new component of `kind`, built from that kind's declared geometry.
///
/// Numbers are canonicalized to `f64`, so a client that sends `0` where another sends `0.0` writes
/// the same bytes, and setting a field back to what it already held is recognized as a no-op.
fn geometry_payload(kind: &str, parameters: &Map<String, Value>) -> Result<Value, Error> {
    let fields = geometry_fields(kind).ok_or_else(|| unknown_kind(kind))?;
    let mut payload = Map::new();
    for field in &fields {
        let value = parameters.get(field).ok_or_else(|| {
            validation(format!(
                "missing required parameter {field} for a {kind} component"
            ))
        })?;
        payload.insert(field.clone(), canonical(value));
    }
    // A sampling kind's list starts empty and present, rather than absent until the first pick: a
    // payload whose shape depends on its history is a payload every reader has to special-case, and
    // an unsampled component is a real state — it selects nothing — not a missing one.
    if component_sample_limit(kind).is_some() {
        payload.insert(SAMPLES_FIELD.to_owned(), Value::Array(Vec::new()));
    }
    Ok(Value::Object(payload))
}

/// Every number a mask command stores is an `f64`, whatever JSON spelling arrived. The generic check
/// already refused anything that is not a finite number.
fn canonical(value: &Value) -> Value {
    match value.as_f64() {
        Some(number) => json!(number),
        None => value.clone(),
    }
}

fn enumeration<'a>(parameters: &'a Map<String, Value>, name: &str) -> Result<&'a str, Error> {
    parameters
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| validation(format!("missing required parameter {name}")))
}

fn number(parameters: &Map<String, Value>, name: &str) -> Result<f64, Error> {
    parameters
        .get(name)
        .and_then(Value::as_f64)
        .ok_or_else(|| validation(format!("missing required parameter {name}")))
}

fn boolean(parameters: &Map<String, Value>, name: &str) -> Result<bool, Error> {
    parameters
        .get(name)
        .and_then(Value::as_bool)
        .ok_or_else(|| validation(format!("missing required parameter {name}")))
}

fn mode(parameters: &Map<String, Value>) -> Result<ComponentMode, Error> {
    match enumeration(parameters, "mode")? {
        "add" => Ok(ComponentMode::Add),
        "subtract" => Ok(ComponentMode::Subtract),
        "intersect" => Ok(ComponentMode::Intersect),
        other => Err(validation(format!("unknown component mode {other}"))),
    }
}

/// A destination index inside a list that currently holds `len` items. The declared range bounds the
/// request; this bounds it against the list it actually addresses, and names what it counted.
fn position(
    parameters: &Map<String, Value>,
    name: &str,
    len: usize,
    what: &str,
) -> Result<usize, Error> {
    let index = parameters
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| validation(format!("missing required parameter {name}")))?;
    let index = usize::try_from(index).unwrap_or(usize::MAX);
    if index >= len {
        return Err(validation(format!(
            "index {index} is outside the {len} {what} of this stack"
        )));
    }
    Ok(index)
}

fn modes() -> Vec<String> {
    ["add", "subtract", "intersect"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// The largest content coordinate a pick can name: one below the per-side admission limit of
/// `docs/design/architecture.md#rendering-and-limits`, exactly as the delivered neutral picker's
/// coordinates are declared. A position inside the declared range but outside the stage the masked
/// layer receives is refused by name when the pixel is read, because only the host knows that stage.
const MAX_COORDINATE: i64 = 16383;

/// One coordinate of a pick, declared as the delivered neutral picker declares its own.
fn sample_coordinate(name: &str, notes: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        unit: Some("px".to_owned()),
        ..parameter(
            name,
            ParameterKind::Integer {
                min: 0,
                max: MAX_COORDINATE,
            },
            true,
            notes,
        )
    }
}

/// The shared descriptor shape the kind-independent commands declare their own values with. A
/// component kind's geometry is *not* declared here: it is declared once in that kind's own module,
/// beside the parser that enforces the same ranges.
fn parameter(name: &str, kind: ParameterKind, required: bool, notes: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.to_owned(),
        kind,
        required,
        default: None,
        unit: None,
        step: None,
        precision: None,
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: None,
        notes: notes.to_owned(),
    }
}

fn command(
    method: &'static str,
    title: &str,
    notes: &str,
    mutates: bool,
    needs: (bool, bool, bool),
    patch: bool,
    parameters: Vec<ParameterDescriptor>,
) -> MaskCommand {
    MaskCommand {
        method,
        mutates,
        needs_mask: Need::of(needs.0),
        needs_component: Need::of(needs.1),
        needs_name: Need::of(needs.2),
        needs_stroke: Need::None,
        geometry: None,
        samples: None,
        action: ActionDescriptor {
            id: method.to_owned(),
            title: title.to_owned(),
            notes: notes.to_owned(),
            // A mask label names the objects it touched and the mask it belongs to, which no
            // template over declared parameters can render; `plan` renders it at commit and the
            // entry stores it, exactly as a module's rendered `summary` is stored.
            summary: None,
            patch,
            parameters,
        },
    }
}

/// The three geometry methods one component kind generates, each declaring exactly that kind's own
/// parameters.
///
/// The method names are leaked for the lifetime of the process, which is what lets a generated
/// command hold the `&'static str` identity every other command holds and a history entry store it
/// as a durable action id. There is one leak per kind per operation, at first use of the table.
fn geometry_commands(kind: &'static str) -> Vec<MaskCommand> {
    let mode = parameter(
        "mode",
        ParameterKind::Enum { options: modes() },
        true,
        "how this component joins the coverage the components before it composed",
    );
    GeometryOp::all()
        .into_iter()
        .map(|op| {
            let method: &'static str = String::leak(format!("{}-{kind}", op.stem()));
            let patch = op == GeometryOp::Set;
            let mut parameters = match op {
                GeometryOp::Add => vec![mode.clone()],
                _ => Vec::new(),
            };
            parameters.extend(
                component_parameters(kind, !patch).expect("a kind from the host's own table"),
            );
            let (title, notes, needs) = match op {
                GeometryOp::Create => (
                    format!("New {} mask", spoken(kind)),
                    format!(
                        "a new mask whose first component is an add {} component; a mask never \
                         exists empty, so the initial component is required and its mode is always \
                         add",
                        spoken(kind)
                    ),
                    (false, false, false),
                ),
                GeometryOp::Add => (
                    format!("Add {}", spoken(kind)),
                    format!(
                        "a second, third, … {} component of a mask, with its mode given explicitly \
                         rather than guessed from a modifier key",
                        spoken(kind)
                    ),
                    (true, false, false),
                ),
                GeometryOp::Set => (
                    format!("Update {}", spoken(kind)),
                    format!(
                        "a field patch over one {0} component's geometry; the fields the request \
                         names are validated and merged over the stored payload, and a component of \
                         any other kind is refused by name rather than patched with a {0}'s fields",
                        spoken(kind)
                    ),
                    (true, true, false),
                ),
            };
            MaskCommand {
                method,
                mutates: true,
                needs_mask: Need::of(needs.0),
                needs_component: Need::of(needs.1),
                needs_name: Need::of(needs.2),
                needs_stroke: Need::None,
                geometry: Some(GeometryMethod { op, kind }),
                samples: None,
                action: ActionDescriptor {
                    id: method.to_owned(),
                    title,
                    notes,
                    summary: None,
                    patch,
                    parameters,
                },
            }
        })
        .collect()
}

/// The two sample methods one sampling kind generates: one adds a colour the canvas picked, one
/// removes the swatch at an index.
///
/// The method names are leaked for the lifetime of the process, exactly as the geometry methods'
/// are, so a generated command holds the `&'static str` identity every other command holds and a
/// history entry stores it as a durable action id.
fn sample_commands(kind: &'static str) -> Vec<MaskCommand> {
    let limit = component_sample_limit(kind).expect("a sampling kind from the host's own table");
    SampleOp::all()
        .into_iter()
        .map(|op| {
            let method: &'static str = String::leak(op.method(kind));
            let (title, notes, parameters) = match op {
                SampleOp::Add => (
                    format!("Sample {}", spoken(kind)),
                    format!(
                        "add one sampled colour to a {0} component, in linear sRGB in the domain of \
                         the operation this mask modulates — the value a pick on the canvas reads \
                         from the stage that operation receives. At most {limit} of them; sampling \
                         a colour the component already holds changes nothing, because the \
                         selection folds its samples by nearest and a duplicate is a no-op",
                        spoken(kind)
                    ),
                    component_sample_parameters(kind)
                        .expect("a sampling kind from the host's own table"),
                ),
                SampleOp::Delete => (
                    format!("Remove {} sample", spoken(kind)),
                    format!(
                        "remove one sampled colour of a {} component by its position in the list, \
                         so a swatch picked by accident is undone on its own rather than by \
                         clearing them all",
                        spoken(kind)
                    ),
                    vec![parameter(
                        "index",
                        ParameterKind::Integer {
                            min: 0,
                            max: limit as i64 - 1,
                        },
                        true,
                        "the sample's position in the component's list of sampled colours",
                    )],
                ),
            };
            MaskCommand {
                method,
                mutates: true,
                needs_mask: Need::Required,
                needs_component: Need::Required,
                needs_name: Need::None,
                needs_stroke: Need::None,
                geometry: None,
                samples: Some(SampleMethod { op, kind }),
                action: ActionDescriptor {
                    id: method.to_owned(),
                    title,
                    notes,
                    summary: None,
                    patch: false,
                    parameters,
                },
            }
        })
        .collect()
}

static COMMANDS: LazyLock<Vec<MaskCommand>> = LazyLock::new(|| {
    let mode = |required| {
        parameter(
            "mode",
            ParameterKind::Enum { options: modes() },
            required,
            "how this component joins the coverage the components before it composed",
        )
    };
    let invert = |notes: &str| parameter("invert", ParameterKind::Boolean, true, notes);
    let index = |limit: i64, notes: &str| {
        parameter(
            "index",
            ParameterKind::Integer {
                min: 0,
                max: limit - 1,
            },
            true,
            notes,
        )
    };
    let mut commands = vec![
        command(
            LIST,
            "Masks",
            "every mask of one stack with its components, values, amount, invert and the layers bound to it; read-only, writes no history and emits no event",
            false,
            (false, false, false),
            false,
            Vec::new(),
        ),
        command(
            SAMPLE_INPUT,
            "Sample input",
            "the pixel the operation this mask modulates receives, at one content position, as \
             linear-sRGB r, g and b. Read-only: it writes no history and emits no event. It is where \
             a canvas pick gets the colour a colour range's swatch is, because a range selection is \
             evaluated on the operation's input while the frame a client can see holds that \
             operation's output — so a colour read from the picture would be a different colour. The \
             position is a pixel of the stage that operation's layer receives, and one outside it is \
             refused rather than clamped",
            false,
            (true, false, false),
            false,
            vec![
                sample_coordinate(
                    "x",
                    "the content column to read, in the stage the masked layer receives",
                ),
                sample_coordinate("y", "the content row to read, in the same stage"),
            ],
        ),
        command(
            "mask.delete",
            "Delete mask",
            "delete a mask and the layers bound to it; destructive, so the history label and the result both name the layers it removed",
            true,
            (true, false, false),
            false,
            Vec::new(),
        ),
        command(
            "mask.rename",
            "Rename mask",
            "set a mask's display name; a name is a person's text and never an identity",
            true,
            (true, false, true),
            false,
            Vec::new(),
        ),
        command(
            "mask.duplicate",
            "Duplicate mask",
            "a copy of a mask, its components and the layers bound to it, with new identities, placed after it; a mask without its adjustments is not a useful copy",
            true,
            (true, false, false),
            false,
            Vec::new(),
        ),
        command(
            "mask.set-amount",
            "Amount",
            "the whole-mask amount multiplying the composed coverage",
            true,
            (true, false, false),
            false,
            vec![ParameterDescriptor {
                step: Some(1.0),
                precision: Some(0),
                ..parameter(
                    "amount",
                    ParameterKind::Number {
                        min: 0.0,
                        max: Mask::FULL_AMOUNT,
                    },
                    true,
                    "0..=100, multiplying the composed coverage",
                )
            }],
        ),
        command(
            "mask.set-invert",
            "Invert mask",
            "invert the composed coverage of a whole mask, before its amount",
            true,
            (true, false, false),
            false,
            vec![invert("invert the composed coverage before the amount")],
        ),
        command(
            "mask.reorder",
            "Move mask",
            "move a mask in the masks list and, with it, the masked layers of every effect, in one transaction; nothing else moves",
            true,
            (true, false, false),
            false,
            vec![index(
                MASKS_PER_RECIPE as i64,
                "the mask's new position in the masks list",
            )],
        ),
        command(
            "mask.set-component-mode",
            "Component mode",
            "change a component's role in the composition after the fact; the first component of a mask is always add",
            true,
            (true, true, false),
            false,
            vec![mode(true)],
        ),
        command(
            "mask.set-component-invert",
            "Invert component",
            "invert one component's own coverage before it is combined",
            true,
            (true, true, false),
            false,
            vec![invert(
                "invert this component's coverage before it is combined",
            )],
        ),
        command(
            "mask.delete-component",
            "Delete component",
            "remove one component from a mask; a mask is never empty, so its last component is not deletable",
            true,
            (true, true, false),
            false,
            Vec::new(),
        ),
        command(
            "mask.reorder-component",
            "Move component",
            "move a component inside its mask; the composition reads the list in order",
            true,
            (true, true, false),
            false,
            vec![index(
                COMPONENTS_PER_MASK as i64,
                "the component's new position in its mask's component list",
            )],
        ),
    ];
    // The brush's own two. They are declared here rather than generated from the kind table because
    // a brush's geometry is drawn: there is no number a `mask.set-brush` could patch, and what these
    // carry is a path and the brush it was drawn with.
    //
    // `mask.add-stroke` is one command over three edits because painting is one gesture: where the
    // brush lands decides whether the stroke drew a mask, put a second brush on one, or added to the
    // brush already there, and the envelope says which. The history label follows that — `Add brush`,
    // `Add subtract brush`, `Update Brush 1` — so undo walks back one stroke at a time while
    // rendering still sees one component whose strokes are already combined.
    commands.push(MaskCommand {
        method: ADD_STROKE,
        mutates: true,
        needs_mask: Need::Optional,
        needs_component: Need::Optional,
        needs_name: Need::None,
        needs_stroke: Need::None,
        geometry: None,
        samples: None,
        action: ActionDescriptor {
            id: ADD_STROKE.to_owned(),
            title: "Paint".to_owned(),
            notes: "one brush stroke: with no mask it draws a new one whose first component is an \
                    add brush, with a mask and no component it puts a further brush on that mask in \
                    the mode given, and with both it appends the stroke to that brush. The path is \
                    snapped to the stored grid and decimated there before it is stored, so the same \
                    posted path always produces the same stored stroke"
                .to_owned(),
            summary: None,
            patch: false,
            parameters: vec![
                parameter(
                    "points",
                    ParameterKind::Points {
                        points_min: 1,
                        points_max: POINTS_PER_STROKE,
                    },
                    true,
                    "the stroke's path, in the content stage's normalized coordinates, in drawn \
                     order; a one-position path is a single dab",
                ),
                ParameterDescriptor {
                    step: Some(0.01),
                    fine_step: Some(0.002),
                    precision: Some(4),
                    ..parameter(
                        "size",
                        ParameterKind::Number {
                            // A stroke's radius is a stored distance and takes the study's own
                            // floor; the ceiling is the path store's, which is the largest radius a
                            // stored stroke can hold.
                            min: DISTANCE_MIN,
                            max: SIZE_MAX,
                        },
                        true,
                        "the brush's radius in mask-space units, one unit being the content stage's \
                         height on both axes, so a round brush is round at any aspect ratio",
                    )
                },
                ParameterDescriptor {
                    step: Some(5.0),
                    precision: Some(0),
                    ..parameter(
                        "feather",
                        ParameterKind::Number { min: 0.0, max: 100.0 },
                        true,
                        "the ramp's width as a percentage of the radius; 0 is an explicit hard edge",
                    )
                },
                ParameterDescriptor {
                    step: Some(5.0),
                    precision: Some(0),
                    ..parameter(
                        "flow",
                        ParameterKind::Number { min: 0.0, max: 100.0 },
                        true,
                        "the coverage one pass of this stroke reaches, 0..=100. There is no density: \
                         its meaning depends on a build-up model along a single stroke, which would \
                         make coverage depend on stamp spacing and therefore on resolution",
                    )
                },
                parameter(
                    "erase",
                    ParameterKind::Boolean,
                    true,
                    "this stroke removes coverage rather than adding it, for the whole of its life",
                ),
                ParameterDescriptor {
                    default: Some(json!(false)),
                    ..parameter(
                        "limit_to_colour",
                        ParameterKind::Boolean,
                        true,
                        "limit this stroke to the colour under the brush where it began: the host \
                         reads the pixel the operation this mask modulates receives at the stroke's \
                         first position, stores it with the stroke, and multiplies the stroke's \
                         coverage by the similarity to it. No colour is sent — a request names the \
                         limit, never the colour, so what is stored is always a colour the \
                         photograph has at that position. It is a per-pixel colour test and not \
                         Lightroom's Auto Mask: it knows nothing about edges or connectivity, so it \
                         also paints a matching colour anywhere else the stroke passes over. A mask \
                         no layer is bound to has no operation to read an input from and is refused \
                         by name",
                    )
                },
                ParameterDescriptor {
                    unit: Some("%".to_owned()),
                    step: Some(1.0),
                    precision: Some(1),
                    fine_step: Some(0.1),
                    zero: Some(REFINE_DEFAULT),
                    default: Some(json!(REFINE_DEFAULT)),
                    ..parameter(
                        "colour_refine",
                        ParameterKind::Number {
                            min: REFINE_MIN,
                            max: REFINE_MAX,
                        },
                        true,
                        "how tight the colour limit is, on the colour range's own refine axis and \
                         with the same meaning: a higher refine is always a narrower hold, \
                         geometrically between a whole colour family at 0 and one flat patch at 100. \
                         Ignored, and stored nowhere, by a stroke that carries no limit",
                    )
                },
                parameter(
                    "mode",
                    ParameterKind::Enum { options: modes() },
                    false,
                    "how the brush this stroke creates joins the components before it; only a \
                     stroke that makes a component on an existing mask may carry one, and a mask's \
                     first component is always add",
                ),
            ],
        },
    });
    commands.push(MaskCommand {
        method: DELETE_STROKE,
        mutates: true,
        needs_mask: Need::Required,
        needs_component: Need::Required,
        needs_name: Need::None,
        needs_stroke: Need::Required,
        geometry: None,
        samples: None,
        action: ActionDescriptor {
            id: DELETE_STROKE.to_owned(),
            title: "Delete stroke".to_owned(),
            notes: "remove one stroke from a brush component. A forward edit and not an undo: it \
                    appends one entry, so a stroke made ten entries ago goes while everything after \
                    it stays. A component's last stroke is not deletable; delete the component"
                .to_owned(),
            summary: None,
            patch: false,
            parameters: Vec::new(),
        },
    });
    // The geometry methods, generated from the host's kind table: registering a kind with declared
    // geometry is what makes it creatable, addable and patchable, and nothing above has to be edited
    // for that to happen. A kind whose geometry is *drawn* declares no parameters and generates none
    // of these: there is no number a `mask.set-brush` could patch, and a create method over an empty
    // parameter list would advertise a component a gesture has to fill in afterwards.
    for kind in declared_geometry_kinds() {
        commands.extend(geometry_commands(kind));
    }
    // The sample methods, generated the same way from the same table: a kind that holds a list of
    // sampled colours becomes samplable by being registered with a sample limit, and nothing above
    // is edited for that to happen.
    for kind in sampling_kinds() {
        commands.extend(sample_commands(kind));
    }
    debug_assert!(
        {
            let mut seen: Vec<&str> = commands.iter().map(|command| command.method).collect();
            seen.sort_unstable();
            let before = seen.len();
            seen.dedup();
            seen.len() == before
        },
        "two mask commands share a method name, so `find` could only ever answer with one of them"
    );
    commands
});

static CONTROLS: LazyLock<Vec<Control>> = LazyLock::new(|| {
    let mut controls = vec![
        Control::Number {
            action: "mask.set-amount".to_owned(),
            parameter: "amount".to_owned(),
            label: "Amount".to_owned(),
            style: NumberStyle::Slider,
            rail: None,
            reset: None,
        },
        Control::Toggle {
            action: "mask.set-invert".to_owned(),
            parameter: "invert".to_owned(),
            label: "Invert".to_owned(),
        },
        Control::Choice {
            action: "mask.set-component-mode".to_owned(),
            parameter: "mode".to_owned(),
            label: "Mode".to_owned(),
            style: ChoiceStyle::Segmented,
        },
        Control::Toggle {
            action: "mask.set-component-invert".to_owned(),
            parameter: "invert".to_owned(),
            label: "Invert component".to_owned(),
        },
    ];
    // Every handle has a number field, for every kind, generated from the same declarations the
    // patch method declares. The control's **action** names the kind it belongs to — a radius is a
    // `mask.set-radial` control and a gradient endpoint a `mask.set-linear` one — so a panel selects
    // the controls of the component it has open without a second table saying which are which, and a
    // kind registered later brings its own fields with it.
    for kind in declared_geometry_kinds() {
        let action = COMMANDS
            .iter()
            .find(|command| {
                command.geometry
                    == Some(GeometryMethod {
                        op: GeometryOp::Set,
                        kind,
                    })
            })
            .expect("every kind generates its patch method")
            .method;
        controls.extend(
            component_parameters(kind, false)
                .expect("a kind from the host's own table")
                .into_iter()
                .map(|parameter| Control::Number {
                    action: action.to_owned(),
                    label: control_label(&parameter.name),
                    parameter: parameter.name,
                    style: NumberStyle::Field,
                    rail: None,
                    reset: None,
                }),
        );
    }
    controls
});

/// A stored field's name as a control shows it: `x0` is `X0`, `radius_x` is `Radius X`. Short
/// segments stay upper case because they are axis names, not words.
fn control_label(field: &str) -> String {
    field
        .split('_')
        .map(|word| {
            if word.len() <= 2 {
                word.to_uppercase()
            } else {
                let mut characters = word.chars();
                match characters.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ModuleRegistry {
        ModuleRegistry::builtin()
    }

    /// Plan a command that needs no colour read, which is every command here: only a stroke asking to
    /// be limited to a colour takes a seed, and the host reads that one — so the tests that cover it
    /// call [`super::plan`] with the seed themselves.
    fn plan(
        command: &MaskCommand,
        recipe: &Recipe,
        target: &MaskTarget,
        parameters: &Map<String, Value>,
        registry: &ModuleRegistry,
    ) -> Result<MaskOutcome, Error> {
        super::plan(command, recipe, target, parameters, registry, None)
    }

    fn linear(x0: f64, y0: f64, x1: f64, y1: f64) -> Map<String, Value> {
        let mut parameters = Map::new();
        for (name, value) in [("x0", x0), ("y0", y0), ("x1", x1), ("y1", y1)] {
            parameters.insert(name.into(), json!(value));
        }
        parameters
    }

    fn radial(x: f64, y: f64, radius: f64, feather: f64) -> Map<String, Value> {
        let mut parameters = Map::new();
        for (name, value) in [
            ("x", x),
            ("y", y),
            ("radius_x", radius),
            ("radius_y", radius),
            ("angle", 0.0),
            ("feather", feather),
        ] {
            parameters.insert(name.into(), json!(value));
        }
        parameters
    }

    fn apply(
        recipe: &Recipe,
        method: &str,
        target: MaskTarget,
        parameters: Map<String, Value>,
    ) -> Result<MaskChange, Error> {
        let command = find(method).expect("a declared command");
        match plan(command, recipe, &target, &parameters, &registry())? {
            MaskOutcome::NoOp => panic!("{method} changed nothing"),
            MaskOutcome::Change(change) => Ok(change),
        }
    }

    fn created() -> (Recipe, MaskChange) {
        let recipe = Recipe::default();
        let change = apply(
            &recipe,
            "mask.create-linear",
            MaskTarget::default(),
            linear(0.0, 0.0, 0.0, 1.0),
        )
        .unwrap();
        (change.recipe.clone(), change)
    }

    #[test]
    fn the_declared_family_is_the_designs_method_table() {
        let methods: Vec<&str> = all().iter().map(|command| command.method).collect();
        assert_eq!(
            methods,
            [
                // The kind-independent commands, in the order the design's method table lists them.
                "mask.list",
                "mask.sample-input",
                "mask.delete",
                "mask.rename",
                "mask.duplicate",
                "mask.set-amount",
                "mask.set-invert",
                "mask.reorder",
                "mask.set-component-mode",
                "mask.set-component-invert",
                "mask.delete-component",
                "mask.reorder-component",
                // The brush's own two, declared rather than generated: a brush's geometry is drawn,
                // so it has no create, add or set method and these are the only way a stroke reaches
                // a mask.
                "mask.add-stroke",
                "mask.delete-stroke",
                // Then three geometry methods per registered kind, generated from the host's own
                // kind table and in its order.
                "mask.create-linear",
                "mask.add-linear",
                "mask.set-linear",
                "mask.create-radial",
                "mask.add-radial",
                "mask.set-radial",
                // Then the two range selections, which the same generation covers because their
                // geometry is declared as numbers.
                "mask.create-luminance-range",
                "mask.add-luminance-range",
                "mask.set-luminance-range",
                "mask.create-colour-range",
                "mask.add-colour-range",
                "mask.set-colour-range",
                // And the sample methods of the one kind that holds a list of picked colours.
                "mask.add-colour-range-sample",
                "mask.delete-colour-range-sample",
            ]
        );
        assert!(
            all()
                .iter()
                .all(|command| command.method == command.action.id),
            "a command's method name is its durable action identity"
        );
        assert!(
            all()
                .iter()
                .all(|command| !crate::valid_name(command.method)),
            "a mask command identity can never be a module action identity"
        );
        // `mask.list` and `mask.sample-input` read; every other command is an ordinary mutation.
        let reading: Vec<&str> = all()
            .iter()
            .filter(|command| !command.mutates)
            .map(|command| command.method)
            .collect();
        assert_eq!(reading, [LIST, SAMPLE_INPUT]);
    }

    /// Registering a kind is **sufficient** to make it creatable, addable and patchable: for every
    /// kind in the host's own table, all three geometry methods exist, are listed by `schema.list`,
    /// and declare exactly that kind's parameters and no other kind's.
    ///
    /// This is the property the delivered `GEOMETRY` table broke — it forced every field through a
    /// normalized-position descriptor and listed only `linear`, so the radial gradient was evaluable
    /// and not creatable. It is asserted over the table rather than over a list of names, so a kind
    /// added later is covered by this test on the day it is registered.
    /// Registering a kind with a sample limit is enough to make its swatches editable: the two
    /// sample methods are generated, listed by `schema.list`, resolvable through the table by the
    /// operation and the kind, and each declares exactly what one swatch or one removal takes.
    ///
    /// The list itself is deliberately not a declared parameter — the closed vocabulary has numbers,
    /// integers, enums, colours, booleans and curves and no list of any of them — so this is the
    /// whole of how a client edits it, and nothing here names a kind.
    #[test]
    fn registering_a_sampling_kind_is_enough_to_make_its_swatches_editable() {
        let schemas = crate::schemas(&registry());
        let listed = schemas["methods"].as_object().expect("a method listing");
        let mut kinds = 0usize;
        for kind in crate::mask::sampling_kinds() {
            kinds += 1;
            let limit = crate::mask::component_sample_limit(kind).expect("a sampling kind");
            // The add method declares one parameter per channel of a sampled colour, each a finite
            // number, and nothing else: the swatch is the whole of its input.
            let add = sample(SampleOp::Add, kind).expect("its add method is generated");
            assert_eq!(add.method, format!("mask.add-{kind}-sample"));
            assert!(add.mutates);
            // A swatch is edited on one component of one mask, always both and never a name or a
            // stroke: the envelope is fixed, as it is for every command but `mask.add-stroke`.
            assert_eq!(add.needs_mask, Need::Required);
            assert_eq!(add.needs_component, Need::Required);
            assert_eq!(add.needs_name, Need::None);
            assert_eq!(add.needs_stroke, Need::None);
            assert!(!add.action.patch, "a swatch is appended, not patched");
            let declared: Vec<&str> = add
                .action
                .parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
                .collect();
            assert_eq!(
                declared,
                crate::mask::component_sample_parameters(kind)
                    .expect("a sampling kind")
                    .iter()
                    .map(|parameter| parameter.name.as_str())
                    .collect::<Vec<_>>()
            );
            for parameter in &add.action.parameters {
                let ParameterKind::Number { min, max } = parameter.kind else {
                    panic!("{} is not a number", parameter.name);
                };
                assert!(min.is_finite() && max.is_finite() && min < max);
                assert!(parameter.unit.is_some() && parameter.step.is_some());
            }
            // The delete method takes the swatch's position, bounded by the kind's own limit, so a
            // request past the list is refused by the declaration rather than by the plan.
            let delete = sample(SampleOp::Delete, kind).expect("its delete method is generated");
            assert_eq!(delete.method, format!("mask.delete-{kind}-sample"));
            let index = delete
                .action
                .parameter("index")
                .expect("a position to remove");
            assert_eq!(
                index.kind,
                ParameterKind::Integer {
                    min: 0,
                    max: limit as i64 - 1
                }
            );
            for method in [add.method, delete.method] {
                assert!(listed.get(method).is_some(), "schema.list omits {method}");
                assert_eq!(
                    find(method).map(|command| command.method),
                    Some(method),
                    "dispatch cannot resolve {method}"
                );
            }
        }
        assert_eq!(
            kinds, 1,
            "the colour range is this build's one kind that holds sampled colours"
        );
        // And the other side: a kind that samples nothing generates neither method and says so.
        for kind in crate::mask::component_kinds() {
            if crate::mask::component_sample_limit(kind).is_some() {
                continue;
            }
            assert!(sample(SampleOp::Add, kind).is_none(), "{kind}");
            assert!(sample(SampleOp::Delete, kind).is_none(), "{kind}");
            assert!(
                crate::mask::component_sample_parameters(kind).is_none(),
                "{kind}"
            );
        }
    }

    #[test]
    fn registering_a_kind_is_enough_to_make_it_creatable_addable_and_patchable() {
        let schemas = crate::schemas(&registry());
        let listed = schemas["methods"].as_object().expect("a method listing");
        let mut kinds = 0usize;
        for kind in declared_geometry_kinds() {
            kinds += 1;
            let declared: Vec<String> = component_parameters(kind, true)
                .expect("the table's own kind")
                .into_iter()
                .map(|parameter| parameter.name)
                .collect();
            for (op, method, extra) in [
                (GeometryOp::Create, format!("mask.create-{kind}"), None),
                (GeometryOp::Add, format!("mask.add-{kind}"), Some("mode")),
                (GeometryOp::Set, format!("mask.set-{kind}"), None),
            ] {
                let command =
                    find(&method).unwrap_or_else(|| panic!("{kind} declares no {method}"));
                assert_eq!(command.geometry, Some(GeometryMethod { op, kind }));
                assert_eq!(command.method, command.action.id, "{method}");
                assert!(
                    listed.get(&method).is_some(),
                    "schema.list does not list {method}"
                );
                let names: Vec<&str> = command
                    .action
                    .parameters
                    .iter()
                    .map(|parameter| parameter.name.as_str())
                    .collect();
                let mut expected: Vec<&str> = extra.into_iter().collect();
                expected.extend(declared.iter().map(String::as_str));
                assert_eq!(names, expected, "{method} declares the wrong parameters");
                assert_eq!(
                    command.action.patch,
                    op == GeometryOp::Set,
                    "only a patch method patches"
                );
                // A generated control has to be usable, not merely present: every geometry parameter
                // is a number over a finite range, with the display hints a number field needs and a
                // soft range inside its hard one.
                for parameter in &command.action.parameters {
                    if parameter.name == "mode" {
                        continue;
                    }
                    let where_ = format!("{method} {}", parameter.name);
                    let ParameterKind::Number { min, max } = parameter.kind else {
                        panic!("{where_} is not a number");
                    };
                    assert!(min.is_finite() && max.is_finite() && min < max, "{where_}");
                    assert!(parameter.unit.is_some(), "{where_} declares no unit");
                    let step = parameter.step.expect("a step");
                    let fine = parameter.fine_step.expect("a fine step");
                    assert!(step.is_finite() && step > 0.0, "{where_}");
                    assert!(fine.is_finite() && fine > 0.0 && fine <= step, "{where_}");
                    assert!(parameter.precision.expect("a precision") <= 6, "{where_}");
                    let soft_min = parameter.soft_min.unwrap_or(min);
                    let soft_max = parameter.soft_max.unwrap_or(max);
                    assert!(
                        soft_min >= min && soft_max <= max && soft_min < soft_max,
                        "{where_} declares a soft range outside {min}..={max}"
                    );
                    if let Some(zero) = parameter.zero {
                        assert!((min..=max).contains(&zero), "{where_}");
                    }
                }
            }
        }
        assert_eq!(
            kinds, 4,
            "linear, radial and the two range selections are the kinds whose geometry is declared \
             as numbers"
        );
        // And the other side of the same contract: a kind whose geometry is drawn is evaluable
        // without being generated over, so it registers, parses and renders while declaring no
        // parameter, no geometry method and no control.
        let drawn: Vec<&str> = crate::mask::component_kinds()
            .filter(|kind| component_geometry_is_drawn(kind))
            .collect();
        assert_eq!(drawn, vec!["brush"]);
        for kind in drawn {
            assert!(knows_component_kind(kind));
            assert!(component_parameters(kind, true).is_none());
            for method in [
                format!("mask.create-{kind}"),
                format!("mask.add-{kind}"),
                format!("mask.set-{kind}"),
            ] {
                assert!(find(&method).is_none(), "{method} should not be generated");
                assert!(listed.get(&method).is_none(), "schema.list lists {method}");
            }
            assert!(
                !controls().iter().any(|control| matches!(
                    control,
                    Control::Number { action, .. } if action.contains(kind)
                )),
                "a drawn kind declares no control"
            );
        }
    }

    #[test]
    fn every_control_binds_to_a_parameter_its_own_command_declares() {
        for control in controls() {
            let (action, parameter) = match control {
                Control::Number {
                    action, parameter, ..
                }
                | Control::Toggle {
                    action, parameter, ..
                }
                | Control::Choice {
                    action, parameter, ..
                } => (action, parameter),
                other => panic!("unexpected mask control {other:?}"),
            };
            let command = find(action).unwrap_or_else(|| panic!("no command {action}"));
            assert!(
                command.action.parameter(parameter).is_some(),
                "{action} declares no parameter {parameter}"
            );
        }
    }

    #[test]
    fn a_created_mask_carries_one_add_component_named_from_its_kind() {
        let (recipe, change) = created();
        assert_eq!(recipe.masks.len(), 1);
        let mask = &recipe.masks[0];
        assert_eq!(mask.name, "Mask 1");
        assert_eq!(mask.amount, Mask::FULL_AMOUNT);
        assert!(!mask.invert);
        assert_eq!(mask.components.len(), 1);
        assert_eq!(mask.components[0].name, "Linear 1");
        assert_eq!(mask.components[0].mode, ComponentMode::Add);
        assert_eq!(
            mask.components[0].payload,
            json!({"x0":0.0,"y0":0.0,"x1":0.0,"y1":1.0})
        );
        assert_eq!(change.label, "Add linear", "one mask needs no prefix");
        assert_eq!(change.mask.as_ref(), Some(&mask.id));
        assert_eq!(change.component.as_ref(), Some(&mask.components[0].id));
    }

    #[test]
    fn an_ordinal_is_never_reused_so_one_label_means_one_component() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let target = MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        };
        let mut add = linear(0.2, 0.0, 0.8, 1.0);
        add.insert("mode".into(), json!("subtract"));
        let second = apply(&recipe, "mask.add-linear", target.clone(), add).unwrap();
        assert_eq!(second.recipe.masks[0].components[1].name, "Linear 2");
        assert_eq!(second.label, "Add subtract linear");
        // Delete the second and add another of the same kind: the freed ordinal is not reused.
        let component = second.recipe.masks[0].components[1].id.clone();
        let deleted = apply(
            &second.recipe,
            "mask.delete-component",
            MaskTarget {
                component: Some(component),
                ..target.clone()
            },
            Map::new(),
        )
        .unwrap();
        assert_eq!(deleted.label, "Delete Linear 2");
        assert_eq!(deleted.recipe.masks[0].components.len(), 1);
        let mut again = linear(0.3, 0.0, 0.9, 1.0);
        again.insert("mode".into(), json!("intersect"));
        let third = apply(&deleted.recipe, "mask.add-linear", target, again).unwrap();
        assert_eq!(
            third.recipe.masks[0].components[1].name, "Linear 3",
            "an entry reading Update Linear 2 can only ever mean the component it was written about"
        );
    }

    #[test]
    fn a_later_edit_names_the_component_and_a_second_mask_names_the_mask() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let component = recipe.masks[0].components[0].id.clone();
        let target = MaskTarget {
            mask: Some(mask.clone()),
            component: Some(component.clone()),
            ..MaskTarget::default()
        };
        let mut patch = Map::new();
        patch.insert("y1".into(), json!(0.6));
        let updated = apply(&recipe, "mask.set-linear", target.clone(), patch.clone()).unwrap();
        assert_eq!(updated.label, "Update Linear 1");
        assert_eq!(
            updated.recipe.masks[0].components[0].payload,
            json!({"x0":0.0,"y0":0.0,"x1":0.0,"y1":0.6}),
            "a patch merges over the stored payload"
        );
        // A second mask exists, so every row that does not already name its mask names it.
        let two = apply(
            &updated.recipe,
            "mask.create-linear",
            MaskTarget::default(),
            linear(1.0, 0.0, 1.0, 1.0),
        )
        .unwrap();
        assert_eq!(two.label, "Mask 2 · Add linear");
        let again = apply(&two.recipe, "mask.set-linear", target, {
            let mut patch = Map::new();
            patch.insert("y1".into(), json!(0.4));
            patch
        })
        .unwrap();
        assert_eq!(again.label, "Mask 1 · Update Linear 1");
    }

    #[test]
    fn every_row_of_the_designs_granularity_table_reads_as_it_states() {
        let (one, create) = created();
        assert_eq!(create.label, "Add linear");
        let mask = one.masks[0].id.clone();
        let component = one.masks[0].components[0].id.clone();
        let of_mask = MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        };
        let of_component = MaskTarget {
            component: Some(component),
            ..of_mask.clone()
        };
        // "Drag a radial's handle" → `Update Radial 1`: the same rule over the kind that exists.
        let dragged = apply(&one, "mask.set-linear", of_component.clone(), {
            let mut patch = Map::new();
            patch.insert("x1".into(), json!(0.5));
            patch
        })
        .unwrap();
        assert_eq!(dragged.label, "Update Linear 1");
        // "Add a subtract brush to the same mask" → `Add subtract brush`.
        let mut add = linear(0.1, 0.1, 0.9, 0.9);
        add.insert("mode".into(), json!("subtract"));
        let added = apply(&dragged.recipe, "mask.add-linear", of_mask.clone(), add).unwrap();
        assert_eq!(added.label, "Add subtract linear");
        // "Change Brush 2 to intersect" → `Brush 2 intersect`.
        let second = added.recipe.masks[0].components[1].id.clone();
        let mut mode = Map::new();
        mode.insert("mode".into(), json!("intersect"));
        let changed = apply(
            &added.recipe,
            "mask.set-component-mode",
            MaskTarget {
                component: Some(second),
                ..of_mask.clone()
            },
            mode,
        )
        .unwrap();
        assert_eq!(changed.label, "Linear 2 intersect");
        // The whole-mask modifiers and the component inversion take the same shape.
        let mut amount = Map::new();
        amount.insert("amount".into(), json!(60.0));
        assert_eq!(
            apply(&one, "mask.set-amount", of_mask.clone(), amount)
                .unwrap()
                .label,
            "Amount 60"
        );
        let mut invert = Map::new();
        invert.insert("invert".into(), json!(true));
        assert_eq!(
            apply(&one, "mask.set-invert", of_mask, invert.clone())
                .unwrap()
                .label,
            "Inverted"
        );
        assert_eq!(
            apply(&one, "mask.set-component-invert", of_component, invert)
                .unwrap()
                .label,
            "Linear 1 inverted"
        );
    }

    /// The correction's own property, at the command level: a radial is created, added as a second
    /// component of another kind's mask, and patched on a field no position range could carry.
    #[test]
    fn a_radial_is_created_added_and_patched_through_its_own_generated_methods() {
        let recipe = Recipe::default();
        let created = apply(
            &recipe,
            "mask.create-radial",
            MaskTarget::default(),
            radial(0.5, 0.5, 0.3, 40.0),
        )
        .unwrap();
        assert_eq!(created.label, "Add radial");
        let mask = &created.recipe.masks[0];
        assert_eq!(mask.components[0].name, "Radial 1");
        assert_eq!(mask.components[0].kind, "radial");
        assert_eq!(mask.components[0].mode, ComponentMode::Add);
        assert_eq!(
            mask.components[0].payload,
            json!({"x":0.5,"y":0.5,"radius_x":0.3,"radius_y":0.3,"angle":0.0,"feather":40.0})
        );
        // A linear joins the same mask, subtracting: two kinds, one component list.
        let of_mask = MaskTarget {
            mask: Some(mask.id.clone()),
            ..MaskTarget::default()
        };
        let mut add = linear(0.1, 0.1, 0.9, 0.9);
        add.insert("mode".into(), json!("subtract"));
        let two = apply(&created.recipe, "mask.add-linear", of_mask.clone(), add).unwrap();
        assert_eq!(two.label, "Add subtract linear");
        assert_eq!(
            two.recipe.masks[0].components[1].name, "Linear 1",
            "the ordinal counter is per kind, so a mask's first linear is Linear 1 whatever else it holds"
        );
        // And the radial is patched on a radius, an angle and a feather — the three fields the
        // delivered single geometry table could not express at all.
        let of_radial = MaskTarget {
            component: Some(two.recipe.masks[0].components[0].id.clone()),
            ..of_mask.clone()
        };
        let mut patch = Map::new();
        patch.insert("radius_y".into(), json!(0.45));
        patch.insert("angle".into(), json!(-30.0));
        patch.insert("feather".into(), json!(0.0));
        let patched = apply(&two.recipe, "mask.set-radial", of_radial.clone(), patch).unwrap();
        assert_eq!(patched.label, "Update Radial 1");
        assert_eq!(
            patched.recipe.masks[0].components[0].payload,
            json!({"x":0.5,"y":0.5,"radius_x":0.3,"radius_y":0.45,"angle":-30.0,"feather":0.0}),
            "a patch merges over the stored payload"
        );
        // One kind's patch may not reach another kind's component, and the refusal names both.
        let of_linear = MaskTarget {
            component: Some(two.recipe.masks[0].components[1].id.clone()),
            ..of_mask
        };
        let mut wrong = Map::new();
        wrong.insert("radius_x".into(), json!(0.2));
        assert_eq!(
            plan(
                find("mask.set-radial").unwrap(),
                &two.recipe,
                &of_linear,
                &wrong,
                &registry()
            )
            .unwrap_err()
            .detail,
            "component Linear 1 is a linear component; patch it with mask.set-linear"
        );
    }

    #[test]
    fn a_change_that_changes_nothing_writes_no_entry() {
        let (recipe, _) = created();
        let target = MaskTarget {
            mask: Some(recipe.masks[0].id.clone()),
            component: Some(recipe.masks[0].components[0].id.clone()),
            ..MaskTarget::default()
        };
        let command = find("mask.set-linear").unwrap();
        // The drag ended where it began: the stored payload, in either JSON spelling of a number.
        for value in [json!(1.0), json!(1)] {
            let mut patch = Map::new();
            patch.insert("y1".into(), value);
            assert!(
                matches!(
                    plan(command, &recipe, &target, &patch, &registry()).unwrap(),
                    MaskOutcome::NoOp
                ),
                "returning to the start is a no-op"
            );
        }
        for (method, name, value) in [
            ("mask.set-amount", "amount", json!(100.0)),
            ("mask.set-invert", "invert", json!(false)),
        ] {
            let mut parameters = Map::new();
            parameters.insert(name.into(), value);
            let command = find(method).unwrap();
            let target = MaskTarget {
                mask: Some(recipe.masks[0].id.clone()),
                ..MaskTarget::default()
            };
            assert!(matches!(
                plan(command, &recipe, &target, &parameters, &registry()).unwrap(),
                MaskOutcome::NoOp
            ));
        }
    }

    #[test]
    fn a_mask_never_exists_empty_and_never_begins_by_subtracting() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let component = recipe.masks[0].components[0].id.clone();
        let target = MaskTarget {
            mask: Some(mask),
            component: Some(component),
            ..MaskTarget::default()
        };
        let only = find("mask.delete-component").unwrap();
        assert_eq!(
            plan(only, &recipe, &target, &Map::new(), &registry())
                .unwrap_err()
                .detail,
            "mask Mask 1 has one component; delete the mask rather than its last component"
        );
        let mut mode = Map::new();
        mode.insert("mode".into(), json!("subtract"));
        assert_eq!(
            plan(
                find("mask.set-component-mode").unwrap(),
                &recipe,
                &target,
                &mode,
                &registry()
            )
            .unwrap_err()
            .detail,
            "mask Mask 1 begins with a subtract component; the first component of a mask is always \
             add"
        );
    }

    #[test]
    fn a_reorder_that_puts_a_subtract_first_is_refused_with_the_models_reason() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let of_mask = MaskTarget {
            mask: Some(mask),
            ..MaskTarget::default()
        };
        let mut add = linear(0.1, 0.1, 0.9, 0.9);
        add.insert("mode".into(), json!("subtract"));
        let two = apply(&recipe, "mask.add-linear", of_mask.clone(), add).unwrap();
        let second = two.recipe.masks[0].components[1].id.clone();
        let mut index = Map::new();
        index.insert("index".into(), json!(0));
        let error = plan(
            find("mask.reorder-component").unwrap(),
            &two.recipe,
            &MaskTarget {
                component: Some(second.clone()),
                ..of_mask.clone()
            },
            &index,
            &registry(),
        )
        .unwrap_err();
        assert_eq!(
            error.detail,
            "mask Mask 1 begins with a subtract component; the first component of a mask is always \
             add"
        );
        // Deleting the leading add would leave the same unreadable mask, so it is refused too.
        let first = two.recipe.masks[0].components[0].id.clone();
        assert_eq!(
            plan(
                find("mask.delete-component").unwrap(),
                &two.recipe,
                &MaskTarget {
                    component: Some(first),
                    ..of_mask.clone()
                },
                &Map::new(),
                &registry()
            )
            .unwrap_err()
            .detail,
            "mask Mask 1 begins with a subtract component; the first component of a mask is always \
             add"
        );
        // Moving it to the position it already holds changes nothing at all.
        let mut index = Map::new();
        index.insert("index".into(), json!(1));
        assert!(
            matches!(
                plan(
                    find("mask.reorder-component").unwrap(),
                    &two.recipe,
                    &MaskTarget {
                        component: Some(second),
                        ..of_mask
                    },
                    &index,
                    &registry()
                )
                .unwrap(),
                MaskOutcome::NoOp
            ),
            "moving a component to the position it holds is a no-op"
        );
    }

    /// Both declared limits refuse with a `resource-limit` error that names the count and the
    /// limit, and nothing is written. A limit is a refusal, never a silent truncation and never a
    /// catalog that grows without one.
    #[test]
    fn a_list_at_its_limit_and_a_mask_at_its_limit_are_refused_by_name() {
        // Components per mask: fill one to the limit through the command that fills it.
        let (mut recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let of_mask = MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        };
        while recipe.masks[0].components.len() < COMPONENTS_PER_MASK {
            let mut add = linear(0.1, 0.1, 0.9, 0.9);
            add.insert("mode".into(), json!("add"));
            recipe = apply(&recipe, "mask.add-linear", of_mask.clone(), add)
                .unwrap()
                .recipe;
        }
        let mut add = radial(0.5, 0.5, 0.3, 40.0);
        add.insert("mode".into(), json!("add"));
        let error = plan(
            find("mask.add-radial").unwrap(),
            &recipe,
            &of_mask,
            &add,
            &registry(),
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(
            error.detail,
            format!(
                "mask Mask 1 has {COMPONENTS_PER_MASK} components; the limit is \
                 {COMPONENTS_PER_MASK} components per mask"
            )
        );

        // Masks per recipe: every creating command refuses at the limit, the duplicate included, and
        // the duplicate refuses before it copies a single layer.
        let mut recipe = Recipe::default();
        while recipe.masks.len() < MASKS_PER_RECIPE {
            recipe = apply(
                &recipe,
                "mask.create-linear",
                MaskTarget::default(),
                linear(0.0, 0.0, 0.0, 1.0),
            )
            .unwrap()
            .recipe;
        }
        recipe.layers.push(Layer {
            id: LayerId::new(),
            effect_id: crate::BASIC_EFFECT.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: Some(recipe.masks[0].id.clone()),
            artifacts: Vec::new(),
        });
        let full = format!(
            "recipe already has {MASKS_PER_RECIPE} masks; the limit is {MASKS_PER_RECIPE} masks \
             per recipe"
        );
        for (method, target, parameters) in [
            (
                "mask.create-linear",
                MaskTarget::default(),
                linear(0.5, 0.0, 0.5, 1.0),
            ),
            (
                "mask.create-radial",
                MaskTarget::default(),
                radial(0.5, 0.5, 0.3, 40.0),
            ),
            (
                "mask.duplicate",
                MaskTarget {
                    mask: Some(recipe.masks[0].id.clone()),
                    ..MaskTarget::default()
                },
                Map::new(),
            ),
        ] {
            let error = plan(
                find(method).unwrap(),
                &recipe,
                &target,
                &parameters,
                &registry(),
            )
            .unwrap_err();
            assert_eq!(error.kind, ErrorKind::ResourceLimit, "{method}");
            assert_eq!(error.detail, full, "{method}");
        }
    }

    #[test]
    fn refusals_name_what_they_refuse() {
        let (recipe, _) = created();
        let stranger = Mask::new("Mask 9");
        assert_eq!(
            plan(
                find("mask.set-invert").unwrap(),
                &recipe,
                &MaskTarget {
                    mask: Some(stranger.id.clone()),
                    ..MaskTarget::default()
                },
                &{
                    let mut fields = Map::new();
                    fields.insert("invert".into(), json!(true));
                    fields
                },
                &registry()
            )
            .unwrap_err()
            .detail,
            format!("unknown mask {}", stranger.id)
        );
        let absent = ComponentId::new();
        assert_eq!(
            plan(
                find("mask.set-linear").unwrap(),
                &recipe,
                &MaskTarget {
                    mask: Some(recipe.masks[0].id.clone()),
                    component: Some(absent.clone()),
                    ..MaskTarget::default()
                },
                &Map::new(),
                &registry()
            )
            .unwrap_err()
            .detail,
            format!("mask Mask 1 has no component {absent}")
        );
        // The envelope is checked before anything is planned.
        assert_eq!(
            find("mask.delete")
                .unwrap()
                .checked_target(&MaskTarget::default())
                .unwrap_err()
                .detail,
            "missing required field mask for mask.delete"
        );
        assert_eq!(
            find("mask.create-linear")
                .unwrap()
                .checked_target(&MaskTarget {
                    mask: Some(recipe.masks[0].id.clone()),
                    ..MaskTarget::default()
                })
                .unwrap_err()
                .detail,
            "unknown field mask for mask.create-linear"
        );
    }

    #[test]
    fn a_rename_names_both_names_and_a_duplicate_takes_new_identities() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let of_mask = MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        };
        let renamed = apply(
            &recipe,
            "mask.rename",
            MaskTarget {
                name: Some("Sky".into()),
                ..of_mask.clone()
            },
            Map::new(),
        )
        .unwrap();
        assert_eq!(renamed.label, "Rename Mask 1 to Sky");
        assert_eq!(renamed.recipe.masks[0].name, "Sky");
        // A rename to the name a mask already has changes nothing.
        assert!(matches!(
            plan(
                find("mask.rename").unwrap(),
                &renamed.recipe,
                &MaskTarget {
                    name: Some("Sky".into()),
                    ..of_mask.clone()
                },
                &Map::new(),
                &registry()
            )
            .unwrap(),
            MaskOutcome::NoOp
        ));
        let copied = apply(&renamed.recipe, "mask.duplicate", of_mask, Map::new()).unwrap();
        assert_eq!(copied.label, "Duplicate Sky");
        assert_eq!(copied.recipe.masks.len(), 2);
        let (source, copy) = (&copied.recipe.masks[0], &copied.recipe.masks[1]);
        assert_ne!(source.id, copy.id);
        assert_ne!(source.components[0].id, copy.components[0].id);
        assert_eq!(copy.name, "Mask 1", "the lowest unused default name");
        assert_eq!(copy.components[0].payload, source.components[0].payload);
        assert_eq!(
            copy.components[0].name, "Linear 1",
            "component names are unique within a mask, not across masks"
        );
        assert_eq!(
            copy.next_ordinal, source.next_ordinal,
            "the copy's next linear is Linear 2, because Linear 1 already names one of its own"
        );
    }

    /// A mask without its adjustments is not a useful copy, so `mask.duplicate` copies the layers
    /// bound to the mask as well — each with a new identity, bound to the copy, and placed by the
    /// ordering rule: after the global layer of its effect and in mask order among the masked ones.
    #[test]
    fn a_duplicate_copies_the_layers_bound_to_the_mask() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let mut recipe = recipe;
        let layer = |effect: &str, bound: Option<&MaskId>| Layer {
            id: LayerId::new(),
            effect_id: effect.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: bound.cloned(),
            artifacts: Vec::new(),
        };
        let global = layer(crate::BASIC_EFFECT, None);
        recipe.layers = vec![
            global.clone(),
            layer(crate::BASIC_EFFECT, Some(&mask)),
            layer(crate::PRESENCE_EFFECT, Some(&mask)),
        ];
        let copied = apply(
            &recipe,
            "mask.duplicate",
            MaskTarget {
                mask: Some(mask.clone()),
                ..MaskTarget::default()
            },
            Map::new(),
        )
        .unwrap();
        let copy = copied.mask.clone().expect("the duplicate names its copy");
        assert_eq!(copied.recipe.masks[1].id, copy);
        // Five layers: the global one, and each masked layer beside its copy.
        let targets: Vec<Option<MaskId>> = copied
            .recipe
            .layers
            .iter()
            .map(|layer| layer.mask.clone())
            .collect();
        assert_eq!(
            targets,
            vec![
                None,
                Some(mask.clone()),
                Some(copy.clone()),
                Some(mask.clone()),
                Some(copy.clone()),
            ],
            "each copy follows the layer it was copied from, which is the ordering rule"
        );
        assert_eq!(
            copied.recipe.layers[2].effect_id,
            crate::BASIC_EFFECT,
            "a copy keeps its source's effect"
        );
        assert_eq!(
            copied.recipe.layers[2].payload, copied.recipe.layers[1].payload,
            "a copy keeps its source's payload"
        );
        assert_ne!(
            copied.recipe.layers[2].id, copied.recipe.layers[1].id,
            "a copy takes a new identity"
        );
        // The copies are legal: `single_layer` is per target and the two masks are two targets, so
        // the whole stack compiles rather than failing as ambiguous.
        registry()
            .compile(400, 300, &copied.recipe)
            .expect("two masked layers of one effect on two masks are two targets");
        // And the order the placement rule would produce is the order it is already in.
        let mut sorted = copied.recipe.layers.clone();
        registry().sort_masked_layers(&mut sorted, &copied.recipe.masks);
        assert_eq!(
            sorted, copied.recipe.layers,
            "the copies are placed in the order the one re-sort rule states"
        );
    }

    #[test]
    fn a_deleted_mask_takes_its_layers_and_says_which() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let mut recipe = recipe;
        for effect in [crate::BASIC_EFFECT, crate::PRESENCE_EFFECT] {
            recipe.layers.push(Layer {
                id: LayerId::new(),
                effect_id: effect.to_owned(),
                effect_format: crate::EFFECT_FORMAT,
                payload: json!({}),
                mask: Some(mask.clone()),
                artifacts: Vec::new(),
            });
        }
        let global = Layer {
            id: LayerId::new(),
            effect_id: crate::BASIC_EFFECT.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
            artifacts: Vec::new(),
        };
        recipe.layers.insert(0, global.clone());
        let deleted = apply(
            &recipe,
            "mask.delete",
            MaskTarget {
                mask: Some(mask),
                ..MaskTarget::default()
            },
            Map::new(),
        )
        .unwrap();
        assert_eq!(deleted.label, "Delete Mask 1 with Basic, Presence");
        assert_eq!(deleted.removed_layers.len(), 2);
        assert_eq!(
            deleted
                .removed_layers
                .iter()
                .map(|layer| layer.title.clone().unwrap())
                .collect::<Vec<_>>(),
            ["Basic", "Presence"]
        );
        assert_eq!(
            deleted.recipe.layers,
            vec![global],
            "the global layer of the same effect stays exactly where it was"
        );
        assert!(deleted.recipe.masks.is_empty());
    }

    #[test]
    fn a_mask_move_resorts_the_masked_layers_and_moves_nothing_else() {
        let (recipe, _) = created();
        let first = recipe.masks[0].id.clone();
        let two = apply(
            &recipe,
            "mask.create-linear",
            MaskTarget::default(),
            linear(1.0, 0.0, 1.0, 1.0),
        )
        .unwrap();
        let second = two.recipe.masks[1].id.clone();
        let mut recipe = two.recipe.clone();
        let masked = |effect: &str, mask: &MaskId| Layer {
            id: LayerId::new(),
            effect_id: effect.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: Some(mask.clone()),
            artifacts: Vec::new(),
        };
        let global = Layer {
            id: LayerId::new(),
            effect_id: crate::BASIC_EFFECT.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
            artifacts: Vec::new(),
        };
        recipe.layers = vec![
            global.clone(),
            masked(crate::BASIC_EFFECT, &first),
            masked(crate::BASIC_EFFECT, &second),
            masked(crate::PRESENCE_EFFECT, &first),
            masked(crate::PRESENCE_EFFECT, &second),
        ];
        let before = recipe.layers.clone();
        let mut index = Map::new();
        index.insert("index".into(), json!(0));
        let moved = apply(
            &recipe,
            "mask.reorder",
            MaskTarget {
                mask: Some(second.clone()),
                ..MaskTarget::default()
            },
            index,
        )
        .unwrap();
        assert_eq!(moved.label, "Move Mask 2 to 1");
        assert_eq!(
            moved
                .recipe
                .masks
                .iter()
                .map(|mask| mask.name.as_str())
                .collect::<Vec<_>>(),
            ["Mask 2", "Mask 1"]
        );
        assert_eq!(
            moved.recipe.layers[0], global,
            "an unmasked layer never moves"
        );
        assert_eq!(
            moved.recipe.layers[1], before[2],
            "the masked Basic layers swap, in their masks' new order"
        );
        assert_eq!(moved.recipe.layers[2], before[1]);
        assert_eq!(
            moved.recipe.layers[3], before[4],
            "and so do the masked Presence layers, independently"
        );
        assert_eq!(moved.recipe.layers[4], before[3]);
    }

    #[test]
    fn the_listing_reports_values_components_and_the_bound_layers() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let mut recipe = recipe;
        let layer = Layer {
            id: LayerId::new(),
            effect_id: crate::BASIC_EFFECT.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: Some(mask.clone()),
            artifacts: Vec::new(),
        };
        recipe.layers.push(layer.clone());
        // A kind this build cannot evaluate is listed, kept and reported as unavailable.
        let mut future = Mask::new("Future");
        let name = future.next_component_name("cloud");
        future
            .components
            .push(Component::new(name, ComponentMode::Add, "cloud", json!({})));
        recipe.masks.push(future);
        let entry = crate::EntryId::new();
        let listed = listing(entry.clone(), &recipe, &registry());
        assert_eq!(listed.entry_id, entry);
        assert_eq!(listed.masks.len(), 2);
        let report = &listed.masks[0];
        assert_eq!(report.index, 0);
        assert_eq!(report.name, "Mask 1");
        assert_eq!(report.amount, Mask::FULL_AMOUNT);
        assert!(!report.invert);
        assert_eq!(report.components.len(), 1);
        assert_eq!(report.components[0].name, "Linear 1");
        assert_eq!(report.components[0].kind, "linear");
        assert!(report.components[0].available);
        assert_eq!(
            report.components[0].payload,
            json!({"x0":0.0,"y0":0.0,"x1":0.0,"y1":1.0})
        );
        assert_eq!(report.layers.len(), 1);
        assert_eq!(report.layers[0].id, layer.id);
        assert_eq!(report.layers[0].title.as_deref(), Some("Basic"));
        assert_eq!(listed.masks[1].components[0].name, "Cloud 1");
        assert!(
            !listed.masks[1].components[0].available,
            "a kind this build does not know is named, kept and reported"
        );
        assert!(listed.masks[1].layers.is_empty());
    }

    #[test]
    fn a_geometry_edit_on_a_kind_this_build_cannot_read_is_incompatible() {
        let mut recipe = Recipe::default();
        let mut future = Mask::new("Mask 1");
        let name = future.next_component_name("cloud");
        future
            .components
            .push(Component::new(name, ComponentMode::Add, "cloud", json!({})));
        let (mask, component) = (future.id.clone(), future.components[0].id.clone());
        recipe.masks.push(future);
        let mut patch = Map::new();
        patch.insert("x0".into(), json!(0.5));
        let error = plan(
            find("mask.set-linear").unwrap(),
            &recipe,
            &MaskTarget {
                mask: Some(mask),
                component: Some(component),
                ..MaskTarget::default()
            },
            &patch,
            &registry(),
        )
        .unwrap_err();
        assert_eq!(error.detail, "unknown mask component cloud");
        assert_eq!(error.kind, ErrorKind::Incompatible);
    }

    #[test]
    fn an_index_outside_the_list_it_addresses_is_refused_with_the_count() {
        let (recipe, _) = created();
        let mut index = Map::new();
        index.insert("index".into(), json!(3));
        assert_eq!(
            plan(
                find("mask.reorder").unwrap(),
                &recipe,
                &MaskTarget {
                    mask: Some(recipe.masks[0].id.clone()),
                    ..MaskTarget::default()
                },
                &index,
                &registry()
            )
            .unwrap_err()
            .detail,
            "index 3 is outside the 1 masks of this stack"
        );
    }

    #[test]
    fn the_schema_of_each_command_states_its_envelope_and_its_parameters() {
        let create = find("mask.create-linear").unwrap().schema();
        assert_eq!(create["mutates"], json!(true));
        assert_eq!(
            create["required"],
            json!(["asset_id", "mutation", "x0", "y0", "x1", "y1"]),
            "the kind is in the method name, so it is not a parameter"
        );
        // And a radial's create declares its own fields and none of the linear's, which is the whole
        // point of generating a method per kind.
        assert_eq!(
            find("mask.create-radial").unwrap().schema()["required"],
            json!([
                "asset_id", "mutation", "x", "y", "radius_x", "radius_y", "angle", "feather"
            ])
        );
        assert_eq!(
            find("mask.add-radial").unwrap().schema()["required"],
            json!([
                "asset_id", "mutation", "mask", "mode", "x", "y", "radius_x", "radius_y", "angle",
                "feather"
            ])
        );
        let patch = find("mask.set-linear").unwrap().schema();
        assert_eq!(patch["patch"], json!(true));
        assert_eq!(
            patch["required"],
            json!(["asset_id", "mutation", "mask", "component"])
        );
        assert_eq!(
            patch["optional"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["x0", "x1", "y0", "y1"],
            "every field of a patch is optional, whatever it declares"
        );
        let list = find(LIST).unwrap().schema();
        assert_eq!(list["mutates"], json!(false));
        assert_eq!(list["required"], json!(["asset_id"]));
        assert_eq!(
            list["optional"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["entry_id"]
        );
        let rename = find("mask.rename").unwrap().schema();
        assert_eq!(
            rename["required"],
            json!(["asset_id", "mutation", "mask", "name"])
        );
    }

    #[test]
    fn the_generic_parameter_check_is_the_only_path_a_value_takes() {
        let create = &find("mask.create-linear").unwrap().action;
        // Out of range, wrong type, unknown and missing, all refused by the delivered check.
        assert_eq!(
            crate::check_parameters(create, &json!({"x0":3.0,"y0":0,"x1":0,"y1":1}))
                .unwrap_err()
                .detail,
            "parameter x0 must be a number within -1..=2"
        );
        assert_eq!(
            crate::check_parameters(create, &json!({"mode":"add","x0":0,"y0":0,"x1":0,"y1":1}))
                .unwrap_err()
                .detail,
            "unknown parameter mode for action mask.create-linear",
            "the first component of a mask is always add, so no mode can be requested"
        );
        assert_eq!(
            crate::check_parameters(create, &json!({"x0":0,"y0":0,"x1":0}))
                .unwrap_err()
                .detail,
            "missing required parameter y1 for action mask.create-linear"
        );
        // One kind's field is simply not a parameter of another kind's method, so the closed
        // vocabulary refuses it by name instead of a radius silently passing a position's range.
        assert_eq!(
            crate::check_parameters(create, &json!({"x0":0,"y0":0,"x1":0,"y1":1,"radius_x":0.5}))
                .unwrap_err()
                .detail,
            "unknown parameter radius_x for action mask.create-linear"
        );
        let radial = &find("mask.create-radial").unwrap().action;
        assert_eq!(
            crate::check_parameters(
                radial,
                &json!({"x":0.5,"y":0.5,"radius_x":0.0,"radius_y":0.3,"angle":0,"feather":50})
            )
            .unwrap_err()
            .detail,
            "parameter radius_x must be a number within 0.0001..=64",
            "a radius takes the study's distance range, which a position's range could not express"
        );
        assert_eq!(
            crate::check_parameters(
                radial,
                &json!({"x":0.5,"y":0.5,"radius_x":0.3,"radius_y":0.3,"angle":270,"feather":50})
            )
            .unwrap_err()
            .detail,
            "parameter angle must be a number within -180..=180"
        );
        // A patch fills no defaults and demands nothing.
        let patch = &find("mask.set-linear").unwrap().action;
        assert_eq!(
            crate::check_parameters(patch, &json!({"y1":0.5})).unwrap(),
            {
                let mut fields = Map::new();
                fields.insert("y1".into(), json!(0.5));
                fields
            }
        );
    }
}
