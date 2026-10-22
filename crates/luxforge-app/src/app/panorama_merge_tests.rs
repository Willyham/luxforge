//! Panorama merges in the Select workspace against a real owner, stitching real JPEG frames of a
//! synthetic scene (`luxforge_reference::panorama`) through the production path: the shared frame
//! reads, registration, the production stitch, the durable writer and the publication. The
//! selection menu offering both kinds, each planned at once, with the core's refusal on HDR's
//! item; the panorama preview sheet's requests and revisions beside a JSON client's, its
//! projection, Boundary Warp, reference and overlays, Cancel, and Merge continuing the preview;
//! ⌥-click merging with the remembered projection and Boundary Warp; the pending row and a failed
//! one naming its stage; and the result's badge, its originals' "Frame k of N", its Info band's
//! projection and field of view read with `merge.inspect`, and the view's "1 merge". Only the
//! failure and the paged result stitch with the core's test stitch
//! (`OwnerHandle::merge_panorama_test_stitch`), the one to refuse, the other to publish a result
//! larger than a resident source may be from small frames. Each owner call runs as its task would,
//! through the `*_now` function the task runs, its answer handed back as the task's message; what
//! the desktop sends is compared with what a second JSON client writes.
use super::{
    hdr_merge_tests::{
        act, active, client_preview, context, expand, extend, follow, follow_job, follow_preview,
        merge, open_preview, pending_row, plan_all, position_of, same_revision, save_preferences,
        sent_merge, sheet, start, update_preview,
    },
    select_owner_tests::{
        click, evaluate, finish, key, read_rows, selecting_with_preferences, settle,
    },
};
use crate::{
    app::{
        Editor,
        message::{Message, select::SelectMessage, select_merge::MergeMessage},
        select_merge::{close_now, inspect_now},
        tasks::call,
    },
    state::{
        select::{Block, InfoModel, MomentAction},
        select_merge::{KindIcon, MergeAction, plan_refusal, sheet::Picture, stack_cell},
    },
};
use iced::{Size, keyboard::Modifiers};
use luxforge_core::{
    SourceTag,
    catalog_types::{
        CaptureTime, Dimensions, ExifOrientation, FileRecord, FileSignature, HeaderMetadata,
        HeaderState, IndexRoot, IndexedFolder, MergeKind, MergePlan, MergePreview, MomentKind,
        PanoramaProjection, RootKind, RowItem, StackRole, ViewSource, Volume, VolumeId,
    },
    preferences::PanoramaMergePreference,
    seed::{CatalogSeeder, IndexSeeder},
};
use luxforge_reference::panorama::{self as reference, Pose, Scene, Shot};
use luxforge_test_macros::slow;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

const CATALOG_ID: &str = "catalog-panorama-merge-test";
/// Each frame's size and horizontal field of view, and the turn between neighbours: three frames
/// span about 120°, which Auto shows cylindrical.
const W: usize = 320;
const H: usize = 240;
const HFOV: f64 = 60.0;
const STEP: f64 = 30.0;
/// The scene's cell, about 17 pixels at the frames' focal length, and its brightness.
const CELL: f64 = 0.06;
const GAIN: f64 = 0.5;
const SEED: u64 = 37;
/// The row's three frames, then a single frame of another scene.
const NAMES: [&str; 4] = [
    "IMG_0001.JPG",
    "IMG_0002.JPG",
    "IMG_0003.JPG",
    "IMG_0009.JPG",
];
const TIMES: [&str; 4] = [
    "2026:09:12 09:00:00",
    "2026:09:12 09:00:05",
    "2026:09:12 09:00:10",
    "2026:09:12 11:30:00",
];

fn volume() -> VolumeId {
    VolumeId::parse("volume-pano-0000000").unwrap()
}

