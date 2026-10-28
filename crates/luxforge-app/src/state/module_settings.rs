//! Settings › Modules: the registered modules grouped by their home toolset, each group in the
//! person's order, with a switch or a lock per module, the selected module's detail, the reorder
//! by drag and by keyboard within a group, Reset order and Reset all, and the one-step Undo toast
//! ([design](../../../../docs/design/module-manager.md#desktop),
//! [sheet](../../../../docs/design/settings-and-flags.md#settings-surface)).
//!
//! The tab reads `module.list` and `module.layout.read` itself, as the sheet reads the flags, and
//! changes the layout only through `module.enable`, `module.disable`, `module.order.set` and
//! `module.layout.reset`, one write per gesture, adopting each answer. A change shows at once: the
//! layout drawn is the last answer with the writes still outstanding laid over it. Nothing here
//! changes a recipe, so the tab needs no draft. Every derivation is `O(m²)` in the module count at
//! most, and a closed tab derives nothing.
//!
//! External modules ([`super::external_modules`]) add `module.package.list` to the read: their
//! origin and state badges, the rows of modules this launch did not register (installed since,
//! or refused), Install module…'s review sheet, Uninstall…'s confirmation, Load from folder… and
//! Remove in developer mode, and the Relaunch to apply line.
use super::external_modules::{
    self, ExternalDetail, PackageList, PackageRow, PackageWrite, Review, ReviewModel, RowBadge,
    Uninstall, UninstallModel,
};
use luxforge_core::{
    EffectStage, ModuleDescriptor, SourceTag, Toolset,
    module_layout::{LayoutView, ListedModule, ModuleOrigin, ToolsetOrder},
};
use serde_json::{Value, json};
use std::collections::VecDeque;

/// What Modules says before its groups.
pub(crate) const INTRO: &str = "Choose which tools you see and where they appear, in every \
     catalog. Panel order never changes how a photo renders, and turning a module off never \
     removes its edits from a photo.";

/// The detail pane's sentence under a module's stages.
pub(crate) const ORDER_NOTE: &str = "Panel order doesn't change this.";

/// The host's own Mask entry: the strip's M, and the Masking group's summary row.
const HOST_MASKS: &str = "luxforge.masks";

/// The stages a photo renders through, in the order the detail pane names them.
const STAGES: [(EffectStage, &str); 7] = [
    (EffectStage::Source, "Source"),
    (EffectStage::Repair, "Repair"),
    (EffectStage::Restoration, "Restoration"),
    (EffectStage::Color, "Colour"),
    (EffectStage::Spatial, "Spatial"),
    (EffectStage::Geometry, "Geometry"),
    (EffectStage::Finish, "Finish"),
];

/// One read of what the tab shows: `module.list`'s modules and the host's descriptors beside them,
/// `module.layout.read`, and `module.package.list`'s external modules.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModulesRead {
    pub(crate) modules: Vec<ListedModule>,
    pub(crate) host: Vec<ModuleDescriptor>,
    pub(crate) layout: LayoutView,
    pub(crate) packages: PackageList,
}

impl ModulesRead {
    /// The read from the first two answers as the owner gave them, with no external module; the
    /// tab's read adds `module.package.list`'s with [`Self::with_packages`].
    pub(crate) fn parse(mut listed: Value, layout: Value) -> Result<Self, String> {
        let unreadable = |error: serde_json::Error| format!("unreadable modules: {error}");
        Ok(Self {
            modules: serde_json::from_value(listed["modules"].take()).map_err(unreadable)?,
            host: serde_json::from_value(listed["host"].take()).map_err(unreadable)?,
            layout: serde_json::from_value(layout)
                .map_err(|error| format!("unreadable module layout: {error}"))?,
            packages: PackageList::default(),
        })
    }

    /// The read with `module.package.list`'s answer.
    pub(crate) fn with_packages(mut self, packages: Value) -> Result<Self, String> {
        self.packages = PackageList::parse(packages)?;
        Ok(self)
    }

    /// The external module rows `module.list` does not list: installed since this launch, or read
    /// and not registered. They join Adjustments, the only home open to an external module.
    fn unregistered(&self) -> impl Iterator<Item = &PackageRow> {
        self.packages.modules.iter().filter(|row| {
            !self
                .modules
                .iter()
                .any(|listed| listed.descriptor.id == row.module)
        })
    }

    fn module(&self, id: &str) -> Option<&ModuleDescriptor> {
        self.modules
            .iter()
            .map(|listed| &listed.descriptor)
            .chain(&self.host)
            .find(|module| module.id == id)
    }

