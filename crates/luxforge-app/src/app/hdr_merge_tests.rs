//! HDR merges in the Select workspace against a real owner, merging real files through a synthetic
//! camera (`OwnerHandle::merge_synthetic_frames`): everything after each frame's development is the
//! production path. A bracket header's Merge HDR and a selection's, the core's refusal said in the
//! menu, the one pending row with its Cancel and a failure naming its input, the result in the
//! catalog folder the plan names, leading its collapsed stack with its badge while Select stays
//! shown, `S` expanding and collapsing it, an original picked as any file is, a merge reused, Remove merge with library undo
//! and redo, and Put back merge for a merge in Removed. Each owner call runs as its task would,
//! through the `*_now` function the task runs, its answer handed back as the task's message; what
//! the desktop sends is compared with what a second JSON client writes, and every effect is read
//! back through that client.
use super::select_owner_tests::{
    click, evaluate, finish, key, newest_change, pick_of, read_rows, selecting_over, sent, settle,
};
use crate::{
    app::{
        Editor,
        message::{Message, select::SelectMessage, select_merge::MergeMessage},
        select_merge::{expanded_now, plan_now, start_now},
        tasks::call,
    },
    state::{
        select::{Block, InfoModel, MomentAction},
        select_merge::{MergeAction, MergeRequest, ev_words, plan_refusal},
    },
};
use iced::{Point, Size, keyboard::Modifiers};
use luxforge_core::{
    SourceTag, SyntheticFrame,
    catalog_types::{
        CameraBody, CaptureTime, Dimensions, ExifOrientation, Exposure, FileRecord, FileSignature,
        HeaderMetadata, HeaderState, IndexRoot, IndexedFolder, MergePlan, RootKind, RowItem,
        StackExpansion, StackRole, ViewSource, Volume, VolumeId,
    },
    seed::{CatalogSeeder, IndexSeeder},
};
use luxforge_reference::{
    hdr::{CAMERA_TO_SRGB, Camera},
    hdr_align::{self as geometry, Rigid},
};
use luxforge_testbase::Gate;
use luxforge_testkit::client as kit;
use luxforge_ui::{GridContext, GridPress, PressModifiers};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const CATALOG_ID: &str = "catalog-hdr-merge-test";
const W: u32 = 384;
const H: u32 = 256;
/// The bracket's exposures, in stops from its middle frame, and the motion each was taken with.
const OFFSETS: [f64; 3] = [-2.0, 0.0, 2.0];
const MOTIONS: [(f64, f64); 3] = [(1.25, -0.75), (0.0, 0.0), (-0.9, 1.1)];
/// The bracket's three frames, then two single frames of the middle exposure.
const NAMES: [&str; 5] = [
    "DSC_0001.NEF",
    "DSC_0002.NEF",
    "DSC_0003.NEF",
    "DSC_0004.NEF",
    "DSC_0005.NEF",
];
const TIMES: [&str; 5] = [
    "2026:09:12 09:00:00.000",
    "2026:09:12 09:00:00.300",
    "2026:09:12 09:00:00.600",
    "2026:09:12 09:30:00",
    "2026:09:12 10:15:00",
];

fn volume() -> VolumeId {
    VolumeId::parse("volume-hdr-00000000").unwrap()
}

fn shutter(at: usize) -> f64 {
    OFFSETS[at.min(2)].exp2() / 250.0
}

/// The bracket's folder beside its catalog: five real files, indexed with their headers, and
/// whether the camera reads `DSC_0002.NEF` (a test breaks it to fail a merge on that input).
struct Bench {
    folder: PathBuf,
    catalog: PathBuf,
    broken: Arc<AtomicBool>,
    reading: Arc<Gate>,
}

