//! Developing picks and the Develop folder against a real owner, over a folder of copied fixture
//! JPEGs the index lane reads: Develop N's confirmation from `pick.plan`, Escape changing nothing,
//! Develop sending the `pick.develop` an agent writes and opening Develop on the first photograph
//! it developed, in the folder holding them as the Develop folder; `→` and `←` drawing the
//! photograph's cached large preview in the update of the key and its render after it; an answer
//! or decode for another photograph arriving after a move never drawn for this one, and a preview
//! of another entry withdrawn; a move refused while a draft is open; a double-click and a file
//! opened choosing their folder, a folder viewed in Select chosen and kept, another client's
//! change reaching the strip; and the keys. Each owner call runs exactly as its task would,
//! through the `*_now` functions the tasks run, and its answer is handed back as the task's
//! message; requests are compared with what an independent JSON client writes and answers.
use super::*;
use crate::{
    Config,
    app::{
        Boot,
        loupe_frames::{Decoded, Slot},
        message::{preview::PreviewMessage, sync::SyncMessage},
        select::refresh_now,
        select_owner_tests::{answer_reads, evaluate, read_rows},
        testing::{
            attach_log, descriptors, events, hold_slider, let_go, logged, patch_control,
            run_commit, slide,
        },
    },
    state::{develop::path_title, select::ReadSource},
};
use crate::{app::message::select::SelectMessage, state::select::Shown};
use iced::{
    Size,
    event::Status,
    keyboard::{Event as KeyEvent, Key, Modifiers, key::Named, key::Physical},
};
use luxforge_core::{
    ApiRequest, Cancel, EditorService, EntryId, JobId,
    catalog_types::{
        FolderChoice, PreviewAnswer, PreviewInfo, PreviewOrigin, PreviewTier, ViewRows, ViewSource,
        ViewSummary,
    },
    decode_preview,
};
use luxforge_test_macros::slow;
use luxforge_testbase::{
    paths::{fixture, temp_dir},
    wait_until,
};
use std::{fs, path::PathBuf, sync::atomic::AtomicU64};

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
                    id: format!("develop-client-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
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

    fn mutation(&self) -> Value {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        json!({"request_id": format!("agent-{}", NEXT.fetch_add(1, Ordering::Relaxed)), "actor": "agent"})
    }
}

/// The scratch catalog, the folder of copied fixtures the editor views, and an independent client.
struct Scene {
    dir: PathBuf,
    editor: Editor,
    client: Client,
    files: Vec<PathBuf>,
}