    fn origin(&self, id: &str) -> Option<ModuleOrigin> {
        self.modules
            .iter()
            .find(|listed| listed.descriptor.id == id)
            .map(|listed| listed.origin)
    }
}

/// One layout write: exactly one method call, answered with the layout it left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LayoutWrite {
    Enable(String),
    Disable(String),
    Order {
        toolset: Toolset,
        order: Vec<String>,
    },
    /// One toolset's order, or with `None` every order and every switch.
    Reset(Option<Toolset>),
}

impl LayoutWrite {
    pub(crate) fn method(&self) -> &'static str {
        match self {
            Self::Enable(_) => "module.enable",
            Self::Disable(_) => "module.disable",
            Self::Order { .. } => "module.order.set",
            Self::Reset(_) => "module.layout.reset",
        }
    }

    pub(crate) fn params(&self) -> Value {
        match self {
            Self::Enable(module) | Self::Disable(module) => json!({"module": module}),
            Self::Order { toolset, order } => {
                json!({"toolset": toolset.as_str(), "order": order})
            }
            Self::Reset(Some(toolset)) => json!({"toolset": toolset.as_str()}),
            Self::Reset(None) => json!({}),
        }
    }

    /// The module a switch write names, which a refusal is said against.
    pub(crate) fn module(&self) -> Option<&str> {
        match self {
            Self::Enable(module) | Self::Disable(module) => Some(module),
            _ => None,
        }
    }

    /// `layout` as the host answers it once this write lands. A write the host refuses is put
    /// right by the read that follows the refusal.
    fn apply(&self, layout: &mut LayoutView) {
        match self {
            Self::Enable(module) => layout.disabled.retain(|id| id != module),
            Self::Disable(module) => {
                if !layout.disabled.contains(module) {
                    layout.disabled.push(module.clone());
                }
            }
            Self::Order { toolset, order } => {
                if let Some(entry) = entry_mut(layout, *toolset) {
                    entry.order.clone_from(order);
                }
            }
            Self::Reset(toolset) => {
                for entry in &mut layout.toolsets {
                    if toolset.is_none_or(|toolset| toolset == entry.toolset) {
                        entry.order.clone_from(&entry.default);
                    }
                }
                if toolset.is_none() {
                    layout.disabled.clear();
                }
            }
        }
    }
}

fn entry_mut(layout: &mut LayoutView, toolset: Toolset) -> Option<&mut ToolsetOrder> {
    layout
        .toolsets
        .iter_mut()
        .find(|entry| entry.toolset == toolset)
}

fn entry(layout: &LayoutView, toolset: Toolset) -> Option<&ToolsetOrder> {
    layout
        .toolsets
        .iter()
        .find(|entry| entry.toolset == toolset)
}

/// One row of the tab: a module in one group. A module with interface in two toolsets, as Crop
/// has, has a row in each, and its switch on its home row.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RowKey {
    pub(crate) toolset: Toolset,
    pub(crate) module: String,
}

/// Where Settings opens Modules: at one module's home row, from a band's Customize panel… or a
/// palette entry, or at one group, from the strip's Customize tools….
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModulesFocus {
    Module(String),
    Toolset(Toolset),
}

/// A row held by its grip: the row, and the row of its group under the pointer, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModuleDrag {
    pub(crate) row: RowKey,
    pub(crate) over: Option<String>,
}

/// The one-step Undo toast: what the last change did, and the writes that put back what was
/// drawn before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UndoToast {
    pub(crate) text: String,
    pub(crate) undo: Vec<LayoutWrite>,
}

/// A keyboard move of the selected row within its group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Move {
    Up,
    Down,
    Top,
    Bottom,
}

