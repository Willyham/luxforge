//! HDR stacks in browsing and the development set, against a real catalog, index and owner: a
//! merge's result shown once over its collapsed originals in folder, event and catalog views and
//! across an index rebuilt with other rows; filters acting on each item's own metadata, Show all
//! labelling what they leave out; a result in the set Picked, deciding its bracket and selected
//! alone; offline and unlisted originals as provenance rows with their reasons; the set's revisions
//! and conflicts across clients; and Remove, Put back, undo and redo of a merge leaving every other
//! member and pick as they were, a replaced set refusing the publication whole. Each expectation is
//! written out from the seeded rows, never read back from what is tested, and the originals' bytes
//! are checked unchanged.
use super::hdr_merge_lifecycle::{Bench, request};
use super::*;
use crate::{
    ApiRequest, ErrorKind, OwnerHandle, SourceTag,
    api::ClientId,
    catalog_types::{
        CameraBody, CaptureTime, Dimensions, Exposure, FileRecord, HeaderState, IndexRoot,
        RootKind, VolumeId,
    },
    editor::distinct_jpeg,
    index::volume_of,
    library::{development_set, journal},
    merge::hdr::DeghostMode,
    seed::IndexSeeder,
};
use luxforge_testbase::paths::jpeg;
use serde_json::{Value, json};
use std::{fs, thread::JoinHandle};

/// The bracket's capture seconds after 09:00:00, by file: three frames a second apart, then two
/// singles an hour and two hours later.
const SECONDS: [u32; 5] = [0, 1, 2, 3600, 7200];

fn capture(second: u32) -> CaptureTime {
    CaptureTime::from_exif(
        &format!(
            "2026:09:12 {:02}:{:02}:{:02}",
            9 + second / 3600,
            second % 3600 / 60,
            second % 60
        ),
        None,
        Some("+02:00"),
    )
    .unwrap()
}

fn header(at: usize) -> HeaderMetadata {
    HeaderMetadata {
        capture: Some(capture(SECONDS[at])),
        camera: Some(CameraBody {
            make: "NIKON CORPORATION".into(),
            model: "NIKON Z 6".into(),
            serial: None,
        }),
        exposure: Exposure {
            iso: Some(100),
            f_number: Some(8.0),
            bias_ev: Some(at as f32 - 1.0),
            ..Exposure::default()
        },
        dimensions: Some(Dimensions {
            width: 48,
            height: 32,
        }),
        ..HeaderMetadata::default()
    }
}

/// A catalog beside a bracket folder of five files the index lists, the second developed, and an
/// ordinary photograph from another folder.
struct Scene {
    bench: Bench,
    other: AssetId,
    volume: VolumeId,
}

/// The owner serving the scene's catalog.
struct Live {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}

impl Scene {
    fn new(name: &str) -> Self {
        let mut bench = Bench::new(&format!("browse-{name}"), 5);
        let other = distinct_jpeg(&jpeg(), &bench.directory.join("other").join("IMG_0001.JPG"));
        let other = bench.service.import(&other).unwrap().asset.id;
        let volume = volume_of(&bench.files[0], 1).unwrap().id;
        let scene = Self {
            bench,
            other,
            volume,
        };
        scene.seed_index(false);
        scene
    }

    fn bracket(&self) -> PathBuf {
        self.bench.files[0].parent().unwrap().to_path_buf()
    }

