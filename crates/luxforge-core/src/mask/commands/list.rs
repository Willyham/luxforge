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
