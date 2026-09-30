//! The catalog in Select against a real owner over a seeded catalog of photographs, folders and
//! collections: the folders and collections read with the counts; the search, a Metadata browser
//! value and the Edited chip each changing the query and nothing else, as an independent JSON
//! client writes it, with the columns' counts the client's own `browse.facets`; the filter bar's
//! "N of M"; Save as smart collection… storing exactly the query shown, and the smart collection
//! viewed; and folder create, rename, nest, merge and delete, a collection's create and rename,
//! the selection moved to a folder and added to a collection, each one library change of this
//! desktop's that `Cmd+Z` undoes. Each owner call runs exactly as its task would, and each library
//! gesture's request is compared with what the independent client writes and its effect read back
//! through that client.
use crate::app::{
    Editor,
    message::{Message, select::SelectMessage, select_catalog::CatalogMessage},
    select_catalog::{lists_now, selection_rows_now, total_now},
    select_owner_tests::{answer_reads, evaluate, finish, read_rows},
    tasks::call,
};
use crate::state::select::Shown;
use crate::state::select_catalog::{CatalogAction, CatalogChange, NamingTarget};
use crate::{Config, app::Boot};
use iced::Size;
use luxforge_core::{
    AssetId, ClientId, OwnerHandle,
    catalog_types::{
        BodyKey, CameraBody, CaptureTime, CatalogFolder, CatalogFolderId, Collection, CollectionId,
        CollectionKind, Dimensions, Exposure, FileAvailability, GeoPosition, HeaderMetadata,
        Volume, VolumeId,
    },
    seed::{CatalogSeeder, SeedAsset, SeedKind},
};
use luxforge_ui::{GridPress, PressModifiers};
use serde_json::{Value, json};
use std::path::PathBuf;

const CATALOG_ID: &str = "catalog-select-catalog-test";

fn ssd() -> VolumeId {
    VolumeId::parse("volume-ssd-00000000").unwrap()
}

fn folder_id(name: &str) -> CatalogFolderId {
    CatalogFolderId::parse(format!("folder-{name}")).unwrap()
}

fn collection_id(name: &str) -> CollectionId {
    CollectionId::parse(format!("collection-{name}")).unwrap()
}

fn folder(id: &str, name: &str, parent: Option<&str>) -> CatalogFolder {
    CatalogFolder {
        id: folder_id(id),
        name: name.into(),
        parent_id: parent.map(folder_id),
        created_ms: 1,
        event: None,
        count: 0,
        year: None,
    }
}

fn leica() -> CameraBody {
    CameraBody {
        make: "LEICA CAMERA AG".into(),
        model: "LEICA Q2".into(),
        serial: None,
    }
}

fn sony() -> CameraBody {
    CameraBody {
        make: "SONY".into(),
        model: "ILCE-6700".into(),
        serial: None,
    }
}

/// One photograph: `n`, in `folder`, taken on `day` (`YYYY:MM:DD`) at `place` with `camera`.
fn photo(n: u32, folder: &str, day: &str, place: &str, camera: CameraBody, raw: bool) -> SeedAsset {
    let name = format!("L100{n:04}.{}", if raw { "DNG" } else { "JPG" });
    SeedAsset {
        id: AssetId::parse(format!("asset-{n:012}")).unwrap(),
        kind: if raw { SeedKind::Raw } else { SeedKind::Jpeg },
        locator: PathBuf::from(format!("/Volumes/SSD/Photographs/{place}/{name}")),
        fingerprint: format!("{n:064x}"),
        file_identity: format!("unix:1:{}", 1000 + n),
        byte_len: 1000 + u64::from(n),
        width: 6000,
        height: 4000,
        catalog_folder_id: folder_id(folder),
        volume_id: ssd(),
        developed_ms: 1_000 + i64::from(n),
        removed_ms: None,
        availability: FileAvailability::Available,
        checked_ms: 1,
        develop_moment: None,
        header: HeaderMetadata {
            capture: CaptureTime::from_exif(&format!("{day} 10:00:{n:02}"), None, Some("+02:00")),
            position: GeoPosition::new(47.66, 9.175, None),
            camera: Some(camera),
            lens: raw.then(|| "Summilux 28 mm f/1.7 ASPH.".into()),
            exposure: Exposure::default(),
            dimensions: Some(Dimensions {
                width: 6000,
                height: 4000,
            }),
            orientation: None,
            thumbnail: None,
        },
        place: Some(place.into()),
    }
}

