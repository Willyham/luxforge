//! The view model: pure functions from core state to plain data. Nothing here draws, allocates a
//! texture or calls the owner, and no framework type appears in any model, so every rule the screen
//! follows is testable without a window.
pub(crate) mod canvas;
pub(crate) mod capabilities;
pub(crate) mod control_tree;
pub(crate) mod fields;
pub(crate) mod histogram;
pub(crate) mod masks;
pub(crate) mod number;
pub(crate) mod palette;
pub(crate) mod panel;
pub(crate) mod performance;
pub(crate) mod presets;
pub(crate) mod status;
#[cfg(test)]
pub(crate) mod testing;
pub(crate) mod title;
pub(crate) mod tools;
pub(crate) mod tracked;

use crate::{crop_draft::CropDraft, mask_draft::MaskDraft};
use fields::Fields;
use luxforge_core::{
    ClientSession, ComponentId, ComponentMode, EditorState, EntryId, HistoryPage, MaskId,
    ModuleDescriptor, RecipeDescription, Version, mask::commands::MaskListing,
};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
};

/// The actor this desktop records on every request it sends, which is how its own history entries
/// are told from another client's.
pub(crate) const ACTOR: &str = "desktop";

/// What an inline menu was opened on. Menus carry no state of their own.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuTarget {
    /// A saved version's chip: Delete.
    Version(String),
    /// A generated control: Copy as JSON request. `parameter` names the one field a control of a
    /// patch action submits, so the copied request is exactly what that control would send, and
    /// `method` is the method that request carries, read from the host's command table once, when
    /// the menu is opened ([`MenuTarget::control`]).
    Control {
        action: String,
        parameter: Option<String>,
        preset: Option<Map<String, Value>>,
        method: String,
    },
    /// The open crop draft's own Apply: Copy as JSON request for its current values.
    Draft,
    /// A module's picker control: Copy as JSON request for the `workspace.set` a click sends.
    Mode(String),
    /// A module's task control: Copy as JSON request for the `task.<id>` a press sends.
    Task { module_id: String, task: String },
    /// A library preset's row, by its identity: Delete, Export and, for an imported preset, Copy
    /// import report.
    Preset(String),
    /// A mask's row in the Masks panel, by its identity: Rename, Duplicate, Invert, Move up, Move
    /// down, Delete and Copy as JSON request.
    Mask(String),
    /// The same menu, opened from the open mask's group rule and dropped under it.
    OpenMask(String),
    /// A mask's Copy as JSON request: the request of every edit its menu offers.
    MaskCopy(String),
    /// The same, opened from the open mask's group rule.
    OpenMaskCopy(String),
    /// A component's row in the open mask, by its identity: Rename, Edit shape or Paint more, Move
    /// up, Move down, Delete and Copy as JSON request.
    Component(String),
    /// A component's Copy as JSON request: the request of every control on its row and in its menu.
    ComponentCopy(String),
    /// The New mask kind menu.
    NewMask,
    /// The Add component kind menu.
    AddComponent,
    /// One sampled colour of a component, by its position: Remove and Copy as JSON request.
    Swatch { component: String, index: usize },
    /// One stroke of a brush component, by its content address: Delete and Copy as JSON request.
    Stroke { component: String, stroke: String },
    /// The title bar's Export button: Export JPEG and Export JPEG, keep metadata.
    Export,
}

/// The open slider gesture as the models read it: the control it drafts and whether its core draft
/// is conflicted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SliderDrafting<'a> {
    pub(crate) action: &'a str,
    pub(crate) parameter: &'a str,
    pub(crate) conflicted: bool,
}

/// The stamps of the inputs too large to compare on every message: each moves whenever its value
/// may have changed ([`tracked::Tracked`]), so a section built from them knows in one comparison
/// whether to build again. The session is stamped by the editor, which compares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Stamps {
    pub(crate) modules: u64,
    pub(crate) history: u64,
    pub(crate) versions: u64,
    pub(crate) lineage: u64,
    pub(crate) recipe: u64,
    pub(crate) current_recipe: u64,
    pub(crate) masks: u64,
    pub(crate) hidden_masks: u64,
    pub(crate) fields: u64,
    pub(crate) controls: u64,
    pub(crate) expanded: u64,
    pub(crate) menu: u64,
    pub(crate) presets: u64,
    pub(crate) preset_form: u64,
    pub(crate) capabilities: u64,
    pub(crate) session: u64,
}

#[cfg(test)]
impl Stamps {
    /// Stamps no section has been built from, so every section builds: what a derivation from
    /// inputs nobody tracks — a test's scene — must do.
    pub(crate) fn fresh() -> Self {
        let stamp = tracked::stamp;
        Self {
            modules: stamp(),
            history: stamp(),
            versions: stamp(),
            lineage: stamp(),
            recipe: stamp(),
            current_recipe: stamp(),
            masks: stamp(),
            hidden_masks: stamp(),
            fields: stamp(),
            controls: stamp(),
            expanded: stamp(),
            menu: stamp(),
            presets: stamp(),
            preset_form: stamp(),
            capabilities: stamp(),
            session: stamp(),
        }
    }
}

/// Why a start that needs the editable state is refused with no photograph open.
pub(crate) const NO_PHOTOGRAPH: &str = "No photograph is open";
/// Why a start that needs the editable state is refused while a history entry is previewed.
pub(crate) const NOT_CURRENT: &str = "Return to the current state before editing";
/// Why a start that waits for requests is refused while one is in flight.
pub(crate) const IN_FLIGHT: &str = "Waiting for the last request";

/// The editable state, as one rule: a photograph is open ([`NO_PHOTOGRAPH`]) and the current state,
/// not a previewed entry, is on screen ([`NOT_CURRENT`]). `None` is editable. The app's one start
/// refusal (`Editor::gesture_refusal`) asks this for every start that takes its editable half, and
/// [`edit_refusal`] adds the busy half for the models.
pub(crate) fn editable_refusal(
    state: Option<&EditorState>,
    session: &ClientSession,
) -> Option<&'static str> {
    if state.is_none() {
        return Some(NO_PHOTOGRAPH);
    }
    (!session.preview.can_edit()).then_some(NOT_CURRENT)
}

/// Why an edit cannot start now, in the words the status bar and a disabled section use: the
/// editable state ([`editable_refusal`]), then no request in flight ([`IN_FLIGHT`]). `None` is
/// editable. It is what the app refuses an edit with (a start taking the editable and busy halves)
/// when no draft is open, computed once per derivation into [`Inputs::edit_refusal`] for every
/// model that reads it.
pub(crate) fn edit_refusal(
    state: Option<&EditorState>,
    session: &ClientSession,
    busy: bool,
) -> Option<String> {
    editable_refusal(state, session)
        .or(busy.then_some(IN_FLIGHT))
        .map(str::to_owned)
}

/// Everything the models are derived from, borrowed for one derivation.
pub(crate) struct Inputs<'a> {
    /// Whether the large inputs below may have changed since a section was last built.
    pub(crate) stamps: Stamps,
    pub(crate) state: Option<&'a EditorState>,
    pub(crate) history: &'a HistoryPage,
    pub(crate) versions: &'a [Version],
    /// Entries on the current undo-parent chain; other loaded entries are abandoned branches.
    pub(crate) lineage: &'a HashSet<EntryId>,
    /// Oldest lineage sequence when the chain was truncated; entries at or below it are unknown.
    pub(crate) lineage_floor: Option<u64>,
    pub(crate) display_entry: Option<&'a EntryId>,
    pub(crate) modules: &'a [ModuleDescriptor],
    pub(crate) modules_ready: bool,
    /// The displayed entry's layers as the owner described them. The idle crop section reads the
    /// committed crop, and the stage it receives, from these rows.
    pub(crate) recipe: Option<&'a RecipeDescription>,
    /// The current entry's layers as the owner described them, whichever entry is displayed. A
    /// section's edited dot reads each row's `neutral`, the core's own answer.
    pub(crate) current_recipe: Option<&'a RecipeDescription>,
    pub(crate) fields: &'a Fields,
    /// Local presentation state for generated controls; it never enters the recipe.
    pub(crate) control_ui: &'a tools::ControlsUi,
    /// The (action, parameter) whose value is being typed.
    pub(crate) editing: Option<&'a (String, String)>,
    /// The (action, parameter) whose slider is being dragged.
    pub(crate) dragging: Option<&'a (String, String)>,
    /// Sections the person collapsed or expanded; everything else follows the default.
    pub(crate) expanded: &'a BTreeMap<String, bool>,
    /// The open slider gesture, when a drafting control is being moved.
    pub(crate) slider_draft: Option<SliderDrafting<'a>>,
    /// The open slider, mask or crop gesture's core draft is conflicted: something else committed
    /// since it was based, and its commit waits for Discard or Reapply.
    pub(crate) gesture_conflicted: bool,
    /// What the open gesture is called — a slider draft, a mask gesture or a crop draft — for the
    /// one Changed elsewhere notice every kind shares.
    pub(crate) gesture: Option<&'static str>,
    /// Why the open gesture cannot be applied now: the app's one release refusal, which Enter,
    /// a pointer release and every Apply button answer to. A button reads this rather than a rule
    /// of its own, so it never reads enabled while the app would refuse the request.
    pub(crate) apply_refusal: Option<String>,
    /// Why a preset cannot be applied, why the components gallery cannot open, and why Undo, Redo
    /// and Restore cannot run, while this client's one draft is held — the one refusal every such
    /// start answers to.
    pub(crate) preset_refusal: Option<String>,
    pub(crate) gallery_refusal: Option<String>,
    pub(crate) history_refusal: Option<String>,
    /// Why nothing can be edited now ([`edit_refusal`]), computed once per derivation: the title
    /// bar's Undo and Redo, the canvas's modes and picks, every module section and the Masks panel
    /// read this answer rather than a rule of their own.
    pub(crate) edit_refusal: Option<String>,
    pub(crate) draft: Option<&'a CropDraft>,
    /// The masks of the displayed entry as `mask.list` last answered them.
    pub(crate) masks: Option<&'a MaskListing>,
    /// The mask the Masks panel has open, and the component selected inside it. Per-client
    /// selection: it commits nothing and appears in no recipe.
    pub(crate) selected_mask: Option<&'a MaskId>,
    pub(crate) selected_component: Option<&'a ComponentId>,
    /// The component row the pointer is over. While it lasts the overlay shows that component's own
    /// contribution instead of the composed mask, which is what makes a subtract legible.
    pub(crate) hovered_component: Option<&'a ComponentId>,
    /// Masks whose overlay the eye has hidden. View state: a hidden mask still applies to the
    /// picture, because hiding an edit and hiding its indicator are different things.
    pub(crate) hidden_masks: &'a HashSet<MaskId>,
    /// Every mask's coverage thumbnail, as the thumbnail worker last delivered them.
    pub(crate) thumbnails: &'a masks::MaskThumbnails,
    /// The open mask shape gesture.
    pub(crate) mask_draft: Option<&'a MaskDraft>,
    /// The mode the next Add-component gesture will use.
    pub(crate) mask_mode: ComponentMode,
    /// The brush the next stroke will be drawn with, and whether the erase modifier is held.
    pub(crate) brush: crate::mask_draft::Brush,
    pub(crate) brush_erase_held: bool,
    /// The Masks panel's one text field while it is open: a rename in place, a gesture field or a
    /// brush number being typed.
    pub(crate) mask_typing: Option<&'a masks::MaskTyping>,
    /// The Masks band is collapsed.
    pub(crate) masks_collapsed: bool,
    /// A reorder by drag in progress in the Masks panel.
    pub(crate) mask_drag: Option<&'a masks::MaskDrag>,
    /// The mask the generated module sections are bound to, which is what a masked slider edits.
    /// `None` binds them to the global layer, as they have always been.
    pub(crate) target: Option<&'a MaskId>,
    /// The draft's own input stage is on the GPU and the current state is shown.
    pub(crate) drafting: bool,
    pub(crate) crop_custom: (&'a str, &'a str),
    pub(crate) crop_guide: bool,
    pub(crate) crop_option: bool,
    pub(crate) crop_space: bool,
    pub(crate) session: &'a ClientSession,
    pub(crate) status: &'a str,
    pub(crate) busy: bool,
    pub(crate) can_open: bool,
    /// An export can start: the app's export refusal has nothing to say.
    pub(crate) can_export: bool,
    /// Developer mode is active (debug build or `--developer`), so diagnostic UI is listed.
    pub(crate) developer: bool,
    pub(crate) compare_held: bool,
    pub(crate) scale_factor: f32,
    pub(crate) zoom: &'a str,
    /// The zoom field is open for typing in the title bar's view control.
    pub(crate) zoom_editing: bool,
    /// The window's logical size, which with the panels and the display scale decides what Fit
    /// comes to as a percentage.
    pub(crate) window: (f32, f32),
    pub(crate) fullscreen: bool,
    pub(crate) version_name: &'a str,
    /// The "+" chip has revealed the version-naming field.
    pub(crate) version_form_open: bool,
    pub(crate) dimensions: Option<(u32, u32)>,
    /// A preview is on the GPU.
    pub(crate) photo: bool,
    /// Live API clients, or `None` when the local server could not start on this host.
    pub(crate) clients: Option<usize>,
    /// A preview job is in flight or its pixels are still being uploaded.
    pub(crate) rendering: bool,
    /// How long the frame on the photo surface took to render, measured on the preview worker for
    /// that frame's own phase. `None` before any frame is on screen.
    pub(crate) render: Option<status::RenderTime>,
    /// The last preview failure, cleared by the next successful upload.
    pub(crate) render_error: Option<&'a luxforge_core::Error>,
    pub(crate) pointer: Option<(u32, u32)>,
    /// The report the desktop's own preview worker reduced for the displayed frame, with the
    /// identity and generation it arrived under. `None` before the first one arrives.
    pub(crate) analysis: Option<&'a histogram::Analysis>,
    /// A newer generation is in flight, so the report above is one frame behind.
    pub(crate) analysis_updating: bool,
    /// The pixel `render.sample` last answered for the pointer's position.
    pub(crate) readout: Option<&'a histogram::Readout>,
    pub(crate) menu: Option<&'a MenuTarget>,
    pub(crate) palette_open: bool,
    pub(crate) palette_query: &'a str,
    pub(crate) palette_selected: usize,
    /// The preset library as `preset.list` last answered it.
    pub(crate) presets: &'a presets::PresetLibrary,
    /// The Presets section's create form.
    pub(crate) preset_form: &'a presets::PresetForm,
    /// What the desktop knows about every capability-declaring module, and the open consent
    /// notice.
    pub(crate) capabilities: &'a capabilities::CapabilityStore,
    /// The Performance section is expanded, which is local to this client and this launch.
    pub(crate) performance_expanded: bool,
    /// What the Performance section's sampler has read since it last started sampling.
    pub(crate) performance: &'a performance::PerformanceHistory,
}

/// The whole screen as plain data. The tools panel keeps its sections across derivations so an
/// untouched module is not rebuilt.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Workspace {
    pub(crate) title: title::TitleBarModel,
    pub(crate) panel: panel::StatePanelModel,
    pub(crate) canvas: canvas::CanvasModel,
    pub(crate) tools: tools::ToolsModel,
    pub(crate) masks: masks::MasksModel,
    pub(crate) histogram: histogram::HistogramModel,
    pub(crate) status: status::StatusBarModel,
    pub(crate) palette: palette::PaletteModel,
    /// The state panel's pinned last block. It keeps itself across derivations and is rebuilt only
    /// when a sample lands or the section opens or closes.
    pub(crate) performance: performance::PerformanceModel,
}

/// What each section of the workspace was last built from, as one key per section: the stamps and
/// the small values that section reads, hashed. A section whose key is unchanged is not built
/// again, so a message that changed nothing a section shows costs that section one comparison. The
/// Performance section keeps its own version and is not listed here.
#[derive(Clone, Debug, Default)]
pub(crate) struct Built {
    title: Option<u64>,
    panel: Option<u64>,
    canvas: Option<u64>,
    masks: Option<u64>,
    tools: Option<u64>,
    histogram: Option<u64>,
    status: Option<u64>,
    palette: Option<u64>,
    /// The session the last derivation read and its stamp. The session is small, and the desktop
    /// edits it in place as well as replacing it, so it is compared rather than tracked.
    session: Option<(ClientSession, u64)>,
    /// How many sections have been built, for tests that prove an unchanged message builds none.
    #[cfg(test)]
    pub(crate) builds: u64,
}

impl Built {
    /// The session's stamp: the one it had, unless it differs from the session last read.
    pub(crate) fn session_stamp(&mut self, session: &ClientSession) -> u64 {
        match &self.session {
            Some((seen, stamp)) if seen == session => *stamp,
            _ => {
                let stamp = tracked::stamp();
                self.session = Some((session.clone(), stamp));
                stamp
            }
        }
    }
}

