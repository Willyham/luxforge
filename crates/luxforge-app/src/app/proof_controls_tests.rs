//! The registered developer module proves the generated desktop vocabulary against the same
//! JSON method table an independent client uses. No descriptor-only desktop fixture is involved.
use super::{
    Boot, Editor,
    controls::CurveSampleIdentity,
    message::{Message, action::ActionMessage, control::ControlMessage, sync::SyncMessage},
    tasks,
    testing::import_and_adopt,
};
use crate::Config;
use crate::state::fields;
use crate::state::tools::ControlModel;
use luxforge_core::{
    ApiRequest, AssetId, ClientId, ModuleDescriptor, ModuleRegistry, OwnerHandle, controls_module,
};
use luxforge_testbase::paths::temp_catalog;
use luxforge_ui::{ColorPickerEvent, CurveEditorEvent};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
const MODULE: &str = "luxforge.controls";
const ACTION: &str = "set-controls";

struct Proof {
    editor: Editor,
    catalog: PathBuf,
    asset: AssetId,
    json_client: ClientId,
}

impl Proof {
    fn new() -> Self {
        let catalog = temp_catalog("desktop-controls-parity");
        let mut registry = ModuleRegistry::new();
        registry.register(Arc::new(controls_module())).unwrap();
        let (owner, join) = OwnerHandle::start_with(&catalog, Arc::new(registry)).unwrap();
        let json_client = owner.register();
        let asset = import_and_adopt(
            &owner,
            json_client,
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg"),
        );
        let (mut editor, _) = Editor::new(Boot {
            owner: owner.clone(),
            join,
            live_server: None,
            config: Config {
                developer: true,
                ..Config::default()
            },
            client: None,
            initial_import: None,
            window: (1440.0, 900.0),
        });
        let listed = call(&owner, json_client, "module.list", json!({})).0;
        let modules: Vec<ModuleDescriptor> =
            serde_json::from_value(listed["modules"].clone()).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].id, MODULE);
        assert!(modules[0].developer);
        editor.controls.expanded.insert(MODULE.into(), true);
        let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(modules))));
        let refreshed = tasks::refresh(
            &owner,
            editor.client,
            asset.clone(),
            tasks::Scope::Open,
            None,
        )
        .unwrap();
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
            refreshed,
        )))));
        assert_eq!(
            editor.gesture_refusal(crate::app::gesture::Starting::Action),
            None
        );
        assert_eq!(editor.workspace.tools.developer.len(), 1);
        Self {
            editor,
            catalog,
            asset,
            json_client,
        }
    }

    fn descriptor(&self) -> &ModuleDescriptor {
        self.editor
            .modules
            .iter()
            .find(|module| module.id == MODULE)
            .unwrap()
    }

    fn displayed_value(&self, parameter: &str) -> Value {
        let mut models = Vec::new();
        flatten(
            &self.editor.workspace.tools.developer[0].controls,
            &mut models,
        );
        for model in models {
            match model {
                ControlModel::Slider(number) if number.parameter == parameter => {
                    return if number.spec.integer {
                        json!(number.value as i64)
                    } else {
                        json!(number.value)
                    };
                }
                ControlModel::Toggle(toggle) if toggle.parameter == parameter => {
                    return json!(toggle.on);
                }
                ControlModel::Enum(choice) if choice.parameter == parameter => {
                    return json!(choice.options[choice.selected.expect("a selected choice")]);
                }
                ControlModel::Color(color) if color.parameter == parameter => {
                    return json!(color.rgb);
                }
                ControlModel::Curve(curve)
                    if curve.channels[curve.selected_channel].parameter == parameter =>
                {
                    return json!(curve.points);
                }
                _ => {}
            }
        }
        panic!("no visible model for {parameter}")
    }

    fn copy_and_post(&mut self, parameter: &str, expected: Value) {
        let action = ACTION.to_owned();
        let parameter_name = parameter.to_owned();
        let _ = self
            .editor
            .update(Message::Action(ActionMessage::CopyRequest {
                action: action.clone(),
                parameter: Some(parameter_name.clone()),
                preset: None,
            }));
        assert_eq!(
            self.editor.status.text,
            format!("Copied the edit.{ACTION} request")
        );
        let copied = self.editor.request_for(ACTION, Some(parameter)).unwrap();
        assert_eq!(copied["method"], format!("edit.{ACTION}"));
        let params = copied["params"].as_object().unwrap();
        assert_eq!(params.len(), 3, "a patch control sends exactly one field");
        assert_eq!(params[parameter], expected, "{parameter} desktop value");
        assert_eq!(
            params["mutation"]["expected_revision"],
            self.editor.document.state.as_ref().unwrap().revision
        );

        // Construct the request independently, as a JSON client would; only the request identity
        // and actor differ from the desktop's copied envelope.
        let mutation = json!({
            "expected_revision": self.editor.document.state.as_ref().unwrap().revision,
            "request_id": format!("proof-json-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            "actor": "proof-json-client"
        });
        let independent = json!({
            "asset_id": self.asset, "mutation": mutation, parameter: expected,
        });
        let mut comparable = copied["params"].clone();
        comparable["mutation"] = independent["mutation"].clone();
        assert_eq!(
            comparable, independent,
            "copied request equals the independent JSON shape"
        );
        call(
            &self.editor.owner,
            self.json_client,
            &format!("edit.{ACTION}"),
            independent,
        );
        self.editor.gesture = None; // The ignored Iced task did not open a real draft.
        self.editor.controls.dragging = None;
        self.editor.controls.editing = None;
        self.editor
            .controls
            .fields
            .set(ACTION, parameter, "stale-local-value".into());
        let refreshed = tasks::refresh(
            &self.editor.owner,
            self.editor.client,
            self.asset.clone(),
            tasks::Scope::Elsewhere,
            None,
        )
        .unwrap();
        let _ = self
            .editor
            .update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
                refreshed,
            )))));
        assert_eq!(
            self.editor.control_field_value(ACTION, parameter),
            Some(expected.clone()),
            "{parameter} re-derives from the independent client's recipe"
        );
        assert_eq!(
            self.displayed_value(parameter),
            expected,
            "{parameter} is reflected in the rendered control model"
        );
        assert!(self.editor.workspace.tools.developer[0].enabled);
    }

    fn post_preset(&mut self, preset: &Map<String, Value>) {
        let _ = self
            .editor
            .update(Message::Action(ActionMessage::CopyRequest {
                action: ACTION.into(),
                parameter: None,
                preset: Some(preset.clone()),
            }));
        assert_eq!(
            self.editor.status.text,
            format!("Copied the edit.{ACTION} request")
        );
        let copied = self
            .editor
            .request_for_preset(ACTION, None, Some(preset))
            .unwrap();
        assert_eq!(copied["method"], format!("edit.{ACTION}"));
        for (parameter, value) in preset {
            assert_eq!(&copied["params"][parameter], value);
        }
        let mut independent = copied["params"].clone();
        independent["mutation"]["request_id"] = json!(format!(
            "proof-json-{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        independent["mutation"]["actor"] = json!("proof-json-client");
        call(
            &self.editor.owner,
            self.json_client,
            &format!("edit.{ACTION}"),
            independent,
        );
        self.editor.controls.dragging = None;
        self.editor.controls.editing = None;
        let refreshed = tasks::refresh(
            &self.editor.owner,
            self.editor.client,
            self.asset.clone(),
            tasks::Scope::Elsewhere,
            None,
        )
        .unwrap();
        let _ = self
            .editor
            .update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
                refreshed,
            )))));
        for (parameter, value) in preset {
            assert_eq!(
                self.editor.control_field_value(ACTION, parameter),
                Some(value.clone())
            );
            assert_eq!(self.displayed_value(parameter), value.clone());
        }
    }

    fn finish(mut self) {
        self.editor.owner.stop();
        self.editor.owner_join.take().unwrap().join().unwrap();
        std::fs::remove_file(&self.catalog).unwrap();
    }
}

