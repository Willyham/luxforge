//! The `mask.*` host command family: what each command declares, what it does to a recipe's mask
//! table, and the history label it commits.
//!
//! Masks are the host's own objects, not a tool module (`docs/design/masking.md#host-commands`): a
//! module commits *layers* through [`crate::ActionPlan`] and must never rewrite the recipe, while
//! every one of these commands rewrites the mask table beside the layers. Everything else about them
//! is a module action's, and deliberately the *same* code rather than a parallel copy of it:
//!
//! - The family is published as one **host descriptor** ([`descriptor`], `luxforge.masks`) in the
//!   same [`crate::ModuleDescriptor`] shape a module's is, listed by `module.list` under `host`: its actions
//!   are the commands, its queries `mask.list` and `mask.sample-input`, its controls the panel's
//!   widgets. An agent discovers a mask command exactly as it discovers a module action.
//! - A command is resolved as [`crate::ActionRef::Host`] by the registry's one action lookup,
//!   checked by the same [`crate::check_parameters`] and planned and committed through the editor's
//!   one action path (`EditorService::run_action`), so the envelope, the deduplication, the
//!   revision, the admission, the single history entry and a draft's equality with its commit are
//!   the delivered ones and not a second implementation.
//! - The mask, component and stroke a command addresses are declared parameters of the identity
//!   kind, and a rename's name one of the string kind, so every one is validated and deduplicated
//!   like any other parameter. [`MaskTarget`] is those four fields as a client or a draft holds
//!   them.
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
//!
//! **Every command is one [`MaskOp`]**, which the command table carries beside its method name and
//! planning matches exhaustively: a kind-independent command is a variant of its own, and a
//! generated one carries the [`GeometryMethod`] or [`SampleMethod`] it was generated as. No command
//! reaches its behaviour by its spelling, and an operation added without a plan arm does not
//! compile.
//!
//! The family is split by concern: `declare` holds what is published (the command table, the
//! controls, the host descriptor and the canvas picks), `plan` what a command does to a stack, and
//! `list` the listing and the result types a client reads.
mod declare;
#[cfg(test)]
mod declare_tests;
mod list;
#[cfg(test)]
mod list_tests;
mod plan;
#[cfg(test)]
mod plan_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use declare::HOST_MODULE;
pub(crate) use declare::descriptor;
pub use declare::{all, canvas, canvas_pick, controls, find, find_query, geometry, sample};
pub use list::{
    ComponentReport, MaskListing, MaskReport, MaskedLayer, StrokeColour, StrokeReport,
    StrokeSettings,
};
pub(crate) use list::{RemovedLayer, listing};
pub(crate) use plan::{
    MaskOutcome, asks_colour_limit, colour_limit_request, input_layer_index, plan,
};

use crate::{ActionDescriptor, ComponentId, Error, MaskId, path::StrokeId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The read-only methods of the family.
pub(crate) const LIST: &str = "mask.list";

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

/// What one declared command does, which is what planning matches on: one variant per
/// kind-independent command, and the generated method a geometry or sample command was generated
/// as. The match is exhaustive, so a command cannot be declared without saying what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskOp {
    /// `mask.delete`: a mask and the layers bound to it.
    Delete,
    /// `mask.rename`: a mask's display name.
    Rename,
    /// `mask.rename-component`: a component's display name.
    RenameComponent,
    /// `mask.duplicate`: a copy of a mask and of the layers bound to it.
    Duplicate,
    /// `mask.set-amount`: the whole-mask amount.
    SetAmount,
    /// `mask.set-invert`: the whole-mask inversion.
    SetInvert,
    /// `mask.reorder`: a mask's position, and with it its masked layers'.
    Reorder,
    /// `mask.set-component-mode`: how a component joins the ones before it.
    SetComponentMode,
    /// `mask.set-component-invert`: one component's own inversion.
    SetComponentInvert,
    /// `mask.delete-component`: one component, never a mask's last.
    DeleteComponent,
    /// `mask.reorder-component`: a component's position inside its mask.
    ReorderComponent,
    /// `mask.add-stroke`: one painted stroke.
    AddStroke,
    /// `mask.delete-stroke`: one stroke removed as a forward edit.
    DeleteStroke,
    /// A generated `mask.create-<kind>`, `mask.add-<kind>` or `mask.set-<kind>`.
    Geometry(GeometryMethod),
    /// A generated `mask.add-<kind>-sample` or `mask.delete-<kind>-sample`.
    Sample(SampleMethod),
}