/// One section's key: the values it reads, hashed. Nothing here allocates.
fn key(parts: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

/// The small values of the open asset that the sections read. The state is replaced whole by each
/// read-back, and a state read at one revision is the same state, so its identity, revision and
/// current entry stand for all of it.
fn state_key<'a>(inputs: &Inputs<'a>) -> Option<(&'a luxforge_core::AssetId, u64, &'a EntryId)> {
    inputs
        .state
        .map(|state| (&state.asset.id, state.revision, &state.current_entry.id))
}

/// The window's size and display scale, which decide what Fit comes to as a percentage.
fn window_key(inputs: &Inputs<'_>) -> (u32, u32, u32) {
    (
        inputs.window.0.to_bits(),
        inputs.window.1.to_bits(),
        inputs.scale_factor.to_bits(),
    )
}

/// A draft being dragged changes with every pointer move, so a section that draws one is built on
/// every derivation while it is open: a fresh stamp can never match.
fn drafting_key(open: bool) -> Option<u64> {
    open.then(tracked::stamp)
}

impl Built {
    /// Build every section whose key moved.
    fn refresh(&mut self, workspace: &mut Workspace, inputs: &Inputs<'_>) {
        let stamps = &inputs.stamps;
        let state = state_key(inputs);
        let session = stamps.session;
        let render_error = inputs.render_error.map(|error| {
            (
                error.kind.code(),
                error.detail.as_str(),
                error.unavailable_effect_id(),
            )
        });
        let title = key((
            (
                inputs.busy,
                inputs.can_open,
                inputs.compare_held,
                inputs.developer,
            ),
            (
                inputs.can_export,
                matches!(inputs.menu, Some(MenuTarget::Export)),
            ),
            inputs.dimensions,
            (&inputs.gallery_refusal, &inputs.history_refusal),
            session,
            state,
            (inputs.zoom, inputs.zoom_editing),
            window_key(inputs),
        ));
        let panel = key((
            (stamps.history, stamps.versions, stamps.lineage),
            (stamps.menu, session),
            inputs.lineage_floor,
            inputs.display_entry,
            state,
            (inputs.busy, inputs.version_form_open, inputs.version_name),
            &inputs.history_refusal,
        ));
        let canvas = key((
            (stamps.modules, stamps.capabilities, stamps.menu, session),
            (inputs.busy, inputs.developer, inputs.drafting, inputs.photo),
            (inputs.crop_guide, inputs.crop_option, inputs.crop_space),
            (
                inputs.gesture_conflicted,
                inputs.gesture,
                &inputs.apply_refusal,
            ),
            drafting_key(inputs.draft.is_some() || inputs.mask_draft.is_some()),
            (
                inputs.dimensions,
                inputs.pointer,
                inputs.scale_factor.to_bits(),
            ),
            render_error,
            inputs.slider_draft,
            state,
        ));
        let brush = inputs.brush;
        let masks = key((
            (
                stamps.masks,
                stamps.hidden_masks,
                stamps.modules,
                stamps.menu,
                session,
            ),
            (
                brush.size.to_bits(),
                brush.feather.to_bits(),
                brush.flow.to_bits(),
                brush.erase,
                brush.limit_to_colour,
                brush.colour_refine.to_bits(),
            ),
            (
                inputs.brush_erase_held,
                inputs.busy,
                inputs.gesture_conflicted,
            ),
            drafting_key(inputs.mask_draft.is_some()),
            std::mem::discriminant(&inputs.mask_mode),
            (inputs.mask_typing, inputs.masks_collapsed, inputs.mask_drag),
            // The panel's generated fields — the amount, the invert and the open component's own
            // geometry and band — read the host's field texts and which of them is typed or held.
            (
                inputs.fields.host_digest(),
                inputs.editing.filter(|(action, _)| action.contains('.')),
                inputs.dragging.filter(|(action, _)| action.contains('.')),
            ),
            (
                inputs.display_entry,
                inputs.selected_mask,
                inputs.selected_component,
                inputs.hovered_component,
            ),
            inputs.thumbnails.version,
            state,
        ));
        let tools = key((
            (
                stamps.modules,
                stamps.fields,
                stamps.controls,
                stamps.expanded,
            ),
            (stamps.current_recipe, stamps.recipe, stamps.menu, session),
            (stamps.presets, stamps.preset_form, stamps.capabilities),
            (inputs.busy, inputs.developer, inputs.modules_ready),
            (inputs.crop_custom, inputs.crop_guide),
            (inputs.editing, inputs.dragging),
            drafting_key(inputs.draft.is_some()),
            (&inputs.preset_refusal, inputs.slider_draft, inputs.target),
            // The bound mask's name is its sections' scope chip, so a rename reaches the bands.
            tools::bound_mask_name(inputs),
            inputs.display_entry,
            state,
        ));
        let histogram = key((
            inputs
                .analysis
                .map(|analysis| (analysis.generation, &analysis.identity)),
            inputs.analysis_updating,
            render_error,
            session,
            state,
        ));
        let status = key((
            inputs.clients,
            inputs
                .readout
                .map(|readout| (readout.x, readout.y, readout.rgba)),
            inputs
                .render
                .map(|render| (render.ms.to_bits(), render.proxy, render.approximate)),
            inputs.rendering,
            inputs.dimensions,
            window_key(inputs),
            session,
            inputs.status,
        ));
        let palette = key((
            (stamps.modules, stamps.presets, stamps.preset_form, session),
            (inputs.developer, inputs.performance_expanded, inputs.busy),
            (
                inputs.palette_open,
                inputs.palette_query,
                inputs.palette_selected,
            ),
            (&inputs.preset_refusal, inputs.display_entry, state),
        ));

        let stale = [
            moved(&mut self.title, title),
            moved(&mut self.panel, panel),
            moved(&mut self.canvas, canvas),
            moved(&mut self.masks, masks),
            moved(&mut self.tools, tools),
            moved(&mut self.histogram, histogram),
            moved(&mut self.status, status),
            moved(&mut self.palette, palette),
        ];
        #[cfg(test)]
        {
            self.builds += stale.iter().filter(|stale| **stale).count() as u64;
        }
        let [
            title_moved,
            panel_moved,
            canvas_moved,
            masks_moved,
            tools_moved,
            histogram_moved,
            status_moved,
            palette_moved,
        ] = stale;
        if title_moved {
            workspace.title = title::derive(inputs);
        }
        if panel_moved {
            workspace.panel = panel::derive(inputs);
        }
        if canvas_moved {
            workspace.canvas = canvas::derive(inputs);
        }
        if masks_moved {
            workspace.masks = masks::derive(inputs);
        }
        if tools_moved {
            workspace.tools.refresh(inputs);
        }
        if histogram_moved {
            workspace.histogram = histogram::derive(inputs, &workspace.histogram);
        }
        if status_moved {
            workspace.status = status::derive(inputs);
        }
        if palette_moved {
            workspace.palette = palette::derive(inputs);
        }
        workspace.performance.refresh(inputs);
    }
}

/// Record a section's new key, and say whether it moved.
fn moved(held: &mut Option<u64>, key: u64) -> bool {
    let moved = *held != Some(key);
    *held = Some(key);
    moved
}

impl Workspace {
    /// Build every section from these inputs, as a test scene does.
    #[cfg(test)]
    pub(crate) fn derive(&mut self, inputs: &Inputs<'_>) {
        self.title = title::derive(inputs);
        self.panel = panel::derive(inputs);
        self.performance.refresh(inputs);
        self.canvas = canvas::derive(inputs);
        self.masks = masks::derive(inputs);
        self.tools.refresh(inputs);
        self.histogram = histogram::derive(inputs, &self.histogram);
        self.status = status::derive(inputs);
        self.palette = palette::derive(inputs);
    }

    /// Build only the sections whose inputs moved since `built` recorded them; the rest stay as
    /// they are, unread and uncloned. A debug build checks every section it kept against a fresh
    /// derivation, so a key that misses an input fails the tests rather than showing a stale panel.
    pub(crate) fn refresh(&mut self, inputs: &Inputs<'_>, built: &mut Built) {
        built.refresh(self, inputs);
        #[cfg(debug_assertions)]
        self.check_kept(inputs);
    }

    /// Every section as a fresh derivation from `inputs` would build it.
    #[cfg(debug_assertions)]
    fn check_kept(&self, inputs: &Inputs<'_>) {
        let mut tools = self.tools.clone();
        tools.refresh(inputs);
        let fresh = Workspace {
            title: title::derive(inputs),
            panel: panel::derive(inputs),
            canvas: canvas::derive(inputs),
            masks: masks::derive(inputs),
            tools,
            histogram: histogram::derive(inputs, &self.histogram),
            status: status::derive(inputs),
            palette: palette::derive(inputs),
            performance: self.performance.clone(),
        };
        debug_assert!(
            fresh == *self,
            "a kept section differs from its fresh derivation: a section key misses an input"
        );
    }