/// Four fixture JPEGs copied into `photos/2026-09-12 Lake`, read by the index lane and viewed in
/// Select, every row read.
fn scene(name: &str) -> Scene {
    let dir = temp_dir(&format!("develop-{name}")).canonicalize().unwrap();
    let catalog = dir.join("catalog.sqlite");
    drop(EditorService::open(&catalog).unwrap());
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = Client {
        id: owner.register(),
        owner: owner.clone(),
    };
    let folder = dir.join("photos").join("2026-09-12 Lake");
    fs::create_dir_all(&folder).unwrap();
    let files: Vec<PathBuf> = (1..=4)
        .map(|at| {
            let path = folder.join(format!("DSC_000{at}.jpg"));
            fs::copy(fixture(&format!("s0/orientation-{at}.jpg")), &path).unwrap();
            path
        })
        .collect();
    let (mut editor, _) = Editor::new(Boot {
        owner,
        join,
        live_server: None,
        config: Config::default(),
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    answer_reads(&mut editor);
    let source = ReadSource::Folder(folder.clone());
    let _ = editor.update(Message::Select(SelectMessage::Read(source.clone())));
    let job = refresh_now(&editor.owner, editor.client, &source);
    let _ = editor.update(Message::Select(SelectMessage::Reading(job.clone())));
    let job = job.unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let record = runtime.block_on(async {
        tokio::time::timeout(luxforge_testbase::HANG, async {
            let mut after = None;
            loop {
                let (change, record) =
                    crate::app::job_reads::wait(&editor.owner, editor.client, &job, after)
                        .await
                        .unwrap();
                after = Some(change);
                if !matches!(record["status"].as_str(), Some("queued" | "running")) {
                    break record;
                }
            }
        })
        .await
        .expect("the folder's index.refresh to end")
    });
    assert_eq!(record["status"], "ready", "{record}");
    let _ = editor.update(Message::Select(SelectMessage::ReadWatched {
        job,
        result: Ok(record),
    }));
    evaluate(&mut editor);
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        1000.0, 700.0,
    ))));
    read_rows(&mut editor);
    assert_eq!(summary(&editor).count, 4);
    Scene {
        dir,
        editor,
        client,
        files,
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

fn summary(editor: &Editor) -> &ViewSummary {
    editor.select.state.summary.as_ref().expect("a view")
}

fn send(editor: &mut Editor, message: DevelopMessage) {
    let _ = editor.update(Message::Develop(message));
}

/// The files at `paths` picked by the independent client, and the view read again.
fn pick(scene: &mut Scene, paths: &[PathBuf]) {
    scene.client.ok(
        "pick.set",
        json!({"targets": {"kind": "paths", "paths": paths}, "picked": true,
            "mutation": scene.client.mutation()}),
    );
    let query = scene.editor.select.state.query.clone().unwrap();
    let _ = scene.editor.evaluate(query);
    evaluate(&mut scene.editor);
    read_rows(&mut scene.editor);
}

/// Answer the confirmation's `pick.plan` as its task would. A view the grid's previews left stale
/// meanwhile is read again by the owner itself, so the plan is never refused for it. Answers the
/// plan.
fn answer_plan(editor: &mut Editor) -> DevelopPlan {
    assert!(editor.develop.state.planning, "{}", editor.status.text);
    let serial = editor.develop.plan_serial;
    let result = plan_now(&editor.owner, editor.client);
    let plan = result.as_ref().expect("a plan").0.clone();
    send(editor, DevelopMessage::Planned { serial, result });
    plan
}

/// Run the Develop the confirmation sent, as its tasks would, to its end. Answers the job's record.
fn run_develop(editor: &mut Editor) -> Value {
    let params = editor.develop.last.request.clone().expect("a Develop sent");
    let job = develop_now(&editor.owner, editor.client, params).expect("a Develop started");
    send(editor, DevelopMessage::Started(Ok(job.clone())));
    let record = ended_now(&editor.owner, editor.client, &job);
    send(
        editor,
        DevelopMessage::Ended {
            job,
            result: record.clone(),
        },
    );
    record.expect("the job's record")
}

/// Run the open of the move in flight, as its task would.
fn run_open(editor: &mut Editor) {
    let switch = editor.develop.switch.clone().expect("a move in flight");
    let result = tasks::open_photograph(
        &editor.owner,
        editor.client,
        &switch.asset,
        switch.generation,
        &editor.open_generation,
        None,
    );
    let _ = editor.update(Message::Sync(SyncMessage::ImportRefreshed(
        switch.generation,
        result.map(Box::new),
    )));
}

/// Deliver the preview worker's frames until the open photograph's own frame is on screen.
fn present(editor: &mut Editor) {
    wait_until("the photograph's frame on screen", || {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        editor.presentation.presented_entry.is_some() && !editor.presentation.queue.is_busy()
    });
}

/// Read and decode the large previews Develop wants until each of `assets` is held as its rendered
/// tier: each read batch run against the owner, the lane's queued renders read again until they
/// land, and each planned decode made from the cached file, as the worker would.
fn decode(editor: &mut Editor, assets: &[AssetId]) {
    decode_until(editor, assets, |held| !held.picture.stand_in);
}

/// [`decode`] until each of `assets` holds a picture `ready` accepts.
fn decode_until(
    editor: &mut Editor,
    assets: &[AssetId],
    ready: impl Fn(&loupe_frames::HeldPhoto<'_>) -> bool,
) {
    wait_until("the large previews decoded", || {
        for batch in std::mem::take(&mut editor.develop.reads) {
            let answers = loupe_frames::read(&editor.owner, editor.client, batch);
            send(
                editor,
                DevelopMessage::Frames(LoupeFramesMessage::Read(answers)),
            );
        }
        let again = editor.develop.frames.woken(true);
        if let Some(batch) = again {
            let answers = loupe_frames::read(&editor.owner, editor.client, batch);
            send(
                editor,
                DevelopMessage::Frames(LoupeFramesMessage::Read(answers)),
            );
        }
        for decode in editor.develop.frames.planned().to_vec() {
            if !assets.iter().any(|asset| item(asset) == decode.slot.item) {
                continue;
            }
            let result = decode_preview(&decode.path, decode.side, &Cancel::never())
                .map_err(|error| error.to_string());
            editor.develop.frames.adopt(Decoded { decode, result });
        }
        assets.iter().all(|asset| {
            editor
                .develop
                .frames
                .photo(&Slot::frame(item(asset)))
                .is_some_and(|held| ready(&held))
        })
    });
}

/// Read the Develop folder into the strip, as the read in flight's task would.
fn read_strip(editor: &mut Editor) {
    let serial = editor.develop.strip_serial;
    assert_eq!(
        editor.develop.reading,
        Some(serial),
        "a read of the Develop folder in flight"
    );
    let folder = editor.develop_folder().cloned().expect("a folder to read");
    let result = folder_strip_now(&editor.owner, &folder);
    send(editor, DevelopMessage::StripRead { serial, result });
}

/// Find and read the folder holding the folders `filed`, as the task would.
fn find_folder(editor: &mut Editor, filed: &[CatalogFolderId]) {
    let serial = editor.develop.strip_serial;
    assert_eq!(
        editor.develop.reading,
        Some(serial),
        "the photographs' folder being found"
    );
    let result = developed_folder_now(&editor.owner, filed);
    send(editor, DevelopMessage::FolderFound { serial, result });
}

/// The catalog folders a Develop's job record says its choices named, as its report names them.
fn filed_in(record: &Value) -> Vec<CatalogFolderId> {
    record["result"]["developed"]
        .as_array()
        .expect("a Develop's report")
        .iter()
        .map(|developed| {
            serde_json::from_value(developed["chosen_folder_id"].clone())
                .expect("each one's folder")
        })
        .collect()
}

/// Four files picked and developed through Develop N into its proposed folder, which becomes the
/// Develop folder, and the strip's first photograph open in Develop with its frame on screen.
/// Answers the strip's photographs.
fn developed(name: &str) -> (Scene, Vec<AssetId>) {
    let mut scene = scene(name);
    let files = scene.files.clone();
    pick(&mut scene, &files);
    let editor = &mut scene.editor;
    editor.develop.frames.paused = true;
    send(editor, DevelopMessage::Open);
    answer_plan(editor);
    send(editor, DevelopMessage::Confirm);
    let record = run_develop(editor);
    assert_eq!(record["status"], "ready", "{record}");
    find_folder(editor, &filed_in(&record));
    run_open(editor);
    if editor.develop.state.strip.as_ref().unwrap().active != Some(0) {
        drop(editor.switch_to(0));
        run_open(editor);
    }
    present(editor);
    let set = strip_assets(editor);
    assert_eq!(set.len(), 4);
    (scene, set)
}

/// Develop N reads the plan of the picks in view, which is what an agent's own view of the same
/// source plans; Escape closes the confirmation and nothing changes; typing renames the proposed
/// folder, and Develop sends the `pick.develop` an agent writes for it, whose job's end opens
/// Develop on the first photograph it developed, in the folder confirmed as the Develop folder. A
/// later Develop into the same place holds the place, with the photographs developed before.
#[test]
#[slow]
fn filmstrip_develop_n_develops_the_picks_into_the_confirmed_folder() {
    let mut scene = scene("develop-n");
    let picked = scene.files[..3].to_vec();
    pick(&mut scene, &picked);
    assert_eq!(summary(&scene.editor).picked, 3);
    let query = summary(&scene.editor).query.clone();
    let editor = &mut scene.editor;
    editor.develop.frames.paused = true;
    send(editor, DevelopMessage::Open);
    let plan = answer_plan(editor);
    // An agent viewing the same source and asking for the plan of its picks is answered the same.
    scene
        .client
        .ok("browse.view", serde_json::to_value(&query).unwrap());
    let agents: DevelopPlan =
        serde_json::from_value(scene.client.ok("pick.plan", json!({}))).unwrap();
    assert_eq!(plan, agents);
    assert_eq!(plan.count, 3);
    assert_eq!(plan.events.len(), 1);
    let proposed = match &plan.events[0].folder {
        FolderChoice::Place { name } => name.clone(),
        other => panic!("the event's place is proposed: {other:?}"),
    };
    let model = editor.workspace.develop.confirm.clone().expect("open");
    assert_eq!(model.title, "Develop 3 picks");
    assert_eq!(model.events[0].field, proposed);
    assert!(!model.events[0].folders.is_empty());
    assert_eq!(model.refusal, None);

    // Escape changes nothing: no request, the picks and the journal as they were.
    let journal = scene.client.ok("library.journal", json!({"limit": 500}));
    let picks = scene.client.ok("pick.list", json!({}));
    let escape = key(editor, Key::Named(Named::Escape), Modifiers::empty());
    assert!(matches!(
        escape,
        Some(Message::Develop(DevelopMessage::Cancel))
    ));
    let _ = editor.update(escape.unwrap());
    assert!(editor.develop.state.confirm.is_none());
    assert!(editor.develop.last.request.is_none());
    assert_eq!(editor.status.text, "Nothing developed");
    assert_eq!(
        scene.client.ok("library.journal", json!({"limit": 500})),
        journal
    );
    assert_eq!(scene.client.ok("pick.list", json!({})), picks);

    // Typing renames the place; Return develops.
    send(editor, DevelopMessage::Open);
    answer_plan(editor);
    send(
        editor,
        DevelopMessage::Name {
            event: 0,
            text: "Lake trip".into(),
        },
    );
    let enter = key(editor, Key::Named(Named::Enter), Modifiers::empty());
    assert!(matches!(
        enter,
        Some(Message::Develop(DevelopMessage::Confirm))
    ));
    let _ = editor.update(enter.unwrap());
    assert!(editor.develop.state.confirm.is_none());
    assert_eq!(
        editor.develop.state.developing.as_ref().map(|d| d.total),
        Some(3)
    );
    let sent = editor.develop.last.request.clone().unwrap();
    assert_eq!(
        sent["into"],
        json!([{"event_id": plan.events[0].event_id, "folder": {"kind": "place", "name": "Lake trip"}}])
    );
    assert_eq!(sent["mutation"]["actor"], "desktop");
    assert!(sent.get("use_copies").is_none() && sent.get("confirm_removable").is_none());
    let record = run_develop(editor);
    assert_eq!(record["status"], "ready", "{record}");
    let report: DevelopReport = serde_json::from_value(record["result"].clone()).unwrap();
    assert_eq!(report.developed.len(), 3);
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    // The catalog holds them under the place confirmed, and their picks are cleared.
    let folders: CatalogFolders =
        serde_json::from_value(scene.client.ok("folder.list", json!({}))).unwrap();
    let lake = folders
        .folders
        .iter()
        .find(|folder| folder.name == "Lake trip")
        .expect("the confirmed place")
        .clone();
    assert_eq!(
        scene.client.ok("pick.list", json!({}))["picks"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    // Develop opens on the first photograph it developed, its photographs in the strip until their
    // folder is found; then the confirmed folder is the Develop folder, read into the strip.
    assert!(!editor.select_shown());
    let first = report.developed[0].asset_id.clone();
    let strip = editor.develop.state.strip.clone().expect("a strip");
    assert!(strip.reading);
    assert_eq!(
        strip
            .photos
            .iter()
            .map(|photo| &photo.asset_id)
            .collect::<Vec<_>>(),
        report
            .developed
            .iter()
            .map(|developed| &developed.asset_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(strip.active, Some(0));
    assert!(editor.filmstrip_shown());
    let switch = editor
        .develop
        .switch
        .clone()
        .expect("the first photograph opening");
    assert_eq!(switch.asset, first);
    assert_eq!(
        editor.workspace.title.file_name.as_deref(),
        Some(strip.photos[0].name.as_str())
    );
    let filed: Vec<CatalogFolderId> = report
        .developed
        .iter()
        .map(|developed| developed.chosen_folder_id.clone())
        .collect();
    find_folder(editor, &filed);
    let strip = editor
        .develop
        .state
        .strip
        .clone()
        .expect("the folder's strip");
    assert!(!strip.reading);
    // The place they went into, never the day folder inside it.
    let held = strip.folder.clone().expect("a folder");
    assert_eq!(editor.develop_folder(), Some(&held));
    assert_eq!(held, lake.id, "{:?}", strip.path);
    assert_eq!(strip.path.last().map(String::as_str), Some("Lake trip"));
    assert_eq!(strip.photos.len(), 3);
    assert_eq!(strip.active, strip.position(&first));
    let model = editor
        .workspace
        .develop
        .strip
        .clone()
        .expect("the filmstrip");
    assert_eq!(model.title, path_title(&strip.path));
    assert_eq!((model.total, model.active), (3, strip.active));
    run_open(editor);
    assert_eq!(
        editor.document.state.as_ref().map(|state| &state.asset.id),
        Some(&first)
    );
    assert!(editor.develop.switch.is_none());
    present(editor);
    assert_eq!(
        editor.gesture_refusal(Starting::Action),
        None,
        "the controls enable"
    );

    // Another pick developed into the same place: Develop holds the place, with the photographs
    // developed into it before.
    drop(editor.update(Message::Select(SelectMessage::Switch(Shown::Select))));
    let more = scene.files[3..].to_vec();
    pick(&mut scene, &more);
    let editor = &mut scene.editor;
    send(editor, DevelopMessage::Open);
    let plan = answer_plan(editor);
    assert!(
        matches!(
            &plan.events[0].folder,
            FolderChoice::Existing { folder_id, .. } if *folder_id == lake.id
        ),
        "{plan:?}"
    );
    send(editor, DevelopMessage::Confirm);
    let record = run_develop(editor);
    assert_eq!(record["status"], "ready", "{record}");
    find_folder(editor, &filed_in(&record));
    let strip = editor
        .develop
        .state
        .strip
        .clone()
        .expect("the place's strip");
    assert_eq!(strip.folder.as_ref(), Some(&lake.id));
    assert_eq!(editor.develop_folder(), Some(&lake.id));
    assert_eq!(strip.photos.len(), 4);
    finish(scene);
}

/// `→` draws the next photograph's cached large preview in the update of the key, labelled a
/// preview, with the controls disabled and nothing of the photograph before it on screen; its open
/// then shows the same photograph at the preview's entry, and its render replaces the preview.
/// `←` goes back the same way. Nothing prepares a neighbour.
#[test]
#[slow]
fn filmstrip_moves_through_the_set_drawing_the_cached_preview_first() {
    let (mut scene, set) = developed("moves");
    let editor = &mut scene.editor;
    decode(editor, &set[1..3]);
    let version = editor.presentation.presenter.photo_version();
    let right = key(editor, Key::Named(Named::ArrowRight), Modifiers::empty());
    assert!(matches!(
        right,
        Some(Message::Develop(DevelopMessage::Step(1)))
    ));
    let _ = editor.update(right.unwrap());
    // In the key's own update: the preview of the photograph moved to, on the surface.
    let preview = editor
        .develop
        .state
        .preview
        .clone()
        .expect("a cached preview");
    assert_eq!(preview.asset, set[1]);
    assert_eq!(
        preview.version,
        editor.presentation.presenter.photo_version()
    );
    assert!(preview.version > version);
    assert!(
        editor
            .presentation
            .presenter
            .photo_for(editor.presentation.presented_content)
            .is_some()
    );
    assert!(
        editor.document.state.is_none(),
        "the photograph before it is closed"
    );
    assert!(editor.presentation.presented_entry.is_none());
    assert_eq!(editor.workspace.status.render, "Cached preview");
    assert!(
        editor
            .status
            .text
            .ends_with("cached preview while the original prepares")
    );
    assert!(
        editor.gesture_refusal(Starting::Action).is_some(),
        "the controls wait"
    );
    assert_eq!(editor.develop.state.strip.as_ref().unwrap().active, Some(1));
    // No neighbour is prepared: the one open in flight is the photograph moved to.
    assert_eq!(editor.develop.switch.as_ref().unwrap().asset, set[1]);

    run_open(editor);
    let state = editor.document.state.clone().expect("open");
    assert_eq!(state.asset.id, set[1]);
    assert_eq!(preview.entry.as_ref(), Some(&state.current_entry.id));
    assert!(
        editor.develop.state.preview.is_some(),
        "the preview of the entry the open shows stays until the render"
    );
    present(editor);
    assert!(
        editor.develop.state.preview.is_none(),
        "the render replaced it"
    );
    assert_eq!(
        editor.presentation.presented_entry.as_ref(),
        Some(&state.current_entry.id)
    );

    decode(editor, &set[..1]);
    send(editor, DevelopMessage::Step(-1));
    let back = editor
        .develop
        .state
        .preview
        .clone()
        .expect("a cached preview");
    assert_eq!(back.asset, set[0]);
    run_open(editor);
    present(editor);
    assert_eq!(
        editor.document.state.as_ref().map(|state| &state.asset.id),
        Some(&set[0])
    );
    finish(scene);
}

/// A warm open's GPU picture stands over the cached preview of the photograph moved to, which stays
/// the surface's base: once the GPU presents the open photograph's content, its picture is the
/// photograph on screen, so the preview is let go of — not withdrawn from under it — and the
/// status bar stops calling the picture a cached preview.
#[test]
#[slow]
fn filmstrip_a_warm_opens_gpu_picture_replaces_the_cached_preview() {
    let (mut scene, set) = developed("warm-move");
    let editor = &mut scene.editor;
    decode(editor, &set[1..2]);
    send(editor, DevelopMessage::Step(1));
    let preview = editor
        .develop
        .state
        .preview
        .clone()
        .expect("a cached preview");
    run_open(editor);
    assert!(
        editor.develop.state.preview.is_some(),
        "until a picture of the entry is on screen"
    );
    // The GPU presents the open photograph's content over the preview, as a warm open does. The
    // test's open was queued on the reference, which has no GPU stage here; a warm open queues no
    // render, and presenting on the GPU raises the floor over anything planned before
    // (`Editor::present_on_gpu`), so that render never lands over the GPU's picture either.
    let _ = editor.presentation.cancel();
    editor.presentation.gpu_presented = Some(editor.presentation.presented_content);
    // Any message's hooks then see it.
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(
        editor.develop.state.preview.is_none(),
        "the GPU's picture replaced it"
    );
    assert_eq!(
        editor.presentation.presenter.photo_version(),
        preview.version,
        "the preview stays the surface's base under the GPU's picture"
    );
    assert_ne!(editor.workspace.status.render, "Cached preview");
    finish(scene);
}

/// A move with nothing decoded for it takes the photograph before it off the surface; a decode or
/// an open of a photograph moved past that lands after the next move is never drawn for the
/// photograph moved to, whose own preview is drawn as soon as it lands.
#[test]
#[slow]
fn filmstrip_a_stale_answer_is_never_drawn_for_another_photograph() {
    let (mut scene, set) = developed("stale");
    let editor = &mut scene.editor;
    send(editor, DevelopMessage::Step(1));
    assert!(
        editor.develop.state.preview.is_none(),
        "nothing decoded yet"
    );
    assert!(
        !editor.presentation.has_picture(),
        "the photograph before is off"
    );
    let passed = editor.develop.switch.clone().unwrap();
    assert_eq!(passed.asset, set[1]);
    send(editor, DevelopMessage::Step(1));
    assert_eq!(editor.develop.switch.as_ref().unwrap().asset, set[2]);

    // The photograph passed: its preview decodes and its open answers, both after the move.
    decode(editor, &set[1..2]);
    send(editor, DevelopMessage::Woken);
    assert!(editor.develop.state.preview.is_none());
    assert!(!editor.presentation.has_picture());
    let late = tasks::open_photograph(
        &editor.owner,
        editor.client,
        &passed.asset,
        passed.generation,
        &editor.open_generation,
        None,
    );
    let _ = editor.update(Message::Sync(SyncMessage::ImportRefreshed(
        passed.generation,
        late.map(Box::new),
    )));
    assert!(editor.document.state.is_none(), "an older open is dropped");
    assert_eq!(editor.develop.switch.as_ref().unwrap().asset, set[2]);

    // Its own preview, once decoded, is drawn while it prepares.
    decode(editor, &set[2..3]);
    send(editor, DevelopMessage::Woken);
    let preview = editor
        .develop
        .state
        .preview
        .clone()
        .expect("its own preview");
    assert_eq!(preview.asset, set[2]);
    run_open(editor);
    assert_eq!(
        editor.document.state.as_ref().map(|state| &state.asset.id),
        Some(&set[2])
    );
    finish(scene);
}

/// A preview whose entry is not the one the open shows — another client committed meanwhile — is
/// taken off the surface once the open answers, rather than standing for the entry shown.
#[test]
#[slow]
fn filmstrip_a_preview_of_another_entry_is_withdrawn() {
    let (mut scene, set) = developed("entry");
    let editor = &mut scene.editor;
    decode(editor, &set[1..2]);
    send(editor, DevelopMessage::Step(1));
    let preview = editor
        .develop
        .state
        .preview
        .as_mut()
        .expect("a cached preview");
    preview.entry = Some(EntryId::new());
    run_open(editor);
    assert_eq!(
        editor.document.state.as_ref().map(|state| &state.asset.id),
        Some(&set[1])
    );
    assert!(editor.develop.state.preview.is_none());
    assert!(!editor.presentation.has_picture());
    present(editor);
    finish(scene);
}

/// The photograph open is edited and left: its large preview, of the entry before the edit, is
/// read again, which renders its new entry ahead. Going back to it never draws the picture from
/// before the edit: the canvas is empty, saying why in `develop_preview_withheld`, until the new
/// entry's preview is decoded, which is then drawn while the original prepares, at the entry the
/// open shows.
#[test]
#[slow]
fn filmstrip_an_edited_photograph_never_shows_its_picture_from_before_the_edit() {
    let (mut scene, set) = developed("edited");
    let editor = &mut scene.editor;
    decode(editor, &set[..2]);
    let before = editor
        .document
        .state
        .as_ref()
        .unwrap()
        .current_entry
        .id
        .clone();
    let slot = Slot::frame(item(&set[0]));
    assert_eq!(
        editor.develop.frames.photo(&slot).unwrap().entry,
        Some(&before)
    );
    let _ = slide(editor, "set-basic", "exposure", 1.0);
    let _ = let_go(editor, "set-basic", "exposure");
    assert!(run_commit(editor));
    let edited = editor
        .document
        .state
        .as_ref()
        .unwrap()
        .current_entry
        .id
        .clone();
    assert_ne!(edited, before, "the commit is a new entry");
    wait_until("the edit on screen", || {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        editor.presentation.presented_entry.as_ref() == Some(&edited)
            && !editor.presentation.queue.is_busy()
    });

    send(editor, DevelopMessage::Step(1));
    assert!(
        !editor.develop.frames.photo(&slot).unwrap().current,
        "the photograph left is no longer taken as its picture"
    );
    run_open(editor);
    present(editor);
    let log = attach_log(editor);
    send(editor, DevelopMessage::Step(-1));
    assert!(
        editor.develop.state.preview.is_none(),
        "the picture from before the edit is never drawn"
    );
    assert!(!editor.presentation.has_picture(), "the canvas is empty");
    let records = logged(editor, &log);
    let withheld = events(&records, "develop_preview_withheld");
    assert_eq!(withheld.len(), 1, "{records:?}");
    assert_eq!(withheld[0]["entry_id"], json!(before));

    // Read again when it was left, its new entry's render is decoded and drawn while it prepares.
    decode_until(editor, &set[..1], |held| {
        held.current && held.entry == Some(&edited)
    });
    send(editor, DevelopMessage::Woken);
    let preview = editor
        .develop
        .state
        .preview
        .clone()
        .expect("its current picture");
    assert_eq!(preview.asset, set[0]);
    assert_eq!(preview.entry.as_ref(), Some(&edited));
    run_open(editor);
    assert_eq!(
        editor.document.state.as_ref().unwrap().current_entry.id,
        edited
    );
    assert!(
        editor.develop.state.preview.is_some(),
        "the preview of the entry the open shows stays until the render"
    );
    present(editor);
    finish(scene);
}

/// A move draws a held camera preview only when its answer says it shows the photograph's entry,
/// as an unedited photograph's does: drawn in the key's update. One the owner says does not, an
/// edited photograph's camera preview, is held but not drawn, and the canvas is empty until the
/// open's render.
#[test]
#[slow]
fn filmstrip_a_camera_preview_is_drawn_only_while_it_shows_the_entry() {
    let (mut scene, set) = developed("camera");
    let editor = &mut scene.editor;
    let path = fixture("s0/orientation-1.jpg");
    let camera = |asset: &AssetId, current: bool| PreviewAnswer::Queued {
        job_id: JobId::new(),
        fallback: Some(PreviewInfo {
            renderer: None,
            item: PreviewItem::Photo {
                asset_id: asset.clone(),
                entry_id: Some(EntryId::new()),
            },
            tier: PreviewTier::Large,
            path: path.clone(),
            width: 480,
            height: 320,
            origin: PreviewOrigin::Embedded,
            approximate: false,
            bytes: fs::metadata(&path).unwrap().len(),
            key: format!("photo:{asset}:camera:large:embedded"),
        }),
        fallback_current: current,
    };
    // The lane's answers while every render waits: the second photograph's camera preview shows
    // its entry, the third's does not.
    wait_until("the neighbours' camera previews held", || {
        for batch in std::mem::take(&mut editor.develop.reads) {
            let answers = batch
                .reads
                .iter()
                .map(|slot| {
                    let PreviewItem::Photo { asset_id, .. } = &slot.item else {
                        unreachable!("a photograph's frame")
                    };
                    let answer = if *asset_id == set[1] {
                        camera(asset_id, true)
                    } else if *asset_id == set[2] {
                        camera(asset_id, false)
                    } else {
                        PreviewAnswer::Queued {
                            job_id: JobId::new(),
                            fallback: None,
                            fallback_current: false,
                        }
                    };
                    (slot.clone(), Ok(answer))
                })
                .collect();
            let answers = loupe_frames::ReadAnswers {
                serial: batch.serial,
                answers,
            };
            send(
                editor,
                DevelopMessage::Frames(LoupeFramesMessage::Read(answers)),
            );
        }
        for decode in editor.develop.frames.planned().to_vec() {
            let result = decode_preview(&decode.path, decode.side, &Cancel::never())
                .map_err(|error| error.to_string());
            editor.develop.frames.adopt(Decoded { decode, result });
        }
        set[1..3].iter().all(|asset| {
            editor
                .develop
                .frames
                .photo(&Slot::frame(item(asset)))
                .is_some()
        })
    });

    send(editor, DevelopMessage::Step(1));
    let preview = editor
        .develop
        .state
        .preview
        .clone()
        .expect("the unedited photograph's camera preview, at once");
    assert_eq!(preview.asset, set[1]);
    assert_eq!(preview.origin, PreviewOrigin::Embedded);
    assert!(editor.presentation.has_picture());

    send(editor, DevelopMessage::Step(1));
    assert_eq!(editor.develop.switch.as_ref().unwrap().asset, set[2]);
    assert!(
        editor.develop.state.preview.is_none(),
        "the edited photograph's camera preview is not drawn"
    );
    assert!(!editor.presentation.has_picture(), "the canvas is empty");
    run_open(editor);
    present(editor);
    assert_eq!(
        editor.document.state.as_ref().map(|state| &state.asset.id),
        Some(&set[2])
    );
    finish(scene);
}

/// While a draft is open a move is refused with the one start refusal's reason and changes nothing;
/// the strip collapses and expands with `Cmd+Option+F`, taking its height off the canvas.
#[test]
#[slow]
fn filmstrip_a_move_is_refused_while_a_draft_is_open() {
    let (mut scene, set) = developed("refused");
    let editor = &mut scene.editor;
    let (action, parameter) = patch_control(editor);
    hold_slider(editor, &action, &parameter);
    // The slider holds the arrow keys; the filmstrip's chevron is refused with the reason.
    assert!(key(editor, Key::Named(Named::ArrowRight), Modifiers::empty()).is_none());
    send(editor, DevelopMessage::Step(1));
    assert_eq!(
        editor.status.text,
        "Finish or discard the slider draft before switching photographs"
    );
    assert!(editor.develop.switch.is_none());
    assert_eq!(
        editor.document.state.as_ref().map(|state| &state.asset.id),
        Some(&set[0])
    );
    editor.gesture = None;

    let collapse = key(
        editor,
        Key::Character("f".into()),
        Modifiers::COMMAND | Modifiers::ALT,
    );
    assert!(matches!(
        collapse,
        Some(Message::Develop(DevelopMessage::Collapse))
    ));
    let before = editor.drawn_photo().expect("drawn");
    let _ = editor.update(collapse.unwrap());
    assert!(!editor.filmstrip_shown());
    assert!(editor.workspace.develop.strip.is_none());
    let after = editor.drawn_photo().expect("drawn");
    assert!(after.height >= before.height, "{before:?} {after:?}");
    send(editor, DevelopMessage::Collapse);
    assert!(editor.filmstrip_shown());
    finish(scene);
}

/// `path` developed by the independent client into the folder the plan proposes, to the job's end.
fn agent_develop(client: &Client, path: &PathBuf) {
    let started = client.ok(
        "pick.develop",
        json!({"targets": {"kind": "paths", "paths": [path]}, "into": [],
            "confirm_removable": true, "mutation": client.mutation()}),
    );
    wait_until("the agent's Develop", || {
        let read = client.ok("job.read", json!({"job_id": started["job_id"]}));
        !matches!(read["status"].as_str(), Some("queued" | "running"))
    });
}

/// The photographs an independent client reads of `folder` with its subfolders, captured oldest
/// first.
fn agent_strip(client: &Client, folder: &CatalogFolderId) -> Vec<AssetId> {
    client.ok(
        "browse.view",
        serde_json::to_value(strip_query(folder)).unwrap(),
    );
    let rows: ViewRows =
        serde_json::from_value(client.ok("browse.rows", json!({"from": 0, "count": 100}))).unwrap();
    rows.rows
        .iter()
        .filter_map(|row| match &row.item {
            RowItem::Photo { asset_id } => Some(asset_id.clone()),
            RowItem::File { .. } | RowItem::Input { .. } => None,
        })
        .collect()
}

fn strip_assets(editor: &Editor) -> Vec<AssetId> {
    editor
        .develop
        .state
        .strip
        .as_ref()
        .expect("a strip")
        .photos
        .iter()
        .map(|photo| photo.asset_id.clone())
        .collect()
}

/// A double-click on a photograph of a catalog view opens Develop on it at once, and its own
/// catalog folder becomes the Develop folder, read into the strip: exactly the photographs an
/// independent client reads of the folder with its subfolders, captured oldest first. Another
/// client's Develop into that folder reaches the strip through the event poll, the photograph on
/// screen keeping its place.
#[test]
#[slow]
fn filmstrip_a_double_click_opens_the_photographs_own_folder() {
    let mut scene = scene("catalog");
    for path in &scene.files[..3] {
        agent_develop(&scene.client, path);
    }
    let editor = &mut scene.editor;
    editor.develop.frames.paused = true;
    let _ = editor.update(Message::Select(SelectMessage::Source(
        ViewSource::AllPhotographs,
    )));
    evaluate(editor);
    read_rows(editor);
    assert_eq!(summary(editor).count, 3);
    let row = editor.select.state.rows.read(1).cloned().expect("a row");
    let RowItem::Photo { asset_id: asset } = row.item else {
        panic!("a photograph: {:?}", row.item);
    };
    let folder = row.folder_id.clone().expect("in a catalog folder");
    send(editor, DevelopMessage::OpenAt(1));
    assert!(!editor.select_shown());
    assert_eq!(editor.develop_folder(), Some(&folder));
    let strip = editor.develop.state.strip.clone().expect("a strip");
    assert!(strip.reading);
    assert_eq!(strip_assets(editor), vec![asset.clone()]);
    assert_eq!(
        editor.develop.switch.as_ref().map(|switch| &switch.asset),
        Some(&asset)
    );
    read_strip(editor);
    let expected = agent_strip(&scene.client, &folder);
    assert_eq!(expected.len(), 3);
    let editor = &mut scene.editor;
    let strip = editor.develop.state.strip.clone().unwrap();
    assert!(!strip.reading);
    assert_eq!(strip_assets(editor), expected);
    assert_eq!(strip.active, strip.position(&asset));
    run_open(editor);
    assert_eq!(
        editor.document.state.as_ref().map(|state| &state.asset.id),
        Some(&asset)
    );

    // Another client develops the fourth file into the same folder: the poll that hears it reads
    // the strip again, the open photograph keeping its place.
    agent_develop(&scene.client, &scene.files[3]);
    let editor = &mut scene.editor;
    let own: Vec<String> = editor.sync.own_requests.iter().cloned().collect();
    let polled = tasks::sync_now(
        &editor.owner,
        editor.client,
        None,
        editor.sync.sequence,
        &own,
        None,
    )
    .unwrap();
    let change = polled.library.expect("the poll hears a library change");
    assert!(change > editor.develop.strip_sequence);
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled))));
    read_strip(editor);
    let expected = agent_strip(&scene.client, &folder);
    assert_eq!(expected.len(), 4);
    let editor = &mut scene.editor;
    assert_eq!(strip_assets(editor), expected);
    let strip = editor.develop.state.strip.clone().unwrap();
    assert_eq!(strip.active, strip.position(&asset));
    assert!(strip.active.is_some());
    assert_eq!(editor.develop.strip_sequence, change);
    finish(scene);
}

/// Opening a file forgets the strip at once; once its open answers with a photograph, that
/// photograph's folder becomes the Develop folder and is read into the strip.
#[test]
#[slow]
fn filmstrip_a_file_opened_has_its_folder() {
    let (mut scene, set) = developed("alone");
    let editor = &mut scene.editor;
    let folder = editor
        .develop_folder()
        .cloned()
        .expect("Develop N's folder");
    assert!(editor.filmstrip_shown());
    editor.select.state.catalog.develop_folder = None;
    let path = scene.files[0].clone();
    let _ = editor.update(Message::Sync(SyncMessage::Picked(Some(path.clone()))));
    assert!(editor.develop.state.strip.is_none());
    assert!(!editor.filmstrip_shown());
    assert!(editor.workspace.develop.strip.is_none());
    let generation = editor.develop.file_open.expect("the file's open in flight");
    // The open's Develop, as its task runs it: the photograph already developed from the file,
    // and the catalog folder its report says it is in.
    let queued = crate::app::tasks::queue_import(&editor.owner, editor.client, &path)
        .expect("the file's Develop");
    let (asset, filed) =
        crate::app::tasks::developed_photograph(&editor.owner, editor.client, queued.job_id())
            .expect("the photograph developed from it");
    assert!(set.contains(&asset));
    // Its open answered, as `ImportRefreshed` hands on the report's folder.
    drop(editor.develop_file_opened(generation, Some(filed.clone())));
    find_folder(editor, &[filed]);
    assert_eq!(editor.develop_folder(), Some(&folder));
    let strip = editor
        .develop
        .state
        .strip
        .clone()
        .expect("the folder's strip");
    assert_eq!(strip.folder.as_ref(), Some(&folder));
    assert_eq!(strip.photos.len(), set.len());
    finish(scene);
}

/// One key through the key table, as the keyboard sends it.
fn key(editor: &Editor, key: Key, modifiers: Modifiers) -> Option<Message> {
    let event = iced::Event::Keyboard(KeyEvent::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
        location: iced::keyboard::Location::Standard,
        modifiers,
        text: None,
        repeat: false,
    });
    crate::app::keymap::keymap(&event, Status::Ignored, &editor.key_context())
}

/// The keys: `D` and `Cmd+Return` in Select; Escape and Return while the confirmation is open;
/// `←`/`→` and `Cmd+Option+F` in Develop with a set, never while a draft holds the keys or a text
/// field took them.
#[test]
fn filmstrip_keys_are_the_keyboard_tables() {
    use crate::app::keymap::{KeyContext, keymap};
    let press = |key: Key, modifiers: Modifiers| {
        iced::Event::Keyboard(KeyEvent::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
            location: iced::keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        })
    };
    let message = |event: &iced::Event, status, context: &KeyContext| {
        keymap(event, status, context).map(|message| format!("{message:?}"))
    };
    let develop = |message: DevelopMessage| Some(format!("{:?}", Message::Develop(message)));
    let select = KeyContext {
        select: true,
        ..KeyContext::default()
    };
    let d = press(Key::Character("d".into()), Modifiers::empty());
    assert_eq!(
        message(&d, Status::Ignored, &select),
        Some(format!(
            "{:?}",
            Message::Select(SelectMessage::Switch(Shown::Develop))
        ))
    );
    assert_eq!(message(&d, Status::Captured, &select), None);
    let enter = press(Key::Named(Named::Enter), Modifiers::COMMAND);
    assert_eq!(
        message(&enter, Status::Ignored, &select),
        develop(DevelopMessage::Open)
    );
    let confirming = KeyContext {
        develop_confirm: true,
        ..select.clone()
    };
    let escape = press(Key::Named(Named::Escape), Modifiers::empty());
    assert_eq!(
        message(&escape, Status::Captured, &confirming),
        develop(DevelopMessage::Cancel),
        "whatever has focus"
    );
    let ret = press(Key::Named(Named::Enter), Modifiers::empty());
    assert_eq!(
        message(&ret, Status::Ignored, &confirming),
        develop(DevelopMessage::Confirm)
    );
    assert_eq!(
        message(&ret, Status::Captured, &confirming),
        None,
        "the name field submits it itself"
    );

    let set = KeyContext {
        filmstrip: true,
        ..KeyContext::default()
    };
    let right = press(Key::Named(Named::ArrowRight), Modifiers::empty());
    let left = press(Key::Named(Named::ArrowLeft), Modifiers::empty());
    assert_eq!(
        message(&right, Status::Ignored, &set),
        develop(DevelopMessage::Step(1))
    );
    assert_eq!(
        message(&left, Status::Ignored, &set),
        develop(DevelopMessage::Step(-1))
    );
    assert_eq!(message(&right, Status::Captured, &set), None);
    assert_eq!(
        message(&right, Status::Ignored, &KeyContext::default()),
        None
    );
    for held in [
        KeyContext {
            drafting: true,
            ..set.clone()
        },
        KeyContext {
            slider_drafting: true,
            ..set.clone()
        },
        KeyContext {
            mask_keys: true,
            ..set.clone()
        },
    ] {
        assert!(!matches!(
            keymap(&right, Status::Ignored, &held),
            Some(Message::Develop(_))
        ));
    }
    let collapse = press(
        Key::Character("f".into()),
        Modifiers::COMMAND | Modifiers::ALT,
    );
    assert_eq!(
        message(&collapse, Status::Ignored, &set),
        develop(DevelopMessage::Collapse)
    );
    assert_eq!(
        message(&collapse, Status::Ignored, &KeyContext::default()),
        None
    );
}

