//! Where a committed layer joins a stack, by its effect's declared stage and order and, for a
//! masked layer, its mask's index; and the one layer of an effect a target owns. `O(layers)`
//! descriptor lookups; nothing here reads pixels.
use super::ModuleRegistry;
use crate::{
    Error, Layer, Mask, MaskId,
    modules::{EffectStage, ModuleDescriptor},
};
use std::collections::HashSet;

/// One target's position in the order masked layers of an effect take: the global layer is first,
/// and each mask follows at its own index in the recipe's mask list. A reference to a mask the table
/// does not hold sorts last rather than being treated as the global layer; such a stack is refused by
/// [`Recipe::validate`] before anything renders, and this keeps the refusal from depending on a
/// position guess.
fn target_rank(mask: Option<&MaskId>, masks: &[Mask]) -> usize {
    match mask {
        None => 0,
        Some(id) => masks
            .iter()
            .position(|mask| &mask.id == id)
            .map_or(usize::MAX, |index| index + 1),
    }
}

impl ModuleRegistry {
    /// The stage and the within-stage order an effect declares, or `None` when no provider declares
    /// the effect at all.
    fn effect_placement(&self, effect_id: &str) -> Option<(EffectStage, u16)> {
        self.effect(effect_id)
            .map(|(_, effect)| (effect.stage, effect.order))
    }

    /// Where a committed layer of this effect joins a stack, by the stage and the order its
    /// descriptor declares. A module names its own effect rather than repeating its placement.
    /// An effect no provider declares is placed as a geometry effect would be and refused by the
    /// whole-stack compile that follows; a *layer* whose effect no provider declares opens no
    /// region either, because such a stack cannot compile at all and the host reports that rather
    /// than guessing a position.
    pub fn insertion_index_for(&self, layers: &[Layer], effect_id: &str) -> usize {
        let (stage, order) = self
            .effect_placement(effect_id)
            .unwrap_or((EffectStage::Geometry, 0));
        self.insertion_index(layers, stage, order)
    }

    /// Where a committed layer of this effect **bound to this target** joins a stack, the global
    /// layer and each mask being distinct targets (`docs/design/masking.md`, "Order"):
    ///
    /// 1. A masked layer of an effect is placed after the global layer of that effect.
    /// 2. Masked layers of one effect are ordered by their mask's index in `recipe.masks`.
    ///
    /// so overlapping masks apply in the order the mask list shows. Nothing else moves: the stage
    /// region and the within-stage order are [`Self::insertion_index_for`]'s, and this only decides
    /// where among the layers of the *same* effect the new one goes, which is inside that region by
    /// construction. A stack that already holds those layers in another order keeps it and renders
    /// in it, exactly as every other placement rule promises.
    ///
    /// Cost is `O(layers)` descriptor lookups plus `O(layers · masks)` target ranks; it reads no
    /// pixels and allocates nothing.
    pub fn insertion_index_for_target(
        &self,
        layers: &[Layer],
        effect_id: &str,
        mask: Option<&MaskId>,
        masks: &[Mask],
    ) -> usize {
        let base = self.insertion_index_for(layers, effect_id);
        let rank = target_rank(mask, masks);
        let same_effect = |layer: &Layer| layer.effect_id == effect_id;
        if rank == 0 {
            // The global layer of an effect keeps exactly the placement it has always had, except
            // that it must not land after a masked layer of its own effect: nothing about masking
            // changes where an unmasked layer goes.
            return match layers
                .iter()
                .position(|layer| same_effect(layer) && layer.mask.is_some())
            {
                Some(index) if index < base => index,
                _ => base,
            };
        }
        let mut after = None;
        let mut before = None;
        for (index, layer) in layers.iter().enumerate() {
            if !same_effect(layer) {
                continue;
            }
            if target_rank(layer.mask.as_ref(), masks) <= rank {
                after = Some(index + 1);
            } else if before.is_none() {
                before = Some(index);
            }
        }
        after.or(before).unwrap_or(base)
    }

    /// Re-sort the masked layers of every effect into the order the mask list now shows, and move
    /// nothing else. This is the second half of the ordering rule: `mask.reorder` changes the index
    /// of a mask, so the layers bound to it change position among the layers of *their own effect*,
    /// as one host transaction.
    ///
    /// Only positions already held by layers of one effect are rewritten, so no layer crosses a
    /// stage boundary, a spatial layer stays after the pointwise ones, and a stack with no masked
    /// layers is returned untouched. The sort is stable, so two layers of one effect with the same
    /// target — a stack the compile below refuses as ambiguous — keep their relative order rather
    /// than being reshuffled. `O(layers · masks)`; reads no pixels.
    pub fn sort_masked_layers(&self, layers: &mut [Layer], masks: &[Mask]) {
        let effects: Vec<String> = layers
            .iter()
            .filter(|layer| layer.mask.is_some())
            .map(|layer| layer.effect_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        for effect_id in effects {
            let positions: Vec<usize> = layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| layer.effect_id == effect_id)
                .map(|(index, _)| index)
                .collect();
            let mut ordered: Vec<Layer> = positions
                .iter()
                .map(|index| layers[*index].clone())
                .collect();
            ordered.sort_by_key(|layer| target_rank(layer.mask.as_ref(), masks));
            for (index, layer) in positions.into_iter().zip(ordered) {
                layers[index] = layer;
            }
        }
    }

    /// Whether `layer` is part of the stack the target `mask` sees: a layer of a maskable effect
    /// only when it carries that same target, and every other layer always. `None` is the global
    /// target. Planning filters a maskable module's stack by it, and [`Self::own_layer`] finds a
    /// module's layer by it, so a capture, a plan and a query all read the layer of one target.
    pub fn in_target(&self, layer: &Layer, mask: Option<&MaskId>) -> bool {
        layer.mask.as_ref() == mask || !self.effect_maskable(&layer.effect_id)
    }

