use luxforge_core::{BASIC_EFFECT, OwnerHandle, auto_tone::FIELDS};
use luxforge_testbase::paths;
use luxforge_testkit::client::{call, mutation, open, refused};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

fn photo(label: &str, uniform: bool) -> PathBuf {
    let path = paths::temp_path(&format!("auto-tone-{label}.jpg"));
    let image = image::RgbImage::from_fn(96, 64, |x, y| {
        let code = if uniform {
            70
        } else {
            (12 + x * 150 / 95 + y * 20 / 63) as u8
        };
        image::Rgb([code, code, code])
    });
    image::codecs::jpeg::JpegEncoder::new_with_quality(fs::File::create(&path).unwrap(), 100)
        .encode_image(&image)
        .unwrap();
    path
}

fn entry(owner: &OwnerHandle, client: luxforge_core::ClientId, asset: &Value) -> Value {
    let state = call(owner, client, "asset.state", json!({"asset_id":asset})).unwrap();
    call(
        owner,
        client,
        "history.inspect",
        json!({"asset_id":asset,"entry_id":state["current_entry"]["id"]}),
    )
    .unwrap()
}

fn basic(entry: &Value) -> Value {
    entry["snapshot"]["recipe"]["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|layer| layer["effect_id"] == BASIC_EFFECT)
        .map(|layer| layer["payload"].clone())
        .unwrap_or(json!({}))
}

#[test]
fn auto_tone_query_action_repeat_undo_and_source_preservation_through_the_owner() {
    let image = photo("owner", false);
    let before = fs::read(&image).unwrap();
    let catalog = paths::temp_catalog("auto-tone-owner");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let asset = open(&owner, client, &image, "test").unwrap()["asset"]["id"].clone();
    call(&owner, client, "edit.set-basic", json!({"asset_id":asset,"mutation":mutation(0,"wb","test"),"temperature":12,"tint":-4,"exposure":-3,"saturation":80})).unwrap();
    let original = entry(&owner, client, &asset);
    let predicted = call(&owner, client, "query.auto-tone", json!({"asset_id":asset})).unwrap();
    assert_eq!(predicted["algorithm"], "auto-tone/1");
    assert!(serde_json::to_vec(&predicted).unwrap().len() <= 4096);
    let request = json!({"asset_id":asset,"mutation":mutation(1,"auto","test")});
    let applied = call(&owner, client, "edit.auto-tone", request.clone()).unwrap();
    assert_eq!(applied["outcome"], "applied");
    // The action answers the report its values came from, which the query gives too.
    assert_eq!(applied["analysis"]["auto-tone"], predicted);
    let after = entry(&owner, client, &asset);
    assert_eq!(after["label"], "Auto tone");
    let payload = basic(&after);
    for field in FIELDS {
        assert_eq!(
            payload[field].as_f64().unwrap_or(0.),
            predicted["values"][field].as_f64().unwrap(),
            "{field}"
        );
    }
    assert_eq!(payload["temperature"], 12.);
    assert_eq!(payload["tint"], -4.);
    let retried = call(&owner, client, "edit.auto-tone", request).unwrap();
    assert_eq!(retried["deduplicated"], true);
    assert!(
        retried.get("analysis").is_none(),
        "a retry analyses nothing: {retried}"
    );
    let repeat = call(
        &owner,
        client,
        "edit.auto-tone",
        json!({"asset_id":asset,"mutation":mutation(2,"repeat","test")}),
    )
    .unwrap();
    assert_eq!(repeat["outcome"], "no-op");
    assert_eq!(repeat["analysis"]["auto-tone"], predicted);
    let repeated = call(&owner, client, "query.auto-tone", json!({"asset_id":asset})).unwrap();
    assert_eq!(predicted, repeated);
    call(
        &owner,
        client,
        "history.undo",
        json!({"asset_id":asset,"mutation":mutation(2,"undo","test")}),
    )
    .unwrap();
    assert_eq!(basic(&entry(&owner, client, &asset)), basic(&original));
    assert_eq!(fs::read(&image).unwrap(), before);
    owner.disconnect(client);
    owner.stop();
    join.join().unwrap();
    let _ = fs::remove_file(image);
    let _ = fs::remove_file(catalog);
}

#[test]
fn auto_tone_copy_paste_carries_values_without_reanalysing_the_target() {
    let source = photo("copy-source", false);
    let target = photo("paste-target", true);
    let before = [fs::read(&source).unwrap(), fs::read(&target).unwrap()];
    let catalog = paths::temp_catalog("auto-tone-copy-paste");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let source_asset = open(&owner, client, &source, "test").unwrap()["asset"]["id"].clone();
    call(
        &owner,
        client,
        "edit.auto-tone",
        json!({"asset_id":source_asset,"mutation":mutation(0,"auto-source","test")}),
    )
    .unwrap();
    let source_values = basic(&entry(&owner, client, &source_asset));
    let captured = call(
        &owner,
        client,
        "preset.capture",
        json!({"asset_id":source_asset,"fields":{"set-basic":FIELDS}}),
    )
    .unwrap();
    assert!(captured["settings"].get("auto-tone").is_none());
    for field in FIELDS {
        assert_eq!(
            captured["settings"]["set-basic"][field].as_f64().unwrap(),
            source_values[field].as_f64().unwrap_or(0.),
            "{field}"
        );
    }
    let target_asset = open(&owner, client, &target, "test").unwrap()["asset"]["id"].clone();
    assert!(
        call(
            &owner,
            client,
            "query.auto-tone",
            json!({"asset_id":target_asset})
        )
        .is_err()
    );
    let applied = call(
        &owner,
        client,
        "edit.paste-settings",
        json!({"asset_id":target_asset,"settings":captured["settings"],"source":"source.jpg","mutation":mutation(0,"paste-auto-values","test")}),
    )
    .unwrap();
    assert_eq!(applied["outcome"], "applied");
    let pasted = entry(&owner, client, &target_asset);
    assert_eq!(pasted["label"], "Paste settings from source.jpg");
    let target_values = basic(&pasted);
    for field in FIELDS {
        assert_eq!(
            target_values[field].as_f64().unwrap_or(0.),
            source_values[field].as_f64().unwrap_or(0.),
            "{field}"
        );
    }
    assert_eq!(
        [fs::read(&source).unwrap(), fs::read(&target).unwrap()],
        before
    );
    owner.disconnect(client);
    owner.stop();
    join.join().unwrap();
    for path in [source, target, catalog] {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn auto_tone_presets_analyse_after_white_balance_and_skip_uniform_photos() {
    let image = photo("preset", false);
    let uniform = photo("uniform", true);
    let catalog = paths::temp_catalog("auto-tone-preset");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let asset = open(&owner, client, &image, "test").unwrap()["asset"]["id"].clone();
    let settings = json!({"auto-tone":{},"set-basic":{"temperature":15,"tint":3}});
    call(&owner, client, "edit.apply-preset", json!({"asset_id":asset,"mutation":mutation(0,"preset","test"),"name":"Auto","settings":settings})).unwrap();
    let composite = entry(&owner, client, &asset);
    assert_eq!(composite["label"], "Preset: Auto");
    call(
        &owner,
        client,
        "history.undo",
        json!({"asset_id":asset,"mutation":mutation(1,"undo","test")}),
    )
    .unwrap();
    call(
        &owner,
        client,
        "edit.set-basic",
        json!({"asset_id":asset,"mutation":mutation(2,"wb","test"),"temperature":15,"tint":3}),
    )
    .unwrap();
    call(
        &owner,
        client,
        "edit.auto-tone",
        json!({"asset_id":asset,"mutation":mutation(3,"auto","test")}),
    )
    .unwrap();
    assert_eq!(basic(&composite), basic(&entry(&owner, client, &asset)));
    let captured = call(
        &owner,
        client,
        "preset.capture",
        json!({"asset_id":asset,"fields":{"auto-tone":true}}),
    )
    .unwrap();
    assert_eq!(captured["settings"], json!({"auto-tone":{}}));
    let overlapping = refused(&owner, client, "preset.create", json!({"name":"invalid","settings":{"auto-tone":{},"set-basic":{"exposure":0}},"mutation":{"request_id":"invalid","actor":"test"}})).unwrap();
    assert!(overlapping.1.contains("overwrites"), "{overlapping:?}");
    let flat = open(&owner, client, &uniform, "test").unwrap()["asset"]["id"].clone();
    let reason = refused(
        &owner,
        client,
        "edit.auto-tone",
        json!({"asset_id":flat,"mutation":mutation(0,"flat","test")}),
    )
    .unwrap();
    assert!(reason.1.contains("half a stop"), "{reason:?}");
    let skipped = call(&owner, client, "edit.apply-preset", json!({"asset_id":flat,"mutation":mutation(0,"flat-preset","test"),"name":"Auto","settings":settings})).unwrap();
    assert_eq!(skipped["outcome"], "applied");
    assert_eq!(skipped["skipped"][0]["action"], "auto-tone");
    assert_eq!(
        basic(&entry(&owner, client, &flat)),
        json!({"temperature":15.,"tint":3.})
    );
    owner.disconnect(client);
    owner.stop();
    join.join().unwrap();
    for path in [image, uniform, catalog] {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn auto_tone_refuses_mask_history_and_open_draft_without_changing_the_recipe() {
    let image = photo("refusals", false);
    let catalog = paths::temp_catalog("auto-tone-refusals");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let asset = open(&owner, client, &image, "test").unwrap()["asset"]["id"].clone();
    let original = entry(&owner, client, &asset)["id"].clone();
    let created = call(&owner, client, "mask.create-linear", json!({"asset_id":asset,"mutation":mutation(0,"mask","test"),"x0":0.,"y0":0.,"x1":0.,"y1":1.})).unwrap();
    let before = entry(&owner, client, &asset);
    let request = json!({"asset_id":asset,"mutation":mutation(1,"auto","test")});
    let mut masked = request.clone();
    masked["mask"] = created["mask"].clone();
    let reason = refused(&owner, client, "edit.auto-tone", masked).unwrap();
    assert!(reason.1.contains("global Basic"), "{reason:?}");
    call(
        &owner,
        client,
        "preview.select",
        json!({"asset_id":asset,"entry_id":original}),
    )
    .unwrap();
    let reason = refused(&owner, client, "edit.auto-tone", request.clone()).unwrap();
    assert!(
        reason.1.contains("current") || reason.1.contains("histor"),
        "{reason:?}"
    );
    call(
        &owner,
        client,
        "preview.select",
        json!({"asset_id":asset,"entry_id":before["id"]}),
    )
    .unwrap();
    let reason = refused(
        &owner,
        client,
        "draft.begin",
        json!({"asset_id":asset,"action":"auto-tone"}),
    )
    .unwrap();
    assert!(reason.1.contains("cannot be drafted"), "{reason:?}");
    let begun = call(
        &owner,
        client,
        "draft.begin",
        json!({"asset_id":asset,"action":"set-basic"}),
    )
    .unwrap();
    let reason = refused(&owner, client, "edit.auto-tone", request).unwrap();
    assert!(reason.1.contains("draft"), "{reason:?}");
    let reason = refused(&owner, client, "edit.apply-preset", json!({"asset_id":asset,"mutation":mutation(1,"preset","test"),"name":"Auto","settings":{"auto-tone":{}}})).unwrap();
    assert!(reason.1.contains("draft"), "{reason:?}");
    call(
        &owner,
        client,
        "draft.cancel",
        json!({"draft_id":begun["draft_id"]}),
    )
    .unwrap();
    assert_eq!(entry(&owner, client, &asset), before);
    owner.disconnect(client);
    owner.stop();
    join.join().unwrap();
    let _ = fs::remove_file(image);
    let _ = fs::remove_file(catalog);
}

#[test]
#[ignore = "set LUXFORGE_AUTO_TONE_RAW to an available RAW for the intermediate-development integration check"]
fn auto_tone_raw_preset_uses_the_new_white_balance_development() {
    fn prepared(
        owner: &OwnerHandle,
        client: luxforge_core::ClientId,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        for _ in 0..4 {
            let response = owner
                .call(
                    client,
                    luxforge_core::ApiRequest {
                        id: luxforge_testkit::client::request_id(method),
                        method: method.into(),
                        params: params.clone(),
                        token: None,
                    },
                )
                .map_err(|error| error.to_string())?;
            match response.error {
                None => return Ok(response.result.unwrap()),
                Some(error) if error.code == "preparation-required" => {
                    let status = luxforge_testkit::client::settle(
                        owner,
                        client,
                        &json!(error.job_id.unwrap()),
                    )?;
                    assert_eq!(status["status"], "ready", "{status}");
                }
                Some(error) => return Err(format!("{method}: {error:?}")),
            }
        }
        Err(format!("{method}: preparation did not settle"))
    }
    let image = PathBuf::from(std::env::var("LUXFORGE_AUTO_TONE_RAW").unwrap());
    let original = fs::read(&image).unwrap();
    let catalog = paths::temp_catalog("auto-tone-raw-preset");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let asset = open(&owner, client, &image, "test").unwrap()["asset"]["id"].clone();
    let revision =
        prepared(&owner, client, "asset.state", json!({"asset_id":asset})).unwrap()["revision"]
            .as_u64()
            .unwrap();
    let original_prediction =
        prepared(&owner, client, "query.auto-tone", json!({"asset_id":asset})).unwrap();
    eprintln!("RAW_AUTO_REFERENCE {}", original_prediction["values"]);
    prepared(&owner, client, "edit.apply-preset", json!({"asset_id":asset,"mutation":mutation(revision,"preset","test"),"name":"WB and Auto","settings":{"auto-tone":{},"set-raw":{"temperature":6000,"tint":7}}})).unwrap();
    let composite = entry(&owner, client, &asset);
    prepared(
        &owner,
        client,
        "history.undo",
        json!({"asset_id":asset,"mutation":mutation(revision + 1,"undo","test")}),
    )
    .unwrap();
    prepared(
        &owner,
        client,
        "edit.set-raw",
        json!({"asset_id":asset,"mutation":mutation(revision + 2,"wb","test"),"temperature":6000,"tint":7}),
    )
    .unwrap();
    let white_balanced = entry(&owner, client, &asset);
    let report = prepared(&owner, client, "query.auto-tone", json!({"asset_id":asset})).unwrap();
    assert_eq!(report["source_clip_detection"], "raw-sensor-white");
    eprintln!("RAW_AUTO_SAMPLE {}", report["sample"]);
    prepared(
        &owner,
        client,
        "edit.auto-tone",
        json!({"asset_id":asset,"mutation":mutation(revision + 3,"auto","test")}),
    )
    .unwrap();
    let sequential = entry(&owner, client, &asset);
    assert_eq!(basic(&composite), basic(&sequential));
    let other_layers = |entry: &Value| {
        entry["snapshot"]["recipe"]["layers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|layer| layer["effect_id"] != BASIC_EFFECT)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(other_layers(&white_balanced), other_layers(&sequential));
    for field in FIELDS {
        assert_eq!(
            basic(&sequential)[field].as_f64().unwrap_or(0.),
            report["values"][field].as_f64().unwrap()
        );
    }
    assert_eq!(fs::read(&image).unwrap(), original);
    owner.disconnect(client);
    owner.stop();
    join.join().unwrap();
    let _ = fs::remove_file(catalog);
}
