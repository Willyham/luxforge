//! The Select workspace against a real owner over a seeded catalog and index: the event list and
//! its search, a view evaluated from a source, the rows read for the visible window, a selection by
//! click and arrow keys reflected from the session, a view made stale by another client's
//! `pick.set` evaluated again without a reload, picking with `P` and Pick all, library undo and
//! redo of this desktop's own changes and not an agent's, and the sources panel's volumes, folders
//! and counts. Each owner call runs exactly as its task would, through the `*_now` functions the
//! tasks run, and its answer is handed back as the task's message; each library gesture's request
//! is compared with what an independent JSON client writes, and its effect read back through that
//! client.
use crate::{
    Config,
    app::{
        Boot, Editor,
        message::{
            Message,
            long_work::LongWorkMessage,
            select::{SelectMessage, Step},
            sync::SyncMessage,
        },
        select::{
            counts_now, disks_now, evaluate_now, events_now, facets_now, folders_now, label_now,
            refresh_now, rows_now, session_now,
        },
        tasks::{call, owner_calls},
    },
    state::select::{Block, Count, Dot, InfoModel, ReadSource, Shown, SourcePress},
};
use iced::Size;
use luxforge_core::{
    ClientId, OwnerHandle, SourceTag,
    catalog_types::{
        CameraBody, CaptureTime, Dimensions, EventList, ExifOrientation, Exposure, FileRecord,
        FileSignature, GeoPosition, HeaderMetadata, HeaderState, IndexRoot, IndexedFolder,
        RootKind, RowItem, ViewSource, Volume, VolumeId,
    },
    seed::{CatalogSeeder, IndexSeeder},
};
use luxforge_ui::{GridPress, PressModifiers};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const CATALOG_ID: &str = "catalog-select-owner-test";
const PICTURES: &str = "/Volumes/SSD/Pictures";
const LAKE: &str = "/Volumes/SSD/Pictures/2026-09-12 Lake";

fn ssd() -> VolumeId {
    VolumeId::parse("volume-ssd-00000000").unwrap()
}

pub(super) fn z8() -> CameraBody {
    CameraBody {
        make: "NIKON CORPORATION".into(),
        model: "NIKON Z 8".into(),
        serial: Some("3001".into()),
    }
}

fn q3() -> CameraBody {
    CameraBody {
        make: "LEICA CAMERA AG".into(),
        model: "LEICA Q3".into(),
        serial: None,
    }
}

/// One file of the lake folder, taken at `when` (`YYYY:MM:DD HH:MM:SS[.mmm]`, +02:00) by `camera`
/// at 1/`speed` s and f/8.
pub(super) fn file(
    name: &str,
    when: &str,
    camera: CameraBody,
    speed: f32,
    bias: f32,
) -> FileRecord {
    let (datetime, subsec) = match when.split_once('.') {
        Some((datetime, subsec)) => (datetime, Some(subsec)),
        None => (when, None),
    };
    let header = HeaderMetadata {
        capture: CaptureTime::from_exif(datetime, subsec, Some("+02:00")),
        position: GeoPosition::new(47.66, 9.175, None),
        camera: Some(camera),
        lens: Some("NIKKOR Z 24-120mm f/4 S".into()),
        exposure: Exposure {
            time_s: Some(1.0 / speed),
            f_number: Some(8.0),
            iso: Some(100),
            bias_ev: Some(bias),
            ..Exposure::default()
        },
        dimensions: Some(Dimensions {
            width: 6000,
            height: 4000,
        }),
        orientation: ExifOrientation::new(1),
        thumbnail: None,
    };
    FileRecord {
        path: Path::new(LAKE).join(name),
        folder: LAKE.into(),
        name: name.into(),
        volume_id: ssd(),
        signature: FileSignature {
            len: 1000 + name.len() as u64,
            modified_ns: 7,
            identity: None,
        },
        kind: SourceTag::Raw,
        header: HeaderState::Ok(Box::new(header)),
        last_seen_ms: 1,
        born_ns: None,
    }
}

/// A catalog with one indexed folder of twelve files over two days: on the first, a Nikon burst of
/// three, two Nikon singles and a Leica bracket of three exposures from its metadata; on the
/// second, four Nikon singles.
fn seeded() -> PathBuf {
    seeded_with(&[
        file("DSC_0001.NEF", "2026:09:12 09:00:00.000", z8(), 250.0, 0.0),
        file("DSC_0002.NEF", "2026:09:12 09:00:00.300", z8(), 250.0, 0.0),
        file("DSC_0003.NEF", "2026:09:12 09:00:00.600", z8(), 250.0, 0.0),
        file("DSC_0004.NEF", "2026:09:12 09:30:00", z8(), 250.0, 0.0),
        file("DSC_0005.NEF", "2026:09:12 10:15:00", z8(), 250.0, 0.0),
        file("L1000001.DNG", "2026:09:12 11:00:00.000", q3(), 500.0, -1.0),
        file("L1000002.DNG", "2026:09:12 11:00:00.800", q3(), 250.0, 0.0),
        file("L1000003.DNG", "2026:09:12 11:00:01.600", q3(), 125.0, 1.0),
        file("DSC_0006.NEF", "2026:09:13 08:00:00", z8(), 250.0, 0.0),
        file("DSC_0007.NEF", "2026:09:13 09:00:00", z8(), 250.0, 0.0),
        file("DSC_0008.NEF", "2026:09:13 10:00:00", z8(), 250.0, 0.0),
        file("DSC_0009.NEF", "2026:09:13 11:00:00", z8(), 250.0, 0.0),
    ])
}

