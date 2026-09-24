//! The developer proof module is deliberately registered only in these tests. Its descriptor,
//! method table, persistence and identity render are exercised through the same public paths as
//! an independent JSON client.
use lightwell_core::{
    ApiRequest, CONTROLS_EFFECT, ClientId, ControlsModule, EFFECT_FORMAT, Layer, LayerId,
    ModuleRegistry, OwnerHandle, RECIPE_FORMAT, Recipe, SnapshotId, SourceImage, render,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

fn call(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Value {
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
        .expect("owner answered");
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.expect("result")
}

fn error(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> String {
    owner
        .call(
            client,
            ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .expect("owner answered")
        .error
        .expect("error")
        .code
}

fn import_asset(owner: &OwnerHandle, client: ClientId) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
    let job = call(owner, client, "catalog.import", json!({"path":path}))["job_id"].clone();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = call(owner, client, "job.status", json!({"job_id":job}));
        match status["state"].as_str() {
            Some("ready") => break,
            Some("queued" | "preparing") => {
                assert!(Instant::now() < deadline, "import timed out: {status}");
                std::thread::sleep(Duration::from_millis(1));
            }
            other => panic!("import failed: {other:?}: {status}"),
        }
    }
    call(owner, client, "job.adopt", json!({"job_id":job}))["asset"]["asset"]["id"].clone()
}

#[test]
fn proof_is_opt_in_and_each_control_field_has_an_independent_json_action() {
    assert!(
        ModuleRegistry::builtin()
            .descriptors()
            .iter()
            .all(|d| d.id != "lightwell.controls")
    );
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(Arc::new(ControlsModule::new()))
        .expect("proof module registers");
    let catalog = std::env::temp_dir().join(format!(
        "lightwell-controls-proof-{}.sqlite",
        std::process::id()
    ));
    let _ = fs::remove_file(&catalog);
    let (owner, join) = OwnerHandle::start_with(&catalog, Arc::new(registry)).expect("owner");
    let client = owner.register();
    let modules = call(&owner, client, "module.list", json!({}));
    let proof = modules["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == "lightwell.controls")
        .expect("proof descriptor");
    assert_eq!(proof["developer"], true);
    assert_eq!(
        proof["reset"],
        json!({"action":"reset-controls","preset":{}})
    );
    assert_eq!(proof["controls"][0]["reset"], proof["reset"]);

    let schema = call(&owner, client, "schema.list", json!({}));
    let set = &schema["methods"]["edit.set-controls"];
    assert_eq!(set["patch"], true);
    assert_eq!(set["parameters"].as_array().unwrap().len(), 11);
    assert_eq!(
        schema["methods"]["edit.reset-controls"]["parameters"],
        json!([])
    );
    assert_eq!(schema["methods"]["edit.reset-controls"]["patch"], false);
    assert_eq!(
        schema["methods"]["query.sample-controls-curve"]["mutates"],
        false
    );

    let asset = import_asset(&owner, client);
    let fields = [
        ("amount", json!(2.5)),
        ("coordinate", json!(72.0)),
        ("count", json!(3)),
        ("enabled", json!(true)),
        ("mode", json!("two")),
        ("mode-chips", json!("four")),
        ("mode-menu", json!("three")),
        ("rgb", json!([20, 40, 60])),
        ("rgb-fields", json!([1, 2, 3])),
        ("master", json!([[0.0, 0.0], [0.4, 0.8], [1.0, 1.0]])),
        ("red", json!([[0.0, 1.0], [0.4, 0.2], [1.0, 0.0]])),
    ];
    for (index, (name, value)) in fields.iter().enumerate() {
        let mut request = json!({
            "asset_id":asset,
            "mutation":{"expected_revision":index,"request_id":format!("field-{index}"),"actor":"independent-json-client"}
        });
        request[name] = value.clone();
        let result = call(&owner, client, "edit.set-controls", request);
        assert_eq!(result["outcome"], "applied", "{name}");
        assert_eq!(result["revision"], index + 1, "{name}");
        let state = call(&owner, client, "asset.state", json!({"asset_id":asset}));
        assert_eq!(state["revision"], index + 1, "{name} persisted");
        let described = call(&owner, client, "recipe.describe", json!({"asset_id":asset}));
        let layer = described["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|layer| layer["effect"] == CONTROLS_EFFECT)
            .expect("proof layer");
        assert_eq!(
            layer["values"][name], *value,
            "{name} has its authoritative value"
        );
    }
    // A patch cannot silently apply two fields or an empty request.
    for (suffix, extra) in [
        ("two", json!({"amount":1.0,"count":2})),
        ("empty", json!({})),
    ] {
        let mut request = json!({"asset_id":asset,"mutation":{
            "expected_revision":fields.len(),"request_id":suffix,"actor":"independent-json-client"
        }});
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            error(&owner, client, "edit.set-controls", request),
            "validation"
        );
    }
    let sampled = call(
        &owner,
        client,
        "query.sample-controls-curve",
        json!({
            "asset_id":asset,"master":[[0.0,0.0],[0.5,1.0],[1.0,1.0]]
        }),
    );
    let points = sampled["points"].as_array().expect("sample points");
    assert_eq!(points.len(), 257);
    assert_eq!(points[0], json!([0.0, 0.0]));
    assert_eq!(points[128], json!([0.5, 1.0]));
    assert_eq!(points[256], json!([1.0, 1.0]));
    assert_eq!(
        error(
            &owner,
            client,
            "query.sample-controls-curve",
            json!({
                "asset_id":asset,"master":[[0.0,0.0],[1.0,1.0]],"red":[[0.0,0.0],[1.0,1.0]]
            })
        ),
        "validation"
    );
    let reset = call(
        &owner,
        client,
        "edit.reset-controls",
        json!({
            "asset_id":asset,"mutation":{"expected_revision":fields.len(),"request_id":"reset","actor":"independent-json-client"}
        }),
    );
    assert_eq!(reset["outcome"], "applied");
    assert_eq!(reset["revision"], fields.len() + 1);
    let reset_recipe = call(&owner, client, "recipe.describe", json!({"asset_id":asset}));
    let reset_layer = reset_recipe["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|layer| layer["effect"] == CONTROLS_EFFECT)
        .expect("neutral proof layer");
    let declared = set["parameters"].as_array().unwrap();
    for parameter in declared {
        let name = parameter["name"].as_str().unwrap();
        assert_eq!(
            reset_layer["values"][name], parameter["default"],
            "{name} reset"
        );
    }
    owner.stop();
    join.join().expect("owner joined");
    fs::remove_file(catalog).expect("catalog removed");
}

#[test]
fn proof_layer_is_byte_exact_and_shares_the_source_allocation() {
    let mut registry = ModuleRegistry::builtin();
    registry.register(Arc::new(ControlsModule::new())).unwrap();
    let source = SourceImage {
        width: 2,
        height: 1,
        rgba: vec![11, 22, 33, 255, 240, 80, 16, 255].into(),
        fingerprint: "sha256:controls-fixture".into(),
        orientation: 1,
    };
    let layer = Layer {
        id: LayerId::new(),
        effect_id: CONTROLS_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"amount":2.5,"rgb":[20,40,60],"master":[[0.0,0.0],[0.4,0.8],[1.0,1.0]]}),
        mask: None,
        artifacts: Vec::new(),
    };
    let raster = render(
        &registry,
        &source,
        SnapshotId::new(),
        &Recipe {
            format: RECIPE_FORMAT,
            layers: vec![layer],
            masks: Vec::new(),
            ..Recipe::default()
        },
    )
    .expect("identity controls render");
    assert_eq!(raster.rgba.as_ref(), source.rgba.as_ref());
    assert!(Arc::ptr_eq(&raster.rgba, &source.rgba));
}
