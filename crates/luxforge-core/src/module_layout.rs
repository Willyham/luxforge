//! The person's module layout: which registered modules their workspace offers and in what order
//! the tools panel and the mode strip list them
//! ([module manager](../../../docs/design/module-manager.md#phase-1-arranging-the-built-in-modules)).
//!
//! The layout is **presentation only**. It is stored in `preferences.json` as `module_layout`,
//! outside every catalog, and nothing that plans, renders, samples, exports or applies a preset
//! reads it: an off module stays registered, the registry keeps its order, and `schema.list` is
//! the same whatever the layout holds. A client reads it through `module.layout.read` and the
//! `enabled` and `placements` of `module.list`, and changes it through `module.enable`,
//! `module.disable`, `module.order.set` and `module.layout.reset`.
//!
//! Only explicit choices are stored: a module never turned off has no entry, and a toolset never
//! reordered has no order, so it follows its default order: registry order, with the mode strip's
//! brush workspaces before its crop frames. An id the layout names that this run does not register
//! is kept through every write and listed as unrecognized. A module registered since the order was
//! stored takes its default place, after the neighbour that precedes it in the default order.
//!
//! Every operation here is `O(m²)` in the registered module count `m` at most, with no image,
//! catalog or registry change.
use crate::{CanvasInteraction, Error, ModuleDescriptor, Toolset};
use serde::{Deserialize, Serialize};

/// The most ids one stored list holds: the disabled set, or one toolset's order.
pub const MAX_LAYOUT_IDS: usize = 64;

/// The longest module id the layout stores, in bytes.
pub const MAX_LAYOUT_ID: usize = 128;

/// The event every change to the layout announces, under the request that made it.
pub const LAYOUT_EVENT: &str = "module.layout";

/// A fill task's id prefix: `fill-generative`. A module declaring one is a Corrections kind.
const FILL_TASK_PREFIX: &str = "fill-";
/// The object pick query, the Corrections band's Object kind.
const PICK_OBJECT_QUERY: &str = "pick-object";
/// The object selection task, the Masks panel's Object kind.
const SELECT_OBJECT_TASK: &str = "select-object";

/// Where a registered module came from. Every module this build registers is linked into it;
/// installed packages and developer folders are the later origins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModuleOrigin {
    /// Linked into this build.
    #[default]
    BuiltIn,
    /// Installed from a package file.
    Community,
    /// Loaded from an unpacked folder in developer mode.
    Folder,
}

/// One module's entry in one toolset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub toolset: Toolset,
}

/// Where `module`'s interface appears, in [`Toolset::ALL`] order, derived from what its descriptor
/// already declares, the same rule the desktop drew by:
///
/// - **adjustments**: it draws a tools-panel section: it declares controls, a crop frame, or a
///   capability block (settings, a resource or a task). A module with a fill task draws a block
///   only for its settings, since its models and setup belong to the Corrections band.
/// - **corrections**: it declares a `fill-` task or the object pick query.
/// - **masking**: it declares the object selection task.
/// - **tools**: its canvas takes the whole canvas over: a crop frame, or a brush whose strokes edit
///   a declared operation list.
///
/// The desktop still filters what it shows by availability, the source kind, developer mode and
/// experiment flags; a placement says where the module belongs, not that it is offered now. A
/// module with none, such as the RAW development, is processing only. `O(declarations)`.
pub fn placements(module: &ModuleDescriptor) -> Vec<Placement> {
    Toolset::ALL
        .into_iter()
        .filter(|toolset| placed_in(module, *toolset))
        .map(|toolset| Placement { toolset })
        .collect()
}

/// Whether `module` has a placement in `toolset` ([`placements`]).
pub fn placed_in(module: &ModuleDescriptor, toolset: Toolset) -> bool {
    match toolset {
        Toolset::Adjustments => {
            let fill = module
                .tasks
                .iter()
                .any(|task| task.id.starts_with(FILL_TASK_PREFIX));
            let capabilities = if fill {
                module.settings.is_some()
            } else {
                module.settings.is_some()
                    || !module.resources.is_empty()
                    || !module.tasks.is_empty()
            };
            !module.controls.is_empty()
                || capabilities
                || matches!(module.canvas, Some(CanvasInteraction::CropFrame { .. }))
        }
        Toolset::Corrections => {
            module
                .tasks
                .iter()
                .any(|task| task.id.starts_with(FILL_TASK_PREFIX))
                || module.query(PICK_OBJECT_QUERY).is_some()
        }
        Toolset::Masking => module.task(SELECT_OBJECT_TASK).is_some(),
        Toolset::Tools => match &module.canvas {
            Some(CanvasInteraction::CropFrame { .. }) => true,
            Some(CanvasInteraction::BrushMask { edit, .. }) => edit.is_some(),
            _ => false,
        },
        Toolset::Processing => false,
    }
}