    /// List the bracket folder's files in a new index, in reverse when `reversed`, so a rebuilt
    /// index gives every file another row.
    fn seed_index(&self, reversed: bool) {
        let dir = crate::index::index_dir(&self.bench.catalog);
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }
        let mut seeder =
            IndexSeeder::create(&self.bench.catalog, self.bench.service.catalog_id()).unwrap();
        seeder
            .roots(&[IndexRoot {
                path: self.bracket(),
                kind: RootKind::Indexed,
                volume_id: self.volume.clone(),
                listed_ms: Some(1),
                file_count: None,
                offline: false,
            }])
            .unwrap();
        let mut records: Vec<FileRecord> = self
            .bench
            .files
            .iter()
            .enumerate()
            .map(|(at, path)| FileRecord {
                path: path.clone(),
                folder: self.bracket(),
                name: path.file_name().unwrap().to_string_lossy().into_owned(),
                volume_id: self.volume.clone(),
                signature: FileSignature::of(&path.metadata().unwrap()),
                born_ns: None,
                kind: SourceTag::Jpeg,
                header: HeaderState::Ok(Box::new(header(at))),
                last_seen_ms: 1,
            })
            .collect();
        if reversed {
            records.reverse();
        }
        seeder.files(&records).unwrap();
        seeder.finish().unwrap();
    }

    /// Publish a merge of the bracket's files `inputs` as `actor`, its result dated by its
    /// reference, with `extra` in its change.
    fn publish_with(
        &mut self,
        actor: &str,
        request_id: &str,
        inputs: &[usize],
        extra: impl FnOnce(
            &Transaction<'_>,
            &MergeId,
            &AssetId,
        ) -> Result<Vec<(LibraryItem, Desired)>, Error>,
    ) -> Result<PublishedMerge, Error> {
        let provenance = self.bench.provenance(inputs, DeghostMode::Auto);
        let source = self.bench.source(inputs.len() as u32);
        let folder = self.bench.folder();
        let reference = provenance.reference().path.clone();
        let at = self
            .bench
            .files
            .iter()
            .position(|file| *file == reference)
            .unwrap();
        let volume = volume_of(&reference, 1).unwrap();
        let name = format!("{} HDR", reference.file_stem().unwrap().to_string_lossy());
        self.bench.service.publish_merge(
            request("merge.create", actor, request_id),
            &Publication {
                provenance: &provenance,
                source: &source,
                catalog_folder_id: &folder,
                file_name: &name,
                header: &header(at),
                place: None,
                volumes: std::slice::from_ref(&volume),
                replaces: None,
            },
            extra,
        )
    }

    /// Import merged: no set membership.
    fn import(&mut self, inputs: &[usize]) -> PublishedMerge {
        self.publish_with("person", "import", inputs, |_, _, _| Ok(Vec::new()))
            .unwrap()
    }

    /// Pick merged: the result joins the set captured now.
    fn pick(
        &mut self,
        actor: &str,
        request_id: &str,
        inputs: &[usize],
    ) -> Result<PublishedMerge, Error> {
        let captured = development_set::capture(&self.bench.service.connection).unwrap();
        self.publish_with(actor, request_id, inputs, move |tx, _, result| {
            development_set::membership_for_publication(tx, &captured, result)
        })
    }

    /// Serve the catalog from an owner, this process's service closed meanwhile.
    fn live(&mut self) -> Live {
        let placeholder =
            EditorService::open(&self.bench.directory.join("placeholder.sqlite")).unwrap();
        drop(std::mem::replace(&mut self.bench.service, placeholder));
        let (owner, join) = OwnerHandle::start(&self.bench.catalog).unwrap();
        Live {
            owner,
            join: Some(join),
        }
    }

    /// Stop serving it and open it here again.
    fn offline(&mut self, live: Live) {
        drop(live);
        self.bench.service = EditorService::open(&self.bench.catalog).unwrap();
    }

    fn set_rows(&mut self, rows: &[&AssetId]) -> development_set::CurrentSet {
        let ids: Vec<AssetId> = rows.iter().map(|asset| (*asset).clone()).collect();
        let rows = development_set::rows_of(&self.bench.service.connection, &ids).unwrap();
        crate::editor::write(&mut self.bench.service.connection, |tx| {
            development_set::replace(tx, Some(&rows))
        })
        .unwrap()
        .unwrap()
    }

    /// The current set's members, by identity, in order.
    fn members(&self) -> Vec<AssetId> {
        development_set::page(&self.bench.service.connection, 0, 1000)
            .unwrap()
            .members
            .into_iter()
            .map(|member| member.asset_id)
            .collect()
    }

    fn picks(&self) -> Vec<String> {
        self.bench
            .service
            .connection
            .prepare("SELECT path FROM picks ORDER BY path")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn path(&self, at: usize) -> String {
        self.bench.files[at].to_string_lossy().into_owned()
    }

    fn finish(self) {
        self.bench.finish();
    }
}

