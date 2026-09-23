use crate::{
    Error, ErrorKind,
    modules::{CropPayload, valid_name},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

/// Format 2 adds the mask table a recipe carries and the optional mask reference a layer carries;
/// a recipe written before it has no `masks` field and is refused by [`Recipe::validate`] and by
/// deserialization rather than defaulted, because a stack whose masks are unknown is not the stack
/// that was stored.
pub const RECIPE_FORMAT: u32 = 2;
pub const PIXEL_EFFECT: &str = "lightwell.pixel.replace";
pub const RAW_EFFECT: &str = "lightwell.raw";
pub const ORIENTATION_EFFECT: &str = "lightwell.geometry.orientation";
pub const CROP_EFFECT: &str = "lightwell.geometry.crop";
/// The one colour-stage effect of the Basic module: every implemented Basic parameter of a stack
/// lives in one layer of this effect.
pub const BASIC_EFFECT: &str = "lightwell.basic.adjust";
/// The colour mixer's one pointwise unit: hue, saturation and luminance for the eight colour
/// ranges, declared order 10 so a mixer layer always follows the Basic layer in the colour run.
pub const MIXER_EFFECT: &str = "lightwell.mixer.hsl";

/// The one spatial-stage effect of the Presence module: Texture, Clarity and Dehaze of a stack live
/// in one layer of this effect, evaluated after the pointwise colour run and before the geometry
/// tail as one tiled neighbourhood pass.
pub const PRESENCE_EFFECT: &str = "lightwell.presence.adjust";

/// The one finish-stage effect of the Vignette module: every implemented Vignette parameter of a
/// stack lives in one layer of this effect, evaluated after the geometry tail in output-stage
/// pixel coordinates. Named `postcrop` rather than `post-crop`: `valid_identity` forbids a hyphen
/// inside a dot-separated identity segment (every other built-in effect follows the same rule,
/// e.g. `lightwell.basic.adjust`), so the closest one-word form of the design's "post-crop
/// vignette" name is used instead of a literal hyphen.
pub const VIGNETTE_EFFECT: &str = "lightwell.vignette.postcrop";
pub const EFFECT_FORMAT: u32 = 1;

fn valid_id(value: &str, prefix: &str) -> bool {
    value.len() > prefix.len() + 8
        && value.len() <= 96
        && value.starts_with(prefix)
        && value[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

macro_rules! identifier {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);
        impl $name {
            pub fn new() -> Self {
                Self(format!(
                    concat!($prefix, "{}"),
                    uuid::Uuid::new_v4().simple()
                ))
            }
            pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
                let value = value.into();
                if valid_id(&value, $prefix) {
                    Ok(Self(value))
                } else {
                    Err(Error::new(
                        ErrorKind::Validation,
                        concat!("invalid ", stringify!($name)),
                    ))
                }
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(de::Error::custom)
            }
        }
    };
}

identifier!(AssetId, "asset-");
identifier!(LayerId, "layer-");
identifier!(SnapshotId, "snapshot-");
identifier!(EntryId, "entry-");
identifier!(DraftId, "draft-");
identifier!(JobId, "job-");
identifier!(PresetId, "preset-");
identifier!(MaskId, "mask-");
identifier!(ComponentId, "component-");

/// Masks per recipe, components per mask and the serialized size of one recipe's mask table: the
/// structural part of the declared masking limits, each refused with a `ResourceLimit` error that
/// names the limit. Every history entry stores a complete stack, so the byte bound is what keeps a
/// long session's snapshots bounded rather than growing with every stroke.
pub const MASKS_PER_RECIPE: usize = 16;
pub const COMPONENTS_PER_MASK: usize = 32;
pub const MASK_BYTES_PER_RECIPE: usize = 256 * 1024;
/// Stored path positions one mask's components may hold between them, summed over every stroke they
/// reference. The per-stroke bound is [`crate::path::POINTS_PER_STROKE`] and belongs to the host's
/// path primitives; this one is the mask's own and is refused with a `ResourceLimit` error naming
/// it. It is checked wherever the referenced strokes are resolved, which is every path that draws
/// the mask.
pub const POINTS_PER_MASK: usize = 8192;
/// Mask and component display names are a person's text, not an identity: printable, trimmed and
/// bounded, exactly as a version name is.
pub const MAX_MASK_NAME: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    pub id: LayerId,
    pub effect_id: String,
    pub effect_format: u32,
    pub payload: Value,
    /// The mask this layer is modulated by, or `None` for a layer that applies everywhere. It is
    /// always written, so a stored stack says for every layer whether it was committed through a
    /// selection. A mask is stored in content-stage coordinates, so only a layer before the
    /// geometry tail may carry one; [`crate::ModuleRegistry`] owns that rule, because the stage
    /// belongs to the effect's provider and not to the recipe.
    pub mask: Option<MaskId>,
}

