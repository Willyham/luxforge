//! HDR merges in the Select workspace against a real owner, merging real files through a synthetic
//! camera (`OwnerHandle::merge_synthetic_frames`): everything after each frame's development is the
//! production path. The merge preview sheet's requests and revisions beside a JSON client's, its
//! settings, folder, Cancel and a merge continuing its preview (also one pressed while it
//! prepares), and ⌥-click merging with the remembered settings; then, ⌥-clicked, a bracket
//! header's Merge HDR and a selection's, the core's refusal said in the menu, the one pending row
//! with its Cancel and a failure naming its input, the result in the catalog folder the plan names,
//! leading its collapsed stack with its badge while Select stays shown, `S` expanding and
//! collapsing it, an original picked as any file is, a merge reused, Remove merge with library undo
//! and redo, and Put back merge for a merge in Removed. Each owner call runs as its task would,
//! through the `*_now` function the task runs, its answer handed back as the task's message; what
//! the desktop sends is compared with what a second JSON client writes, and every effect is read
//! back through that client.
use super::select_owner_tests::{
    click, evaluate, finish, key, newest_change, pick_of, read_rows, selecting_with_preferences,
    sent, settle,
};
use crate::{
    app::{
        Editor,
        message::{
            Message, preferences::PreferenceMessage, select::SelectMessage,
            select_merge::MergeMessage,
        },
        select_merge::{
            close_now, expanded_now, open_now, picture_now, plan_now, preview_now, start_now,
        },
        tasks::call,
    },
    state::{
        select::{Block, InfoModel, MomentAction},
        select_merge::{
            FolderPick, MergeAction, MergeRequest, PreviewSheet, Toggle, ev_words, folder_choice,
            plan_refusal,
            sheet::{self, Picture, SheetContext},
        },
    },
};
use iced::{Point, Size, keyboard::Modifiers};
use luxforge_core::{
    ClientId, SourceTag, SyntheticFrame,
    catalog_types::{
        CameraBody, CaptureTime, CatalogFolderId, Dimensions, ExifOrientation, Exposure,
        FileRecord, FileSignature, FolderChoice, HeaderMetadata, HeaderState, IndexRoot,
        IndexedFolder, MergeKind, MergePlan, MergePreview, RootKind, RowItem, StackExpansion,
        StackRole, ViewSource, Volume, VolumeId,
    },
    merge::hdr::{AlignmentMode, DeghostMode},
    preferences::HdrMergePreference,
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
        // The person's preferences beside the catalog: a merge remembers its settings there.
        let preferences = self.catalog.with_extension("config");
        let (mut editor, _) = selecting_with_preferences(self.catalog.clone(), preferences);
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
    // A merge from the sheet continues its preview; with ⌥ held there is none.
    let preview = editor
        .select
        .state
        .merge
        .sheet
        .as_ref()
        .filter(|sheet| sheet.merging)
        .and_then(|sheet| sheet.preview_id.clone());
    let result = start_now(&editor.owner, editor.client, &request, preview.as_ref());
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
fn slow_hdr_merge_from_a_bracket_header_leads_a_collapsed_stack_in_the_plans_folder() {
    let bench = Bench::new("pick");
    let mut editor = bench.open();
    // ⌥ held: each merge action merges at once with the last settings, without the preview.
    merge(&mut editor, MergeMessage::Option(true));
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
    let request = start(&mut editor);
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
    // the folder choice the plan suggested, whose folders the merge makes; nothing else first.
    let files: Vec<Value> = (0..3)
        .map(|at| match editor.select.state.rows.row(at).unwrap().item {
            RowItem::File { file_id } => json!(file_id),
            _ => panic!("a file"),
        })
        .collect();
    let planned = sent_merge(&editor, "merge.plan");
    assert_eq!(
        planned,
        json!({"kind": "hdr", "targets": {"kind": "files", "file_ids": files}})
    );
    let (plan, _) = call(&editor.owner, agent, "merge.plan", planned).unwrap();
    let plan: MergePlan = serde_json::from_value(plan).unwrap();
    assert!(plan_refusal(&plan).is_none(), "{plan:?}");
    let methods: Vec<&Value> = editor
        .select
        .merge_sent
        .as_ref()
        .and_then(Value::as_array)
        .map(|sent| sent.iter().map(|sent| &sent["method"]).collect())
        .unwrap_or_default();
    assert_eq!(methods, [&json!("merge.plan"), &json!("merge.create")]);
    let Some(FolderChoice::Place { name: place }) = plan.suggested_folder.clone() else {
        panic!("a new catalog's merge is filed by place: {plan:?}");
    };
    assert!(plan.folder.is_none(), "{plan:?}");
    let created = sent_merge(&editor, "merge.create");
    assert_eq!(created["folder"], json!({"kind": "place", "name": place}));
    // The preview sheet says the place the merge makes; a folder chosen there is sent as itself.
    let mut sheet = PreviewSheet::new(request.clone(), SheetContext::default(), Vec::new());
    sheet.planned(plan.clone());
    let model = sheet::model(&sheet, None);
    assert_eq!(
        (model.folder.as_str(), model.folder_note),
        (place.as_str(), Some("new place"))
    );
    let chosen = CatalogFolderId::new();
    sheet.request.options.folder = FolderPick::Existing {
        folder_id: chosen.clone(),
        name: "Travel \u{203a} Alps".into(),
    };
    assert_eq!(sheet::model(&sheet, None).folder, "Travel \u{203a} Alps");
    assert_eq!(
        folder_choice(&sheet.request.options, &plan),
        Some(FolderChoice::Existing {
            folder_id: chosen,
            days: false,
        })
    );
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
    assert_eq!((stack.kind, stack.size), (MergeKind::Hdr, 3));
    assert_eq!(stack.collapsed, 3);
    let at = editor.session.browse.selection.active.unwrap();
    assert!(
        !editor.select.state.rows.cell(at).unwrap().picked,
        "a result is never shown picked"
    );
    // In the folder of the place the plan named, as a second client reads it.
    let folders: luxforge_core::catalog_types::CatalogFolders = serde_json::from_value(
        call(&editor.owner, agent, "folder.list", json!({}))
            .unwrap()
            .0,
    )
    .unwrap();
    let by_id = |id: Option<&CatalogFolderId>| {
        folders
            .folders
            .iter()
            .find(|held| Some(&held.id) == id)
            .expect("a listed folder")
    };
    // The bracket's frames carry no capture day, so the place is filed under Undated, with no day.
    let named = by_id(leader.folder_id.as_ref());
    assert_eq!(named.count, 1);
    assert!(named.name.eq_ignore_ascii_case(&place), "{named:?}");
    let undated = by_id(named.parent_id.as_ref());
    assert!(
        undated.name == "Undated" && undated.parent_id.is_none(),
        "{undated:?}"
    );
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
fn slow_hdr_merge_from_a_selection_refuses_imports_reuses_removes_and_puts_back() {
    let bench = Bench::new("selection");
    let mut editor = bench.open();
    // ⌥ held: each merge action merges at once with the last settings, without the preview.
    merge(&mut editor, MergeMessage::Option(true));
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
    assert_eq!(menu.heading, Some("HDR"));
    assert_eq!(menu.entries[0].label, "Merge HDR");
    assert_eq!(
        menu.entries[0].hint,
        Some("Merge the frames into one HDR photograph in the catalog")
    );
    assert_eq!(menu.entries.len(), 1, "one merge action: {menu:?}");
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
    assert_eq!(menu.entries.len(), 1, "one merge action: {menu:?}");
    act(&mut editor, MergeAction::Start(MergeKind::Hdr));
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
    act(&mut editor, MergeAction::Start(MergeKind::Hdr));
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
    // ⌥ held: each merge action merges at once with the last settings, without the preview.
    merge(&mut editor, MergeMessage::Option(true));
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

// -- The merge preview sheet ------------------------------------------------------------------------

/// Wait on `job` as a `job.wait` reader does, handing each record to `deliver`, until it ends; its
/// last record.
fn follow_job(
    editor: &mut Editor,
    client: ClientId,
    job: &str,
    mut deliver: impl FnMut(&mut Editor, Value),
) -> Value {
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
                    super::job_reads::wait(&editor.owner, client, job, after),
                )
                .await
            })
            .expect("the job to move")
            .unwrap();
        after = Some(change);
        deliver(editor, record.clone());
        if !matches!(record["status"].as_str(), Some("queued" | "running")) {
            return record;
        }
    }
}