/// A catalog with one indexed folder of `files`, each of the lake folder ([`file`]).
pub(super) fn seeded_with(files: &[FileRecord]) -> PathBuf {
    let dir = luxforge_testbase::paths::temp_dir("select-owner");
    let catalog = dir.join("catalog.sqlite");
    let mut seeder = CatalogSeeder::create(&catalog, CATALOG_ID).unwrap();
    seeder
        .volumes(&[Volume {
            id: ssd(),
            mount_point: "/Volumes/SSD".into(),
            label: "SSD".into(),
            removable: false,
            platform_id: None,
            last_seen_ms: 1,
        }])
        .unwrap();
    seeder
        .indexed_folders(&[IndexedFolder {
            path: PICTURES.into(),
            volume_id: ssd(),
            added_ms: 1,
            actor: "desktop".into(),
        }])
        .unwrap();
    seeder.finish().unwrap();
    let mut index = IndexSeeder::create(&catalog, CATALOG_ID).unwrap();
    index.files(files).unwrap();
    index
        .roots(&[IndexRoot {
            path: PICTURES.into(),
            kind: RootKind::Indexed,
            volume_id: ssd(),
            listed_ms: Some(1),
            file_count: Some(u32::try_from(files.len()).unwrap()),
            offline: false,
        }])
        .unwrap();
    index.finish().unwrap();
    catalog
}

/// The editor over the seeded catalog, with Select shown and what showing it reads — the events,
/// the cards and volumes, and the catalog's counts — answered by the owner.
pub(super) fn selecting() -> (Editor, PathBuf) {
    selecting_over(seeded())
}

/// The editor over `catalog`, as [`selecting`] opens it. Its owner reads no mount table: a card
/// the host mounts meanwhile, such as the core's disk-image tests attach with a `DCIM` folder of
/// two photographs, would otherwise be listed as the catalog opens, and its event listed beside
/// the seeded one (`OwnerHandle::read_no_host_mounts`).
pub(super) fn selecting_over(catalog: PathBuf) -> (Editor, PathBuf) {
    OwnerHandle::read_no_host_mounts(&catalog);
    opened(catalog)
}

/// The editor over the seeded catalog, as [`selecting`] opens it but reading the host's mount
/// table: for a test of what the host has mounted, which is checked against what it read.
fn selecting_on_the_host() -> (Editor, PathBuf) {
    opened(seeded())
}

/// The editor over `catalog` with Select shown, what showing it reads answered.
fn opened(catalog: PathBuf) -> (Editor, PathBuf) {
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
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
    answer_reads(&mut editor);
    (editor, catalog)
}

/// Answer the events, the cards and volumes and the catalog's counts the editor has asked for, as
/// their tasks would.
pub(super) fn answer_reads(editor: &mut Editor) {
    if editor.select.events.in_flight() {
        let events = events_now(&editor.owner, editor.client, &editor.select.state.search);
        let _ = editor.update(Message::Select(SelectMessage::Events(events)));
    }
    if editor.select.disks.in_flight() {
        let disks = disks_now(&editor.owner, editor.client);
        let _ = editor.update(Message::Select(SelectMessage::Disks(disks)));
    }
    if editor.select.counts.in_flight() {
        let counts = counts_now(&editor.owner, editor.client);
        let _ = editor.update(Message::Select(SelectMessage::Counted(counts)));
    }
    // The catalog's folders and collections, read with the counts.
    if editor.select.catalog.lists.in_flight() {
        let lists = crate::app::select_catalog::lists_now(&editor.owner, editor.client);
        let _ = editor.update(Message::Select(SelectMessage::Catalog(
            crate::app::message::select_catalog::CatalogMessage::Lists(lists),
        )));
    }
}

pub(super) fn finish(mut editor: Editor, catalog: PathBuf) {
    editor.owner.stop();
    editor.owner_join.take().unwrap().join().unwrap();
    drop(editor);
    let _ = std::fs::remove_dir_all(catalog.parent().unwrap());
}

/// Run the evaluation the editor has in flight, as its tasks would, and hand the answers back.
pub(super) fn evaluate(editor: &mut Editor) {
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
}

/// Read every block of rows the editor asks for, as its tasks would.
pub(super) fn read_rows(editor: &mut Editor) {
    while let Some(request) = editor.select.state.rows.in_flight() {
        let rows = rows_now(&editor.owner, editor.client, &request);
        let _ = editor.update(Message::Select(SelectMessage::Rows {
            revision: request.revision,
            from: request.from,
            result: rows,
        }));
    }
}