fn call(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> (Value, u64) {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("proof-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                method: method.into(),
                params,
                token: None,
            },
        )
        .unwrap();
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    (response.result.unwrap(), response.sequence)
}

fn flatten<'a>(controls: &'a [ControlModel], output: &mut Vec<&'a ControlModel>) {
    output.extend(crate::state::control_tree::walk(controls));
}

#[test]
fn registered_proof_descriptor_generates_the_whole_vocabulary() {
    let proof = Proof::new();
    let section = &proof.editor.workspace.tools.developer[0];
    assert_eq!(section.module_id, MODULE);
    let mut controls = Vec::new();
    flatten(&section.controls, &mut controls);
    // The proof declares its whole vocabulary inside one group. A module's only group is drawn
    // without a header, so the group kind is proven by the declaration here and drawn by Basic's
    // three groups and by the fixtures with two.
    assert!(
        matches!(
            proof.descriptor().controls.as_slice(),
            [luxforge_core::Control::Group(luxforge_core::GroupControl {
                reset: Some(_),
                ..
            })]
        ),
        "the proof declares one resettable group"
    );
    let signatures: BTreeSet<String> = controls
        .iter()
        .map(|control| match control {
            // The proof's only group draws no header; the one nested group is the wheel's tab
            // row, and its two children are views.
            ControlModel::Group(group) if group.view => format!("view:{}", group.label),
            ControlModel::Group(group) => format!("tabs:{}:{:?}", group.label, group.selected),
            ControlModel::Wheel(wheel) => {
                format!("wheel:{}", if wheel.large { "large" } else { "compact" })
            }
            ControlModel::Slider(number) => format!("number:{:?}", number.style),
            ControlModel::Toggle(_) => "toggle".into(),
            ControlModel::Enum(choice) => format!("choice:{:?}", choice.style),
            ControlModel::QueryChoice(_) => "query-choice".into(),
            ControlModel::Color(color) => format!("color:{:?}", color.style),
            ControlModel::Curve(curve) => {
                assert_eq!(curve.channels.len(), 2);
                assert!(curve.background);
                "curve".into()
            }
            ControlModel::Action(action) => format!("action:{:?}", action.style),
            ControlModel::Range(_) => panic!("proof module declares no range"),
            ControlModel::Picker(_) => panic!("proof module declares no canvas picker"),
            ControlModel::Task(_) => panic!("proof module declares no task"),
            ControlModel::Unsupported(kind) => panic!("unsupported proof control {kind}"),
            ControlModel::CropFrame(_) => panic!("proof module declares no crop frame"),
            ControlModel::Presets(_) => panic!("proof module declares no preset library"),
        })
        .collect();
    let expected = BTreeSet::from([
        "query-choice".into(),
        "number:Slider".into(),
        "number:Field".into(),
        "number:Stepper { rail: false }".into(),
        "toggle".into(),
        "choice:Segmented".into(),
        "choice:Chips".into(),
        "choice:Menu".into(),
        "color:Picker".into(),
        "color:Fields".into(),
        "curve".into(),
        "action:Default".into(),
        "action:Primary".into(),
        "action:Icon".into(),
        "tabs:Colour wheel:Some(0)".into(),
        "view:Compact".into(),
        "view:Large".into(),
        "wheel:compact".into(),
        "wheel:large".into(),
    ]);
    assert_eq!(signatures, expected);
    assert!(proof.descriptor().action(ACTION).unwrap().patch);
    proof.finish();
}

/// The wheel group's tab row as the panel derived it: the selected view's label and whether its
/// wheel is the large one.
fn wheel_row(editor: &Editor) -> (String, bool) {
    let mut models = Vec::new();
    flatten(&editor.workspace.tools.developer[0].controls, &mut models);
    let row = models
        .into_iter()
        .find_map(|model| match model {
            ControlModel::Group(group) if group.label == "Colour wheel" => Some(group),
            _ => None,
        })
        .expect("the wheel's tab row");
    assert_eq!(row.id.as_deref(), Some("colour-wheel"));
    let visible = row.visible_view().expect("a tabbed group shows a view");
    let large = visible
        .controls
        .iter()
        .any(|control| matches!(control, ControlModel::Wheel(wheel) if wheel.large));
    (visible.label.clone(), large)
}