/// The open sheet.
fn sheet(editor: &Editor) -> &PreviewSheet {
    editor
        .select
        .state
        .merge
        .sheet
        .as_ref()
        .expect("the preview sheet open")
}

/// Run the sheet's opening, as its owner task does: the plan, then `merge.preview`.
fn open_preview(editor: &mut Editor) {
    let (serial, request) = {
        let sheet = sheet(editor);
        assert!(sheet.asking, "the sheet's opening in flight");
        (sheet.serial, sheet.request.clone())
    };
    let answer = open_now(&editor.owner, editor.client, &request);
    merge(
        editor,
        MergeMessage::Opened {
            serial,
            answer: Box::new(answer),
        },
    );
}

/// Run the `merge.preview-update` the sheet sent, as its owner task does; its parameters.
fn update_preview(editor: &mut Editor) -> Value {
    let (serial, sent) = {
        let sheet = sheet(editor);
        assert!(sheet.asking, "the sheet's update in flight");
        (sheet.serial, sheet.sent.last().cloned().unwrap())
    };
    assert_eq!(sent["method"], "merge.preview-update");
    let result = preview_now(
        &editor.owner,
        editor.client,
        "merge.preview-update",
        sent["params"].clone(),
    );
    merge(editor, MergeMessage::Updated { serial, result });
    sent["params"].clone()
}