/// The tab's own state, kept while the sheet is closed so it opens on what it last read.
#[derive(Clone, Debug, Default)]
pub(crate) struct ModulesTab {
    /// The last read, with each write's answer adopted into its layout.
    pub(crate) read: Option<ModulesRead>,
    /// A read is in flight.
    pub(crate) reading: bool,
    /// Why the last read failed, until one succeeds.
    pub(crate) read_error: Option<String>,
    /// The write in flight. Writes go one at a time, in the order they were made.
    pub(crate) writing: Option<LayoutWrite>,
    pub(crate) waiting: VecDeque<LayoutWrite>,
    pub(crate) selected: Option<RowKey>,
    /// Where the tab was opened at, until a read lets it choose the row.
    pub(crate) focus: Option<ModulesFocus>,
    pub(crate) drag: Option<ModuleDrag>,
    pub(crate) toast: Option<UndoToast>,
    /// A change refused, by module when it named one, with its reason, shown in the detail pane.
    pub(crate) refusal: Option<(Option<String>, String)>,
    /// This launch serves developer mode: Load from folder… and Remove are offered.
    pub(crate) developer: bool,
    /// The system file picker Install module… or Load from folder… opened is up.
    pub(crate) picking: bool,
    /// Install module…'s review sheet, from the package's inspection to its install.
    pub(crate) review: Option<Review>,
    /// Uninstall…'s confirmation.
    pub(crate) uninstall: Option<Uninstall>,
    /// The lifecycle write in flight. One goes at a time; a gesture while one is out sends
    /// nothing.
    pub(crate) package_write: Option<PackageWrite>,
    /// The last lifecycle write and what it answered, for the captured frame.
    pub(crate) answered: Option<Value>,
    /// What Relaunch did: the command a relaunch starts once the catalog has closed, or in an
    /// evidence run the command it would have started.
    pub(crate) relaunch: Option<Value>,
}

impl ModulesTab {
    /// The tab was left or the sheet closed: the toast goes, a held row is let go, a refusal is no
    /// longer said and the review sheet or confirmation is put away. The writes made still land,
    /// pending changes stay pending, and the selection is kept for the next opening.
    pub(crate) fn leave(&mut self) {
        self.toast = None;
        self.drag = None;
        self.refusal = None;
        self.review = None;
        self.uninstall = None;
    }

    /// No layout or lifecycle write is in flight or waiting.
    pub(crate) fn idle(&self) -> bool {
        self.writing.is_none() && self.waiting.is_empty() && self.package_write.is_none()
    }

    /// The review sheet or the uninstall confirmation is over the tab: it takes Escape and
    /// Return, and the list's keys wait.
    pub(crate) fn dialog(&self) -> bool {
        self.review.is_some() || self.uninstall.is_some()
    }

    /// Install module… can open its picker now: the store takes writes, nothing is in flight and
    /// no dialog or picker is up.
    pub(crate) fn can_install(&self) -> bool {
        self.read
            .as_ref()
            .is_some_and(|read| read.packages.writable())
            && self.package_write.is_none()
            && !self.dialog()
            && !self.picking
    }

    /// The external module row of `module`, as the last read listed it.
    pub(crate) fn package(&self, module: &str) -> Option<&PackageRow> {
        self.read.as_ref()?.packages.row(module)
    }

    /// The layout as drawn: the last answer with every outstanding write laid over it.
    pub(crate) fn layout(&self) -> Option<LayoutView> {
        let mut layout = self.read.as_ref()?.layout.clone();
        for write in self.writing.iter().chain(&self.waiting) {
            write.apply(&mut layout);
        }
        Some(layout)
    }

    /// Every row in the order the tab lists them, group by group.
    pub(crate) fn rows(&self) -> Vec<RowKey> {
        let (Some(read), Some(layout)) = (self.read.as_ref(), self.layout()) else {
            return Vec::new();
        };
        groups(read, &layout)
            .into_iter()
            .flat_map(|group| group.rows.into_iter().map(|row| row.key))
            .collect()
    }

    /// The row a focus names: its module's home row, or its group's first row.
    pub(crate) fn resolve(&self, focus: &ModulesFocus) -> Option<RowKey> {
        let read = self.read.as_ref()?;
        let rows = self.rows();
        match focus {
            ModulesFocus::Module(id) => {
                let home = read.module(id).map(|module| module.toolset);
                rows.iter()
                    .find(|row| &row.module == id && Some(row.toolset) == home)
                    .or_else(|| rows.iter().find(|row| &row.module == id))
                    .cloned()
            }
            ModulesFocus::Toolset(toolset) => rows.into_iter().find(|row| row.toolset == *toolset),
        }
    }