impl Layer {
    pub fn pixel(x: u32, y: u32, rgb: [u8; 3]) -> Self {
        Self {
            id: LayerId::new(),
            effect_id: PIXEL_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"x": x, "y": y, "rgb": rgb}),
            mask: None,
        }
    }
    /// The one orientation layer of a stage: the composed quarter turns and reflections that every
    /// exact transform action applied there reaches.
    pub fn orientation(orientation: Orientation) -> Self {
        Self {
            id: LayerId::new(),
            effect_id: ORIENTATION_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: serde_json::to_value(orientation).expect("orientation is serializable"),
            mask: None,
        }
    }
    /// The one crop layer of a stack: straightening and a rectangle over its own input stage.
    pub fn crop(payload: CropPayload) -> Self {
        Self {
            id: LayerId::new(),
            effect_id: CROP_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: serde_json::to_value(payload).expect("crop payload is serializable"),
            mask: None,
        }
    }
    /// Structural only: effect availability and payload shape belong to the providing module,
    /// reached through [`crate::ModuleRegistry`].
    pub fn validate(&self) -> Result<(), Error> {
        if self.effect_id.is_empty() {
            return Err(Error::new(
                ErrorKind::Validation,
                "layer has no effect identity",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelReplace {
    pub x: u32,
    pub y: u32,
    pub rgb: [u8; 3],
}

/// The action vocabulary of the transform module: what a person or a client asks for. The stack
/// stores the resulting [`Orientation`], not the gestures that reached it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transform {
    RotateLeft,
    RotateRight,
    MirrorHorizontal,
    FlipVertical,
}

impl Transform {
    pub fn action_id(self) -> &'static str {
        match self {
            Self::RotateLeft => "rotate-left",
            Self::RotateRight => "rotate-right",
            Self::MirrorHorizontal => "mirror-horizontal",
            Self::FlipVertical => "flip-vertical",
        }
    }
}

/// The composed exact orientation one layer holds: mirror horizontally when `mirror` is set, then
/// rotate clockwise by `turns` quarter turns. Each of the eight exact orientations is exactly one
/// of these payloads, so any number of [`Transform`] actions applied to one stage stay one layer.
/// [`Orientation::NEUTRAL`] maps every pixel to itself and leaves the stage unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Orientation {
    pub mirror: bool,
    pub turns: u8,
}

impl Orientation {
    /// The identity mapping: no reflection and no quarter turn.
    pub const NEUTRAL: Self = Self {
        mirror: false,
        turns: 0,
    };
}

impl Default for Orientation {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// How one component joins the coverage the components before it composed. The first component of
/// a mask is always [`ComponentMode::Add`], because there is nothing yet to subtract from or
/// intersect with. Serialized kebab-case like [`Transform`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentMode {
    Add,
    Subtract,
    Intersect,
}

impl ComponentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Intersect => "intersect",
        }
    }
}

/// One selection a mask composes: the geometry a person drew, its role in the composition and its
/// own inversion.
///
/// `kind` and `payload` are to a component what `effect_id` and `payload` are to a [`Layer`]: the
/// host stores them and the provider of that kind reads them, so a component whose kind this build
/// does not know is retained byte for byte instead of being dropped or rewritten. This model
/// validates the pair structurally only — a well-formed kind token and a payload it never
/// inspects; which kinds exist, and what each payload means, belongs to the kind's provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub id: ComponentId,
    /// The display name a history row names, such as `Brush 1`: renameable, never an identity.
    pub name: String,
    pub mode: ComponentMode,
    pub invert: bool,
    pub kind: String,
    pub payload: Value,
}

impl Component {
    /// A component of `kind` carrying the payload its provider will read. The name comes from the
    /// mask, through [`Mask::next_component_name`], because the ordinal it spends belongs there.
    pub fn new(
        name: impl Into<String>,
        mode: ComponentMode,
        kind: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self {
            id: ComponentId::new(),
            name: name.into(),
            mode,
            invert: false,
            kind: kind.into(),
            payload,
        }
    }

    /// Structural only: the identity is checked by its own type, the kind is a well-formed token
    /// and the payload is the kind provider's business, unread here.
    pub fn validate(&self) -> Result<(), Error> {
        valid_display_name("component", &self.name)?;
        if !valid_name(&self.kind) {
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "component {} has an invalid kind {:?}",
                    self.name, self.kind
                ),
            ));
        }
        Ok(())
    }
}

