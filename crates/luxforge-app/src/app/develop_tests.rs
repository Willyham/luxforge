//! Developing picks and the development set against a real owner, over a folder of copied fixture
//! JPEGs the index lane reads: Develop N's confirmation from `pick.plan`, Escape changing nothing,
//! Develop sending the `pick.develop` an agent writes and opening Develop on the photographs it
//! answered as the set; `→` and `←` drawing the photograph's cached large preview in the update of
//! the key and its render after it; an answer or decode for another photograph arriving after a
//! move never drawn for this one, and a preview of another entry withdrawn; a move refused while a
//! draft is open; a catalog view's photographs as the set; and the keys. Each owner call runs
//! exactly as its task would, through the `*_now` functions the tasks run, and its answer is handed
//! back as the task's message; requests are compared with what an independent JSON client writes
//! and answers.
use super::*;
use crate::{
    Config,
    app::{
        Boot,
        loupe_frames::{Decoded, Slot},
        message::{preview::PreviewMessage, sync::SyncMessage},
        select::refresh_now,
        select_owner_tests::{answer_reads, evaluate, read_rows},
        testing::{descriptors, hold_slider, patch_control},
    },
    state::select::ReadSource,
};
use iced::{
    Size,
    event::Status,
    keyboard::{Event as KeyEvent, Key, Modifiers, key::Named, key::Physical},
};
use luxforge_core::{
    ApiRequest, Cancel, EditorService, EntryId,
    catalog_types::{FolderChoice, ViewSource, ViewSummary},
    decode_preview,
};
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
                .is_some_and(|held| !held.picture.stand_in)
        })
    });
}

/// Four files picked and developed through Develop N into its proposed folder, and the first
/// photograph open in Develop with its frame on screen. Answers the set.
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
    run_open(editor);
    present(editor);
    let set: Vec<AssetId> = editor
        .develop
        .state
        .set
        .as_ref()
        .expect("a set")
        .photos
        .iter()
        .map(|photo| photo.asset_id.clone())
        .collect();
    assert_eq!(set.len(), 4);
    (scene, set)
}