/// The event list and its search answer with the owner's own events; a source row viewed lays the
/// grid out from the owner's group layout; the visible window's rows are read; a click and arrow
/// keys select through the session; and another client's pick makes the view stale, which the
/// wake evaluates again without a reload.
#[test]
fn select_views_selects_and_follows_another_clients_pick_on_a_real_owner() {
    let (mut editor, catalog) = selecting();
    let list: &EventList = editor
        .select
        .state
        .events
        .as_ref()
        .expect("the events were read");
    assert_eq!(list.events.len(), 1, "{list:?}");
    assert_eq!(list.events[0].count, 12);
    // The search finds the event by its camera and by its date, and nothing by another place.
    for (query, found) in [("Z 8", 1), ("2026-09-12", 1), ("Zermatt", 0)] {
        let answer = events_now(&editor.owner, editor.client, query).unwrap();
        assert_eq!(answer.events.len(), found, "{query}");
    }
    // The sources panel lists it under September 2026; pressing it views it.
    let sources = &editor.workspace.select.sources;
    assert_eq!(sources.months[0].label, "September 2026");
    let row = &sources.months[0].rows[0];
    assert_eq!(row.secondary.as_deref(), Some("12\u{2013}13 Sep"));
    let Some(SourcePress::View(source)) = row.press.clone() else {
        panic!("an event row views its event");
    };
    assert!(matches!(source, ViewSource::Event { .. }));
    let _ = editor.update(Message::Select(SelectMessage::Source(source.clone())));
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        1000.0, 700.0,
    ))));
    evaluate(&mut editor);
    let summary = editor.select.state.summary.clone().expect("the view");
    assert_eq!(summary.count, 12);
    assert_eq!(summary.query.source, source);
    assert_eq!(editor.select.layout.item_count(), 12);
    let blocks = &editor.select.state.content.blocks;
    let count = |kind: fn(&Block) -> bool| blocks.iter().filter(|block| kind(block)).count();
    assert_eq!(count(|block| matches!(block, Block::Day { .. })), 2);
    assert_eq!(
        count(|block| matches!(block, Block::Camera { .. })),
        summary.groups.cameras.len()
    );
    assert!(
        blocks.iter().any(|block| matches!(
            block,
            Block::Moment {
                bracket: false,
                frames: 3,
                ..
            }
        )),
        "the Nikon burst: {blocks:?}"
    );
    assert!(
        blocks.iter().any(|block| matches!(
            block,
            Block::Moment { bracket: true, frames: 3, evidence: Some(evidence), .. }
                if evidence == "from metadata"
        )),
        "the Leica bracket: {blocks:?}"
    );
    assert_eq!(editor.select.state.content.labels.len(), 3);
    assert!(editor.select.state.facets.is_some());
    assert_eq!(
        editor.workspace.select.status.line,
        "Camera previews \u{b7} auto-organized \u{b7} 12 in view \u{b7} 0 picked"
    );
    // The rows for the visible window, which here is the whole view.
    read_rows(&mut editor);
    assert_eq!(editor.select.state.rows.len(), 12);
    assert!(editor.select_reads_quiet());

    // A click selects through the session; the arrows move the active item and Shift extends.
    let press = |item: u32| {
        let layout = &editor.select.layout;
        let cell = layout.cell(layout.cell_of_item(item).unwrap());
        GridPress {
            cell: cell.cell,
            item: cell.item,
            span: cell.span,
            modifiers: PressModifiers::default(),
            double: false,
        }
    };
    let click = press(4);
    let _ = editor.update(Message::Select(SelectMessage::Press(click)));
    assert_eq!(editor.session.browse.selection.active, Some(4));
    let InfoModel::One(item) = &editor.workspace.select.info else {
        panic!("one item: {:?}", editor.workspace.select.info);
    };
    assert_eq!(item.name, "DSC_0005.NEF");
    let _ = editor.update(Message::Select(SelectMessage::Move {
        step: Step::Right,
        extend: false,
    }));
    let _ = editor.update(Message::Select(SelectMessage::Move {
        step: Step::Right,
        extend: true,
    }));
    let selection = editor.session.browse.selection.clone();
    assert_eq!(selection.active, Some(6));
    assert_eq!(selection.count, 2);
    assert_eq!(
        session_now(&editor.owner, editor.client)
            .unwrap()
            .browse
            .selection,
        selection,
        "the grid draws the owner's own selection"
    );
    assert!(editor.workspace.select.selection.selected(5, 1));
    assert!(editor.workspace.select.selection.active_in(6, 1));

    // Another client picks the first frame: the view goes stale, and the wake evaluates it again.
    let agent = editor.owner.register();
    let Some(RowItem::File { file_id }) =
        editor.select.state.rows.row(0).map(|row| row.item.clone())
    else {
        panic!("a file");
    };
    call(
        &editor.owner,
        agent,
        "pick.set",
        json!({
            "targets": {"kind": "files", "file_ids": [file_id]},
            "picked": true,
            "mutation": {"request_id": "agent-pick-1", "actor": "agent"},
        }),
    )
    .unwrap();
    let revision = summary.revision;
    // What the owner's wake posts.
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    assert!(editor.select.check.in_flight());
    let checked = session_now(&editor.owner, editor.client).map(Box::new);
    assert!(checked.as_ref().unwrap().browse.stale);
    let _ = editor.update(Message::Select(SelectMessage::Checked(checked)));
    assert!(editor.select.state.loading, "evaluated again");
    evaluate(&mut editor);
    read_rows(&mut editor);
    let again = editor.select.state.summary.as_ref().unwrap();
    assert!(again.revision > revision);
    assert_eq!(again.picked, 1);
    assert!(!editor.session.browse.stale);
    assert_eq!(
        editor.session.browse.selection, selection,
        "the selection follows its items"
    );
    assert!(editor.select.state.rows.row(0).unwrap().picked);
    assert!(
        editor.status.text.contains("changed elsewhere"),
        "{}",
        editor.status.text
    );
    assert!(editor.document.state.is_none(), "nothing reloaded Develop");
    finish(editor, catalog);
}

/// Browse a folder…: the index lane reads the folder (`index.refresh`, a job the desktop reads until
/// it ends, the status bar saying so meanwhile), and then the folder is viewed with its
/// subfolders.
#[test]
fn broad_folder_navigation_never_starts_a_scan_and_the_api_refuses_one() {
    let (mut editor, catalog) = selecting();
    let root = if cfg!(windows) {
        std::path::PathBuf::from("C:/")
    } else {
        std::path::PathBuf::from("/")
    };
    drop(
        editor.update(Message::Select(SelectMessage::FolderPicked(Some(
            root.clone(),
        )))),
    );
    assert!(editor.select.reading.is_none());
    assert!(editor.status.text.contains("specific subfolder"));
    for (method, params) in [
        (
            "index.refresh",
            json!({"source":{"kind":"folder","path":root}}),
        ),
        (
            "index.add-folder",
            json!({"path":root,"mutation":{"request_id":"broad-folder-refusal","actor":"test"}}),
        ),
    ] {
        let error =
            crate::app::tasks::call(&editor.owner, editor.client, method, params).unwrap_err();
        assert!(error.contains("too broad"), "{method}: {error}");
    }
    finish(editor, catalog);
}