/// Write the frame of `scene` taken at `pose` as an sRGB JPEG at `path`.
fn write_frame(path: &Path, seed: u64, pose: Pose) {
    let frame = reference::record(
        &Scene::new(seed, CELL),
        &reference::Lens::with_hfov(W, H, HFOV),
        &Shot {
            gain: [GAIN; 3],
            ..Shot::at(pose)
        },
        None,
        0,
    );
    let mut rgb = Vec::with_capacity(W * H * 3);
    for y in 0..H {
        for x in 0..W {
            rgb.extend(frame.at(x, y).map(luxforge_reference::srgb::code));
        }
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 100)
        .encode(&rgb, W as u32, H as u32, image::ExtendedColorType::Rgb8)
        .unwrap();
    fs::write(path, out).unwrap();
}

/// The row's folder beside its catalog: four real JPEG files, indexed with their headers.
struct Bench {
    folder: PathBuf,
    catalog: PathBuf,
}

impl Bench {
    fn new(name: &str) -> Self {
        let directory = luxforge_testbase::paths::temp_dir(&format!("panorama-merge-{name}"))
            .canonicalize()
            .unwrap();
        let folder = directory.join("row");
        fs::create_dir_all(&folder).unwrap();
        let mut poses = reference::row(3, STEP, 0.0);
        poses.push(Pose::new(150.0, 20.0, 0.0));
        let mut files = Vec::new();
        for (at, (name, pose)) in NAMES.iter().zip(poses).enumerate() {
            let path = folder.join(name);
            write_frame(&path, if at < 3 { SEED } else { SEED + 1 }, pose);
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
        Self { folder, catalog }
    }

    /// The editor over the catalog with the row's folder in view, every row read; its owner
    /// stitches with the production stitch.
    fn open(&self) -> Editor {
        let preferences = self.catalog.with_extension("config");
        let (mut editor, _) = selecting_with_preferences(self.catalog.clone(), preferences);
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

/// One frame's index record: a 320 × 240 JPEG taken at its time.
fn record(folder: &Path, path: &Path, at: usize) -> FileRecord {
    let header = HeaderMetadata {
        capture: CaptureTime::from_exif(TIMES[at], None, Some("+02:00")),
        dimensions: Some(Dimensions {
            width: W as u32,
            height: H as u32,
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
        kind: SourceTag::Jpeg,
        header: HeaderState::Ok(Box::new(header)),
        last_seen_ms: 1,
        born_ns: None,
    }
}

/// Select the row's three frames and open the menu over them; every plan it sends run, by kind.
fn menu_over_the_row(editor: &mut Editor) -> Vec<Value> {
    let first = position_of(editor, NAMES[0]);
    click(editor, first);
    extend(editor, first + 2);
    context(editor, first + 2);
    assert!(
        !editor.merge_reads_quiet(),
        "the menu's plans are in flight"
    );
    plan_all(editor)
}

/// The grid's pending row's kind, when it draws one.
fn pending_kind(editor: &Editor) -> Option<MergeKind> {
    editor
        .select
        .state
        .content
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Pending { kind, .. } => Some(*kind),
            _ => None,
        })
}

/// Run the `merge.inspect` the Info band sent, as its owner task does; its parameters.
fn inspect(editor: &mut Editor) -> Value {
    let params = editor
        .select
        .inspect_sent
        .take()
        .expect("a merge.inspect in flight");
    let merge_id = serde_json::from_value(params["merge_id"].clone()).unwrap();
    let result = inspect_now(&editor.owner, editor.client, params.clone());
    merge(editor, MergeMessage::Inspected { merge_id, result });
    params
}

/// The menu over a row of frames offers Merge HDR… and Merge panorama…, each planned at once as a
/// JSON client plans them; HDR's is refused with the core's reason. Merge panorama opens the
/// sheet, whose requests are a JSON client's and whose revisions equal that client's; Projection,
/// Boundary Warp (sent once let go) and the reference update the same preview with the whole
/// options; the overlay switches send nothing. The sheet's Merge continues the preview into the
/// pending row and remembers the settings; the result leads its stack with its own words, its
/// originals are "Frame k of 3", its band reads its projection and field of view, and the view
/// counts it as a merge.
#[test]
#[slow]
fn panorama_merge_menu_and_sheet_requests_are_a_json_clients() {
    let bench = Bench::new("sheet");
    let mut editor = bench.open();
    let agent = editor.owner.register();
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 4);
    // A moment header offers Merge panorama only over a panorama, and Merge HDR only over a
    // bracket.
    for block in &editor.select.state.content.blocks {
        if let Block::Moment { kind, actions, .. } = block {
            let offers = |merge| actions.contains(&MomentAction::Merge(merge));
            assert_eq!(
                offers(MergeKind::Panorama),
                *kind == MomentKind::Panorama,
                "{kind:?} {actions:?}"
            );
            assert_eq!(
                offers(MergeKind::Hdr),
                *kind == MomentKind::Bracket,
                "{kind:?} {actions:?}"
            );
        }
    }

    // The menu plans each kind it offers at once, as a JSON client would.
    let plans = menu_over_the_row(&mut editor);
    let kinds: Vec<&Value> = plans.iter().map(|params| &params["kind"]).collect();
    assert_eq!(kinds, [&json!("hdr"), &json!("panorama")]);
    // Each answer is kept for evidence; until all have come, Select's reads are not quiet.
    let answered: Vec<&Value> = editor
        .select
        .plan_answers
        .iter()
        .map(|answer| &answer["kind"])
        .collect();
    assert_eq!(answered, [&json!("hdr"), &json!("panorama")]);
    assert!(editor.merge_reads_quiet());
    let files: Vec<Value> = (0..3)
        .map(|at| {
            let at = position_of(&editor, NAMES[at]);
            match editor.select.state.rows.row(at).unwrap().item {
                RowItem::File { file_id } => json!(file_id),
                _ => panic!("a file"),
            }
        })
        .collect();
    for params in &plans {
        assert_eq!(
            params,
            &json!({"kind": params["kind"], "targets": {"kind": "files", "file_ids": files}})
        );
    }
    let answer = |params: &Value| -> MergePlan {
        serde_json::from_value(
            call(&editor.owner, agent, "merge.plan", params.clone())
                .unwrap()
                .0,
        )
        .unwrap()
    };
    let hdr_reason = plan_refusal(&answer(&plans[0])).expect("JPEG frames are no HDR bracket");
    let panorama_plan = answer(&plans[1]);
    assert!(plan_refusal(&panorama_plan).is_none(), "{panorama_plan:?}");
    let menu = editor
        .workspace
        .select
        .merge
        .menu
        .clone()
        .expect("the menu");
    let hdr = &menu.groups[0];
    assert_eq!(hdr.heading, Some("HDR"));
    assert_eq!(hdr.entries[0].label, "Merge HDR\u{2026}");
    assert!(hdr.entries[0].action.is_none());
    assert_eq!(hdr.entries[0].reason.as_deref(), Some(hdr_reason.as_str()));
    let panorama = &menu.groups[1];
    assert_eq!(panorama.heading, Some("Panorama"));
    assert_eq!(panorama.entries[0].label, "Merge panorama\u{2026}");
    assert_eq!(
        panorama.entries[0].hint,
        Some("Merge the frames into one panorama in the catalog")
    );
    assert_eq!(
        panorama.entries[0].action,
        Some(MergeAction::Start(MergeKind::Panorama))
    );

    // Merge panorama…: the sheet, preparing.
    act(&mut editor, MergeAction::Start(MergeKind::Panorama));
    assert!(editor.select.state.merge.starting.is_none(), "no merge yet");
    let model = editor
        .workspace
        .select
        .merge
        .sheet
        .clone()
        .expect("the sheet");
    assert_eq!(model.title, "Merge panorama \u{b7} 3 photos");
    assert!(matches!(model.picture, Picture::Preparing { .. }));
    assert!(model.panorama.is_some() && model.enabled);
    open_preview(&mut editor);
    let sent = sheet(&editor).sent.clone();
    assert_eq!(sent[0]["method"], "merge.plan");
    assert_eq!(sent[1]["method"], "merge.preview");
    let plan_params = sent[0]["params"].clone();
    let preview_params = sent[1]["params"].clone();
    assert_eq!(plan_params, plans[1], "the menu's plan request");
    assert!(preview_params.get("panorama").is_none(), "{preview_params}");
    let record = follow_preview(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let preview = sheet(&editor).preview.clone().unwrap();
    let ours = client_preview(&mut editor, agent, &plan_params, preview_params.clone());
    same_revision(&preview, &ours);
    let model = editor.workspace.select.merge.sheet.clone().unwrap();
    assert!(model.enabled);
    assert!(
        model.status.starts_with("Preview from ")
            && model.status.contains("Crop loses ")
            && model.status.ends_with(" at the edges"),
        "{}",
        model.status
    );
    let settings = model.panorama.clone().unwrap();
    assert_eq!(settings.projection, PanoramaProjection::Auto);
    assert!(
        settings
            .resolved
            .as_deref()
            .is_some_and(|resolved| resolved.starts_with("Auto chose ")),
        "{settings:?}"
    );
    assert_eq!(settings.frames_legend, "Frames 1\u{2013}3");
    let numbers: Vec<Option<u32>> = model.frames.iter().map(|frame| frame.number).collect();
    assert_eq!(numbers, [Some(1), Some(2), Some(3)]);
    assert_eq!(
        model.frames.iter().filter(|frame| frame.reference).count(),
        1
    );
    let Picture::Ready {
        stitch: Some(stitch),
        ..
    } = &model.picture
    else {
        panic!("a stitched picture: {:?}", model.picture);
    };
    assert!(stitch.crop.is_some() && !stitch.covered.is_empty());
    let outlined: Vec<u32> = stitch.frames.iter().map(|(number, _)| *number).collect();
    assert_eq!(outlined, [1, 2, 3]);
    assert!(
        model
            .output
            .as_deref()
            .is_some_and(|output| output.contains(" MP \u{b7} ")),
        "{:?}",
        model.output
    );

    // Projection: the whole options, to the same preview, as a client's update.
    act(
        &mut editor,
        MergeAction::Projection(PanoramaProjection::Spherical),
    );
    let params = update_preview(&mut editor);
    let preview_id = sheet(&editor).preview_id.clone().unwrap();
    assert_eq!(
        params,
        json!({"preview_id": preview_id, "panorama": {"projection": "spherical"}})
    );
    let record = follow_preview(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let updated = sheet(&editor).preview.clone().unwrap();
    let (answer, _) = call(
        &editor.owner,
        agent,
        "merge.preview-update",
        json!({"preview_id": ours.preview_id, "panorama": {"projection": "spherical"}}),
    )
    .unwrap();
    let job = answer["job_id"].as_str().unwrap().to_owned();
    let record = follow_job(&mut editor, agent, &job, |_, _| {});
    let theirs: MergePreview = serde_json::from_value(record["result"].clone()).unwrap();
    same_revision(&updated, &theirs);
    let model = editor.workspace.select.merge.sheet.clone().unwrap();
    assert_eq!(model.panorama.as_ref().unwrap().resolved, None);
    assert!(model.status.contains("Spherical"), "{}", model.status);

    // Boundary Warp: nothing sent while dragged, the whole options once let go.
    let before = sheet(&editor).sent.len();
    act(&mut editor, MergeAction::BoundaryWarp(12));
    act(&mut editor, MergeAction::BoundaryWarp(30));
    assert_eq!(
        sheet(&editor).sent.len(),
        before,
        "nothing sent while dragged"
    );
    assert_eq!(
        editor
            .workspace
            .select
            .merge
            .sheet
            .as_ref()
            .unwrap()
            .panorama
            .as_ref()
            .unwrap()
            .boundary_warp,
        30
    );
    act(&mut editor, MergeAction::BoundaryWarpReleased);
    let params = update_preview(&mut editor);
    assert_eq!(
        params,
        json!({"preview_id": preview_id, "panorama": {"projection": "spherical", "boundary_warp": 30}})
    );
    let record = follow_preview(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");

    // A frame clicked is the reference, by path.
    let last = sheet(&editor).plan.as_ref().unwrap().inputs[2].path.clone();
    act(&mut editor, MergeAction::Reference(last.clone()));
    let params = update_preview(&mut editor);
    assert_eq!(
        params,
        json!({"preview_id": preview_id, "panorama": {
            "reference": last, "projection": "spherical", "boundary_warp": 30,
        }})
    );
    let record = follow_preview(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let model = editor.workspace.select.merge.sheet.clone().unwrap();
    let reference: Vec<&str> = model
        .frames
        .iter()
        .filter(|frame| frame.reference)
        .map(|frame| frame.name.as_str())
        .collect();
    assert_eq!(reference, [NAMES[2]]);

    // The overlay switches draw from the revision's data and send nothing.
    let before = sheet(&editor).sent.len();
    act(&mut editor, MergeAction::FrameOutlines(false));
    act(&mut editor, MergeAction::CropCoverage(false));
    assert_eq!(sheet(&editor).sent.len(), before);
    let model = editor.workspace.select.merge.sheet.clone().unwrap();
    let Picture::Ready {
        stitch: Some(stitch),
        ..
    } = &model.picture
    else {
        panic!("a stitched picture");
    };
    assert!(stitch.frames.is_empty() && stitch.crop.is_none() && stitch.covered.is_empty());

    // Merge: the preview continued with its options, no plan named.
    act(&mut editor, MergeAction::SheetMerge);
    let request = start(&mut editor);
    assert_eq!(request.kind, MergeKind::Panorama);
    let create = sent_merge(&editor, "merge.create");
    assert_eq!(create["kind"], "panorama");
    assert_eq!(create["preview_id"], json!(preview_id));
    assert!(create.get("plan_id").is_none(), "{create}");
    assert_eq!(
        create["panorama"],
        json!({"reference": last, "projection": "spherical", "boundary_warp": 30})
    );
    assert!(
        editor.select.state.merge.sheet.is_none(),
        "the sheet is gone"
    );
    let (title, _, button, failed) = pending_row(&editor).expect("the pending row");
    assert_eq!(title, "Merging panorama \u{b7} 3 photos");
    assert_eq!((button.as_str(), failed), ("Cancel", false));
    assert_eq!(pending_kind(&editor), Some(MergeKind::Panorama));
    // Remembered for the next ⌥-click: the projection and Boundary Warp, not the reference.
    assert_eq!(
        editor.preferences.applied().unwrap().panorama_merge,
        Some(PanoramaMergePreference {
            projection: PanoramaProjection::Spherical,
            boundary_warp: 30,
        })
    );
    assert_eq!(editor.preferences.applied().unwrap().hdr_merge, None);
    save_preferences(&mut editor);
    let (stored, _) = call(&editor.owner, agent, "preferences.read", json!({})).unwrap();
    assert_eq!(
        stored["panorama_merge"],
        json!({"projection": "spherical", "boundary_warp": 30})
    );
    let record = follow(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let _ = close_now(&editor.owner, agent, json!({"preview_id": ours.preview_id}));
    settle(&mut editor);
    assert!(pending_row(&editor).is_none());

    // The result, active, leads its collapsed stack with its own words and icon.
    let summary = editor.select.state.summary.clone().unwrap();
    assert_eq!((summary.count, summary.stacks), (2, 1), "{summary:?}");
    assert_eq!(
        editor.workspace.select.filter.count,
        "1 photograph \u{b7} 1 merge"
    );
    assert!(
        editor
            .workspace
            .select
            .status
            .line
            .contains("1 in view \u{b7} 1 merge \u{b7} "),
        "{}",
        editor.workspace.select.status.line
    );
    let leader = active(&editor);
    let stack = leader.stack.clone().expect("the result leads its stack");
    assert_eq!(
        (stack.kind, stack.role, stack.size),
        (MergeKind::Panorama, StackRole::Leader, 3)
    );
    let cell = stack_cell(Some(&stack), None);
    assert_eq!(
        cell.badge.map(|(long, short, _)| (long, short)),
        Some(("Panorama \u{b7} 3 photos", "Pano \u{b7} 3"))
    );
    assert_eq!(cell.icon, Some(KindIcon::Panorama));

    // Its Info band: "Panorama · 3 photos", then the projection and field of view a client reads.
    let InfoModel::One(info) = &editor.workspace.select.info else {
        panic!("one item: {:?}", editor.workspace.select.info);
    };
    let band = info.stack.as_ref().expect("the stack band");
    assert_eq!(band.title, "Panorama \u{b7} 3 photos");
    assert_eq!(band.detail.as_deref(), Some("Reading the merge\u{2026}"));
    assert!(!editor.merge_reads_quiet(), "the band's read is in flight");
    let params = inspect(&mut editor);
    assert!(editor.merge_reads_quiet());
    assert_eq!(params, json!({"merge_id": stack.merge_id}));
    let (inspected, _) = call(&editor.owner, agent, "merge.inspect", params).unwrap();
    assert_eq!(inspected["projection"], "spherical", "{inspected}");
    let field = inspected["field_of_view_deg"].as_array().unwrap();
    let words = format!(
        "Spherical \u{b7} {}\u{b0} \u{d7} {}\u{b0}",
        field[0].as_f64().unwrap().round(),
        field[1].as_f64().unwrap().round()
    );
    let InfoModel::One(info) = &editor.workspace.select.info else {
        panic!("one item");
    };
    assert_eq!(
        info.stack.as_ref().unwrap().detail.as_deref(),
        Some(words.as_str())
    );
    assert!(editor.select.inspect_sent.is_none(), "read once");

    // `S`: its originals follow it, each "Frame k of 3" in the stack's order.
    key(&mut editor, "s", Modifiers::empty());
    expand(&mut editor);
    let at = editor.session.browse.selection.active.unwrap();
    let footers: Vec<&str> = (1..=3)
        .map(|offset| {
            let row = editor.select.state.rows.row(at + offset).unwrap();
            let stack = row.stack.as_ref().expect("an original of the stack");
            assert_eq!(stack.role, StackRole::Original);
            stack_cell(Some(stack), None).footer.unwrap()
        })
        .collect();
    assert_eq!(footers, ["Frame 1 of 3", "Frame 2 of 3", "Frame 3 of 3"]);
    finish(editor, bench.catalog.clone());
}

/// The sheet opens with the remembered projection and Boundary Warp, and its Cancel closes the
/// preview; ⌥-click merges at once with them, naming the plan and no preview; the menu's item
/// drops its ellipsis while ⌥ is held.
#[test]
#[slow]
fn panorama_merge_option_click_merges_with_the_remembered_settings() {
    let bench = Bench::new("option");
    let mut editor = bench.open();
    let agent = editor.owner.register();
    let remembered = json!({"projection": "cylindrical", "boundary_warp": 20});
    call(
        &editor.owner,
        agent,
        "preferences.set",
        json!({"panorama_merge": remembered}),
    )
    .unwrap();
    let (stored, _) = call(&editor.owner, editor.client, "preferences.read", json!({})).unwrap();
    let _ = editor.preferences_read(crate::state::preferences::parse(stored));

    // A plain press opens the sheet with the last settings; Cancel closes its preview.
    menu_over_the_row(&mut editor);
    act(&mut editor, MergeAction::Start(MergeKind::Panorama));
    let options = &sheet(&editor).request.options;
    assert_eq!(
        (options.projection, options.boundary_warp),
        (PanoramaProjection::Cylindrical, 20)
    );
    open_preview(&mut editor);
    assert_eq!(sheet(&editor).sent[1]["params"]["panorama"], remembered);
    let record = follow_preview(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    let model = editor.workspace.select.merge.sheet.clone().unwrap();
    let settings = model.panorama.unwrap();
    assert_eq!(
        (
            settings.projection,
            settings.boundary_warp,
            settings.resolved
        ),
        (PanoramaProjection::Cylindrical, 20, None)
    );
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
            json!({"preview_id": preview_id, "panorama": {}}),
        )
        .is_err(),
        "the preview is closed"
    );

    // ⌥ held: the item drops its ellipsis and merges at once, of the plan, no preview opened.
    merge(&mut editor, MergeMessage::Option(true));
    menu_over_the_row(&mut editor);
    let menu = editor.workspace.select.merge.menu.clone().unwrap();
    assert_eq!(menu.groups[1].entries[0].label, "Merge panorama");
    act(&mut editor, MergeAction::Start(MergeKind::Panorama));
    merge(&mut editor, MergeMessage::Option(false));
    assert!(editor.select.state.merge.sheet.is_none(), "no sheet");
    start(&mut editor);
    let plan = sent_merge(&editor, "merge.plan");
    let create = sent_merge(&editor, "merge.create");
    assert_eq!(plan["kind"], "panorama");
    assert_eq!(plan["panorama"], remembered);
    assert_eq!(create["panorama"], remembered);
    assert!(create["plan_id"].is_string(), "{create}");
    assert!(create.get("preview_id").is_none(), "{create}");
    let record = follow(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    settle(&mut editor);
    let stack = active(&editor).stack.expect("the result leads its stack");
    let (inspected, _) = call(
        &editor.owner,
        agent,
        "merge.inspect",
        json!({"merge_id": stack.merge_id}),
    )
    .unwrap();
    assert_eq!(
        inspected["parameters"]["panorama"], remembered,
        "{inspected}"
    );
    finish(editor, bench.catalog.clone());
}

/// The pending row's Cancel is `job.cancel` of the merge's job; a stitch the core refuses fails
/// the row with the refusal's words, and Dismiss clears it. The test stitch refuses every plan.
#[test]
#[slow]
fn panorama_merge_pending_row_cancels_and_a_failure_names_its_frame() {
    let bench = Bench::new("pending");
    let mut editor = bench.open();
    merge(&mut editor, MergeMessage::Option(true));

    menu_over_the_row(&mut editor);
    act(&mut editor, MergeAction::Start(MergeKind::Panorama));
    start(&mut editor);
    let job = editor.select.state.merge.running_job().unwrap().to_owned();
    merge(&mut editor, MergeMessage::Pending);
    assert_eq!(editor.long_work.cancels.last(), Some(&job));
    let (_, detail, _, _) = pending_row(&editor).unwrap();
    assert_eq!(detail, "Cancelling\u{2026}");
    crate::app::long_work::cancel_now(&editor.owner, editor.client, &job).unwrap();
    // A merge this small may finish before the cancel reaches it.
    let record = follow(&mut editor);
    assert!(
        matches!(record["status"].as_str(), Some("cancelled" | "ready")),
        "{record}"
    );
    assert!(pending_row(&editor).is_none());
    settle(&mut editor);

    // The stitch refuses: the row fails with its words.
    editor.owner.merge_panorama_test_stitch(1.0, true);
    let first = position_of(&editor, NAMES[0]);
    click(&mut editor, first);
    menu_over_the_row(&mut editor);
    act(&mut editor, MergeAction::Start(MergeKind::Panorama));
    start(&mut editor);
    let record = follow(&mut editor);
    assert_eq!(record["status"], "failed", "{record}");
    let (title, detail, button, failed) = pending_row(&editor).expect("the failed row");
    assert!(failed);
    assert_eq!(title, "Panorama merge failed \u{b7} 3 photos");
    assert!(detail.contains(NAMES[0]), "{detail}");
    assert_eq!(button, "Dismiss");
    assert_eq!(pending_kind(&editor), Some(MergeKind::Panorama));
    merge(&mut editor, MergeMessage::Pending);
    assert!(pending_row(&editor).is_none());
    finish(editor, bench.catalog.clone());
}

/// A panorama larger than a resident source may be (the test stitch magnifies the row past
/// 16,384 pixels a side) is published paged; viewed in its catalog folder, `D` opens it in Develop
/// through the open every photograph takes, its frame drawn with no render error, and Select is
/// shown again after, as it was.
#[test]
#[slow]
fn panorama_merge_paged_result_opens_in_develop_and_select_returns() {
    use crate::app::{
        develop::folder_strip_now,
        message::{develop::DevelopMessage, preview::PreviewMessage, sync::SyncMessage},
        tasks,
    };
    use crate::state::select::Shown;
    let bench = Bench::new("paged");
    let mut editor = bench.open();
    editor.owner.merge_panorama_test_stitch(36.0, false);
    merge(&mut editor, MergeMessage::Option(true));
    menu_over_the_row(&mut editor);
    act(&mut editor, MergeAction::Start(MergeKind::Panorama));
    start(&mut editor);
    let record = follow(&mut editor);
    assert_eq!(record["status"], "ready", "{record}");
    settle(&mut editor);
    let leader = active(&editor);
    let RowItem::Photo { asset_id } = leader.item.clone() else {
        panic!("the result: {leader:?}");
    };
    let folder_id = leader.folder_id.clone().expect("the result's folder");

    // Its catalog folder viewed, then `D`: the strip read, the result opened.
    let _ = editor.update(Message::Select(SelectMessage::Source(
        ViewSource::CatalogFolder {
            folder_id: folder_id.clone(),
            subfolders: true,
        },
    )));
    evaluate(&mut editor);
    read_rows(&mut editor);
    settle(&mut editor);
    assert_eq!(editor.develop_folder(), Some(&folder_id));
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Develop)));
    let serial = editor.develop.strip_serial;
    let result = folder_strip_now(&editor.owner, &folder_id);
    let _ = editor.update(Message::Develop(DevelopMessage::StripRead {
        serial,
        result,
    }));
    assert!(!editor.select_shown());
    let switch = editor.develop.switch.clone().expect("an open in flight");
    assert_eq!(switch.asset, asset_id);
    let opened = tasks::open_photograph(
        &editor.owner,
        editor.client,
        &asset_id,
        switch.generation,
        &editor.open_generation,
        editor.drawn(),
    );
    assert!(opened.is_ok(), "{:?}", opened.as_ref().err());
    let _ = editor.update(Message::Sync(SyncMessage::ImportRefreshed(
        switch.generation,
        opened.map(Box::new),
    )));
    luxforge_testbase::wait_until("the paged result's frame on screen", || {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        editor.presentation.presented_entry.is_some() && !editor.presentation.queue.is_busy()
    });
    let state = editor.document.state.as_ref().expect("the result open");
    assert_eq!(state.asset.id, asset_id);
    let (width, height) = (state.asset.width, state.asset.height);
    assert!(
        width > 16_384 || u64::from(width) * u64::from(height) > 128_000_000,
        "a paged result: {width} × {height}"
    );
    let snapshot = editor.snapshot();
    assert_eq!(snapshot["render_error"], Value::Null, "{snapshot}");
    assert!(editor.develop.switch.is_none(), "the open has ended");

    // Select again, as it was: the result's folder in view.
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    assert!(editor.select_shown());
    assert_eq!(editor.status.text, "Showing Select");
    assert_eq!(editor.develop_folder(), Some(&folder_id));
    finish(editor, bench.catalog.clone());
}