/// Choosing a view is the session's view state, set through the same `workspace.set` an API
/// client sends: the desktop sends one request, draws the view the answer holds, and neither the
/// history, the revision nor the requested frame moves. An API client reads the same selection.
#[test]
fn selecting_a_view_is_session_state_with_no_history_or_frame() {
    let mut proof = Proof::new();
    assert_eq!(wheel_row(&proof.editor), ("Compact".into(), false));
    let history = proof.editor.document.history.entries.len();
    let revision = proof.editor.document.state.as_ref().unwrap().revision;
    let generation = proof.editor.activity.requested;
    let select = |view: &str| {
        Message::Control(ControlMessage::SelectView {
            module_id: MODULE.into(),
            group: Some("colour-wheel".into()),
            view: view.into(),
        })
    };
    assert_eq!(
        proof.editor.update(select("compact")).units(),
        0,
        "the view already shown sends nothing"
    );
    assert_eq!(
        proof.editor.update(select("huge")).units(),
        0,
        "an undeclared view sends nothing"
    );
    assert_eq!(proof.editor.update(select("large")).units(), 1);
    // Answered as the task does: the owner's `workspace.set`, and the session it returns.
    let views = json!([{"module": MODULE, "group": "colour-wheel",
                        "view": "large"}]);
    let (answer, _) = call(
        &proof.editor.owner,
        proof.editor.client,
        "workspace.set",
        json!({ "views": views }),
    );
    let session: luxforge_core::ClientSession = serde_json::from_value(answer).unwrap();
    let _ = proof.editor.update(Message::View(
        super::message::view::ViewMessage::WorkspaceUpdated(Ok(session)),
    ));
    assert_eq!(wheel_row(&proof.editor), ("Large".into(), true));
    assert_eq!(proof.editor.document.history.entries.len(), history);
    assert_eq!(
        proof.editor.document.state.as_ref().unwrap().revision,
        revision
    );
    assert_eq!(
        proof.editor.activity.requested, generation,
        "a view asks for no frame"
    );
    assert_eq!(proof.editor.snapshot()["workspace"]["views"], views);
    let (state, _) = call(
        &proof.editor.owner,
        proof.editor.client,
        "session.state",
        json!({}),
    );
    assert_eq!(
        state["workspace"]["views"], views,
        "the API reads the desktop's view"
    );
    proof.finish();
}

/// The preset form offers the proof's one group as one capture group: the wheel's three fields
/// are captured with it once, and neither the wheel's tab row nor its views is a checkbox.
#[test]
fn the_wheels_views_add_no_capture_groups_and_no_duplicate_fields() {
    let proof = Proof::new();
    let groups = crate::state::presets::settings_groups(&proof.editor.modules, true).groups;
    let labels: Vec<&str> = groups.iter().map(|group| group.title.as_str()).collect();
    // The nested Colour wheel group is a capture group of its own, as any nested group is; its
    // Compact and Large views are not.
    assert_eq!(
        labels,
        [
            "Controls \u{00b7} Control vocabulary",
            "Controls \u{00b7} Colour wheel"
        ]
    );
    let parameters = |index: usize| -> Vec<String> {
        groups[index]
            .fields
            .get(ACTION)
            .cloned()
            .unwrap_or_default()
    };
    assert_eq!(
        parameters(1),
        ["wheel-hue", "wheel-saturation", "wheel-luminance"],
        "each wheel field is captured once, by its own group"
    );
    assert!(
        parameters(0).iter().all(|name| !name.starts_with("wheel-")),
        "the enclosing group does not capture the wheel's fields again"
    );
    proof.finish();
}

#[test]
fn query_choice_desktop_uses_the_declared_query_and_shared_selection_request() {
    let mut proof = Proof::new();
    const SELECT: &str = "select-controls-choice";
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::QueryChoiceSearch {
            action: SELECT.into(),
            text: "Three".into(),
        }));
    let stale = proof.editor.controls.ui.query_choices[SELECT]
        .request
        .clone()
        .unwrap();
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::QueryChoiceSearch {
            action: SELECT.into(),
            text: "Two".into(),
        }));
    let current = proof.editor.controls.ui.query_choices[SELECT]
        .request
        .clone()
        .unwrap();
    let _ = proof.editor.update(Message::Control(ControlMessage::QueryChoiceAnswered {
        identity: stale,
        result: Ok(json!({"rows":[{"key":"three", "title":"Three", "eligible":false, "reasons":["Developer refusal example"]}]})),
    }));
    assert!(
        proof.editor.controls.ui.query_choices[SELECT]
            .rows
            .is_empty()
    );
    let declared = crate::state::control_tree::walk(&proof.descriptor().controls)
        .find_map(|control| match control {
            luxforge_core::Control::QueryChoice(control) => Some(control.clone()),
            _ => None,
        })
        .unwrap();
    let mut params = crate::state::query_choice::query_parameters(&declared, &current);
    assert_eq!(params["show-disabled"], true);
    params.insert("asset_id".into(), json!(proof.asset));
    params.insert("entry_id".into(), json!(current.entry));
    let answer = call(
        &proof.editor.owner,
        proof.json_client,
        "query.controls-choices",
        Value::Object(params),
    )
    .0;
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::QueryChoiceAnswered {
            identity: current,
            result: Ok(answer),
        }));
    assert_eq!(
        proof.editor.controls.ui.query_choices[SELECT].rows[0].key,
        "two"
    );
    let parameters = proof.editor.controls.ui.query_choices[SELECT]
        .selection(
            &declared,
            "two",
            Map::from_iter([("show-disabled".into(), json!(true))]),
        )
        .unwrap();
    let copied = proof
        .editor
        .request_for_preset(SELECT, None, Some(&parameters))
        .unwrap();
    assert_eq!(copied["method"], "edit.select-controls-choice");
    assert_eq!(copied["params"]["key"], "two");
    assert_eq!(copied["params"]["show-disabled"], true);
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::QueryChoiceSelect {
            action: SELECT.into(),
            key: "three".into(),
        }));
    assert!(
        !proof.editor.busy,
        "an undisplayed or ineligible choice never submits"
    );
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::QueryChoiceSelect {
            action: SELECT.into(),
            key: "two".into(),
        }));
    assert!(
        proof.editor.busy,
        "an eligible choice reaches the ordinary action service"
    );
    proof.finish();
}