    /// The row `step` rows after the selected one, clamped to the list; the first row when none is
    /// selected.
    pub(crate) fn step_selection(&self, step: isize) -> Option<RowKey> {
        let rows = self.rows();
        let at = self
            .selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| row == selected));
        let next = match at {
            Some(at) => at
                .saturating_add_signed(step)
                .min(rows.len().saturating_sub(1)),
            None => 0,
        };
        rows.get(next).cloned()
    }

    /// Whether `row` can be moved: it is a placement in an orderable toolset's order.
    pub(crate) fn movable(&self, row: &RowKey) -> bool {
        self.layout().is_some_and(|layout| {
            row.toolset.orderable()
                && entry(&layout, row.toolset)
                    .is_some_and(|entry| entry.order.contains(&row.module))
        })
    }

    /// The write that moves `row` as `how` says, with the toast that names it, or `None` when it
    /// cannot move that way.
    pub(crate) fn moved(&self, row: &RowKey, how: Move) -> Option<(LayoutWrite, UndoToast)> {
        let layout = self.layout()?;
        let order = &entry(&layout, row.toolset)?.order;
        let from = order.iter().position(|id| id == &row.module)?;
        let to = match how {
            Move::Up => from.checked_sub(1)?,
            Move::Down => from + 1,
            Move::Top => 0,
            Move::Bottom => order.len().checked_sub(1)?,
        };
        self.moved_to(row, to)
    }

    /// The write that puts `row` where `target` is now in its group, as a drop on `target` does.
    pub(crate) fn dropped(&self, row: &RowKey, target: &str) -> Option<(LayoutWrite, UndoToast)> {
        let layout = self.layout()?;
        let to = entry(&layout, row.toolset)?
            .order
            .iter()
            .position(|id| id == target)?;
        self.moved_to(row, to)
    }

    fn moved_to(&self, row: &RowKey, to: usize) -> Option<(LayoutWrite, UndoToast)> {
        let read = self.read.as_ref()?;
        let layout = self.layout()?;
        let previous = entry(&layout, row.toolset)?.order.clone();
        let from = previous.iter().position(|id| id == &row.module)?;
        if to == from || to >= previous.len() {
            return None;
        }
        let mut order = previous.clone();
        let moved = order.remove(from);
        order.insert(to, moved);
        let title = |id: &str| row_title(read, row.toolset, id);
        let text = if to == 0 {
            format!("Moved {} to the top", title(&row.module))
        } else if to + 1 == order.len() {
            format!("Moved {} to the bottom", title(&row.module))
        } else if to < from {
            format!(
                "Moved {} above {}",
                title(&row.module),
                title(&order[to + 1])
            )
        } else {
            format!(
                "Moved {} below {}",
                title(&row.module),
                title(&order[to - 1])
            )
        };
        let toolset = row.toolset;
        Some((
            LayoutWrite::Order { toolset, order },
            UndoToast {
                text,
                undo: vec![LayoutWrite::Order {
                    toolset,
                    order: previous,
                }],
            },
        ))
    }

    /// The write that turns `module` on or off, with its toast; or the lock's reason, for a module
    /// that is always on.
    pub(crate) fn switched(
        &self,
        module: &str,
        on: bool,
    ) -> Result<Option<(LayoutWrite, UndoToast)>, String> {
        let (Some(read), Some(layout)) = (self.read.as_ref(), self.layout()) else {
            return Ok(None);
        };
        let Some(descriptor) = read.module(module) else {
            return Ok(None);
        };
        if let Some(lock) = &descriptor.locked {
            return Err(format!(
                "{} is always on: {}",
                descriptor.title, lock.reason
            ));
        }
        let is_on = !layout.disabled.iter().any(|id| id == module);
        if is_on == on {
            return Ok(None);
        }
        let (write, undo, word) = if on {
            (
                LayoutWrite::Enable(module.to_owned()),
                LayoutWrite::Disable(module.to_owned()),
                "on",
            )
        } else {
            (
                LayoutWrite::Disable(module.to_owned()),
                LayoutWrite::Enable(module.to_owned()),
                "off",
            )
        };
        Ok(Some((
            write,
            UndoToast {
                text: format!("Turned {} {word}", descriptor.title),
                undo: vec![undo],
            },
        )))
    }

    /// The selected module turned the other way, as Space and a row's switch do.
    pub(crate) fn toggled(&self, module: &str) -> Result<Option<(LayoutWrite, UndoToast)>, String> {
        let on = self
            .layout()
            .is_some_and(|layout| layout.disabled.iter().any(|id| id == module));
        self.switched(module, on)
    }

    /// The write that restores `toolset`'s order, or everything, with the toast whose Undo writes
    /// back what was drawn; `None` when there is nothing to restore.
    pub(crate) fn reset(&self, toolset: Option<Toolset>) -> Option<(LayoutWrite, UndoToast)> {
        let layout = self.layout()?;
        let mut undo: Vec<LayoutWrite> = layout
            .toolsets
            .iter()
            .filter(|entry| toolset.is_none_or(|toolset| toolset == entry.toolset))
            .filter(|entry| entry.order != entry.default)
            .map(|entry| LayoutWrite::Order {
                toolset: entry.toolset,
                order: entry.order.clone(),
            })
            .collect();
        if toolset.is_none() {
            undo.extend(layout.disabled.iter().cloned().map(LayoutWrite::Disable));
        }
        if undo.is_empty() {
            return None;
        }
        let text = match toolset {
            Some(toolset) => format!("Reset the {} order", group_label(toolset)),
            None => "Reset every module".to_owned(),
        };
        Some((LayoutWrite::Reset(toolset), UndoToast { text, undo }))
    }
}