#[test]
fn a_select_folder_browsed_on_disk_is_read_then_viewed_on_a_real_owner() {
    let (mut editor, catalog) = selecting();
    let folder = catalog.parent().unwrap().join("Card dump");
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0");
    for (name, into) in [
        ("orientation-1.jpg", "a.jpg"),
        ("orientation-6.jpg", "b.jpg"),
        ("greyscale.jpg", "sub/c.jpg"),
    ] {
        std::fs::copy(fixtures.join(name), folder.join(into)).unwrap();
    }
    let _ = editor.update(Message::Select(SelectMessage::FolderPicked(Some(
        folder.clone(),
    ))));
    assert!(editor.select.reading.is_some());
    assert!(
        !editor.select_reads_quiet(),
        "a folder being read is in flight"
    );
    assert!(
        editor.status.text.starts_with("Reading "),
        "{}",
        editor.status.text
    );
    let on_disk = &editor.workspace.select.sources.on_disk;
    assert!(
        on_disk.iter().any(|row| row.name == "Card dump"),
        "the folder being read is listed On disk: {on_disk:?}"
    );
    let job = refresh_now(
        &editor.owner,
        editor.client,
        &ReadSource::Folder(folder.clone()),
    );
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
                    super::job_reads::wait(&editor.owner, editor.client, &job, after)
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
    assert!(editor.select.reading.is_none());
    assert!(
        editor.select.state.loading,
        "the folder is viewed once it is read"
    );
    evaluate(&mut editor);
    let summary = editor.select.state.summary.as_ref().unwrap();
    assert_eq!(summary.count, 3, "the folder with its subfolder");
    let listed = folder.canonicalize().unwrap();
    assert!(
        matches!(
            &summary.query.source,
            ViewSource::Folder { path, subfolders: true } if path == &listed
        ),
        "the folder as the index listed it: {:?}",
        summary.query.source
    );
    assert!(
        editor
            .workspace
            .select
            .sources
            .on_disk
            .iter()
            .any(|row| row.name == "Card dump" && row.selected)
    );
    assert_eq!(editor.workspace.select.title.name, "Card dump");
    // A failed read says why and views nothing.
    let _ = editor.update(Message::Select(SelectMessage::FolderPicked(Some(
        folder.join("missing"),
    ))));
    let refused = refresh_now(
        &editor.owner,
        editor.client,
        &ReadSource::Folder(folder.join("missing")),
    );
    let failed = refused.is_err();
    let _ = editor.update(Message::Select(SelectMessage::Reading(refused)));
    if failed {
        assert!(editor.select.reading.is_none());
        assert!(
            editor.status.text.starts_with("Could not read"),
            "{}",
            editor.status.text
        );
    }
    finish(editor, catalog);
}

/// A view that goes stale while a folder is read for this desktop — the index lane's own batches
/// make it so — is never announced as changed elsewhere. While the reading waits to replace it,
/// the title names the folder and the view is not read again; sent to the background, the view is
/// back in the title and read again quietly; a reading that ends without replacing it reads a view
/// that went stale meanwhile once, quietly; and choosing another view leaves the reading to the
/// status bar.
#[test]
fn a_select_view_gone_stale_while_a_folder_is_read_is_read_again_quietly() {
    let (mut editor, catalog) = selecting();
    let Some(SourcePress::View(source)) = editor.workspace.select.sources.months[0].rows[0]
        .press
        .clone()
    else {
        panic!("an event row views its event");
    };
    let _ = editor.update(Message::Select(SelectMessage::Source(source.clone())));
    evaluate(&mut editor);
    let viewed = editor.workspace.select.title.name.clone();
    let stale = |editor: &Editor| {
        let mut session = session_now(&editor.owner, editor.client).unwrap();
        session.browse.stale = true;
        Ok(Box::new(session))
    };
    // Stale with no library change since — the index alone moved — it is read again without a word.
    let _ = editor.update(Message::Select(SelectMessage::Checked(stale(&editor))));
    assert!(editor.select.state.loading, "read again");
    evaluate(&mut editor);
    assert!(
        !editor.status.text.contains("changed elsewhere"),
        "{}",
        editor.status.text
    );

    let folder = catalog.parent().unwrap().join("Trip");
    let read = |editor: &mut Editor, job: &str| {
        let _ = editor.update(Message::Select(SelectMessage::FolderPicked(Some(
            folder.clone(),
        ))));
        let _ = editor.update(Message::Select(SelectMessage::Reading(Ok(job.into()))));
        editor.status.text.clone()
    };

    // Waiting: the title is the folder's, and the stale view waits with it.
    let reading = read(&mut editor, "job-look");
    assert!(reading.starts_with("Reading "), "{reading}");
    let title = &editor.workspace.select.title;
    assert_eq!(title.name, "Trip");
    assert!(
        title.summary.starts_with("Reading\u{2026}"),
        "{}",
        title.summary
    );
    let serial = editor.select.serial;
    let _ = editor.update(Message::Select(SelectMessage::Checked(stale(&editor))));
    assert_eq!(
        editor.select.serial, serial,
        "not read again while it waits"
    );
    assert!(editor.select.stale_while_reading);
    assert_eq!(editor.status.text, reading);

    // In the background: the view is back in the title, and read again without a word.
    let _ = editor.update(Message::LongWork(LongWorkMessage::ContinueInBackground));
    assert_eq!(editor.workspace.select.title.name, viewed);
    let _ = editor.update(Message::Select(SelectMessage::Checked(stale(&editor))));
    assert!(editor.select.state.loading, "read again");
    assert!(!editor.select.stale_while_reading);
    evaluate(&mut editor);
    assert_eq!(
        editor.status.text, reading,
        "no change elsewhere is announced"
    );

    // Cancelled while waiting: the view that went stale is read again once, quietly.
    let _ = editor.update(Message::Select(SelectMessage::Source(source.clone())));
    evaluate(&mut editor);
    read(&mut editor, "job-cancelled");
    let _ = editor.update(Message::Select(SelectMessage::Checked(stale(&editor))));
    assert!(!editor.select.state.loading);
    let _ = editor.update(Message::Select(SelectMessage::ReadWatched {
        job: "an-earlier-reading".into(),
        result: Ok(json!({"status": "ready"})),
    }));
    assert_eq!(
        editor
            .select
            .reading
            .as_ref()
            .and_then(|reading| reading.job.as_deref()),
        Some("job-cancelled"),
        "an old completion cannot replace the current reading"
    );
    assert!(!editor.select.state.loading);
    let _ = editor.update(Message::Select(SelectMessage::ReadWatched {
        job: "job-cancelled".into(),
        result: Ok(json!({"status": "cancelled"})),
    }));
    assert!(editor.select.reading.is_none());
    assert!(editor.select.state.loading, "the stale view is read again");
    evaluate(&mut editor);
    assert!(
        editor.status.text.starts_with("Cancelled reading "),
        "{}",
        editor.status.text
    );
    assert_eq!(editor.workspace.select.title.name, viewed);

    // Another view chosen while it waits: the reading is the status bar's job from then on.
    read(&mut editor, "job-left");
    let _ = editor.update(Message::Select(SelectMessage::Source(source)));
    assert!(editor.select.reading.is_none());
    evaluate(&mut editor);
    assert_eq!(editor.workspace.select.title.name, viewed);
    finish(editor, catalog);
}