/// With no Develop folder chosen and no photograph open, `D` stays in Select, the Develop tab is
/// disabled, and the status bar says to choose a catalog folder.
#[test]
fn develop_without_a_folder_stays_in_select_and_explains_the_choice() {
    let mut scene = scene("no-folder");
    drop(
        scene
            .editor
            .update(Message::Select(SelectMessage::Switch(Shown::Develop))),
    );
    assert!(scene.editor.select_shown());
    assert!(!scene.editor.workspace.develop.can_enter);
    assert_eq!(scene.editor.status.text, DEVELOP_GUIDANCE);
    finish(scene);
}

/// A catalog folder viewed in Select is the Develop folder, and browsing files or another catalog
/// view keeps it. `D` reads it through a client of its own, leaving Select's session as it was: an
/// empty folder keeps Select shown and says so; a folder holding the open photograph opens Develop
/// on it, with the folder's path as the strip's title.
#[test]
#[slow]
fn filmstrip_the_catalog_folder_viewed_is_the_develop_folder_and_survives_browsing() {
    let (mut scene, assets) = developed("folder-browse");
    let open = scene
        .editor
        .document
        .state
        .as_ref()
        .unwrap()
        .asset
        .id
        .clone();
    let lake = scene
        .editor
        .develop_folder()
        .cloned()
        .expect("Develop N's folder");
    let empty: luxforge_core::catalog_types::CatalogFolder = serde_json::from_value(
        scene.client.ok(
            "folder.create",
            json!({"name": "Empty", "mutation": scene.client.mutation()}),
        )["folder"]
            .clone(),
    )
    .unwrap();
    let editor = &mut scene.editor;
    drop(editor.update(Message::Select(SelectMessage::Switch(Shown::Select))));
    let view = |editor: &mut Editor, folder: &CatalogFolderId| {
        drop(editor.update(Message::Select(SelectMessage::Source(
            ViewSource::CatalogFolder {
                folder_id: folder.clone(),
                subfolders: true,
            },
        ))));
        evaluate(editor);
    };

    // The empty folder viewed is the Develop folder; `D` reads it and stays in Select.
    view(editor, &empty.id);
    assert_eq!(editor.develop_folder(), Some(&empty.id));
    assert!(editor.workspace.develop.can_enter);
    drop(editor.update(Message::Select(SelectMessage::Switch(Shown::Develop))));
    assert!(editor.develop.state.entering);
    read_strip(editor);
    assert!(editor.select_shown());
    assert_eq!(editor.status.text, EMPTY_FOLDER);

    // The developed folder viewed, then files and All photographs browsed: it stays chosen.
    view(editor, &lake);
    assert_eq!(editor.develop_folder(), Some(&lake));
    let path = scene.files[0].parent().unwrap().to_path_buf();
    drop(
        editor.update(Message::Select(SelectMessage::Source(ViewSource::Folder {
            path,
            subfolders: true,
        }))),
    );
    evaluate(editor);
    drop(editor.update(Message::Select(SelectMessage::Source(
        ViewSource::AllPhotographs,
    ))));
    evaluate(editor);
    assert_eq!(editor.develop_folder(), Some(&lake));

    // `D` reads it on a client of its own and opens Develop on the photograph already open.
    let before = editor.session.browse.clone();
    drop(editor.update(Message::Select(SelectMessage::Switch(Shown::Develop))));
    assert!(editor.select_shown(), "shown once the folder is read");
    read_strip(editor);
    assert_eq!(
        editor.session.browse, before,
        "the Select view's session is untouched"
    );
    assert!(!editor.select_shown());
    let strip = editor.develop.state.strip.clone().expect("the strip");
    assert_eq!(strip.folder.as_ref(), Some(&lake));
    let mut held: Vec<AssetId> = strip
        .photos
        .iter()
        .map(|photo| photo.asset_id.clone())
        .collect();
    let mut developed = assets.clone();
    held.sort_by_key(ToString::to_string);
    developed.sort_by_key(ToString::to_string);
    assert_eq!(held, developed);
    assert_eq!(strip.active, strip.position(&open));
    assert!(
        editor.develop.switch.is_none(),
        "the open photograph stays open"
    );
    let folders: CatalogFolders =
        serde_json::from_value(scene.client.ok("folder.list", json!({}))).unwrap();
    let name = &folders
        .folders
        .iter()
        .find(|folder| folder.id == lake)
        .unwrap()
        .name;
    assert_eq!(strip.path.last(), Some(name));
    assert_eq!(
        editor
            .workspace
            .develop
            .strip
            .as_ref()
            .map(|model| model.title.clone()),
        Some(path_title(&strip.path))
    );

    // Back in Select and in Develop again, the strip as read is shown with no read.
    drop(editor.update(Message::Select(SelectMessage::Switch(Shown::Select))));
    drop(editor.update(Message::Select(SelectMessage::Switch(Shown::Develop))));
    assert!(!editor.select_shown());
    assert_eq!(editor.develop.reading, None);
    finish(scene);
}