#[test]
fn query_choice_desktop_retry_recovers_with_same_inputs_and_no_automatic_polling() {
    const SELECT: &str = "select-controls-choice";
    let mut proof = Proof::new();
    let first = proof.editor.controls.ui.query_choices[SELECT]
        .request
        .clone()
        .unwrap();
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::QueryChoiceAnswered {
            identity: first.clone(),
            result: Err("not-ready: choices are still being prepared".into()),
        }));
    assert!(proof.editor.controls.ui.query_choices[SELECT].can_retry());
    assert!(
        proof.editor.controls.query_choice_slot.idle(),
        "a failure does not poll"
    );
    let _ = proof.editor.request_visible_query_choices();
    assert!(
        proof.editor.controls.query_choice_slot.idle(),
        "deriving the same screen does not retry"
    );
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::QueryChoiceRetry {
            action: SELECT.into(),
        }));
    let retry = proof.editor.controls.ui.query_choices[SELECT]
        .request
        .clone()
        .unwrap();
    assert_eq!(retry.sequence, first.sequence + 1);
    let mut same_inputs = retry.clone();
    same_inputs.sequence = first.sequence;
    assert_eq!(same_inputs, first);
    assert!(proof.editor.controls.query_choice_slot.in_flight());
    assert!(proof.editor.controls.query_choice_slot.pending().is_none());
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::QueryChoiceRetry {
            action: SELECT.into(),
        }));
    assert_eq!(
        proof.editor.controls.ui.query_choices[SELECT]
            .request
            .as_ref(),
        Some(&retry),
        "repeated Retry while loading queues nothing"
    );
    assert!(proof.editor.controls.query_choice_slot.pending().is_none());
    assert!(
        !proof
            .editor
            .controls
            .ui
            .query_choices
            .get_mut(SELECT)
            .unwrap()
            .accept(
                &first,
                Ok(json!({"rows":[{"key":"old","title":"Old","eligible":true}]}))
            ),
        "a pre-Retry answer cannot replace the new request"
    );
    let declared = crate::state::control_tree::walk(&proof.descriptor().controls)
        .find_map(|control| match control {
            luxforge_core::Control::QueryChoice(control) => Some(control.clone()),
            _ => None,
        })
        .unwrap();
    let mut params = crate::state::query_choice::query_parameters(&declared, &retry);
    params.insert("asset_id".into(), json!(retry.asset));
    params.insert("entry_id".into(), json!(retry.entry));
    let answer = call(
        &proof.editor.owner,
        proof.json_client,
        "query.controls-choices",
        Value::Object(params),
    )
    .0;
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::QueryChoiceAnswered {
            identity: retry.clone(),
            result: Ok(answer),
        }));
    let ui = &proof.editor.controls.ui.query_choices[SELECT];
    assert!(!ui.rows.is_empty() && ui.error.is_none() && !ui.can_retry());
    assert!(proof.editor.controls.query_choice_slot.idle());
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::QueryChoiceRetry {
            action: SELECT.into(),
        }));
    assert_eq!(
        proof.editor.controls.ui.query_choices[SELECT]
            .request
            .as_ref(),
        Some(&retry),
        "Retry after success is inert"
    );
    assert!(proof.editor.controls.query_choice_slot.idle());
    proof.finish();
}

/// The Lens section over a camera JPEG: no list on opening, the detected lens offered with an
/// Apply that carries its acknowledgement, the applied card with Change revealing a search that
/// lists only compatible profiles, and a report link when a search finds none. Every state comes
/// from the core's answer; the desktop only draws and forwards it.
#[test]
fn lens_section_offers_the_detected_lens_then_shows_it_applied_with_a_compatible_search() {
    const SELECT: &str = "select-lens-profile";
    let catalog = temp_catalog("desktop-lens-query-choice");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/geometry/z6-24-70-35mm-grid.jpg");
    let (mut editor, asset, client) = super::testing::real_photo_at(&catalog, &fixture);
    let control = editor
        .modules
        .iter()
        .flat_map(|module| crate::state::control_tree::walk(&module.controls))
        .find_map(|control| match control {
            luxforge_core::Control::QueryChoice(control) if control.action == SELECT => {
                Some(control.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(control.shared, ["focal"]);
    let answer_current = |editor: &mut Editor| {
        let identity = editor.controls.ui.query_choices[SELECT]
            .request
            .clone()
            .unwrap();
        let mut params = crate::state::query_choice::query_parameters(&control, &identity);
        assert!(
            !params.contains_key("focal"),
            "EXIF focal is an absent override"
        );
        params.insert("asset_id".into(), json!(asset));
        params.insert("entry_id".into(), json!(identity.entry));
        let answer = call(
            &editor.owner,
            client,
            "query.lens-profiles",
            Value::Object(params),
        )
        .0;
        let _ = editor.update(Message::Control(ControlMessage::QueryChoiceAnswered {
            identity,
            result: Ok(answer),
        }));
    };
    let submit = |editor: &mut Editor, parameters: &Map<String, Value>| {
        let request = editor
            .request_for_preset(SELECT, None, Some(parameters))
            .unwrap();
        call(
            &editor.owner,
            client,
            "edit.select-lens-profile",
            request["params"].clone(),
        )
        .0
    };
    let refresh = |editor: &mut Editor| {
        let refreshed = tasks::refresh(
            &editor.owner,
            editor.client,
            asset.clone(),
            tasks::Scope::Elsewhere,
            None,
        )
        .unwrap();
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
            refreshed,
        )))));
    };

    // Expanding the collapsed section asks for the status alone: empty text, so no list.
    assert!(!editor.controls.ui.query_choices.contains_key(SELECT));
    let _ = editor.update(Message::Control(ControlMessage::ToggleSection(
        "luxforge.lens".into(),
    )));
    answer_current(&mut editor);
    let ui = &editor.controls.ui.query_choices[SELECT];
    assert_eq!(ui.request.as_ref().unwrap().text, "");
    assert!(ui.rows.is_empty());
    assert!(!ui.shows_search(), "a card hides the search until Change");
    assert!(ui.current.is_none() && ui.report.is_none() && ui.notice.is_none());
    let suggestion = ui.suggestion.clone().unwrap();
    assert_eq!(suggestion.title, "NIKKOR Z 24-70mm f/4 S");
    assert_eq!(suggestion.subtitle.as_deref(), Some("Nikon Z 6 · 35 mm"));
    assert!(suggestion.eligible);
    assert!(suggestion.note.unwrap().contains("assumes it did not"));
    assert!(!ui.shows_shared("focal") && !ui.shows_shared("assume-uncorrected"));

    // Apply sends the suggestion's key and its acknowledgement.
    let shared = ui.request.as_ref().unwrap().shared.clone();
    let parameters = ui.suggested(&control, shared).unwrap();
    assert_eq!(parameters["assume-uncorrected"], true);
    assert_eq!(parameters["profile"], json!(suggestion.key));
    let applied = submit(&mut editor, &parameters);
    assert_eq!(applied["outcome"], "applied");
    refresh(&mut editor);
    assert_eq!(editor.controls.fields.get(SELECT, "focal"), Some(""));
    let layer = editor
        .document
        .state
        .as_ref()
        .unwrap()
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .find(|layer| layer.effect_id == "luxforge.lens.distortion")
        .unwrap()
        .clone();
    assert_eq!(layer.payload["profile"]["focal"]["source"], "exif");
    assert_eq!(
        layer.payload["profile"]["optics"]["acknowledged"],
        "assume-uncorrected"
    );

    // The new entry's answer: the applied card, and still no list.
    let _ = editor.request_visible_query_choices();
    answer_current(&mut editor);
    let ui = &editor.controls.ui.query_choices[SELECT];
    let current = ui.current.clone().unwrap();
    assert_eq!(current.key, suggestion.key);
    assert_eq!(current.title, "NIKKOR Z 24-70mm f/4 S");
    assert!(ui.suggestion.is_none() && ui.rows.is_empty() && !ui.shows_search());

    // Change reveals the search, which asks nothing until text is typed.
    let _ = editor.update(Message::Control(ControlMessage::QueryChoiceChange {
        action: SELECT.into(),
        open: true,
    }));
    assert!(editor.controls.ui.query_choices[SELECT].shows_search());
    assert!(editor.controls.query_choice_slot.idle());
    let _ = editor.update(Message::Control(ControlMessage::QueryChoiceSearch {
        action: SELECT.into(),
        text: "NIKKOR Z 24-70mm".into(),
    }));
    answer_current(&mut editor);
    let ui = &editor.controls.ui.query_choices[SELECT];
    assert!(!ui.rows.is_empty());
    assert!(ui.rows.iter().all(|row| {
        row.eligible && row.parameters.get("assume-uncorrected") == Some(&json!(true))
    }));
    // Choosing the applied profile again keeps the frozen payload and writes nothing.
    let revision = editor.document.state.as_ref().unwrap().revision;
    let shared = ui.request.as_ref().unwrap().shared.clone();
    let parameters = ui.selection(&control, &current.key, shared).unwrap();
    let again = submit(&mut editor, &parameters);
    assert_eq!(again["outcome"], "no-op");
    assert_eq!(again["revision"], revision);

    // A search that finds no compatible profile warns and offers the report, which a test
    // records rather than opening a browser.
    let _ = editor.update(Message::Control(ControlMessage::QueryChoiceSearch {
        action: SELECT.into(),
        text: "Summilux".into(),
    }));
    answer_current(&mut editor);
    let ui = &editor.controls.ui.query_choices[SELECT];
    assert!(ui.rows.is_empty());
    let notice = ui.notice.clone().unwrap();
    assert_eq!(
        notice.level,
        crate::state::query_choice::NoticeLevel::Warning
    );
    assert!(notice.text.contains("Summilux"));
    let report = ui.report.clone().unwrap();
    assert_eq!(report.label, "Report missing lens");
    let _ = editor.update(Message::Control(ControlMessage::QueryChoiceReport {
        action: SELECT.into(),
    }));
    assert_eq!(
        editor.controls.ui.query_choices[SELECT].opened.as_deref(),
        Some(report.url.as_str())
    );
    assert!(editor.status.text.contains("no browser"));

    // Closing Change drops the search and asks for the status again.
    let _ = editor.update(Message::Control(ControlMessage::QueryChoiceChange {
        action: SELECT.into(),
        open: false,
    }));
    let ui = &editor.controls.ui.query_choices[SELECT];
    assert!(!ui.changing && ui.text.is_empty() && ui.rows.is_empty());
    assert!(editor.controls.query_choice_slot.in_flight());
    super::testing::finish(editor, catalog);
}

