//! Missing originals against a real owner, over a catalog whose originals are copied fixtures in a
//! scratch directory that the test then moves, rewrites, duplicates and deletes: the groups, each
//! row's result, a Choose…, Relink of exactly the verified pairs, Stop search, a Locate… that
//! verifies and one that is refused, and Develop's Locate original…. Each owner call runs exactly as
//! its task would, through the `*_now` functions the tasks run, and its answer is handed back as the
//! task's message. A second client of the same owner — an independent API client — sends the same
//! requests as plain JSON, and every row and every change is compared with what it is answered.
use crate::{
    Config,
    app::{
        Boot, Editor,
        message::{Message, select::SelectMessage, select_missing::MissingMessage},
        select::{evaluate_now, facets_now, job_now},
        select_missing::{cancel_now, facts_now, find_now, locate_now, missing_now, relink_now},
        tasks::request,
    },
    state::{
        canvas::NoticeAction,
        select::Shown,
        select_missing::{self as model, LocateFrom, MissingInfo, SearchStatus},
    },
};
use luxforge_core::{
    ApiRequest, AssetId, ClientId, EditorService, OwnerHandle,
    catalog_types::{FindResult, MissingOriginals, ViewSource},
};
use luxforge_testbase::paths::{fixture, temp_dir};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// An independent API client on the editor's owner, and its plain JSON requests.
struct Client {
    owner: OwnerHandle,
    id: ClientId,
}

