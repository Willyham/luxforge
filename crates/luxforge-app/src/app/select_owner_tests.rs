//! The Select workspace against a real owner over a seeded catalog and index: the event list and
//! its search, a view evaluated from a source, the rows read for the visible window, a selection by
//! click and arrow keys reflected from the session, and a view made stale by another client's
//! `pick.set` evaluated again without a reload. Each owner call runs exactly as its task would,
//! through the `*_now` functions the tasks run, and its answer is handed back as the task's message.
use crate::{
    Config,
    app::{
        Boot, Editor,
        message::{
            Message,
            select::{SelectMessage, Step},
            sync::SyncMessage,
        },
        select::{
            evaluate_now, events_now, facets_now, job_now, refresh_now, rows_now, session_now,
        },
        tasks::call,
    },
    state::select::{Block, InfoModel, Shown, SourcePress},
};
use iced::Size;
use luxforge_core::{
    OwnerHandle, SourceTag,
    catalog_types::{
        CameraBody, CaptureTime, Dimensions, EventList, ExifOrientation, Exposure, FileRecord,
        FileSignature, GeoPosition, HeaderMetadata, HeaderState, IndexRoot, IndexedFolder,
        RootKind, RowItem, ViewSource, Volume, VolumeId,
    },
    seed::{CatalogSeeder, IndexSeeder},
};
use luxforge_ui::{GridPress, PressModifiers};
use serde_json::json;
use std::path::{Path, PathBuf};

const CATALOG_ID: &str = "catalog-select-owner-test";
const PICTURES: &str = "/Volumes/SSD/Pictures";
const LAKE: &str = "/Volumes/SSD/Pictures/2026-09-12 Lake";

fn ssd() -> VolumeId {
    VolumeId::parse("volume-ssd-00000000").unwrap()
}

fn z8() -> CameraBody {
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
fn file(name: &str, when: &str, camera: CameraBody, speed: f32, bias: f32) -> FileRecord {
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
    }
}

/// A catalog with one indexed folder of twelve files over two days: on the first, a Nikon burst of
/// three, two Nikon singles and a Leica bracket of three exposures from its metadata; on the
/// second, four Nikon singles.
fn seeded() -> PathBuf {
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
    index
        .files(&[
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
        .unwrap();
    index
        .roots(&[IndexRoot {
            path: PICTURES.into(),
            kind: RootKind::Indexed,
            volume_id: ssd(),
            listed_ms: Some(1),
            file_count: Some(12),
            offline: false,
        }])
        .unwrap();
    index.finish().unwrap();
    catalog
}

/// The editor over the seeded catalog, with Select shown and the events read from the owner.
pub(super) fn selecting() -> (Editor, PathBuf) {
    let catalog = seeded();
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
    let events = events_now(&editor.owner, editor.client, "");
    let _ = editor.update(Message::Select(SelectMessage::Events(events)));
    (editor, catalog)
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
    let SourcePress::View(source) = row.press.clone() else {
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
    assert_eq!(editor.workspace.select.sources.on_disk[1].name, "Card dump");
    let job = refresh_now(&editor.owner, editor.client, &folder);
    let _ = editor.update(Message::Select(SelectMessage::Reading(job.clone())));
    let job = job.unwrap();
    let record = luxforge_testbase::wait_for("the folder's index.refresh to end", || {
        let record = job_now(&editor.owner, editor.client, &job).unwrap();
        (!matches!(record["status"].as_str(), Some("queued" | "running"))).then_some(record)
    });
    assert_eq!(record["status"], "ready", "{record}");
    let _ = editor.update(Message::Select(SelectMessage::ReadAnswered(Ok(record))));
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
    assert!(editor.workspace.select.sources.on_disk[1].selected);
    assert_eq!(editor.workspace.select.title.name, "Card dump");
    // A failed read says why and views nothing.
    let _ = editor.update(Message::Select(SelectMessage::FolderPicked(Some(
        folder.join("missing"),
    ))));
    let refused = refresh_now(&editor.owner, editor.client, &folder.join("missing"));
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