/// The deepest folder holding every photograph, by `folder.list`'s parent links, or the first
/// photograph's own when they share none.
#[test]
fn filmstrip_develop_n_opens_on_the_common_folder() {
    let folder = |name: &str, parent: Option<&CatalogFolderId>| {
        serde_json::from_value::<luxforge_core::catalog_types::CatalogFolder>(json!({
            "id": CatalogFolderId::new(),
            "name": name,
            "parent_id": parent,
            "created_ms": 0,
        }))
        .unwrap()
    };
    let year = folder("2026", None);
    let place = folder("Konstanz", Some(&year.id));
    let first = folder("12 Sep", Some(&place.id));
    let second = folder("13 Sep", Some(&place.id));
    let other = folder("Lake", None);
    let folders = CatalogFolders {
        folders: vec![
            year.clone(),
            place.clone(),
            first.clone(),
            second.clone(),
            other.clone(),
        ],
    };
    assert_eq!(
        common_folder(&folders, &[first.id.clone(), second.id.clone()]),
        Some(place.id.clone())
    );
    assert_eq!(
        common_folder(&folders, &[first.id.clone(), first.id.clone()]),
        Some(first.id.clone())
    );
    assert_eq!(
        common_folder(&folders, &[first.id.clone(), year.id.clone()]),
        Some(year.id.clone())
    );
    assert_eq!(
        common_folder(&folders, &[second.id.clone(), other.id.clone()]),
        Some(second.id.clone()),
        "no folder holds both: the first one's own"
    );
    assert_eq!(common_folder(&folders, &[]), None);
}