impl Bench {
    fn new(name: &str) -> Self {
        let directory = luxforge_testbase::paths::temp_dir(&format!("hdr-merge-{name}"))
            .canonicalize()
            .unwrap();
        let folder = directory.join("bracket");
        fs::create_dir_all(&folder).unwrap();
        let mut files = Vec::new();
        for (at, name) in NAMES.iter().enumerate() {
            let path = folder.join(name);
            // Distinct bytes, so each file is its own frame.
            let mut bytes = format!("synthetic frame {at}\n").into_bytes();
            bytes.resize(1 << 14, at as u8);
            fs::write(&path, bytes).unwrap();
            files.push(record(&folder, &path, at));
        }
        let catalog = directory.join("catalog.sqlite");
        let mut seeder = CatalogSeeder::create(&catalog, CATALOG_ID).unwrap();
        seeder
            .volumes(&[Volume {
                id: volume(),
                mount_point: "/".into(),
                label: "Macintosh HD".into(),
                removable: false,
                platform_id: None,
                last_seen_ms: 1,
            }])
            .unwrap();
        seeder
            .indexed_folders(&[IndexedFolder {
                path: folder.clone(),
                volume_id: volume(),
                added_ms: 1,
                actor: "desktop".into(),
            }])
            .unwrap();
        seeder.finish().unwrap();
        let mut index = IndexSeeder::create(&catalog, CATALOG_ID).unwrap();
        index.files(&files).unwrap();
        index
            .roots(&[IndexRoot {
                path: folder.clone(),
                kind: RootKind::Indexed,
                volume_id: volume(),
                listed_ms: Some(1),
                file_count: Some(files.len() as u32),
                offline: false,
            }])
            .unwrap();
        index.finish().unwrap();
        Self {
            folder,
            catalog,
            broken: Arc::new(AtomicBool::new(false)),
            reading: Arc::new(Gate::new()),
        }
    }

    /// The editor over the catalog with the bracket's folder in view, every row read, its owner
    /// merging with the synthetic camera.
    fn open(&self) -> Editor {
        let (mut editor, _) = selecting_over(self.catalog.clone());
        let broken = self.broken.clone();
        editor.owner.merge_synthetic_frames(
            Arc::new(move |path: &Path| frame(path, &broken)),
            Some(self.reading.clone()),
        );
        let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
            1000.0, 700.0,
        ))));
        let _ = editor.update(Message::Select(SelectMessage::Source(ViewSource::Folder {
            path: self.folder.clone(),
            subfolders: false,
        })));
        evaluate(&mut editor);
        read_rows(&mut editor);
        settle(&mut editor);
        editor
    }
}

/// One frame's index record: a Nikon Z 6's 384 × 256 RAW taken at its time and exposure.
fn record(folder: &Path, path: &Path, at: usize) -> FileRecord {
    let (datetime, subsec) = match TIMES[at].split_once('.') {
        Some((datetime, subsec)) => (datetime, Some(subsec)),
        None => (TIMES[at], None),
    };
    let bracket = at < 3;
    let header = HeaderMetadata {
        capture: CaptureTime::from_exif(datetime, subsec, Some("+02:00")),
        camera: Some(CameraBody {
            make: "NIKON CORPORATION".into(),
            model: "NIKON Z 6".into(),
            serial: Some("6001".into()),
        }),
        exposure: Exposure {
            time_s: Some(shutter(if bracket { at } else { 1 }) as f32),
            f_number: Some(8.0),
            iso: Some(100),
            bias_ev: Some(if bracket { OFFSETS[at] as f32 } else { 0.0 }),
            focal_mm: Some(24.0),
            ..Exposure::default()
        },
        dimensions: Some(Dimensions {
            width: W,
            height: H,
        }),
        orientation: ExifOrientation::new(1),
        ..HeaderMetadata::default()
    };
    FileRecord {
        path: path.to_path_buf(),
        folder: folder.to_path_buf(),
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        volume_id: volume(),
        signature: FileSignature::of(&fs::metadata(path).unwrap()),
        kind: SourceTag::Raw,
        header: HeaderState::Ok(Box::new(header)),
        last_seen_ms: 1,
        born_ns: None,
    }
}

