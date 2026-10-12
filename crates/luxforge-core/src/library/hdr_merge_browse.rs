//! HDR stacks in browsing, against a real catalog, index and owner: a merge's result shown once
//! over its collapsed originals in folder, event and catalog views and across an index rebuilt
//! with other rows; filters acting on each item's own metadata, Show all labelling what they leave
//! out; an active result deciding its bracket, never Picked, and selected alone; offline and
//! unlisted originals as provenance rows with their reasons; and Remove, Put back, undo and redo of
//! a merge leaving every pick as it was. Each expectation is written out from the seeded rows,
//! never read back from what is tested, and the originals' bytes are checked unchanged.
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
        let reference = provenance.reference_input().path.clone();
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
                folder: &crate::library::develop::Destination::Existing {
                    folder_id: folder.clone(),
                    days: false,
                },
                file_name: &name,
                header: &header(at),
                place: None,
                volumes: std::slice::from_ref(&volume),
                replaces: None,
            },
            extra,
        )
    }

    /// A merge of the bracket's files `inputs`, with nothing else in its change.
    fn merge(&mut self, request_id: &str, inputs: &[usize]) -> PublishedMerge {
        self.publish_with("person", request_id, inputs, |_, _, _| Ok(Vec::new()))
            .unwrap()
    }

    fn import(&mut self, inputs: &[usize]) -> PublishedMerge {
        self.merge("import", inputs)
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
/// and its stack role with what a collapsed leader hides and whether only Show all revealed it.
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
    assert_eq!(first_rows[0]["stack"]["size"], 3);
    assert_eq!(first_rows[0]["stack"]["kind"], "hdr");
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

/// An active result decides its bracket for Moments without a pick, and itself with it, but is
/// never Picked: Picked and Develop N count file picks only, a hidden original's pick among them.
/// A collapsed cell's selection is the leader alone. Another client's Remove merge makes the view
/// stale, and its next evaluation leaves the bracket undecided.
#[test]
fn hdr_merge_browse_an_active_result_decides_its_bracket_and_is_not_picked() {
    let mut scene = Scene::new("picked");
    let merge = scene.import(&[0, 1, 2]);
    let bracket = scene.bracket();
    let (first, fourth) = (scene.path(0), scene.path(3));
    let live = scene.live();
    let client = live.owner.register();
    let agent = live.owner.register();
    let pick = |paths: Value, picked: bool, request_id: &str| {
        ok(
            &live,
            client,
            "pick.set",
            json!({
                "targets": {"kind": "paths", "paths": paths},
                "picked": picked,
                "mutation": envelope(request_id),
            }),
        )
    };
    pick(json!([fourth]), true, "pick-fourth");
    let query =
        |filter: Value| json!({"source": {"kind": "folder", "path": bracket}, "filter": filter});
    // No original is picked: the active result alone decides the bracket, and itself.
    assert_eq!(
        view(&live, client, query(json!({"without_pick": true}))).1,
        ["DSC_0005.JPG"]
    );
    pick(json!([first]), true, "pick-first");
    // The result is not Picked: the picked original shows on its own, its leader left out.
    let (summary, shown) = view(&live, client, query(json!({"picked": true})));
    assert_eq!(shown, ["DSC_0001.JPG separate", "DSC_0004.JPG"]);
    assert_eq!(summary["picked"], 2, "picked counts the two files");
    let (_, shown) = view(&live, client, query(json!({})));
    assert_eq!(
        shown,
        [
            "DSC_0002 HDR leader collapsed=3",
            "DSC_0004.JPG",
            "DSC_0005.JPG"
        ]
    );
    // Develop N: the file picks of the view, the hidden original's among them, never the result.
    let plan = ok(&live, client, "pick.plan", json!({}));
    assert_eq!(plan["count"], 2, "{plan}");
    // The collapsed cell's selection is its leader alone. The index lane can move the index on in
    // the background after the owner starts, and `pick.plan` evaluates a view the index moved on
    // from again, so the selection names the view's revision as it is now.
    let (summary, shown) = view(&live, client, query(json!({})));
    assert_eq!(shown[0], "DSC_0002 HDR leader collapsed=3");
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
    // Another client removes the merge: the view goes stale, and with the original's pick cleared
    // the bracket is undecided again.
    ok(
        &live,
        agent,
        "merge.remove",
        json!({"merge_ids": [merge.merge_id], "mutation": envelope("remove")}),
    );
    let state = ok(&live, client, "session.state", json!({}));
    assert_eq!(state["browse"]["stale"], true);
    pick(json!([first]), false, "clear-first");
    assert_eq!(
        view(&live, client, query(json!({"without_pick": true}))).1,
        [
            "DSC_0001.JPG",
            "DSC_0002.JPG",
            "DSC_0003.JPG",
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
    assert_eq!((inspected["size"].as_u64(), originals.len()), (Some(3), 3));
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

/// Remove takes a merge's result to Removed with its stack in the same change, and undo and Put
/// back restore both; a creation's undo and redo move only its own result; another actor's later
/// change to the result refuses the creation's undo by name, all or nothing; every pick stays as it
/// was, and the originals' bytes too.
#[test]
fn hdr_merge_browse_remove_put_back_undo_and_redo_keep_unrelated_picks() {
    let mut scene = Scene::new("lifecycle");
    let hashes = scene.bench.hashes();
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
    // Whether a merge's result is in Removed, and its stack's state.
    let state = |scene: &Scene, merge: &PublishedMerge| {
        (
            scene.bench.removed_ms(&merge.result).is_some(),
            super::state(&scene.bench.service.connection, &merge.merge_id).unwrap(),
        )
    };
    let active = (false, Some(MergeState::Active));
    let removed = (true, Some(MergeState::Removed));
    let merge = scene.merge("merge", &[0, 1, 2]);
    let result = merge.result.clone();
    assert_eq!(state(&scene, &merge), active);

    scene.bench.remove_as("person", "remove", &result).unwrap();
    assert_eq!(state(&scene, &merge), removed);
    assert_eq!(scene.picks(), picks);
    scene.bench.undo_as("person", "undo-remove").unwrap();
    assert_eq!(state(&scene, &merge), active, "back as it was");
    scene
        .bench
        .remove_as("person", "remove-2", &result)
        .unwrap();
    scene
        .bench
        .restore_as("person", "put-back", &result)
        .unwrap();
    assert_eq!(state(&scene, &merge), active, "Put back");
    scene.bench.undo_as("person", "undo-put-back").unwrap();
    assert_eq!(state(&scene, &merge), removed);
    scene.bench.redo_as("person", "redo-put-back").unwrap();
    assert_eq!(state(&scene, &merge), active);

    // A second merge, its creation undone and redone: only its own result moves.
    let second = scene.merge("merge-2", &[3, 4]);
    assert_eq!(state(&scene, &second), active);
    scene.bench.undo_as("person", "undo-create").unwrap();
    assert_eq!(state(&scene, &second), (true, Some(MergeState::Undone)));
    assert_eq!(state(&scene, &merge), active);
    scene.bench.redo_as("person", "redo-create").unwrap();
    assert_eq!(state(&scene, &second), active);
    // Another actor removes the second result: the creation's undo is refused by name, all or
    // nothing.
    scene
        .bench
        .remove_as("agent", "take", &second.result)
        .unwrap();
    let refused = scene.bench.undo_as("person", "undo-create-2").unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Conflict, "{refused}");
    assert!(
        refused.data.as_ref().unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "asset-removal"),
        "{refused:?}"
    );
    assert_eq!(state(&scene, &second), removed, "nothing undone");
    assert_eq!(state(&scene, &merge), active);
    assert_eq!(scene.picks(), picks);
    assert_eq!(scene.bench.hashes(), hashes);
    scene.finish();
}

/// Browse reads a panorama stack, which this build cannot make, beside an HDR one: its rows carry
/// its kind and size and no EV offset, and `stack.inspect` lists its originals without offsets
/// and, since this build reads no panorama provenance, without a reference; HDR's reference comes
/// from its provenance.
#[test]
fn merge_kind_browse_reads_a_panorama_stack_beside_an_hdr_one() {
    let mut scene = Scene::new("panorama");
    let hdr = scene.import(&[0, 1, 2]);
    let connection = &scene.bench.service.connection;
    let source: String = connection
        .query_row(
            "SELECT source_id FROM merges WHERE id = ?1",
            [hdr.merge_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    // A panorama of the two singles, led by the other folder's photograph, as a later build
    // records one: its inputs have no EV offset.
    let panorama = MergeId::new();
    connection
        .execute(
            "INSERT INTO merges (id, kind, result_asset_id, source_id, match_key,
                 provenance_json, created_ms)
             VALUES (?1, 'panorama', ?2, ?3, 'key', ?4, 1)",
            rusqlite::params![
                panorama.as_str(),
                scene.other.as_str(),
                source,
                json!({"format": 2, "kind": "panorama", "parameters": {"panorama": {}}})
                    .to_string(),
            ],
        )
        .unwrap();
    for (ordinal, at) in [3, 4].into_iter().enumerate() {
        connection
            .execute(
                "INSERT INTO merge_inputs (merge_id, ordinal, path, asset_id, offset_ev)
                 VALUES (?1, ?2, ?3, NULL, NULL)",
                rusqlite::params![panorama.as_str(), ordinal as i64, scene.path(at)],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO stack_claims (path, merge_id) VALUES (?1, ?2)",
                [scene.path(at), panorama.to_string()],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO merge_stacks (merge_id, state) VALUES (?1, 'active')",
            [panorama.as_str()],
        )
        .unwrap();
    let bracket = scene.bracket();
    let live = scene.live();
    let client = live.owner.register();
    let folder = json!({"source": {"kind": "folder", "path": bracket}});
    let (summary, shown) = view(&live, client, folder);
    assert_eq!(
        sorted(shown),
        [
            "DSC_0002 HDR leader collapsed=3",
            "IMG_0001.JPG leader collapsed=2"
        ]
    );
    let rows = raw_rows(&live, client, &summary);
    let led = |merge: &MergeId| {
        rows.iter()
            .find(|row| row["stack"]["merge_id"] == merge.as_str())
            .unwrap()["stack"]
            .clone()
    };
    assert_eq!(
        (&led(&hdr.merge_id)["kind"], &led(&hdr.merge_id)["size"]),
        (&json!("hdr"), &json!(3))
    );
    assert_eq!(
        (&led(&panorama)["kind"], &led(&panorama)["size"]),
        (&json!("panorama"), &json!(2))
    );
    let expanded = ok(
        &live,
        client,
        "stack.set-expanded",
        json!({"merge_id": panorama, "expansion": "expanded"}),
    );
    let originals: Vec<Value> = raw_rows(&live, client, &expanded)
        .into_iter()
        .filter(|row| row["stack"]["role"] == "original")
        .map(|row| row["stack"].clone())
        .collect();
    assert_eq!(originals.len(), 2, "{originals:?}");
    for original in &originals {
        assert_eq!(original["kind"], "panorama");
        assert!(original.get("offset_ev").is_none(), "{original}");
    }

    let inspected = ok(
        &live,
        client,
        "stack.inspect",
        json!({"merge_id": panorama}),
    );
    assert_eq!(
        (&inspected["kind"], &inspected["size"]),
        (&json!("panorama"), &json!(2))
    );
    for original in inspected["originals"].as_array().unwrap() {
        assert!(original.get("offset_ev").is_none(), "{original}");
        assert!(original.get("reference").is_none(), "{original}");
    }
    let inspected = ok(
        &live,
        client,
        "stack.inspect",
        json!({"merge_id": hdr.merge_id}),
    );
    let offsets: Vec<&Value> = inspected["originals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|original| &original["offset_ev"])
        .collect();
    assert_eq!(offsets, [&json!(-1.0), &json!(0.0), &json!(1.0)]);
    assert_eq!(inspected["originals"][1]["reference"], true);
    // The panorama's provenance is kept as it was written, and read as incompatible.
    let refused = call(
        &live,
        client,
        "merge.inspect",
        json!({"merge_id": panorama}),
    )
    .unwrap_err();
    assert_eq!(refused.0, "incompatible", "{refused:?}");
    drop(live);
    scene.bench.service = EditorService::open(&scene.bench.catalog).unwrap();
    scene.finish();
}
