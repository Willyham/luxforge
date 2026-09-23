//! The view model: pure functions from core state to plain data. Nothing here draws, allocates a
//! texture or calls the owner, and no framework type appears in any model, so every rule the screen
//! follows is testable without a window.
pub(crate) mod canvas;
pub(crate) mod histogram;
pub(crate) mod masks;
pub(crate) mod palette;
pub(crate) mod panel;
pub(crate) mod presets;
pub(crate) mod status;
pub(crate) mod title;
pub(crate) mod tools;

use crate::{
    app::{fields::Fields, message::MenuTarget},
    crop_draft::CropDraft,
    mask_draft::MaskDraft,
};
use lightwell_core::{
    ClientSession, ComponentId, ComponentMode, EditorState, EntryId, ErrorKind, HistoryPage,
    MaskId, ModuleDescriptor, RecipeDescription, Version, mask::commands::MaskListing,
};
use std::collections::{BTreeMap, HashSet};

/// Everything the models are derived from, borrowed for one derivation.
pub(crate) struct Inputs<'a> {
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
    /// The displayed entry's layers as the owner described them.
    pub(crate) recipe: Option<&'a RecipeDescription>,
    pub(crate) fields: &'a Fields,
    /// Local presentation state for generated controls; it never enters the recipe.
    pub(crate) control_ui: &'a tools::ControlsUi,
    /// The (action, parameter) whose value is being typed.
    pub(crate) editing: Option<&'a (String, String)>,
    /// The (action, parameter) whose slider is being dragged.
    pub(crate) dragging: Option<&'a (String, String)>,
    /// Sections the person collapsed or expanded; everything else follows the default.
    pub(crate) expanded: &'a BTreeMap<String, bool>,
    /// The open slider gesture's draft, when a control of a patch action is being moved.
    pub(crate) slider_draft: Option<&'a crate::app::slider::SliderDraft>,
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
    /// The open mask shape gesture.
    pub(crate) mask_draft: Option<&'a MaskDraft>,
    /// The mode the next Add-component gesture will use.
    pub(crate) mask_mode: ComponentMode,
    /// The brush the next stroke will be drawn with, and whether the erase modifier is held.
    pub(crate) brush: crate::mask_draft::Brush,
    pub(crate) brush_erase_held: bool,
    /// The open mask's name as it is being typed in the panel's rename field.
    pub(crate) mask_name: &'a str,
    /// The mask the generated module sections are bound to, which is what a masked slider edits.
    /// `None` binds them to the global layer, as they have always been.
    pub(crate) target: Option<&'a MaskId>,
    /// The truncated preview that opens a draft is in flight.
    pub(crate) draft_pending: bool,
    /// The draft's own input stage is on the GPU and the current state is shown.
    pub(crate) drafting: bool,
    pub(crate) crop_angle: &'a str,
    pub(crate) crop_custom: (&'a str, &'a str),
    pub(crate) crop_guide: bool,
    pub(crate) crop_option: bool,
    pub(crate) crop_space: bool,
    pub(crate) session: &'a ClientSession,
    pub(crate) status: &'a str,
    pub(crate) busy: bool,
    pub(crate) can_open: bool,
    /// Developer mode is active (debug build or `--developer`), so diagnostic UI is listed.
    pub(crate) developer: bool,
    pub(crate) compare_held: bool,
    pub(crate) scale_factor: f32,
    pub(crate) zoom: &'a str,
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
    /// How long the displayed preview took from request to upload.
    pub(crate) render_ms: Option<f64>,
    /// The last preview failure, cleared by the next successful upload.
    pub(crate) render_error: Option<&'a (ErrorKind, String)>,
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
}