/// One module as `module.list` answers it: its descriptor's fields, then where it came from, its
/// placements and whether the person has it on. A client reads the descriptor back with
/// [`ListedModule::descriptor`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListedModule {
    #[serde(flatten)]
    pub descriptor: ModuleDescriptor,
    pub origin: ModuleOrigin,
    pub placements: Vec<Placement>,
    /// Whether the person has the module on. A locked module always is. An off module stays
    /// registered: its photos render and export, and its methods answer.
    pub enabled: bool,
}

/// The descriptors of `module.list`'s `modules`, in their listed order, for a client that reads
/// only the descriptors.
pub fn listed_descriptors(modules: serde_json::Value) -> serde_json::Result<Vec<ModuleDescriptor>> {
    Ok(serde_json::from_value::<Vec<ListedModule>>(modules)?
        .into_iter()
        .map(|listed| listed.descriptor)
        .collect())
}

/// [`ListedModule`] borrowing its descriptor, as `module.list` writes it.
#[derive(Serialize)]
pub(crate) struct Listing<'a> {
    #[serde(flatten)]
    descriptor: &'a ModuleDescriptor,
    origin: ModuleOrigin,
    placements: Vec<Placement>,
    enabled: bool,
}

/// The person's stored choices, `module_layout` in `preferences.json`: the modules they turned off
/// and the orders they chose, each exactly as stored, recognized or not.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredLayout {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<String>,
    #[serde(default, skip_serializing_if = "StoredOrders::is_empty")]
    pub order: StoredOrders,
}

/// The stored order of each orderable toolset; an empty one is not stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredOrders {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adjustments: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
}

impl StoredOrders {
    fn is_empty(&self) -> bool {
        self.adjustments.is_empty() && self.tools.is_empty()
    }

    fn of(&self, toolset: Toolset) -> &[String] {
        match toolset {
            Toolset::Adjustments => &self.adjustments,
            Toolset::Tools => &self.tools,
            _ => &[],
        }
    }

    fn of_mut(&mut self, toolset: Toolset) -> Option<&mut Vec<String>> {
        match toolset {
            Toolset::Adjustments => Some(&mut self.adjustments),
            Toolset::Tools => Some(&mut self.tools),
            _ => None,
        }
    }
}

impl StoredLayout {
    fn is_empty(&self) -> bool {
        self.disabled.is_empty() && self.order.is_empty()
    }

    /// The rules a stored layout meets that its type cannot say, refused by the list's name: each
    /// list holds at most [`MAX_LAYOUT_IDS`] distinct module identities of at most
    /// [`MAX_LAYOUT_ID`] bytes. Whether an id is registered is not checked: an unrecognized one is
    /// kept.
    pub(crate) fn check(&self) -> Result<(), String> {
        for (name, ids) in [
            ("disabled", &self.disabled),
            ("order.adjustments", &self.order.adjustments),
            ("order.tools", &self.order.tools),
        ] {
            if ids.len() > MAX_LAYOUT_IDS {
                return Err(format!(
                    "module_layout.{name} holds more than {MAX_LAYOUT_IDS} modules"
                ));
            }
            for (at, id) in ids.iter().enumerate() {
                if id.len() > MAX_LAYOUT_ID || !crate::modules::valid_identity(id) {
                    return Err(format!(
                        "module_layout.{name} holds {id:?}, which is not a module identity of \
                         at most {MAX_LAYOUT_ID} bytes"
                    ));
                }
                if ids[..at].contains(id) {
                    return Err(format!("module_layout.{name} names {id} twice"));
                }
            }
        }
        Ok(())
    }
}

/// The layout as `module.layout.read` answers it, and every layout write after its change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutView {
    /// Each orderable toolset ([`Toolset::ORDERABLE`]): the person's order and the default.
    pub toolsets: Vec<ToolsetOrder>,
    /// The registered modules the person turned off, in registry order.
    pub disabled: Vec<String>,
    /// The ids the stored layout names that this run does not register, sorted: an uninstalled
    /// module, one this build no longer has, or a developer module outside developer mode. They
    /// are kept through every write.
    pub unrecognized: Vec<String>,
}

/// One orderable toolset's placements: in the person's order, and in the default order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsetOrder {
    pub toolset: Toolset,
    pub order: Vec<String>,
    pub default: Vec<String>,
}