/// An editable selection in the recipe: an ordered list of components, a whole-mask amount and a
/// whole-mask inversion. A mask is a host object beside the layers, not a layer and not a module's
/// state, so every history entry's complete snapshot already carries it and undo, redo, preview,
/// Restore and versions need no new machinery.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mask {
    pub id: MaskId,
    /// The display name the masks list and the history labels show, such as `Mask 1`.
    pub name: String,
    /// 0..=100, multiplying the composed coverage.
    pub amount: f64,
    /// Inverts the composed coverage before `amount`.
    pub invert: bool,
    /// The next ordinal to spend on a component display name, per component kind, so an ordinal is
    /// never reused: deleting `Brush 1` and adding another brush yields `Brush 2`. That is what
    /// makes a stored history label honest — an entry that says `Update Brush 1` can only ever mean
    /// the one component it was written about. A [`BTreeMap`] keeps the serialized object's keys in
    /// one order, so a mask that did not change serializes to the same bytes.
    pub next_ordinal: BTreeMap<String, u32>,
    pub components: Vec<Component>,
}

/// Equality on the stored values, with `amount` compared by its bits. A mask therefore stays
/// [`Eq`], and so do the recipe, snapshot and history entry that carry one: comparing two stored
/// recipes asks whether they hold the same numbers, which is reflexive for every `f64` pattern,
/// unlike `==` on a float.
impl PartialEq for Mask {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.name == other.name
            && self.amount.to_bits() == other.amount.to_bits()
            && self.invert == other.invert
            && self.next_ordinal == other.next_ordinal
            && self.components == other.components
    }
}

impl Eq for Mask {}

impl Mask {
    /// The default whole-mask amount: the composed coverage, unattenuated.
    pub const FULL_AMOUNT: f64 = 100.0;