fn call(
    live: &Live,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<Value, (String, String)> {
    let response = live
        .owner
        .call(
            client,
            ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .expect("the owner answered");
    match response.error {
        None => Ok(response.result.expect("a result")),
        Some(error) => Err((error.code, error.message)),
    }
}

fn ok(live: &Live, client: ClientId, method: &str, params: Value) -> Value {
    call(live, client, method, params).unwrap_or_else(|error| panic!("{method}: {error:?}"))
}

fn envelope(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": "person"})
}

/// A row as these tests name it: its file's name (`input` before an original no source item is),
/// its stack role with what a collapsed leader hides and whether only Show all revealed it, and
/// whether it is in the development set.
fn describe(row: &Value) -> String {
    let mut text = match row["item"].as_str().unwrap() {
        "input" => format!("input {}", row["file_name"].as_str().unwrap()),
        _ => row["file_name"].as_str().unwrap().to_owned(),
    };
    if let Some(stack) = row.get("stack") {
        text.push(' ');
        text.push_str(stack["role"].as_str().unwrap());
        if let Some(collapsed) = stack.get("collapsed") {
            text.push_str(&format!(" collapsed={collapsed}"));
        }
        if stack.get("outside_filter") == Some(&json!(true)) {
            text.push_str(" outside");
        }
    }
    if row.get("in_development_set") == Some(&json!(true)) {
        text.push_str(" in-set");
    }
    text
}

/// Evaluate `query` into `client`'s view: its summary and every row, described.
fn view(live: &Live, client: ClientId, query: Value) -> (Value, Vec<String>) {
    let summary = ok(live, client, "browse.view", query);
    (summary.clone(), rows(live, client, &summary))
}

fn rows(live: &Live, client: ClientId, summary: &Value) -> Vec<String> {
    let count = summary["count"].as_u64().unwrap();
    if count == 0 {
        return Vec::new();
    }
    let rows = ok(
        live,
        client,
        "browse.rows",
        json!({"from": 0, "count": count, "revision": summary["revision"]}),
    );
    rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(describe)
        .collect()
}

fn raw_rows(live: &Live, client: ClientId, summary: &Value) -> Vec<Value> {
    ok(
        live,
        client,
        "browse.rows",
        json!({"from": 0, "count": summary["count"], "revision": summary["revision"]}),
    )["rows"]
        .as_array()
        .unwrap()
        .clone()
}

fn sorted(mut rows: Vec<String>) -> Vec<String> {
    rows.sort();
    rows
}

/// A folder, event or catalog view shows the result once, over its collapsed originals, ordered by
/// its reference's capture time; overlapping folders do not repeat it; and an index rebuilt with
/// every file under another row finds the same stack. Counts tell the rows from the originals the
/// leader hides.
#[test]
fn hdr_merge_browse_sources_show_the_result_once_over_its_collapsed_originals() {
    let mut scene = Scene::new("sources");
    let hashes = scene.bench.hashes();
    let merge = scene.import(&[0, 1, 2]);
    let bracket = scene.bracket();
    let live = scene.live();
    let client = live.owner.register();
    let folder = json!({"source": {"kind": "folder", "path": bracket}});
    let expected = [
        "DSC_0002 HDR leader collapsed=3",
        "DSC_0004.JPG",
        "DSC_0005.JPG",
    ];
    let (summary, shown) = view(&live, client, folder.clone());
    assert_eq!(shown, expected);
    assert_eq!(
        (
            &summary["count"],
            &summary["stacks"],
            &summary["collapsed"],
            &summary["picked"]
        ),
        (&json!(3), &json!(1), &json!(3), &json!(0))
    );
    let first_rows = raw_rows(&live, client, &summary);
    assert_eq!(first_rows[0]["asset_id"], merge.result.as_str());
    assert_eq!(first_rows[0]["stack"]["merge_id"], merge.merge_id.as_str());
    assert_eq!(first_rows[0]["stack"]["exposures"], 3);
    assert_eq!(first_rows[0]["stack"]["expansion"], "collapsed");

    // The bracket's parent with its subfolders covers the same files: the same rows.
    let parent = json!({"source": {
        "kind": "folder", "path": bracket.parent().unwrap(), "subfolders": true,
    }});
    assert_eq!(view(&live, client, parent).1, expected);

    // Across every event, the result once, over its originals.
    let events = ok(&live, client, "event.list", json!({}));
    let mut across = Vec::new();
    for event in events["events"].as_array().unwrap() {
        let source = json!({"source": {"kind": "event", "event_id": event["id"]}});
        across.extend(view(&live, client, source).1);
    }
    assert_eq!(sorted(across), sorted(expected.map(str::to_owned).to_vec()));

    // Over photographs: the result leads the developed original; the undeveloped ones are none
    // of the catalog's.
    let all = json!({"source": {"kind": "all-photographs"}});
    let (summary, shown) = view(&live, client, all);
    assert_eq!(
        sorted(shown),
        ["DSC_0002 HDR leader collapsed=1", "IMG_0001.JPG"]
    );
    assert_eq!(
        (&summary["count"], &summary["collapsed"]),
        (&json!(2), &json!(1))
    );

    // The index rebuilt, every file under another row: the same stack.
    scene.offline(live);
    scene.seed_index(true);
    let live = scene.live();
    let client = live.owner.register();
    let (summary, shown) = view(&live, client, folder);
    assert_eq!(shown, expected);
    let rebuilt = raw_rows(&live, client, &summary);
    assert_eq!(rebuilt[0]["stack"]["merge_id"], merge.merge_id.as_str());
    assert_ne!(
        rebuilt[1]["file_id"], first_rows[1]["file_id"],
        "another row"
    );
    drop(live);
    assert_eq!(scene.bench.hashes(), hashes);
    scene.bench.service = EditorService::open(&scene.bench.catalog).unwrap();
    scene.finish();
}

/// Filters act on each item's own metadata: a matching original whose leader does not match shows
/// on its own with its stack's badge; a matching leader hides only the originals the view holds,
/// and Show all brings back the rest labelled outside the filter; expanding changes no library
/// item. Picked counts files, and the pick facet counts each item once.
#[test]
fn hdr_merge_browse_filters_act_on_each_items_own_metadata() {
    let mut scene = Scene::new("filters");
    let merge = scene.import(&[0, 1, 2]);
    let bracket = scene.bracket();
    let (third, fourth) = (scene.path(2), scene.path(3));
    let live = scene.live();
    let client = live.owner.register();
    ok(
        &live,
        client,
        "pick.set",
        json!({
            "targets": {"kind": "paths", "paths": [third, fourth]},
            "picked": true,
            "mutation": envelope("pick"),
        }),
    );
    let query =
        |filter: Value| json!({"source": {"kind": "folder", "path": bracket}, "filter": filter});
    let (summary, shown) = view(&live, client, query(json!({"picked": true})));
    assert_eq!(shown, ["DSC_0003.JPG separate", "DSC_0004.JPG"]);
    assert_eq!(
        (&summary["picked"], &summary["stacks"]),
        (&json!(2), &json!(0))
    );
    assert_eq!(
        view(&live, client, query(json!({"text": "dsc_0001"}))).1,
        ["DSC_0001.JPG separate"]
    );
    // The leader matches and none of its originals does.
    let (summary, shown) = view(&live, client, query(json!({"text": "HDR"})));
    assert_eq!(shown, ["DSC_0002 HDR leader"]);
    let journal = ok(&live, client, "library.journal", json!({}));
    let expanded = ok(
        &live,
        client,
        "stack.set-expanded",
        json!({"merge_id": merge.merge_id, "expansion": "expanded", "revision": summary["revision"]}),
    );
    assert_eq!(rows(&live, client, &expanded), ["DSC_0002 HDR leader"]);
    let all = ok(
        &live,
        client,
        "stack.set-expanded",
        json!({"merge_id": merge.merge_id, "expansion": "all"}),
    );
    assert_eq!(
        rows(&live, client, &all),
        [
            "DSC_0002 HDR leader",
            "DSC_0001.JPG original outside",
            "DSC_0002.JPG original outside",
            "DSC_0003.JPG original outside",
        ]
    );
    assert_eq!(ok(&live, client, "library.journal", json!({})), journal);
    // Another client's view keeps the stack collapsed: expansion is the caller's.
    let other = live.owner.register();
    assert_eq!(
        view(&live, other, query(json!({}))).1,
        [
            "DSC_0002 HDR leader collapsed=3",
            "DSC_0004.JPG",
            "DSC_0005.JPG"
        ]
    );
    let inspected = ok(
        &live,
        client,
        "stack.inspect",
        json!({"merge_id": merge.merge_id}),
    );
    assert_eq!(inspected["expansion"], "all");
    // The pick facet counts every item of the source once: two picked files, the result and three
    // other files not picked.
    let facets = ok(
        &live,
        client,
        "browse.facets",
        json!({"source": {"kind": "folder", "path": bracket}, "facets": ["pick"]}),
    );
    let counts: Vec<(String, u64)> = facets["counts"]["pick"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            (
                value["value"].as_str().unwrap().to_owned(),
                value["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        counts,
        [("not-picked".to_owned(), 4), ("picked".to_owned(), 2)]
    );
    scene.offline(live);
    scene.finish();
}

/// A result in the current development set is Picked over files, carries In Development set and
/// decides its bracket for Moments without a pick, while Picked and Develop N count file picks
/// only, a hidden original's pick among them. A collapsed cell's selection is the leader alone.
/// Another client's change to the set makes the view stale, and its next evaluation shows it.
#[test]
fn hdr_merge_browse_a_result_in_the_set_is_picked_and_decides_its_bracket() {
    let mut scene = Scene::new("picked");
    let merge = scene.import(&[0, 1, 2]);
    let bracket = scene.bracket();
    let (first, fourth) = (scene.path(0), scene.path(3));
    let live = scene.live();
    let client = live.owner.register();
    let agent = live.owner.register();
    ok(
        &live,
        client,
        "pick.set",
        json!({
            "targets": {"kind": "paths", "paths": [first, fourth]},
            "picked": true,
            "mutation": envelope("pick"),
        }),
    );
    let query =
        |filter: Value| json!({"source": {"kind": "folder", "path": bracket}, "filter": filter});
    // Imported alone, the result decides nothing: the bracket is decided by its own pick.
    let (_, shown) = view(&live, client, query(json!({"without_pick": true})));
    assert!(
        shown.contains(&"DSC_0002 HDR leader".to_owned()),
        "{shown:?}"
    );
    ok(
        &live,
        agent,
        "development-set.add",
        json!({"asset_ids": [merge.result], "mutation": envelope("add")}),
    );
    let (summary, shown) = view(&live, client, query(json!({"picked": true})));
    assert_eq!(
        shown,
        ["DSC_0002 HDR leader collapsed=1 in-set", "DSC_0004.JPG"]
    );
    assert_eq!(
        (&summary["picked"], &summary["in_development_set"]),
        (&json!(2), &json!(1)),
        "picked counts the two files"
    );
    assert_eq!(
        view(&live, client, query(json!({"without_pick": true}))).1,
        ["DSC_0005.JPG"]
    );
    let (summary, shown) = view(&live, client, query(json!({})));
    assert_eq!(
        shown,
        [
            "DSC_0002 HDR leader collapsed=3 in-set",
            "DSC_0004.JPG",
            "DSC_0005.JPG"
        ]
    );
    // Develop N: the file picks of the view, the hidden original's among them, never the result.
    let plan = ok(&live, client, "pick.plan", json!({}));
    assert_eq!(plan["count"], 2, "{plan}");
    // The collapsed cell's selection is its leader alone.
    let session = ok(
        &live,
        client,
        "browse.select",
        json!({"range": {"start": 0, "len": 1}, "revision": summary["revision"]}),
    );
    assert_eq!(session["browse"]["selection"]["count"], 1);
    let refused = call(
        &live,
        client,
        "pick.set",
        json!({"targets": {"kind": "selection"}, "picked": true, "mutation": envelope("sel")}),
    )
    .unwrap_err();
    assert_eq!(
        refused.0, "validation",
        "the selection holds the result, no file"
    );
    // Another client takes the result out of the set: the view goes stale with the event.
    let events = ok(&live, client, "events.since", json!({"after": 0}));
    let cursor = events["current_sequence"].clone();
    ok(
        &live,
        agent,
        "development-set.remove",
        json!({"asset_ids": [merge.result], "mutation": envelope("remove")}),
    );
    let events = ok(&live, client, "events.since", json!({"after": cursor}));
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event.get("development_set_revision").is_some()),
        "{events}"
    );
    let state = ok(&live, client, "session.state", json!({}));
    assert_eq!(state["browse"]["stale"], true);
    assert_eq!(
        view(&live, client, query(json!({}))).1,
        [
            "DSC_0002 HDR leader collapsed=3",
            "DSC_0004.JPG",
            "DSC_0005.JPG"
        ]
    );
    scene.offline(live);
    scene.finish();
}

/// Originals the view's source does not hold are provenance rows once their stack is expanded, with
/// why each is not available; `stack.inspect` names each original's availability, reason, pick,
/// index row and photograph, and its reference.
#[test]
fn hdr_merge_browse_offline_and_unlisted_originals_have_provenance_rows() {
    let mut scene = Scene::new("provenance");
    let merge = scene.import(&[0, 1, 2]);
    let bracket = scene.bracket();
    let developed = scene.bench.developed.clone();
    let first = scene.path(0);
    // The first original is no longer listed.
    let index = rusqlite::Connection::open(
        crate::index::index_dir(&scene.bench.catalog).join(crate::INDEX_FILE),
    )
    .unwrap();
    index
        .execute("DELETE FROM files WHERE path = ?1", [&first])
        .unwrap();
    let live = scene.live();
    let client = live.owner.register();
    let folder = json!({"source": {"kind": "folder", "path": bracket}});
    assert_eq!(
        view(&live, client, folder).1,
        [
            "DSC_0002 HDR leader collapsed=2",
            "DSC_0004.JPG",
            "DSC_0005.JPG"
        ]
    );
    let expanded = ok(
        &live,
        client,
        "stack.set-expanded",
        json!({"merge_id": merge.merge_id, "expansion": "expanded"}),
    );
    assert_eq!(
        rows(&live, client, &expanded),
        [
            "DSC_0002 HDR leader",
            "input DSC_0001.JPG original",
            "DSC_0002.JPG original",
            "DSC_0003.JPG original",
            "DSC_0004.JPG",
            "DSC_0005.JPG",
        ]
    );
    let provenance = &raw_rows(&live, client, &expanded)[1];
    assert_eq!(
        (
            &provenance["merge_id"],
            &provenance["ordinal"],
            &provenance["availability"],
            &provenance["reason"]
        ),
        (
            &json!(merge.merge_id),
            &json!(0),
            &json!("missing"),
            &json!("no indexed folder or card lists it")
        )
    );
    let inspected = ok(
        &live,
        client,
        "stack.inspect",
        json!({"merge_id": merge.merge_id}),
    );
    let originals = inspected["originals"].as_array().unwrap();
    assert_eq!(
        (inspected["exposures"].as_u64(), originals.len()),
        (Some(3), 3)
    );
    assert_eq!(originals[0]["availability"], "missing");
    assert!(originals[0].get("file_id").is_none());
    assert_eq!(originals[1]["asset_id"], developed.as_str());
    assert_eq!(originals[1]["reference"], true);
    assert_eq!(originals[2]["availability"], "available");
    let page = ok(
        &live,
        client,
        "stack.inspect",
        json!({"merge_id": merge.merge_id, "from": 2, "limit": 1}),
    );
    assert_eq!(page["originals"].as_array().unwrap().len(), 1);
    assert_eq!(page["originals"][0]["ordinal"], 2);
    // The folder's volume taken offline: its listed originals are offline, with the reason.
    index.execute("UPDATE roots SET offline = 1", []).unwrap();
    let inspected = ok(
        &live,
        client,
        "stack.inspect",
        json!({"merge_id": merge.merge_id}),
    );
    assert_eq!(
        (
            &inspected["originals"][2]["availability"],
            &inspected["originals"][2]["reason"]
        ),
        (&json!("offline"), &json!("its volume is offline"))
    );
    // Over photographs, expanded: the developed original under its leader, and the others as
    // provenance rows.
    let all = ok(
        &live,
        client,
        "browse.view",
        json!({"source": {"kind": "all-photographs"}}),
    );
    let shown = rows(&live, client, &all);
    let at = shown
        .iter()
        .position(|row| row == "DSC_0002 HDR leader")
        .unwrap();
    assert_eq!(
        shown[at..at + 4],
        [
            "DSC_0002 HDR leader",
            "input DSC_0001.JPG original",
            "DSC_0002.JPG original",
            "input DSC_0003.JPG original",
        ]
    );
    assert_eq!(
        call(
            &live,
            client,
            "stack.inspect",
            json!({"merge_id": "merge-0123456789abcdef0123456789abcdef"})
        )
        .unwrap_err()
        .0,
        "validation"
    );
    drop(index);
    scene.offline(live);
    scene.finish();
}

/// The set is one authority for every client: a replacement and each membership change move its
/// revision on and announce it; a client naming a set since replaced is refused by name; a retry
/// is answered as first; an undo writes only the member it names, in a retired set too.
#[test]
fn hdr_merge_browse_development_set_revisions_and_conflicts_across_clients() {
    let mut scene = Scene::new("clients");
    let merge = scene.import(&[0, 1, 2]);
    let (developed, other) = (scene.bench.developed.clone(), scene.other.clone());
    let live = scene.live();
    let desktop = live.owner.register();
    let agent = live.owner.register();
    assert_eq!(
        ok(&live, agent, "development-set.read", json!({}))["count"],
        0,
        "opening the catalog leaves no set"
    );
    let replace = json!({
        "source": {"kind": "assets", "asset_ids": [developed, other, developed]},
        "mutation": envelope("open"),
    });
    let first = ok(&live, desktop, "development-set.replace", replace.clone());
    assert_eq!(first["count"], 2);
    let mut retry = first.clone();
    retry["deduplicated"] = json!(true);
    assert_eq!(
        ok(&live, desktop, "development-set.replace", replace),
        retry,
        "a retry"
    );
    let set = first["set_id"].clone();
    let read = ok(&live, agent, "development-set.read", json!({}));
    assert_eq!(
        (&read["set_id"], &read["revision"]),
        (&set, &first["revision"])
    );
    let added = ok(
        &live,
        agent,
        "development-set.add",
        json!({"asset_ids": [merge.result, other], "set_id": set, "mutation": {"request_id": "add", "actor": "agent"}}),
    );
    assert_eq!((&added["count"], &added["items"]), (&json!(3), &json!(1)));
    assert!(added["revision"].as_u64() > first["revision"].as_u64());
    let members: Vec<Value> = ok(&live, desktop, "development-set.read", json!({}))["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["asset_id"].clone())
        .collect();
    assert_eq!(
        members,
        [json!(developed), json!(other), json!(merge.result)]
    );
    // The desktop replaces the set; the agent's next change, meant for the old one, is refused.
    let second = ok(
        &live,
        desktop,
        "development-set.replace",
        json!({"source": {"kind": "assets", "asset_ids": [other]}, "mutation": envelope("open-2")}),
    );
    let (code, message) = call(
        &live,
        agent,
        "development-set.add",
        json!({"asset_ids": [developed], "set_id": set, "mutation": {"request_id": "late", "actor": "agent"}}),
    )
    .unwrap_err();
    assert_eq!(code, "conflict");
    assert!(message.contains(set.as_str().unwrap()), "{message}");
    // The agent's undo takes its member out of the retired set and leaves the current one alone.
    ok(
        &live,
        agent,
        "library.undo",
        json!({"mutation": {"request_id": "undo", "actor": "agent"}}),
    );
    let current = ok(&live, desktop, "development-set.read", json!({}));
    assert_eq!(current["set_id"], second["set_id"]);
    assert_eq!(current["members"][0]["asset_id"], json!(other));
    assert_eq!(current["count"], 1);
    let removed = ok(
        &live,
        desktop,
        "development-set.remove",
        json!({"asset_ids": [developed], "mutation": envelope("remove-none")}),
    );
    assert_eq!(removed["outcome"], "no-op");
    assert_eq!(
        call(
            &live,
            desktop,
            "development-set.replace",
            json!({"source": {"kind": "view", "revision": 4, "position": 0}, "mutation": envelope("no-view")}),
        )
        .unwrap_err()
        .0,
        "validation"
    );
    // A view's set: its photographs around the one opened.
    let all = ok(
        &live,
        desktop,
        "browse.view",
        json!({"source": {"kind": "all-photographs"}}),
    );
    let from_view = ok(
        &live,
        desktop,
        "development-set.replace",
        json!({"source": {"kind": "view", "revision": all["revision"], "position": 0}, "mutation": envelope("open-view")}),
    );
    assert_eq!(from_view["count"], all["count"]);
    assert_eq!(from_view["active"], 0);
    scene.offline(live);
    scene.finish();
}

/// Pick merged appends its result to the captured set; Remove takes it out, with its stack, in
/// the same change, and undo and Put back restore that membership at its place; creation undo and
/// redo move only the result; another actor's later change to the member refuses the undo by name;
/// every other member and every pick stays as it was, and the originals' bytes too.
#[test]
fn hdr_merge_browse_remove_put_back_undo_and_redo_keep_unrelated_members_and_picks() {
    let mut scene = Scene::new("lifecycle");
    let hashes = scene.bench.hashes();
    let (developed, other) = (scene.bench.developed.clone(), scene.other.clone());
    let late = scene
        .bench
        .service
        .import(&scene.bench.files[4].clone())
        .unwrap()
        .asset
        .id;
    let pick = scene.path(3);
    scene
        .bench
        .service
        .connection
        .execute(
            "INSERT INTO picks (path, byte_len, modified_ns, device, inode, volume_id, actor,
                 request_id, picked_ms)
             VALUES (?1, 1, 1, NULL, NULL, ?2, 'person', 'pick', 1)",
            [pick.as_str(), scene.volume.as_str()],
        )
        .unwrap();
    let picks = scene.picks();
    scene.set_rows(&[&developed, &other]);
    let merge = scene.pick("person", "pick-merged", &[0, 1, 2]).unwrap();
    let result = merge.result.clone();
    assert_eq!(
        scene.members(),
        [developed.clone(), other.clone(), result.clone()]
    );
    // An agent appends a later member.
    let set = development_set::current(&scene.bench.service.connection)
        .unwrap()
        .unwrap();
    let changes = development_set::additions(&set, std::slice::from_ref(&late)).unwrap();
    scene
        .bench
        .service
        .library_write(|tx| {
            journal::apply(
                tx,
                request("development-set.add", "agent", "late"),
                changes,
                |_| "late".into(),
            )
        })
        .unwrap();
    let all = vec![
        developed.clone(),
        other.clone(),
        result.clone(),
        late.clone(),
    ];
    let without = vec![developed.clone(), other.clone(), late.clone()];
    assert_eq!(scene.members(), all);

    scene.bench.remove_as("person", "remove", &result).unwrap();
    assert_eq!(scene.members(), without);
    assert_eq!(scene.picks(), picks);
    scene.bench.undo_as("person", "undo-remove").unwrap();
    assert_eq!(scene.members(), all, "back at its place");
    scene
        .bench
        .remove_as("person", "remove-2", &result)
        .unwrap();
    scene
        .bench
        .restore_as("person", "put-back", &result)
        .unwrap();
    assert_eq!(
        scene.members(),
        all,
        "Put back restores what the removal took"
    );
    scene.bench.undo_as("person", "undo-put-back").unwrap();
    assert_eq!(scene.members(), without);
    scene.bench.redo_as("person", "redo-put-back").unwrap();
    assert_eq!(scene.members(), all);

    // A second merge, its creation undone and redone: only its own membership moves.
    let second = scene.pick("person", "pick-merged-2", &[3, 4]).unwrap();
    assert_eq!(scene.members().last(), Some(&second.result));
    scene.bench.undo_as("person", "undo-create").unwrap();
    assert_eq!(scene.members(), all);
    scene.bench.redo_as("person", "redo-create").unwrap();
    assert_eq!(scene.members().len(), 5);
    // Another actor takes the second result out of the set: the creation's undo is refused by name,
    // all or nothing.
    let set = development_set::current(&scene.bench.service.connection)
        .unwrap()
        .unwrap();
    let changes = development_set::removals(&set, std::slice::from_ref(&second.result));
    scene
        .bench
        .service
        .library_write(|tx| {
            journal::apply(
                tx,
                request("development-set.remove", "agent", "take"),
                changes,
                |_| "take".into(),
            )
        })
        .unwrap();
    let refused = scene.bench.undo_as("person", "undo-create-2").unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Conflict, "{refused}");
    assert!(
        refused.data.as_ref().unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "set-member"),
        "{refused:?}"
    );
    assert_eq!(
        scene.bench.removed_ms(&second.result),
        None,
        "nothing undone"
    );
    assert_eq!(scene.members(), all);
    assert_eq!(scene.picks(), picks);
    assert_eq!(scene.bench.hashes(), hashes);
    scene.finish();
}