/// What the synthetic camera develops `path` to: a textured scene, moved and exposed as its
/// frame was; nothing for `DSC_0002.NEF` while the camera is broken.
fn frame(path: &Path, broken: &AtomicBool) -> Option<SyntheticFrame> {
    let name = path.file_name()?.to_string_lossy();
    let at = NAMES.iter().position(|known| *known == name)?;
    if at == 1 && broken.load(Ordering::SeqCst) {
        return None;
    }
    let position = at.min(2);
    let camera = Camera {
        camera_to_srgb: CAMERA_TO_SRGB,
        gains: [2.0, 1.0, 1.5],
        bayer: true,
    };
    let scene = geometry::textured_scene(11 + (at / 3) as u64);
    let exposure = geometry::record(
        &scene,
        W as usize,
        H as usize,
        &Rigid::new(W as usize, H as usize, 0.0, MOTIONS[position]),
        &camera,
        if at < 3 { OFFSETS[position] } else { 0.0 },
        None,
    );
    let mut planes = Vec::with_capacity(3 * (W * H) as usize);
    for plane in &exposure.rgb {
        planes.extend_from_slice(plane);
    }
    Some(SyntheticFrame {
        width: W,
        height: H,
        planes,
        shutter_s: shutter(if at < 3 { at } else { 1 }) as f32,
        f_number: 8.0,
        iso: 100,
        camera_to_srgb: CAMERA_TO_SRGB.map(|row| row.map(|value| value as f32)),
        gains: [2.0, 1.0, 1.5],
    })
}

fn merge(editor: &mut Editor, message: MergeMessage) {
    let _ = editor.update(Message::Select(SelectMessage::Merge(message)));
}

fn act(editor: &mut Editor, action: MergeAction) {
    merge(editor, MergeMessage::Act(action));
}

/// The grid's moment number of the bracket and its actions' labels.
fn bracket_header(editor: &Editor) -> Option<(u32, Vec<String>)> {
    editor
        .select
        .state
        .content
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Moment {
                bracket, actions, ..
            } => Some((*bracket, actions)),
            _ => None,
        })
        .enumerate()
        .find(|(_, (bracket, _))| *bracket)
        .map(|(number, (_, actions))| {
            (
                number as u32,
                actions.iter().map(|action| action.label()).collect(),
            )
        })
}

/// The pending row's title, detail and button, when the grid draws one.
fn pending_row(editor: &Editor) -> Option<(String, String, String, bool)> {
    editor
        .select
        .state
        .content
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Pending {
                title,
                detail,
                action,
                failed,
                ..
            } => Some((title.clone(), detail.clone(), action.clone(), *failed)),
            _ => None,
        })
}

/// Run the merge start the editor has in flight, as its owner task would: the request it was
/// asked for and every request it sent.
fn start(editor: &mut Editor) -> MergeRequest {
    let request = editor
        .select
        .state
        .merge
        .starting
        .clone()
        .expect("a merge starting");
    let result = start_now(&editor.owner, editor.client, &request);
    merge(
        editor,
        MergeMessage::Started {
            request: Box::new(request.clone()),
            result,
        },
    );
    request
}

/// The requests the newest merge start sent, by method.
fn sent_merge(editor: &Editor, method: &str) -> Value {
    editor
        .select
        .merge_sent
        .as_ref()
        .and_then(Value::as_array)
        .and_then(|sent| sent.iter().find(|sent| sent["method"] == method))
        .map(|sent| sent["params"].clone())
        .unwrap_or_else(|| panic!("no {method} in {:?}", editor.select.merge_sent))
}