#[test]
fn proof_curve_query_samples_the_active_channel_through_the_json_method_table() {
    let mut proof = Proof::new();
    let points = proof.editor.control_field_value(ACTION, "master").unwrap();
    let entry = proof.editor.displayed_entry().unwrap();
    let previous_revision = proof.editor.document.state.as_ref().unwrap().revision;
    let (sampled, _) = call(
        &proof.editor.owner,
        proof.json_client,
        "query.sample-controls-curve",
        json!({"asset_id":proof.asset,"entry_id":entry,"master":points}),
    );
    let samples = sampled["points"]
        .as_array()
        .expect("declared sample output");
    assert_eq!(samples.len(), 257);
    assert_eq!(samples[128], json!([0.5, 0.5]));
    assert_eq!(
        proof.editor.document.state.as_ref().unwrap().revision,
        previous_revision,
        "a query changes no recipe"
    );
    let sequence = proof
        .editor
        .curve_sampling
        .requested
        .get(&(ACTION.into(), "master".into()))
        .copied()
        .expect("initial visible query");
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::CurveSampled {
            identity: CurveSampleIdentity {
                sequence,
                asset: proof.asset.clone(),
                entry,
                action: ACTION.into(),
                parameter: "master".into(),
                channel: 0,
                points,
            },
            result: Ok(sampled),
        }));
    let mut models = Vec::new();
    flatten(
        &proof.editor.workspace.tools.developer[0].controls,
        &mut models,
    );
    let curve = models
        .into_iter()
        .find_map(|model| match model {
            ControlModel::Curve(curve) => Some(curve),
            _ => None,
        })
        .unwrap();
    assert_eq!(curve.sampled.len(), 257);
    assert_eq!(curve.sampled[128], [0.5, 0.5]);
    proof.finish();
}

#[test]
fn every_proof_value_control_matches_independent_json_and_authoritative_ui() {
    let mut proof = Proof::new();
    let ui = &mut proof.editor;
    let _ = ui.update(Message::Control(ControlMessage::Fraction {
        action: ACTION.into(),
        parameter: "amount".into(),
        fraction: 0.6,
    }));
    proof.copy_and_post("amount", json!(1.0));

    let _ = proof.editor.update(Message::Control(ControlMessage::Field {
        action: ACTION.into(),
        parameter: "coordinate".into(),
        text: "55".into(),
    }));
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::Submit {
            action: ACTION.into(),
            parameter: Some("coordinate".into()),
        }));
    proof.copy_and_post("coordinate", json!(55.0));

    let _ = proof.editor.update(Message::Control(ControlMessage::Step {
        action: ACTION.into(),
        parameter: "count".into(),
        direction: 1,
    }));
    proof.copy_and_post("count", json!(1));

    for (parameter, value) in [
        ("enabled", json!(true)),
        ("mode", json!("two")),
        ("mode-chips", json!("three")),
        ("mode-menu", json!("five")),
    ] {
        let _ = proof
            .editor
            .update(Message::Control(ControlMessage::Discrete {
                action: ACTION.into(),
                parameter: parameter.into(),
                value: value.clone(),
            }));
        proof.copy_and_post(parameter, value);
    }

    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::TogglePicker {
            action: ACTION.into(),
            parameter: "rgb".into(),
        }));
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::Picker {
            action: ACTION.into(),
            parameter: "rgb".into(),
            event: ColorPickerEvent::Text {
                field: 3,
                text: "#112233".into(),
            },
        }));
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::Picker {
            action: ACTION.into(),
            parameter: "rgb".into(),
            event: ColorPickerEvent::Submit(3),
        }));
    proof.copy_and_post("rgb", json!([17, 34, 51]));

    let original = proof
        .editor
        .controls
        .fields
        .get(ACTION, "rgb-fields")
        .unwrap()
        .to_owned();
    let _ = proof.editor.update(Message::Control(ControlMessage::Field {
        action: ACTION.into(),
        parameter: "rgb-fields".into(),
        text: fields::replace_channel(&original, 0, "17"),
    }));
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::Submit {
            action: ACTION.into(),
            parameter: Some("rgb-fields".into()),
        }));
    proof.copy_and_post("rgb-fields", json!([17, 0, 0]));

    let _ = proof.editor.update(Message::Control(ControlMessage::Curve {
        action: ACTION.into(),
        parameter: "master".into(),
        event: CurveEditorEvent::Move {
            index: 1,
            position: [0.5, 0.75],
        },
    }));
    proof.copy_and_post("master", json!([[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]]));

    let _ = proof.editor.update(Message::Control(ControlMessage::Curve {
        action: ACTION.into(),
        parameter: "master".into(),
        event: CurveEditorEvent::Channel(1),
    }));
    let _ = proof.editor.update(Message::Control(ControlMessage::Curve {
        action: ACTION.into(),
        parameter: "red".into(),
        event: CurveEditorEvent::Move {
            index: 1,
            position: [0.5, 0.25],
        },
    }));
    proof.copy_and_post("red", json!([[0.0, 0.0], [0.5, 0.25], [1.0, 1.0]]));
    let mut models = Vec::new();
    flatten(
        &proof.editor.workspace.tools.developer[0].controls,
        &mut models,
    );
    let curve = models
        .into_iter()
        .find_map(|model| match model {
            ControlModel::Curve(curve) => Some(curve),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        curve.selected_channel, 1,
        "local channel survives authoritative refresh"
    );
    assert_eq!(curve.points[1], [0.5, 0.25]);
    proof.finish();
}

