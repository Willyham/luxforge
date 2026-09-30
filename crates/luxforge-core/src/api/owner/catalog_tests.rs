//! A catalog of several photos through the catalog owner: one client's selection per asset and the
//! paged `catalog.list`. Relocating an original is `source.locate` (`library/locate_tests.rs`).
use super::*;
use crate::{
    api::ApiFailure,
    editor::{mutation, mutation_json},
};
use std::fs;

static NEXT: AtomicU64 = AtomicU64::new(1);

fn directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "luxforge-owner-catalog-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).unwrap();
    directory.canonicalize().unwrap()
}

/// A copy of the fixture at `name` in `directory`: a distinct file, so a distinct asset.
fn copy(directory: &Path, name: &str) -> PathBuf {
    let path = directory.join(name);
    fs::copy(luxforge_testbase::paths::jpeg(), &path).unwrap();
    path
}

fn call(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> ApiResponse {
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
        .expect("the owner answered")
}

fn ok(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Value {
    let response = call(owner, client, method, params);
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.expect("a result")
}

fn failure(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> ApiFailure {
    call(owner, client, method, params)
        .error
        .unwrap_or_else(|| panic!("{method} was expected to fail"))
}

/// Retry a request through every preparation job it asks for, as a client does.
fn prepared(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Value {
    for _ in 0..4 {
        let response = call(owner, client, method, params.clone());
        match response.error {
            None => return response.result.expect("a result"),
            Some(error) if error.code == "preparation-required" => {
                let job = error.job_id.expect("a preparation names its job");
                let status = luxforge_testbase::wait_for("the source job to settle", || {
                    let read = ok(owner, client, "job.read", json!({"job_id": job}));
                    (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
                });
                assert_eq!(status["status"], "ready", "{status}");
            }
            Some(error) => panic!("{method}: {error:?}"),
        }
    }
    panic!("{method} kept asking for preparation");
}

fn revision(owner: &OwnerHandle, client: ClientId, asset: &AssetId) -> u64 {
    ok(owner, client, "asset.state", json!({"asset_id": asset}))["revision"]
        .as_u64()
        .unwrap()
}

fn current(owner: &OwnerHandle, client: ClientId, asset: &AssetId) -> Value {
    ok(owner, client, "asset.state", json!({"asset_id": asset}))["current_entry"]["id"].clone()
}

/// A Basic edit of `asset` at its revision now, as one request.
fn edit(owner: &OwnerHandle, client: ClientId, asset: &AssetId, request: &str) -> ApiResponse {
    let revision = revision(owner, client, asset);
    call(
        owner,
        client,
        "edit.set-basic",
        json!({
            "asset_id": asset,
            "mutation": mutation_json(revision, request),
            "contrast": 10,
        }),
    )
}

/// The entry `render.sample` answered for `asset` at the session's selection.
fn sampled_entry(owner: &OwnerHandle, client: ClientId, asset: &AssetId) -> Value {
    prepared(
        owner,
        client,
        "render.sample",
        json!({"asset_id": asset, "x": 0, "y": 0}),
    )["entry_id"]
        .clone()
}

/// The Basic exposure `preset.capture` reads from `asset` at the session's selection.
fn exposure(owner: &OwnerHandle, client: ClientId, asset: &AssetId) -> Value {
    ok(
        owner,
        client,
        "preset.capture",
        json!({"asset_id": asset, "fields": {"set-basic": ["exposure"]}}),
    )["settings"]["set-basic"]["exposure"]
        .clone()
}

/// The assets the session holds a historical selection of, and the entry each selects.
fn selections(session: &Value) -> Value {
    let selections = session["preview"]["selections"].as_object().unwrap();
    Value::Object(
        selections
            .iter()
            .map(|(asset, selected)| (asset.clone(), selected["entry_id"].clone()))
            .collect(),
    )
}

/// A client previewing a historical entry of one photo edits another freely, each photo's
/// questions are answered against its own selection, and two photos' selections are independent.
#[test]
fn a_selection_is_per_asset_and_never_pauses_or_answers_for_another() {
    let directory = directory("two-assets");
    let catalog = directory.join("catalog.sqlite");
    let (a, b, a_original, b_original) = {
        let mut service = EditorService::open(&catalog).unwrap();
        let a = service.import(&copy(&directory, "a.jpg")).unwrap();
        let b = service.import(&copy(&directory, "b.jpg")).unwrap();
        for (state, request, exposure) in [(&a, "a-basic", -1.0), (&b, "b-basic", 0.5)] {
            service
                .run_action(
                    &state.asset.id,
                    mutation(0, request),
                    "set-basic",
                    json!({"exposure": exposure}),
                )
                .unwrap();
        }
        (
            a.asset.id,
            b.asset.id,
            a.current_entry.id,
            b.current_entry.id,
        )
    };
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();

    // Previewing A's Original leaves B at current.
    let selected = ok(
        &owner,
        client,
        "preview.select",
        json!({"asset_id": a, "entry_id": a_original}),
    );
    assert_eq!(
        selections(&selected["session"]),
        json!({a.as_str(): a_original})
    );
    // B is edited while A is previewed; A's own edits are refused.
    let edited = edit(&owner, client, &b, "b-edit");
    assert!(edited.error.is_none(), "{:?}", edited.error);
    assert_eq!(
        edit(&owner, client, &a, "a-refused").error.unwrap().code,
        "conflict"
    );
    // Each asset's questions are answered against its own selection: A's previewed Original, B's
    // current entry, never the other's.
    assert_eq!(sampled_entry(&owner, client, &a), json!(a_original));
    assert_eq!(
        sampled_entry(&owner, client, &b),
        current(&owner, client, &b)
    );
    ok(
        &owner,
        client,
        "render.locate",
        json!({"asset_id": b, "x": 0, "y": 0}),
    );
    assert_eq!(
        exposure(&owner, client, &b),
        json!(0.5),
        "B's current entry"
    );
    assert_eq!(exposure(&owner, client, &a), json!(0.0), "A's Original");

    // B's Original too: two independent selections, and now B's edits are refused.
    let both = ok(
        &owner,
        client,
        "preview.select",
        json!({"asset_id": b, "entry_id": b_original}),
    );
    assert_eq!(
        selections(&both["session"]),
        json!({a.as_str(): a_original, b.as_str(): b_original})
    );
    assert_eq!(
        edit(&owner, client, &b, "b-refused").error.unwrap().code,
        "conflict"
    );
    assert_eq!(sampled_entry(&owner, client, &b), json!(b_original));
    assert_eq!(sampled_entry(&owner, client, &a), json!(a_original));
    assert_eq!(exposure(&owner, client, &b), json!(0.0), "B's Original");

    // Selecting A's current entry returns A alone.
    let a_current = current(&owner, client, &a);
    let returned = ok(
        &owner,
        client,
        "preview.select",
        json!({"asset_id": a, "entry_id": a_current}),
    );
    assert_eq!(
        selections(&returned["session"]),
        json!({b.as_str(): b_original})
    );
    let edited = edit(&owner, client, &a, "a-edit");
    assert!(edited.error.is_none(), "{:?}", edited.error);

    // Restoring B's selection returns B to current and leaves A's selection alone.
    ok(
        &owner,
        client,
        "preview.select",
        json!({"asset_id": a, "entry_id": a_original}),
    );
    let b_revision = revision(&owner, client, &b);
    ok(
        &owner,
        client,
        "history.restore",
        json!({"asset_id": b, "entry_id": b_original, "mutation": mutation_json(b_revision, "b-restore")}),
    );
    let session = ok(&owner, client, "session.state", json!({}));
    assert_eq!(selections(&session), json!({a.as_str(): a_original}));

    // Another client's selections are its own.
    let other = owner.register();
    let edited = edit(&owner, other, &a, "a-other");
    assert!(edited.error.is_none(), "{:?}", edited.error);

    // Return to current returns every asset.
    let cleared = ok(&owner, client, "preview.return-current", json!({}));
    assert_eq!(selections(&cleared["session"]), json!({}));
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

/// `catalog.list` pages the catalog in import order with a cursor, refuses a limit outside its
/// typed range, and lists a row whose stored interpretation this build cannot decode, because a
/// summary row reads the asset's own columns and decodes no interpretation.
#[test]
fn catalog_list_pages_summary_rows_without_decoding_an_interpretation() {
    let directory = directory("paging");
    let catalog = directory.join("catalog.sqlite");
    let imported: Vec<EditorState> = {
        let mut service = EditorService::open(&catalog).unwrap();
        (0..5)
            .map(|index| {
                service
                    .import(&copy(&directory, &format!("{index}.jpg")))
                    .unwrap()
            })
            .collect()
    };
    // The third asset's interpretation is text no build can decode; its own columns are intact.
    let broken = imported[2].asset.id.clone();
    rusqlite::Connection::open(&catalog)
        .unwrap()
        .execute(
            "UPDATE assets SET source_json='{not an interpretation' WHERE id=?1",
            [broken.as_str()],
        )
        .unwrap();
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();

    // A walk of pages of two: two, two, then one with no cursor after it.
    let mut listed = Vec::new();
    let mut after = Value::Null;
    let mut pages = Vec::new();
    loop {
        let mut params = json!({"limit": 2});
        if !after.is_null() {
            params["after"] = after.clone();
        }
        let page = ok(&owner, client, "catalog.list", params);
        let assets = page["assets"].as_array().unwrap().clone();
        pages.push(assets.len());
        listed.extend(assets.iter().cloned());
        after = page["next"].clone();
        if after.is_null() {
            break;
        }
        assert_eq!(after, assets.last().unwrap()["id"], "next is the last row");
    }
    assert_eq!(pages, [2, 2, 1]);
    let expected: Vec<Value> = imported
        .iter()
        .map(|state| {
            json!({
                "id": state.asset.id,
                "locator": state.asset.locator,
                "kind": "jpeg",
                "width": state.asset.width,
                "height": state.asset.height,
            })
        })
        .collect();
    assert_eq!(listed, expected, "summary rows in import order");
    // The default page holds them all.
    let whole = ok(&owner, client, "catalog.list", json!({}));
    assert_eq!(whole["assets"], json!(expected));
    assert_eq!(whole["next"], Value::Null);
    // The broken row listed above; reading the asset itself refuses its interpretation by name.
    assert_eq!(
        failure(&owner, client, "asset.state", json!({"asset_id": broken})).code,
        "incompatible"
    );
    // The typed limit refuses what is out of range before the handler runs, and a cursor must
    // name an asset.
    for limit in [0, 501] {
        let refused = failure(&owner, client, "catalog.list", json!({"limit": limit}));
        assert_eq!(refused.code, "validation", "{limit}: {}", refused.message);
    }
    assert_eq!(
        failure(
            &owner,
            client,
            "catalog.list",
            json!({"after": AssetId::new()})
        )
        .code,
        "validation"
    );
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}