/// Follow the running merge's job as its `job.wait` reader does, handing every record to the
/// editor, until it ends; its last record.
fn follow(editor: &mut Editor) -> Value {
    let job = editor
        .select
        .state
        .merge
        .running_job()
        .unwrap_or_else(|| panic!("a merge running: {}", editor.status.text))
        .to_owned();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let mut after = None;
    loop {
        let (change, record) = runtime
            .block_on(async {
                tokio::time::timeout(
                    luxforge_testbase::HANG,
                    super::job_reads::wait(&editor.owner, editor.client, &job, after),
                )
                .await
            })
            .expect("the merge's job to move")
            .unwrap();
        after = Some(change);
        merge(
            editor,
            MergeMessage::Watched {
                job: job.clone(),
                result: Ok(record.clone()),
            },
        );
        if !matches!(record["status"].as_str(), Some("queued" | "running")) {
            return record;
        }
    }
}

/// Run the `stack.set-expanded` the editor has in flight, as its owner task would, and settle.
fn expand(editor: &mut Editor) -> Value {
    let params = editor.select.stack_sent.clone().expect("a stack request");
    let query = editor.select.state.query.clone().unwrap();
    let serial = editor.select.serial;
    let result = expanded_now(&editor.owner, editor.client, params.clone(), &query);
    let _ = editor.update(Message::Select(SelectMessage::Viewed { serial, result }));
    settle(editor);
    params
}

/// Run the `merge.plan` the menu or the form has in flight, as its owner task would.
fn plan(editor: &mut Editor) -> Value {
    let params = editor.select.plan_sent.clone().expect("a plan request");
    let serial = editor.select.merge_plans;
    let result = plan_now(&editor.owner, editor.client, params.clone());
    merge(editor, MergeMessage::Planned { serial, result });
    params
}

/// Shift-click the cell at `item`.
fn extend(editor: &mut Editor, item: u32) {
    let layout = &editor.select.layout;
    let cell = layout.cell(layout.cell_of_item(item).unwrap());
    let press = GridPress {
        cell: cell.cell,
        item: cell.item,
        span: cell.span,
        modifiers: PressModifiers {
            shift: true,
            command: false,
        },
        double: false,
    };
    let _ = editor.update(Message::Select(SelectMessage::Press(press)));
}

/// A right-click on the cell at `item`.
fn context(editor: &mut Editor, item: u32) {
    let layout = &editor.select.layout;
    let cell = layout.cell(layout.cell_of_item(item).unwrap());
    let _ = editor.update(Message::Select(SelectMessage::Context(GridContext {
        cell: cell.cell,
        item: cell.item,
        span: cell.span,
        at: Point::new(100.0, 100.0),
    })));
}

/// The position of the row whose file is called `name`.
fn position_of(editor: &Editor, name: &str) -> u32 {
    let count = editor.select.state.summary.as_ref().unwrap().count;
    (0..count)
        .find(|&at| {
            editor
                .select
                .state
                .rows
                .row(at)
                .is_some_and(|row| row.file_name == name)
        })
        .unwrap_or_else(|| panic!("{name} is not in view"))
}

/// The active row.
fn active(editor: &Editor) -> luxforge_core::catalog_types::ViewRow {
    let at = editor
        .session
        .browse
        .selection
        .active
        .expect("an active item");
    editor.select.state.rows.row(at).expect("its row").clone()
}