/// Follow the sheet's preview job to its revision, then decode its picture as the update loop
/// does; the job's last record.
fn follow_preview(editor: &mut Editor) -> Value {
    let job = sheet(editor).job.clone().expect("a preview job");
    let client = editor.client;
    let record = follow_job(editor, client, &job, |editor, record| {
        merge(
            editor,
            MergeMessage::PreviewWatched {
                job: job.clone(),
                result: Ok(record),
            },
        );
    });
    if let Some(path) = editor.select.merge_picture_wanted.clone() {
        let result = picture_now(&path);
        merge(editor, MergeMessage::Picture { path, result });
    }
    record
}

/// A JSON client's own preview of the same frames: `merge.plan`, then `merge.preview` of
/// `params` with its own plan, followed to its revision.
fn client_preview(
    editor: &mut Editor,
    agent: ClientId,
    plan: &Value,
    mut params: Value,
) -> MergePreview {
    let (planned, _) = call(&editor.owner, agent, "merge.plan", plan.clone()).unwrap();
    params["plan_id"] = planned["plan_id"].clone();
    let (answer, _) = call(&editor.owner, agent, "merge.preview", params).unwrap();
    let job = answer["job_id"].as_str().unwrap().to_owned();
    let record = follow_job(editor, agent, &job, |_, _| {});
    assert_eq!(record["status"], "ready", "{record}");
    serde_json::from_value(record["result"].clone()).unwrap()
}

/// The same revision, but for the preview's own identity and picture.
fn same_revision(desktop: &MergePreview, client: &MergePreview) {
    assert_eq!(desktop.revision, client.revision);
    assert_eq!(desktop.kind, client.kind);
    assert_eq!(desktop.options, client.options);
    assert_eq!(
        (desktop.width, desktop.height),
        (client.width, client.height)
    );
    assert_eq!(desktop.diagnostics, client.diagnostics);
    assert_eq!(desktop.overlays, client.overlays);
    assert_eq!(desktop.output, client.output);
    assert_eq!(desktop.refusal, client.refusal);
}