    /// Every picker control the panel derived, by the module whose pick mode it selects, with the
    /// label it shows and whether it reads selected. A captured frame carries it so the rendered
    /// button can be checked against what the model said it should be.
    pub(crate) fn pickers(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.tools
                .all()
                .flat_map(|section| section.pickers())
                .map(|picker| {
                    (
                        picker.module_id.clone(),
                        serde_json::json!({
                            "label": picker.label,
                            "title": picker.title,
                            "shortcut": picker.shortcut,
                            "selected": picker.selected,
                            "enabled": picker.enabled,
                        }),
                    )
                })
                .collect(),
        )
    }

    /// Every section's controls in the order the panel draws them, each with its kind and label
    /// and what it addresses: a slider's action, parameter and unit, a button's action and a
    /// picker's mode. The correlated evidence state reads which controls a section shows, and
    /// where a variant put another module's control in a declared control's place.
    pub(crate) fn section_controls(&self) -> serde_json::Value {
        use serde_json::json;
        serde_json::Value::Object(
            self.tools
                .all()
                .map(|section| {
                    let controls = control_tree::walk(&section.controls)
                        .filter_map(|control| match control {
                            tools::ControlModel::Group(group) => {
                                Some(json!({"kind": "group", "label": group.label}))
                            }
                            tools::ControlModel::Slider(slider) => Some(json!({
                                "kind": "number", "label": slider.label,
                                "action": slider.action, "parameter": slider.parameter,
                                "unit": slider.unit,
                            })),
                            tools::ControlModel::Range(range) => Some(json!({
                                "kind": "range", "label": range.label, "action": range.action,
                                "parameters": range.fields()
                                    .map(|field| field.parameter.as_str())
                                    .collect::<Vec<_>>(),
                            })),
                            tools::ControlModel::Action(action) => Some(json!({
                                "kind": "action", "label": action.label, "action": action.action,
                            })),
                            tools::ControlModel::Picker(picker) => Some(json!({
                                "kind": "picker", "label": picker.label, "mode": picker.module_id,
                            })),
                            _ => None,
                        })
                        .collect();
                    (
                        section.module_id.clone(),
                        serde_json::Value::Array(controls),
                    )
                })
                .collect(),
        )
    }

    /// Which sections' bands carry the edited dot, for the correlated evidence state.
    pub(crate) fn active(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.tools
                .all()
                .map(|section| {
                    (
                        section.module_id.clone(),
                        serde_json::Value::from(section.active),
                    )
                })
                .collect(),
        )
    }

    /// Which sections' bands carry a scope chip, and the mask it names, for the correlated evidence
    /// state. Only sections with a chip are listed, so outside Mask mode it is empty.
    pub(crate) fn scopes(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.tools
                .all()
                .filter_map(|section| {
                    section.scope.as_ref().map(|scope| {
                        (
                            section.module_id.clone(),
                            serde_json::Value::from(scope.as_str()),
                        )
                    })
                })
                .collect(),
        )
    }

    /// Which sections are expanded, for the correlated evidence state.
    pub(crate) fn expanded(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.tools
                .all()
                .map(|section| {
                    (
                        section.module_id.clone(),
                        serde_json::Value::from(section.expanded),
                    )
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        panel::Marker,
        tools::{ControlModel, ValueEdit},
        *,
    };
    use crate::state::testing::{
        controls_descriptor, crop_descriptor, crop_layer, descriptors, entry, listed,
        tabs_descriptor,
    };
    use luxforge_core::{
        AssetId, AssetRecord, Availability, CropPayload, Orientation, POINTER_MODE,
    };
    use serde_json::json;
    use std::path::PathBuf;

    /// The pieces a derivation borrows, owned by the test so `Inputs` can point at them.
    struct Scene {
        state: Option<EditorState>,
        history: HistoryPage,
        /// The whole entries behind the page's rows, where the displayed entry's layers are read.
        stacks: Vec<luxforge_core::HistoryEntry>,
        versions: Vec<Version>,
        lineage: HashSet<EntryId>,
        lineage_floor: Option<u64>,
        display_entry: Option<EntryId>,
        modules: Vec<ModuleDescriptor>,
        recipe: Option<RecipeDescription>,
        current_recipe: Option<RecipeDescription>,
        fields: Fields,
        control_ui: tools::ControlsUi,
        editing: Option<(String, String)>,
        dragging: Option<(String, String)>,
        expanded: BTreeMap<String, bool>,
        draft: Option<CropDraft>,
        /// The open crop draft's core draft is conflicted.
        crop_conflicted: bool,
        /// What the app's release refusal answers for the open gesture.
        apply_refusal: Option<String>,
        masks: Option<MaskListing>,
        selected_mask: Option<MaskId>,
        selected_component: Option<ComponentId>,
        hovered_component: Option<ComponentId>,
        hidden_masks: HashSet<MaskId>,
        thumbnails: masks::MaskThumbnails,
        mask_draft: Option<MaskDraft>,
        session: ClientSession,
        status: String,
        busy: bool,
        developer: bool,
        render_error: Option<luxforge_core::Error>,
        analysis: Option<histogram::Analysis>,
        analysis_updating: bool,
        readout: Option<histogram::Readout>,
        presets: presets::PresetLibrary,
        preset_form: presets::PresetForm,
        /// An open slider gesture's (action, parameter, conflicted).
        slider_draft: Option<(String, String, bool)>,
        capabilities: capabilities::CapabilityStore,
        performance_expanded: bool,
        performance: performance::PerformanceHistory,
    }

    impl Scene {
        fn new(modules: Vec<ModuleDescriptor>) -> Self {
            let fields = Fields::seeded(&modules);
            Self {
                state: None,
                history: HistoryPage {
                    entries: Vec::new(),
                    next_before_sequence: None,
                },
                stacks: Vec::new(),
                versions: Vec::new(),
                lineage: HashSet::new(),
                lineage_floor: None,
                display_entry: None,
                modules,
                recipe: None,
                current_recipe: None,
                fields,
                control_ui: tools::ControlsUi::default(),
                editing: None,
                dragging: None,
                expanded: BTreeMap::new(),
                draft: None,
                crop_conflicted: false,
                apply_refusal: None,
                masks: None,
                selected_mask: None,
                selected_component: None,
                hovered_component: None,
                hidden_masks: HashSet::new(),
                thumbnails: masks::MaskThumbnails::default(),
                mask_draft: None,
                session: ClientSession::default(),
                status: "ready".into(),
                busy: false,
                developer: false,
                render_error: None,
                analysis: None,
                analysis_updating: false,
                readout: None,
                presets: presets::PresetLibrary::default(),
                preset_form: presets::PresetForm::default(),
                slider_draft: None,
                capabilities: capabilities::CapabilityStore::default(),
                performance_expanded: false,
                performance: performance::PerformanceHistory::default(),
            }
        }

        /// One asset open at that revision with those layers, and one history entry for it.
        fn opened(mut self, layers: Vec<luxforge_core::Layer>) -> Self {
            let asset = AssetId::new();
            let mut current = entry(&asset, 3, None);
            current.label = "Crop 4:5".into();
            current.actor = "agent".into();
            for layer in layers {
                current.snapshot = current.snapshot.append(layer).expect("a valid stack");
            }
            self.display_entry = Some(current.id.clone());
            self.current_recipe = Some(crate::state::testing::described(&current));
            self.lineage.insert(current.id.clone());
            self.history = HistoryPage {
                entries: vec![luxforge_core::HistoryRow::from(&current)],
                next_before_sequence: None,
            };
            self.stacks = vec![current.clone()];
            self.state = Some(EditorState {
                asset: AssetRecord {
                    id: asset,
                    source_root: PathBuf::new(),
                    locator: PathBuf::from("photo.jpg"),
                    fingerprint: "f".into(),
                    file_identity: "i".into(),
                    byte_len: 0,
                    width: 1,
                    height: 1,
                    source: luxforge_core::SourceKind::Jpeg,
                },
                revision: 3,
                current_entry: current,
                redo: Vec::new(),
            });
            self
        }

        fn inputs(&self) -> Inputs<'_> {
            Inputs {
                stamps: Stamps::fresh(),
                state: self.state.as_ref(),
                history: &self.history,
                versions: &self.versions,
                lineage: &self.lineage,
                lineage_floor: self.lineage_floor,
                display_entry: self.display_entry.as_ref(),
                modules: &self.modules,
                modules_ready: true,
                recipe: self.recipe.as_ref(),
                current_recipe: self.current_recipe.as_ref(),
                fields: &self.fields,
                control_ui: &self.control_ui,
                editing: self.editing.as_ref(),
                dragging: self.dragging.as_ref(),
                expanded: &self.expanded,
                slider_draft: self
                    .slider_draft
                    .as_ref()
                    .map(|(action, parameter, conflicted)| SliderDrafting {
                        action,
                        parameter,
                        conflicted: *conflicted,
                    }),
                gesture_conflicted: self.crop_conflicted
                    || self
                        .slider_draft
                        .as_ref()
                        .is_some_and(|(_, _, conflicted)| *conflicted),
                gesture: if self.slider_draft.is_some() {
                    Some("slider draft")
                } else if self.mask_draft.is_some() {
                    Some("mask gesture")
                } else {
                    self.draft.as_ref().map(|_| "crop draft")
                },
                apply_refusal: self.apply_refusal.clone(),
                preset_refusal: (self.slider_draft.is_some() || self.draft.is_some())
                    .then(|| "Finish the open draft before applying a preset".to_owned()),
                gallery_refusal: (self.slider_draft.is_some() || self.draft.is_some())
                    .then(|| "Finish the open draft before opening Components".to_owned()),
                history_refusal: (self.slider_draft.is_some() || self.draft.is_some()).then(|| {
                    "Finish the open draft before undoing, redoing or restoring".to_owned()
                }),
                edit_refusal: edit_refusal(self.state.as_ref(), &self.session, self.busy),
                draft: self.draft.as_ref(),
                masks: self.masks.as_ref(),
                selected_mask: self.selected_mask.as_ref(),
                selected_component: self.selected_component.as_ref(),
                hovered_component: self.hovered_component.as_ref(),
                hidden_masks: &self.hidden_masks,
                thumbnails: &self.thumbnails,
                mask_draft: self.mask_draft.as_ref(),
                mask_mode: ComponentMode::Add,
                brush: crate::mask_draft::NEUTRAL_BRUSH,
                brush_erase_held: false,
                mask_typing: None,
                masks_collapsed: false,
                mask_drag: None,
                target: self
                    .selected_mask
                    .as_ref()
                    .filter(|_| crate::state::canvas::mask_workspace(&self.session.workspace.mode)),
                drafting: self.draft.is_some(),
                crop_custom: ("5", "4"),
                crop_guide: false,
                crop_option: false,
                crop_space: false,
                session: &self.session,
                status: &self.status,
                busy: self.busy,
                can_open: true,
                can_export: true,
                developer: self.developer,
                compare_held: false,
                scale_factor: 2.0,
                zoom: "100",
                zoom_editing: false,
                window: (1440.0, 900.0),
                fullscreen: false,
                version_name: "",
                version_form_open: false,
                dimensions: Some((480, 320)),
                photo: true,
                clients: Some(1),
                rendering: false,
                render: Some(status::RenderTime {
                    ms: 41.0,
                    proxy: false,
                    approximate: false,
                }),
                render_error: self.render_error.as_ref(),
                pointer: None,
                analysis: self.analysis.as_ref(),
                analysis_updating: self.analysis_updating,
                readout: self.readout.as_ref(),
                menu: None,
                palette_open: false,
                palette_query: "",
                palette_selected: 0,
                presets: &self.presets,
                preset_form: &self.preset_form,
                capabilities: &self.capabilities,
                performance_expanded: self.performance_expanded,
                performance: &self.performance,
            }
        }

        fn derive(&self) -> Workspace {
            let mut workspace = Workspace::default();
            workspace.derive(&self.inputs());
            workspace
        }

        /// Give the scene the owner's `recipe.describe` rows for whichever entry it displays,
        /// described against the open asset's extents, input stages included.
        fn describe_displayed(&mut self) {
            let displayed = self.display_entry.as_ref().expect("a displayed entry");
            let asset = &self.state.as_ref().expect("an open asset").asset;
            let entry = self
                .stacks
                .iter()
                .find(|entry| &entry.id == displayed)
                .expect("the displayed entry's stack");
            self.recipe = Some(crate::state::testing::described_at(
                entry,
                (asset.width, asset.height),
            ));
        }

        /// Add one entry's row to the loaded page, and its stack to what the scene can display.
        fn list(&mut self, entry: luxforge_core::HistoryEntry) {
            self.history
                .entries
                .push(luxforge_core::HistoryRow::from(&entry));
            self.stacks.push(entry);
        }

        /// The open asset's source dimensions, which the crop layer's input stage starts from, and
        /// the displayed entry's rows described against them.
        fn sized(mut self, width: u32, height: u32) -> Self {
            let asset = &mut self.state.as_mut().expect("an open asset").asset;
            asset.width = width;
            asset.height = height;
            self.describe_displayed();
            self
        }
    }

    fn section<'a>(workspace: &'a Workspace, id: &str) -> &'a tools::SectionModel {
        workspace
            .tools
            .all()
            .find(|section| section.module_id == id)
            .unwrap_or_else(|| panic!("no section for {id}"))
    }

    /// The Performance section is rebuilt only when a sample lands or it opens or closes: every
    /// other derivation — a drag re-derives dozens of times a second — leaves it, and so its
    /// sparklines' version, exactly as it was.
    #[test]
    fn the_performance_section_is_rebuilt_only_by_its_own_inputs() {
        let mut scene = Scene::new(descriptors());
        let mut workspace = scene.derive();
        assert!(!workspace.performance.expanded);
        assert!(workspace.performance.metrics.is_empty());

        scene.performance_expanded = true;
        scene.performance.clear();
        workspace.derive(&scene.inputs());
        assert!(workspace.performance.expanded);
        assert_eq!(workspace.performance.metrics.len(), 3);
        let version = workspace.performance.version;

        scene.status = "Something else changed".into();
        scene.busy = true;
        workspace.derive(&scene.inputs());
        assert_eq!(workspace.performance.version, version);

        let budget = json!({"target_bytes": 0, "in_use_bytes": 0, "peak_bytes": 0});
        let sample: luxforge_core::resources::ResourceReport = serde_json::from_value(json!({
            "monotonic_ns": 1,
            "cpu": {"time_ns": 5, "logical_cpus": 14},
            "memory": {"kind": "footprint", "bytes": 1_523_000_000_u64},
            "gpu": {"time_ns": 1},
            "budgets": {"colour_scratch": budget, "spatial": budget}
        }))
        .unwrap();
        scene
            .performance
            .push(sample, luxforge_core::ActivitySnapshot::default());
        workspace.derive(&scene.inputs());
        assert_ne!(workspace.performance.version, version);
        assert_eq!(workspace.performance.metrics[0].value, "1.42");

        scene.performance_expanded = false;
        workspace.derive(&scene.inputs());
        assert!(workspace.performance.metrics.is_empty(), "collapsed");
    }

    #[test]
    fn history_rows_carry_the_stored_label_actor_and_marker() {
        let scene = Scene::new(descriptors()).opened(Vec::new());
        let workspace = scene.derive();
        let row = &workspace.panel.history[0];
        assert_eq!(row.label, "Crop 4:5", "the stored label, not a rebuild");
        // The scene's entry is another client's, which the row names as an agent.
        assert_eq!(row.actor.as_deref(), Some("agent \u{b7} agent"));
        assert_eq!(row.marker, Marker::Current);
        assert!(!row.branch);
        assert!(workspace.panel.preview.is_none());

        // A history preview marks the displayed entry and offers the preview controls.
        let asset = scene.state.as_ref().expect("an asset").asset.id.clone();
        let older = entry(&asset, 1, None);
        let mut scene = scene;
        scene.list(older.clone());
        scene.display_entry = Some(older.id.clone());
        scene.session.preview.selection = luxforge_core::HistorySelection::Entry(older.id.clone());
        let workspace = scene.derive();
        assert_eq!(workspace.panel.history[0].marker, Marker::Current);
        assert_eq!(workspace.panel.history[1].marker, Marker::Previewed);
        assert_eq!(
            workspace.panel.preview.map(|preview| preview.can_restore),
            Some(true)
        );
    }

    /// Restore commits, so the panel offers it only where the app's history refusal would let it
    /// run: never while a draft is held or a request is in flight. Return to current is a
    /// selection, which a held draft does not refuse.
    #[test]
    fn restore_is_disabled_while_the_history_refusal_holds() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        let asset = scene.state.as_ref().expect("an asset").asset.id.clone();
        let older = entry(&asset, 1, None);
        scene.list(older.clone());
        scene.display_entry = Some(older.id.clone());
        scene.session.preview.selection = luxforge_core::HistorySelection::Entry(older.id);
        let preview = |scene: &Scene| scene.derive().panel.preview.expect("preview controls");
        assert_eq!(
            preview(&scene),
            panel::PreviewControls {
                can_return: true,
                can_restore: true
            }
        );
        scene.slider_draft = Some(("fixture-set".into(), "amount".into(), false));
        assert!(scene.inputs().history_refusal.is_some());
        assert_eq!(
            preview(&scene),
            panel::PreviewControls {
                can_return: true,
                can_restore: false
            },
            "a held draft refuses Restore"
        );
        scene.slider_draft = None;
        scene.busy = true;
        let workspace = scene.derive();
        assert_eq!(
            workspace.panel.preview,
            Some(panel::PreviewControls {
                can_return: false,
                can_restore: false
            })
        );
        assert!(
            !workspace.panel.can_select,
            "no selection while a request is in flight"
        );
    }

    #[test]
    fn an_entry_off_the_lineage_is_marked_as_a_branch() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        let asset = scene.state.as_ref().expect("an asset").asset.id.clone();
        let abandoned = entry(&asset, 2, None);
        scene.list(abandoned.clone());
        assert!(
            scene.derive().panel.history[1].branch,
            "an entry the lineage walk never reached is a branch"
        );
        // Below a truncated lineage nothing can be called abandoned.
        scene.lineage_floor = Some(2);
        assert!(!scene.derive().panel.history[1].branch);
    }

    #[test]
    fn a_section_names_why_editing_is_disabled() {
        let crop = crop_descriptor();
        let mut scene = Scene::new(vec![crop.clone()]);
        assert_eq!(
            section(&scene.derive(), &crop.id)
                .disabled_reason
                .as_deref(),
            Some("No photograph is open")
        );
        scene = scene.opened(Vec::new());
        assert!(section(&scene.derive(), &crop.id).enabled);
        scene.session.preview.selection = luxforge_core::HistorySelection::Entry(EntryId::new());
        assert_eq!(
            section(&scene.derive(), &crop.id)
                .disabled_reason
                .as_deref(),
            Some(NOT_CURRENT)
        );
        scene.session.preview.selection = luxforge_core::HistorySelection::Current;
        scene.busy = true;
        assert_eq!(
            section(&scene.derive(), &crop.id)
                .disabled_reason
                .as_deref(),
            Some("Waiting for the last request")
        );
        scene.busy = false;
        scene.modules = vec![ModuleDescriptor {
            availability: Availability::Unavailable {
                reason: "disabled by --disable-module".into(),
            },
            ..crop.clone()
        }];
        let workspace = scene.derive();
        let disabled = section(&workspace, &crop.id);
        assert_eq!(
            disabled.unavailable.as_deref(),
            Some("disabled by --disable-module")
        );
        assert_eq!(
            disabled.disabled_reason.as_deref(),
            Some("disabled by --disable-module")
        );
        assert!(!disabled.enabled);
    }

    fn crop_model(workspace: &Workspace, id: &str) -> tools::CropSectionModel {
        match section(workspace, id).controls.first() {
            Some(ControlModel::CropFrame(frame)) => (**frame).clone(),
            other => panic!("the crop section starts with its frame controls, not {other:?}"),
        }
    }

    fn chosen(model: &tools::CropSectionModel) -> Vec<&str> {
        model
            .presets
            .iter()
            .filter(|chip| chip.chosen)
            .map(|chip| chip.label.as_str())
            .collect()
    }

    /// The largest rectangle of that ratio inside the whole stage, fitted by the core's own geometry
    /// exactly as `crop-fit` or a ratio chip fits it, as the payload that commits it.
    fn fitted(stage: luxforge_core::CropStage, ratio: f64) -> CropPayload {
        let (width, height) = stage.bounding_box();
        let whole = luxforge_core::BoxRect {
            x: 0.0,
            y: 0.0,
            width,
            height,
        };
        stage
            .fit_about_center(luxforge_core::largest_with_ratio_inside(whole, ratio))
            .normalized(&stage)
    }

    /// Idle, the crop section shows the drafting section's Ratio and Angle controls reading the
    /// displayed entry's committed crop exactly as a draft opened on it seeds them: the ratio it
    /// reads as chosen and locked, or Free, and its angle on the rail at rest.
    #[test]
    fn the_idle_crop_section_reads_the_committed_crop_as_a_draft_would_seed_it() {
        let crop = crop_descriptor();
        let stage = luxforge_core::CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        };
        let idle = |layers: Vec<luxforge_core::Layer>| {
            let scene = Scene::new(vec![crop.clone()])
                .opened(layers)
                .sized(480, 320);
            crop_model(&scene.derive(), &crop.id)
        };

        // No crop, or a neutral one: Free with the lock open, no swap, 0° on the rail at rest, and
        // neither readout nor Apply, which belong to a draft.
        for layers in [Vec::new(), vec![crop_layer(CropPayload::NEUTRAL)]] {
            let model = idle(layers);
            assert!(!model.drafting && model.enabled);
            assert_eq!(chosen(&model), ["Free"]);
            assert!(!model.locked && !model.can_swap);
            assert_eq!(model.lock_label, "Lock ratio");
            let angle = model.angle.as_ref().expect("the angle's stepper");
            assert_eq!(
                angle.style,
                tools::NumberControlStyle::Stepper { rail: true }
            );
            assert_eq!(
                (angle.display.as_str(), angle.unit.as_deref(), angle.value),
                ("0.0", Some("\u{b0}"), 0.0)
            );
            assert_eq!((angle.spec.min, angle.spec.max), (-45.0, 45.0));
            assert_eq!((angle.spec.step, angle.spec.fine_step), (0.5, 0.05));
            assert!(!angle.dragging, "the rail rests while idle");
            assert!(model.readout.is_empty() && !model.can_apply);
            assert_eq!(model.presets.len(), 7, "every declared ratio is a chip");
        }

        // A committed 16:9 crop reads as 16:9 with the lock closed, and a draft opened on it seeds
        // exactly that.
        let wide = fitted(stage, 16.0 / 9.0);
        let model = idle(vec![crop_layer(wide)]);
        assert_eq!(chosen(&model), ["16:9"]);
        assert!(model.locked && model.can_swap);
        assert_eq!(model.lock_label, "Unlock ratio");
        let presets = crate::crop_draft::aspect_presets(
            &crate::state::testing::CROP_ASPECTS.map(String::from),
        );
        let draft = CropDraft::from_layer(stage, wide, luxforge_core::LayerId::new(), 0, &presets);
        assert_eq!(draft.preset, "16:9");

        // Straightened to 2.4° at the stage's own ratio reads as Original, at 2.4° on the rail.
        let straightened = fitted(
            luxforge_core::CropStage {
                angle: 2.4,
                ..stage
            },
            1.5,
        );
        let model = idle(vec![crop_layer(straightened)]);
        assert_eq!(chosen(&model), ["Original"]);
        let angle = model.angle.expect("the angle's stepper");
        assert_eq!((angle.display.as_str(), angle.value), ("2.4", 2.4));

        // An off-centre rectangle no ratio produces reads as Free, at its own angle.
        let free = CropPayload {
            angle: 7.0,
            x: 0.2,
            y: 0.25,
            width: 0.4,
            height: 0.3,
        };
        let model = idle(vec![crop_layer(free)]);
        assert_eq!(chosen(&model), ["Free"]);
        assert!(!model.locked);
        assert_eq!(model.angle.expect("the angle's stepper").display, "7.0");

        // Behind a quarter turn the crop's input stage is portrait. A 16:9 fitted there reads as
        // 16:9 only because the turn is read: on the unturned stage the same payload is 480 × 120.
        let tall = fitted(
            luxforge_core::CropStage {
                width: 320,
                height: 480,
                angle: 0.0,
            },
            16.0 / 9.0,
        );
        let turned = luxforge_core::Layer::orientation(Orientation {
            mirror: false,
            turns: 1,
        });
        assert_eq!(chosen(&idle(vec![turned, crop_layer(tall)])), ["16:9"]);
        assert_eq!(chosen(&idle(vec![crop_layer(tall)])), ["Free"]);

        // Whatever precedes the crop, the section reads the stage the core reports on the crop's
        // row and folds no geometry itself. Here the rows say another geometry layer, one this
        // desktop has no descriptor for, hands the crop 200 × 120: a square fitted there reads as
        // 1:1, and the same rectangle read against the source's own stage would not.
        let shrunk = luxforge_core::CropStage {
            width: 200,
            height: 120,
            angle: 3.0,
        };
        let mut scene = Scene::new(vec![crop.clone()])
            .opened(vec![crop_layer(fitted(shrunk, 1.0))])
            .sized(480, 320);
        assert_eq!(chosen(&crop_model(&scene.derive(), &crop.id)), ["Free"]);
        fn row(scene: &mut Scene) -> &mut luxforge_core::LayerDescription {
            &mut scene.recipe.as_mut().expect("described rows").layers[0]
        }
        row(&mut scene).input_stage = Some(luxforge_core::StageSize {
            width: 200,
            height: 120,
        });
        let model = crop_model(&scene.derive(), &crop.id);
        assert_eq!(chosen(&model), ["1:1"]);
        assert!(model.locked);
        assert_eq!(model.angle.expect("the angle's stepper").display, "3.0");
        // A row without a stage, which the core reports after a layer it cannot compile, reads as
        // Free at the crop's own angle rather than as a guess.
        row(&mut scene).input_stage = None;
        let model = crop_model(&scene.derive(), &crop.id);
        assert_eq!(chosen(&model), ["Free"]);
        assert_eq!(model.angle.expect("the angle's stepper").display, "3.0");
    }

    /// The idle controls read the displayed entry, not the current one, and a historical preview or
    /// a request in flight disables them exactly as it disables every other section's controls.
    #[test]
    fn the_idle_crop_section_follows_the_displayed_entry_and_the_disabled_states() {
        let crop = crop_descriptor();
        let wide = fitted(
            luxforge_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            16.0 / 9.0,
        );
        let mut scene = Scene::new(vec![crop.clone()])
            .opened(vec![crop_layer(wide)])
            .sized(480, 320);
        scene.busy = true;
        let model = crop_model(&scene.derive(), &crop.id);
        assert!(!model.enabled && !model.can_swap);
        assert_eq!(
            chosen(&model),
            ["16:9"],
            "a disabled section still reads the crop"
        );
        scene.busy = false;

        let asset = scene.state.as_ref().expect("an asset").asset.id.clone();
        let older = entry(&asset, 1, None);
        scene.list(older.clone());
        scene.display_entry = Some(older.id.clone());
        scene.session.preview.selection = luxforge_core::HistorySelection::Entry(older.id);
        let model = crop_model(&scene.derive(), &crop.id);
        assert!(!model.enabled && !model.can_swap);
        assert_eq!(
            chosen(&model),
            ["Free"],
            "rows that describe another entry are not the displayed entry's crop"
        );
        assert_eq!(model.angle.expect("the angle's stepper").display, "0.0");
        scene.describe_displayed();
        let model = crop_model(&scene.derive(), &crop.id);
        assert_eq!(chosen(&model), ["Free"], "the displayed entry has no crop");
        assert_eq!(model.angle.expect("the angle's stepper").display, "0.0");
    }

    #[test]
    fn a_conflicted_draft_shows_its_state_in_the_crop_model_and_a_notice() {
        let crop = crop_descriptor();
        let mut scene = Scene::new(vec![crop.clone()]).opened(Vec::new());
        scene.draft = Some(CropDraft::neutral(
            luxforge_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            0,
        ));
        let workspace = scene.derive();
        let ControlModel::CropFrame(frame) = &section(&workspace, &crop.id).controls[0] else {
            panic!("the crop section is first in its module")
        };
        assert!(frame.drafting && !frame.conflicted && frame.can_apply);
        assert_eq!(frame.presets.len(), 7, "the ratios are generated");
        assert!(workspace.canvas.notices.is_empty());

        // The app's one refusal says why Apply cannot run; the section reads it as it is.
        scene.crop_conflicted = true;
        scene.apply_refusal =
            Some("Changed elsewhere: discard the crop draft or reapply it".into());
        let workspace = scene.derive();
        let ControlModel::CropFrame(frame) = &section(&workspace, &crop.id).controls[0] else {
            panic!("the crop section is first in its module")
        };
        assert!(frame.conflicted && !frame.can_apply);
        assert_eq!(
            workspace.canvas.notices[0].title, "Changed elsewhere",
            "the conflict is a notice, not a silent discard"
        );
        assert_eq!(
            workspace.canvas.notices[0].body,
            "Another client committed revision 3 while your crop draft was open. Your crop draft is kept.",
            "the one notice names the gesture that is open"
        );
        assert_eq!(
            workspace.canvas.draft_bar.map(|bar| bar.conflicted),
            Some(true)
        );
    }

    #[test]
    fn a_section_declared_collapsed_starts_collapsed_until_the_person_expands_it() {
        let mut crop = crop_descriptor();
        crop.collapsed = true;
        let mut scene = Scene::new(vec![crop.clone()]).opened(Vec::new());
        assert!(
            !section(&scene.derive(), &crop.id).expanded,
            "the descriptor's hint is the initial state"
        );
        scene.expanded.insert(crop.id.clone(), true);
        assert!(
            section(&scene.derive(), &crop.id).expanded,
            "a person's own choice wins over the hint"
        );
    }

    #[test]
    fn sections_start_expanded_and_a_draft_holds_its_own_section_open() {
        let crop = crop_descriptor();
        let mut scene = Scene::new(vec![crop.clone()]).opened(Vec::new());
        assert!(section(&scene.derive(), &crop.id).expanded);
        scene.expanded.insert(crop.id.clone(), false);
        assert!(!section(&scene.derive(), &crop.id).expanded);
        // While the module's own canvas mode is drafting the section cannot stay collapsed.
        scene.session.workspace.mode = crop.id.clone();
        scene.draft = Some(CropDraft::neutral(
            luxforge_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            0,
        ));
        assert!(section(&scene.derive(), &crop.id).expanded);
        scene.session.workspace.mode = POINTER_MODE.into();
        assert!(
            !section(&scene.derive(), &crop.id).expanded,
            "a draft in another mode does not force this section open"
        );
    }

    #[test]
    fn a_value_shows_what_is_typed_while_typing_and_the_formatted_value_otherwise() {
        let modules = descriptors();
        let (action, x, _) = tools::point_pick(&modules).expect("a canvas pick");
        let (action, x) = (action.to_owned(), x.to_owned());
        let mut scene = Scene::new(modules).opened(Vec::new());
        // The pixel proof tool declares the only number controls today, so its section is listed.
        scene.developer = true;
        scene.fields.set(&action, &x, "007".into());
        let workspace = scene.derive();
        let slider = find_slider(&workspace, &action, &x);
        assert_eq!(slider.display, "7", "a settled field shows the value");
        assert_eq!(slider.edit, ValueEdit::None);
        assert_eq!(slider.edit.text(&slider.display), "7");
        assert!(!slider.dragging);

        scene.editing = Some((action.clone(), x.clone()));
        let workspace = scene.derive();
        let slider = find_slider(&workspace, &action, &x);
        assert_eq!(slider.edit, ValueEdit::Typing("007".into()));
        assert_eq!(
            slider.edit.text(&slider.display),
            "007",
            "typing shows the text exactly as typed"
        );

        // A dragging slider carries the drag, and unparsable text keeps itself on screen.
        scene.editing = None;
        scene.dragging = Some((action.clone(), x.clone()));
        scene.fields.set(&action, &x, "nine".into());
        let workspace = scene.derive();
        let slider = find_slider(&workspace, &action, &x);
        assert!(slider.dragging);
        assert_eq!(slider.display, "nine");
        assert!(slider.invalid.is_some());
        assert_eq!(
            slider.value, slider.spec.min,
            "an unreadable field reads as its min"
        );
    }

    /// Every sub-group in the panel, with the state it reads and the fields it holds.
    fn sub_groups(workspace: &Workspace) -> Vec<(String, Option<tools::GroupState>, Vec<String>)> {
        let mut found = Vec::new();
        for section in workspace.tools.all() {
            for control in control_tree::walk(&section.controls) {
                if let ControlModel::Group(group) = control {
                    let fields = group
                        .controls
                        .iter()
                        .filter_map(|child| match child {
                            ControlModel::Slider(slider) => Some(slider.parameter.clone()),
                            _ => None,
                        })
                        .collect();
                    found.push((group.label.clone(), group.state, fields));
                }
            }
        }
        found
    }

    /// A sub-group of a field-patch action reads Original while every one of its fields is at its
    /// declared default and Custom as soon as one is not. The words are derived from the values on
    /// screen, which a patch action's fields take from the displayed entry's own layer, so no
    /// module declares them and none can.
    #[test]
    fn a_sub_group_reads_original_until_one_of_its_fields_leaves_its_default() {
        let modules = descriptors();
        // The first patch a JPEG shows; the RAW development's applies only to a RAW photo.
        let patch = modules
            .iter()
            .filter(|module| module.applies_to(luxforge_core::SourceTag::Jpeg))
            .flat_map(|module| module.actions.iter())
            .find(|action| action.patch)
            .expect("a built-in declares a field patch")
            .clone();
        let parameter = patch
            .parameters
            .first()
            .expect("the patch declares a field")
            .name
            .clone();
        let mut scene = Scene::new(modules).opened(Vec::new());
        scene.developer = true;

        let listed = sub_groups(&scene.derive());
        assert!(listed.len() >= 3, "the built-ins declare sub-groups");
        let stateful: Vec<&(String, Option<tools::GroupState>, Vec<String>)> = listed
            .iter()
            .filter(|(_, state, _)| state.is_some())
            .collect();
        assert!(
            stateful.len() >= 3,
            "no group of a patch action was modelled: {listed:?}"
        );
        for (label, state, _) in &stateful {
            assert_eq!(
                *state,
                Some(tools::GroupState::Original),
                "{label} does not start at its declared defaults"
            );
        }
        assert_eq!(tools::GroupState::Original.caption(), "Original");
        assert_eq!(tools::GroupState::Custom.caption(), "Custom");

        // One field off its default turns exactly the group that holds it Custom.
        scene.fields.set(&patch.id, &parameter, "1.5".into());
        let changed = sub_groups(&scene.derive());
        for (label, state, fields) in &changed {
            let expected = if fields.contains(&parameter) {
                Some(tools::GroupState::Custom)
            } else if listed
                .iter()
                .any(|(other, other_state, _)| other == label && other_state.is_some())
            {
                Some(tools::GroupState::Original)
            } else {
                None
            };
            assert_eq!(*state, expected, "{label} reads the wrong state");
        }

        // An unreadable value is not its default either, so its group says so rather than
        // pretending the field still holds what the layer stores.
        scene
            .fields
            .set(&patch.id, &parameter, "not a number".into());
        let invalid = sub_groups(&scene.derive());
        assert!(
            invalid
                .iter()
                .any(|(_, state, fields)| fields.contains(&parameter)
                    && *state == Some(tools::GroupState::Custom))
        );
    }

    fn find_slider<'a>(
        workspace: &'a Workspace,
        action: &str,
        parameter: &str,
    ) -> &'a tools::SliderControl {
        workspace
            .tools
            .all()
            .find_map(|section| {
                control_tree::walk(&section.controls).find_map(|control| match control {
                    ControlModel::Slider(slider)
                        if slider.action == action && slider.parameter == parameter =>
                    {
                        Some(slider)
                    }
                    _ => None,
                })
            })
            .expect("a declared slider")
    }

    #[test]
    fn developer_modules_are_listed_only_when_the_flag_is_set() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        let developer: Vec<String> = scene
            .modules
            .iter()
            .filter(|module| module.developer)
            .map(|module| module.id.clone())
            .collect();
        assert!(
            !developer.is_empty(),
            "the pixel proof module marks itself as a developer tool"
        );
        let workspace = scene.derive();
        assert!(workspace.tools.developer.is_empty());
        for id in &developer {
            assert!(
                !workspace.tools.sections.iter().any(|s| s.module_id == *id),
                "{id} is hidden without --developer"
            );
        }
        scene.developer = true;
        let workspace = scene.derive();
        assert_eq!(
            workspace
                .tools
                .developer
                .iter()
                .map(|section| section.module_id.clone())
                .collect::<Vec<_>>(),
            developer
        );
        assert!(
            !section(&workspace, &developer[0]).expanded,
            "a developer section starts collapsed"
        );
    }

    /// A module's declared picker is a control of its own panel: it names the mode, carries its
    /// letter, reads selected exactly while that mode is active, and its section re-derives when
    /// the mode changes so the button on screen is never a frame behind the session.
    #[test]
    fn a_declared_picker_is_a_control_of_its_module_and_follows_the_workspace_mode() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        scene.developer = true;
        let mut workspace = Workspace::default();
        workspace.derive(&scene.inputs());
        let basic = section(&workspace, "luxforge.basic");
        let picker = *basic
            .pickers()
            .first()
            .expect("the Basic module declares a picker");
        assert_eq!(picker.module_id, "luxforge.basic");
        assert_eq!(picker.label, "Neutral picker");
        assert_eq!(
            picker.title, "Neutral picker",
            "the tooltip names the declared canvas mode"
        );
        assert_eq!(picker.shortcut.as_deref(), Some("W"));
        assert!(!picker.selected, "the pointer is the mode on opening");
        assert!(picker.enabled, "an editable section offers its picker");

        // It sits inside the White balance group, with the two fields a pick fills.
        let group = basic
            .controls
            .iter()
            .find_map(|control| match control {
                ControlModel::Group(group) if group.label == "White balance" => Some(group),
                _ => None,
            })
            .expect("the White balance group");
        assert!(
            group
                .controls
                .iter()
                .any(|control| matches!(control, ControlModel::Picker(_))),
            "the picker is a control of the group whose fields it sets"
        );
        assert_eq!(
            group.state,
            Some(tools::GroupState::Original),
            "a picker is not a value, so it does not make the group Custom"
        );

        // Every declaring module gets one, and only the module whose mode is active reads selected.
        let versions: Vec<(String, u64)> = workspace
            .tools
            .all()
            .map(|section| (section.module_id.clone(), section.version))
            .collect();
        scene.session.workspace.mode = "luxforge.basic".into();
        workspace.derive(&scene.inputs());
        assert!(
            section(&workspace, "luxforge.basic").pickers()[0].selected,
            "the picker reads selected while its own mode is active"
        );
        for (id, before) in versions {
            let after = section(&workspace, &id).version;
            if id == "luxforge.basic" {
                assert_eq!(
                    after,
                    before + 1,
                    "{id} re-derives when its mode is entered"
                );
            } else {
                assert_eq!(after, before, "{id} is untouched by another module's mode");
            }
        }
        assert!(
            !section(&workspace, "luxforge.pixel").pickers()[0].selected,
            "another module's picker is not selected by the Basic mode"
        );
    }

    #[test]
    fn a_field_change_re_derives_only_its_own_section() {
        let modules = descriptors();
        let (action, x, _) = tools::point_pick(&modules).expect("a canvas pick");
        let (action, x) = (action.to_owned(), x.to_owned());
        let pixel = modules
            .iter()
            .find(|module| module.action(&action).is_some())
            .expect("the declaring module")
            .id
            .clone();
        let other = modules
            .iter()
            .find(|module| module.id != pixel && module.applies_to(luxforge_core::SourceTag::Jpeg))
            .expect("a second module")
            .id
            .clone();
        let mut scene = Scene::new(modules).opened(Vec::new());
        scene.developer = true;
        let mut workspace = Workspace::default();
        workspace.derive(&scene.inputs());
        let before = (
            section(&workspace, &pixel).version,
            section(&workspace, &other).version,
        );

        // Deriving again with the same inputs changes nothing at all.
        workspace.derive(&scene.inputs());
        assert_eq!(
            (
                section(&workspace, &pixel).version,
                section(&workspace, &other).version
            ),
            before,
            "an unchanged section is not re-derived"
        );

        scene.fields.set(&action, &x, "42".into());
        workspace.derive(&scene.inputs());
        assert_eq!(
            section(&workspace, &pixel).version,
            before.0 + 1,
            "the module whose field changed is re-derived"
        );
        assert_eq!(
            section(&workspace, &other).version,
            before.1,
            "every other section keeps its version"
        );
    }

    /// A refresh builds exactly the sections whose inputs moved: none when nothing did, the status
    /// bar alone for a new status line, the tools panel alone for a field whose stamp moved, and
    /// every section on a new session. What it keeps is what a fresh derivation would build.
    #[test]
    fn a_refresh_builds_only_the_sections_whose_inputs_moved() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        let mut stamps = Stamps::fresh();
        let mut workspace = Workspace::default();
        let mut built = Built::default();
        let refresh =
            |scene: &Scene, stamps: Stamps, workspace: &mut Workspace, built: &mut Built| {
                let before = built.builds;
                let mut inputs = scene.inputs();
                inputs.stamps = stamps;
                workspace.refresh(&inputs, built);
                built.builds - before
            };
        assert_eq!(refresh(&scene, stamps, &mut workspace, &mut built), 8);
        assert_eq!(workspace, scene.derive(), "the first refresh builds it all");
        assert_eq!(
            refresh(&scene, stamps, &mut workspace, &mut built),
            0,
            "nothing moved, so nothing is built"
        );

        scene.status = "Something else".into();
        assert_eq!(refresh(&scene, stamps, &mut workspace, &mut built), 1);
        assert_eq!(workspace.status.message, "Something else");

        stamps.fields = tracked::stamp();
        assert_eq!(
            refresh(&scene, stamps, &mut workspace, &mut built),
            1,
            "a field's stamp reaches the tools panel alone"
        );

        stamps.session = tracked::stamp();
        assert_eq!(
            refresh(&scene, stamps, &mut workspace, &mut built),
            8,
            "every section reads the session"
        );
        assert_eq!(workspace, scene.derive());
    }

    /// The debug build's check is what keeps the keys honest: a section kept while an input it
    /// shows changed without its key moving fails the derivation instead of drawing a stale panel.
    #[test]
    #[should_panic(expected = "a section key misses an input")]
    fn a_kept_section_that_differs_from_a_fresh_one_fails_a_debug_build() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        let stamps = Stamps::fresh();
        let mut workspace = Workspace::default();
        let mut built = Built::default();
        let mut inputs = scene.inputs();
        inputs.stamps = stamps;
        workspace.refresh(&inputs, &mut built);
        // The history page changed with its stamp held still, which no tracked value allows.
        scene.history.entries.clear();
        let mut inputs = scene.inputs();
        inputs.stamps = stamps;
        workspace.refresh(&inputs, &mut built);
    }

    #[test]
    fn a_section_is_active_only_when_a_layer_of_its_effects_does_something() {
        let crop = crop_descriptor();
        let neutral = Scene::new(vec![crop.clone()]).opened(vec![crop_layer(CropPayload::NEUTRAL)]);
        assert!(
            !section(&neutral.derive(), &crop.id).active,
            "a whole-image crop is stored but is not an edit"
        );
        let cropped = Scene::new(vec![crop.clone()]).opened(vec![crop_layer(CropPayload {
            angle: 0.0,
            x: 0.1,
            y: 0.1,
            width: 0.5,
            height: 0.5,
        })]);
        assert!(section(&cropped.derive(), &crop.id).active);

        // The orientation layer is the same story: four quarter turns leave a layer holding the
        // identity, which is stored but is not an edit; anything else is.
        let transforms = descriptors()
            .into_iter()
            .find(|module| {
                module
                    .effects
                    .iter()
                    .any(|effect| effect.id == luxforge_core::ORIENTATION_EFFECT)
            })
            .expect("the registered transform module");
        let oriented = |orientation| {
            Scene::new(vec![transforms.clone()])
                .opened(vec![luxforge_core::Layer::orientation(orientation)])
        };
        assert!(
            !section(&oriented(Orientation::NEUTRAL).derive(), &transforms.id).active,
            "the neutral orientation is stored but is not an edit"
        );
        for turned in [
            Orientation {
                mirror: false,
                turns: 1,
            },
            Orientation {
                mirror: true,
                turns: 0,
            },
        ] {
            assert!(
                section(&oriented(turned).derive(), &transforms.id).active,
                "{turned:?} changes the image"
            );
        }
    }

    /// Every RAW recipe holds its development layer from the Original on, and on a RAW photo
    /// Basic's White balance controls edit it, so Basic's dot and its White balance caption ask the
    /// core whether that layer does anything: an untouched RAW, at As shot, has neither; a custom
    /// temperature and tint and a neutral pick each light both. No RAW section is drawn at all.
    #[test]
    fn basics_dot_follows_the_raw_development_its_white_balance_edits() {
        use crate::state::testing::{Z6_AS_SHOT, Z6_CAM_XYZ, raw_source};
        use luxforge_core::{RawPayload, WhiteBalanceMode};
        let modules = descriptors();
        let basic = "luxforge.basic";
        let derive = |payload: &RawPayload| {
            let mut scene = Scene::new(modules.clone())
                .opened(vec![payload.layer(luxforge_core::LayerId::new())]);
            scene.state.as_mut().expect("an asset").asset.source = raw_source();
            scene.derive()
        };
        let white_balance = |workspace: &Workspace| {
            section(workspace, basic)
                .controls
                .iter()
                .find_map(|control| match control {
                    ControlModel::Group(group) if group.label == "White balance" => group.state,
                    _ => None,
                })
                .expect("the White balance group's caption")
        };
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let untouched = derive(&original);
        assert!(
            !section(&untouched, basic).active,
            "an untouched RAW is not an edit"
        );
        assert_eq!(white_balance(&untouched), tools::GroupState::Original);
        // A Temperature drag has left the committed As shot before its release commits it, as a
        // dragged JPEG field has left its default.
        let dragged = {
            let mut scene = Scene::new(modules.clone())
                .opened(vec![original.layer(luxforge_core::LayerId::new())]);
            scene.state.as_mut().expect("an asset").asset.source = raw_source();
            scene.dragging = Some(("set-raw".into(), "temperature".into()));
            scene.derive()
        };
        assert_eq!(white_balance(&dragged), tools::GroupState::Custom);
        assert!(
            !section(&dragged, basic).active,
            "the dot follows what is committed"
        );
        assert_eq!(
            untouched.tools.all().count(),
            modules
                .iter()
                .filter(|module| module.applies_to(luxforge_core::SourceTag::Raw))
                .filter(|module| !module.developer && tools::draws_section(module))
                .count(),
            "a module with nothing of its own to draw has no section"
        );
        // Crop declares no controls but draws the host's crop frame; the RAW development draws
        // nothing of its own.
        let listed: Vec<&str> = untouched
            .tools
            .all()
            .map(|section| section.module_id.as_str())
            .collect();
        assert!(listed.contains(&"luxforge.crop"), "{listed:?}");
        assert!(!listed.contains(&"luxforge.raw"), "{listed:?}");

        let custom = RawPayload {
            wb_mode: WhiteBalanceMode::Custom,
            temperature_kelvin: Some(5000.0),
            tint: Some(12.0),
            gains: luxforge_core::gains_from_temperature_tint(5000.0, 12.0, Z6_CAM_XYZ).unwrap(),
            ..original.clone()
        };
        let picked = RawPayload {
            wb_mode: WhiteBalanceMode::Custom,
            gains: [1.9, 1.0, 1.4],
            ..original.clone()
        };
        for (edit, payload) in [("custom white balance", &custom), ("neutral pick", &picked)] {
            let workspace = derive(payload);
            assert!(section(&workspace, basic).active, "{edit} is an edit");
            assert_eq!(
                white_balance(&workspace),
                tools::GroupState::Custom,
                "{edit}"
            );
        }
    }

    /// Parity: a JPEG, a RAW photo's global target and a RAW photo's mask target derive Basic's
    /// section with the same labels in the same order. Units and ranges differ only where a
    /// variant declares them, and every control addresses exactly what the core's one resolver
    /// answers for that photo and target: the action and parameter a slider edits, the action and
    /// preset a button runs, the canvas a picker enters and the reset a group header runs. The
    /// picker answers to the same letter on every kind, and so does the keyboard.
    #[test]
    fn a_jpeg_and_a_raw_photo_derive_one_basic_section() {
        use luxforge_core::{Control, SourceTag, resolve_control, resolve_group_reset};
        let modules = descriptors();
        let basic = modules
            .iter()
            .find(|module| module.id == "luxforge.basic")
            .expect("Basic")
            .clone();
        let mask = MaskId::new();
        // (name, kind, the mask the sections are bound to)
        let targets = [
            ("JPEG", SourceTag::Jpeg, None),
            ("RAW global", SourceTag::Raw, None),
            ("RAW mask", SourceTag::Raw, Some(mask.clone())),
        ];
        let mut derived = Vec::new();
        for (name, kind, target) in &targets {
            let mut scene = Scene::new(modules.clone()).opened(Vec::new());
            if *kind == SourceTag::Raw {
                scene.state.as_mut().expect("an asset").asset.source =
                    crate::state::testing::raw_source();
            }
            if let Some(mask) = target {
                scene.session.workspace.mode = luxforge_core::MASK_MODE.into();
                scene.selected_mask = Some(mask.clone());
            }
            let workspace = scene.derive();
            let shortcuts =
                tools::mode_shortcuts(&scene.modules, scene.state.as_ref(), target.as_ref());
            let section = section(&workspace, &basic.id).clone();
            derived.push((*name, *kind, target.clone(), section, shortcuts));
        }
        // What one model shows and what it addresses.
        let shown = |control: &ControlModel| -> (String, String) {
            match control {
                ControlModel::Group(group) => ("group".into(), group.label.clone()),
                ControlModel::Slider(slider) => ("number".into(), slider.label.clone()),
                ControlModel::Action(action) => ("action".into(), action.label.clone()),
                ControlModel::Picker(picker) => ("picker".into(), picker.label.clone()),
                other => panic!("Basic draws no {other:?}"),
            }
        };
        let labels: Vec<Vec<(String, String)>> = derived
            .iter()
            .map(|(_, _, _, section, _)| {
                crate::state::control_tree::walk(&section.controls)
                    .map(shown)
                    .collect()
            })
            .collect();
        assert!(labels[0].iter().any(|(_, label)| label == "As shot"));
        for (at, (name, ..)) in derived.iter().enumerate().skip(1) {
            assert_eq!(
                labels[at], labels[0],
                "{name}: the same labels in the same order"
            );
        }

        for (name, kind, target, section, shortcuts) in &derived {
            let declared: Vec<&Control> =
                crate::state::control_tree::walk(&basic.controls).collect();
            let models: Vec<&ControlModel> =
                crate::state::control_tree::walk(&section.controls).collect();
            assert_eq!(declared.len(), models.len(), "{name}");
            let mut variants = 0;
            for (control, model) in declared.iter().zip(&models) {
                let resolved = resolve_control(&basic.id, control, Some(*kind), target.as_ref());
                variants += usize::from(resolved.variant);
                let provider = modules
                    .iter()
                    .find(|module| module.id == resolved.module)
                    .expect("the providing module");
                match (resolved.control, model) {
                    (
                        Control::Number {
                            action, parameter, ..
                        },
                        ControlModel::Slider(slider),
                    ) => {
                        assert_eq!(
                            (slider.action.as_str(), slider.parameter.as_str()),
                            (action.as_str(), parameter.as_str()),
                            "{name}: {}",
                            slider.label
                        );
                        // The unit and range are the providing action's own; they differ from
                        // the base only where a variant provides the control.
                        let declared = provider
                            .action(action)
                            .and_then(|action| action.parameter(parameter))
                            .expect("the declared parameter");
                        assert_eq!(slider.unit, declared.unit, "{name}: {}", slider.label);
                        let base = match control {
                            Control::Number {
                                action, parameter, ..
                            } => basic.action(action).unwrap().parameter(parameter).unwrap(),
                            _ => unreachable!(),
                        };
                        if !resolved.variant {
                            assert_eq!(slider.unit, base.unit, "{name}: {}", slider.label);
                            assert_eq!(
                                (slider.spec.min, slider.spec.max),
                                crate::state::number::NumberSpec::of(base)
                                    .map(|spec| (spec.min, spec.max))
                                    .unwrap()
                            );
                        }
                    }
                    (Control::Action { action, preset, .. }, ControlModel::Action(model)) => {
                        assert_eq!(
                            (&model.action, &model.preset),
                            (action, preset),
                            "{name}: {}",
                            model.label
                        );
                    }
                    (Control::Picker { .. }, ControlModel::Picker(picker)) => {
                        assert_eq!(picker.module_id, resolved.module, "{name}: the picker");
                        // One letter, and the keyboard enters the mode the picker enters.
                        assert_eq!(picker.shortcut.as_deref(), Some("W"), "{name}");
                        assert!(
                            shortcuts.contains(&('W', picker.module_id.clone())),
                            "{name}: W enters {}: {shortcuts:?}",
                            picker.module_id
                        );
                    }
                    (Control::Group { .. }, ControlModel::Group(group)) => {
                        let reset =
                            resolve_group_reset(&basic.id, control, Some(*kind), target.as_ref())
                                .map(|resolved| tools::ResetRef {
                                    action: resolved.reset.action.clone(),
                                    preset: resolved.reset.preset.clone(),
                                });
                        assert_eq!(group.reset, reset, "{name}: {}", group.label);
                    }
                    (control, model) => panic!("{name}: {control:?} drawn as {model:?}"),
                }
            }
            // Only the RAW global target resolves any variant: White balance's four controls.
            let expected = if *kind == SourceTag::Raw && target.is_none() {
                4
            } else {
                0
            };
            assert_eq!(variants, expected, "{name}");
        }

        // What the variants change, read off the models: Temperature is in kelvin on RAW's
        // global target alone.
        let temperature = |at: usize| {
            crate::state::control_tree::walk(&derived[at].3.controls)
                .find_map(|control| match control {
                    ControlModel::Slider(slider) if slider.label == "Temperature" => {
                        Some((slider.action.clone(), slider.unit.clone()))
                    }
                    _ => None,
                })
                .expect("Temperature")
        };
        assert_eq!(temperature(0), ("set-basic".into(), None));
        assert_eq!(temperature(1), ("set-raw".into(), Some("K".into())));
        assert_eq!(temperature(2), ("set-basic".into(), None));
    }

    /// A field-patch layer returned to its neutral values stays in the stack but is not an edit, so
    /// its band has no dot; any field that changes the picture lights it. Neutrality is the core's
    /// answer on each `recipe.describe` row, which is what makes the vignette's rule (amount 0,
    /// whatever its shape) come out right with no payload parsing here.
    #[test]
    fn a_field_patch_section_has_no_dot_once_its_layer_is_neutral() {
        let modules = descriptors();
        for (module_id, effect, neutral, edited) in [
            (
                "luxforge.basic",
                luxforge_core::BASIC_EFFECT,
                json!({"exposure": 0.0}),
                json!({"exposure": 0.5}),
            ),
            (
                "luxforge.presence",
                luxforge_core::PRESENCE_EFFECT,
                json!({}),
                json!({"clarity": -20}),
            ),
            (
                "luxforge.mixer",
                luxforge_core::MIXER_EFFECT,
                json!({"red-hue": 0}),
                json!({"blue-saturation": 30}),
            ),
            (
                "luxforge.vignette",
                luxforge_core::VIGNETTE_EFFECT,
                json!({"midpoint": 70, "roundness": -40}),
                json!({"amount": -25}),
            ),
        ] {
            let module = modules
                .iter()
                .find(|module| module.id == module_id)
                .expect("a registered module")
                .clone();
            let dot = |payload: &serde_json::Value| {
                let scene = Scene::new(vec![module.clone()])
                    .opened(vec![luxforge_core::Layer::new(effect, payload.clone())]);
                section(&scene.derive(), module_id).active
            };
            assert!(
                !dot(&neutral),
                "{module_id}: {neutral} is stored but not an edit"
            );
            assert!(dot(&edited), "{module_id}: {edited} is an edit");
        }
    }

    /// The dot follows the current entry's rows, not the displayed entry's: previewing an older
    /// entry whose Basic layer was an edit leaves the dot as the current, reset layer has it.
    #[test]
    fn the_dot_follows_the_current_entry_not_a_historical_preview() {
        let basic = descriptors()
            .into_iter()
            .find(|module| module.id == "luxforge.basic")
            .expect("the registered Basic module");
        let mut scene = Scene::new(vec![basic.clone()]).opened(vec![luxforge_core::Layer::new(
            luxforge_core::BASIC_EFFECT,
            json!({}),
        )]);
        let current = scene
            .state
            .as_ref()
            .expect("an asset")
            .current_entry
            .clone();
        let mut older = entry(&current.asset_id, 2, None);
        older.snapshot = older
            .snapshot
            .append(luxforge_core::Layer::new(
                luxforge_core::BASIC_EFFECT,
                json!({"exposure": 1.0}),
            ))
            .expect("a valid stack");
        scene.session.preview.selection = luxforge_core::HistorySelection::Entry(older.id.clone());
        scene.display_entry = Some(older.id.clone());
        scene.recipe = Some(crate::state::testing::described(&older));
        assert!(
            !section(&scene.derive(), &basic.id).active,
            "the previewed edit does not light the current entry's dot"
        );
        // And the other way round: a current edit keeps its dot while a neutral entry is shown.
        let mut edited = Scene::new(vec![basic.clone()]).opened(vec![luxforge_core::Layer::new(
            luxforge_core::BASIC_EFFECT,
            json!({"exposure": 1.0}),
        )]);
        let mut neutral = entry(&current.asset_id, 1, None);
        neutral.snapshot = neutral
            .snapshot
            .append(luxforge_core::Layer::new(
                luxforge_core::BASIC_EFFECT,
                json!({}),
            ))
            .expect("a valid stack");
        edited.session.preview.selection =
            luxforge_core::HistorySelection::Entry(neutral.id.clone());
        edited.display_entry = Some(neutral.id.clone());
        edited.recipe = Some(crate::state::testing::described(&neutral));
        assert!(
            section(&edited.derive(), &basic.id).active,
            "the current edit keeps its dot during a preview"
        );
    }

    /// The strip holds the pointer, the canvas-takeover modes and the view overlays, and nothing
    /// else. A pick mode takes no canvas over: it belongs beside the controls its pick fills, so it
    /// is reached from its module's own picker control and never appears here.
    #[test]
    fn the_mode_strip_lists_the_pointer_then_every_declared_crop_frame() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        let strip = scene.derive().canvas.modes;
        assert_eq!(strip[0].id, POINTER_MODE);
        assert_eq!(strip[0].shortcut.as_deref(), Some("V"));
        assert!(strip[0].selected, "the pointer is the default mode");
        assert_eq!(strip[0].icon.as_deref(), Some("pointer"));
        // Mask is the host's own takeover mode: a mask is a host object in the recipe, so it is
        // offered whatever modules are registered and no module declares its canvas.
        let mask = &strip[1];
        assert_eq!(mask.id, luxforge_core::MASK_MODE);
        assert_eq!(mask.label, "Mask");
        assert_eq!(mask.shortcut.as_deref(), Some("M"));
        assert_eq!(mask.icon.as_deref(), Some("mask"));
        let crop = strip
            .iter()
            .find(|mode| mode.id == "luxforge.crop")
            .expect("the crop module declares a canvas mode");
        assert_eq!(crop.label, "Crop", "the descriptor's own canvas title");
        assert_eq!(crop.shortcut.as_deref(), Some("R"));
        assert_eq!(
            crop.icon.as_deref(),
            Some("crop"),
            "the descriptor's own icon"
        );
        assert!(crop.enabled);
        assert_eq!(strip[2].id, crop.id, "the modules' modes follow Mask");
        assert_eq!(
            strip.len(),
            3,
            "the pointer, the mask mode and the crop frame alone: {:?}",
            strip.iter().map(|mode| &mode.id).collect::<Vec<_>>()
        );
        // Every module that declares a pick — a point pick or a sample apply — stays out, for
        // every run, including one that asked for developer tools.
        let picks: Vec<String> = scene
            .modules
            .iter()
            .filter(|module| {
                matches!(
                    module.canvas,
                    Some(luxforge_core::CanvasInteraction::PointPick { .. })
                        | Some(luxforge_core::CanvasInteraction::SampleApply { .. })
                )
            })
            .map(|module| module.id.clone())
            .collect();
        assert!(
            picks.iter().any(|id| id == "luxforge.basic"),
            "the Basic module declares the neutral picker: {picks:?}"
        );
        scene.developer = true;
        let strip = scene.derive().canvas.modes;
        for id in &picks {
            assert!(
                !strip.iter().any(|mode| &mode.id == id),
                "{id} is a pick mode and is reached from its own panel, not the strip"
            );
        }
        // An unavailable module offers no mode at all.
        scene.modules = vec![ModuleDescriptor {
            availability: Availability::Unavailable {
                reason: "disabled by --disable-module".into(),
            },
            ..crop_descriptor()
        }];
        assert_eq!(
            scene.derive().canvas.modes.len(),
            2,
            "the pointer and the host's mask mode alone"
        );
    }

    #[test]
    fn the_draft_bar_reads_out_the_draft_and_shows_the_apps_refusal() {
        let crop = crop_descriptor();
        let mut scene = Scene::new(vec![crop.clone()]).opened(Vec::new());
        scene.session.workspace.mode = crop.id.clone();
        scene.draft = Some(CropDraft::neutral(
            luxforge_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            0,
        ));
        let bar = scene.derive().canvas.draft_bar.expect("an open draft");
        assert_eq!(bar.title, "Crop");
        assert_eq!(bar.readout, "480 × 320 px · 0°");
        assert!(bar.can_apply && bar.apply_reason.is_none());
        // The crop's title says what it edits, and it ends with Cancel and Apply.
        assert_eq!((bar.subject, bar.kind, bar.done), (None, None, false));

        // The bar decides nothing itself: Apply reads the app's one release refusal, whatever it
        // is, conflicted or not.
        scene.crop_conflicted = true;
        scene.apply_refusal =
            Some("Changed elsewhere: discard the crop draft or reapply it".into());
        let bar = scene.derive().canvas.draft_bar.expect("an open draft");
        assert!(!bar.can_apply && bar.conflicted);
        assert_eq!(
            bar.apply_reason.as_deref(),
            Some("Changed elsewhere: discard the crop draft or reapply it")
        );
        scene.crop_conflicted = false;
        scene.apply_refusal = Some("Waiting for the last request".into());
        let bar = scene.derive().canvas.draft_bar.expect("an open draft");
        assert!(!bar.can_apply && !bar.conflicted);
        assert_eq!(
            bar.apply_reason.as_deref(),
            Some("Waiting for the last request")
        );
    }

    /// One mask, `Face`, holding a radial `Radial 1` in subtract mode after an add, and a brush.
    fn face() -> (MaskListing, MaskId, ComponentId, ComponentId) {
        use luxforge_core::mask::commands::{ComponentReport, MaskReport};
        let (mask, radial, brush) = (MaskId::new(), ComponentId::new(), ComponentId::new());
        let component = |id: &ComponentId, index, name: &str, mode, kind: &str| ComponentReport {
            id: id.clone(),
            index,
            name: name.into(),
            mode,
            invert: false,
            kind: kind.into(),
            payload: json!({}),
            available: true,
            strokes: Vec::new(),
        };
        let listing = MaskListing {
            entry_id: EntryId::new(),
            masks: vec![MaskReport {
                id: mask.clone(),
                index: 0,
                name: "Face".into(),
                amount: 100.0,
                invert: false,
                components: vec![
                    component(
                        &ComponentId::new(),
                        0,
                        "Linear 1",
                        ComponentMode::Add,
                        "linear",
                    ),
                    component(&radial, 1, "Radial 1", ComponentMode::Subtract, "radial"),
                    component(&brush, 2, "Brush 1", ComponentMode::Add, "brush"),
                ],
                layers: Vec::new(),
            }],
        };
        (listing, mask, radial, brush)
    }

    /// A mask gesture's bar leads with the mask, names the component and its mode beside its kind,
    /// and reads out the kind's own numbers to their declared precision with a true minus sign.
    #[test]
    fn a_mask_gestures_bar_names_the_mask_the_component_and_its_mode() {
        use crate::mask_draft::{BRUSH, LINEAR, MaskDraft, NEUTRAL_BRUSH, RADIAL};
        let (listing, mask, radial, brush) = face();
        let mut scene = Scene::new(Vec::new()).opened(Vec::new());
        scene.session.workspace.mode = luxforge_core::MASK_MODE.into();
        scene.masks = Some(listing);
        scene.selected_mask = Some(mask.clone());

        // Adding a radial to Face: the component has no name until the commit spends its
        // ordinal, so it is named by its kind.
        let mut adding = MaskDraft::adding(mask.clone(), RADIAL, ComponentMode::Add, NEUTRAL_BRUSH)
            .expect("a drawn kind");
        for (name, value) in [
            ("radius_x", 0.18),
            ("radius_y", 0.24),
            ("angle", -12.0),
            ("feather", 60.0),
        ] {
            assert!(adding.set_field(name, value), "{name}");
        }
        scene.mask_draft = Some(adding);
        let bar = scene.derive().canvas.draft_bar.expect("an open gesture");
        assert_eq!(bar.title, "Face");
        assert_eq!(bar.subject.as_deref(), Some("Radial · Add"));
        assert_eq!(bar.kind, Some(RADIAL));
        // A readout, not an entry field: compact, as the board draws it, while the panel's fields
        // keep the declared precision.
        assert_eq!(bar.readout, "0.180 × 0.240 · \u{2212}12° · feather 60");
        assert!(bar.can_apply && !bar.done);

        // Editing the stored Radial 1 names it, with the mode it holds.
        scene.mask_draft = Some(
            MaskDraft::editing(
                mask.clone(),
                radial,
                RADIAL,
                &json!(crate::mask_draft::NEUTRAL_RADIAL),
                NEUTRAL_BRUSH,
            )
            .expect("a drawn kind"),
        );
        let bar = scene.derive().canvas.draft_bar.expect("an open gesture");
        assert_eq!(
            (bar.title.as_str(), bar.subject.as_deref()),
            ("Face", Some("Radial 1 · Subtract"))
        );

        // A new mask has no name until it is committed; a linear reads out its axis.
        let mut creating = MaskDraft::creating(LINEAR, NEUTRAL_BRUSH).expect("a drawn kind");
        creating.sweep((0.1, 0.92), (0.14, 0.38));
        creating.end();
        scene.mask_draft = Some(creating);
        let bar = scene.derive().canvas.draft_bar.expect("an open gesture");
        assert_eq!(bar.title, "New mask");
        assert_eq!(bar.subject.as_deref(), Some("Linear · Add"));
        assert_eq!(bar.readout, "0.100, 0.920 → 0.140, 0.380");

        // A brush ends with Done: each stroke committed on release, so an Apply refusal is not the
        // bar's to state. Idle, it reads the brush the next stroke takes; with the stroke down,
        // that it is painting.
        scene.apply_refusal =
            Some("Changed elsewhere: discard the mask gesture or reapply it".into());
        let held = crate::mask_draft::Brush {
            size: 0.06,
            feather: 50.0,
            ..NEUTRAL_BRUSH
        };
        let mut painting =
            MaskDraft::editing(mask, brush, BRUSH, &Value::Null, held).expect("a drawn kind");
        scene.mask_draft = Some(painting.clone());
        let bar = scene.derive().canvas.draft_bar.expect("an open gesture");
        assert_eq!(bar.subject.as_deref(), Some("Brush 1 · Add"));
        assert!(bar.done && bar.can_apply && bar.apply_reason.is_none());
        assert_eq!(bar.readout, "size 0.060 · feather 50");
        painting.paint_begin((0.5, 0.5));
        scene.mask_draft = Some(painting);
        let bar = scene.derive().canvas.draft_bar.expect("an open gesture");
        assert_eq!(bar.readout, "painting");
    }

    #[test]
    fn a_render_failure_becomes_the_notice_that_names_its_cause() {
        let unavailable = ModuleDescriptor {
            availability: Availability::Unavailable {
                reason: "disabled by --disable-module".into(),
            },
            ..crop_descriptor()
        };
        let effect = unavailable.effects[0].id.clone();
        let mut scene = Scene::new(vec![unavailable]).opened(Vec::new());
        // An unavailable provider on its own is reported by its section header, not by a notice.
        assert!(scene.derive().canvas.notices.is_empty());

        scene.render_error = Some(luxforge_core::Error::unavailable_effect(&effect, &["l1"]));
        let notice = &scene.derive().canvas.notices[0];
        assert_eq!(notice.title, "Preview is stale");
        assert_eq!(
            notice.body, "Crop is unavailable: disabled by --disable-module",
            "the notice names the module and the reason, not the effect identity"
        );
        assert!(notice.actions.is_empty(), "Locate is a later feature");

        // A layer nothing provides is still named, by its effect identity.
        scene.render_error = Some(luxforge_core::Error::unavailable_effect(
            "other.effect",
            &["l1"],
        ));
        assert_eq!(
            scene.derive().canvas.notices[0].body,
            "No registered module provides other.effect"
        );

        for (kind, title) in [
            (
                luxforge_core::ErrorKind::SourceUnavailable,
                "Original not found",
            ),
            (luxforge_core::ErrorKind::FileAccess, "Original not found"),
            (luxforge_core::ErrorKind::ResourceLimit, "Rendering limit"),
        ] {
            scene.render_error = Some(luxforge_core::Error::new(kind, "the detail"));
            let notice = &scene.derive().canvas.notices[0];
            assert_eq!(notice.title, title, "{kind:?}");
            assert_eq!(notice.body, "the detail");
            assert!(notice.actions.is_empty());
        }
        // A kind the workspace has nothing to say about is left to the status bar.
        scene.render_error = Some(luxforge_core::Error::internal("boom"));
        assert!(scene.derive().canvas.notices.is_empty());
    }

    /// The canvas reads what a failure is from its kind and data, never its message: the stale
    /// notice follows the effect the data names whatever the message says, and a message that
    /// merely reads like an unavailable effect, with no data behind it, is not one.
    #[test]
    fn a_render_failure_is_read_from_its_data_not_its_message() {
        let unavailable = ModuleDescriptor {
            availability: Availability::Unavailable {
                reason: "disabled by --disable-module".into(),
            },
            ..crop_descriptor()
        };
        let effect = unavailable.effects[0].id.clone();
        let mut scene = Scene::new(vec![unavailable]).opened(Vec::new());

        scene.render_error = Some(
            luxforge_core::Error::incompatible("a reworded refusal")
                .with_data(serde_json::json!({ "effect_id": effect })),
        );
        let notice = &scene.derive().canvas.notices[0];
        assert_eq!(notice.title, "Preview is stale");
        assert_eq!(
            notice.body,
            "Crop is unavailable: disabled by --disable-module"
        );

        scene.render_error = Some(luxforge_core::Error::incompatible(format!(
            "unavailable effect {effect} (layers l1)"
        )));
        assert!(
            scene.derive().canvas.notices.is_empty(),
            "an incompatible failure without the data is not an unavailable effect"
        );
    }

    #[test]
    fn a_render_failure_before_any_upload_explains_itself_on_the_canvas_placeholder() {
        let unavailable = ModuleDescriptor {
            availability: Availability::Unavailable {
                reason: "disabled by --disable-module".into(),
            },
            ..crop_descriptor()
        };
        let effect = unavailable.effects[0].id.clone();
        let scene = Scene::new(vec![unavailable]).opened(Vec::new());
        let render_error = Some(luxforge_core::Error::unavailable_effect(&effect, &["l1"]));
        let mut inputs = scene.inputs();
        inputs.photo = false;
        inputs.render_error = render_error.as_ref();
        let mut workspace = Workspace::default();
        workspace.derive(&inputs);
        assert_eq!(
            workspace.canvas.photo,
            canvas::PhotoView::Empty(
                "Preview unavailable: Crop is unavailable: disabled by --disable-module".into()
            ),
            "a photograph is open but nothing has ever rendered for it"
        );

        // Nothing open at all still invites opening one, never "unavailable".
        let closed = Scene::new(Vec::new());
        let mut inputs = closed.inputs();
        inputs.photo = false;
        inputs.render_error = render_error.as_ref();
        let mut workspace = Workspace::default();
        workspace.derive(&inputs);
        assert_eq!(
            workspace.canvas.photo,
            canvas::PhotoView::Empty("Open a photograph".into())
        );

        // Once a photograph is actually on the GPU, the placeholder never applies.
        let mut inputs = scene.inputs();
        inputs.render_error = render_error.as_ref();
        let mut workspace = Workspace::default();
        workspace.derive(&inputs);
        assert_eq!(workspace.canvas.photo, canvas::PhotoView::Plain);
    }

    #[test]
    fn the_conflict_notice_names_the_revision_that_arrived() {
        let crop = crop_descriptor();
        let mut scene = Scene::new(vec![crop]).opened(Vec::new());
        let draft = CropDraft::neutral(
            luxforge_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            0,
        );
        scene.crop_conflicted = true;
        scene.draft = Some(draft);
        let notice = &scene.derive().canvas.notices[0];
        assert_eq!(notice.title, "Changed elsewhere");
        assert!(notice.body.contains("revision 3"), "{}", notice.body);
        // The board's card: the accent tone with the spark, and its two actions.
        assert_eq!(notice.tone, crate::state::canvas::NoticeTone::Warning);
        assert_eq!(notice.icon, crate::state::canvas::NoticeIcon::Spark);
        assert_eq!(
            notice
                .actions
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            ["Discard draft", "Reapply"]
        );
    }

    #[test]
    fn the_status_bar_reports_clients_render_state_and_what_the_zoom_means() {
        let scene = Scene::new(vec![crop_descriptor()]).opened(Vec::new());
        let status = scene.derive().status;
        assert_eq!(status.clients, "1 agent connected");
        assert!(status.agents_connected, "the dot is lit");
        assert_eq!(status.render, "Exact render \u{b7} 41 ms");
        // The session's zoom, not the field, and what Fit comes to on this window: a 480 px
        // photograph fitted larger than it is.
        let effective = title::effective_percent(&scene.inputs()).expect("a fitted size");
        assert!(effective > 100.0, "{effective}");
        assert_eq!(
            status.view,
            format!(
                "Fit \u{b7} {} \u{b7} 2\u{d7}",
                title::percent_text(effective)
            )
        );
        assert_eq!(
            scene.derive().title.zoom_percent,
            title::percent_text(effective)
        );

        let mut inputs = scene.inputs();
        inputs.clients = Some(3);
        inputs.rendering = true;
        let mut workspace = Workspace::default();
        workspace.derive(&inputs);
        assert_eq!(workspace.status.clients, "3 agents connected");
        assert_eq!(workspace.status.render, "Rendering…");

        // A display-size proxy on screen says it is approximate beside its own time.
        let mut inputs = scene.inputs();
        inputs.render = Some(status::RenderTime {
            ms: 7.6,
            proxy: true,
            approximate: false,
        });
        workspace.derive(&inputs);
        assert_eq!(workspace.status.render, "Approximate render \u{b7} 8 ms");

        // A drafted RAW white balance approximated on the developed planes says that too.
        let mut inputs = scene.inputs();
        inputs.render = Some(status::RenderTime {
            ms: 9.4,
            proxy: true,
            approximate: true,
        });
        workspace.derive(&inputs);
        assert_eq!(workspace.status.render, "Approximate render \u{b7} 9 ms");

        // Nobody else connected is a count of none, with the dot unlit.
        let mut inputs = scene.inputs();
        inputs.clients = Some(0);
        workspace.derive(&inputs);
        assert_eq!(workspace.status.clients, "No agents connected");
        assert!(!workspace.status.agents_connected);

        // No local server is a stated fact, never a client count of zero.
        let mut inputs = scene.inputs();
        inputs.clients = None;
        inputs.render = None;
        workspace.derive(&inputs);
        assert_eq!(workspace.status.clients, "Live API unavailable");
        assert!(!workspace.status.agents_connected);
        assert_eq!(workspace.status.render, "Idle");
    }

    /// The title bar names the file, its dimensions and the format the core reports, and no colour
    /// space, because the core reports none.
    #[test]
    fn the_title_identity_is_the_dimensions_and_the_reported_format() {
        let scene = Scene::new(vec![crop_descriptor()]).opened(Vec::new());
        let title = scene.derive().title;
        assert_eq!(title.file_name.as_deref(), Some("photo.jpg"));
        assert_eq!(
            title.identity.as_deref(),
            Some("480 \u{d7} 320 \u{b7} JPEG")
        );
        assert_eq!(title.zoom_segment, title::SEGMENT_FIT);
        // An entry with no parent has nothing to undo, and nothing has been undone to redo.
        assert!(!title.can_undo && !title.can_redo);
        let mut undone = Scene::new(vec![crop_descriptor()]).opened(Vec::new());
        let state = undone.state.as_mut().expect("an open photograph");
        state.redo.push(EntryId::new());
        state.current_entry.undo_parent = Some(EntryId::new());
        let title = undone.derive().title;
        assert!(title.can_undo && title.can_redo);
        // An open draft refuses both, so the title bar offers neither.
        undone.slider_draft = Some(("set-basic".into(), "exposure".into(), false));
        let title = undone.derive().title;
        assert!(!title.can_undo && !title.can_redo);
        let mut inputs = scene.inputs();
        inputs.dimensions = None;
        let mut workspace = Workspace::default();
        workspace.derive(&inputs);
        assert_eq!(workspace.title.identity, None);
        assert_eq!(
            workspace.title.zoom_percent, "%",
            "nothing gives Fit a size yet"
        );
    }

    /// The pointer readout is the status bar's, and only the status bar's: moving the pointer onto
    /// the photograph changes nothing in the histogram inspector, so no control in the tools panel
    /// can move, and nothing else in the bar changes either.
    #[test]
    fn the_pointer_readout_is_in_the_status_bar_and_leaves_the_inspector_unchanged() {
        let mut scene = Scene::new(vec![crop_descriptor()]).opened(Vec::new());
        let without = scene.derive();
        assert_eq!(without.status.readout, None);
        scene.readout = Some(histogram::Readout {
            x: 360,
            y: 240,
            rgba: [0, 128, 255, 255],
        });
        let with = scene.derive();
        assert_eq!(
            with.status.readout.as_deref(),
            Some("R 0 \u{b7} G 128 \u{b7} B 255 \u{b7} 360, 240")
        );
        assert_eq!(with.histogram, without.histogram);
        assert_eq!(
            status::StatusBarModel {
                readout: None,
                ..with.status.clone()
            },
            without.status
        );
    }

    #[test]
    fn a_historical_preview_disables_every_control_and_dims_the_reset() {
        let crop = crop_descriptor();
        let mut scene = Scene::new(vec![crop.clone()]).opened(Vec::new());
        assert!(section(&scene.derive(), &crop.id).reset.is_some());
        scene.session.preview.selection = luxforge_core::HistorySelection::Entry(EntryId::new());
        let workspace = scene.derive();
        let section = section(&workspace, &crop.id);
        assert!(!section.enabled);
        assert_eq!(section.disabled_reason.as_deref(), Some(NOT_CURRENT));
        assert!(
            section.reset.is_some(),
            "a section that cannot edit keeps its reset, dimmed, so the header keeps its height"
        );
        assert!(
            section.expanded,
            "the values stay visible while the preview is shown"
        );
        for control in control_tree::walk(&section.controls) {
            match control {
                ControlModel::Action(action) => {
                    assert!(!action.runnable, "{} stayed runnable", action.action)
                }
                ControlModel::CropFrame(frame) => {
                    assert!(!frame.enabled && !frame.can_swap && !frame.can_apply)
                }
                _ => {}
            }
        }
    }

    #[test]
    fn the_empty_workspace_says_what_it_is_waiting_for() {
        let scene = Scene::new(Vec::new());
        let mut workspace = Workspace::default();
        let mut inputs = scene.inputs();
        inputs.modules_ready = false;
        inputs.photo = false;
        workspace.derive(&inputs);
        assert_eq!(
            workspace.tools.status.message(),
            Some("Loading tool modules…")
        );
        assert_eq!(
            workspace.canvas.photo,
            canvas::PhotoView::Empty("Open a photograph".into())
        );
        workspace.derive(&scene.inputs());
        assert_eq!(
            workspace.tools.status.message(),
            Some("No tool modules are available")
        );
        // Snapshot evidence names every section it drew.
        let scene = Scene::new(vec![crop_descriptor()]).opened(Vec::new());
        assert_eq!(scene.derive().expanded(), json!({"luxforge.crop": true}));
    }

    #[test]
    fn every_declared_control_maps_to_plain_data_and_local_curve_state_survives_refresh() {
        let fixture = controls_descriptor();
        let mut scene = Scene::new(vec![fixture.clone(), crop_descriptor()]).opened(Vec::new());
        let mut workspace = Workspace::default();
        workspace.derive(&scene.inputs());
        let fixture_section = section(&workspace, &fixture.id);
        // The fixture declares its controls inside one group, which the panel draws without a
        // header: the group's controls are the section's own rows.
        let controls = &fixture_section.controls;
        assert!(
            !controls
                .iter()
                .any(|control| matches!(control, ControlModel::Group(_))),
            "a module's only group draws no header"
        );
        let ControlModel::Slider(amount) = &controls[0] else {
            panic!("number")
        };
        assert_eq!(
            (
                amount.spec.min,
                amount.spec.max,
                amount.spec.soft_min,
                amount.spec.soft_max
            ),
            (-10.0, 10.0, -5.0, 5.0)
        );
        assert_eq!(
            (amount.spec.step, amount.spec.fine_step, amount.spec.zero),
            (0.1, 0.01, 0.0)
        );
        assert!(matches!(amount.rail, tools::RailStyle::Temperature));
        assert!(
            matches!(controls[1], ControlModel::Slider(ref field) if field.style == tools::NumberControlStyle::Stepper { rail: false })
        );
        assert!(
            matches!(controls[2], ControlModel::Slider(ref field) if field.style == tools::NumberControlStyle::Field)
        );
        assert!(matches!(controls[3], ControlModel::Toggle(ref field) if !field.on));
        assert!(
            matches!(controls[4], ControlModel::Enum(ref field) if field.style == tools::ChoiceControlStyle::Menu)
        );
        assert!(
            matches!(controls[5], ControlModel::Color(ref field) if field.style == tools::ColorControlStyle::Picker && field.rgb == [32,64,128])
        );
        assert!(
            matches!(controls[6], ControlModel::Curve(ref field) if field.channels.len() == 2 && field.sample_query == "fixture-samples" && field.points.len() == 3)
        );
        assert!(
            matches!(controls[7], ControlModel::Action(ref field) if field.style == tools::ActionControlStyle::Icon && field.icon.as_deref() == Some("reset"))
        );
        let crop_version = section(&workspace, "luxforge.crop").version;
        let fixture_version = fixture_section.version;
        scene
            .control_ui
            .curve_channels
            .insert(("fixture-set".into(), "master".into()), 1);
        scene
            .control_ui
            .curve_points
            .insert(("fixture-set".into(), "master".into()), 2);
        scene.control_ui.curve_samples.insert(
            ("fixture-set".into(), "red".into()),
            tools::CurveSamples {
                asset: scene.state.as_ref().unwrap().asset.id.clone(),
                entry: scene.display_entry.clone().unwrap(),
                source: json!([[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]),
                points: vec![[0.0, 0.0], [1.0, 1.0]],
                version: 7,
            },
        );
        // A recorded collapse of the only group changes nothing: that group has no header, so
        // its controls are always shown.
        scene
            .control_ui
            .group_expanded
            .insert(tools::group_key(&fixture.id, &[0]), false);
        workspace.derive(&scene.inputs());
        let fixture_section = section(&workspace, &fixture.id);
        assert_eq!(fixture_section.version, fixture_version + 1);
        assert_eq!(section(&workspace, "luxforge.crop").version, crop_version);
        assert_eq!(
            fixture_section.controls.len(),
            8,
            "every control is still drawn"
        );
        assert!(
            matches!(fixture_section.controls[6], ControlModel::Curve(ref field) if field.selected_channel == 1 && field.selected_point == Some(2) && field.sampled.len() == 2)
        );
        workspace.derive(&scene.inputs());
        assert_eq!(
            section(&workspace, &fixture.id).version,
            fixture_version + 1
        );
        let original_entry = scene.display_entry.replace(EntryId::new()).unwrap();
        workspace.derive(&scene.inputs());
        assert!(
            matches!(section(&workspace, &fixture.id).controls[6], ControlModel::Curve(ref field) if field.sampled.is_empty()),
            "a different entry cannot reuse sampled geometry for identical control points"
        );
        scene.display_entry = Some(original_entry);
        workspace.derive(&scene.inputs());
        scene.fields.set(
            "fixture-set",
            "red",
            "[[0.0,0.0],[0.5,0.7],[1.0,1.0]]".into(),
        );
        workspace.derive(&scene.inputs());
        assert!(
            matches!(section(&workspace, &fixture.id).controls[6], ControlModel::Curve(ref field) if field.sampled.is_empty()),
            "old sampled geometry is hidden until the query matches the current points"
        );
        assert_eq!(section(&workspace, "luxforge.crop").version, crop_version);
    }

    /// A stacked module whose controls are one group draws that group's controls straight under
    /// its band: no sub-group header, no disclosure and no caption, and a recorded collapse of that
    /// group changes nothing. The band keeps the module's reset. Modules with more than one group
    /// keep their headers, a tabbed module keeps its groups as tabs, and the descriptors that
    /// `module.list` returns are untouched.
    #[test]
    fn a_modules_only_group_is_drawn_without_a_header_and_never_collapses() {
        let modules = descriptors();
        let mut scene = Scene::new(modules.clone()).opened(Vec::new());
        scene.developer = true;
        if let Some(state) = &mut scene.state {
            state.asset.source = crate::state::testing::raw_source();
        }
        let workspace = scene.derive();
        let groups = |section: &tools::SectionModel| {
            section
                .controls
                .iter()
                .filter(|control| matches!(control, ControlModel::Group(_)))
                .count()
        };
        for id in [
            "luxforge.transform",
            "luxforge.pixel",
            "luxforge.presence",
            "luxforge.vignette",
        ] {
            let module = modules.iter().find(|module| module.id == id).unwrap();
            let [luxforge_core::Control::Group { controls, .. }] = module.controls.as_slice()
            else {
                panic!("{id} declares exactly one group");
            };
            let drawn = section(&workspace, id);
            assert_eq!(groups(drawn), 0, "{id} draws no sub-group header");
            assert_eq!(
                drawn.controls.len(),
                controls.len(),
                "{id} draws every control of its only group directly"
            );
            assert_eq!(
                drawn.reset.as_ref().map(|reset| reset.action.as_str()),
                module.reset.as_ref().map(|reset| reset.action.as_str()),
                "{id} keeps the band's own reset"
            );
        }
        assert_eq!(groups(section(&workspace, "luxforge.basic")), 3);
        let mixer = section(&workspace, "luxforge.mixer");
        assert_eq!(groups(mixer), 3, "a tabbed module keeps its groups as tabs");
        assert!(matches!(mixer.layout, tools::SectionLayout::Tabs { .. }));

        // The only group has nothing to collapse: a recorded collapse, however it got there,
        // leaves every control drawn.
        let before = section(&workspace, "luxforge.vignette").controls.clone();
        scene
            .control_ui
            .group_expanded
            .insert(tools::group_key("luxforge.vignette", &[0]), false);
        let collapsed = scene.derive();
        assert_eq!(section(&collapsed, "luxforge.vignette").controls, before);
        // A group of a multi-group module still collapses.
        scene
            .control_ui
            .group_expanded
            .insert(tools::group_key("luxforge.basic", &[1]), false);
        let toggled = scene.derive();
        let ControlModel::Group(tone) = &section(&toggled, "luxforge.basic").controls[1] else {
            panic!("Basic's second group")
        };
        assert!(!tone.expanded);
        // The descriptors are what the API lists, unchanged.
        assert_eq!(scene.modules, modules);
    }

    /// A one-group tabbed module is still tabs: the rule is for stacked sections only.
    #[test]
    fn a_tabbed_module_with_one_group_keeps_it() {
        let mut tabs = tabs_descriptor();
        tabs.controls.truncate(1);
        let scene = Scene::new(vec![tabs.clone()]).opened(Vec::new());
        let workspace = scene.derive();
        let drawn = section(&workspace, &tabs.id);
        assert!(matches!(
            drawn.controls.as_slice(),
            [ControlModel::Group(_)]
        ));
        assert_eq!(drawn.layout, tools::SectionLayout::Tabs { selected: 0 });
    }

    /// Selecting a tab in a `layout: tabs` module is per-client view state exactly like a group's
    /// expansion: it re-derives only that section, changes no recipe and issues no command (the
    /// message handler that would send one lives outside this crate's UI-independent state).
    #[test]
    fn selecting_a_tab_rederives_only_its_own_section_and_changes_no_recipe() {
        let tabs = tabs_descriptor();
        let mut scene = Scene::new(vec![tabs.clone(), crop_descriptor()]).opened(Vec::new());
        let mut workspace = Workspace::default();
        workspace.derive(&scene.inputs());
        let tabs_section = section(&workspace, &tabs.id);
        assert_eq!(
            tabs_section.layout,
            tools::SectionLayout::Tabs { selected: 0 },
            "the default tab is the first group"
        );
        let recipe_before = scene
            .state
            .as_ref()
            .map(|state| state.current_entry.snapshot.recipe.layers.clone());
        let tabs_version = tabs_section.version;
        let crop_version = section(&workspace, "luxforge.crop").version;

        scene.control_ui.selected_tab.insert(tabs.id.clone(), 1);
        workspace.derive(&scene.inputs());
        let selected = section(&workspace, &tabs.id);
        assert_eq!(selected.layout, tools::SectionLayout::Tabs { selected: 1 });
        assert_eq!(
            selected.version,
            tabs_version + 1,
            "the tabbed section is re-derived"
        );
        assert_eq!(
            section(&workspace, "luxforge.crop").version,
            crop_version,
            "an unrelated section keeps its version"
        );
        assert_eq!(
            scene
                .state
                .as_ref()
                .map(|state| state.current_entry.snapshot.recipe.layers.clone()),
            recipe_before,
            "selecting a tab changes no recipe"
        );

        // An out-of-range selection, from a descriptor that shrank since it was stored, clamps to
        // the last group rather than panicking or pointing past the end.
        scene.control_ui.selected_tab.insert(tabs.id.clone(), 9);
        workspace.derive(&scene.inputs());
        assert_eq!(
            section(&workspace, &tabs.id).layout,
            tools::SectionLayout::Tabs { selected: 1 },
            "clamped to the last of the two declared groups"
        );
    }

    #[test]
    fn cached_canvas_versions_track_picker_fractions_selection_and_drag_state() {
        let fixture = controls_descriptor();
        let mut scene = Scene::new(vec![fixture.clone()]).opened(Vec::new());
        let mut workspace = Workspace::default();
        workspace.derive(&scene.inputs());
        let versions = |workspace: &Workspace| {
            // The fixture's only group draws no header, so its controls are the section's rows.
            let controls = &section(workspace, &fixture.id).controls;
            let (ControlModel::Color(color), ControlModel::Curve(curve)) =
                (&controls[5], &controls[6])
            else {
                panic!("canvas controls")
            };
            (color.version, curve.version, color.picker_hsv)
        };
        let initial = versions(&workspace);
        scene.dragging = Some(("fixture-set".into(), "rgb".into()));
        workspace.derive(&scene.inputs());
        assert_ne!(versions(&workspace).0, initial.0);
        scene.dragging = Some(("fixture-set".into(), "master".into()));
        workspace.derive(&scene.inputs());
        assert_ne!(versions(&workspace).1, initial.1);
        scene.dragging = None;
        scene
            .control_ui
            .curve_points
            .insert(("fixture-set".into(), "master".into()), 1);
        workspace.derive(&scene.inputs());
        assert_ne!(versions(&workspace).1, initial.1);
        scene.control_ui.picker_hsv.insert(
            ("fixture-set".into(), "rgb".into()),
            tools::PickerHsv {
                rgb: [32, 64, 128],
                hsv: [0.7, 0.75, 0.5],
            },
        );
        workspace.derive(&scene.inputs());
        assert_eq!(versions(&workspace).2, Some([0.7, 0.75, 0.5]));
        assert_ne!(versions(&workspace).0, initial.0);
        scene.fields.set("fixture-set", "rgb", "[0,255,0]".into());
        workspace.derive(&scene.inputs());
        assert_eq!(versions(&workspace).2, None, "the field is authoritative");
    }

    /// The Presets section's model, from the section the registry's presets control generates.
    fn presets_of(workspace: &Workspace) -> &presets::PresetsModel {
        section(workspace, "luxforge.presets")
            .presets()
            .expect("the presets module renders its library")
    }

    fn counts(unsupported: usize, refused: usize) -> luxforge_core::ReportCounts {
        luxforge_core::ReportCounts {
            mapped: 3,
            neutral: 1,
            unsupported,
            refused,
        }
    }

    #[test]
    fn the_library_is_grouped_as_listed_with_its_partial_badges_and_unavailable_reasons() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        let mut legacy = listed("Legacy", "Imported", Some(counts(0, 0)));
        legacy.unavailable = vec!["set-curve".into()];
        scene.presets.adopt(
            vec![
                listed("Clean", "Imported", Some(counts(0, 0))),
                legacy,
                // Groups are unique ignoring case, so this row belongs under the same heading.
                listed("Lossy", "imported", Some(counts(0, 2))),
                listed("Warm", "User presets", None),
            ],
            5,
        );
        let workspace = scene.derive();
        let presets = presets_of(&workspace);
        let headings: Vec<(&str, Vec<&str>)> = presets
            .groups
            .iter()
            .map(|group| {
                (
                    group.name.as_str(),
                    group.rows.iter().map(|row| row.name.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            headings,
            [
                ("Imported", vec!["Clean", "Legacy", "Lossy"]),
                ("User presets", vec!["Warm"])
            ]
        );
        let rows: Vec<_> = presets.rows().collect();
        assert_eq!(
            rows.iter().map(|row| row.partial).collect::<Vec<_>>(),
            [false, false, true, false],
            "only unsupported or refused settings make a preset partial"
        );
        assert_eq!(
            rows[2].counts.as_deref(),
            Some("3 mapped, 1 neutral, 0 unsupported, 2 refused"),
            "the badge's tooltip gives the report counts"
        );
        assert_eq!(rows[3].counts, None, "a native preset has no report");
        assert_eq!(
            rows[1].unavailable.as_deref(),
            Some("Cannot apply: set-curve is unavailable")
        );
        assert!(!rows[1].enabled, "an unavailable preset cannot apply");
        assert!(rows[0].enabled && rows[2].enabled && rows[3].enabled);
        assert_eq!(
            rows.iter().map(|row| row.imported).collect::<Vec<_>>(),
            [true, true, true, false],
            "only an import has a report to copy"
        );
        assert!(!presets.empty && !presets.loading && presets.error.is_none());
    }

    #[test]
    fn loading_empty_and_failed_libraries_say_so() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        let workspace = scene.derive();
        assert!(presets_of(&workspace).loading, "nothing has answered yet");
        assert!(!presets_of(&workspace).empty);
        scene.presets.adopt(Vec::new(), 1);
        let workspace = scene.derive();
        assert!(presets_of(&workspace).empty && !presets_of(&workspace).loading);
        scene.presets.failed("catalog: busy".into());
        let workspace = scene.derive();
        assert_eq!(
            presets_of(&workspace).error.as_deref(),
            Some("catalog: busy")
        );
        // A listing read at an older sequence never replaces a newer one.
        assert!(
            !scene
                .presets
                .adopt(vec![listed("Old", "Imported", None)], 0)
        );
        assert!(
            scene
                .presets
                .adopt(vec![listed("New", "Imported", None)], 2)
        );
        let workspace = scene.derive();
        assert_eq!(
            presets_of(&workspace)
                .rows()
                .next()
                .map(|row| row.name.as_str()),
            Some("New")
        );
        assert!(presets_of(&workspace).error.is_none());
    }

    #[test]
    fn a_preset_applies_only_where_an_edit_could_and_never_over_a_draft() {
        let mut scene = Scene::new(descriptors());
        scene
            .presets
            .adopt(vec![listed("Warm", "User presets", None)], 1);
        let enabled = |scene: &Scene| {
            let workspace = scene.derive();
            let presets = presets_of(&workspace);
            (
                presets.rows().all(|row| row.enabled),
                presets.apply_disabled.clone(),
            )
        };
        assert_eq!(
            enabled(&scene),
            (false, Some("No photograph is open".into()))
        );
        scene = scene.opened(Vec::new());
        scene
            .presets
            .adopt(vec![listed("Warm", "User presets", None)], 1);
        assert_eq!(enabled(&scene), (true, None));
        scene.busy = true;
        assert_eq!(
            enabled(&scene),
            (false, Some("Waiting for the last request".into()))
        );
        scene.busy = false;
        scene.session.preview.selection = luxforge_core::HistorySelection::Entry(EntryId::new());
        assert_eq!(enabled(&scene), (false, Some(NOT_CURRENT.into())));
        scene.session.preview.selection = luxforge_core::HistorySelection::Current;
        scene.draft = Some(CropDraft::neutral(
            luxforge_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            0,
        ));
        assert_eq!(
            enabled(&scene),
            (
                false,
                Some("Finish the open draft before applying a preset".into())
            )
        );
        scene.draft = None;
        assert_eq!(enabled(&scene), (true, None));
    }

    #[test]
    fn the_palette_offers_each_applicable_preset_with_its_rows_own_message() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        let mut legacy = listed("Legacy", "Imported", Some(counts(0, 0)));
        legacy.unavailable = vec!["set-curve".into()];
        scene
            .presets
            .adopt(vec![legacy, listed("Warm", "User presets", None)], 1);
        let workspace = scene.derive();
        let row = presets_of(&workspace)
            .rows()
            .find(|row| row.name == "Warm")
            .expect("the Warm row")
            .clone();
        let entries: Vec<_> = workspace
            .palette
            .entries
            .iter()
            .filter(|entry| entry.label.starts_with("Apply preset: "))
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "one entry per preset that can apply in this build"
        );
        assert_eq!(entries[0].label, "Apply preset: Warm");
        assert_eq!(entries[0].detail, "edit.apply-preset \u{00b7} User presets");
        assert_eq!(
            entries[0].action,
            crate::state::palette::PaletteAction::Run {
                action: "apply-preset".into(),
                preset: row.apply.expect("the row applies"),
            },
            "the palette runs exactly what a click on the row runs"
        );
    }

    #[test]
    fn the_create_form_offers_each_presettable_group_and_is_ready_only_when_complete() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        scene.presets.adopt(Vec::new(), 1);
        let form = |scene: &Scene| presets_of(&scene.derive()).form.clone();
        let opened = form(&scene);
        assert!(!opened.open);
        assert_eq!(opened.group, "User presets");
        assert_eq!(
            opened
                .checks
                .iter()
                .filter(|check| !check.checked)
                .map(|check| check.label.as_str())
                .collect::<Vec<_>>(),
            ["Basic \u{00b7} White balance"],
            "white balance is left out by default"
        );
        assert!(!opened.can_create, "a preset needs a name");
        scene.preset_form.open = true;
        scene.preset_form.name = "Tone only".into();
        assert!(form(&scene).can_create);
        for check in &opened.checks {
            scene.preset_form.checked.insert(check.label.clone(), false);
        }
        assert!(
            !form(&scene).can_create,
            "a preset needs at least one group"
        );
        scene
            .preset_form
            .checked
            .insert("Basic \u{00b7} Tone".into(), true);
        assert!(form(&scene).can_create);
        scene.busy = true;
        assert!(!form(&scene).can_create);
        scene.busy = false;
        scene.presets.pending = true;
        assert!(!form(&scene).can_create, "one library request at a time");
        scene.presets.pending = false;
        scene.state = None;
        assert!(
            !form(&scene).can_create,
            "capture reads the displayed photograph"
        );
    }

    /// What a photo offers follows the source kinds each module's effects declare, and nothing
    /// here names a module: the kind-specific modules are found by their declarations. On a JPEG
    /// and on a RAW photo, a module has a section, palette entries, its mode shortcut, a place in
    /// the mode strip and a pick gate only when it applies to that kind, and every other module
    /// offers the same on both.
    #[test]
    fn sections_palette_shortcuts_and_the_pick_gate_follow_the_declared_sources() {
        use luxforge_core::SourceTag;
        let modules = descriptors();
        let every_kind = |module: &ModuleDescriptor| {
            SourceTag::ALL
                .into_iter()
                .all(|kind| module.applies_to(kind))
        };
        assert!(
            modules
                .iter()
                .any(|module| module.applies_to(SourceTag::Raw)
                    && !module.applies_to(SourceTag::Jpeg)),
            "a registered module applies only to RAW photos"
        );
        // The module that owns a palette entry: the declarer of its action or reset, or its mode.
        let owner = |action: &palette::PaletteAction| -> Option<String> {
            match action {
                palette::PaletteAction::Run { action, .. } => modules
                    .iter()
                    .find(|module| {
                        module.action(action).is_some()
                            || module
                                .reset
                                .as_ref()
                                .is_some_and(|reset| &reset.action == action)
                    })
                    .map(|module| module.id.clone()),
                palette::PaletteAction::Mode(mode) => Some(mode.clone()),
                _ => None,
            }
        };
        let mut offered = Vec::new();
        for kind in SourceTag::ALL {
            let label = kind.label();
            let mut scene = Scene::new(modules.clone()).opened(Vec::new());
            scene.developer = true;
            if kind == SourceTag::Raw {
                scene.state.as_mut().expect("an asset").asset.source =
                    crate::state::testing::raw_source();
            }
            assert_eq!(
                scene.state.as_ref().map(|state| state.asset.source.tag()),
                Some(kind)
            );
            let workspace = scene.derive();
            let sections: Vec<&str> = workspace
                .tools
                .all()
                .map(|section| section.module_id.as_str())
                .collect();
            let listed: HashSet<String> = workspace
                .palette
                .entries
                .iter()
                .filter_map(|entry| owner(&entry.action))
                .collect();
            let shortcuts = tools::mode_shortcuts(&scene.modules, scene.state.as_ref(), None);
            let picks = tools::pick_modes(&scene.modules, scene.state.as_ref(), None);
            for module in &modules {
                let applies = module.applies_to(kind);
                let id = &module.id;
                // A module that declares nothing to draw has no section anywhere: the RAW
                // development's controls are Basic's variants.
                assert_eq!(
                    sections.contains(&id.as_str()),
                    applies && tools::draws_section(module),
                    "{id} has a section on a {label} photo exactly when it applies and declares \
                     controls"
                );
                assert!(
                    applies || !listed.contains(id),
                    "{id} offers no palette entry on a {label} photo"
                );
                assert!(
                    applies || !workspace.canvas.modes.iter().any(|mode| &mode.id == id),
                    "{id} is in the mode strip only where it applies"
                );
                let letter = module
                    .canvas
                    .as_ref()
                    .and_then(|canvas| canvas.shortcut())
                    .and_then(|shortcut| shortcut.chars().next());
                if let Some(letter) = letter.filter(|_| module.is_available()) {
                    assert_eq!(
                        shortcuts.iter().any(|(bound, _)| *bound == letter),
                        applies,
                        "{id}'s shortcut on a {label} photo"
                    );
                }
                // The gate: a module's pick mode takes clicks only on a photo it applies to, and
                // only while a resolved picker names it.
                if tools::canvas_pick(&scene.modules, id).is_some() {
                    assert!(!picks.contains(&id.as_str()) || applies);
                    scene.session.workspace.mode = id.clone();
                    assert_eq!(
                        scene.derive().canvas.picking,
                        picks.contains(&id.as_str()),
                        "{id}'s pick on a {label} photo"
                    );
                    scene.session.workspace.mode = POINTER_MODE.into();
                }
            }
            offered.push(listed);
        }
        // A kind-specific module is offered on its own kind, and every other module offers the
        // same entries on both.
        for module in modules.iter().filter(|module| !every_kind(module)) {
            for (kind, listed) in SourceTag::ALL.into_iter().zip(&offered) {
                assert_eq!(
                    listed.contains(&module.id),
                    module.applies_to(kind),
                    "{} in the palette on a {} photo",
                    module.id,
                    kind.label()
                );
            }
        }
        let shared = |listed: &HashSet<String>| -> HashSet<String> {
            listed
                .iter()
                .filter(|id| tools::module_of(&modules, id).is_none_or(&every_kind))
                .cloned()
                .collect()
        };
        assert_eq!(shared(&offered[0]), shared(&offered[1]));
    }

    /// One `mask.list` row for the displayed entry, with nothing bound to it yet.
    fn reported(
        id: &MaskId,
        index: usize,
        name: &str,
    ) -> luxforge_core::mask::commands::MaskReport {
        luxforge_core::mask::commands::MaskReport {
            id: id.clone(),
            index,
            name: name.into(),
            amount: 100.0,
            invert: false,
            components: Vec::new(),
            layers: Vec::new(),
        }
    }

    /// A photograph open with `masks` listed for its displayed entry.
    fn masked_scene(masks: &[(&MaskId, &str)]) -> Scene {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        scene.masks = Some(MaskListing {
            entry_id: scene.display_entry.clone().expect("a displayed entry"),
            masks: masks
                .iter()
                .enumerate()
                .map(|(index, (id, name))| reported(id, index, name))
                .collect(),
        });
        scene
    }

    /// The open mask's name is the scope chip on every maskable module's band while Mask mode has a
    /// mask open, and on no band otherwise: outside Mask mode, and in Mask mode with no mask open,
    /// the bands carry none. Only a module with a maskable effect ever carries one.
    #[test]
    fn the_scope_chip_names_the_open_mask_on_maskable_bands_in_mask_mode_only() {
        let face = MaskId::new();
        let mut scene = masked_scene(&[(&face, "Face")]);
        scene.selected_mask = Some(face.clone());
        let workspace = scene.derive();
        assert!(
            workspace.tools.all().all(|section| section.scope.is_none()),
            "outside Mask mode the sections are global"
        );
        assert_eq!(workspace.scopes(), json!({}));

        scene.session.workspace.mode = luxforge_core::MASK_MODE.into();
        let workspace = scene.derive();
        let maskable = |id: &str| {
            scene
                .modules
                .iter()
                .find(|module| module.id == id)
                .is_some_and(|module| module.effects.iter().any(|effect| effect.maskable))
        };
        assert!(
            workspace.tools.all().next().is_some(),
            "maskable bands are shown"
        );
        for section in workspace.tools.all() {
            assert!(maskable(&section.module_id), "{}", section.module_id);
            assert_eq!(
                section.scope.as_deref(),
                Some("Face"),
                "{}",
                section.module_id
            );
        }
        assert_eq!(workspace.scopes()["luxforge.basic"], json!("Face"));

        // A rename reaches the bands.
        scene.masks.as_mut().expect("a listing").masks[0].name = "Portrait".into();
        let workspace = scene.derive();
        assert_eq!(
            section(&workspace, "luxforge.basic").scope.as_deref(),
            Some("Portrait")
        );

        // No mask open: nothing is bound, so nothing is scoped.
        scene.selected_mask = None;
        let workspace = scene.derive();
        assert!(workspace.tools.all().all(|section| section.scope.is_none()));
    }

    /// Each mask row carries the thumbnail delivered for its own mask, by identity, and the panel's
    /// evidence records whether one is drawn and a digest of it; a mask with none reads `null`.
    #[test]
    fn a_mask_row_carries_its_own_thumbnail_and_the_summary_records_it() {
        let (sky, face) = (MaskId::new(), MaskId::new());
        let mut scene = masked_scene(&[(&sky, "Sky"), (&face, "Face")]);
        let mut cells = vec![0u8; 28 * 19];
        cells[..28 * 19 / 2].fill(255);
        let thumbnail = masks::Thumbnail {
            cells: cells.into(),
            width: 28,
            height: 19,
        };
        scene.thumbnails = masks::MaskThumbnails {
            version: 1,
            masks: vec![(sky.clone(), Some(thumbnail.clone())), (face.clone(), None)],
        };
        let workspace = scene.derive();
        let rows = &workspace.masks.masks;
        assert_eq!(rows[0].thumbnail.as_ref(), Some(&thumbnail));
        assert!(std::sync::Arc::ptr_eq(
            &rows[0].thumbnail.as_ref().expect("the sky's").cells,
            &thumbnail.cells
        ));
        assert_eq!(rows[1].thumbnail, None);
        let summary = workspace.masks.summary();
        assert_eq!(summary["masks"][0]["thumbnail"]["cells"], json!([28, 19]));
        assert_eq!(summary["masks"][0]["thumbnail"]["mean"], json!(0.5));
        assert_eq!(summary["masks"][1]["thumbnail"], Value::Null);
        // A copy of the same bytes is another thumbnail: rows compare by identity, never by cells.
        let copy = masks::Thumbnail {
            cells: thumbnail.cells.to_vec().into(),
            ..thumbnail.clone()
        };
        assert_ne!(copy, thumbnail);
    }
}