/// How a row's trailing control is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowControl {
    /// The module's one switch, on its home row.
    Switch(bool),
    /// The home row's switch, dimmed, on a row in another group that follows it.
    Follows(bool),
    /// Always on, with the reason.
    Lock(String),
    /// None: an external module this launch did not register has no switch until it does.
    None,
}

/// Where the accent line marks the dragged row landing on this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropSide {
    Above,
    Below,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModuleRow {
    pub(crate) key: RowKey,
    pub(crate) title: String,
    /// A one-line qualifier: "RAW photos only", or whose switch a following row shows.
    pub(crate) qualifier: Option<String>,
    /// A strip tool's letter.
    pub(crate) letter: Option<String>,
    pub(crate) control: RowControl,
    /// The row can be dragged by its grip and moved with ⌥↑ and ⌥↓.
    pub(crate) grip: bool,
    pub(crate) selected: bool,
    /// The module is off: the row dims and keeps its place.
    pub(crate) off: bool,
    /// This row is held by its grip.
    pub(crate) dragged: bool,
    /// The held row lands here.
    pub(crate) drop: Option<DropSide>,
    /// A drag of a row in this group is under way, so the pointer entering this row names it.
    pub(crate) drop_target: bool,
    /// An external module's origin badge, then its state badge.
    pub(crate) badges: Vec<RowBadge>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModuleGroup {
    pub(crate) toolset: Toolset,
    pub(crate) label: &'static str,
    pub(crate) caption: &'static str,
    /// Reset order is drawn, enabled when the order differs from the default.
    pub(crate) reorderable: bool,
    pub(crate) can_reset: bool,
    pub(crate) rows: Vec<ModuleRow>,
}

/// The selected module's detail pane.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModuleDetail {
    pub(crate) title: String,
    /// The origin badge: Built in, Community or From folder, or Part of Luxforge for the host.
    pub(crate) origin: &'static str,
    pub(crate) hint: Option<String>,
    /// For a locked module, why it is always on.
    pub(crate) shown: Option<String>,
    /// One line per placement: the group, and its place there.
    pub(crate) lives: Vec<String>,
    /// The seven stages, each with whether the module renders in it.
    pub(crate) stages: Vec<(&'static str, bool)>,
    /// What it renders, said over the stages.
    pub(crate) renders: String,
    /// What turning it off does to the photos that use it, for a module that can be turned off.
    pub(crate) photos: Option<String>,
    /// The API methods it adds.
    pub(crate) api: Vec<String>,
    /// Why the last change to it was refused.
    pub(crate) refusal: Option<String>,
    /// An external module's version, program, state, folder and actions.
    pub(crate) external: Option<ExternalDetail>,
}

/// The tab as drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ModulesModel {
    pub(crate) groups: Vec<ModuleGroup>,
    pub(crate) detail: Option<ModuleDetail>,
    pub(crate) toast: Option<String>,
    pub(crate) can_reset_all: bool,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    /// A refused change that names no module.
    pub(crate) refusal: Option<String>,
    /// Stored choices for modules this run does not register, which are kept.
    pub(crate) unrecognized: Vec<String>,
    pub(crate) dragging: bool,
    /// Install module… is enabled.
    pub(crate) install: bool,
    /// Load from folder… is drawn, in developer mode, and enabled when `install` is.
    pub(crate) load_folder: bool,
    /// The line under the header while changes wait for the next launch, with Relaunch.
    pub(crate) pending: Option<String>,
    /// Why nothing can be installed, or why the launch loaded no external module.
    pub(crate) store_problem: Option<String>,
    pub(crate) review: Option<ReviewModel>,
    pub(crate) uninstall: Option<UninstallModel>,
}