/// A kind as a label or a title says it: `linear`, `luminance range`. Lower case, because the
/// design's history rows read `Add brush` and `Add subtract brush`.
fn spoken(kind: &str) -> String {
    kind.replace('-', " ")
}

/// The request fields that name what one mask command addresses: the mask, the component inside it,
/// the stroke by its content address, and the display name a rename sets.
///
/// Each is a declared parameter of the commands that take it — the three identities of the
/// `crate::IdentityKind` kind and the name of the string kind — so the generic check validates
/// them and the request identity hashes them like any other parameter. This is those fields as a
/// client holds them when it builds a request, and as a draft holds the objects its gesture edits
/// ([`crate::Draft::target`]): the handle drag edits *that* component, whichever fields it drafts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The stroke `mask.delete-stroke` removes, by its content address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<StrokeId>,
}

/// The parameter names a [`MaskTarget`] fills, in the order a command declares them.
pub(crate) const TARGET_FIELDS: [&str; 4] = [MASK, COMPONENT, STROKE, NAME];
const MASK: &str = "mask";
const COMPONENT: &str = "component";
const STROKE: &str = "stroke";
const NAME: &str = "name";

impl MaskTarget {
    /// Write the fields this target holds into a request's parameters, as the declared parameters
    /// they are. A field the target does not hold is left as the parameters have it.
    pub fn insert_into(&self, parameters: &mut Map<String, Value>) {
        if let Some(mask) = &self.mask {
            parameters.insert(MASK.to_owned(), json!(mask.as_str()));
        }
        if let Some(component) = &self.component {
            parameters.insert(COMPONENT.to_owned(), json!(component.as_str()));
        }
        if let Some(stroke) = &self.stroke {
            parameters.insert(STROKE.to_owned(), json!(stroke.as_str()));
        }
        if let Some(name) = &self.name {
            parameters.insert(NAME.to_owned(), json!(name));
        }
    }

    /// The identities this target holds as a draft's target ([`crate::DraftTarget`]): the mask,
    /// component and stroke it names, by the parameter names a command declares them under. A
    /// rename's `name` is a value and not an identity, so it is never part of one.
    pub fn identities(&self) -> crate::DraftTarget {
        [
            (MASK, self.mask.as_ref().map(MaskId::as_str)),
            (COMPONENT, self.component.as_ref().map(ComponentId::as_str)),
            (STROKE, self.stroke.as_ref().map(StrokeId::as_str)),
        ]
        .into_iter()
        .filter_map(|(name, identity)| {
            identity.map(|identity| (name.to_owned(), identity.to_owned()))
        })
        .collect()
    }

    /// A request's parameters with this target's fields added: what a client sends.
    pub fn request(&self, parameters: Value) -> Value {
        let mut parameters = match parameters {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        self.insert_into(&mut parameters);
        Value::Object(parameters)
    }

    /// Checked parameters split into the objects they address and the values they set. The generic
    /// check has already validated each identity's syntax, so a parse here cannot refuse one it
    /// accepted.
    pub(crate) fn split(
        parameters: &Map<String, Value>,
    ) -> Result<(Self, Map<String, Value>), Error> {
        let text = |name: &str| parameters.get(name).and_then(Value::as_str);
        let target = Self {
            mask: text(MASK).map(MaskId::parse).transpose()?,
            component: text(COMPONENT).map(ComponentId::parse).transpose()?,
            name: text(NAME).map(str::to_owned),
            stroke: text(STROKE).map(StrokeId::parse).transpose()?,
        };
        let values = parameters
            .iter()
            .filter(|(name, _)| !TARGET_FIELDS.contains(&name.as_str()))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        Ok((target, values))
    }
}

/// One declared `mask.*` command: an action of the host descriptor.
#[derive(Debug)]
pub struct MaskCommand {
    /// The method name, which is also the durable action identity a history entry stores. It carries
    /// a dot, which `crate::valid_name` forbids inside an action identity, so a module action and a
    /// mask command cannot collide however either grows; [`crate::ModuleRegistry::register`] checks
    /// that rather than assuming it.
    pub method: &'static str,
    /// What the command does. A generated geometry method's says which kind's parameters it
    /// declares and which kind's components it may touch, and a generated sample method's which
    /// kind's sampled colours it edits.
    pub op: MaskOp,
    /// The action as the host descriptor lists it: the identities it addresses first, then the
    /// values it sets.
    pub action: ActionDescriptor,
}
