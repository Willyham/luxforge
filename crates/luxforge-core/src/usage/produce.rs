//! The core's producers: what its authoritative boundaries share to record a usage event
//! (`docs/engineering/usage-statistics-coverage.md`).
//!
//! Every producer first asks [`Producer::on`], one atomic load, and does nothing more while
//! collection is off. While it is on, a producer records fixed-size events into the collector's
//! active bank: no formatting, no waiting, and no allocation but the one an adopted photograph's
//! cache slot takes for its asset identity.
//!
//! The keyed image ID of a photograph is computed once, when it is adopted as a client's open
//! photograph ([`Producer::adopt`]), from the catalog fingerprint the adoption already holds, and
//! kept here beside its asset for the events that act on it; the adoption's ask is usually
//! answered by the collector's memo of the last ID, which the open's preparation outcome just
//! computed ([`Collector::image_id`]). A commit to a photograph no client has open computes its ID
//! from the fingerprint its state holds (at most one HMAC) without keeping it.
use super::{Collector, EventKind, Feature, Generation, ImageId, Recorded, Tool};
use crate::{AssetId, ModuleRegistry, Recipe};
use std::sync::Arc;

/// How many adopted photographs keep their image ID: one per live client the owner serves is
/// plenty, and a photograph that falls out costs one HMAC when next used.
const OPEN_IMAGES: usize = 8;

struct OpenImage {
    generation: Generation,
    asset: AssetId,
    image: ImageId,
}

/// The collector the core records into, and the image IDs of the photographs clients have open.
pub(crate) struct Producer {
    collector: Arc<Collector>,
    open: [Option<OpenImage>; OPEN_IMAGES],
    next: usize,
}

impl Default for Producer {
    /// A producer over a collector that is never started: every record is a no-op.
    fn default() -> Self {
        Self::new(Arc::new(Collector::new()))
    }
}

impl std::fmt::Debug for Producer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Producer")
    }
}

impl Producer {
    pub(crate) fn new(collector: Arc<Collector>) -> Self {
        Self {
            collector,
            open: Default::default(),
            next: 0,
        }
    }

    pub(crate) fn collector(&self) -> &Arc<Collector> {
        &self.collector
    }

    /// The current consent generation, or `None` while collection is off: one atomic load.
    pub(crate) fn on(&self) -> Option<Generation> {
        self.collector.gate().current()
    }

    /// Record `kind` in `generation`, the one the producer saw when it started.
    pub(crate) fn record(&self, generation: Generation, kind: EventKind) -> Recorded {
        self.collector.record_in(generation, kind)
    }

    /// `asset` became a client's open photograph: its image ID, computed from `fingerprint` the
    /// first time in this generation and kept for the events that act on it.
    pub(crate) fn adopt(
        &mut self,
        generation: Generation,
        asset: &AssetId,
        fingerprint: &str,
    ) -> Option<ImageId> {
        if let Some(image) = self.open_image(generation, asset.as_str()) {
            return Some(image);
        }
        let image = self.collector.image_id(fingerprint)?;
        self.open[self.next] = Some(OpenImage {
            generation,
            asset: asset.clone(),
            image,
        });
        self.next = (self.next + 1) % OPEN_IMAGES;
        Some(image)
    }

    /// The image ID of `asset` when a client adopted it in this generation.
    pub(crate) fn open_image(&self, generation: Generation, asset: &str) -> Option<ImageId> {
        self.open.iter().flatten().find_map(|open| {
            (open.generation == generation && open.asset.as_str() == asset).then_some(open.image)
        })
    }

    /// The image ID of `asset`: the adopted one, or one computed from `fingerprint` now.
    pub(crate) fn image_of(
        &self,
        generation: Generation,
        asset: &AssetId,
        fingerprint: &str,
    ) -> Option<ImageId> {
        self.open_image(generation, asset.as_str())
            .or_else(|| self.collector.image_id(fingerprint))
    }
}

