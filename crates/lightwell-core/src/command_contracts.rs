//! Current command contracts across the core service and the JSON API.
use crate::{
    ApiRequest, ClientId, EditorService, ErrorKind, Mutation, MutationOutcome, OwnerHandle,
    Transform,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lightwell-command-contracts-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0")
        .join(name)
}

fn mutation(revision: u64, request: &str, actor: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: actor.into(),
    }
}

fn stacks(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .map(|entry| {
            json!({
                "action_id": entry["action_id"],
                "parameters": entry["parameters"],
                "layers": entry["snapshot"]["recipe"]["layers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|layer| json!({
                        "effect_id": layer["effect_id"],
                        "effect_format": layer["effect_format"],
                        "payload": layer["payload"],
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

const PROBES: [(u32, u32); 4] = [(0, 0), (2, 1), (1, 0), (479, 319)];

fn direct_journey(catalog: &Path, source: &Path, wrappers: bool) -> (Vec<Value>, Vec<Value>) {
    let mut service = EditorService::open(catalog).unwrap();
    let asset = service.import(source).unwrap().asset.id;
    if wrappers {
        service
            .apply_pixel(&asset, mutation(0, "a", "parity"), 0, 0, [1, 2, 3])
            .unwrap();
        service
            .apply_transform(&asset, mutation(1, "b", "parity"), Transform::RotateRight)
            .unwrap();
        service
            .apply_pixel(&asset, mutation(2, "c", "parity"), 2, 1, [4, 5, 6])
            .unwrap();
        service
            .apply_transform(
                &asset,
                mutation(3, "d", "parity"),
                Transform::MirrorHorizontal,
            )
            .unwrap();
    } else {
        for (revision, request, action, parameters) in [
            (0, "a", "set-pixel", json!({"x":0,"y":0,"rgb":[1,2,3]})),
            (1, "b", "transform", json!({"transform":"rotate-right"})),
            (2, "c", "set-pixel", json!({"x":2,"y":1,"rgb":[4,5,6]})),
            (
                3,
                "d",
                "transform",
                json!({"transform":"mirror-horizontal"}),
            ),
        ] {
            service
                .apply_action(
                    &asset,
                    mutation(revision, request, "parity"),
                    action,
                    parameters,
                )
                .unwrap();
        }
    }
    let entries: Vec<Value> = serde_json::to_value(service.history(&asset, None, 50).unwrap())
        .unwrap()["entries"]
        .as_array()
        .unwrap()
        .clone();
    let current = service.state(&asset).unwrap().current_entry.id;
    let samples = PROBES
        .iter()
        .map(|(x, y)| {
            let sampled = service.sample_entry(&asset, &current, *x, *y).unwrap();
            json!([sampled.rgba, sampled.width, sampled.height])
        })
        .collect();
    drop(service);
    (stacks(&entries), samples)
}

fn api_journey(catalog: &Path, source: &Path) -> (Vec<Value>, Vec<Value>, Value) {
    let (owner, join) = OwnerHandle::start(catalog).unwrap();
    let client = owner.register();
    let call = |client: ClientId, method: &str, params: Value| -> Value {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .unwrap();
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.unwrap()
    };
    let asset = call(client, "catalog.import", json!({"path": source}))["asset"]["id"].clone();
    for (revision, request, method, mut params) in [
        (0, "a", "edit.set-pixel", json!({"x":0,"y":0,"rgb":[1,2,3]})),
        (
            1,
            "b",
            "edit.transform",
            json!({"transform":"rotate-right"}),
        ),
        (2, "c", "edit.set-pixel", json!({"x":2,"y":1,"rgb":[4,5,6]})),
        (
            3,
            "d",
            "edit.transform",
            json!({"transform":"mirror-horizontal"}),
        ),
    ] {
        let object = params.as_object_mut().unwrap();
        object.insert("asset_id".into(), asset.clone());
        object.insert(
            "mutation".into(),
            json!({"expected_revision":revision,"request_id":request,"actor":"parity"}),
        );
        call(client, method, params);
    }
    let entries = call(client, "history.list", json!({"asset_id":asset,"limit":50}))["entries"]
        .as_array()
        .unwrap()
        .clone();
    let samples = PROBES
        .iter()
        .map(|(x, y)| {
            let sampled = call(
                client,
                "render.sample",
                json!({"asset_id":asset,"x":x,"y":y}),
            );
            json!([sampled["rgba"], sampled["width"], sampled["height"]])
        })
        .collect();
    let malformed = owner
        .call(
            client,
            ApiRequest {
                id: "malformed".into(),
                method: "edit.set-pixel".into(),
                params: json!({"asset_id":asset,"mutation":{"expected_revision":4,"request_id":"e","actor":"parity"},"x":0,"y":0,"rgb":[1,2]}),
                token: None,
            },
        )
        .unwrap();
    let failure = serde_json::to_value(malformed.error.unwrap()).unwrap();
    owner.stop();
    join.join().unwrap();
    (stacks(&entries), samples, failure)
}

#[test]
fn wrappers_actions_and_the_api_produce_identical_entries_pixels_and_errors() {
    let dir = temp("parity");
    let source = dir.join("orientation-6.jpg");
    std::fs::copy(fixture("orientation-6.jpg"), &source).unwrap();
    let bytes = std::fs::read(&source).unwrap();

    let wrapped = direct_journey(&dir.join("wrappers.sqlite"), &source, true);
    let actions = direct_journey(&dir.join("actions.sqlite"), &source, false);
    let (api_stacks, api_samples, failure) = api_journey(&dir.join("api.sqlite"), &source);

    assert_eq!(wrapped.0, actions.0, "wrappers and apply_action agree");
    assert_eq!(wrapped.0, api_stacks, "the API agrees with the core");
    assert_eq!(wrapped.1, actions.1);
    assert_eq!(wrapped.1, api_samples);
    assert_eq!(
        wrapped.0[0]["action_id"],
        json!("mirror-horizontal"),
        "durable transform action identities"
    );

    // One malformed request reports the same structured error everywhere.
    let mut service = EditorService::open(&dir.join("errors.sqlite")).unwrap();
    let asset = service.import(&source).unwrap().asset.id;
    let direct = service
        .apply_action(
            &asset,
            mutation(0, "e", "parity"),
            "set-pixel",
            json!({"x":0,"y":0,"rgb":[1,2]}),
        )
        .unwrap_err();
    assert_eq!(
        json!({"code": direct.kind.code(), "message": direct.detail}),
        failure
    );
    drop(service);

    assert_eq!(std::fs::read(&source).unwrap(), bytes, "source unchanged");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Every workspace addition through the JSON API alone: labels on history rows, the recipe
/// description of the committed stack, and the session's workspace state.
#[test]
fn the_workspace_additions_are_reachable_through_the_json_api() {
    let dir = temp("workspace");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture("orientation-1.jpg"), &source).unwrap();
    let bytes = std::fs::read(&source).unwrap();
    let (owner, join) = OwnerHandle::start(&dir.join("catalog.sqlite")).unwrap();
    let client = owner.register();
    let request = |method: &str, params: Value| -> Result<Value, Value> {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .unwrap();
        match response.error {
            Some(error) => Err(serde_json::to_value(error).unwrap()),
            None => Ok(response.result.unwrap()),
        }
    };
    let call = |method: &str, params: Value| -> Value {
        request(method, params).unwrap_or_else(|error| panic!("{method}: {error}"))
    };

    // Every descriptor addition the workspace renders from.
    let modules = call("module.list", json!({}));
    let module = |id: &str| -> Value {
        modules["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|module| module["id"] == json!(id))
            .unwrap_or_else(|| panic!("{id} is registered"))
            .clone()
    };
    assert_eq!(
        module("lightwell.crop")["hint"],
        json!("Frame, ratio and angle")
    );
    assert_eq!(
        module("lightwell.crop")["reset"],
        json!({"action": "crop-reset", "preset": {}})
    );
    assert_eq!(module("lightwell.crop")["canvas"]["shortcut"], json!("R"));
    assert_eq!(module("lightwell.pixel")["developer"], json!(true));

    let asset = call("catalog.import", json!({"path": source}))["asset"]["id"].clone();
    let original = call("asset.state", json!({"asset_id": asset}))["current_entry"].clone();
    assert_eq!(original["label"], json!("Original"));
    call(
        "edit.transform",
        json!({"asset_id":asset,"mutation":{"expected_revision":0,"request_id":"a","actor":"workspace"},"transform":"rotate-right"}),
    );
    call(
        "edit.crop",
        json!({"asset_id":asset,"mutation":{"expected_revision":1,"request_id":"b","actor":"workspace"},"x":0.0,"y":0.0,"width":0.5,"height":0.5}),
    );
    let entries = call("history.list", json!({"asset_id": asset}))["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry["action_id"].clone(), entry["label"].clone()))
            .collect::<Vec<_>>(),
        [
            (json!("crop"), json!("Crop 0°")),
            (json!("rotate-right"), json!("Rotate right")),
            (json!("original"), json!("Original")),
        ]
    );

    // The recipe rows for the committed stack, in stored order.
    let described = call("recipe.describe", json!({"asset_id": asset}));
    assert_eq!(described["entry_id"], entries[0]["id"]);
    assert_eq!(
        described["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| (
                layer["module"].clone(),
                layer["summary"].clone(),
                layer["available"].clone()
            ))
            .collect::<Vec<_>>(),
        [
            (
                json!("lightwell.transform"),
                json!("Rotate right"),
                json!(true)
            ),
            (json!("lightwell.crop"), json!("50% × 50%"), json!(true)),
        ]
    );
    assert!(
        call(
            "recipe.describe",
            json!({"asset_id": asset, "entry_id": original["id"]})
        )["layers"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the original entry has no layers"
    );

    // Workspace state is per-client session state, reported like the view.
    assert_eq!(
        call("session.state", json!({}))["workspace"],
        json!({"state_panel": true, "tools_panel": true, "mode": "pointer", "thirds": false})
    );
    assert_eq!(
        call(
            "workspace.set",
            json!({"mode": "lightwell.crop", "tools_panel": false})
        )["workspace"],
        json!({"state_panel": true, "tools_panel": false, "mode": "lightwell.crop", "thirds": false})
    );
    let refused = request("workspace.set", json!({"mode": "lightwell.transform"}))
        .expect_err("a module without a canvas is not a mode");
    assert_eq!(refused["code"], json!("validation"));
    assert_eq!(
        call("session.state", json!({}))["workspace"]["mode"],
        json!("lightwell.crop")
    );
    owner.stop();
    join.join().unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), bytes, "source unchanged");
    std::fs::remove_dir_all(dir).unwrap();
}

/// The orientation layer changes what the stack holds, not what anyone can ask for: the four
/// durable action identities, the `edit.transform` method with its enum and the four generated
/// controls stay exactly as they were, and the module declares the one orientation effect.
#[test]
fn transform_actions_controls_and_the_orientation_effect_stay_discoverable() {
    const IDENTITIES: [&str; 4] = [
        "rotate-left",
        "rotate-right",
        "mirror-horizontal",
        "flip-vertical",
    ];
    let dir = temp("transform-discovery");
    let (owner, join) = OwnerHandle::start(&dir.join("api.sqlite")).unwrap();
    let client = owner.register();
    let call = |method: &str| -> Value {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params: json!({}),
                    token: None,
                },
            )
            .unwrap();
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.unwrap()
    };

    let schema = call("schema.list");
    let method = schema["methods"]["edit.transform"].clone();
    assert!(method["mutates"].as_bool().unwrap());
    assert_eq!(
        method["required"],
        json!(["asset_id", "mutation", "transform"])
    );
    let declared = method["parameters"]
        .as_array()
        .expect("the declared parameters")
        .clone();
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0]["name"], json!("transform"));
    assert_eq!(declared[0]["kind"], json!("enum"));
    assert_eq!(declared[0]["options"], json!(IDENTITIES));

    let modules = call("module.list");
    let transform = modules["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == json!("lightwell.transform"))
        .expect("the transform module descriptor")
        .clone();
    assert_eq!(
        transform["effects"],
        json!([{"id":"lightwell.geometry.orientation","format":1,"stage":"geometry"}]),
        "one geometry effect, holding the composed orientation"
    );
    let grouped = transform["controls"].as_array().unwrap();
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0]["kind"], json!("group"));
    let controls = grouped[0]["controls"].as_array().unwrap();
    assert_eq!(
        controls
            .iter()
            .map(|control| {
                assert_eq!(control["kind"], json!("action"));
                assert_eq!(control["action"], json!("transform"));
                control["preset"]["transform"].clone()
            })
            .collect::<Vec<_>>(),
        IDENTITIES.map(Value::from).to_vec(),
        "one control per durable action identity"
    );
    owner.stop();
    join.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// The crop journey used for parity: one rectangle, a ratio fit at an angle, a reset and an angled
/// rectangle, as (expected revision, request id, action, parameters).
const CROP_JOURNEY: [(u64, &str, &str, &str); 4] = [
    (
        0,
        "crop-a",
        "crop",
        r#"{"x":0.25,"y":0.25,"width":0.5,"height":0.5}"#,
    ),
    (
        1,
        "crop-fit-b",
        "crop-fit",
        r#"{"aspect":"16:9","angle":4}"#,
    ),
    (2, "crop-reset-c", "crop-reset", r#"{}"#),
    (
        3,
        "crop-d",
        "crop",
        r#"{"angle":3.0,"x":0.1,"y":0.1,"width":0.6,"height":0.6}"#,
    ),
];

/// A rectangle whose corners leave the rotated source: the coverage error both paths must report.
const UNCOVERED: &str = r#"{"angle":45.0,"x":0.0,"y":0.0,"width":1.0,"height":1.0}"#;

fn parameters(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap()
}

#[test]
fn crop_actions_are_discoverable_and_identical_through_actions_and_the_api() {
    let dir = temp("crop-parity");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture("orientation-1.jpg"), &source).unwrap();
    let bytes = std::fs::read(&source).unwrap();

    // Through the core's one action path.
    let mut service = EditorService::open(&dir.join("actions.sqlite")).unwrap();
    let asset = service.import(&source).unwrap().asset.id;
    for (revision, request, action, raw) in CROP_JOURNEY {
        let result = service
            .apply_action(
                &asset,
                mutation(revision, request, "crop-parity"),
                action,
                parameters(raw),
            )
            .unwrap_or_else(|error| panic!("{action}: {error}"));
        assert_eq!(result.outcome, MutationOutcome::Applied, "{action}");
    }
    let direct_entries: Vec<Value> =
        serde_json::to_value(service.history(&asset, None, 50).unwrap()).unwrap()["entries"]
            .as_array()
            .unwrap()
            .clone();
    let direct_stacks = stacks(&direct_entries);
    let direct_layers = service
        .state(&asset)
        .unwrap()
        .current_entry
        .snapshot
        .recipe
        .layers;
    assert_eq!(direct_layers.len(), 1, "the stack holds one crop layer");
    let direct_error = service
        .apply_action(
            &asset,
            mutation(4, "crop-uncovered", "crop-parity"),
            "crop",
            parameters(UNCOVERED),
        )
        .expect_err("the box corners are empty at 45 degrees");
    assert_eq!(direct_error.kind, ErrorKind::Validation);
    drop(service);

    // Through the JSON API, including discovery.
    let (owner, join) = OwnerHandle::start(&dir.join("api.sqlite")).unwrap();
    let client = owner.register();
    let call = |method: &str, params: Value| -> Value {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .unwrap();
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.unwrap()
    };
    let schema = call("schema.list", json!({}));
    let methods = schema["methods"].as_object().unwrap();
    // The published contract says which stage a pixel edit's coordinates address.
    for name in ["x", "y"] {
        let coordinate = methods["edit.set-pixel"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == json!(name))
            .expect("a declared coordinate")
            .clone();
        assert!(
            coordinate["notes"]
                .as_str()
                .unwrap()
                .contains("content stage"),
            "{coordinate}"
        );
    }
    assert!(
        methods["edit.set-pixel"]["notes"]
            .as_str()
            .unwrap()
            .contains("content stage"),
        "the action notes describe the content stage"
    );
    assert!(
        schema["coordinate_space"]
            .as_str()
            .unwrap()
            .contains("content stage"),
        "the coordinate note describes the content stage"
    );
    for method in ["edit.crop", "edit.crop-fit", "edit.crop-reset"] {
        assert!(methods.contains_key(method), "{method} is not listed");
        assert!(methods[method]["mutates"].as_bool().unwrap(), "{method}");
    }
    assert_eq!(
        methods["edit.crop"]["required"],
        json!(["asset_id", "mutation", "x", "y", "width", "height"]),
        "the angle carries a declared default, so it is optional"
    );
    let described: Vec<Value> = methods["edit.crop"]["parameters"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(described.len(), 5);
    for parameter in &described {
        assert_eq!(parameter["kind"], json!("number"), "{parameter}");
        assert!(
            parameter["min"].is_number() && parameter["max"].is_number(),
            "{parameter}"
        );
    }
    assert_eq!(
        methods["edit.crop-fit"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == json!("aspect"))
            .expect("the aspect parameter")["options"],
        json!(["free", "original", "1:1", "3:2", "4:3", "16:9", "custom"])
    );
    assert!(
        methods["edit.crop-reset"]["parameters"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let modules = call("module.list", json!({}));
    let crop = modules["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == json!("lightwell.crop"))
        .expect("the crop module descriptor")
        .clone();
    assert_eq!(crop["title"], json!("Crop and straighten"));
    assert_eq!(crop["effects"][0]["id"], json!("lightwell.geometry.crop"));
    assert_eq!(crop["effects"][0]["stage"], json!("geometry"));
    assert_eq!(
        crop["canvas"],
        json!({
            "kind": "crop-frame",
            "action": "crop",
            "angle": "angle",
            "x": "x",
            "y": "y",
            "width": "width",
            "height": "height",
            "fit_action": "crop-fit",
            "aspect": "aspect",
            "title": "Crop",
            "shortcut": "R",
        })
    );

    let asset = call("catalog.import", json!({"path": source}))["asset"]["id"].clone();
    for (revision, request, action, raw) in CROP_JOURNEY {
        let mut params = parameters(raw);
        let object = params.as_object_mut().unwrap();
        object.insert("asset_id".into(), asset.clone());
        object.insert(
            "mutation".into(),
            json!({"expected_revision":revision,"request_id":request,"actor":"crop-parity"}),
        );
        call(&format!("edit.{action}"), params);
    }
    let api_entries = call("history.list", json!({"asset_id":asset,"limit":50}))["entries"]
        .as_array()
        .unwrap()
        .clone();
    let mut uncovered = parameters(UNCOVERED);
    let object = uncovered.as_object_mut().unwrap();
    object.insert("asset_id".into(), asset.clone());
    object.insert(
        "mutation".into(),
        json!({"expected_revision":4,"request_id":"crop-uncovered","actor":"crop-parity"}),
    );
    let refused = owner
        .call(
            client,
            ApiRequest {
                id: "crop-uncovered".into(),
                method: "edit.crop".into(),
                params: uncovered,
                token: None,
            },
        )
        .unwrap()
        .error
        .expect("an uncovered rectangle is refused");
    owner.stop();
    join.join().unwrap();

    assert_eq!(
        stacks(&api_entries),
        direct_stacks,
        "the API and the action path build the same stacks and history parameters"
    );
    assert_eq!(
        json!({"code": refused.code, "message": refused.message}),
        json!({"code": direct_error.kind.code(), "message": direct_error.detail}),
        "the coverage rejection is the same error everywhere"
    );
    assert_eq!(refused.code, "validation");
    assert!(
        refused.message.contains("corner maps to")
            && refused.message.contains("outside the 480x320 input stage"),
        "{}",
        refused.message
    );
    assert_eq!(std::fs::read(&source).unwrap(), bytes, "source unchanged");
    std::fs::remove_dir_all(dir).unwrap();
}