/// Merge HDR from the bracket's header: one pending row while it runs, with the originals still
/// shown; then the result active and selected in Select, which stays shown, in the catalog folder
/// the plan named, leading its collapsed stack with its badge, never shown picked; `S` expands and
/// collapses it through the owner; an original is picked as any file is, and only that pick is
/// Develop N's.
#[test]
fn hdr_merge_from_a_bracket_header_leads_a_collapsed_stack_in_the_plans_folder() {
    let bench = Bench::new("pick");
    let mut editor = bench.open();
    let agent = editor.owner.register();
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 5);
    let (number, labels) = bracket_header(&editor).expect("the bracket's header");
    assert_eq!(labels, ["Pick all 3", "Merge HDR"]);
    let actions = editor
        .select
        .state
        .content
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Moment {
                bracket: true,
                actions,
                ..
            } => Some(actions.clone()),
            _ => None,
        })
        .unwrap();
    assert!(actions[1].primary() && !actions[0].primary());
    assert_eq!(actions[1], MomentAction::Merge);

    // Pressed with the camera held at its first read: the pending row before the frames.
    bench.reading.shut();
    merge(
        &mut editor,
        MergeMessage::Header {
            moment: number,
            action: 1,
        },
    );
    assert_eq!(editor.status.text, "Merging HDR\u{2026}");
    start(&mut editor);
    let (title, _, button, failed) = pending_row(&editor).expect("the pending row");
    assert_eq!(title, "Merging HDR \u{b7} 3 exposures");
    assert_eq!(button, "Cancel");
    assert!(!failed);
    assert_eq!(
        editor.select.state.summary.as_ref().unwrap().count,
        5,
        "nothing is hidden before the merge is published"
    );
    // What the desktop sent is what a JSON client writes: the frames by identity, as planned, into
    // the folder the plan suggested and the desktop made.
    let files: Vec<Value> = (0..3)
        .map(|at| match editor.select.state.rows.row(at).unwrap().item {
            RowItem::File { file_id } => json!(file_id),
            _ => panic!("a file"),
        })
        .collect();
    let planned = sent_merge(&editor, "merge.plan");
    assert_eq!(
        planned,
        json!({"targets": {"kind": "files", "file_ids": files}})
    );
    let (plan, _) = call(&editor.owner, agent, "merge.plan", planned).unwrap();
    let plan: MergePlan = serde_json::from_value(plan).unwrap();
    assert!(plan_refusal(&plan).is_none(), "{plan:?}");
    let folder = sent_merge(&editor, "folder.create");
    assert!(folder["name"].as_str().is_some_and(|name| !name.is_empty()));
    let created = sent_merge(&editor, "merge.create");
    assert_eq!(created["kind"], "hdr");
    assert_eq!(created["targets"], planned_targets(&files));
    assert_eq!(created["plan_id"], json!(plan.plan_id));
    assert_eq!(created["mutation"]["actor"], "desktop");
    let (record, _) = call(
        &editor.owner,
        agent,
        "job.read",
        json!({"job_id": editor.select.state.merge.running_job().unwrap()}),
    )
    .unwrap();
    assert!(
        matches!(record["status"].as_str(), Some("queued" | "running")),
        "{record}"
    );

    // Released: the job's progress reaches the row, and its end the view.
    bench.reading.open();
    let record = follow(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let result = record["result"]["result_asset_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        editor.status.text.starts_with("Merged ") && editor.status.text.ends_with("Undo \u{2318}Z"),
        "{}",
        editor.status.text
    );
    settle(&mut editor);
    assert!(
        pending_row(&editor).is_none(),
        "the row gives way to the result"
    );
    let summary = editor.select.state.summary.clone().unwrap();
    assert_eq!(summary.count, 3, "two singles and the result: {summary:?}");
    assert_eq!(summary.collapsed, 3);
    assert_eq!(
        summary.picked, 0,
        "Develop N counts only pending file picks"
    );
    assert_eq!(editor.workspace.select.title.picks, 0);
    assert!(
        editor.workspace.select.status.line.ends_with("0 picked"),
        "{}",
        editor.workspace.select.status.line
    );
    assert_eq!(
        editor.select.state.shown,
        crate::state::select::Shown::Select
    );
    let leader = active(&editor);
    assert!(
        matches!(&leader.item, RowItem::Photo { asset_id } if asset_id.to_string() == result),
        "the result is active: {leader:?}"
    );
    let stack = leader.stack.clone().expect("the result leads its stack");
    assert_eq!(stack.role, StackRole::Leader);
    assert_eq!(stack.exposures, 3);
    assert_eq!(stack.collapsed, 3);
    let at = editor.session.browse.selection.active.unwrap();
    assert!(
        !editor.select.state.rows.cell(at).unwrap().picked,
        "a result is never shown picked"
    );
    // In the catalog folder the plan named, as a second client reads it.
    let folders: luxforge_core::catalog_types::CatalogFolders = serde_json::from_value(
        call(&editor.owner, agent, "folder.list", json!({}))
            .unwrap()
            .0,
    )
    .unwrap();
    let named = folders
        .folders
        .iter()
        .find(|held| Some(held.name.as_str()) == folder["name"].as_str())
        .expect("the plan's folder");
    assert_eq!(leader.folder_id.as_ref(), Some(&named.id));
    assert_eq!(named.count, 1);
    let InfoModel::One(info) = &editor.workspace.select.info else {
        panic!("one item: {:?}", editor.workspace.select.info);
    };
    let band = info.stack.as_ref().expect("the stack band");
    assert_eq!(band.title, "HDR \u{b7} 3 exposures");
    assert_eq!(band.note.as_deref(), Some("3 originals beneath it"));

    // `S` expands the stack through the owner: the originals follow the result, each with its
    // exposure; `S` on an original collapses it again.
    key(&mut editor, "s", Modifiers::empty());
    let expanded = expand(&mut editor);
    assert_eq!(
        expanded,
        json!({
            "merge_id": stack.merge_id,
            "expansion": "expanded",
            "revision": summary.revision,
        })
    );
    let summary = editor.select.state.summary.clone().unwrap();
    assert_eq!(summary.count, 6, "{summary:?}");
    let first = position_of(&editor, "DSC_0001.NEF");
    let original = editor.select.state.rows.row(first).unwrap().clone();
    let original_stack = original.stack.expect("an original of the stack");
    assert_eq!(original_stack.role, StackRole::Original);
    assert_eq!(
        original_stack.offset_ev.and_then(ev_words),
        Some("\u{2212}2 EV")
    );
    assert_eq!(
        editor.select.state.rows.row(first - 1).unwrap().file_name,
        leader.file_name,
        "the originals follow their result"
    );
    // An original is picked as any file is: `pick.set` of its file, Develop N's only pick.
    click(&mut editor, first);
    key(&mut editor, "p", Modifiers::empty());
    let (params, _) = sent(&editor, "pick.set");
    assert_eq!(params["picked"], true);
    settle(&mut editor);
    assert_eq!(
        pick_of(&editor.owner, agent, "DSC_0001.NEF").as_deref(),
        Some("desktop")
    );
    assert_eq!(editor.select.state.summary.as_ref().unwrap().picked, 1);
    assert_eq!(editor.workspace.select.title.picks, 1);
    key(&mut editor, "s", Modifiers::empty());
    let collapsed = expand(&mut editor);
    assert_eq!(collapsed["expansion"], "collapsed");
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 3);
    finish(editor, bench.catalog.clone());
}