/// The open photograph's refusal (here a request in flight) stops a paste into that photograph
/// alone. A paste to several photographs goes to the batch, which skips what it cannot write and
/// reports it. A pending paste request refuses another paste, not an export or a preset batch.
#[test]
#[slow]
fn copy_settings_the_open_photographs_refusal_stops_only_a_single_photo_paste() {
    use crate::state::copy_settings::{Clipboard, Source};
    let (mut scene, set) = developed("paste-refusal");
    let editor = &mut scene.editor;
    editor.view_state.copy_settings.clipboard = Some(std::sync::Arc::new(Clipboard {
        source: Source {
            asset: set[3].clone(),
            entry: None,
            name: "IMG_0003.JPG".into(),
        },
        kind: luxforge_core::SourceTag::Jpeg,
        settings: Default::default(),
        groups: Vec::new(),
        copied_at: 0,
        capture: Value::Null,
    }));
    assert_eq!(editor.paste_refusal(false), None);
    editor.busy = true;
    assert!(editor.paste_refusal(false).is_some(), "one photograph");
    editor
        .develop
        .state
        .strip
        .as_mut()
        .expect("a strip")
        .select(2, true, false);
    assert_eq!(editor.paste_targets().count(), 2);
    assert_eq!(editor.paste_refusal(false), None, "the batch reports skips");
    editor.busy = false;

    editor.view_state.copy_settings.batch_pending = true;
    assert!(editor.paste_refusal(false).is_some());
    assert!(editor.batch_refusal().is_some());
    editor.view_state.copy_settings.batch_pending = false;
    editor.view_state.copy_settings.pending = true;
    assert!(editor.paste_refusal(false).is_some());
    assert_ne!(
        editor.batch_refusal().as_deref(),
        Some("Waiting for the settings request"),
        "a capture in flight is no reason to refuse an export or a preset batch"
    );
    editor.view_state.copy_settings.pending = false;
    finish(scene);
}
