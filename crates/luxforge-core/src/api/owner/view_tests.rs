//! `view.set` through the catalog owner: the schema states what its pan is measured in and from,
//! the session it answers holds the pan exactly as stored, equivalent requests leave identical
//! views, and each client's view is its own and edits nothing.
use super::*;
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};

fn ok(owner: &OwnerHandle, client: ClientId, id: &str, method: &str, params: Value) -> Value {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: id.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .expect("the owner answered");
    assert!(response.error.is_none(), "{id}: {:?}", response.error);
    response.result.expect("a result")
}

#[test]
fn view_set_describes_its_pan_and_keeps_it_per_client_without_editing() {
    let dir = temp_dir("view-set").canonicalize().unwrap();
    let (owner, join) = OwnerHandle::start_with_host(
        &dir.join("catalog.sqlite"),
        Arc::new(ModuleRegistry::builtin()),
        HostConfig {
            preferences_dir: Some(dir.join("config")),
            ..HostConfig::unconfigured()
        },
    )
    .unwrap();
    let viewer = owner.register();
    let other = owner.register();

    // Discovery says what the pan is measured in and from.
    let schema = ok(&owner, viewer, "schema", "schema.list", json!({}));
    let listed = &schema["methods"]["view.set"];
    for (field, edge) in [("pan_x", "left edge"), ("pan_y", "top edge")] {
        let described = listed["optional"][field].as_str().expect("an optional pan");
        assert!(described.contains("logical pixels"), "{described}");
        assert!(described.contains(edge), "{described}");
    }
    let notes = listed["notes"].as_str().expect("notes");
    for phrase in [
        "top-left corner from the viewport's top-left",
        "percent / 100 / display scale",
        "physical pixels per logical pixel",
        "clamped to 0..max(0, stage extent × that scale − viewport extent)",
        "centres an axis the stage fits",
        "ignores it at Fit",
        "preview.view holds the pan as stored",
    ] {
        assert!(notes.contains(phrase), "the notes say {phrase:?}: {notes}");
    }

    let photo = dir.join("photo.jpg");
    std::fs::copy(fixture(), &photo).unwrap();
    let opened = super::library::opening::open(&owner, viewer, &photo);
    let asset = opened["asset"]["id"].clone();
    let state = || {
        ok(
            &owner,
            other,
            "state",
            "asset.state",
            json!({"asset_id": asset}),
        )
    };
    let before = state();
    let session = ok(&owner, viewer, "session", "session.state", json!({}));
    let generation = session["preview"]["generation"].clone();

    // The answer holds the pan exactly as asked, in the existing session response.
    let asked =
        json!({"zoom": {"mode": "percent", "value": 400.0}, "pan_x": 5551.5, "pan_y": 3585.25});
    let answered = ok(&owner, viewer, "pan", "view.set", asked.clone());
    let expected =
        json!({"zoom": {"mode": "percent", "value": 400.0}, "pan_x": 5551.5, "pan_y": 3585.25});
    assert_eq!(answered["preview"]["view"], expected);
    assert_eq!(
        ok(&owner, viewer, "read", "session.state", json!({}))["preview"]["view"],
        expected
    );
    // The same request again, and the pan alone, leave the identical view.
    assert_eq!(
        ok(&owner, viewer, "again", "view.set", asked)["preview"]["view"],
        expected
    );
    assert_eq!(
        ok(
            &owner,
            viewer,
            "pan-only",
            "view.set",
            json!({"pan_x": 5551.5, "pan_y": 3585.25})
        )["preview"]["view"],
        expected
    );
    // One axis keeps the other's stored pan.
    assert_eq!(
        ok(&owner, viewer, "y", "view.set", json!({"pan_y": 10.0}))["preview"]["view"],
        json!({"zoom": {"mode": "percent", "value": 400.0}, "pan_x": 5551.5, "pan_y": 10.0})
    );

    // Each client's view is its own, in both directions.
    let fit = json!({"zoom": {"mode": "fit"}, "pan_x": 0.0, "pan_y": 0.0});
    assert_eq!(
        ok(&owner, other, "other", "session.state", json!({}))["preview"]["view"],
        fit
    );
    ok(
        &owner,
        other,
        "other-pan",
        "view.set",
        json!({"pan_x": 1.0, "pan_y": 2.0}),
    );
    assert_eq!(
        ok(&owner, viewer, "mine", "session.state", json!({}))["preview"]["view"]["pan_x"],
        json!(5551.5)
    );

    // No recipe, history or preview content changed.
    let session = ok(&owner, viewer, "after", "session.state", json!({}));
    assert_eq!(session["preview"]["generation"], generation);
    assert_eq!(state(), before);

    owner.stop();
    join.join().unwrap();
    std::fs::remove_dir_all(&dir).ok();
}