    /// A mask with no components yet, named by the caller. The components a gesture or a request
    /// adds carry their own ordinals from [`Self::next_component_name`].
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: MaskId::new(),
            name: name.into(),
            amount: Self::FULL_AMOUNT,
            invert: false,
            next_ordinal: BTreeMap::new(),
            components: Vec::new(),
        }
    }

    /// The display name the next component of `kind` gets, spending this mask's ordinal for that
    /// kind: `brush` reads as `Brush 1`, then `Brush 2`, whatever was deleted in between. The
    /// counter advances even when the caller does not go on to add the component, because a spent
    /// ordinal must never name a second component.
    pub fn next_component_name(&mut self, kind: &str) -> String {
        let ordinal = self.next_ordinal.entry(kind.to_string()).or_insert(0);
        *ordinal = ordinal.saturating_add(1);
        format!("{} {}", crate::modules::title_case(kind), ordinal)
    }

    /// Structural only: identities, display names, the composition's first mode, the amount range
    /// and the component-count limit. Coverage mathematics and payload shapes belong elsewhere.
    pub fn validate(&self) -> Result<(), Error> {
        valid_display_name("mask", &self.name)?;
        if !self.amount.is_finite() || !(0.0..=Self::FULL_AMOUNT).contains(&self.amount) {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("mask {} amount must be a number within 0..=100", self.name),
            ));
        }
        if self.components.len() > COMPONENTS_PER_MASK {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "mask {} has {} components; the limit is {COMPONENTS_PER_MASK} components per mask",
                    self.name,
                    self.components.len()
                ),
            ));
        }
        // Nothing precedes the first component, so it can only add to an empty coverage. A stored
        // mask that begins by subtracting or intersecting is refused as it stands, with the mode
        // named, rather than read as if its first component had been an add.
        if let Some(first) = self.components.first()
            && first.mode != ComponentMode::Add
        {
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "mask {} begins with a {} component; the first component of a mask is always add",
                    self.name,
                    first.mode.as_str()
                ),
            ));
        }
        let mut ids = HashSet::with_capacity(self.components.len());
        let mut names = HashSet::with_capacity(self.components.len());
        for component in &self.components {
            component.validate()?;
            if !ids.insert(&component.id) {
                return Err(Error::new(
                    ErrorKind::Validation,
                    format!("duplicate component identity in mask {}", self.name),
                ));
            }
            if !names.insert(&component.name) {
                return Err(Error::new(
                    ErrorKind::Validation,
                    format!(
                        "duplicate component name {} in mask {}",
                        component.name, self.name
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// A person's text for a mask or a component, held to the same shape a version name is: trimmed,
/// printable and bounded, so a display name cannot carry control characters into a history label or
/// grow a snapshot.
fn valid_display_name(what: &str, name: &str) -> Result<(), Error> {
    if name.trim() != name
        || name.is_empty()
        || name.chars().count() > MAX_MASK_NAME
        || name.chars().any(char::is_control)
    {
        return Err(Error::new(
            ErrorKind::Validation,
            format!("{what} name must contain 1..={MAX_MASK_NAME} printable characters"),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub format: u32,
    pub layers: Vec<Layer>,
    /// The masks the layers of this recipe may reference, in the order the masks list shows.
    pub masks: Vec<Mask>,
    /// The strokes this recipe's payloads reference, resolved from the content-addressed store
    /// ([`crate::path`]), and the references that could not be resolved.
    ///
    /// **Never serialized.** A stored recipe holds stroke *addresses* in its payloads and no
    /// positions at all, so "no catalog ever holds embedded stroke points" is a property of this
    /// type and not of the code that happens to write it. It is filled in when a recipe is read out
    /// of a store that has the strokes, and it is empty on a recipe that references none — which is
    /// every recipe without a painted edit.
    #[serde(skip)]
    pub strokes: crate::path::StrokeTable,
}

/// Equality on the stored recipe, which is what two recipes being the same means: the format, the
/// layers and the mask table. The resolved stroke table is excluded because it is not stored — it
/// is what the addresses in those payloads resolved to, so two recipes that compare equal reference
/// the same strokes by construction, and a hydrated recipe must not compare unequal to the recipe
/// it was hydrated from.
impl PartialEq for Recipe {
    fn eq(&self, other: &Self) -> bool {
        self.format == other.format && self.layers == other.layers && self.masks == other.masks
    }
}

impl Eq for Recipe {}

impl Default for Recipe {
    fn default() -> Self {
        Self {
            format: RECIPE_FORMAT,
            layers: Vec::new(),
            masks: Vec::new(),
            strokes: crate::path::StrokeTable::default(),
        }
    }
}

impl Recipe {
    /// Structural validation of a whole stack: `O(layers + components)` identity checks plus one
    /// serialization of the mask table when the recipe has masks at all. It reads no pixels and asks
    /// no provider anything, so every evaluation path can afford it — [`crate::ModuleRegistry`]
    /// compiles through it, which is how a stack that names a mask it does not carry fails
    /// compiling, rendering, sampling and planning alike instead of quietly rendering unmasked.
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != RECIPE_FORMAT {
            return Err(Error::new(
                ErrorKind::Incompatible,
                format!("unsupported recipe format {}", self.format),
            ));
        }
        self.validate_masks()?;
        let mut ids = HashSet::with_capacity(self.layers.len());
        for layer in &self.layers {
            if !ids.insert(&layer.id) {
                return Err(Error::new(
                    ErrorKind::Validation,
                    "duplicate layer identity",
                ));
            }
            layer.validate()?;
            // A layer's mask must be in the same snapshot. A missing one is incompatible data, not
            // a layer that applies everywhere: the effect was committed through a selection, so the
            // host refuses the stack and names both rather than rendering something else.
            if let Some(mask) = &layer.mask
                && !self.masks.iter().any(|candidate| candidate.id == *mask)
            {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    format!(
                        "layer {} references mask {mask}, which this recipe does not carry",
                        layer.id
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Every stroke address this recipe's payloads carry, in stored order: the layers first, then
    /// the components of each mask, with the thing that carries each list named for a refusal.
    ///
    /// The host reads exactly [`crate::path::STROKES_FIELD`] of each payload and parses nothing
    /// else, because a payload belongs to the effect or component kind that provides it. Cost is
    /// `O(layers + components)` and no store is touched: this answers what a recipe references, not
    /// what it resolves to.
    pub fn stroke_references(&self) -> Result<Vec<(String, crate::path::StrokeId)>, Error> {
        let mut found = Vec::new();
        for layer in &self.layers {
            let what = format!("layer {}", layer.id);
            for id in crate::path::references(&layer.payload, &what)? {
                found.push((what.clone(), id));
            }
        }
        for mask in &self.masks {
            for component in &mask.components {
                let what = format!("component {} of mask {}", component.name, mask.name);
                for id in crate::path::references(&component.payload, &what)? {
                    found.push((what.clone(), id));
                }
            }
        }
        Ok(found)
    }

    /// Resolve every stroke this recipe references against the table it was hydrated with, and
    /// enforce the per-mask point bound while the strokes are in hand.
    ///
    /// This is what makes a missing or corrupt stroke refuse by name instead of drawing something
    /// else: it runs where a recipe is compiled, so rendering, sampling, proxy planning, module
    /// planning and the export that will compile the same way all pass through it, and it returns
    /// the store's own refusal unchanged. It reads the recipe and rewrites nothing, so listing, undoing and carrying the
    /// stack forward keep working exactly as they do for an unavailable effect.
    ///
    /// A recipe that references no stroke — every recipe without a painted edit — costs one walk of
    /// its payloads and touches nothing else.
    pub fn resolve_strokes(&self) -> Result<(), Error> {
        let references = self.stroke_references()?;
        if references.is_empty() {
            return Ok(());
        }
        for (what, id) in &references {
            self.strokes.resolve(id).map_err(|error| {
                Error::new(error.kind, format!("{} referenced by {what}", error.detail))
            })?;
        }
        for mask in &self.masks {
            let mut points = 0_usize;
            for component in &mask.components {
                let what = format!("component {} of mask {}", component.name, mask.name);
                for id in crate::path::references(&component.payload, &what)? {
                    points += self
                        .strokes
                        .get(&id)
                        .map_or(0, crate::path::Stroke::point_count);
                }
            }
            if points > POINTS_PER_MASK {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "mask {} holds {points} stored path positions; the limit is \
                         {POINTS_PER_MASK} points per mask",
                        mask.name
                    ),
                ));
            }
        }
        Ok(())
    }

    /// The mask table on its own: the per-recipe limits, unique identities and each mask's own
    /// structure. The serialized-bytes bound is measured only when there are masks, so an unmasked
    /// recipe — every recipe without a local adjustment — pays nothing for it.
    fn validate_masks(&self) -> Result<(), Error> {
        if self.masks.is_empty() {
            return Ok(());
        }
        if self.masks.len() > MASKS_PER_RECIPE {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "recipe has {} masks; the limit is {MASKS_PER_RECIPE} masks per recipe",
                    self.masks.len()
                ),
            ));
        }
        let mut ids = HashSet::with_capacity(self.masks.len());
        for mask in &self.masks {
            mask.validate()?;
            if !ids.insert(&mask.id) {
                return Err(Error::new(ErrorKind::Validation, "duplicate mask identity"));
            }
        }
        // Every history entry stores a complete stack, so the mask table's size is multiplied by
        // the number of entries a session writes. The bound fails explicitly instead of letting a
        // brush session grow the catalog without a stated limit.
        let bytes = serde_json::to_vec(&self.masks)
            .map_err(|e| Error::new(ErrorKind::Internal, format!("cannot measure masks: {e}")))?
            .len();
        if bytes > MASK_BYTES_PER_RECIPE {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "recipe masks serialize to {bytes} bytes; the limit is {MASK_BYTES_PER_RECIPE} serialized mask bytes per recipe"
                ),
            ));
        }
        Ok(())
    }
    pub fn appended(&self, layer: Layer) -> Result<Self, Error> {
        self.with_layer_inserted(self.layers.len(), layer)
    }
    /// Insert a layer at `index`, keeping every other layer and its order; `layers.len()` appends.
    /// The host chooses the index from the effect's declared stage, so a pixel-stage layer joins
    /// the stack before the geometry tail that must carry it. An index past the end is a
    /// validation error.
    pub fn with_layer_inserted(&self, index: usize, layer: Layer) -> Result<Self, Error> {
        layer.validate()?;
        if index > self.layers.len() {
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "layer index {index} is outside the {} layers of the stack",
                    self.layers.len()
                ),
            ));
        }
        let mut next = self.clone();
        next.layers.insert(index, layer);
        next.validate()?;
        Ok(next)
    }
    /// Replace the layer with the same identity in place, keeping every other layer and every
    /// position. An identity that is not in this recipe is a validation error.
    pub fn with_layer_replaced(&self, layer: Layer) -> Result<Self, Error> {
        layer.validate()?;
        let position = self
            .layers
            .iter()
            .position(|existing| existing.id == layer.id)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Validation,
                    "plan updates a layer that is not in the stack",
                )
            })?;
        let mut next = self.clone();
        next.layers[position] = layer;
        next.validate()?;
        Ok(next)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub id: SnapshotId,
    pub asset_id: AssetId,
    pub recipe: Recipe,
}