/// A catalog of eight photographs: four from Konstanz (Leica, RAW but one) and three from Brighton
/// (Sony JPEGs) in September 2026, and one from Lake in 2025 nested in Bodensee; an empty folder;
/// "Print order" holding the first two, and the group "Portfolio" holding "Landscapes".
fn seeded() -> PathBuf {
    let dir = luxforge_testbase::paths::temp_dir("select-catalog");
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
        .folders(&[
            folder("bodensee01", "Bodensee", None),
            folder("konstanz01", "Konstanz \u{b7} Sep 2026", None),
            folder("brighton01", "Brighton \u{b7} Sep 2026", None),
            folder("empty00001", "Unsorted", None),
            folder("lake000001", "Lake", Some("bodensee01")),
        ])
        .unwrap();
    let rows = seeder
        .assets(&[
            photo(1, "konstanz01", "2026:09:12", "Konstanz", leica(), true),
            photo(2, "konstanz01", "2026:09:12", "Konstanz", leica(), true),
            photo(3, "konstanz01", "2026:09:13", "Konstanz", leica(), true),
            photo(4, "konstanz01", "2026:09:13", "Konstanz", leica(), false),
            photo(5, "brighton01", "2026:09:10", "Brighton", sony(), false),
            photo(6, "brighton01", "2026:09:10", "Brighton", sony(), false),
            photo(7, "brighton01", "2026:09:10", "Brighton", sony(), false),
            photo(8, "lake000001", "2025:05:01", "Lake", leica(), true),
        ])
        .unwrap();
    let collection = |id: &str, name: &str, parent: Option<&str>, kind| Collection {
        id: collection_id(id),
        name: name.into(),
        parent_id: parent.map(collection_id),
        kind,
        query: None,
        created_ms: 1,
        count: None,
    };
    seeder
        .collections(&[
            collection("portfolio1", "Portfolio", None, CollectionKind::Group),
            collection(
                "landscape1",
                "Landscapes",
                Some("portfolio1"),
                CollectionKind::Collection,
            ),
            collection(
                "prints0001",
                "Print order",
                None,
                CollectionKind::Collection,
            ),
        ])
        .unwrap();
    seeder
        .members(&[
            (collection_id("prints0001"), rows[0], 1),
            (collection_id("prints0001"), rows[1], 1),
        ])
        .unwrap();
    seeder.finish().unwrap();
    catalog
}

/// The editor over the seeded catalog with Select shown, and an independent client of the same
/// owner, which writes what an agent writes and reads the catalog back.
fn catalog_editor() -> (Editor, PathBuf, ClientId) {
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
    let agent = editor.owner.register();
    settle(&mut editor);
    (editor, catalog, agent)
}

fn act(editor: &mut Editor, action: CatalogAction) {
    let _ = editor.update(Message::Select(SelectMessage::Catalog(
        CatalogMessage::Act(action),
    )));
}

/// One request through the independent client.
fn ask(editor: &Editor, agent: ClientId, method: &str, params: Value) -> Value {
    call(&editor.owner, agent, method, params)
        .unwrap_or_else(|error| panic!("{method}: {error}"))
        .0
}

/// Run everything the editor has asked the owner for, as its tasks would, until nothing is in
/// flight or wanted: the reads showing Select or a change makes (the counts with the folders and
/// collections), a staleness check, an evaluation, a change's label, the rows near the screen, the
/// source's total and the selection's rows.
fn settle(editor: &mut Editor) {
    for _ in 0..12 {
        answer_reads(editor);
        if editor.select.check.in_flight() {
            let checked =
                crate::app::select::session_now(&editor.owner, editor.client).map(Box::new);
            let _ = editor.update(Message::Select(SelectMessage::Checked(checked)));
        }
        if editor.select.state.loading {
            evaluate(editor);
        }
        if let Some(sequence) = editor.select.label {
            let result = crate::app::select::label_now(&editor.owner, editor.client, sequence);
            let _ = editor.update(Message::Select(SelectMessage::Labelled {
                sequence,
                result,
            }));
        }
        read_rows(editor);
        if let Some((source, sequence)) = editor.select.catalog.totalling.clone() {
            let result = total_now(&editor.owner, editor.client, &source);
            let _ = editor.update(Message::Select(SelectMessage::Catalog(
                CatalogMessage::Totalled {
                    source,
                    sequence,
                    result,
                },
            )));
        }
        if let Some((revision, ranges)) = editor.select.catalog.reading.clone() {
            let result = selection_rows_now(&editor.owner, editor.client, revision, &ranges);
            let _ = editor.update(Message::Select(SelectMessage::Catalog(
                CatalogMessage::SelectionRows {
                    revision,
                    ranges,
                    result,
                },
            )));
        }
        if editor.select.catalog.lists.in_flight() {
            let lists = lists_now(&editor.owner, editor.client);
            let _ = editor.update(Message::Select(SelectMessage::Catalog(
                CatalogMessage::Lists(lists),
            )));
        }
        if editor.select_reads_quiet() {
            return;
        }
    }
    panic!(
        "Select did not settle: {}",
        editor.select_summary()["catalog"]
    );
}

