//! What a client reads back: `mask.list`'s listing of one stack, each stroke with the settings it
//! was drawn with, and the layers a destructive command reports it removed.
use crate::mask::knows_component_kind;
use crate::{
    ComponentId, ComponentMode, LayerId, MaskId, ModuleRegistry, Recipe,
    path::{self, StrokeId},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// The strokes the payload's reserved `strokes` field references, in the order they compose,
    /// each with the settings it was drawn with as the recipe's stroke store holds them. Empty for a
    /// component that references none, which is every component whose geometry is declared as
    /// numbers; the payload itself is still reported unchanged beside it.
    pub strokes: Vec<StrokeReport>,
    /// What a model selection records and how it stands against the photograph now; absent for
    /// every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelReport>,
}

/// A model selection's row: what computed it, when its raster was published, and why it is stale.
///
/// `stale` lists each reason in the order [`ModelStale`] declares them, and is empty for a current
/// selection; it is `None` only when the current input could not be determined, as for a stack that
/// cannot be evaluated, which says so rather than claiming the selection is current. A stale
/// selection keeps rendering from its raster; Recompute is the task again and `mask.set-model`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelReport {
    pub class: String,
    pub model: String,
    pub model_version: String,
    pub artifact: crate::ArtifactId,
    /// When the raster was published, in milliseconds since the Unix epoch, from the catalog's
    /// artifact row; `None` when the catalog does not record it.
    pub computed_ms: Option<i64>,
    pub stale: Option<Vec<ModelStale>>,
    /// How an object was picked ([`crate::mask::ObjectPrompt`]), which Recompute sends
    /// `task.select-object` again; `None` for another class or a prompt that was not stored.
    #[serde(default)]
    pub prompt: Option<crate::mask::ObjectPrompt>,
}

/// One reason a model selection no longer matches what it was computed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelStale {
    /// The original's interpretation: another file, or another decode of it.
    Source,
    /// The repair stage's output: the development before it or a correction in it.
    RepairPrefix,
    /// The module now declares another version of the model that computed it.
    ModelVersion,
}

impl ModelStale {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::RepairPrefix => "repair-prefix",
            Self::ModelVersion => "model-version",
        }
    }

    /// What changed, in the words a row says it: `the original changed`.
    pub fn sentence(self) -> &'static str {
        match self {
            Self::Source => "the original changed",
            Self::RepairPrefix => "the photograph was developed or repaired differently",
            Self::ModelVersion => "the model was updated",
        }
    }
}

/// One stroke a component references: its content address and, when the recipe's stroke store
/// resolves it, the settings it was drawn with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrokeReport {
    pub id: StrokeId,
    /// `None` when the store does not hold the referenced stroke. The reference is still listed —
    /// a stroke the store cannot resolve is reported, never dropped — and rendering the mask is
    /// refused by name as it always is.
    pub settings: Option<StrokeSettings>,
}

impl Eq for StrokeReport {}

/// A stored stroke's settings, exactly as stored: `size` is the radius in normalized units (one
/// unit the content stage's height), `feather` and `flow` whole percentages, `erase` whether it takes
/// coverage away, and `colour` the hold it carries when it is limited to a colour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrokeSettings {
    pub erase: bool,
    pub size: f64,
    pub feather: f64,
    pub flow: f64,
    pub colour: Option<StrokeColour>,
}

/// The colour a limited stroke holds: the sRGB codes it was seeded on and its refine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrokeColour {
    pub seed: [u8; 3],
    pub refine: f64,
}

impl StrokeReport {
    fn of(id: StrokeId, strokes: &path::StrokeTable) -> Self {
        let settings = strokes
            .get::<crate::mask::Stroke>(&id)
            .map(|stroke| StrokeSettings {
                erase: stroke.erase(),
                size: stroke.size(),
                feather: stroke.feather(),
                flow: stroke.flow(),
                colour: stroke.colour_limit().map(|limit| StrokeColour {
                    seed: limit.codes(),
                    refine: limit.refine(),
                }),
            });
        Self { id, settings }
    }
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
                    // A malformed reserved field is reported as the payload it is and refused where
                    // the mask is drawn; the listing names no stroke it cannot read.
                    strokes: path::references(&component.payload, &component.name)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|id| StrokeReport::of(id, &recipe.strokes))
                        .collect(),
                    model: crate::mask::model_selection(component).map(|selection| ModelReport {
                        class: selection.class,
                        model: selection.model,
                        model_version: selection.model_version,
                        artifact: selection.artifact,
                        computed_ms: None,
                        stale: None,
                        prompt: selection.prompt,
                    }),
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