/// The tab for `tab`.
pub(crate) fn derive(tab: &ModulesTab) -> ModulesModel {
    let mut model = ModulesModel {
        loading: tab.read.is_none() && tab.read_error.is_none(),
        error: tab.read_error.clone(),
        toast: tab.toast.as_ref().map(|toast| toast.text.clone()),
        refusal: tab
            .refusal
            .as_ref()
            .filter(|(module, _)| module.is_none())
            .map(|(_, reason)| reason.clone()),
        dragging: tab.drag.is_some(),
        review: tab.review.as_ref().map(external_modules::derive_review),
        uninstall: tab
            .uninstall
            .as_ref()
            .map(external_modules::derive_uninstall),
        ..ModulesModel::default()
    };
    let (Some(read), Some(layout)) = (tab.read.as_ref(), tab.layout()) else {
        return model;
    };
    model.install = tab.can_install();
    model.load_folder = tab.developer;
    model.store_problem = read.packages.problem();
    model.pending = match read.packages.pending() {
        0 => None,
        1 => Some("1 change applies when Luxforge relaunches.".to_owned()),
        count => Some(format!("{count} changes apply when Luxforge relaunches.")),
    };
    model.can_reset_all = tab.reset(None).is_some();
    model.unrecognized = layout.unrecognized.clone();
    let drag = tab.drag.as_ref();
    let dragged_order = drag.and_then(|drag| {
        entry(&layout, drag.row.toolset).map(|entry| (drag, entry.order.as_slice()))
    });
    model.groups = groups(read, &layout)
        .into_iter()
        .map(|mut group| {
            for row in &mut group.rows {
                row.selected = tab.selected.as_ref() == Some(&row.key);
                let Some((drag, order)) = dragged_order
                    .filter(|(drag, _)| drag.row.toolset == row.key.toolset && row.grip)
                else {
                    continue;
                };
                row.drop_target = true;
                row.dragged = drag.row == row.key;
                let position = |id: &str| order.iter().position(|placed| placed == id);
                if drag.over.as_deref() == Some(row.key.module.as_str()) {
                    row.drop = match (position(&drag.row.module), position(&row.key.module)) {
                        (Some(from), Some(to)) if to < from => Some(DropSide::Above),
                        (Some(from), Some(to)) if to > from => Some(DropSide::Below),
                        _ => None,
                    };
                }
            }
            group
        })
        .collect();
    model.detail = tab
        .selected
        .as_ref()
        .and_then(|selected| detail(tab, read, &layout, selected));
    model
}

/// The groups, in the order Settings lists them, each with its rows in the person's order, before
/// selection and drag are drawn on them.
fn groups(read: &ModulesRead, layout: &LayoutView) -> Vec<ModuleGroup> {
    Toolset::ALL
        .into_iter()
        .map(|toolset| {
            let reorderable = toolset.orderable();
            let order = entry(layout, toolset);
            let mut rows = Vec::new();
            // The host's own entries come first: the strip's Mask, which the strip fixes in place,
            // and the masks' summary row.
            if let Some(host) = read
                .host
                .iter()
                .find(|module| module.id == HOST_MASKS)
                .filter(|_| matches!(toolset, Toolset::Tools | Toolset::Masking))
            {
                rows.push(host_row(host, toolset));
            }
            let placed = |module: &ListedModule| {
                module
                    .placements
                    .iter()
                    .any(|placement| placement.toolset == toolset)
            };
            // An orderable group in the person's order; the others in registry order. A module whose
            // home declares no placement there still has its home row, after the order, so its
            // switch is always reachable.
            let ordered: Vec<&ListedModule> = match order {
                Some(order) => order
                    .order
                    .iter()
                    .filter_map(|id| {
                        read.modules
                            .iter()
                            .find(|module| &module.descriptor.id == id)
                    })
                    .collect(),
                None => read
                    .modules
                    .iter()
                    .filter(|module| placed(module))
                    .collect(),
            };
            let homeless = read.modules.iter().filter(|module| {
                module.descriptor.toolset == toolset
                    && !ordered
                        .iter()
                        .any(|placed| placed.descriptor.id == module.descriptor.id)
            });
            for module in ordered.iter().copied().chain(homeless) {
                let grip = order.is_some_and(|order| order.order.contains(&module.descriptor.id));
                let mut row = module_row(module, toolset, layout, grip);
                row.badges = external_modules::row_badges(
                    module.origin,
                    read.packages.row(&module.descriptor.id),
                );
                rows.push(row);
            }
            // An external module this launch did not register keeps its place at the group's end,
            // with no switch or grip until a launch registers it.
            if toolset == Toolset::Adjustments {
                rows.extend(read.unregistered().map(unregistered_row));
            }
            let can_reset = order.is_some_and(|order| order.order != order.default);
            ModuleGroup {
                toolset,
                label: group_label(toolset),
                caption: group_caption(toolset),
                reorderable,
                can_reset,
                rows,
            }
        })
        .filter(|group| !group.rows.is_empty())
        .collect()
}