/// The query of the view on screen, as `browse.view` answered it.
fn shown(editor: &Editor) -> Value {
    serde_json::to_value(&editor.select.state.summary.as_ref().unwrap().query).unwrap()
}

/// The last library request the editor sent: its method and parameters without their identity,
/// and whether the owner accepted it.
fn sent(editor: &Editor) -> (String, Value) {
    let library = editor.select.library.clone().expect("a library request");
    assert!(library["error"].is_null(), "refused: {library}");
    let mut params = library["params"].clone();
    assert_eq!(params["mutation"]["actor"], "desktop");
    params.as_object_mut().unwrap().remove("mutation");
    (library["method"].as_str().unwrap().to_owned(), params)
}

/// The desktop's changes in the journal, oldest first, as `[method, label]`.
fn journal(editor: &Editor, agent: ClientId) -> Vec<Value> {
    ask(editor, agent, "library.journal", json!({"limit": 500}))["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|change| change["actor"] == "desktop")
        .map(|change| json!([change["method"], change["label"]]))
        .collect()
}

fn press(editor: &mut Editor, item: u32, shift: bool) {
    let layout = &editor.select.layout;
    let cell = layout.cell(layout.cell_of_item(item).unwrap());
    let press = GridPress {
        cell: cell.cell,
        item: cell.item,
        span: cell.span,
        modifiers: PressModifiers {
            shift,
            command: false,
        },
        double: false,
    };
    let _ = editor.update(Message::Select(SelectMessage::Press(press)));
    settle(editor);
}

/// The search, a Metadata browser value and the Edited chip change the query and nothing else,
/// each exactly as an independent client writes `browse.view`, and the view the owner answers the
/// client's; the columns' counts are the client's own `browse.facets` over the source and filter;
/// the bar says "N of M" against the source counted with no filter; and Save as smart collection…
/// stores exactly the query shown, which the smart collection then views.
#[test]
fn catalog_browse_queries_counts_and_a_smart_collection_on_a_real_owner() {
    let (mut editor, catalog, agent) = catalog_editor();
    // The folders and collections are the owner's.
    let folders = ask(&editor, agent, "folder.list", json!({}));
    let listed = editor.select.state.catalog.folders.as_ref().unwrap();
    assert_eq!(serde_json::to_value(listed).unwrap(), folders);
    let sources = &editor.workspace.select.catalog.sources;
    let names: Vec<&str> = sources
        .folders
        .iter()
        .map(|row| row.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "2026",
            "Brighton \u{b7} Sep 2026",
            "Konstanz \u{b7} Sep 2026",
            "2025",
            "Empty",
            "Unsorted"
        ]
    );
    let collections: Vec<&str> = sources
        .collections
        .iter()
        .map(|row| row.name.as_str())
        .collect();
    assert_eq!(collections, ["Portfolio", "Landscapes", "Print order"]);

    // All photographs, newest first.
    let _ = editor.update(Message::Select(SelectMessage::Source(
        luxforge_core::catalog_types::ViewSource::AllPhotographs,
    )));
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        1000.0, 700.0,
    ))));
    settle(&mut editor);
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 8);
    let base = shown(&editor);

    // The search: the query with its text and nothing else.
    act(&mut editor, CatalogAction::Search("konstanz".into()));
    assert!(editor.select.state.loading, "the search is evaluated");
    settle(&mut editor);
    let mut expected = base.clone();
    expected["filter"] = json!({"text": "konstanz"});
    assert_eq!(shown(&editor), expected);
    let client = ask(&editor, agent, "browse.view", expected.clone());
    assert_eq!(
        editor.select.state.summary.as_ref().unwrap().count,
        client["count"].as_u64().unwrap() as u32
    );
    assert_eq!(client["count"], 4);
    // Four of eight.
    assert_eq!(
        editor
            .workspace
            .select
            .catalog
            .filter
            .as_ref()
            .unwrap()
            .count,
        "4 of 8"
    );

    // The Metadata browser's counts are the client's own facets for the source and filter.
    act(&mut editor, CatalogAction::Metadata);
    let facets = ask(
        &editor,
        agent,
        "browse.facets",
        json!({
            "source": {"kind": "all-photographs"},
            "filter": {"text": "konstanz"},
            "facets": ["date", "place", "camera", "lens", "kind"],
        }),
    );
    assert_eq!(
        serde_json::to_value(editor.select.state.facets.as_ref().unwrap()).unwrap(),
        facets
    );
    let columns = editor.workspace.select.catalog.metadata.clone().unwrap();
    let camera = &columns[2];
    let body = facets["counts"]["camera"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["value"] == "LEICA CAMERA AG|LEICA Q2|")
        .expect("the Leica counted")
        .clone();
    let counted = body["count"].clone();
    let leica = camera
        .rows
        .iter()
        .find(|row| Some(row.label.as_str()) == body["label"].as_str())
        .expect("the Leica listed by its label");
    assert_eq!(leica.count, counted.as_u64().unwrap().to_string());
    // Pressing it narrows the view to it: its camera and nothing else.
    let change = leica.change.clone().unwrap();
    assert_eq!(
        change,
        CatalogChange::Camera(Some(BodyKey("LEICA CAMERA AG|LEICA Q2|".into())))
    );
    act(&mut editor, CatalogAction::Change(change));
    settle(&mut editor);
    let mut expected_camera = expected.clone();
    expected_camera["filter"]["cameras"] = json!(["LEICA CAMERA AG|LEICA Q2|"]);
    assert_eq!(shown(&editor), expected_camera);
    assert_eq!(
        editor.select.state.summary.as_ref().unwrap().count,
        counted.as_u64().unwrap() as u32,
        "the value's count is the view it gives"
    );
    // The Edited chip: edited or not, and nothing else.
    act(
        &mut editor,
        CatalogAction::Change(CatalogChange::Edited(Some(false))),
    );
    settle(&mut editor);
    let mut expected_edited = expected_camera.clone();
    expected_edited["filter"]["edited"] = json!(false);
    assert_eq!(shown(&editor), expected_edited);
    // The sort from the strip changes the sort and nothing else.
    let _ = editor.update(Message::Select(SelectMessage::Change(
        crate::state::select::QueryChange::Sort(luxforge_core::catalog_types::SortKey::FileName),
    )));
    settle(&mut editor);
    let mut expected_sort = expected_edited.clone();
    expected_sort["sort"] = json!({"key": "file-name"});
    assert_eq!(shown(&editor), expected_sort);
    let query = shown(&editor);
    let count = editor.select.state.summary.as_ref().unwrap().count;

    // Save as smart collection…: exactly the query shown, as an agent writes it.
    act(
        &mut editor,
        CatalogAction::Name(NamingTarget::SmartCollection),
    );
    act(
        &mut editor,
        CatalogAction::NameText("Konstanz Leica".into()),
    );
    act(&mut editor, CatalogAction::Submit);
    assert_eq!(
        sent(&editor),
        (
            "collection.create-smart".to_owned(),
            json!({"name": "Konstanz Leica", "query": query})
        )
    );
    assert!(editor.select.state.catalog.naming.is_none());
    settle(&mut editor);
    let stored = ask(&editor, agent, "collection.list", json!({}));
    let smart = stored["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|collection| collection["name"] == "Konstanz Leica")
        .expect("saved")
        .clone();
    assert_eq!(smart["query"], query, "it stores exactly the query shown");
    // Listed and viewed, in the sort it was saved with.
    let row = editor
        .workspace
        .select
        .catalog
        .sources
        .collections
        .iter()
        .find(|row| row.name == "Konstanz Leica")
        .expect("listed")
        .clone();
    act(&mut editor, row.press.unwrap());
    settle(&mut editor);
    let viewed = shown(&editor);
    assert_eq!(
        viewed["source"],
        json!({"kind": "collection", "collection_id": smart["id"]})
    );
    assert_eq!(viewed["sort"], json!({"key": "file-name"}));
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, count);
    assert_eq!(
        editor
            .workspace
            .select
            .catalog
            .filter
            .as_ref()
            .unwrap()
            .save_refused
            .as_deref(),
        Some("A smart collection cannot be saved from another smart collection")
    );
    assert_eq!(
        journal(&editor, agent),
        [json!([
            "collection.create-smart",
            "Created smart collection Konstanz Leica"
        ])]
    );
    finish(editor, catalog);
}