impl Workspace {
    pub(crate) fn derive(&mut self, inputs: &Inputs<'_>) {
        self.title = title::derive(inputs);
        self.panel = panel::derive(inputs);
        self.canvas = canvas::derive(inputs);
        self.masks = masks::derive(inputs);
        self.tools.refresh(inputs);
        self.histogram = histogram::derive(inputs, &self.histogram);
        self.status = status::derive(inputs);
        self.palette = palette::derive(inputs);
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
    use crate::app::testing::{
        controls_descriptor, crop_descriptor, crop_layer, descriptors, entry, listed,
        tabs_descriptor,
    };
    use lightwell_core::{
        AssetId, AssetRecord, Availability, CropPayload, LayerDescription, Orientation,
        POINTER_MODE,
    };
    use serde_json::json;
    use std::path::PathBuf;

    /// The pieces a derivation borrows, owned by the test so `Inputs` can point at them.
    struct Scene {
        state: Option<EditorState>,
        history: HistoryPage,
        versions: Vec<Version>,
        lineage: HashSet<EntryId>,
        lineage_floor: Option<u64>,
        display_entry: Option<EntryId>,
        modules: Vec<ModuleDescriptor>,
        recipe: Option<RecipeDescription>,
        fields: Fields,
        control_ui: tools::ControlsUi,
        editing: Option<(String, String)>,
        dragging: Option<(String, String)>,
        expanded: BTreeMap<String, bool>,
        draft: Option<CropDraft>,
        masks: Option<MaskListing>,
        selected_mask: Option<MaskId>,
        selected_component: Option<ComponentId>,
        hovered_component: Option<ComponentId>,
        hidden_masks: HashSet<MaskId>,
        mask_draft: Option<MaskDraft>,
        session: ClientSession,
        status: String,
        busy: bool,
        developer: bool,
        crop_angle: String,
        render_error: Option<(lightwell_core::ErrorKind, String)>,
        analysis: Option<histogram::Analysis>,
        analysis_updating: bool,
        readout: Option<histogram::Readout>,
        presets: presets::PresetLibrary,
        preset_form: presets::PresetForm,
        slider_draft: Option<crate::app::slider::SliderDraft>,
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
                versions: Vec::new(),
                lineage: HashSet::new(),
                lineage_floor: None,
                display_entry: None,
                modules,
                recipe: None,
                fields,
                control_ui: tools::ControlsUi::default(),
                editing: None,
                dragging: None,
                expanded: BTreeMap::new(),
                draft: None,
                masks: None,
                selected_mask: None,
                selected_component: None,
                hovered_component: None,
                hidden_masks: HashSet::new(),
                mask_draft: None,
                session: ClientSession::default(),
                status: "ready".into(),
                busy: false,
                developer: false,
                crop_angle: "0".into(),
                render_error: None,
                analysis: None,
                analysis_updating: false,
                readout: None,
                presets: presets::PresetLibrary::default(),
                preset_form: presets::PresetForm::default(),
                slider_draft: None,
            }
        }