impl Snapshot {
    pub fn original(asset_id: AssetId) -> Self {
        Self {
            id: SnapshotId::new(),
            asset_id,
            recipe: Recipe::default(),
        }
    }
    pub fn append(&self, layer: Layer) -> Result<Self, Error> {
        self.with_layer_inserted(self.recipe.layers.len(), layer)
    }
    /// A new snapshot whose stack carries this layer at `index`. Earlier snapshots keep their own
    /// recipe, so history stays immutable whatever the position.
    pub fn with_layer_inserted(&self, index: usize, layer: Layer) -> Result<Self, Error> {
        Ok(Self {
            id: SnapshotId::new(),
            asset_id: self.asset_id.clone(),
            recipe: self.recipe.with_layer_inserted(index, layer)?,
        })
    }
    /// A new snapshot whose stack differs only in the layer with this identity. Earlier snapshots
    /// keep their own recipe, so history stays immutable.
    pub fn with_layer_replaced(&self, layer: Layer) -> Result<Self, Error> {
        Ok(Self {
            id: SnapshotId::new(),
            asset_id: self.asset_id.clone(),
            recipe: self.recipe.with_layer_replaced(layer)?,
        })
    }
    /// A new snapshot of the same asset holding this stack, which the host resolved from this one:
    /// the result of one plan or of every step of a composite. Earlier snapshots keep their own
    /// recipe, so history stays immutable.
    pub fn with_recipe(&self, recipe: Recipe) -> Result<Self, Error> {
        recipe.validate()?;
        Ok(Self {
            id: SnapshotId::new(),
            asset_id: self.asset_id.clone(),
            recipe,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    pub id: EntryId,
    pub asset_id: AssetId,
    pub sequence: u64,
    pub action_id: String,
    /// The one-line label history rows show, rendered by the host when the entry was committed
    /// from the requested action's summary template, or its title. Stored with the entry so a row
    /// reads the same however the providers change.
    pub label: String,
    pub parameters: Value,
    pub actor: String,
    pub timestamp_ms: i64,
    pub request_id: Option<String>,
    pub base_revision: u64,
    pub result_revision: u64,
    pub snapshot: Snapshot,
    pub undo_parent: Option<EntryId>,
    pub restore_target: Option<EntryId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mutation {
    pub expected_revision: u64,
    pub request_id: String,
    pub actor: String,
}

impl Mutation {
    pub fn validate(&self) -> Result<(), Error> {
        if self.request_id.is_empty() || self.request_id.len() > 128 {
            return Err(Error::new(
                ErrorKind::Validation,
                "request_id must contain 1..128 characters",
            ));
        }
        if self.actor.is_empty() || self.actor.len() > 128 {
            return Err(Error::new(
                ErrorKind::Validation,
                "actor must contain 1..128 characters",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mask with one component of a kind this build knows nothing about, which is the case the
    /// model has to carry: a well-formed kind token and a payload nothing here reads.
    fn future_mask() -> Mask {
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name("future-kind");
        mask.components.push(Component::new(
            name,
            ComponentMode::Add,
            "future-kind",
            json!({"nested": {"points": [[0.25, 0.5], [0.75, 0.5]]}, "flag": true, "n": 3.5}),
        ));
        mask
    }

    fn masked_layer(mask: &Mask) -> Layer {
        Layer {
            mask: Some(mask.id.clone()),
            ..Layer::pixel(0, 0, [1, 2, 3])
        }
    }

    #[test]
    fn masks_ride_in_the_snapshot_and_an_unknown_component_kind_survives_byte_for_byte() {
        let mask = future_mask();
        let mut snapshot = Snapshot::original(AssetId::new());
        snapshot.recipe.masks.push(mask.clone());
        snapshot.recipe.layers.push(masked_layer(&mask));
        snapshot.recipe.validate().unwrap();
        let encoded = serde_json::to_vec(&snapshot).unwrap();
        let reopened: Snapshot = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(reopened, snapshot, "the whole snapshot round-trips");
        // Retention is about the bytes, not about equality of a type this build understands: the
        // stored kind and payload come back exactly as they were written, unparsed and unrewritten.
        let stored = &reopened.recipe.masks[0].components[0];
        assert_eq!(stored.kind, "future-kind");
        assert_eq!(
            serde_json::to_string(&stored.payload).unwrap(),
            serde_json::to_string(&mask.components[0].payload).unwrap()
        );
        assert_eq!(
            reopened.recipe.layers[0].mask.as_ref(),
            Some(&mask.id),
            "the layer still names the mask it was committed through"
        );
        // Nothing in validation asks what a kind means, so an unknown one is not a reason to refuse
        // a stack; the provider table that parses payloads reports an unknown kind on its own.
        assert!(reopened.recipe.validate().is_ok());
    }

    #[test]
    fn a_recipe_stored_without_a_mask_table_is_refused_rather_than_defaulted() {
        let current = serde_json::to_value(Recipe::default()).unwrap();
        assert_eq!(current["masks"], json!([]), "masks are always written");
        let older = json!({"format": RECIPE_FORMAT, "layers": []});
        assert!(
            serde_json::from_value::<Recipe>(older).is_err(),
            "a recipe shape without masks fails explicitly instead of becoming an unmasked one"
        );
        // A layer always writes its mask reference, so a stored stack states for every layer
        // whether it was committed through a selection. The recipe is the unit whose shape is
        // marked, and the required table above is what refuses the older one.
        let layer = serde_json::to_value(Layer::pixel(0, 0, [1, 2, 3])).unwrap();
        assert_eq!(layer["mask"], Value::Null);
    }

    #[test]
    fn a_layer_may_only_name_a_mask_its_own_recipe_carries() {
        let mask = future_mask();
        let layer = masked_layer(&mask);
        let dangling = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![layer.clone()],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let error = dangling.validate().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert!(
            error.detail.contains(layer.id.as_str()) && error.detail.contains(mask.id.as_str()),
            "the error names both the layer and the mask: {}",
            error.detail
        );
        assert_eq!(
            dangling.layers.len(),
            1,
            "the refused stack is left as it stands"
        );
        assert!(
            Recipe {
                masks: vec![mask],
                ..dangling
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn mask_structure_is_validated_and_names_what_it_refuses() {
        let mut mask = future_mask();
        let subtract = Component {
            mode: ComponentMode::Subtract,
            ..mask.components[0].clone()
        };
        let first_subtracts = Mask {
            components: vec![subtract.clone()],
            ..mask.clone()
        };
        let error = first_subtracts.validate().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            "mask Mask 1 begins with a subtract component; the first component of a mask is always \
             add"
        );
        // A second component in any mode is ordinary: only the first is constrained.
        let second = Component::new("Radial 1", ComponentMode::Intersect, "radial", json!({}));
        mask.components.push(second.clone());
        mask.validate().unwrap();

        let duplicate_identity = Mask {
            components: vec![mask.components[0].clone(), mask.components[0].clone()],
            ..mask.clone()
        };
        assert_eq!(
            duplicate_identity.validate().unwrap_err().detail,
            "duplicate component identity in mask Mask 1"
        );
        let duplicate_name = Mask {
            components: vec![
                mask.components[0].clone(),
                Component {
                    name: mask.components[0].name.clone(),
                    ..second
                },
            ],
            ..mask.clone()
        };
        assert_eq!(
            duplicate_name.validate().unwrap_err().detail,
            "duplicate component name Future kind 1 in mask Mask 1"
        );
        for amount in [-0.5, 100.5, f64::NAN, f64::INFINITY] {
            let error = Mask {
                amount,
                ..mask.clone()
            }
            .validate()
            .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "amount {amount}");
            assert_eq!(
                error.detail,
                "mask Mask 1 amount must be a number within 0..=100"
            );
        }
        for edge in [0.0, Mask::FULL_AMOUNT] {
            assert!(
                Mask {
                    amount: edge,
                    ..mask.clone()
                }
                .validate()
                .is_ok()
            );
        }
        for name in ["", " Mask 1", "Mask\n1", &"M".repeat(MAX_MASK_NAME + 1)] {
            assert_eq!(
                Mask {
                    name: name.into(),
                    ..mask.clone()
                }
                .validate()
                .unwrap_err()
                .detail,
                format!("mask name must contain 1..={MAX_MASK_NAME} printable characters")
            );
        }
        // A kind is an identity token, whether or not this build knows it; a display name is not.
        for kind in ["", "Brush", "future kind"] {
            let refused = Mask {
                components: vec![Component::new(
                    "Brush 1",
                    ComponentMode::Add,
                    kind,
                    json!({}),
                )],
                ..mask.clone()
            };
            assert_eq!(
                refused.validate().unwrap_err().detail,
                format!("component Brush 1 has an invalid kind {kind:?}")
            );
        }
    }

    #[test]
    fn a_component_ordinal_is_spent_once_and_never_reused() {
        let mut mask = Mask::new("Mask 1");
        assert_eq!(mask.next_component_name("brush"), "Brush 1");
        assert_eq!(mask.next_component_name("radial"), "Radial 1");
        assert_eq!(mask.next_component_name("brush"), "Brush 2");
        assert_eq!(
            mask.next_component_name("luminance-range"),
            "Luminance range 1"
        );
        // Deleting a component does not return its ordinal: a history row that says Brush 1 can
        // only ever have meant the component it was written about.
        mask.components.push(Component::new(
            "Brush 3",
            ComponentMode::Add,
            "brush",
            json!({}),
        ));
        mask.components.clear();
        assert_eq!(mask.next_component_name("brush"), "Brush 3");
        assert_eq!(mask.next_ordinal["brush"], 3);
        assert_eq!(mask.next_ordinal["radial"], 1);
    }

    #[test]
    fn the_declared_mask_limits_are_refused_by_the_limit_they_name() {
        let mask = future_mask();
        let mut recipe = Recipe {
            masks: (0..MASKS_PER_RECIPE)
                .map(|index| Mask {
                    name: format!("Mask {index}"),
                    ..Mask::new("Mask")
                })
                .collect(),
            ..Recipe::default()
        };
        recipe.validate().unwrap();
        recipe.masks.push(Mask::new("One too many"));
        let error = recipe.validate().unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(
            error.detail,
            format!(
                "recipe has {} masks; the limit is {MASKS_PER_RECIPE} masks per recipe",
                MASKS_PER_RECIPE + 1
            )
        );

        let components = |count: usize| {
            (0..count)
                .map(|index| {
                    Component::new(
                        format!("Brush {index}"),
                        ComponentMode::Add,
                        "brush",
                        json!({}),
                    )
                })
                .collect::<Vec<_>>()
        };
        let full = Mask {
            components: components(COMPONENTS_PER_MASK),
            ..mask.clone()
        };
        full.validate().unwrap();
        let error = Mask {
            components: components(COMPONENTS_PER_MASK + 1),
            ..mask.clone()
        }
        .validate()
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(
            error.detail,
            format!(
                "mask Mask 1 has {} components; the limit is {COMPONENTS_PER_MASK} components per \
                 mask",
                COMPONENTS_PER_MASK + 1
            )
        );

        // One payload well inside every other limit, whose bytes are not: the bound is on what a
        // snapshot stores, because every history entry stores all of it.
        let heavy = Mask {
            components: vec![Component::new(
                "Brush 1",
                ComponentMode::Add,
                "brush",
                json!({"points": vec![[0.123_456_7_f64, 0.765_432_1]; 14_000]}),
            )],
            ..mask
        };
        let bytes = serde_json::to_vec(&vec![heavy.clone()]).unwrap().len();
        assert!(bytes > MASK_BYTES_PER_RECIPE, "{bytes} bytes");
        let error = Recipe {
            masks: vec![heavy],
            ..Recipe::default()
        }
        .validate()
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(
            error.detail,
            format!(
                "recipe masks serialize to {bytes} bytes; the limit is {MASK_BYTES_PER_RECIPE} \
                 serialized mask bytes per recipe"
            )
        );
    }

    #[test]
    fn snapshots_are_complete_immutable_and_round_trip() {
        let original = Snapshot::original(AssetId::new());
        let a = original.append(Layer::pixel(1, 2, [3, 4, 5])).unwrap();
        let b = a.append(Layer::pixel(1, 2, [6, 7, 8])).unwrap();
        assert!(original.recipe.layers.is_empty());
        assert_eq!(a.recipe.layers.len(), 1);
        assert_eq!(b.recipe.layers.len(), 2);
        let encoded = serde_json::to_vec(&b).unwrap();
        assert_eq!(serde_json::from_slice::<Snapshot>(&encoded).unwrap(), b);
    }

    #[test]
    fn invalid_identity_structure_and_duplicates_are_rejected() {
        assert!(serde_json::from_str::<AssetId>("\"bad\"").is_err());
        let mut recipe = Recipe::default();
        let layer = Layer::pixel(0, 0, [1, 2, 3]);
        recipe.layers.extend([layer.clone(), layer]);
        assert!(recipe.validate().is_err());
        let unsupported = Recipe {
            format: 99,
            layers: Vec::new(),
            masks: Vec::new(),
            ..Recipe::default()
        };
        assert_eq!(
            unsupported.validate().unwrap_err().kind,
            ErrorKind::Incompatible
        );
        let nameless = Layer {
            id: LayerId::new(),
            effect_id: String::new(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
        };
        assert_eq!(nameless.validate().unwrap_err().kind, ErrorKind::Validation);
        // Payload shape and effect format are the providing module's business, not the model's.
        assert!(
            Layer {
                effect_format: 99,
                payload: json!({"x": 1}),
                ..Layer::pixel(0, 0, [1, 2, 3])
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn a_layer_is_inserted_at_its_position_and_an_index_past_the_end_is_rejected() {
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![Layer::pixel(0, 0, [1, 2, 3]), Layer::pixel(1, 1, [4, 5, 6])],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let joined = Layer::orientation(Orientation::NEUTRAL);
        for index in 0..=recipe.layers.len() {
            let next = recipe.with_layer_inserted(index, joined.clone()).unwrap();
            assert_eq!(next.layers.len(), 3);
            assert_eq!(next.layers[index], joined, "inserted at {index}");
            let kept: Vec<&Layer> = next
                .layers
                .iter()
                .filter(|layer| layer.id != joined.id)
                .collect();
            assert_eq!(kept, recipe.layers.iter().collect::<Vec<_>>(), "order kept");
            assert_eq!(recipe.layers.len(), 2, "the original recipe is untouched");
        }
        assert_eq!(
            recipe.appended(joined.clone()).unwrap(),
            recipe
                .with_layer_inserted(recipe.layers.len(), joined.clone())
                .unwrap(),
            "appending is inserting at the end"
        );
        let error = recipe.with_layer_inserted(3, joined).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            "layer index 3 is outside the 2 layers of the stack"
        );
        let duplicate = recipe.layers[1].clone();
        assert_eq!(
            recipe.with_layer_inserted(0, duplicate).unwrap_err().kind,
            ErrorKind::Validation,
            "a duplicate identity is rejected wherever it is inserted"
        );
    }
}