#[test]
fn proof_action_styles_and_group_reset_reach_the_same_json_method() {
    let mut proof = Proof::new();
    let mut models = Vec::new();
    flatten(
        &proof.editor.workspace.tools.developer[0].controls,
        &mut models,
    );
    let presets: Vec<Map<String, Value>> = models
        .iter()
        .filter_map(|model| match model {
            ControlModel::Action(action) => Some(action.preset.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(presets.len(), 3);
    for preset in presets {
        assert_eq!(
            preset.len(),
            1,
            "each patch action button changes one field"
        );
        let _ = proof.editor.update(Message::Action(ActionMessage::Run {
            action: ACTION.into(),
            preset: preset.clone(),
        }));
        proof.post_preset(&preset);
    }
    // The proof's controls are one declared group, which the panel draws without a header: its
    // reset is reached from the band, whose reset is the module's. The group declares the reset
    // every field-patch group does, the patch of its fields to their defaults, which here is every
    // field and so the same state the module reset reaches.
    let section = &proof.editor.workspace.tools.developer[0];
    // Its one nested group, the wheel's tab row, keeps its own header and path.
    assert!(
        !section
            .controls
            .iter()
            .any(|control| matches!(control, ControlModel::Group(group) if group.path == [0])),
        "the module's only group is drawn without a header"
    );
    let Some(luxforge_core::Control::Group(luxforge_core::GroupControl {
        reset: Some(declared),
        ..
    })) = proof.descriptor().controls.first()
    else {
        panic!("the proof declares one resettable group");
    };
    let reset = section.reset.clone().expect("the band's reset");
    assert_eq!(reset.action, "reset-controls");
    assert_eq!(
        Some(&reset.action),
        proof.descriptor().reset.as_ref().map(|reset| &reset.action),
        "the band resets what the module declares"
    );
    assert_eq!(declared.action, ACTION);
    let defaults: Map<String, Value> = proof
        .descriptor()
        .action(ACTION)
        .unwrap()
        .parameters
        .iter()
        .map(|parameter| (parameter.name.clone(), parameter.default.clone().unwrap()))
        .collect();
    assert_eq!(
        declared.preset, defaults,
        "the group resets every field to its default"
    );
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::ResetModule(MODULE.into())));
    let _ = proof
        .editor
        .update(Message::Action(ActionMessage::CopyRequest {
            action: reset.action.clone(),
            parameter: None,
            preset: Some(reset.preset.clone()),
        }));
    assert_eq!(
        proof.editor.status.text,
        "Copied the edit.reset-controls request"
    );
    let copied = proof
        .editor
        .request_for_preset(&reset.action, None, Some(&reset.preset))
        .unwrap();
    assert_eq!(copied["method"], format!("edit.{}", reset.action));
    assert_eq!(
        copied["params"].as_object().unwrap().len(),
        2,
        "the separate reset action takes no edit field"
    );
    let mut independent = copied["params"].clone();
    independent["mutation"]["request_id"] = json!(format!(
        "proof-json-{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    independent["mutation"]["actor"] = json!("proof-json-client");
    call(
        &proof.editor.owner,
        proof.json_client,
        &format!("edit.{}", reset.action),
        independent,
    );
    let refreshed = tasks::refresh(
        &proof.editor.owner,
        proof.editor.client,
        proof.asset.clone(),
        tasks::Scope::Elsewhere,
        None,
    )
    .unwrap();
    let _ = proof
        .editor
        .update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
            refreshed,
        )))));
    for parameter in &proof.descriptor().action(ACTION).unwrap().parameters {
        let expected = parameter.default.as_ref().unwrap();
        assert_eq!(
            proof.editor.control_field_value(ACTION, &parameter.name),
            Some(expected.clone())
        );
    }
    proof.finish();
}

/// The shared curve editor's desktop rules, on the proof's curve: its `master` channel holds 2 to
/// 8 monotone points and its control is labelled `Curve`.
mod curve_editor {
    use super::*;
    use crate::state::tools::{CURVE_HINT, CurveControl, CurveUi, ValueEdit};
    use luxforge_core::{Control, GroupControl, ParameterKind};