/// Folders and collections organized from the sources panel: a folder made, renamed, nested,
/// merged and deleted, a collection made and renamed, each one library change of this desktop's
/// sent as an agent writes it, listed again once it lands, and undone with `Cmd+Z`.
#[test]
fn catalog_browse_folders_and_collections_are_organized_on_a_real_owner() {
    let (mut editor, catalog, agent) = catalog_editor();
    let list = |editor: &Editor| {
        editor
            .select
            .state
            .catalog
            .folders
            .as_ref()
            .unwrap()
            .folders
            .iter()
            .map(|folder| (folder.name.clone(), folder.parent_id.clone()))
            .collect::<Vec<_>>()
    };
    // The `+` makes a folder with the name typed.
    act(
        &mut editor,
        CatalogAction::Name(NamingTarget::NewFolder { parent: None }),
    );
    act(&mut editor, CatalogAction::NameText("Lakes".into()));
    act(&mut editor, CatalogAction::Submit);
    assert_eq!(
        sent(&editor),
        ("folder.create".to_owned(), json!({"name": "Lakes"}))
    );
    settle(&mut editor);
    assert!(list(&editor).iter().any(|(name, _)| name == "Lakes"));
    let lakes = editor
        .select
        .state
        .catalog
        .folders
        .as_ref()
        .unwrap()
        .folders
        .iter()
        .find(|folder| folder.name == "Lakes")
        .unwrap()
        .id
        .clone();
    // Renamed in place.
    act(
        &mut editor,
        CatalogAction::Name(NamingTarget::RenameFolder(folder_id("konstanz01"))),
    );
    assert_eq!(
        editor.select.state.catalog.naming.as_ref().unwrap().text,
        "Konstanz \u{b7} Sep 2026",
        "a rename starts from the name"
    );
    act(&mut editor, CatalogAction::NameText("Konstanz".into()));
    act(&mut editor, CatalogAction::Submit);
    assert_eq!(
        sent(&editor),
        (
            "folder.rename".to_owned(),
            json!({"folder_id": "folder-konstanz01", "name": "Konstanz"})
        )
    );
    settle(&mut editor);
    // Nested in Lakes.
    act(
        &mut editor,
        CatalogAction::MoveFolder {
            folder: folder_id("brighton01"),
            parent: Some(lakes.clone()),
        },
    );
    assert_eq!(
        sent(&editor),
        (
            "folder.move".to_owned(),
            json!({"folder_id": "folder-brighton01", "parent_id": lakes})
        )
    );
    settle(&mut editor);
    assert!(
        list(&editor).contains(&("Brighton \u{b7} Sep 2026".into(), Some(lakes.clone()))),
        "{:?}",
        list(&editor)
    );
    // Lake merged into Konstanz: its photograph goes with it, and it is gone.
    act(
        &mut editor,
        CatalogAction::MergeFolder {
            folder: folder_id("lake000001"),
            into: folder_id("konstanz01"),
        },
    );
    assert_eq!(
        sent(&editor),
        (
            "folder.merge".to_owned(),
            json!({"folder_id": "folder-lake000001", "into_id": "folder-konstanz01"})
        )
    );
    settle(&mut editor);
    assert!(!list(&editor).iter().any(|(name, _)| name == "Lake"));
    let konstanz = ask(
        &editor,
        agent,
        "browse.facets",
        json!({"source": {"kind": "catalog-folder", "folder_id": "folder-konstanz01"}, "facets": ["kind"]}),
    );
    let photographs: u64 = konstanz["counts"]["kind"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value["count"].as_u64().unwrap())
        .sum();
    assert_eq!(photographs, 5);
    // The empty folder is deleted from its menu; a folder with photographs is not offered it.
    act(
        &mut editor,
        CatalogAction::Menu(Some(crate::state::select_catalog::CatalogMenu::Folder(
            folder_id("empty00001"),
        ))),
    );
    let row = editor
        .workspace
        .select
        .catalog
        .sources
        .folders
        .iter()
        .find(|row| row.name == "Unsorted")
        .unwrap()
        .clone();
    let delete = row.menu.unwrap().last().unwrap().action.clone().unwrap();
    act(&mut editor, delete);
    assert_eq!(
        sent(&editor),
        (
            "folder.delete".to_owned(),
            json!({"folder_id": "folder-empty00001"})
        )
    );
    settle(&mut editor);
    // A collection made and renamed.
    act(
        &mut editor,
        CatalogAction::Name(NamingTarget::NewCollection {
            kind: CollectionKind::Collection,
            parent: Some(collection_id("portfolio1")),
        }),
    );
    act(&mut editor, CatalogAction::NameText("Nights".into()));
    act(&mut editor, CatalogAction::Submit);
    assert_eq!(
        sent(&editor),
        (
            "collection.create".to_owned(),
            json!({"name": "Nights", "kind": "collection", "parent_id": "collection-portfolio1"})
        )
    );
    settle(&mut editor);
    act(
        &mut editor,
        CatalogAction::Name(NamingTarget::RenameCollection(collection_id("prints0001"))),
    );
    act(&mut editor, CatalogAction::NameText("Prints".into()));
    act(&mut editor, CatalogAction::Submit);
    assert_eq!(
        sent(&editor),
        (
            "collection.rename".to_owned(),
            json!({"collection_id": "collection-prints0001", "name": "Prints"})
        )
    );
    settle(&mut editor);
    // A name already taken among its siblings is refused, and stays in its field.
    act(
        &mut editor,
        CatalogAction::Name(NamingTarget::RenameFolder(lakes.clone())),
    );
    act(&mut editor, CatalogAction::NameText("Bodensee".into()));
    act(&mut editor, CatalogAction::Submit);
    assert_eq!(
        editor.select.library.as_ref().unwrap()["error"]["code"],
        "conflict"
    );
    assert!(editor.select.state.catalog.naming.is_some());
    assert!(
        editor
            .status
            .text
            .starts_with("Could not rename the folder"),
        "{}",
        editor.status.text
    );
    act(&mut editor, CatalogAction::Menu(None));
    assert!(editor.select.state.catalog.naming.is_none());

    // Each is one library change of this desktop's.
    let changes = journal(&editor, agent);
    assert_eq!(
        changes,
        [
            json!(["folder.create", "Created folder Lakes"]),
            json!([
                "folder.rename",
                "Renamed folder Konstanz \u{b7} Sep 2026 to Konstanz"
            ]),
            json!([
                "folder.move",
                "Moved folder Brighton \u{b7} Sep 2026 into Lakes"
            ]),
            json!(["folder.merge", "Merged Lake into Konstanz"]),
            json!(["folder.delete", "Deleted folder Unsorted"]),
            json!(["collection.create", "Created collection Nights"]),
            json!([
                "collection.rename",
                "Renamed collection Print order to Prints"
            ]),
        ]
    );
    // `Cmd+Z` undoes them newest first: the rename, then the collection, then the delete.
    for _ in 0..3 {
        let _ = editor.update(Message::Select(SelectMessage::Undo));
        settle(&mut editor);
    }
    let collections = ask(&editor, agent, "collection.list", json!({}));
    let names: Vec<&str> = collections["collections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|collection| collection["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"Print order") && !names.contains(&"Nights"),
        "{names:?}"
    );
    assert!(list(&editor).iter().any(|(name, _)| name == "Unsorted"));
    assert!(
        editor
            .status
            .text
            .starts_with("Undid Deleted folder Unsorted"),
        "{}",
        editor.status.text
    );
    // The merge undone brings Lake back with its photograph.
    let _ = editor.update(Message::Select(SelectMessage::Undo));
    settle(&mut editor);
    assert!(list(&editor).contains(&("Lake".into(), Some(folder_id("bodensee01")))));
    finish(editor, catalog);
}