fn host_row(host: &ModuleDescriptor, toolset: Toolset) -> ModuleRow {
    let tools = toolset == Toolset::Tools;
    ModuleRow {
        key: RowKey {
            toolset,
            module: host.id.clone(),
        },
        title: if tools { "Mask" } else { &host.title }.to_owned(),
        qualifier: Some("Part of Luxforge".to_owned()),
        letter: tools.then(|| "M".to_owned()),
        control: RowControl::Lock(
            host.locked
                .as_ref()
                .map(|lock| lock.reason.clone())
                .unwrap_or_default(),
        ),
        grip: false,
        selected: false,
        off: false,
        dragged: false,
        drop: None,
        drop_target: false,
        badges: Vec::new(),
    }
}

fn module_row(
    module: &ListedModule,
    toolset: Toolset,
    layout: &LayoutView,
    grip: bool,
) -> ModuleRow {
    let descriptor = &module.descriptor;
    let home = descriptor.toolset == toolset;
    let on = descriptor.locked.is_some() || !layout.disabled.contains(&descriptor.id);
    let title = placement_title(descriptor, toolset);
    let qualifier = if home {
        source_qualifier(descriptor)
    } else if title != descriptor.title {
        Some(format!("of {}", descriptor.title))
    } else {
        Some(format!("switch in {}", group_label(descriptor.toolset)))
    };
    let control = match &descriptor.locked {
        Some(lock) => RowControl::Lock(lock.reason.clone()),
        None if home => RowControl::Switch(on),
        None => RowControl::Follows(on),
    };
    ModuleRow {
        key: RowKey {
            toolset,
            module: descriptor.id.clone(),
        },
        title,
        qualifier,
        letter: (toolset == Toolset::Tools)
            .then(|| descriptor.canvas.as_ref()?.shortcut().map(str::to_owned))
            .flatten(),
        control,
        grip,
        selected: false,
        off: !on,
        dragged: false,
        drop: None,
        drop_target: false,
        badges: Vec::new(),
    }
}

/// The row of an external module this launch did not register: installed or loaded since it
/// started, or read and refused. Its title is the one the store or the launch knows.
fn unregistered_row(row: &PackageRow) -> ModuleRow {
    ModuleRow {
        key: RowKey {
            toolset: Toolset::Adjustments,
            module: row.module.clone(),
        },
        title: row.title(),
        qualifier: None,
        letter: None,
        control: RowControl::None,
        grip: false,
        selected: false,
        off: false,
        dragged: false,
        drop: None,
        drop_target: false,
        badges: external_modules::row_badges(row.origin, Some(row)),
    }
}

/// A row's title: a strip tool's own name in Tools, the module's title elsewhere.
fn placement_title(module: &ModuleDescriptor, toolset: Toolset) -> String {
    match (&module.canvas, toolset) {
        (Some(canvas), Toolset::Tools) => canvas.title().to_owned(),
        _ => module.title.clone(),
    }
}

fn row_title(read: &ModulesRead, toolset: Toolset, id: &str) -> String {
    read.module(id)
        .map_or_else(|| id.to_owned(), |module| placement_title(module, toolset))
}

/// "RAW photos only" for a module that applies to RAW photos and not JPEGs, and the reverse; a
/// developer proof says so.
fn source_qualifier(module: &ModuleDescriptor) -> Option<String> {
    if module.developer {
        return Some("Developer".to_owned());
    }
    match (
        module.applies_to(SourceTag::Raw),
        module.applies_to(SourceTag::Jpeg),
    ) {
        (true, false) => Some("RAW photos only".to_owned()),
        (false, true) => Some("JPEG photos only".to_owned()),
        _ => None,
    }
}

pub(crate) fn group_label(toolset: Toolset) -> &'static str {
    match toolset {
        Toolset::Adjustments => "Adjustments",
        Toolset::Corrections => "Corrections",
        Toolset::Tools => "Tools",
        Toolset::Masking => "Masking",
        Toolset::Processing => "Processing only",
    }
}