/// Develop N reads the plan of the picks in view, which is what an agent's own view of the same
/// source plans; Escape closes the confirmation and nothing changes; typing renames the proposed
/// folder, and Develop sends the `pick.develop` an agent writes for it, whose job's end opens
/// Develop on the first photograph it developed, with every photograph it answered as the set.
#[test]
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
        FolderChoice::New { name, .. } => name.clone(),
        other => panic!("a new folder is proposed: {other:?}"),
    };
    let model = editor.workspace.develop.confirm.clone().expect("open");
    assert_eq!(model.title, "Develop 3 picks");
    assert_eq!(model.events[0].field.as_deref(), Some(proposed.as_str()));
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

    // Typing renames the new folder; Return develops.
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
        json!([{"event_id": plan.events[0].event_id, "folder": {"kind": "new", "name": "Lake trip"}}])
    );
    assert_eq!(sent["mutation"]["actor"], "desktop");
    assert!(sent.get("use_copies").is_none() && sent.get("confirm_removable").is_none());
    let record = run_develop(editor);
    assert_eq!(record["status"], "ready", "{record}");
    let report: DevelopReport = serde_json::from_value(record["result"].clone()).unwrap();
    assert_eq!(report.developed.len(), 3);
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    // The catalog holds them in the folder confirmed, and their picks are cleared.
    let folders: CatalogFolders =
        serde_json::from_value(scene.client.ok("folder.list", json!({}))).unwrap();
    let lake = folders
        .folders
        .iter()
        .find(|folder| folder.name == "Lake trip")
        .expect("the confirmed folder");
    assert_eq!(lake.count, 3);
    assert_eq!(
        scene.client.ok("pick.list", json!({}))["picks"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    // Develop opens on the first photograph, with the photographs it answered as the set.
    assert!(!editor.select_shown());
    let set = editor.develop.state.set.clone().expect("a set");
    assert_eq!(
        set.photos
            .iter()
            .map(|photo| &photo.asset_id)
            .collect::<Vec<_>>(),
        report
            .developed
            .iter()
            .map(|developed| &developed.asset_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(set.active, 0);
    assert!(editor.filmstrip_shown());
    let strip = editor
        .workspace
        .develop
        .strip
        .clone()
        .expect("the filmstrip");
    assert_eq!((strip.total, strip.active), (3, Some(0)));
    let switch = editor
        .develop
        .switch
        .clone()
        .expect("the first photograph opening");
    assert_eq!(switch.asset, set.photos[0].asset_id);
    assert_eq!(
        editor.workspace.title.file_name.as_deref(),
        Some(set.photos[0].name.as_str())
    );
    run_open(editor);
    assert_eq!(
        editor.document.state.as_ref().map(|state| &state.asset.id),
        Some(&set.photos[0].asset_id)
    );
    assert!(editor.develop.switch.is_none());
    present(editor);
    assert_eq!(
        editor.gesture_refusal(Starting::Action),
        None,
        "the controls enable"
    );
    finish(scene);
}

/// `→` draws the next photograph's cached large preview in the update of the key, labelled a
/// preview, with the controls disabled and nothing of the photograph before it on screen; its open
/// then shows the same photograph at the preview's entry, and its render replaces the preview.
/// `←` goes back the same way. Nothing prepares a neighbour.
#[test]
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
    assert_eq!(editor.develop.state.set.as_ref().unwrap().active, 1);
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
/// status bar no longer calls the picture a cached preview.
#[test]
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
    // The GPU presents the open photograph's content over the preview, as a warm open does.
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

/// While a draft is open a move is refused with the one start refusal's reason and changes nothing;
/// the strip collapses and expands with `Cmd+Option+F`, taking its height off the canvas.
#[test]
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

/// `D` over a catalog view, or a double-click, opens Develop on the photograph with the view's
/// photographs as the set, read into it a window of rows at a time: exactly the rows an
/// independent client reads of the same view.
#[test]
fn filmstrip_a_catalog_view_opens_develop_with_its_photographs_as_the_set() {
    let mut scene = scene("catalog");
    for path in &scene.files {
        let started = scene.client.ok(
            "pick.develop",
            json!({"targets": {"kind": "paths", "paths": [path]}, "into": [],
                "confirm_removable": true, "mutation": scene.client.mutation()}),
        );
        wait_until("the agent's Develop", || {
            let read = scene
                .client
                .ok("job.read", json!({"job_id": started["job_id"]}));
            !matches!(read["status"].as_str(), Some("queued" | "running"))
        });
    }
    let editor = &mut scene.editor;
    editor.develop.frames.paused = true;
    let _ = editor.update(Message::Select(SelectMessage::Source(
        ViewSource::AllPhotographs,
    )));
    evaluate(editor);
    read_rows(editor);
    let query = summary(editor).query.clone();
    let (revision, count) = (summary(editor).revision, summary(editor).count);
    assert_eq!(count, 4);
    send(editor, DevelopMessage::OpenAt(2));
    assert!(!editor.select_shown());
    let set = editor.develop.state.set.clone().expect("a set");
    assert!(set.reading);
    let opening = editor
        .develop
        .switch
        .clone()
        .expect("the photograph opening");
    let serial = set.serial;
    let result = set_now(&editor.owner, editor.client, revision, count, 2);
    send(editor, DevelopMessage::SetRead { serial, result });
    scene
        .client
        .ok("browse.view", serde_json::to_value(&query).unwrap());
    let rows: ViewRows = serde_json::from_value(
        scene
            .client
            .ok("browse.rows", json!({"from": 0, "count": 4})),
    )
    .unwrap();
    let expected: Vec<AssetId> = rows
        .rows
        .iter()
        .filter_map(|row| match &row.item {
            RowItem::Photo { asset_id } => Some(asset_id.clone()),
            RowItem::File { .. } => None,
        })
        .collect();
    let set = editor.develop.state.set.clone().unwrap();
    assert!(!set.reading);
    assert_eq!(
        set.photos
            .iter()
            .map(|photo| photo.asset_id.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(set.active, 2);
    assert_eq!(opening.asset, expected[2]);
    run_open(editor);
    assert_eq!(
        editor.document.state.as_ref().map(|state| &state.asset.id),
        Some(&expected[2])
    );
    finish(scene);
}

/// Opening a file alone leaves Develop with no set, and no strip.
#[test]
fn filmstrip_a_file_opened_alone_has_no_set() {
    let (mut scene, _) = developed("alone");
    let editor = &mut scene.editor;
    assert!(editor.filmstrip_shown());
    let path = scene.files[0].clone();
    let _ = editor.update(Message::Sync(SyncMessage::Picked(Some(path))));
    assert!(editor.develop.state.set.is_none());
    assert!(!editor.filmstrip_shown());
    assert!(editor.workspace.develop.strip.is_none());
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
        develop(DevelopMessage::Key)
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
        development_set: true,
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