    const MASTER: &str = "master";
    const THREE: [[f64; 2]; 3] = [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]];

    impl Proof {
        /// The proof's curve as the tools panel derives it.
        fn curve(&self) -> CurveControl {
            let mut models = Vec::new();
            flatten(
                &self.editor.workspace.tools.developer[0].controls,
                &mut models,
            );
            models
                .into_iter()
                .find_map(|model| match model {
                    ControlModel::Curve(curve) => Some(curve.clone()),
                    _ => None,
                })
                .expect("the proof draws its curve")
        }

        /// The curve's local state, keyed by its action and first channel.
        fn curve_ui(&self) -> CurveUi {
            self.editor
                .controls
                .ui
                .curve(&(ACTION.into(), MASTER.into()))
                .cloned()
                .unwrap_or_default()
        }

        /// One curve editor event on the `master` channel, and the task it returned.
        fn curve_event(&mut self, event: CurveEditorEvent) -> iced::Task<Message> {
            self.editor.update(Message::Control(ControlMessage::Curve {
                action: ACTION.into(),
                parameter: MASTER.into(),
                event,
            }))
        }

        /// Set the `master` points as an independent JSON client would, and refresh the desktop.
        fn post_points(&mut self, points: Value) {
            let mutation = json!({
                "expected_revision": self.editor.document.state.as_ref().unwrap().revision,
                "request_id": format!("proof-json-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                "actor": "proof-json-client"
            });
            call(
                &self.editor.owner,
                self.json_client,
                &format!("edit.{ACTION}"),
                json!({"asset_id": self.asset, "mutation": mutation, MASTER: points}),
            );
            let refreshed = tasks::refresh(
                &self.editor.owner,
                self.editor.client,
                self.asset.clone(),
                tasks::Scope::Elsewhere,
                None,
            )
            .unwrap();
            let _ = self
                .editor
                .update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
                    refreshed,
                )))));
            assert_eq!(
                self.editor.control_field_value(ACTION, MASTER),
                Some(points)
            );
            assert!(!self.editor.busy);
        }

        /// The `master` points the desktop would send next.
        fn sent_points(&mut self) -> Value {
            self.editor.request_for(ACTION, Some(MASTER)).unwrap()["params"][MASTER].clone()
        }

        /// Load `descriptor` in place of the proof's own, as discovery would.
        fn load(&mut self, descriptor: ModuleDescriptor) {
            let _ = self
                .editor
                .update(Message::Sync(SyncMessage::ModulesLoaded(Ok(vec![
                    descriptor,
                ]))));
        }

        /// The proof's descriptor with the `master` parameter declared as `kind`.
        fn with_master_kind(&self, kind: ParameterKind) -> ModuleDescriptor {
            let mut descriptor = self.descriptor().clone();
            for action in &mut descriptor.actions {
                for parameter in &mut action.parameters {
                    if parameter.name == MASTER {
                        parameter.kind = kind.clone();
                    }
                }
            }
            descriptor
        }
    }

    #[test]
    fn adding_a_point_selects_it() {
        let mut proof = Proof::new();
        proof.post_points(json!(THREE));
        let _ = proof.curve_event(CurveEditorEvent::Select(2));
        assert_eq!(proof.curve().selected_point, Some(2));

        let _ = proof.curve_event(CurveEditorEvent::Add([0.25, 0.25]));
        assert!(proof.editor.busy, "the add commits at once");
        assert_eq!(
            proof.sent_points(),
            json!([[0.0, 0.0], [0.25, 0.25], [0.5, 0.5], [1.0, 1.0]])
        );
        assert_eq!(
            proof.curve_ui().point,
            Some(1),
            "the new point's sorted index is selected"
        );
        let curve = proof.curve();
        assert_eq!(curve.selected_point, Some(1));
        assert_eq!(curve.points[1], [0.25, 0.25]);
        proof.finish();
    }

    /// The widget publishes a drag straight after the press that added its point, before the add
    /// has answered. That move opens no draft; the first move after the answer drafts on the
    /// revision the add made.
    #[test]
    fn a_drag_after_an_add_drafts_once_the_add_has_answered() {
        let mut proof = Proof::new();
        proof.post_points(json!(THREE));
        let _ = proof.curve_event(CurveEditorEvent::Add([0.25, 0.25]));
        assert!(proof.editor.busy, "the add is in flight");
        let early = proof.curve_event(CurveEditorEvent::Move {
            index: 1,
            position: [0.25, 0.4],
        });
        assert_eq!(
            early.units(),
            0,
            "nothing is sent while the add is in flight"
        );
        assert!(proof.editor.slider_gesture().is_none());
        assert_eq!(
            proof.sent_points(),
            json!([[0.0, 0.0], [0.25, 0.25], [0.5, 0.5], [1.0, 1.0]]),
            "the held move changes no value"
        );

        // The add's request, answered as its task would answer it.
        let request = proof.editor.request_for(ACTION, Some(MASTER)).unwrap();
        let refreshed = tasks::command_now(
            &proof.editor.owner,
            proof.editor.client,
            proof.asset.clone(),
            request["method"].as_str().unwrap(),
            request["params"].clone(),
            None,
        )
        .unwrap();
        let added = refreshed.state.revision;
        let _ = proof
            .editor
            .update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
                refreshed,
            )))));
        assert!(!proof.editor.busy);

        let _ = proof.curve_event(CurveEditorEvent::Move {
            index: 1,
            position: [0.25, 0.375],
        });
        assert!(proof.editor.slider_gesture().is_some(), "the drag drafts");
        assert_eq!(
            proof.editor.document.state.as_ref().unwrap().revision,
            added,
            "on the revision the add made"
        );
        assert_eq!(
            proof.sent_points(),
            json!([[0.0, 0.0], [0.25, 0.375], [0.5, 0.5], [1.0, 1.0]])
        );
        proof.finish();
    }

    #[test]
    fn removing_a_point_clears_the_selection() {
        let mut proof = Proof::new();
        proof.post_points(json!(THREE));
        let _ = proof.curve_event(CurveEditorEvent::Select(1));
        assert_eq!(proof.curve().selected_point, Some(1));

        let _ = proof.curve_event(CurveEditorEvent::Remove(1));
        assert!(proof.editor.busy, "the removal commits at once");
        assert_eq!(proof.sent_points(), json!([[0.0, 0.0], [1.0, 1.0]]));
        assert_eq!(proof.curve_ui().point, None);
        assert_eq!(proof.curve().selected_point, None);
        proof.finish();
    }

    /// A double-click on a point, Delete or Backspace on the selected point and a point row's
    /// remove button all publish the same `Remove`, so this refusal covers every desktop removal.
    #[test]
    fn removing_an_end_point_is_refused_with_a_status() {
        let mut proof = Proof::new();
        proof.post_points(json!(THREE));
        for end in [0, 2] {
            let _ = proof.curve_event(CurveEditorEvent::Select(end));
            proof.editor.status.text.clear();
            let task = proof.curve_event(CurveEditorEvent::Remove(end));
            assert_eq!(
                proof.editor.status.text,
                "The end points move but are not removed"
            );
            assert_eq!(task.units(), 0, "nothing is sent");
            assert!(!proof.editor.busy);
            assert_eq!(
                proof.editor.control_field_value(ACTION, MASTER),
                Some(json!(THREE))
            );
            assert_eq!(
                proof.curve().selected_point,
                Some(end),
                "a refused removal keeps the selection"
            );
        }
        proof.finish();
    }

    #[test]
    fn an_add_past_the_maximum_reports_the_limit() {
        let mut proof = Proof::new();
        let full: Vec<[f64; 2]> = (0..8)
            .map(|k| {
                let x = f64::from(k) / 7.0;
                [x, x]
            })
            .collect();
        proof.post_points(json!(full));
        let task = proof.curve_event(CurveEditorEvent::Add([0.1, 0.1]));
        assert_eq!(proof.editor.status.text, "Curve holds at most 8 points");
        assert_eq!(task.units(), 0, "nothing is sent");
        assert!(!proof.editor.busy);
        assert_eq!(
            proof.editor.control_field_value(ACTION, MASTER),
            Some(json!(full))
        );
        proof.finish();
    }

    #[test]
    fn a_remove_below_the_minimum_reports_the_limit() {
        let mut proof = Proof::new();
        proof.post_points(json!([[0.0, 0.0], [1.0, 1.0]]));
        assert_eq!(proof.curve().points.len(), 2, "the curve is at its minimum");
        for index in [0, 1] {
            proof.editor.status.text.clear();
            let task = proof.curve_event(CurveEditorEvent::Remove(index));
            assert_eq!(proof.editor.status.text, "Curve needs at least 2 points");
            assert_eq!(task.units(), 0, "nothing is sent");
            assert!(!proof.editor.busy);
            assert_eq!(proof.curve().points.len(), 2);
        }
        proof.finish();
    }

    #[test]
    fn opening_the_points_list_sends_no_request() {
        let mut proof = Proof::new();
        let closed = proof.curve();
        assert!(!closed.points_open, "the list starts closed");
        let fields = proof.editor.controls.fields.clone();
        let revision = proof.editor.document.state.as_ref().unwrap().revision;
        let sequence = proof.editor.curve_sampling.sequence;
        for open in [true, false, true] {
            let task = proof.curve_event(CurveEditorEvent::Points(open));
            assert_eq!(task.units(), 0, "toggling the list sends nothing");
            assert!(!proof.editor.busy);
            assert!(proof.editor.slider_gesture().is_none());
            assert_eq!(proof.editor.controls.fields, fields);
            assert_eq!(
                proof.editor.document.state.as_ref().unwrap().revision,
                revision
            );
            assert_eq!(
                proof.editor.curve_sampling.sequence, sequence,
                "no sample query is asked"
            );
            assert_eq!(proof.curve_ui().points_open, open);
            let curve = proof.curve();
            assert_eq!(curve.points_open, open);
            assert_eq!(curve.version, closed.version, "the plot is not redrawn");
        }

        // Open or closed is view state: a refresh of the photo and dropped samples keep it.
        proof.post_points(json!(THREE));
        assert!(proof.curve().points_open, "a refresh keeps the list open");
        proof.editor.controls.ui.clear_curve_samples();
        assert!(
            proof.curve_ui().points_open,
            "dropping samples keeps it open"
        );
        proof.finish();
    }

    #[test]
    fn closing_the_points_list_drops_uncommitted_text() {
        let mut proof = Proof::new();
        proof.post_points(json!(THREE));
        let _ = proof.curve_event(CurveEditorEvent::Points(true));
        let _ = proof.curve_event(CurveEditorEvent::Text {
            index: 1,
            axis: 1,
            text: "0.7".into(),
        });
        assert_eq!(
            proof.curve().point_rows[1].edit[1],
            ValueEdit::Typing("0.7".into())
        );

        let task = proof.curve_event(CurveEditorEvent::Points(false));
        assert_eq!(task.units(), 0, "closing sends nothing");
        assert!(!proof.editor.busy);
        assert!(
            proof.curve_ui().edits.is_empty(),
            "the typed text is dropped"
        );
        let _ = proof.curve_event(CurveEditorEvent::Points(true));
        assert_eq!(
            proof.curve().point_rows[1].edit[1],
            ValueEdit::None,
            "reopening shows the committed value"
        );
        let _ = proof.curve_event(CurveEditorEvent::Submit { index: 1, axis: 1 });
        assert!(
            !proof.editor.busy,
            "Enter after reopening has no typed text to commit"
        );
        assert_eq!(
            proof.editor.control_field_value(ACTION, MASTER),
            Some(json!(THREE)),
            "nothing typed was committed"
        );
        proof.finish();
    }

    #[test]
    fn the_hint_shows_only_when_points_can_be_added_and_removed() {
        let mut proof = Proof::new();
        let curve = proof.curve();
        assert_eq!(curve.hint.as_deref(), Some(CURVE_HINT));
        assert_eq!(curve.points_max, 8);

        for (case, points_min, points_max, fixed_x) in [
            ("a fixed x", 3, 3, Some(vec![0.0, 0.5, 1.0])),
            ("a fixed x with a varying count", 2, 8, Some(vec![0.0, 1.0])),
            ("a fixed point count", 2, 2, None),
        ] {
            let descriptor = proof.with_master_kind(ParameterKind::Curve {
                points_min,
                points_max,
                monotone: true,
                fixed_x,
            });
            proof.load(descriptor);
            let curve = proof.curve();
            assert_eq!(curve.hint, None, "{case}: no hint");
            assert_eq!(curve.points_max, points_max);
        }
        let descriptor = proof.with_master_kind(ParameterKind::Curve {
            points_min: 2,
            points_max: 16,
            monotone: true,
            fixed_x: None,
        });
        proof.load(descriptor);
        let curve = proof.curve();
        assert_eq!(curve.hint.as_deref(), Some(CURVE_HINT));
        assert_eq!(curve.points_max, 16);
        proof.finish();
    }

    #[test]
    fn a_single_curve_group_draws_no_label_line() {
        let mut proof = Proof::new();
        assert!(
            proof.curve().label_shown,
            "a curve beside other controls in a headerless group keeps its label"
        );

        let mut descriptor = proof.descriptor().clone();
        let Some(Control::Group(GroupControl { controls, .. })) = descriptor.controls.first_mut()
        else {
            panic!("the proof's controls are one group");
        };
        controls.retain(|control| matches!(control, Control::Curve(_)));
        assert_eq!(controls.len(), 1);
        proof.load(descriptor.clone());
        let section = &proof.editor.workspace.tools.developer[0];
        assert!(
            matches!(section.controls.as_slice(), [ControlModel::Curve(_)]),
            "the curve is the headerless section's only control"
        );
        let curve = proof.curve();
        assert!(!curve.label_shown, "the band already names it");
        assert_eq!(curve.label, "Curve");

        // With a second group the module is no longer headerless: the curve sits under its
        // group's header, which does not name the control, so it keeps its label line.
        let mut second = descriptor.controls[0].clone();
        if let Control::Group(group) = &mut second {
            group.label = "Second".into();
        }
        descriptor.controls.push(second);
        proof.load(descriptor);
        assert!(proof.curve().label_shown);
        proof.finish();
    }
}