impl Client {
    fn send(&self, method: &str, params: Value) -> Result<Value, String> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let response = self
            .owner
            .call(
                self.id,
                ApiRequest {
                    id: format!("resolve-client-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .map_err(|error| error.to_string())?;
        match response.error {
            Some(error) => Err(format!("{}: {}", error.code, error.message)),
            None => Ok(response.result.unwrap_or(Value::Null)),
        }
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        self.send(method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    /// Read a job until it ends.
    fn settle(&self, job: &Value) -> Value {
        luxforge_testbase::wait_for("a job to end", || {
            let read = self.ok("job.read", json!({ "job_id": job }));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    fn import(&self, path: &Path) -> AssetId {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let started = self.ok(
            "catalog.import",
            json!({"path": path, "mutation": {"request_id": format!("import-{}", NEXT.fetch_add(1, Ordering::Relaxed)), "actor": "setup"}}),
        );
        let settled = self.settle(&started["job_id"]);
        assert_eq!(settled["status"], "ready", "{settled}");
        serde_json::from_value(settled["result"]["asset"]["id"].clone()).unwrap()
    }

    fn locator(&self, asset: &AssetId) -> PathBuf {
        PathBuf::from(
            self.ok("asset.state", json!({ "asset_id": asset }))["asset"]["locator"]
                .as_str()
                .unwrap(),
        )
    }

    fn journal(&self) -> Value {
        self.ok("library.journal", json!({"limit": 500}))
    }
}

/// The scratch catalog and its originals.
struct Scene {
    dir: PathBuf,
    editor: Editor,
    client: Client,
    /// The folder the lake photographs were developed from, and the one the archive now holds.
    lake: PathBuf,
    archive: PathBuf,
    /// Two photographs of another folder, for a search that is stopped, and a deep tree to search.
    winter: PathBuf,
    tree: PathBuf,
    /// By name: the photographs and where their files are now.
    assets: Vec<(String, AssetId)>,
    moved: PathBuf,
    copies: [PathBuf; 2],
    rewritten: PathBuf,
    rescued: PathBuf,
}

impl Scene {
    fn asset(&self, name: &str) -> AssetId {
        self.assets
            .iter()
            .find(|(each, _)| each == name)
            .map(|(_, asset)| asset.clone())
            .unwrap()
    }
}

fn copy(name: &str, to: &Path) -> PathBuf {
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    fs::copy(fixture(&format!("s0/{name}")), to).unwrap();
    to.canonicalize().unwrap()
}

fn place(from: &Path, to: &Path) -> PathBuf {
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    fs::rename(from, to).unwrap();
    to.canonicalize().unwrap()
}

/// Five photographs developed from `card/2026-09-12 Lake` and two from `card/2025 Winter`; then
/// the lake folder is reorganized into `archive/`: one moved, one rewritten (other bytes at its
/// name), one duplicated into two folders, one deleted (a copy rescued outside the archive), one
/// moved where the search will not look; the winter folder goes into a deep tree. A check records
/// every original missing, and the editor shows Missing originals.
fn scene(name: &str) -> Scene {
    let dir = temp_dir(&format!("resolve-missing-{name}"))
        .canonicalize()
        .unwrap();
    let catalog = dir.join("catalog.sqlite");
    drop(EditorService::open(&catalog).unwrap());
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = Client {
        id: owner.register(),
        owner: owner.clone(),
    };
    let lake = dir.join("card").join("2026-09-12 Lake");
    let winter = dir.join("card").join("2025 Winter");
    let named = [
        ("DSC_0001.jpg", "orientation-1.jpg", &lake),
        ("DSC_0002.jpg", "orientation-2.jpg", &lake),
        ("DSC_0003.jpg", "orientation-3.jpg", &lake),
        ("DSC_0004.jpg", "orientation-4.jpg", &lake),
        ("DSC_0005.jpg", "orientation-5.jpg", &lake),
        ("W_0001.jpg", "orientation-6.jpg", &winter),
        ("W_0002.jpg", "orientation-7.jpg", &winter),
    ];
    let mut assets = Vec::new();
    let mut originals = Vec::new();
    for (file, fixture, folder) in named {
        let path = copy(fixture, &folder.join(file));
        assets.push((file.to_owned(), client.import(&path)));
        originals.push(path);
    }
    let lake = lake.canonicalize().unwrap();
    let winter = winter.canonicalize().unwrap();
    let archive = dir.join("archive");
    let moved = place(
        &originals[0],
        &archive.join("2026").join("Lake").join("DSC_0001.jpg"),
    );
    // Another picture under the rewritten one's name.
    fs::remove_file(&originals[1]).unwrap();
    let rewritten = copy(
        "orientation-8.jpg",
        &archive.join("2026").join("Lake").join("DSC_0002.jpg"),
    );
    let copies = [
        place(&originals[2], &archive.join("a").join("DSC_0003.jpg")),
        copy("orientation-3.jpg", &archive.join("b").join("DSC_0003.jpg")),
    ];
    // Deleted from the card; a copy survives outside the archive, under another name.
    let rescued = copy("orientation-4.jpg", &dir.join("rescued").join("kept.jpg"));
    fs::remove_file(&originals[3]).unwrap();
    // Moved where the search will not look.
    place(&originals[4], &dir.join("elsewhere").join("DSC_0005.jpg"));
    // The winter folder, deep in a tree big enough that a search of it is still walking when it is
    // stopped.
    let tree = dir.join("tree");
    for index in 0..3000 {
        fs::create_dir_all(
            tree.join(format!("{}", index / 100))
                .join(format!("{index}")),
        )
        .unwrap();
    }
    place(
        &originals[5],
        &tree.join("29").join("2999").join("W_0001.jpg"),
    );
    place(
        &originals[6],
        &tree.join("29").join("2999").join("W_0002.jpg"),
    );
    fs::remove_dir(&winter).unwrap();
    let every: Vec<&AssetId> = assets.iter().map(|(_, asset)| asset).collect();
    let checked = client.ok(
        "source.check",
        json!({"targets": {"kind": "assets", "asset_ids": every}}),
    );
    let checked = client.settle(&checked["job_id"]);
    assert_eq!(checked["status"], "ready", "{checked}");

    let (mut editor, _) = Editor::new(Boot {
        owner,
        join,
        live_server: None,
        config: Config::default(),
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    let _ = editor.update(Message::Select(SelectMessage::Source(
        ViewSource::MissingOriginals,
    )));
    evaluate(&mut editor);
    Scene {
        dir,
        editor,
        client,
        lake,
        archive: archive.canonicalize().unwrap(),
        winter,
        tree: tree.canonicalize().unwrap(),
        assets,
        moved,
        copies,
        rewritten,
        rescued,
    }
}

fn finish(scene: Scene) {
    let Scene {
        mut editor, dir, ..
    } = scene;
    editor.owner.stop();
    editor.owner_join.take().unwrap().join().unwrap();
    drop(editor);
    let _ = fs::remove_dir_all(dir);
}

fn missing(message: MissingMessage) -> Message {
    Message::Select(SelectMessage::Missing(message))
}

/// Run the Select shell's evaluation in flight, as its tasks would, then Missing originals' read of
/// the list it asked for.
fn evaluate(editor: &mut Editor) {
    let query = editor
        .select
        .state
        .query
        .clone()
        .expect("a query in flight");
    let serial = editor.select.serial;
    let viewed = evaluate_now(&editor.owner, editor.client, &query);
    let faceted = facets_now(&editor.owner, editor.client, &query);
    let _ = editor.update(Message::Select(SelectMessage::Viewed {
        serial,
        result: viewed,
    }));
    let _ = editor.update(Message::Select(SelectMessage::Faceted {
        serial,
        result: faceted,
    }));
    read_list(editor);
}

/// Answer the read of the list Missing originals has in flight, as its task would.
fn read_list(editor: &mut Editor) {
    let state = &editor.select.state.missing;
    assert!(state.reading, "Missing originals asked for its list");
    let serial = state.read_for.expect("the evaluation it was read for");
    let result = missing_now(&editor.owner, editor.client);
    let _ = editor.update(missing(MissingMessage::Listed { serial, result }));
    assert!(!editor.select.state.missing.reading);
}

/// Read the running jobs as the board's wakes would, until none runs.
fn poll_until_ended(editor: &mut Editor) {
    luxforge_testbase::wait_for("Missing originals' jobs to end", || {
        let (search, locate) = editor.select.state.missing.running_jobs();
        if search.is_none() && locate.is_none() {
            return Some(());
        }
        let read = |job: String| {
            let record = job_now(&editor.owner, editor.client, &job);
            (job, record)
        };
        let (search, locate) = (search.map(read), locate.map(read));
        let _ = editor.update(missing(MissingMessage::Polled { search, locate }));
        None
    });
}

/// Find in a folder… on `group`, answering the dialog with `root`, as its task would, and read it
/// until it ends. Answers the params the desktop sent.
fn find(editor: &mut Editor, group: &Path, root: &Path) -> Value {
    let _ = editor.update(missing(MissingMessage::FindIn {
        group: group.to_owned(),
        root: Some(root.to_owned()),
    }));
    assert_eq!(
        editor.select.state.missing.searches[group].status,
        SearchStatus::Starting
    );
    let params = model::find_params(root, group);
    let result = find_now(&editor.owner, editor.client, params.clone());
    let _ = editor.update(missing(MissingMessage::Finding {
        group: group.to_owned(),
        result,
    }));
    params
}

fn listed(editor: &Editor) -> &MissingOriginals {
    editor.select.state.missing.list.as_ref().expect("the list")
}

/// Missing originals lists what `source.missing` answers an independent client; a search's rows
/// are, row for row, what the same `source.find` answers it; Choose… picks one of several
/// identical files; Relink commits exactly the verified pairs as one library change, and the
/// others are left as they were; Stop search changes nothing; Locate… verifies before it
/// changes anything, and a file of other bytes changes nothing.
#[test]
fn resolve_missing_from_the_desktop_is_the_apis() {
    let mut scene = scene("desktop");
    let client = &scene.client;

    // The groups are the API's.
    assert!(scene.editor.missing_shown());
    let answered: MissingOriginals =
        serde_json::from_value(client.ok("source.missing", json!({}))).unwrap();
    assert_eq!(listed(&scene.editor), &answered);
    assert_eq!(answered.count, 7);
    let model = &scene.editor.workspace.select.missing;
    assert!(model.shown);
    assert_eq!(model.groups.len(), 2);
    assert_eq!(
        scene.editor.workspace.select.title.summary,
        "7 in the catalog"
    );

    // Find in a folder… on the lake group: the request is the one a JSON client sends, and each
    // row is what that client is answered.
    let params = find(&mut scene.editor, &scene.lake, &scene.archive);
    assert_eq!(
        params,
        json!({"search_root": scene.archive, "source_folder": scene.lake})
    );
    let journal = client.journal();
    poll_until_ended(&mut scene.editor);
    let search = &scene.editor.select.state.missing.searches[&scene.lake];
    assert_eq!(search.status, SearchStatus::Ended);
    let theirs = client.ok("source.find", params);
    let theirs = client.settle(&theirs["job_id"]);
    assert_eq!(theirs["status"], "ready", "{theirs}");
    assert_eq!(
        serde_json::to_value(&search.rows).unwrap(),
        theirs["result"]["rows"],
        "every row's result is the API's"
    );
    let result = |name: &str| {
        search
            .rows
            .iter()
            .find(|row| row.file_name == name)
            .map(|row| row.result.clone())
            .unwrap()
    };
    assert_eq!(
        result("DSC_0001.jpg"),
        FindResult::Found {
            path: scene.moved.clone()
        }
    );
    assert_eq!(
        result("DSC_0002.jpg"),
        FindResult::DifferentBytes {
            path: scene.rewritten.clone()
        }
    );
    assert_eq!(
        result("DSC_0003.jpg"),
        FindResult::SeveralIdentical {
            paths: scene.copies.to_vec()
        }
    );
    assert_eq!(result("DSC_0004.jpg"), FindResult::NotFound);
    assert_eq!(result("DSC_0005.jpg"), FindResult::NotFound);
    assert_eq!(client.journal(), journal, "a search changes nothing");

    // Choose… the second copy.
    let duplicated = scene.asset("DSC_0003.jpg");
    let _ = scene
        .editor
        .update(missing(MissingMessage::Menu(Some(duplicated.clone()))));
    let _ = scene.editor.update(missing(MissingMessage::Choose {
        asset: duplicated.clone(),
        path: scene.copies[1].clone(),
    }));
    let bar = scene.editor.workspace.select.missing.bar.clone().unwrap();
    assert_eq!((bar.relink.as_str(), bar.pairs), ("Relink 2", 2));

    // The Info panel reads the selected photograph's facts.
    let moved = scene.asset("DSC_0001.jpg");
    let _ = scene
        .editor
        .update(missing(MissingMessage::Row(moved.clone())));
    let facts = facts_now(&scene.editor.owner, scene.editor.client, &moved);
    assert_eq!(
        facts.as_ref().map(|facts| facts.was.clone()),
        Ok(scene.lake.join("DSC_0001.jpg"))
    );
    let _ = scene.editor.update(missing(MissingMessage::Facts {
        asset: moved.clone(),
        result: facts,
    }));
    let MissingInfo::One(info) = &scene.editor.workspace.select.missing.info else {
        panic!("the Info panel describes the row");
    };
    assert_eq!(info.rows[4].1, "The Original alone");

    // Relink sends exactly the verified pairs: the found file and the chosen copy.
    let pairs = model::relink_pairs(&scene.editor.select.state.missing);
    assert_eq!(
        serde_json::to_value(&pairs).unwrap(),
        json!([
            {"asset_id": moved, "path": scene.moved},
            {"asset_id": duplicated, "path": scene.copies[1]},
        ])
    );
    let _ = scene.editor.update(missing(MissingMessage::Relink));
    assert!(scene.editor.select.state.missing.relinking);
    let result = relink_now(
        &scene.editor.owner,
        scene.editor.client,
        model::relink_params(&pairs, &request()),
    );
    let _ = scene.editor.update(missing(MissingMessage::Relinked {
        pairs: pairs.iter().map(|pair| pair.asset_id.clone()).collect(),
        result,
    }));
    assert!(
        scene.editor.status.text.starts_with("Relinked 2 originals"),
        "{}",
        scene.editor.status.text
    );
    let client = &scene.client;
    assert_eq!(client.locator(&moved), scene.moved);
    assert_eq!(client.locator(&duplicated), scene.copies[1]);
    for name in ["DSC_0002.jpg", "DSC_0004.jpg", "DSC_0005.jpg"] {
        assert_eq!(
            client.locator(&scene.asset(name)),
            scene.lake.join(name),
            "{name} is left as it was"
        );
    }
    let changes = client.journal()["changes"].as_array().unwrap().clone();
    let last = changes.last().unwrap();
    assert_eq!(last["method"], "source.relink");
    assert_eq!(last["label"], "Relinked 2 originals");
    // The list is read again, without the two relinked.
    read_list(&mut scene.editor);
    assert_eq!(listed(&scene.editor).count, 5);
    let rows: Vec<String> = scene.editor.select.state.missing.searches[&scene.lake]
        .rows
        .iter()
        .map(|row| row.file_name.clone())
        .collect();
    assert_eq!(rows, ["DSC_0002.jpg", "DSC_0004.jpg", "DSC_0005.jpg"]);

    // Stop search: a search of the deep tree stopped while it walks changes nothing, and the group
    // goes back to its header.
    let journal = client.journal();
    let _ = find(&mut scene.editor, &scene.winter, &scene.tree);
    let SearchStatus::Running { job } = scene.editor.select.state.missing.searches[&scene.winter]
        .status
        .clone()
    else {
        panic!("the search runs");
    };
    let _ = scene.editor.update(missing(MissingMessage::Stop));
    let stopped = cancel_now(&scene.editor.owner, scene.editor.client, &job);
    let _ = scene
        .editor
        .update(missing(MissingMessage::Stopped(stopped)));
    poll_until_ended(&mut scene.editor);
    assert!(
        !scene
            .editor
            .select
            .state
            .missing
            .searches
            .contains_key(&scene.winter),
        "stopped while walking the tree: {}",
        scene.editor.status.text
    );
    assert!(scene.editor.status.text.starts_with("Stopped searching"));
    let client = &scene.client;
    assert_eq!(client.journal(), journal, "stopping changes nothing");
    for name in ["W_0001.jpg", "W_0002.jpg"] {
        assert_eq!(client.locator(&scene.asset(name)), scene.winter.join(name));
    }

    // Locate… a file of other bytes: refused after its fingerprint is read, nothing changed.
    let rewritten = scene.asset("DSC_0002.jpg");
    locate(
        &mut scene.editor,
        &rewritten,
        "DSC_0002.jpg",
        &scene.rewritten,
    );
    assert!(
        scene
            .editor
            .status
            .text
            .starts_with("Could not locate DSC_0002.jpg"),
        "{}",
        scene.editor.status.text
    );
    let client = &scene.client;
    assert_eq!(client.journal(), journal);
    assert_eq!(client.locator(&rewritten), scene.lake.join("DSC_0002.jpg"));

    // Locate… the rescued copy of the deleted one: verified, one library change, and its row goes.
    let deleted = scene.asset("DSC_0004.jpg");
    locate(&mut scene.editor, &deleted, "DSC_0004.jpg", &scene.rescued);
    let client = &scene.client;
    assert_eq!(client.locator(&deleted), scene.rescued);
    let changes = client.journal()["changes"].as_array().unwrap().clone();
    assert_eq!(changes.last().unwrap()["method"], "source.locate");
    assert!(scene.editor.select.state.missing.row(&deleted).is_none());
    read_list(&mut scene.editor);
    assert_eq!(listed(&scene.editor).count, 4);
    finish(scene);
}

/// Locate… on a row, answering the dialog with `path`, as its tasks would, until the job ends.
fn locate(editor: &mut Editor, asset: &AssetId, file_name: &str, path: &Path) {
    let _ = editor.update(missing(MissingMessage::LocatePicked {
        asset: asset.clone(),
        file_name: file_name.into(),
        from: LocateFrom::Missing,
        path: Some(path.to_owned()),
    }));
    let params = model::locate_params(asset, path, &request());
    assert_eq!(
        params["asset_id"],
        json!(asset),
        "the request names the photograph"
    );
    assert_eq!(params["path"], json!(path));
    let result = locate_now(&editor.owner, editor.client, params);
    let _ = editor.update(missing(MissingMessage::Locating(result)));
    poll_until_ended(editor);
    assert!(editor.select.state.missing.locating.is_none());
}

/// Develop's Original not found notice offers Locate original… for the open photograph; the file
/// chosen is verified and relinked through `source.locate`, and the photograph is opened again from
/// it through the Open path.
#[test]
fn resolve_missing_locate_original_in_develop_reopens_the_photograph() {
    let mut scene = scene("develop");
    let deleted = scene.asset("DSC_0004.jpg");
    let state = scene
        .client
        .ok("asset.state", json!({ "asset_id": deleted }));
    scene.editor.document.state = Some(serde_json::from_value(state).unwrap());
    let _ = scene
        .editor
        .update(Message::Select(SelectMessage::Switch(Shown::Develop)));
    scene.editor.presentation.render_error = Some(luxforge_core::Error::new(
        luxforge_core::ErrorKind::SourceUnavailable,
        "the original DSC_0004.jpg is missing",
    ));
    let _ = scene.editor.update(Message::Select(SelectMessage::Missing(
        MissingMessage::Menu(None),
    )));
    let notice = scene
        .editor
        .workspace
        .canvas
        .notices
        .iter()
        .find(|notice| notice.title == "Original not found")
        .expect("the notice")
        .clone();
    assert_eq!(
        notice.actions,
        vec![(
            "Locate original\u{2026}".to_owned(),
            NoticeAction::LocateOriginal
        )]
    );
    // The dialog answers with the rescued copy.
    let _ = scene.editor.update(missing(MissingMessage::LocateOriginal));
    assert!(
        scene.editor.view_state.picker_open,
        "the file dialog is open"
    );
    let _ = scene.editor.update(missing(MissingMessage::LocatePicked {
        asset: deleted.clone(),
        file_name: "DSC_0004.jpg".into(),
        from: LocateFrom::Develop,
        path: Some(scene.rescued.clone()),
    }));
    assert!(!scene.editor.view_state.picker_open);
    let params = model::locate_params(&deleted, &scene.rescued, &request());
    let result = locate_now(&scene.editor.owner, scene.editor.client, params);
    let _ = scene
        .editor
        .update(missing(MissingMessage::Locating(result)));
    let opened = scene.editor.activity.requested;
    poll_until_ended(&mut scene.editor);
    assert_eq!(scene.client.locator(&deleted), scene.rescued);
    assert_eq!(
        scene.editor.activity.requested,
        opened + 1,
        "the located photograph is opened again: {}",
        scene.editor.status.text
    );
    assert!(scene.editor.busy, "the Open path is under way");
    finish(scene);
}

/// A search is read with `job.read` once as soon as it is named, and again each time long-running
/// work reads the activity board — which its watch wakes it to do as the search reports each row
/// and when it ends — one read at a time, a board read while one is out asking for one more. No
/// timer asks after it.
#[test]
fn resolve_missing_follows_a_search_through_the_activity_board() {
    let mut scene = scene("followed");
    let _ = find(&mut scene.editor, &scene.lake, &scene.archive);
    let editor = &mut scene.editor;
    assert!(
        editor.select.state.missing.polling,
        "read as soon as it is named"
    );
    let _ = editor.missing_followed();
    assert!(
        editor.select.state.missing.poll_again,
        "a board read while that read is out asks for one more"
    );
    let mut reads = 0;
    luxforge_testbase::wait_for("the search to end", || {
        let (search, _) = editor.select.state.missing.running_jobs();
        let Some(job) = search else {
            return Some(());
        };
        if editor.select.state.missing.polling {
            // The read that is out answers, as its task would.
            reads += 1;
            let record = job_now(&editor.owner, editor.client, &job);
            let _ = editor.update(missing(MissingMessage::Polled {
                search: Some((job, record)),
                locate: None,
            }));
        } else {
            // What long-running work calls after each read of the board.
            let _ = editor.missing_followed();
        }
        None
    });
    let state = &editor.select.state.missing;
    assert!(reads >= 2, "named, then after the board read");
    assert_eq!(state.searches[&scene.lake].status, SearchStatus::Ended);
    assert!(!state.polling && !state.poll_again);
    finish(scene);
}