/// A view evaluated again while the loupe shows a frame keeps that frame's row, and its moment's,
/// until the new revision's rows arrive: the loupe never loses its frame for an update, and the
/// frame it keeps is the item the owner carried the active item over as.
#[test]
fn a_select_view_read_again_keeps_the_loupes_frame_until_its_rows_arrive() {
    let (mut editor, catalog) = selecting();
    let Some(SourcePress::View(source)) = editor.workspace.select.sources.months[0].rows[0]
        .press
        .clone()
    else {
        panic!("an event row views its event");
    };
    let _ = editor.update(Message::Select(SelectMessage::Source(source)));
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        1000.0, 700.0,
    ))));
    evaluate(&mut editor);
    read_rows(&mut editor);
    let _ = editor.update(Message::Select(SelectMessage::Move {
        step: Step::Right,
        extend: false,
    }));
    let _ = editor.update(Message::Select(SelectMessage::Loupe(
        crate::app::message::loupe::LoupeMessage::Open,
    )));
    let active = editor
        .session
        .browse
        .selection
        .active
        .expect("an active frame");
    let item = editor.select.state.rows.row(active).unwrap().item.clone();
    let frame = editor
        .workspace
        .select
        .loupe
        .subject
        .expect("the loupe's frame");
    let revision = editor.select.state.summary.as_ref().unwrap().revision;

    // The view goes stale and is read again: until its rows are read, the frame keeps its row.
    let mut session = session_now(&editor.owner, editor.client).unwrap();
    session.browse.stale = true;
    let _ = editor.update(Message::Select(SelectMessage::Checked(Ok(Box::new(
        session,
    )))));
    evaluate(&mut editor);
    assert!(editor.select.state.summary.as_ref().unwrap().revision > revision);
    assert!(
        editor.select.state.rows.len() == 0,
        "no row of the new revision read yet"
    );
    let now = editor
        .session
        .browse
        .selection
        .active
        .expect("carried over");
    assert_eq!(
        editor.select.state.rows.row(now).map(|row| &row.item),
        Some(&item)
    );
    assert!(editor.select.state.rows.read(now).is_none());
    let kept = editor
        .workspace
        .select
        .loupe
        .subject
        .expect("the loupe keeps its frame");
    assert_eq!(kept.position, frame.position);
    assert!(
        editor.select.state.rows.row(now + 1).is_some(),
        "its neighbours too"
    );
    // The new revision's rows replace it.
    read_rows(&mut editor);
    assert_eq!(
        editor.select.state.rows.read(now).map(|row| &row.item),
        Some(&item)
    );
    finish(editor, catalog);
}

/// Run everything the editor has asked the owner for — the reads showing Select or a change makes,
/// a staleness check, an evaluation, a change's label and the rows near the screen — as their
/// tasks would, until nothing is in flight.
fn settle(editor: &mut Editor) {
    for _ in 0..8 {
        answer_reads(editor);
        if editor.select.check.in_flight() {
            let checked = session_now(&editor.owner, editor.client).map(Box::new);
            let _ = editor.update(Message::Select(SelectMessage::Checked(checked)));
        }
        if editor.select.state.loading {
            evaluate(editor);
        }
        if let Some(sequence) = editor.select.label {
            let result = label_now(&editor.owner, editor.client, sequence);
            let _ = editor.update(Message::Select(SelectMessage::Labelled {
                sequence,
                result,
            }));
        }
        read_rows(editor);
        if editor.select_reads_quiet() {
            return;
        }
    }
    panic!("Select did not settle");
}

/// Wait until the owner runs no background work — the preview lane's grid job for a view writes
/// fingerprints that move the index's revision and leave the view stale — and the view is current,
/// read again through the wake as the desktop reads it, so the next gesture names the view the
/// owner holds whatever the host's load.
fn steady(editor: &mut Editor) {
    luxforge_testbase::wait_for("the owner's work to end and the view to be current", || {
        let (board, _) = call(&editor.owner, editor.client, "activity.list", json!({})).unwrap();
        if !board["active"].as_array().is_some_and(Vec::is_empty) {
            return None;
        }
        let _ = editor.update(Message::Sync(SyncMessage::Changed));
        settle(editor);
        let current = !session_now(&editor.owner, editor.client)
            .unwrap()
            .browse
            .stale;
        current.then_some(())
    });
}

/// A key pressed with no text field focused, through the editor's own key table.
fn key(editor: &mut Editor, letter: &str, modifiers: iced::keyboard::Modifiers) {
    use iced::keyboard::{
        Event as KeyEvent, Key, Location,
        key::{NativeCode, Physical},
    };
    let pressed = Key::Character(letter.into());
    let event = iced::Event::Keyboard(KeyEvent::KeyPressed {
        key: pressed.clone(),
        modified_key: pressed,
        physical_key: Physical::Unidentified(NativeCode::Unidentified),
        location: Location::Standard,
        modifiers,
        text: None,
        repeat: false,
    });
    let _ = editor.update(Message::Key(event, iced::event::Status::Ignored));
}

/// Click the cell showing view position `item`.
fn click(editor: &mut Editor, item: u32) {
    let layout = &editor.select.layout;
    let cell = layout.cell(layout.cell_of_item(item).unwrap());
    let press = GridPress {
        cell: cell.cell,
        item: cell.item,
        span: cell.span,
        modifiers: PressModifiers::default(),
        double: false,
    };
    let _ = editor.update(Message::Select(SelectMessage::Press(press)));
}

/// The request the desktop last sent for a library gesture, with its request identity: what an
/// independent JSON client writes for the same gesture, given that identity.
fn sent(editor: &Editor, method: &str) -> (Value, Value) {
    let library = editor.select.library.clone().expect("a library request");
    assert_eq!(library["method"], method, "{library}");
    let request_id = library["params"]["mutation"]["request_id"].clone();
    assert!(request_id.as_str().is_some_and(|id| !id.is_empty()));
    (library["params"].clone(), request_id)
}