/// `merge.create`'s targets for `files`.
fn planned_targets(files: &[Value]) -> Value {
    json!({"kind": "files", "file_ids": files})
}

/// A selection's menu says why two frames that are no bracket cannot merge, in the core's words;
/// the bracket's frames selected merge with Merge HDR; the same frames
/// again reuse the result; Remove merge shows the originals again, and library undo and redo move
/// the merge, edited, back and forth, legibly; a merge found in Removed is offered back.
#[test]
fn hdr_merge_from_a_selection_refuses_imports_reuses_removes_and_puts_back() {
    let bench = Bench::new("selection");
    let mut editor = bench.open();
    let agent = editor.owner.register();

    // Two singles of one exposure: the menu's merges say what the core said.
    click(&mut editor, 3);
    extend(&mut editor, 4);
    context(&mut editor, 4);
    let params = plan(&mut editor);
    let (answer, _) = call(&editor.owner, agent, "merge.plan", params).unwrap();
    let answer: MergePlan = serde_json::from_value(answer).unwrap();
    let reason = plan_refusal(&answer).expect("two singles are no bracket");
    let menu = editor
        .workspace
        .select
        .merge
        .menu
        .clone()
        .expect("the menu");
    assert_eq!(menu.entries[0].label, "Merge HDR");
    assert_eq!(
        menu.entries[0].hint,
        Some("Merge the frames into one HDR photograph in the catalog")
    );
    assert_eq!(menu.entries[1].label, "Merge options\u{2026}");
    assert!(menu.entries[0].action.is_none());
    assert_eq!(menu.entries[0].reason.as_deref(), Some(reason.as_str()));
    act(&mut editor, MergeAction::CloseMenu);

    // The bracket's frames, selected: Merge HDR.
    click(&mut editor, 0);
    extend(&mut editor, 2);
    context(&mut editor, 2);
    plan(&mut editor);
    let menu = editor.workspace.select.merge.menu.clone().unwrap();
    assert!(menu.entries[0].action.is_some(), "{menu:?}");
    assert_eq!(menu.entries[1].trailing.as_deref(), Some("3 frames"));
    act(&mut editor, MergeAction::Start);
    start(&mut editor);
    let record = follow(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let merge_id = record["result"]["merge_id"].as_str().unwrap().to_owned();
    let result = record["result"]["result_asset_id"]
        .as_str()
        .unwrap()
        .to_owned();
    settle(&mut editor);
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 3);

    // Edited by an agent, as any photograph is.
    let asset = json!(result);
    let revision = kit::revision(&editor.owner, agent, &asset).unwrap();
    kit::call(
        &editor.owner,
        agent,
        "edit.set-basic",
        json!({
            "asset_id": asset,
            "exposure": 0.4,
            "mutation": kit::mutation(revision, &kit::request_id("agent-edit"), "agent"),
        }),
    )
    .unwrap();
    let edited = kit::recipe(&editor.owner, agent, &asset).unwrap();

    // The same frames again: the result reused.
    act(
        &mut editor,
        MergeAction::Expand(
            serde_json::from_value(json!(merge_id)).unwrap(),
            StackExpansion::Expanded,
        ),
    );
    expand(&mut editor);
    let first = position_of(&editor, "DSC_0001.NEF");
    click(&mut editor, first);
    extend(&mut editor, first + 2);
    context(&mut editor, first + 2);
    plan(&mut editor);
    act(&mut editor, MergeAction::Start);
    start(&mut editor);
    let record = follow(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(record["result"]["reused"], true);
    assert_eq!(record["result"]["merge_id"], json!(merge_id));
    assert!(editor.status.text.contains("is this merge already"));
    settle(&mut editor);
    assert!(
        matches!(&active(&editor).item, RowItem::Photo { asset_id } if asset_id.to_string() == result)
    );

    // Remove merge from the result's menu: the originals are files again.
    let at = editor.session.browse.selection.active.unwrap();
    context(&mut editor, at);
    let menu = editor
        .workspace
        .select
        .merge
        .menu
        .clone()
        .expect("the stack menu");
    let remove = menu
        .entries
        .iter()
        .find(|entry| entry.label == "Remove merge")
        .and_then(|entry| entry.action.clone())
        .expect("Remove merge");
    act(&mut editor, remove);
    let (params, request_id) = sent(&editor, "merge.remove");
    assert_eq!(
        params,
        json!({"merge_ids": [merge_id], "mutation": {"request_id": request_id, "actor": "desktop"}})
    );
    settle(&mut editor);
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 5);
    assert_eq!(newest_change(&editor.owner, agent)["actor"], "desktop");

    // Undo puts it back, edited; redo removes it again. Each is said.
    let _ = editor.update(Message::Select(SelectMessage::Undo));
    settle(&mut editor);
    assert!(
        editor.status.text.starts_with("Undid"),
        "{}",
        editor.status.text
    );
    // Back as this client last showed it: expanded, its originals following it.
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 6);
    let back = position_of(&editor, "DSC_0001.NEF") - 1;
    assert!(
        editor
            .select
            .state
            .rows
            .row(back)
            .unwrap()
            .stack
            .as_ref()
            .is_some_and(|stack| {
                stack.role == StackRole::Leader && stack.expansion == Some(StackExpansion::Expanded)
            })
    );
    assert_eq!(
        kit::recipe(&editor.owner, agent, &asset).unwrap(),
        edited,
        "the result comes back with its edit"
    );
    let _ = editor.update(Message::Select(SelectMessage::Redo));
    settle(&mut editor);
    assert!(
        editor.status.text.starts_with("Redid"),
        "{}",
        editor.status.text
    );
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 5);

    // The bracket's header again: this exact merge is in Removed, offered back.
    let (number, labels) = bracket_header(&editor).expect("the bracket again");
    let pick = labels
        .iter()
        .position(|label| label == "Merge HDR")
        .unwrap();
    merge(
        &mut editor,
        MergeMessage::Header {
            moment: number,
            action: pick as u32,
        },
    );
    start(&mut editor);
    let put_back = editor
        .workspace
        .select
        .merge
        .put_back
        .clone()
        .expect("Put back merge offered");
    assert_eq!(put_back.merge_id.to_string(), merge_id);
    assert!(
        editor.select.state.merge.pending.is_none(),
        "nothing merges"
    );
    act(&mut editor, MergeAction::PutBack(put_back.merge_id));
    let (params, _) = sent(&editor, "merge.restore");
    assert_eq!(params["merge_ids"], json!([merge_id]));
    settle(&mut editor);
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 6);
    assert!(
        matches!(&active(&editor).item, RowItem::Photo { asset_id } if asset_id.to_string() == result),
        "the result put back is active"
    );
    assert!(editor.workspace.select.merge.put_back.is_none());
    finish(editor, bench.catalog.clone());
}