impl LayoutView {
    /// The person's order of `toolset`, or `None` for one that is not orderable.
    pub fn order(&self, toolset: Toolset) -> Option<&[String]> {
        self.toolsets
            .iter()
            .find(|entry| entry.toolset == toolset)
            .map(|entry| entry.order.as_slice())
    }
}

/// The registered modules a layout is read and changed against, in registry order.
pub(crate) struct Arrangement<'a> {
    modules: Vec<&'a ModuleDescriptor>,
}

impl<'a> Arrangement<'a> {
    pub(crate) fn new(modules: Vec<&'a ModuleDescriptor>) -> Self {
        Self { modules }
    }

    fn registered(&self, id: &str) -> bool {
        self.modules.iter().any(|module| module.id == id)
    }

    /// The registered module `id`, refused by name when it is not registered.
    fn module(&self, id: &str) -> Result<&'a ModuleDescriptor, Error> {
        self.modules
            .iter()
            .copied()
            .find(|module| module.id == id)
            .ok_or_else(|| Error::validation(format!("module {id} is not registered")))
    }

    /// The modules placed in `toolset`, in registry order, except that the mode strip's tools list
    /// the brush workspaces before the crop frames, each in registry order, as the strip has always
    /// drawn them after Mask.
    fn default_order(&self, toolset: Toolset) -> Vec<&'a str> {
        let (brushes, others): (Vec<&ModuleDescriptor>, Vec<&ModuleDescriptor>) = self
            .modules
            .iter()
            .copied()
            .filter(|module| placed_in(module, toolset))
            .partition(|module| {
                toolset == Toolset::Tools
                    && matches!(module.canvas, Some(CanvasInteraction::BrushMask { .. }))
            });
        brushes
            .into_iter()
            .chain(others)
            .map(|module| module.id.as_str())
            .collect()
    }

    /// `toolset`'s effective order: the stored ids placed in it, in their stored order, and every
    /// other placement after the neighbour that precedes it in the default order, or first when
    /// none does.
    fn order(&self, stored: &StoredLayout, toolset: Toolset) -> Vec<&'a str> {
        let default = self.default_order(toolset);
        let mut order: Vec<&'a str> = stored
            .order
            .of(toolset)
            .iter()
            .filter_map(|id| default.iter().copied().find(|placed| placed == id))
            .collect();
        for (at, id) in default.iter().enumerate() {
            if order.contains(id) {
                continue;
            }
            let after = default[..at]
                .iter()
                .rev()
                .find_map(|before| order.iter().position(|placed| placed == before))
                .map_or(0, |position| position + 1);
            order.insert(after, id);
        }
        order
    }

    /// Whether the person has `module` on: it is locked, or they did not turn it off.
    pub(crate) fn enabled(&self, stored: Option<&StoredLayout>, module: &ModuleDescriptor) -> bool {
        module.locked.is_some() || stored.is_none_or(|stored| !stored.disabled.contains(&module.id))
    }

    /// `module.list`'s modules: `modules`, each with its origin, placements and switch.
    pub(crate) fn listing<'m>(
        &self,
        stored: Option<&StoredLayout>,
        modules: impl IntoIterator<Item = &'m ModuleDescriptor>,
    ) -> Vec<Listing<'m>> {
        modules
            .into_iter()
            .map(|descriptor| Listing {
                origin: ModuleOrigin::BuiltIn,
                placements: placements(descriptor),
                enabled: self.enabled(stored, descriptor),
                descriptor,
            })
            .collect()
    }

    /// The layout as `module.layout.read` answers it.
    pub(crate) fn view(&self, stored: Option<&StoredLayout>) -> LayoutView {
        let empty = StoredLayout::default();
        let stored = stored.unwrap_or(&empty);
        let toolsets = Toolset::ORDERABLE
            .into_iter()
            .map(|toolset| ToolsetOrder {
                toolset,
                order: owned(self.order(stored, toolset)),
                default: owned(self.default_order(toolset)),
            })
            .collect();
        let disabled = self
            .modules
            .iter()
            .filter(|module| !self.enabled(Some(stored), module))
            .map(|module| module.id.clone())
            .collect();
        let mut unrecognized: Vec<String> = stored
            .disabled
            .iter()
            .chain(&stored.order.adjustments)
            .chain(&stored.order.tools)
            .filter(|id| !self.registered(id))
            .cloned()
            .collect();
        unrecognized.sort();
        unrecognized.dedup();
        LayoutView {
            toolsets,
            disabled,
            unrecognized,
        }
    }

    /// Turn the registered module `id` on or off. A locked module is refused with its reason, and
    /// turning a module on removes its entry, since every module is on unless the person turned it
    /// off. Answers whether the stored layout changed.
    pub(crate) fn set_enabled(
        &self,
        stored: &mut StoredLayout,
        id: &str,
        enabled: bool,
    ) -> Result<bool, Error> {
        let module = self.module(id)?;
        if let Some(lock) = &module.locked {
            return Err(Error::validation(format!(
                "module {id} is locked and always on: {}",
                lock.reason
            )));
        }
        let listed = stored.disabled.iter().position(|disabled| disabled == id);
        match (enabled, listed) {
            (true, Some(at)) => {
                stored.disabled.remove(at);
                Ok(true)
            }
            (false, None) => {
                if stored.disabled.len() >= MAX_LAYOUT_IDS {
                    return Err(Error::resource_limit(format!(
                        "the layout holds at most {MAX_LAYOUT_IDS} modules turned off"
                    )));
                }
                stored.disabled.push(id.to_owned());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Store `order`, which must name each of `toolset`'s placements exactly once, as the person's
    /// order of that orderable toolset. Unrecognized ids stored in it keep their stored positions;
    /// an order that is the default stores only them. Answers whether the stored layout changed.
    pub(crate) fn set_order(
        &self,
        stored: &mut StoredLayout,
        toolset: Toolset,
        order: &[String],
    ) -> Result<bool, Error> {
        let default = self.default_order(toolset);
        let Some(previous) = stored.order.of_mut(toolset) else {
            return Err(not_orderable(toolset));
        };
        for (at, id) in order.iter().enumerate() {
            if !default.contains(&id.as_str()) {
                return Err(Error::validation(format!(
                    "order names {id}, which has no {} placement",
                    toolset.as_str()
                )));
            }
            if order[..at].contains(id) {
                return Err(Error::validation(format!("order names {id} twice")));
            }
        }
        if let Some(missing) = default
            .iter()
            .find(|id| !order.iter().any(|named| named == *id))
        {
            return Err(Error::validation(format!(
                "order leaves out {missing}: it names every {} placement exactly once",
                toolset.as_str()
            )));
        }
        let is_default = order.iter().map(String::as_str).eq(default.iter().copied());
        let kept = |id: &String| !default.contains(&id.as_str());
        let mut fresh = order.iter().filter(|_| !is_default);
        let mut next = Vec::with_capacity(previous.len().max(order.len()));
        for id in previous.iter() {
            if kept(id) {
                next.push(id.clone());
            } else if let Some(placed) = fresh.next() {
                next.push(placed.clone());
            }
        }
        next.extend(fresh.cloned());
        Ok(replace(previous, next))
    }

    /// Restore `toolset`'s order, or with none every order and every module's switch. What the
    /// stored layout names that this run does not register is kept. Answers whether the stored
    /// layout changed.
    pub(crate) fn reset(
        &self,
        stored: &mut StoredLayout,
        toolset: Option<Toolset>,
    ) -> Result<bool, Error> {
        let toolsets: &[Toolset] = match &toolset {
            Some(toolset) if !toolset.orderable() => return Err(not_orderable(*toolset)),
            Some(toolset) => std::slice::from_ref(toolset),
            None => &Toolset::ORDERABLE,
        };
        let mut changed = false;
        for toolset in toolsets {
            let default = self.default_order(*toolset);
            if let Some(previous) = stored.order.of_mut(*toolset) {
                let next = previous
                    .iter()
                    .filter(|id| !default.contains(&id.as_str()))
                    .cloned()
                    .collect();
                changed |= replace(previous, next);
            }
        }
        if toolset.is_none() {
            let next = stored
                .disabled
                .iter()
                .filter(|id| !self.registered(id))
                .cloned()
                .collect();
            changed |= replace(&mut stored.disabled, next);
        }
        Ok(changed)
    }
}

/// The stored layout as `preferences.json` keeps it: `None` once it holds no choice.
pub(crate) fn normalized(stored: StoredLayout) -> Option<StoredLayout> {
    (!stored.is_empty()).then_some(stored)
}

fn not_orderable(toolset: Toolset) -> Error {
    Error::validation(format!(
        "toolset {} cannot be reordered; the orderable toolsets are adjustments and tools",
        toolset.as_str()
    ))
}

fn owned(ids: Vec<&str>) -> Vec<String> {
    ids.into_iter().map(str::to_owned).collect()
}

/// Put `next` in `previous`, answering whether it differed.
fn replace(previous: &mut Vec<String>, next: Vec<String>) -> bool {
    if *previous == next {
        return false;
    }
    *previous = next;
    true
}

#[cfg(test)]
#[path = "module_layout_tests.rs"]
mod tests;