/// The selection moved to a folder and added to a collection from the Info panel: `asset.move`
/// and `collection.add` of the selection as an agent writes them, the batch form counting the
/// selection's folders and collections before and after, and `Cmd+Z` undoing the add.
#[test]
fn catalog_browse_the_batch_form_moves_and_collects_the_selection_on_a_real_owner() {
    let (mut editor, catalog, agent) = catalog_editor();
    let _ = editor.update(Message::Select(SelectMessage::Source(
        luxforge_core::catalog_types::ViewSource::AllPhotographs,
    )));
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        1000.0, 700.0,
    ))));
    settle(&mut editor);
    // Sorted newest first: Konstanz 13 Sep (4, 3), 12 Sep (2, 1), Brighton (7, 6, 5), Lake (8).
    press(&mut editor, 2, false);
    press(&mut editor, 6, true);
    let info = editor.workspace.select.catalog.info.clone().unwrap();
    assert_eq!(info.title, "5 selected");
    let chips = |chips: &[crate::state::select_catalog::OrganizeChip]| {
        let mut chips: Vec<(String, Option<String>)> = chips
            .iter()
            .map(|chip| (chip.label.clone(), chip.partial.clone()))
            .collect();
        chips.sort();
        chips
    };
    assert_eq!(
        chips(&info.folders),
        [
            ("Brighton \u{b7} Sep 2026".into(), Some("3 of 5".into())),
            ("Konstanz \u{b7} Sep 2026".into(), Some("2 of 5".into())),
        ]
    );
    assert_eq!(
        chips(&info.collections),
        [("Print order".into(), Some("2 of 5".into()))]
    );
    // Add to…: the selection, into Landscapes.
    act(
        &mut editor,
        CatalogAction::AddPhotos(collection_id("landscape1")),
    );
    assert_eq!(
        sent(&editor),
        (
            "collection.add".to_owned(),
            json!({"collection_id": "collection-landscape1", "targets": {"kind": "selection"}})
        )
    );
    settle(&mut editor);
    let info = editor.workspace.select.catalog.info.clone().unwrap();
    assert_eq!(info.count, 5, "the selection is kept");
    assert!(
        chips(&info.collections).contains(&("Portfolio \u{203a} Landscapes".into(), None)),
        "{:?}",
        info.collections
    );
    let counted = |editor: &Editor| {
        ask(editor, agent, "collection.list", json!({}))["collections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|collection| collection["name"] == "Landscapes")
            .unwrap()["count"]
            .clone()
    };
    assert_eq!(counted(&editor), 5);
    // `Cmd+Z` undoes the add, one change.
    let _ = editor.update(Message::Select(SelectMessage::Undo));
    settle(&mut editor);
    assert_eq!(counted(&editor), 0);
    // Move to…: the selection, into Brighton.
    act(
        &mut editor,
        CatalogAction::MovePhotos(folder_id("brighton01")),
    );
    assert_eq!(
        sent(&editor),
        (
            "asset.move".to_owned(),
            json!({"targets": {"kind": "selection"}, "folder_id": "folder-brighton01"})
        )
    );
    settle(&mut editor);
    let info = editor.workspace.select.catalog.info.clone().unwrap();
    assert_eq!(
        chips(&info.folders),
        [("Brighton \u{b7} Sep 2026".into(), None)]
    );
    let brighton = ask(
        &editor,
        agent,
        "browse.view",
        json!({"source": {"kind": "catalog-folder", "folder_id": "folder-brighton01"}}),
    );
    assert_eq!(brighton["count"], 5);
    assert_eq!(
        journal(&editor, agent),
        [
            json!(["collection.add", "Added 5 to Portfolio \u{203a} Landscapes"]),
            json!([
                "library.undo",
                "Undo Added 5 to Portfolio \u{203a} Landscapes"
            ]),
            json!([
                "asset.move",
                "Moved 2 photographs to Brighton \u{b7} Sep 2026"
            ]),
        ]
    );
    // A whole-view selection larger than the rows near the screen is read for the panel.
    let _ = editor.update(Message::Select(SelectMessage::SelectAll));
    settle(&mut editor);
    let info = editor.workspace.select.catalog.info.clone().unwrap();
    assert_eq!(info.count, 8);
    assert!(info.organize_note.is_none(), "{:?}", info.organize_note);
    finish(editor, catalog);
}