/// The pending row's Cancel cancels the job and adds nothing; a merge whose input cannot be read
/// says which in its row until it is dismissed.
#[test]
fn hdr_merge_pending_row_cancels_and_a_failure_names_its_input() {
    let bench = Bench::new("pending");
    let mut editor = bench.open();
    let agent = editor.owner.register();
    let (number, _) = bracket_header(&editor).unwrap();

    bench.reading.shut();
    merge(
        &mut editor,
        MergeMessage::Header {
            moment: number,
            action: 1,
        },
    );
    start(&mut editor);
    assert!(pending_row(&editor).is_some());
    let job = editor.select.state.merge.running_job().unwrap().to_owned();
    // The board's Cancel and the row's are one: `job.cancel` of the merge's job.
    merge(&mut editor, MergeMessage::Pending);
    assert_eq!(editor.long_work.cancels.last(), Some(&job));
    let (_, detail, _, _) = pending_row(&editor).unwrap();
    assert_eq!(detail, "Cancelling\u{2026}");
    // What the cancel task sends.
    crate::app::long_work::cancel_now(&editor.owner, editor.client, &job).unwrap();
    let record = follow(&mut editor);
    assert_eq!(record["status"], "cancelled", "{record}");
    bench.reading.open();
    assert!(pending_row(&editor).is_none());
    assert_eq!(
        editor.status.text,
        "Cancelled the merge \u{b7} nothing was added"
    );
    settle(&mut editor);
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 5);
    assert!(
        call(
            &editor.owner,
            agent,
            "merge.inspect",
            json!({"merge_id": "merge-none"})
        )
        .is_err(),
        "nothing was merged"
    );

    // The camera cannot read the middle frame: the row says so, naming it.
    bench.broken.store(true, Ordering::SeqCst);
    merge(
        &mut editor,
        MergeMessage::Header {
            moment: number,
            action: 1,
        },
    );
    start(&mut editor);
    let record = follow(&mut editor);
    assert_eq!(record["status"], "failed", "{record}");
    let (title, detail, button, failed) = pending_row(&editor).expect("the failed row");
    assert!(failed);
    assert_eq!(title, "HDR merge failed \u{b7} 3 exposures");
    assert!(detail.contains("DSC_0002.NEF"), "{detail}");
    assert_eq!(button, "Dismiss");
    merge(&mut editor, MergeMessage::Pending);
    assert!(pending_row(&editor).is_none());
    finish(editor, bench.catalog.clone());
}