/// The interaction a committed mask command is: its action identity, as its history entry
/// records it, whether it came from its own method or a drawn gesture's `draft.commit`.
pub(crate) fn mask_feature(action_id: &str) -> Option<Feature> {
    let command = action_id.strip_prefix("mask.")?;
    Some(match command {
        "delete" => Feature::MaskDelete,
        "delete-component" => Feature::MaskComponentDelete,
        "duplicate" => Feature::MaskDuplicate,
        "rename" | "rename-component" => Feature::MaskRename,
        "reorder" | "reorder-component" => Feature::MaskReorder,
        // A brush stroke, a sampled colour added or removed, a pick: a change to a component.
        "add-stroke" | "delete-stroke" => Feature::MaskEdit,
        pick if pick.starts_with("pick-") => Feature::MaskEdit,
        sample if sample.ends_with("-sample") => Feature::MaskEdit,
        create if create.starts_with("create-") => Feature::MaskCreate,
        add if add.starts_with("add-") => Feature::MaskComponentAdd,
        set if set.starts_with("set-") => Feature::MaskEdit,
        _ => return None,
    })
}

/// A set of [`Tool`]s, one bit each: what one transaction changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ToolSet(u32);

impl ToolSet {
    fn bit(tool: Tool) -> u32 {
        let index = Tool::ALL
            .iter()
            .position(|candidate| *candidate == tool)
            .unwrap_or(0);
        1 << index
    }

    pub(crate) fn insert(&mut self, tool: Tool) {
        self.0 |= Self::bit(tool);
    }

    pub(crate) fn contains(self, tool: Tool) -> bool {
        self.0 & Self::bit(tool) != 0
    }

    #[cfg(test)]
    pub(crate) fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The tools in the registry's order.
    pub(crate) fn iter(self) -> impl Iterator<Item = Tool> {
        Tool::ALL
            .iter()
            .copied()
            .filter(move |tool| self.contains(*tool))
    }
}

/// The tool group of a layer's effect, through the module that provides it: a built-in group, or
/// `external` for any external module, whose identity is never reported.
fn tool_of(registry: &ModuleRegistry, effect_id: &str) -> Option<Tool> {
    let module = registry.effect_module(effect_id)?;
    if registry.external_origin(module).is_some() {
        return Some(Tool::External);
    }
    Tool::from_module_id(module)
}

/// What a recipe's layers of one tool group are, layer identity aside.
fn group<'a>(
    registry: &'a ModuleRegistry,
    recipe: &'a Recipe,
    tool: Tool,
) -> impl Iterator<
    Item = (
        &'a String,
        u32,
        &'a serde_json::Value,
        &'a Option<crate::MaskId>,
        &'a Vec<crate::ArtifactId>,
    ),
> {
    recipe
        .layers
        .iter()
        .filter(move |layer| tool_of(registry, &layer.effect_id) == Some(tool))
        .map(|layer| {
            (
                &layer.effect_id,
                layer.effect_format,
                &layer.payload,
                &layer.mask,
                &layer.artifacts,
            )
        })
}

/// The built-in tool groups whose layers differ between `before` and `after`: a layer added,
/// removed, reordered within its group, or with another payload, format, mask or artifact list. A
/// layer's identity alone is not a change, and neither is a mask's geometry (a mask edit, not a
/// tool's). Allocates nothing: `O(tools × layers)` lookups, on a commit only.
pub(crate) fn changed_tools(registry: &ModuleRegistry, before: &Recipe, after: &Recipe) -> ToolSet {
    let mut present = ToolSet::default();
    for layer in before.layers.iter().chain(&after.layers) {
        if let Some(tool) = tool_of(registry, &layer.effect_id) {
            present.insert(tool);
        }
    }
    let mut changed = ToolSet::default();
    for tool in present.iter() {
        if !group(registry, before, tool).eq(group(registry, after, tool)) {
            changed.insert(tool);
        }
    }
    changed
}

#[cfg(test)]
#[path = "produce_tests.rs"]
mod tests;