fn group_caption(toolset: Toolset) -> &'static str {
    match toolset {
        Toolset::Adjustments => "tools panel",
        Toolset::Corrections => "band kinds \u{b7} fixed order",
        Toolset::Tools => "mode strip",
        Toolset::Masking => "New mask kinds \u{b7} fixed order",
        Toolset::Processing => "no panel",
    }
}

/// `n` (from 1) as an English ordinal.
fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn detail(
    tab: &ModulesTab,
    read: &ModulesRead,
    layout: &LayoutView,
    selected: &RowKey,
) -> Option<ModuleDetail> {
    let Some(module) = read.module(&selected.module) else {
        // An external module this launch did not register: what the store and the launch say of
        // it, with no placement, stages or methods until a launch registers it.
        let row = read.packages.row(&selected.module)?;
        return Some(ModuleDetail {
            title: row.title(),
            origin: external_modules::origin_badge(row.origin).unwrap_or("Built in"),
            hint: None,
            shown: None,
            lives: Vec::new(),
            stages: Vec::new(),
            renders: String::new(),
            photos: None,
            api: Vec::new(),
            refusal: tab
                .refusal
                .as_ref()
                .filter(|(refused, _)| refused.as_deref() == Some(row.module.as_str()))
                .map(|(_, reason)| reason.clone()),
            external: Some(external_modules::external_detail(row, tab.developer)),
        });
    };
    let host = module.id == HOST_MASKS;
    // Where it lives: each group listing it, with its place in an ordered one.
    let lives = groups(read, layout)
        .into_iter()
        .filter_map(|group| {
            let at = group
                .rows
                .iter()
                .position(|row| row.key.module == module.id)?;
            let place = if group.reorderable {
                if at + 1 == group.rows.len() && at > 0 {
                    ", last".to_owned()
                } else {
                    format!(", {}", ordinal(at + 1))
                }
            } else {
                String::new()
            };
            Some(format!("{} \u{b7} {}{place}", group.label, group.caption))
        })
        .collect();
    let stages: Vec<(&'static str, bool)> = STAGES
        .iter()
        .map(|(stage, name)| {
            (
                *name,
                module.effects.iter().any(|effect| effect.stage == *stage),
            )
        })
        .collect();
    let renders = if module.effects.is_empty() {
        "It renders nothing of its own.".to_owned()
    } else {
        let names: Vec<&str> = stages
            .iter()
            .filter(|(_, own)| *own)
            .map(|(name, _)| *name)
            .collect();
        format!("In {}. {ORDER_NOTE}", names.join(" and "))
    };
    let mut api: Vec<String> = module
        .actions
        .iter()
        .map(|action| method_name("edit.", &action.id))
        .chain(
            module
                .queries
                .iter()
                .map(|query| method_name("query.", &query.id)),
        )
        .chain(module.tasks.iter().map(|task| format!("task.{}", task.id)))
        .collect();
    api.dedup();
    let movable = !host
        && Toolset::ORDERABLE.into_iter().any(|toolset| {
            entry(layout, toolset).is_some_and(|entry| entry.order.contains(&module.id))
        });
    Some(ModuleDetail {
        title: module.title.clone(),
        origin: if host {
            "Part of Luxforge"
        } else {
            match read.origin(&module.id).unwrap_or_default() {
                ModuleOrigin::BuiltIn => "Built in",
                ModuleOrigin::Community => "Community",
                ModuleOrigin::Folder => "From folder",
            }
        },
        hint: module.hint.clone(),
        shown: module.locked.as_ref().map(|lock| {
            if movable {
                format!("Always on: {}. You can still move it.", lock.reason)
            } else {
                format!("Always on: {}.", lock.reason)
            }
        }),
        lives,
        stages,
        renders,
        photos: module.locked.is_none().then(|| {
            format!(
                "Keep their {} edits. Off hides it from the workspace only: those edits render \
                 and export as before.",
                module.title
            )
        }),
        api,
        refusal: tab
            .refusal
            .as_ref()
            .filter(|(refused, _)| refused.as_deref() == Some(module.id.as_str()))
            .map(|(_, reason)| reason.clone()),
        external: read
            .packages
            .row(&module.id)
            .map(|row| external_modules::external_detail(row, tab.developer)),
    })
}

/// A declared id as the method a client calls: a module's `edit.` or `query.` method, or a host
/// command, which declares its whole method name.
fn method_name(prefix: &str, id: &str) -> String {
    if id.contains('.') {
        id.to_owned()
    } else {
        format!("{prefix}{id}")
    }
}

#[cfg(test)]
#[path = "module_settings_tests.rs"]
mod tests;
