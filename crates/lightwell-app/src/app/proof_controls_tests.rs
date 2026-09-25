//! The registered developer module proves the generated desktop vocabulary against the same
//! JSON method table an independent client uses. No descriptor-only desktop fixture is involved.
use super::{
    Boot, Editor,
    controls::CurveSampleIdentity,
    message::{ActionMessage, ControlMessage, Message, SyncMessage},
    tasks,
};
use crate::Config;
use crate::state::fields;
use crate::state::tools::ControlModel;
use lightwell_core::{
    ApiRequest, AssetId, ClientId, ControlsModule, ModuleDescriptor, ModuleRegistry, OwnerHandle,
};
use lightwell_ui::{ColorPickerEvent, CurveEditorEvent};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
const MODULE: &str = "lightwell.controls";
const ACTION: &str = "set-controls";

struct Proof {
    editor: Editor,
    catalog: PathBuf,
    asset: AssetId,
    json_client: ClientId,
}

impl Proof {
    fn new() -> Self {
        let catalog = std::env::temp_dir().join(format!(
            "lightwell-controls-parity-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut registry = ModuleRegistry::new();
        registry.register(Arc::new(ControlsModule::new())).unwrap();
        let (owner, join) = OwnerHandle::start_with(&catalog, Arc::new(registry)).unwrap();
        let json_client = owner.register();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
        let queued = call(
            &owner,
            json_client,
            "catalog.import",
            json!({"path":source,"mutation":crate::app::tasks::request()}),
        )
        .0;
        let job_id = queued["job_id"].as_str().expect("source job");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = call(&owner, json_client, "job.status", json!({"job_id":job_id})).0;
            match status["status"].as_str() {
                Some("ready") => break,
                Some("queued" | "running") => {
                    assert!(Instant::now() < deadline, "source preparation: {status}");
                    std::thread::sleep(Duration::from_millis(1));
                }
                other => panic!("source preparation failed {other:?}: {status}"),
            }
        }
        let imported = call(&owner, json_client, "job.adopt", json!({"job_id":job_id})).0;
        let asset = AssetId::parse(imported["asset"]["asset"]["id"].as_str().unwrap()).unwrap();
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
        editor.expanded.insert(MODULE.into(), true);
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
        assert!(editor.editable());
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
                    return if number.integer {
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
            self.editor.status,
            format!("Copied the edit.{ACTION} request")
        );
        let copied = self.editor.request_for(ACTION, Some(parameter)).unwrap();
        assert_eq!(copied["method"], format!("edit.{ACTION}"));
        let params = copied["params"].as_object().unwrap();
        assert_eq!(params.len(), 3, "a patch control sends exactly one field");
        assert_eq!(params[parameter], expected, "{parameter} desktop value");
        assert_eq!(
            params["mutation"]["expected_revision"],
            self.editor.state.as_ref().unwrap().revision
        );

        // Construct the request independently, as a JSON client would; only the request identity
        // and actor differ from the desktop's copied envelope.
        let mutation = json!({
            "expected_revision": self.editor.state.as_ref().unwrap().revision,
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
        self.editor.dragging = None;
        self.editor.editing = None;
        self.editor
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
            self.editor.status,
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
        self.editor.dragging = None;
        self.editor.editing = None;
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
    for control in controls {
        output.push(control);
        if let ControlModel::Group(group) = control {
            flatten(&group.controls, output);
        }
    }
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
            [lightwell_core::Control::Group { reset: Some(_), .. }]
        ),
        "the proof declares one resettable group"
    );
    let signatures: BTreeSet<String> = controls
        .iter()
        .map(|control| match control {
            ControlModel::Group(_) => panic!("the proof's only group draws no header"),
            ControlModel::Slider(number) => format!("number:{:?}", number.style),
            ControlModel::Toggle(_) => "toggle".into(),
            ControlModel::Enum(choice) => format!("choice:{:?}", choice.style),
            ControlModel::Color(color) => format!("color:{:?}", color.style),
            ControlModel::Curve(curve) => {
                assert_eq!(curve.channels.len(), 2);
                assert!(curve.background);
                "curve".into()
            }
            ControlModel::Action(action) => format!("action:{:?}", action.style),
            ControlModel::Picker(_) => panic!("proof module declares no canvas picker"),
            ControlModel::Task(_) => panic!("proof module declares no task"),
            ControlModel::Unsupported(kind) => panic!("unsupported proof control {kind}"),
            ControlModel::CropFrame(_) => panic!("proof module declares no crop frame"),
            ControlModel::Presets(_) => panic!("proof module declares no preset library"),
        })
        .collect();
    let expected = BTreeSet::from([
        "number:Slider".into(),
        "number:Field".into(),
        "number:Stepper".into(),
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
    ]);
    assert_eq!(signatures, expected);
    assert!(proof.descriptor().action(ACTION).unwrap().patch);
    proof.finish();
}

#[test]
fn proof_curve_query_samples_the_active_channel_through_the_json_method_table() {
    let mut proof = Proof::new();
    let points = proof.editor.control_field_value(ACTION, "master").unwrap();
    let entry = proof.editor.displayed_entry().unwrap();
    let previous_revision = proof.editor.state.as_ref().unwrap().revision;
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
        proof.editor.state.as_ref().unwrap().revision,
        previous_revision,
        "a query changes no recipe"
    );
    let sequence = proof
        .editor
        .curve_sample_requested
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
    // reset is reached from the band, whose reset is the same declared action.
    let section = &proof.editor.workspace.tools.developer[0];
    assert!(
        !section
            .controls
            .iter()
            .any(|control| matches!(control, ControlModel::Group(_))),
        "the module's only group is drawn without a header"
    );
    let Some(lightwell_core::Control::Group {
        reset: Some(declared),
        ..
    }) = proof.descriptor().controls.first()
    else {
        panic!("the proof declares one resettable group");
    };
    let reset = section.reset.clone().expect("the band's reset");
    assert_eq!(reset.action, "reset-controls");
    assert_eq!(
        reset.action, declared.action,
        "the band resets what the group declares"
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
        proof.editor.status,
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