/// A set replaced while a Pick merged ran is a named conflict at publication, and the transaction
/// writes nothing: no result, merge, stack or member; with no set captured, the result starts one
/// of its own; Import merged leaves the set alone.
#[test]
fn hdr_merge_browse_a_replaced_set_refuses_the_publication_whole() {
    let mut scene = Scene::new("replaced");
    let (developed, other) = (scene.bench.developed.clone(), scene.other.clone());
    let first = scene.set_rows(&[&other]);
    let captured = development_set::capture(&scene.bench.service.connection).unwrap();
    let second = scene.set_rows(&[&developed]);
    let assets = scene.bench.count("assets");
    let error = scene
        .publish_with("person", "pick-merged", &[0, 1, 2], move |tx, _, result| {
            development_set::membership_for_publication(tx, &captured, result)
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    let data = error.data.as_ref().unwrap();
    assert_eq!(
        (&data["set_id"], &data["current"]),
        (&json!(first.id), &json!(second.id))
    );
    assert_eq!(scene.bench.count("assets"), assets);
    assert_eq!(scene.bench.count("merges"), 0);
    assert_eq!(scene.bench.count("stack_claims"), 0);
    assert_eq!(scene.members(), std::slice::from_ref(&developed));
    // Import merged: the set is as it was.
    let imported = scene.import(&[0, 1, 2]);
    assert_eq!(scene.members(), std::slice::from_ref(&developed));
    assert!(scene.bench.removed_ms(&imported.result).is_none());
    // No set captured and none now: the result starts one of its own.
    crate::editor::write(&mut scene.bench.service.connection, |tx| {
        development_set::replace(tx, None)
    })
    .unwrap();
    let picked = scene.pick("person", "pick-alone", &[3, 4]).unwrap();
    assert_eq!(scene.members(), [picked.result]);
    scene.finish();
}