        /// One asset open at that revision with those layers, and one history entry for it.
        fn opened(mut self, layers: Vec<lightwell_core::Layer>) -> Self {
            let asset = AssetId::new();
            let mut current = entry(&asset, 3, None);
            current.label = "Crop 4:5".into();
            current.actor = "agent".into();
            for layer in layers {
                current.snapshot = current.snapshot.append(layer).expect("a valid stack");
            }
            self.display_entry = Some(current.id.clone());
            self.lineage.insert(current.id.clone());
            self.history = HistoryPage {
                entries: vec![current.clone()],
                next_before_sequence: None,
            };
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
                    source: lightwell_core::SourceKind::Jpeg,
                },
                revision: 3,
                current_entry: current,
                redo: Vec::new(),
            });
            self
        }

        fn inputs(&self) -> Inputs<'_> {
            Inputs {
                state: self.state.as_ref(),
                history: &self.history,
                versions: &self.versions,
                lineage: &self.lineage,
                lineage_floor: self.lineage_floor,
                display_entry: self.display_entry.as_ref(),
                modules: &self.modules,
                modules_ready: true,
                recipe: self.recipe.as_ref(),
                fields: &self.fields,
                control_ui: &self.control_ui,
                editing: self.editing.as_ref(),
                dragging: self.dragging.as_ref(),
                expanded: &self.expanded,
                slider_draft: self.slider_draft.as_ref(),
                draft: self.draft.as_ref(),
                masks: self.masks.as_ref(),
                selected_mask: self.selected_mask.as_ref(),
                selected_component: self.selected_component.as_ref(),
                hovered_component: self.hovered_component.as_ref(),
                hidden_masks: &self.hidden_masks,
                mask_draft: self.mask_draft.as_ref(),
                mask_mode: ComponentMode::Add,
                brush: crate::mask_draft::NEUTRAL_BRUSH,
                brush_erase_held: false,
                mask_name: "",
                target: self
                    .selected_mask
                    .as_ref()
                    .filter(|_| crate::state::canvas::mask_workspace(&self.session.workspace.mode)),
                draft_pending: false,
                drafting: self.draft.is_some(),
                crop_angle: &self.crop_angle,
                crop_custom: ("5", "4"),
                crop_guide: false,
                crop_option: false,
                crop_space: false,
                session: &self.session,
                status: &self.status,
                busy: self.busy,
                can_open: true,
                developer: self.developer,
                compare_held: false,
                scale_factor: 2.0,
                zoom: "100",
                version_name: "",
                version_form_open: false,
                dimensions: Some((480, 320)),
                photo: true,
                clients: Some(1),
                rendering: false,
                render_ms: Some(41.0),
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
            }
        }

        fn derive(&self) -> Workspace {
            let mut workspace = Workspace::default();
            workspace.derive(&self.inputs());
            workspace
        }
    }

    fn section<'a>(workspace: &'a Workspace, id: &str) -> &'a tools::SectionModel {
        workspace
            .tools
            .all()
            .find(|section| section.module_id == id)
            .unwrap_or_else(|| panic!("no section for {id}"))
    }

    #[test]
    fn history_rows_carry_the_stored_label_actor_and_marker() {
        let scene = Scene::new(descriptors()).opened(Vec::new());
        let workspace = scene.derive();
        let row = &workspace.panel.history[0];
        assert_eq!(row.label, "Crop 4:5", "the stored label, not a rebuild");
        assert_eq!(row.actor, "agent");
        assert_eq!(row.marker, Marker::Current);
        assert!(!row.branch);
        assert!(workspace.panel.preview.is_none());

        // A history preview marks the displayed entry and offers the preview controls.
        let asset = scene.state.as_ref().expect("an asset").asset.id.clone();
        let older = entry(&asset, 1, None);
        let mut scene = scene;
        scene.history.entries.push(older.clone());
        scene.display_entry = Some(older.id.clone());
        scene.session.preview.selection = lightwell_core::HistorySelection::Entry(older.id.clone());
        let workspace = scene.derive();
        assert_eq!(workspace.panel.history[0].marker, Marker::Current);
        assert_eq!(workspace.panel.history[1].marker, Marker::Previewed);
        assert_eq!(
            workspace.panel.preview.map(|preview| preview.can_restore),
            Some(true)
        );
    }

    #[test]
    fn an_entry_off_the_lineage_is_marked_as_a_branch() {
        let mut scene = Scene::new(descriptors()).opened(Vec::new());
        let asset = scene.state.as_ref().expect("an asset").asset.id.clone();
        let abandoned = entry(&asset, 2, None);
        scene.history.entries.push(abandoned.clone());
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
        scene.session.preview.selection = lightwell_core::HistorySelection::Entry(EntryId::new());
        assert_eq!(
            section(&scene.derive(), &crop.id)
                .disabled_reason
                .as_deref(),
            Some("Return to current to edit")
        );
        scene.session.preview.selection = lightwell_core::HistorySelection::Current;
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

    #[test]
    fn a_conflicted_draft_shows_its_state_in_the_crop_model_and_a_notice() {
        let crop = crop_descriptor();
        let mut scene = Scene::new(vec![crop.clone()]).opened(Vec::new());
        scene.draft = Some(CropDraft::neutral(
            lightwell_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            3,
            0,
        ));
        let workspace = scene.derive();
        let ControlModel::CropFrame(frame) = &section(&workspace, &crop.id).controls[0] else {
            panic!("the crop section is first in its module")
        };
        assert!(frame.drafting && !frame.conflicted && frame.can_apply);
        assert_eq!(frame.presets.len(), 7, "the ratios are generated");
        assert!(workspace.canvas.notices.is_empty());

        scene.draft.as_mut().expect("a draft").mark_conflicted();
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
            lightwell_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            3,
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
            slider.value, slider.min,
            "an unreadable field reads as its min"
        );
    }

    /// Every sub-group in the panel, with the state it reads and the fields it holds.
    fn sub_groups(workspace: &Workspace) -> Vec<(String, Option<tools::GroupState>, Vec<String>)> {
        fn walk(
            controls: &[ControlModel],
            found: &mut Vec<(String, Option<tools::GroupState>, Vec<String>)>,
        ) {
            for control in controls {
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
                    walk(&group.controls, found);
                }
            }
        }
        let mut found = Vec::new();
        for section in workspace.tools.all() {
            walk(&section.controls, &mut found);
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
        let patch = modules
            .iter()
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
        fn walk<'a>(
            controls: &'a [ControlModel],
            action: &str,
            parameter: &str,
        ) -> Option<&'a tools::SliderControl> {
            for control in controls {
                match control {
                    ControlModel::Slider(slider)
                        if slider.action == action && slider.parameter == parameter =>
                    {
                        return Some(slider);
                    }
                    ControlModel::Group(group) => {
                        if let Some(found) = walk(&group.controls, action, parameter) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        workspace
            .tools
            .all()
            .find_map(|section| walk(&section.controls, action, parameter))
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
        let basic = section(&workspace, "lightwell.basic");
        let picker = *basic
            .pickers()
            .first()
            .expect("the Basic module declares a picker");
        assert_eq!(picker.module_id, "lightwell.basic");
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
            matches!(group.controls.last(), Some(ControlModel::Picker(_))),
            "the picker is the last control of the group whose fields it sets"
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
        scene.session.workspace.mode = "lightwell.basic".into();
        workspace.derive(&scene.inputs());
        assert!(
            section(&workspace, "lightwell.basic").pickers()[0].selected,
            "the picker reads selected while its own mode is active"
        );
        for (id, before) in versions {
            let after = section(&workspace, &id).version;
            if id == "lightwell.basic" {
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
            !section(&workspace, "lightwell.pixel").pickers()[0].selected,
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
            .find(|module| module.id != pixel && module.id != "lightwell.raw")
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
                    .any(|effect| effect.id == lightwell_core::ORIENTATION_EFFECT)
            })
            .expect("the registered transform module");
        let oriented = |orientation| {
            Scene::new(vec![transforms.clone()])
                .opened(vec![lightwell_core::Layer::orientation(orientation)])
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

    #[test]
    fn recipe_rows_come_from_the_owners_own_layer_descriptions() {
        let crop = crop_descriptor();
        let mut scene =
            Scene::new(vec![crop.clone()]).opened(vec![crop_layer(CropPayload::NEUTRAL)]);
        let layer_id = scene
            .state
            .as_ref()
            .expect("an asset")
            .current_entry
            .snapshot
            .recipe
            .layers[0]
            .id
            .clone();
        scene.recipe = Some(RecipeDescription {
            entry_id: scene.display_entry.clone().expect("a displayed entry"),
            layers: vec![LayerDescription {
                id: layer_id.clone(),
                effect: "lightwell.geometry.crop".into(),
                module: Some(crop.id.clone()),
                title: Some("Crop".into()),
                summary: "Whole image".into(),
                values: serde_json::Map::new(),
                available: true,
                mask: None,
            }],
        });
        let workspace = scene.derive();
        assert_eq!(workspace.panel.recipe.len(), 1);
        assert_eq!(workspace.panel.recipe[0].title, "Crop");
        assert_eq!(workspace.panel.recipe[0].summary, "Whole image");
        assert!(workspace.panel.recipe[0].available);
        // A description of a different entry is never shown against this one.
        scene.display_entry = Some(EntryId::new());
        assert!(scene.derive().panel.recipe.is_empty());
    }

    #[test]
    fn an_empty_recipe_says_whether_anything_is_displayed_at_all() {
        let mut scene = Scene::new(vec![crop_descriptor()]).opened(Vec::new());
        assert_eq!(
            scene.derive().panel.recipe_caption.as_deref(),
            Some("Original · no edit layers"),
            "a displayed entry with no layers is the original, not an empty selection"
        );
        scene.display_entry = None;
        assert_eq!(
            scene.derive().panel.recipe_caption.as_deref(),
            Some("No entry displayed")
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
        // Mask is the host's own takeover mode: a mask is a host object in the recipe, so it is
        // offered whatever modules are registered and no module declares its canvas.
        assert_eq!(strip[1].id, lightwell_core::MASK_MODE);
        assert_eq!(strip[1].label, "Mask");
        assert_eq!(strip[1].shortcut.as_deref(), Some("M"));
        let crop = strip
            .iter()
            .find(|mode| mode.id == "lightwell.crop")
            .expect("the crop module declares a canvas mode");
        assert_eq!(crop.label, "Crop", "the descriptor's own canvas title");
        assert_eq!(crop.shortcut.as_deref(), Some("R"));
        assert!(crop.enabled);
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
                    Some(lightwell_core::CanvasInteraction::PointPick { .. })
                        | Some(lightwell_core::CanvasInteraction::SampleApply { .. })
                )
            })
            .map(|module| module.id.clone())
            .collect();
        assert!(
            picks.iter().any(|id| id == "lightwell.basic"),
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
    fn the_draft_bar_reads_out_the_draft_and_names_why_apply_is_refused() {
        let crop = crop_descriptor();
        let mut scene = Scene::new(vec![crop.clone()]).opened(Vec::new());
        scene.session.workspace.mode = crop.id.clone();
        scene.draft = Some(CropDraft::neutral(
            lightwell_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            3,
            0,
        ));
        let bar = scene.derive().canvas.draft_bar.expect("an open draft");
        assert_eq!(bar.title, "Crop");
        assert_eq!(bar.readout, "480 × 320 px · 0°");
        assert!(bar.can_apply && bar.apply_reason.is_none());

        scene.draft.as_mut().expect("a draft").mark_conflicted();
        let bar = scene.derive().canvas.draft_bar.expect("an open draft");
        assert!(!bar.can_apply && bar.conflicted);
        assert_eq!(
            bar.apply_reason.as_deref(),
            Some("Changed elsewhere: discard the draft or reapply it")
        );
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

        scene.render_error = Some((
            lightwell_core::ErrorKind::Incompatible,
            format!("unavailable effect {effect} (layers l1)"),
        ));
        let notice = &scene.derive().canvas.notices[0];
        assert_eq!(notice.title, "Preview is stale");
        assert_eq!(
            notice.body, "Crop is unavailable: disabled by --disable-module",
            "the notice names the module and the reason, not the effect identity"
        );
        assert!(notice.actions.is_empty(), "Locate is a later feature");

        // A layer nothing provides is still named, by its effect identity.
        scene.render_error = Some((
            lightwell_core::ErrorKind::Incompatible,
            "unavailable effect other.effect (layers l1)".into(),
        ));
        assert_eq!(
            scene.derive().canvas.notices[0].body,
            "No registered module provides other.effect"
        );

        for (kind, title) in [
            (
                lightwell_core::ErrorKind::SourceUnavailable,
                "Original not found",
            ),
            (lightwell_core::ErrorKind::FileAccess, "Original not found"),
            (lightwell_core::ErrorKind::ResourceLimit, "Rendering limit"),
        ] {
            scene.render_error = Some((kind, "the detail".into()));
            let notice = &scene.derive().canvas.notices[0];
            assert_eq!(notice.title, title, "{kind:?}");
            assert_eq!(notice.body, "the detail");
            assert!(notice.actions.is_empty());
        }
        // A kind the workspace has nothing to say about is left to the status bar.
        scene.render_error = Some((lightwell_core::ErrorKind::Internal, "boom".into()));
        assert!(scene.derive().canvas.notices.is_empty());
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
        let render_error = Some((
            lightwell_core::ErrorKind::Incompatible,
            format!("unavailable effect {effect} (layers l1)"),
        ));
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
        let mut draft = CropDraft::neutral(
            lightwell_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            2,
            0,
        );
        draft.mark_conflicted();
        scene.draft = Some(draft);
        let notice = &scene.derive().canvas.notices[0];
        assert_eq!(notice.title, "Changed elsewhere");
        assert!(notice.body.contains("revision 3"), "{}", notice.body);
        assert_eq!(
            notice
                .actions
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            ["Discard", "Reapply"]
        );
    }

    #[test]
    fn the_status_bar_reports_clients_render_state_and_what_the_zoom_means() {
        let scene = Scene::new(vec![crop_descriptor()]).opened(Vec::new());
        let status = scene.derive().status;
        assert_eq!(status.clients, "1 client");
        assert_eq!(status.render, "Rendered in 41 ms");
        assert_eq!(status.zoom_text, "Fit", "the session's zoom, not the field");
        assert_eq!(status.scale_text, "@2.00×");

        let mut inputs = scene.inputs();
        inputs.clients = Some(3);
        inputs.rendering = true;
        let mut workspace = Workspace::default();
        workspace.derive(&inputs);
        assert_eq!(workspace.status.clients, "3 clients");
        assert_eq!(workspace.status.render, "Rendering…");

        // No local server is a stated fact, never a client count of zero.
        let mut inputs = scene.inputs();
        inputs.clients = None;
        inputs.render_ms = None;
        workspace.derive(&inputs);
        assert_eq!(workspace.status.clients, "live API unavailable");
        assert_eq!(workspace.status.render, "Idle");
    }

    #[test]
    fn a_historical_preview_disables_every_control_and_dims_the_reset() {
        let crop = crop_descriptor();
        let mut scene = Scene::new(vec![crop.clone()]).opened(Vec::new());
        assert!(section(&scene.derive(), &crop.id).reset.is_some());
        scene.session.preview.selection = lightwell_core::HistorySelection::Entry(EntryId::new());
        let workspace = scene.derive();
        let section = section(&workspace, &crop.id);
        assert!(!section.enabled);
        assert_eq!(
            section.disabled_reason.as_deref(),
            Some("Return to current to edit")
        );
        assert!(
            section.reset.is_some(),
            "a section that cannot edit keeps its reset, dimmed, so the header keeps its height"
        );
        assert!(
            section.expanded,
            "the values stay visible while the preview is shown"
        );
        fn all_refused(controls: &[ControlModel]) {
            for control in controls {
                match control {
                    ControlModel::Action(action) => {
                        assert!(!action.runnable, "{} stayed runnable", action.action)
                    }
                    ControlModel::Group(group) => all_refused(&group.controls),
                    ControlModel::CropFrame(frame) => {
                        assert!(!frame.enabled && !frame.can_start && !frame.can_apply)
                    }
                    _ => {}
                }
            }
        }
        all_refused(&section.controls);
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
        assert_eq!(scene.derive().expanded(), json!({"lightwell.crop": true}));
    }

    #[test]
    fn every_declared_control_maps_to_plain_data_and_local_curve_state_survives_refresh() {
        let fixture = controls_descriptor();
        let mut scene = Scene::new(vec![fixture.clone(), crop_descriptor()]).opened(Vec::new());
        let mut workspace = Workspace::default();
        workspace.derive(&scene.inputs());
        let fixture_section = section(&workspace, &fixture.id);
        let ControlModel::Group(group) = &fixture_section.controls[0] else {
            panic!("declared group")
        };
        assert!(group.expanded);
        let ControlModel::Slider(amount) = &group.controls[0] else {
            panic!("number")
        };
        assert_eq!(
            (amount.min, amount.max, amount.soft_min, amount.soft_max),
            (-10.0, 10.0, -5.0, 5.0)
        );
        assert_eq!(
            (amount.step, amount.fine_step, amount.zero),
            (0.1, 0.01, 0.0)
        );
        assert!(matches!(amount.rail, tools::RailStyle::Temperature));
        assert!(
            matches!(group.controls[1], ControlModel::Slider(ref field) if field.style == tools::NumberControlStyle::Stepper)
        );
        assert!(
            matches!(group.controls[2], ControlModel::Slider(ref field) if field.style == tools::NumberControlStyle::Field)
        );
        assert!(matches!(group.controls[3], ControlModel::Toggle(ref field) if !field.on));
        assert!(
            matches!(group.controls[4], ControlModel::Enum(ref field) if field.style == tools::ChoiceControlStyle::Menu)
        );
        assert!(
            matches!(group.controls[5], ControlModel::Color(ref field) if field.style == tools::ColorControlStyle::Picker && field.rgb == [32,64,128])
        );
        assert!(
            matches!(group.controls[6], ControlModel::Curve(ref field) if field.channels.len() == 2 && field.sample_query == "fixture-samples" && field.points.len() == 3)
        );
        assert!(
            matches!(group.controls[7], ControlModel::Action(ref field) if field.style == tools::ActionControlStyle::Icon && field.icon.as_deref() == Some("reset"))
        );
        let crop_version = section(&workspace, "lightwell.crop").version;
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
        scene
            .control_ui
            .group_expanded
            .insert(tools::group_key(&fixture.id, &[0]), false);
        workspace.derive(&scene.inputs());
        let fixture_section = section(&workspace, &fixture.id);
        assert_eq!(fixture_section.version, fixture_version + 1);
        assert_eq!(section(&workspace, "lightwell.crop").version, crop_version);
        let ControlModel::Group(group) = &fixture_section.controls[0] else {
            panic!("group")
        };
        assert!(!group.expanded);
        assert!(
            matches!(group.controls[6], ControlModel::Curve(ref field) if field.selected_channel == 1 && field.selected_point == Some(2) && field.sampled.len() == 2)
        );
        workspace.derive(&scene.inputs());
        assert_eq!(
            section(&workspace, &fixture.id).version,
            fixture_version + 1
        );
        let original_entry = scene.display_entry.replace(EntryId::new()).unwrap();
        workspace.derive(&scene.inputs());
        let ControlModel::Group(group) = &section(&workspace, &fixture.id).controls[0] else {
            panic!("group")
        };
        assert!(
            matches!(group.controls[6], ControlModel::Curve(ref field) if field.sampled.is_empty()),
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
        let ControlModel::Group(group) = &section(&workspace, &fixture.id).controls[0] else {
            panic!("group")
        };
        assert!(
            matches!(group.controls[6], ControlModel::Curve(ref field) if field.sampled.is_empty()),
            "old sampled geometry is hidden until the query matches the current points"
        );
        assert_eq!(section(&workspace, "lightwell.crop").version, crop_version);
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
        let crop_version = section(&workspace, "lightwell.crop").version;

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
            section(&workspace, "lightwell.crop").version,
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
            let ControlModel::Group(group) = &section(workspace, &fixture.id).controls[0] else {
                panic!("group")
            };
            let (ControlModel::Color(color), ControlModel::Curve(curve)) =
                (&group.controls[5], &group.controls[6])
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
        section(workspace, "lightwell.presets")
            .presets()
            .expect("the presets module renders its library")
    }

    fn counts(unsupported: usize, refused: usize) -> lightwell_core::ReportCounts {
        lightwell_core::ReportCounts {
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
        scene.session.preview.selection = lightwell_core::HistorySelection::Entry(EntryId::new());
        assert_eq!(
            enabled(&scene),
            (false, Some("Return to current to edit".into()))
        );
        scene.session.preview.selection = lightwell_core::HistorySelection::Current;
        scene.draft = Some(CropDraft::neutral(
            lightwell_core::CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            3,
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
            crate::app::message::PaletteAction::Run {
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
}