    /// The one layer of `effect_id` that belongs to `target`, with its index, or `None` when the
    /// stack holds none: how every module that owns one layer finds it, through
    /// [`super::StageContext::own_layer`], and how a preset captures one. A stack that holds two
    /// such layers is refused with `ambiguous <module title> layers`, the refusal the whole-stack
    /// compile makes for a `single` effect, rather than resolved by guessing; nothing is rewritten.
    /// `O(layers)`; reads no pixels.
    pub fn own_layer<'l>(
        &self,
        layers: &'l [Layer],
        effect_id: &str,
        target: Option<&MaskId>,
    ) -> Result<Option<(usize, &'l Layer)>, Error> {
        let mut found = None;
        for (index, layer) in layers.iter().enumerate() {
            if layer.effect_id != effect_id || !self.in_target(layer, target) {
                continue;
            }
            if found.is_some() {
                return Err(self.ambiguous(effect_id));
            }
            found = Some((index, layer));
        }
        Ok(found)
    }

    /// The refusal of a stack that holds two layers of one effect for one target, named by the
    /// providing module's title (or the effect, when no provider declares it).
    pub(super) fn ambiguous(&self, effect_id: &str) -> Error {
        let title = self
            .effect(effect_id)
            .map_or(effect_id, |(module, _)| module.descriptor().title.as_str());
        Error::validation(format!("ambiguous {title} layers"))
    }

    /// Where a committed layer of this stage and order joins a stack:
    ///
    /// | Stage | Placement |
    /// | --- | --- |
    /// | `source` | index zero |
    /// | `pixel`, `color` | before the first spatial, geometry or finish layer |
    /// | `spatial` | before the first geometry or finish layer, after every pixel and colour layer |
    /// | `geometry` | before the first finish layer |
    /// | `finish` | at the end |
    ///
    /// so the geometry tail carries every content-stage edit, a neighbourhood operation reads the
    /// finished pointwise colour, and a finish effect sees the output coordinates the tail
    /// produced. A leading source layer keeps index zero whatever is inserted.
    ///
    /// Within the region its stage chooses, the new layer goes after the last layer of the *same*
    /// stage whose order is at most `order` and before the first whose order is greater. Layers of
    /// other stages inside the region keep their positions, and no existing layer ever moves, so a
    /// stack stored in another order stays exactly as it is and renders in its stored order.
    ///
    /// Cost is `O(layers)` in descriptor lookups; it reads no pixels and allocates nothing.
    pub fn insertion_index(&self, layers: &[Layer], stage: EffectStage, order: u16) -> usize {
        placement_index(layers, stage, order, |effect_id| {
            self.effect_placement(effect_id)
        })
    }
}

/// The placement rule of [`ModuleRegistry::insertion_index`] over any lookup of an effect's
/// declared stage and order, so the host's registry and a client's module list answer alike.
fn placement_index(
    layers: &[Layer],
    stage: EffectStage,
    order: u16,
    placement: impl Fn(&str) -> Option<(EffectStage, u16)>,
) -> usize {
    if stage == EffectStage::Source {
        return 0;
    }
    // A source layer prepares the content stage and always stays at index zero.
    let mut lower = usize::from(layers.first().is_some_and(|layer| {
        placement(&layer.effect_id).map(|(stage, _)| stage) == Some(EffectStage::Source)
    }));
    let opens_region = |candidate: EffectStage| match stage {
        EffectStage::Pixel | EffectStage::Color => matches!(
            candidate,
            EffectStage::Spatial | EffectStage::Geometry | EffectStage::Finish
        ),
        EffectStage::Spatial => {
            matches!(candidate, EffectStage::Geometry | EffectStage::Finish)
        }
        EffectStage::Geometry => candidate == EffectStage::Finish,
        EffectStage::Source | EffectStage::Finish => false,
    };
    let mut upper = layers.len();
    // The first layer of the same stage whose order is greater: the new layer goes before it.
    let mut successor = None;
    for (index, layer) in layers.iter().enumerate() {
        let Some((layer_stage, layer_order)) = placement(&layer.effect_id) else {
            continue;
        };
        if opens_region(layer_stage) {
            upper = index;
            break;
        }
        // A spatial layer reads what the pointwise colour run produced, so it never lands
        // before a pixel or colour layer a stored stack kept later than usual.
        if stage == EffectStage::Spatial
            && matches!(layer_stage, EffectStage::Pixel | EffectStage::Color)
        {
            lower = index + 1;
        }
        if layer_stage == stage && layer_order > order && successor.is_none() {
            successor = Some(index);
        }
    }
    let upper = upper.max(lower);
    successor.unwrap_or(upper).clamp(lower, upper)
}

/// Where a committed layer of `effect_id` joins `layers`, by the host's placement rule read from a
/// client's module descriptors (what `module.list` returns) instead of a registry. A client that
/// has to show a stage the host will address, such as a crop draft showing the crop's input stage
/// before any crop exists, asks this rather than repeating the rule. An effect no listed module
/// declares is placed as a geometry effect would be, as the host places it.
pub fn insertion_index_among(
    modules: &[ModuleDescriptor],
    layers: &[Layer],
    effect_id: &str,
) -> usize {
    let placement = |id: &str| {
        modules
            .iter()
            .flat_map(|module| module.effects.iter())
            .find(|effect| effect.id == id)
            .map(|effect| (effect.stage, effect.order))
    };
    let (stage, order) = placement(effect_id).unwrap_or((EffectStage::Geometry, 0));
    placement_index(layers, stage, order, placement)
}