/// Complete the preference write in flight, as its owner task does.
fn save_preferences(editor: &mut Editor) {
    let change = editor
        .preferences
        .writing()
        .cloned()
        .expect("a preference write in flight");
    let result = call(
        &editor.owner,
        editor.client,
        "preferences.set",
        change.params(),
    )
    .and_then(|(answer, _)| Ok((crate::state::preferences::parse(answer)?, String::new())));
    let _ = editor.update(Message::Preferences(PreferenceMessage::Saved(result)));
}

/// Merge HDR opens the sheet, whose requests are a JSON client's and whose revisions equal that
/// client's, with the plan's folder; Deghosting Off updates the same preview with the whole
/// options; the overlay sends nothing; the sheet's Merge continues the preview with its options
/// into the plan's folder choice, closes the sheet, shows the pending row and remembers the
/// settings for the next ⌥-click.
#[test]
fn slow_merge_preview_sheet_requests_and_revisions_are_a_json_clients() {
    let bench = Bench::new("preview");
    let mut editor = bench.open();
    let agent = editor.owner.register();
    let (number, _) = bracket_header(&editor).unwrap();

    merge(
        &mut editor,
        MergeMessage::Header {
            moment: number,
            action: 1,
        },
    );
    assert!(editor.select.state.merge.starting.is_none(), "no merge yet");
    let model = editor
        .workspace
        .select
        .merge
        .sheet
        .clone()
        .expect("the sheet");
    assert_eq!(model.title, "Merge HDR \u{b7} 3 exposures");
    assert!(
        model.subtitle.starts_with("DSC_0001\u{2013}0003.NEF"),
        "{}",
        model.subtitle
    );
    assert!(matches!(model.picture, Picture::Preparing { .. }));
    open_preview(&mut editor);
    let sent = sheet(&editor).sent.clone();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[0]["method"], "merge.plan");
    assert_eq!(sent[1]["method"], "merge.preview");
    let plan_params = sent[0]["params"].clone();
    let preview_params = sent[1]["params"].clone();
    // What a JSON client writes for this bracket with the defaults.
    assert_eq!(plan_params["kind"], "hdr");
    assert!(plan_params.get("hdr").is_none(), "{plan_params}");
    assert!(preview_params["plan_id"].is_string());
    assert_eq!(preview_params["targets"], plan_params["targets"]);
    assert!(
        sheet(&editor).preview_id.is_none(),
        "named with its revision"
    );

    let record = follow_preview(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let sheet_now = sheet(&editor).clone();
    let preview = sheet_now.preview.clone().unwrap();
    assert_eq!(preview.revision, 1);
    let path = preview.path.clone().unwrap();
    assert!(path.is_file(), "{}", path.display());
    assert_eq!(
        editor
            .select
            .merge_picture
            .as_ref()
            .map(|picture| &picture.path),
        Some(&path)
    );
    let ours = client_preview(&mut editor, agent, &plan_params, preview_params.clone());
    same_revision(&preview, &ours);
    let model = editor.workspace.select.merge.sheet.clone().unwrap();
    assert!(model.enabled);
    assert!(
        model.status.starts_with("Preview from ") && model.status.contains("-pixel frames"),
        "{}",
        model.status
    );
    assert_eq!(model.note, Some(sheet::DISPLAY_NOTE));
    assert_eq!(model.frames.len(), 3);
    assert_eq!(
        model
            .frames
            .iter()
            .map(|frame| frame.name.as_str())
            .collect::<Vec<_>>(),
        NAMES[..3]
    );
    assert!(model.frames[1].reference, "{:?}", model.frames);
    assert!(
        model.frames[0].detail.starts_with("\u{2212}2 EV"),
        "{:?}",
        model.frames[0]
    );
    assert!(
        model
            .output
            .as_deref()
            .is_some_and(|output| output.contains(" MB"))
    );
    // The folder: the place the plan suggests, whose folders the merge makes.
    let Some(FolderChoice::Place { name: place }) = sheet_now
        .plan
        .as_ref()
        .and_then(|plan| plan.suggested_folder.clone())
    else {
        panic!(
            "a new catalog's merge is filed by place: {:?}",
            sheet_now.plan
        );
    };
    assert_eq!(
        (model.folder.as_str(), model.folder_note),
        (place.as_str(), Some("new place"))
    );

    // Deghosting Off: the whole options, to the same preview, drawn as its next revision.
    act(&mut editor, MergeAction::Deghost(Toggle::Off));
    let params = update_preview(&mut editor);
    let preview_id = sheet(&editor).preview_id.clone().unwrap();
    assert_eq!(
        params,
        json!({"preview_id": preview_id, "hdr": {"deghost": "off"}})
    );
    let record = follow_preview(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let updated = sheet(&editor).preview.clone().unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.preview_id, preview_id);
    let (answer, _) = call(
        &editor.owner,
        agent,
        "merge.preview-update",
        json!({"preview_id": ours.preview_id, "hdr": {"deghost": "off"}}),
    )
    .unwrap();
    let job = answer["job_id"].as_str().unwrap().to_owned();
    let record = follow_job(&mut editor, agent, &job, |_, _| {});
    let theirs: MergePreview = serde_json::from_value(record["result"].clone()).unwrap();
    same_revision(&updated, &theirs);
    assert_eq!(
        editor
            .select
            .merge_picture
            .as_ref()
            .map(|picture| &picture.path),
        updated.path.as_ref()
    );

    // Show deghost overlay: drawn from the revision's data, nothing sent.
    let before = sheet(&editor).sent.len();
    act(&mut editor, MergeAction::Overlay(true));
    assert_eq!(sheet(&editor).sent.len(), before);
    assert!(sheet(&editor).overlay);

    // The sheet's Merge: the preview continued with its options, no plan named, into the plan's
    // folder choice.
    act(&mut editor, MergeAction::SheetMerge);
    let request = start(&mut editor);
    assert_eq!(request.kind, MergeKind::Hdr);
    let create = sent_merge(&editor, "merge.create");
    assert_eq!(create["preview_id"], json!(preview_id));
    assert!(create.get("plan_id").is_none(), "{create}");
    assert_eq!(create["hdr"], json!({"deghost": "off"}));
    assert_eq!(create["folder"], json!({"kind": "place", "name": place}));
    for removed in ["folder_id", "add_to_development_set"] {
        assert!(create.get(removed).is_none(), "{create}");
    }
    assert!(
        editor.select.state.merge.sheet.is_none(),
        "the sheet is gone"
    );
    assert!(editor.select.merge_picture.is_none());
    assert!(pending_row(&editor).is_some());
    // Remembered for the next ⌥-click: its alignment and deghosting.
    assert_eq!(
        editor.preferences.applied().unwrap().hdr_merge,
        Some(HdrMergePreference {
            alignment: AlignmentMode::Auto,
            deghost: DeghostMode::Off,
        })
    );
    save_preferences(&mut editor);
    let (stored, _) = call(&editor.owner, agent, "preferences.read", json!({})).unwrap();
    assert_eq!(
        stored["hdr_merge"],
        json!({"alignment": "auto", "deghost": "off"})
    );
    let record = follow(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    // The preview is merged: it is gone with its merge.
    assert!(
        call(
            &editor.owner,
            editor.client,
            "merge.preview-update",
            json!({"preview_id": preview_id, "hdr": {}}),
        )
        .is_err()
    );
    let _ = close_now(&editor.owner, agent, json!({"preview_id": ours.preview_id}));
    settle(&mut editor);
    finish(editor, bench.catalog.clone());
}

/// The sheet opens with the remembered settings and its Cancel closes the preview; ⌥-click merges
/// at once with them, naming the plan and no preview; a merge pressed while the preview still
/// prepares waits for its revision and continues it.
#[test]
fn slow_merge_preview_option_click_merges_with_the_remembered_settings() {
    let bench = Bench::new("option");
    let mut editor = bench.open();
    let agent = editor.owner.register();
    let (number, _) = bracket_header(&editor).unwrap();
    let remembered = json!({"alignment": "off", "deghost": "off"});
    call(
        &editor.owner,
        agent,
        "preferences.set",
        json!({"hdr_merge": remembered}),
    )
    .unwrap();
    let (stored, _) = call(&editor.owner, editor.client, "preferences.read", json!({})).unwrap();
    let _ = editor.preferences_read(crate::state::preferences::parse(stored));

    // A plain press opens the sheet with the last settings; Cancel closes its preview.
    merge(
        &mut editor,
        MergeMessage::Header {
            moment: number,
            action: 1,
        },
    );
    assert_eq!(sheet(&editor).request.options.alignment, Toggle::Off);
    assert_eq!(sheet(&editor).request.options.deghost, Toggle::Off);
    open_preview(&mut editor);
    assert_eq!(sheet(&editor).sent[1]["params"]["hdr"], remembered);
    let record = follow_preview(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let model = editor.workspace.select.merge.sheet.clone().unwrap();
    assert!(model.status.ends_with("not aligned"), "{}", model.status);
    let preview_id = sheet(&editor).preview_id.clone().unwrap();
    act(&mut editor, MergeAction::SheetClose);
    assert!(editor.select.state.merge.sheet.is_none());
    let closed = editor.select.preview_closed.clone().unwrap();
    assert_eq!(closed, json!({"preview_id": preview_id}));
    close_now(&editor.owner, editor.client, closed).unwrap();
    assert!(
        call(
            &editor.owner,
            editor.client,
            "merge.preview-update",
            json!({"preview_id": preview_id, "hdr": {}}),
        )
        .is_err(),
        "the preview is closed"
    );

    // ⌥-click: a merge at once with the last settings, of the plan, no preview opened.
    merge(&mut editor, MergeMessage::Option(true));
    merge(
        &mut editor,
        MergeMessage::Header {
            moment: number,
            action: 1,
        },
    );
    merge(&mut editor, MergeMessage::Option(false));
    assert!(editor.select.state.merge.sheet.is_none(), "no sheet");
    start(&mut editor);
    let plan = sent_merge(&editor, "merge.plan");
    let create = sent_merge(&editor, "merge.create");
    assert_eq!(plan["hdr"], remembered);
    assert_eq!(create["hdr"], remembered);
    assert!(create["plan_id"].is_string(), "{create}");
    assert!(create.get("preview_id").is_none(), "{create}");
    assert!(create["folder"].is_object(), "{create}");
    let record = follow(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    settle(&mut editor);
    finish(editor, bench.catalog.clone());
}

/// A merge pressed while the preview still reads its frames waits for the preview's revision and
/// then continues it: one read of the frames, the merge naming the preview.
#[test]
fn slow_merge_preview_merge_while_preparing_continues_the_preview() {
    let bench = Bench::new("preparing");
    let mut editor = bench.open();
    let (number, _) = bracket_header(&editor).unwrap();
    merge(
        &mut editor,
        MergeMessage::Header {
            moment: number,
            action: 1,
        },
    );
    open_preview(&mut editor);
    assert!(sheet(&editor).job.is_some() && sheet(&editor).preview_id.is_none());
    act(&mut editor, MergeAction::SheetMerge);
    assert!(editor.select.state.merge.starting.is_none(), "it waits");
    let model = editor.workspace.select.merge.sheet.clone().unwrap();
    assert!(!model.enabled);
    assert_eq!(model.reason.as_deref(), Some("Starting the merge\u{2026}"));
    let record = follow_preview(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let preview_id = record["result"]["preview_id"].clone();
    assert!(editor.select.state.merge.starting.is_some(), "it started");
    start(&mut editor);
    let create = sent_merge(&editor, "merge.create");
    assert_eq!(create["preview_id"], preview_id);
    assert!(create.get("plan_id").is_none());
    assert!(editor.select.state.merge.sheet.is_none());
    let record = follow(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    settle(&mut editor);
    finish(editor, bench.catalog.clone());
}
