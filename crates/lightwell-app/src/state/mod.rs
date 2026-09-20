//! The view model: pure functions from core state to plain data. Nothing here draws, allocates a
//! texture or calls the owner, and no framework type appears in any model, so every rule the screen
//! follows is testable without a window.
pub(crate) mod canvas;
pub(crate) mod palette;
pub(crate) mod panel;
pub(crate) mod status;
pub(crate) mod title;
pub(crate) mod tools;

use crate::{
    app::{fields::Fields, message::MenuTarget},
    crop_draft::CropDraft,
};
use lightwell_core::{
    ClientSession, EditorState, EntryId, HistoryPage, ModuleDescriptor, RecipeDescription, Version,
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
    /// The (action, parameter) whose value is being typed.
    pub(crate) editing: Option<&'a (String, String)>,
    /// The (action, parameter) whose slider is being dragged.
    pub(crate) dragging: Option<&'a (String, String)>,
    /// Sections the person collapsed or expanded; everything else follows the default.
    pub(crate) expanded: &'a BTreeMap<String, bool>,
    pub(crate) draft: Option<&'a CropDraft>,
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
    /// The desktop was started with `--developer`, so developer modules are listed.
    pub(crate) developer: bool,
    pub(crate) compare_held: bool,
    pub(crate) scale_factor: f32,
    pub(crate) zoom: &'a str,
    pub(crate) version_name: &'a str,
    pub(crate) dimensions: Option<(u32, u32)>,
    /// A preview is on the GPU.
    pub(crate) photo: bool,
    pub(crate) phase: &'static str,
    pub(crate) clients: usize,
    pub(crate) pointer: Option<(u32, u32)>,
    pub(crate) menu: Option<&'a MenuTarget>,
    pub(crate) palette_open: bool,
    pub(crate) palette_query: &'a str,
    pub(crate) palette_selected: usize,
}

/// The whole screen as plain data. The tools panel keeps its sections across derivations so an
/// untouched module is not rebuilt.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Workspace {
    pub(crate) title: title::TitleBarModel,
    pub(crate) panel: panel::StatePanelModel,
    pub(crate) canvas: canvas::CanvasModel,
    pub(crate) tools: tools::ToolsModel,
    pub(crate) status: status::StatusBarModel,
    pub(crate) palette: palette::PaletteModel,
}

impl Workspace {
    pub(crate) fn derive(&mut self, inputs: &Inputs<'_>) {
        self.title = title::derive(inputs);
        self.panel = panel::derive(inputs);
        self.canvas = canvas::derive(inputs);
        self.tools.refresh(inputs);
        self.status = status::derive(inputs);
        self.palette = palette::derive(inputs);
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
    use crate::app::testing::{crop_descriptor, crop_layer, descriptors, entry};
    use lightwell_core::{
        AssetId, AssetRecord, Availability, CropPayload, LayerDescription, POINTER_MODE,
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
        editing: Option<(String, String)>,
        dragging: Option<(String, String)>,
        expanded: BTreeMap<String, bool>,
        draft: Option<CropDraft>,
        session: ClientSession,
        status: String,
        busy: bool,
        developer: bool,
        crop_angle: String,
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
                editing: None,
                dragging: None,
                expanded: BTreeMap::new(),
                draft: None,
                session: ClientSession::default(),
                status: "ready".into(),
                busy: false,
                developer: false,
                crop_angle: "0".into(),
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
                editing: self.editing.as_ref(),
                dragging: self.dragging.as_ref(),
                expanded: &self.expanded,
                draft: self.draft.as_ref(),
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
                dimensions: Some((480, 320)),
                photo: true,
                phase: "ready",
                clients: 1,
                pointer: None,
                menu: None,
                palette_open: false,
                palette_query: "",
                palette_selected: 0,
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
            .find(|module| module.id != pixel)
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
                available: true,
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
}