/// The journal's newest change, read through another client.
fn newest_change(owner: &OwnerHandle, client: ClientId) -> Value {
    let (page, _) = call(owner, client, "library.journal", json!({"limit": 500})).unwrap();
    page["changes"]
        .as_array()
        .and_then(|changes| changes.last())
        .cloned()
        .expect("a change")
}

/// Whether the file at `path`'s name is picked, read through another client, and by whom.
fn pick_of(owner: &OwnerHandle, client: ClientId, name: &str) -> Option<String> {
    let (page, _) = call(owner, client, "pick.list", json!({})).unwrap();
    page["picks"].as_array().and_then(|picks| {
        picks
            .iter()
            .find(|pick| {
                pick["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(name))
            })
            .map(|pick| pick["actor"].as_str().unwrap_or_default().to_owned())
    })
}

/// `P` picks the selection as `pick.set {targets: {kind: selection}}` with this desktop's actor, a
/// journaled change whose label the status bar says; a bracket's Pick all picks its frames' files;
/// an agent's pick arrives through the wake; `Cmd+Z` undoes this desktop's changes newest first and
/// never the agent's, then has nothing to undo; `Shift+Cmd+Z` redoes; `P` on a picked file clears
/// it; the loupe's pick names the active frame; and an undo whose item an agent changed since is
/// refused, naming it. Each gesture is one synchronous owner call, the same request an independent
/// JSON client writes, and each change is read back through that client.
#[test]
fn select_picks_undoes_and_redoes_through_the_journal_on_a_real_owner() {
    use iced::keyboard::Modifiers;
    let (mut editor, catalog) = selecting();
    let agent = editor.owner.register();
    let source = match editor.workspace.select.sources.months[0].rows[0]
        .press
        .clone()
    {
        Some(SourcePress::View(source)) => source,
        other => panic!("an event row views its event: {other:?}"),
    };
    let _ = editor.update(Message::Select(SelectMessage::Source(source)));
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        1000.0, 700.0,
    ))));
    settle(&mut editor);
    steady(&mut editor);
    let name = |editor: &Editor, item: u32| {
        editor
            .select
            .state
            .rows
            .row(item)
            .unwrap()
            .file_name
            .clone()
    };
    let single = name(&editor, 4);
    assert_eq!(single, "DSC_0005.NEF");

    // `P` on one selected file: one synchronous `pick.set` of the selection.
    click(&mut editor, 4);
    owner_calls::take();
    key(&mut editor, "p", Modifiers::empty());
    assert_eq!(owner_calls::take(), vec!["pick.set".to_owned()]);
    let (params, request_id) = sent(&editor, "pick.set");
    assert_eq!(
        params,
        json!({
            "targets": {"kind": "selection"},
            "picked": true,
            "mutation": {"request_id": request_id, "actor": "desktop"},
        })
    );
    assert!(
        editor.select.state.loading,
        "the view it made stale is read again"
    );
    settle(&mut editor);
    let change = newest_change(&editor.owner, agent);
    assert_eq!(
        (
            &change["actor"],
            &change["method"],
            &change["label"],
            &change["request_id"]
        ),
        (
            &json!("desktop"),
            &json!("pick.set"),
            &json!("Picked DSC_0005.NEF"),
            &request_id
        )
    );
    assert_eq!(
        pick_of(&editor.owner, agent, &single).as_deref(),
        Some("desktop")
    );
    assert_eq!(
        editor.status.text,
        "Picked DSC_0005.NEF \u{b7} Undo \u{2318}Z"
    );
    let summary = editor.select.state.summary.clone().unwrap();
    assert_eq!(summary.picked, 1);
    assert_eq!(summary.groups.days[0].picked, 1);
    assert!(editor.select.state.rows.row(4).unwrap().picked);
    assert_eq!(
        editor.session.browse.selection.active,
        Some(4),
        "the active item is kept"
    );
    assert!(matches!(
        &editor.select.state.content.blocks[0],
        Block::Day { detail, .. } if detail.ends_with("\u{b7} 1 picked")
    ));
    let InfoModel::One(item) = &editor.workspace.select.info else {
        panic!("one item: {:?}", editor.workspace.select.info);
    };
    assert!(item.pick.as_ref().is_some_and(|band| band.picked));
    assert_eq!(
        editor.workspace.select.title.picks, 1,
        "Develop N counts it"
    );
    // The event's row counts its pick, read again with the events.
    assert_eq!(
        editor.workspace.select.sources.months[0].rows[0].count,
        Count::Picks {
            picked: "1".into(),
            total: "12".into()
        }
    );

    // The Leica bracket's Pick all: one synchronous `pick.set` of its three files.
    let (number, bracket) = editor
        .select
        .state
        .content
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Moment {
                index,
                bracket,
                action,
                ..
            } => Some((*index, *bracket, action.clone())),
            _ => None,
        })
        .enumerate()
        .find(|(_, (_, bracket, _))| *bracket)
        .map(|(number, (index, _, action))| {
            assert_eq!(action.as_deref(), Some("Pick all 3"));
            (
                number as u32,
                summary.groups.moments[index as usize].clone(),
            )
        })
        .expect("the Leica bracket");
    let files: Vec<Value> = (bracket.start..bracket.start + bracket.len)
        .map(
            |position| match editor.select.state.rows.row(position).unwrap().item {
                RowItem::File { file_id } => json!(file_id),
                RowItem::Photo { .. } => panic!("a file"),
            },
        )
        .collect();
    owner_calls::take();
    let _ = editor.update(Message::Select(SelectMessage::PickAll(number)));
    assert_eq!(owner_calls::take(), vec!["pick.set".to_owned()]);
    let (params, request_id) = sent(&editor, "pick.set");
    assert_eq!(
        params,
        json!({
            "targets": {"kind": "files", "file_ids": files},
            "picked": true,
            "mutation": {"request_id": request_id, "actor": "desktop"},
        })
    );
    settle(&mut editor);
    assert_eq!(editor.status.text, "Picked 3 files \u{b7} Undo \u{2318}Z");
    let picked_bracket = |editor: &Editor| {
        let summary = editor.select.state.summary.as_ref().unwrap();
        summary
            .groups
            .moments
            .iter()
            .find(|moment| moment.start == bracket.start)
            .map(|moment| moment.picked)
    };
    assert_eq!(picked_bracket(&editor), Some(3));
    assert!(editor.select.state.content.blocks.iter().any(|block| matches!(
        block,
        Block::Moment { bracket: true, picked: Some(picked), action: None, .. } if picked == "3 picked"
    )));

    // An agent picks the last file; the wake reads it.
    let last = match editor.select.state.rows.row(11).unwrap().item {
        RowItem::File { file_id } => file_id,
        RowItem::Photo { .. } => panic!("a file"),
    };
    let last_name = name(&editor, 11);
    call(
        &editor.owner,
        agent,
        "pick.set",
        json!({
            "targets": {"kind": "files", "file_ids": [last]},
            "picked": true,
            "mutation": {"request_id": "agent-pick-1", "actor": "agent"},
        }),
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    settle(&mut editor);
    assert_eq!(editor.select.state.summary.as_ref().unwrap().picked, 5);

    // `Cmd+Z`: this desktop's newest change, Pick all, is undone; one synchronous call.
    owner_calls::take();
    key(&mut editor, "z", Modifiers::COMMAND);
    assert_eq!(owner_calls::take(), vec!["library.undo".to_owned()]);
    let (params, request_id) = sent(&editor, "library.undo");
    assert_eq!(
        params,
        json!({"mutation": {"request_id": request_id, "actor": "desktop"}})
    );
    settle(&mut editor);
    assert_eq!(
        editor.status.text,
        "Undid Picked 3 files \u{b7} Redo \u{21e7}\u{2318}Z"
    );
    assert_eq!(picked_bracket(&editor), Some(0));
    // Again: the pick of DSC_0005, never the agent's.
    key(&mut editor, "z", Modifiers::COMMAND);
    settle(&mut editor);
    assert_eq!(pick_of(&editor.owner, agent, &single), None);
    assert_eq!(
        pick_of(&editor.owner, agent, &last_name).as_deref(),
        Some("agent")
    );
    assert_eq!(editor.select.state.summary.as_ref().unwrap().picked, 1);
    let undo = newest_change(&editor.owner, agent);
    assert_eq!(
        (&undo["actor"], &undo["label"]),
        (&json!("desktop"), &json!("Undo Picked DSC_0005.NEF"))
    );
    // And again: nothing of this desktop's is left, so nothing changes and nothing is read.
    let sequence = newest_change(&editor.owner, agent)["sequence"].clone();
    key(&mut editor, "z", Modifiers::COMMAND);
    assert_eq!(editor.status.text, "Nothing to undo");
    assert!(!editor.select.state.loading && editor.select_reads_quiet());
    assert_eq!(newest_change(&editor.owner, agent)["sequence"], sequence);
    assert_eq!(
        pick_of(&editor.owner, agent, &last_name).as_deref(),
        Some("agent")
    );

    // `Shift+Cmd+Z` redoes the pick of DSC_0005.
    owner_calls::take();
    key(&mut editor, "z", Modifiers::COMMAND | Modifiers::SHIFT);
    assert_eq!(owner_calls::take(), vec!["library.redo".to_owned()]);
    let (params, request_id) = sent(&editor, "library.redo");
    assert_eq!(
        params,
        json!({"mutation": {"request_id": request_id, "actor": "desktop"}})
    );
    settle(&mut editor);
    assert_eq!(
        editor.status.text,
        "Redid Picked DSC_0005.NEF \u{b7} Undo \u{2318}Z"
    );
    assert!(editor.select.state.rows.row(4).unwrap().picked);
    assert_eq!(
        pick_of(&editor.owner, agent, &single).as_deref(),
        Some("desktop")
    );

    // `P` on the picked file clears it.
    click(&mut editor, 4);
    key(&mut editor, "p", Modifiers::empty());
    let (params, _) = sent(&editor, "pick.set");
    assert_eq!(params["picked"], false);
    settle(&mut editor);
    assert_eq!(
        editor.status.text,
        "Cleared the pick of DSC_0005.NEF \u{b7} Undo \u{2318}Z"
    );
    assert_eq!(pick_of(&editor.owner, agent, &single), None);

    // The loupe's pick names the active frame alone; it moves on to the next moment itself.
    click(&mut editor, 1);
    let frame = match editor.select.state.rows.row(1).unwrap().item {
        RowItem::File { file_id } => file_id,
        RowItem::Photo { .. } => panic!("a file"),
    };
    let _ = editor.pick_active();
    let (params, request_id) = sent(&editor, "pick.set");
    assert_eq!(
        params,
        json!({
            "targets": {"kind": "files", "file_ids": [frame]},
            "picked": true,
            "mutation": {"request_id": request_id, "actor": "desktop"},
        })
    );
    settle(&mut editor);

    // An undo whose item an agent changed since is refused, naming it; nothing changes.
    let first = name(&editor, 1);
    call(
        &editor.owner,
        agent,
        "pick.set",
        json!({
            "targets": {"kind": "files", "file_ids": [frame]},
            "picked": false,
            "mutation": {"request_id": "agent-clear-1", "actor": "agent"},
        }),
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    settle(&mut editor);
    let sequence = newest_change(&editor.owner, agent)["sequence"].clone();
    key(&mut editor, "z", Modifiers::COMMAND);
    assert_eq!(
        editor.status.text,
        format!("Could not undo: {first} changed since")
    );
    let library = editor.select.library.clone().unwrap();
    assert_eq!(library["error"]["code"], "conflict");
    assert_eq!(library["error"]["data"]["count"], 1);
    assert_eq!(newest_change(&editor.owner, agent)["sequence"], sequence);
    finish(editor, catalog);
}

/// Showing Select reads the cards and volumes and the catalog's counts: On disk lists the volumes
/// but the cards, the startup disk first and mounted, the seeded volume that is not mounted
/// offline; opening a volume reads its subfolders with `disk.folders`, listed under it, each read
/// before it is viewed; and the Catalog rows carry `catalog.info`'s counts.
#[test]
fn select_sources_list_volumes_folders_and_counts_on_a_real_owner() {
    let (mut editor, catalog) = selecting_on_the_host();
    // The rows are checked against the one `card.list` and `volume.list` the desktop read: other
    // tests attach and detach disk images, so the host's mounts may differ at a second read.
    let cards = serde_json::to_value(editor.select.state.cards.as_ref().unwrap()).unwrap();
    let volumes = serde_json::to_value(editor.select.state.volumes.as_ref().unwrap()).unwrap();
    let sources = editor.workspace.select.sources.clone();
    assert_eq!(
        sources.cards.len(),
        cards["cards"].as_array().map_or(0, Vec::len)
    );
    let listed: Vec<&Value> = volumes["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|volume| volume["card"] == false)
        .collect();
    let volume_rows: Vec<_> = sources
        .on_disk
        .iter()
        .filter(|row| row.indent == 0 && row.dot.is_some())
        .collect();
    assert_eq!(volume_rows.len(), listed.len());
    assert_eq!(
        volume_rows[0].dot,
        Some(Dot::Mounted),
        "the startup disk first"
    );
    assert!(
        volume_rows
            .iter()
            .any(|row| row.name == "SSD" && row.dot == Some(Dot::Offline) && row.press.is_none()),
        "the seeded volume, not mounted: {volume_rows:?}"
    );
    assert_eq!(
        sources.on_disk.last().map(|row| row.name.as_str()),
        Some("Browse a folder\u{2026}")
    );
    assert_eq!(sources.catalog[0].count, Count::Total("0".into()));
    assert_eq!(sources.catalog[2].count, Count::None, "no missing original");

    // Opening the startup disk reads its subfolders.
    let Some(SourcePress::Toggle(mount)) = volume_rows[0].press.clone() else {
        panic!("a mounted volume opens");
    };
    let _ = editor.update(Message::Select(SelectMessage::Toggle(mount.clone())));
    assert!(editor.select.listing.contains(&mount));
    assert!(!editor.select_reads_quiet());
    let folders = folders_now(&editor.owner, editor.client, &mount);
    let _ = editor.update(Message::Select(SelectMessage::Listed {
        path: mount.clone(),
        result: folders.clone(),
    }));
    let folders = folders.unwrap();
    let under: Vec<_> = editor
        .workspace
        .select
        .sources
        .on_disk
        .iter()
        .filter(|row| row.indent == 1)
        .map(|row| (row.name.clone(), row.press.clone()))
        .collect();
    assert_eq!(
        under,
        folders
            .folders
            .iter()
            .map(|folder| (
                folder.name.clone(),
                Some(
                    if luxforge_core::catalog_types::disk::broad_folder(
                        &folder.path,
                        editor.select.state.home.as_deref()
                    ) {
                        SourcePress::Toggle(folder.path.clone())
                    } else {
                        SourcePress::Read(ReadSource::Folder(folder.path.clone()))
                    }
                )
            ))
            .collect::<Vec<_>>()
    );
    // Closing it hides them and reads nothing.
    owner_calls::take();
    let _ = editor.update(Message::Select(SelectMessage::Toggle(mount)));
    assert!(owner_calls::take().is_empty());
    assert!(
        editor
            .workspace
            .select
            .sources
            .on_disk
            .iter()
            .all(|row| row.indent == 0)
    );
    finish(editor, catalog);
}

/// A plain click made while the view is being read again waits for it and then selects the
/// clicked cell's item over the view that landed, by its identity, making it active: the owner,
/// already holding the next evaluation, would refuse a selection naming the view on screen, and the
/// click would be lost. Clicked while a view without that item is read, it is dropped, the status
/// bar saying so, and nothing is selected.
#[test]
fn a_click_made_while_the_view_is_read_again_selects_its_item_once_it_lands() {
    let (mut editor, catalog) = selecting();
    let sources: Vec<_> = editor
        .workspace
        .select
        .sources
        .months
        .iter()
        .flat_map(|month| month.rows.iter())
        .filter_map(|row| match row.press.clone() {
            Some(SourcePress::View(source)) => Some(source),
            _ => None,
        })
        .collect();
    let _ = editor.update(Message::Select(SelectMessage::Source(sources[0].clone())));
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        1000.0, 700.0,
    ))));
    settle(&mut editor);
    steady(&mut editor);
    let row = |editor: &Editor, item: u32| editor.select.state.rows.row(item).unwrap().clone();
    let clicked = row(&editor, 4);

    // The same view read again, which the owner evaluates before its answer lands.
    let query = editor.select.state.query.clone().expect("a view");
    let _ = editor.evaluate(query.clone());
    let serial = editor.select.serial;
    let viewed = evaluate_now(&editor.owner, editor.client, &query);
    click(&mut editor, 4);
    assert!(
        editor.select.held_click.is_some(),
        "held while the view is read"
    );
    assert!(
        !editor.status.text.starts_with("Selection failed"),
        "{}",
        editor.status.text
    );
    let _ = editor.update(Message::Select(SelectMessage::Viewed {
        serial,
        result: viewed,
    }));
    let selection = &editor.session.browse.selection;
    assert_eq!(selection.count, 1);
    let active = selection.active.expect("an active item");
    read_rows(&mut editor);
    assert_eq!(
        row(&editor, active).item,
        clicked.item,
        "the item clicked, by identity"
    );

    // Clicked while a view without it is read — the same source narrowed to another file's name —
    // it is dropped, quietly.
    let mut narrowed = editor.select.state.query.clone().expect("a view");
    narrowed.filter.text = Some(row(&editor, 0).file_name);
    let _ = editor.evaluate(narrowed.clone());
    let serial = editor.select.serial;
    let viewed = evaluate_now(&editor.owner, editor.client, &narrowed);
    click(&mut editor, 4);
    assert!(editor.select.held_click.is_some());
    let _ = editor.update(Message::Select(SelectMessage::Viewed {
        serial,
        result: viewed,
    }));
    assert!(editor.select.held_click.is_none());
    assert_eq!(
        editor.status.text,
        "The item clicked is no longer in the view"
    );
    finish(editor, catalog);
}
