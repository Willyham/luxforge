//! The catalog's view model against synthetic owner answers: the query each chip, column value and
//! search makes (and nothing else), the requests each organizing gesture sends as an agent writes
//! them, the folders by year and the collections, the Metadata browser's columns and counts, the
//! filter bar's conditions and count, the folder and collection menus, and the batch form.
use super::*;
use crate::state::select::{SelectionModel, Shown};
use luxforge_core::{
    AssetId,
    catalog_types::{
        BrowseSession, Dimensions, Exposure, FacetValue, Facets, FileAvailability, GroupLayout,
        LibraryChangeSeq, PositionRange, PreviewState, RowItem, SortKey, ViewSelection, ViewSort,
        ViewSummary,
    },
};
use serde_json::json;

fn folder_id(name: &str) -> CatalogFolderId {
    CatalogFolderId::parse(format!("folder-{name}")).unwrap()
}

fn collection_id(name: &str) -> CollectionId {
    CollectionId::parse(format!("collection-{name}")).unwrap()
}

fn asset(n: u32) -> AssetId {
    AssetId::parse(format!("asset-{n:012}")).unwrap()
}

fn folder(
    id: &str,
    name: &str,
    parent: Option<&str>,
    count: u32,
    year: Option<i32>,
) -> CatalogFolder {
    CatalogFolder {
        id: folder_id(id),
        name: name.into(),
        parent_id: parent.map(folder_id),
        created_ms: 1,
        event: None,
        count,
        year,
    }
}

fn collection(id: &str, name: &str, parent: Option<&str>, kind: CollectionKind) -> Collection {
    Collection {
        id: collection_id(id),
        name: name.into(),
        parent_id: parent.map(collection_id),
        kind,
        query: (kind == CollectionKind::Smart).then(|| ViewQuery {
            sort: ViewSort {
                key: SortKey::FileName,
                descending: false,
            },
            ..ViewQuery::of(ViewSource::AllPhotographs)
        }),
        created_ms: 1,
        count: (kind == CollectionKind::Collection).then_some(58),
    }
}

/// The catalog board's folders: 2026 with Konstanz, Sa Pa and Brighton, 2025 with Bodensee (no
/// photographs of its own) holding Reichenau and Lake, an undated folder and an empty one.
fn folders() -> CatalogFolders {
    CatalogFolders {
        folders: vec![
            folder("bodensee01", "Bodensee", None, 0, None),
            folder(
                "brighton01",
                "Brighton \u{b7} Sep 2026",
                None,
                31,
                Some(2026),
            ),
            folder("empty00001", "Empty for now", None, 0, None),
            folder(
                "konstanz01",
                "Konstanz \u{b7} Sep 2026",
                None,
                18,
                Some(2026),
            ),
            folder("sapa000001", "Sa Pa \u{b7} Sep 2026", None, 6, Some(2026)),
            folder("undated001", "From Anna", None, 4, None),
            folder("lake000001", "Lake", Some("bodensee01"), 12, Some(2025)),
            folder(
                "reichenau1",
                "Reichenau \u{b7} Aug 2025",
                Some("bodensee01"),
                212,
                Some(2025),
            ),
        ],
    }
}

fn collections() -> Collections {
    Collections {
        collections: vec![
            collection("portfolio1", "Portfolio", None, CollectionKind::Group),
            collection(
                "prints0001",
                "Print order",
                None,
                CollectionKind::Collection,
            ),
            collection("drone00001", "Drone", None, CollectionKind::Smart),
            collection(
                "landscape1",
                "Landscapes",
                Some("portfolio1"),
                CollectionKind::Collection,
            ),
        ],
    }
}

fn catalog_state(source: ViewSource) -> SelectState {
    let mut state = SelectState {
        shown: Shown::Select,
        query: Some(super::super::select::source_query(source)),
        ..SelectState::default()
    };
    state.catalog.folders = Some(folders());
    state.catalog.collections = Some(collections());
    state
}

fn summary_of(query: ViewQuery, count: u32) -> ViewSummary {
    ViewSummary {
        revision: 7,
        query,
        count,
        picked: 0,
        in_catalog: count,
        unavailable: 0,
        groups: GroupLayout::default(),
        library_sequence: LibraryChangeSeq(4),
        index_revision: 1,
    }
}

fn photo_row(position: u32, folder: &str, collections: &[&str], edited: bool) -> ViewRow {
    ViewRow {
        position,
        item: RowItem::Photo {
            asset_id: asset(position),
        },
        path: format!("/Volumes/SSD/2026/L10{position:05}.DNG").into(),
        file_name: format!("L10{position:05}.DNG"),
        kind: if position % 5 == 4 {
            SourceTag::Jpeg
        } else {
            SourceTag::Raw
        },
        dimensions: Some(Dimensions {
            width: 6000,
            height: 4000,
        }),
        orientation: None,
        capture: Some(format!("2026-09-{:02}T10:00:00+02:00", 4 + position)),
        place: Some(["Konstanz", "Sa Pa", "Brighton"][position as usize % 3].into()),
        camera: Some("Leica Q2".into()),
        lens: None,
        exposure: Exposure::default(),
        moment: None,
        picked: false,
        developed_as: None,
        edited,
        folder_id: Some(folder_id(folder)),
        collections: collections.iter().map(|id| collection_id(id)).collect(),
        availability: FileAvailability::Available,
        preview: PreviewState::Pending,
    }
}

/// `state` holding a view of `rows`, every one read.
fn with_rows(state: &mut SelectState, rows: Vec<ViewRow>) {
    let query = state.query.clone().unwrap();
    let count = rows.len() as u32;
    state.summary = Some(summary_of(query, count));
    state.rows.reset(7, count);
    let request = state.rows.next_request(0..count).unwrap();
    assert!(state.rows.answered(7, request.from, rows, 0..count));
}

fn selected(start: u32, len: u32, active: u32) -> SelectionModel {
    SelectionModel::of(
        &BrowseSession {
            revision: 7,
            count: len,
            selection: ViewSelection {
                count: len,
                ranges: vec![PositionRange { start, len }],
                active: Some(active),
            },
            ..BrowseSession::default()
        },
        Some(7),
    )
}

fn mutation() -> MutationRequest {
    MutationRequest {
        request_id: "request-test".into(),
        actor: "desktop".into(),
    }
}

/// Every chip, column value and search changes its own part of the query and nothing else, and
/// the whole query is what an independent JSON client writes for `browse.view`.
#[test]
fn catalog_browse_each_change_changes_only_its_part_of_the_query() {
    let base = ViewQuery {
        filter: ViewFilter {
            kinds: vec![SourceTag::Raw],
            ..ViewFilter::default()
        },
        ..super::super::select::source_query(ViewSource::AllPhotographs)
    };
    let september = month_range(Month {
        year: 2026,
        month: 9,
    })
    .unwrap();
    let cases: Vec<(CatalogChange, serde_json::Value)> = vec![
        (
            CatalogChange::Text("  Konstanz ".into()),
            json!({"text": "Konstanz", "kinds": ["raw"]}),
        ),
        (
            CatalogChange::Edited(Some(true)),
            json!({"edited": true, "kinds": ["raw"]}),
        ),
        (
            CatalogChange::Dates(Some(september)),
            json!({"dates": {"from": "2026-09-01", "to": "2026-09-30"}, "kinds": ["raw"]}),
        ),
        (
            CatalogChange::Place(Some("Sa Pa".into())),
            json!({"places": ["Sa Pa"], "kinds": ["raw"]}),
        ),
        (
            CatalogChange::Camera(Some(BodyKey("LEICA CAMERA AG|LEICA Q2|".into()))),
            json!({"cameras": ["LEICA CAMERA AG|LEICA Q2|"], "kinds": ["raw"]}),
        ),
        (
            CatalogChange::Lens(Some("Summilux 28 mm".into())),
            json!({"lenses": ["Summilux 28 mm"], "kinds": ["raw"]}),
        ),
    ];
    for (change, filter) in cases {
        let query = changed(&base, &change);
        // Only the filter moved; the source, sort, grouping and thresholds are the base's.
        assert_eq!(
            ViewQuery {
                filter: base.filter.clone(),
                ..query.clone()
            },
            base,
            "{change:?}"
        );
        let sent = super::super::select::view_params(&query);
        assert_eq!(
            sent,
            json!({
                "source": {"kind": "all-photographs"},
                "filter": filter,
                "sort": {"key": "capture-time", "descending": true},
                "grouping": "none",
                "thresholds": serde_json::to_value(base.thresholds).unwrap(),
            }),
            "{change:?}"
        );
        // Clearing it gives the base back.
        let cleared = match change {
            CatalogChange::Text(_) => CatalogChange::Text(String::new()),
            CatalogChange::Edited(_) => CatalogChange::Edited(None),
            CatalogChange::Dates(_) => CatalogChange::Dates(None),
            CatalogChange::Place(_) => CatalogChange::Place(None),
            CatalogChange::Camera(_) => CatalogChange::Camera(None),
            CatalogChange::Lens(_) => CatalogChange::Lens(None),
        };
        assert_eq!(changed(&query, &cleared), base);
    }
    assert_eq!(search_text("   "), None);
    assert_eq!(
        search_text(&"x".repeat(MAX_FILTER_TEXT + 10)).map(|text| text.chars().count()),
        Some(MAX_FILTER_TEXT)
    );
    // A year and a month are whole ranges; the chip names them.
    assert_eq!(dates_label(year_range(2025).unwrap()), "2025");
    assert_eq!(dates_label(september), "September 2026");
    let december = month_range(Month {
        year: 2025,
        month: 12,
    })
    .unwrap();
    assert_eq!(december.to, LocalDay::from_ymd(2025, 12, 31).unwrap());
}

/// Each organizing gesture sends exactly what an agent writes.
#[test]
fn catalog_browse_requests_are_what_an_agent_writes() {
    let m = mutation();
    let envelope = json!({"request_id": "request-test", "actor": "desktop"});
    let konstanz = folder_id("konstanz01");
    let bodensee = folder_id("bodensee01");
    let prints = collection_id("prints0001");
    assert_eq!(
        folder_create_params("Lakes", None, &m),
        json!({"name": "Lakes", "mutation": envelope})
    );
    assert_eq!(
        folder_create_params("Lakes", Some(&bodensee), &m),
        json!({"name": "Lakes", "parent_id": "folder-bodensee01", "mutation": envelope})
    );
    assert_eq!(
        folder_rename_params(&konstanz, "Konstanz", &m),
        json!({"folder_id": "folder-konstanz01", "name": "Konstanz", "mutation": envelope})
    );
    assert_eq!(
        folder_move_params(&konstanz, Some(&bodensee), &m),
        json!({"folder_id": "folder-konstanz01", "parent_id": "folder-bodensee01", "mutation": envelope})
    );
    assert_eq!(
        folder_move_params(&konstanz, None, &m),
        json!({"folder_id": "folder-konstanz01", "mutation": envelope})
    );
    assert_eq!(
        folder_merge_params(&konstanz, &bodensee, &m),
        json!({"folder_id": "folder-konstanz01", "into_id": "folder-bodensee01", "mutation": envelope})
    );
    assert_eq!(
        folder_delete_params(&konstanz, &m),
        json!({"folder_id": "folder-konstanz01", "mutation": envelope})
    );
    assert_eq!(
        asset_move_params(&konstanz, &m),
        json!({"targets": {"kind": "selection"}, "folder_id": "folder-konstanz01", "mutation": envelope})
    );
    assert_eq!(
        collection_create_params("Prints", CollectionKind::Collection, None, &m),
        json!({"name": "Prints", "kind": "collection", "mutation": envelope})
    );
    assert_eq!(
        collection_create_params(
            "Trips",
            CollectionKind::Group,
            Some(&collection_id("portfolio1")),
            &m
        ),
        json!({"name": "Trips", "kind": "group", "parent_id": "collection-portfolio1", "mutation": envelope})
    );
    let query = ViewQuery {
        filter: ViewFilter {
            edited: Some(true),
            ..ViewFilter::default()
        },
        ..super::super::select::source_query(ViewSource::AllPhotographs)
    };
    assert_eq!(
        smart_create_params("Edited", &query, &m),
        json!({"name": "Edited", "query": serde_json::to_value(&query).unwrap(), "mutation": envelope})
    );
    assert_eq!(
        collection_rename_params(&prints, "Prints", &m),
        json!({"collection_id": "collection-prints0001", "name": "Prints", "mutation": envelope})
    );
    assert_eq!(
        collection_delete_params(&prints, &m),
        json!({"collection_id": "collection-prints0001", "mutation": envelope})
    );
    assert_eq!(
        members_params(&prints, &m),
        json!({"collection_id": "collection-prints0001", "targets": {"kind": "selection"}, "mutation": envelope})
    );
    assert_eq!(
        facets_params(&query),
        json!({
            "source": {"kind": "all-photographs"},
            "filter": {"edited": true},
            "facets": ["date", "place", "camera", "lens", "kind"],
        })
    );
    // Over the catalog the shell asks for the Metadata browser's columns.
    assert_eq!(
        super::super::select::facets_params(&query),
        facets_params(&query)
    );
    assert_eq!(
        total_params(&ViewSource::AllPhotographs),
        json!({"source": {"kind": "all-photographs"}, "facets": ["kind"]})
    );
    for gesture in [
        CatalogGesture::CreateFolder,
        CatalogGesture::CreateGroup,
        CatalogGesture::CreateSmart,
    ] {
        assert!(gesture.creates());
    }
    assert!(!CatalogGesture::MoveFolder.creates());
    assert!(CatalogGesture::MovePhotos.of_selection());
    assert_eq!(CatalogGesture::CreateGroup.method(), "collection.create");
}

/// The folders are listed under their years, newest first, then Undated and Empty: a top-level
/// folder under the earliest year in it or its subfolders, counting them all, its subfolders under
/// it once opened; the newest year is open by default, and a year closes.
#[test]
fn catalog_browse_folders_are_listed_by_year() {
    let mut state = catalog_state(ViewSource::CatalogFolder {
        folder_id: folder_id("konstanz01"),
        subfolders: true,
    });
    let names = |sources: &CatalogSources| {
        sources
            .folders
            .iter()
            .map(|row| (row.name.clone(), row.indent, row.count.clone(), row.open))
            .collect::<Vec<_>>()
    };
    let sources = sources(&state);
    assert_eq!(
        names(&sources),
        vec![
            ("2026".into(), 0, None, Some(true)),
            (
                "Brighton \u{b7} Sep 2026".into(),
                1,
                Some("31".into()),
                None
            ),
            (
                "Konstanz \u{b7} Sep 2026".into(),
                1,
                Some("18".into()),
                None
            ),
            ("Sa Pa \u{b7} Sep 2026".into(), 1, Some("6".into()), None),
            ("2025".into(), 0, None, Some(false)),
            ("Undated".into(), 0, None, Some(false)),
            ("Empty".into(), 0, None, Some(true)),
            ("Empty for now".into(), 1, None, None),
        ]
    );
    assert!(sources.folders[2].selected, "the folder viewed is selected");
    assert_eq!(
        sources.folders[2].press,
        Some(CatalogAction::View(ViewSource::CatalogFolder {
            folder_id: folder_id("konstanz01"),
            subfolders: true,
        }))
    );
    // 2025 opened: Bodensee counts its subfolders, and opens to them.
    state.catalog.years.insert(YearGroup::Year(2025), true);
    state.catalog.years.insert(YearGroup::Year(2026), false);
    state.catalog.open_folders.insert(folder_id("bodensee01"));
    let sources = super::sources(&state);
    assert_eq!(
        names(&sources)[..5],
        [
            ("2026".into(), 0, None, Some(false)),
            ("2025".into(), 0, None, Some(true)),
            ("Bodensee".into(), 1, Some("224".into()), Some(true)),
            ("Lake".into(), 2, Some("12".into()), None),
            (
                "Reichenau \u{b7} Aug 2025".into(),
                2,
                Some("212".into()),
                None
            ),
        ]
    );
    // The collections: the group open over what it holds, the smart one and the plain one.
    let collections: Vec<(String, u8, Option<String>, CatalogIcon)> = sources
        .collections
        .iter()
        .map(|row| (row.name.clone(), row.indent, row.count.clone(), row.icon))
        .collect();
    assert_eq!(
        collections,
        vec![
            ("Portfolio".into(), 0, None, CatalogIcon::Group),
            (
                "Landscapes".into(),
                1,
                Some("58".into()),
                CatalogIcon::Collection
            ),
            (
                "Print order".into(),
                0,
                Some("58".into()),
                CatalogIcon::Collection
            ),
            ("Drone".into(), 0, None, CatalogIcon::Smart),
        ]
    );
    assert_eq!(
        sources.collections[0].press,
        Some(CatalogAction::ToggleGroup(collection_id("portfolio1"))),
        "a group opens rather than viewing"
    );
    // A new folder's name is typed at the top, a rename in the folder's own row.
    state.catalog.naming = Some(Naming {
        target: NamingTarget::NewFolder { parent: None },
        text: "Lakes".into(),
    });
    assert_eq!(
        super::sources(&state).folders[0].naming.as_deref(),
        Some("Lakes")
    );
    state.catalog.naming = Some(Naming {
        target: NamingTarget::RenameFolder(folder_id("lake000001")),
        text: "Lakes".into(),
    });
    let renaming = super::sources(&state);
    let row = renaming
        .folders
        .iter()
        .find(|row| row.naming.is_some())
        .unwrap();
    assert_eq!((row.name.as_str(), row.indent), ("Lake", 2));
}

/// A smart collection is viewed in the sort it was saved with.
#[test]
fn catalog_browse_a_smart_collection_is_viewed_as_saved() {
    let state = catalog_state(ViewSource::AllPhotographs);
    let drone = ViewSource::Collection {
        collection_id: collection_id("drone00001"),
    };
    let query = view_query(&state.catalog, drone.clone());
    assert_eq!(query.source, drone);
    assert_eq!(query.sort.key, SortKey::FileName);
    let plain = view_query(
        &state.catalog,
        ViewSource::Collection {
            collection_id: collection_id("prints0001"),
        },
    );
    assert_eq!(plain.sort.key, SortKey::CaptureTime);
    assert!(plain.sort.descending);
}

/// A folder's menu offers Delete only when it is empty, Move to… never itself or a folder inside
/// it, and Merge into… the same.
#[test]
fn catalog_browse_folder_menus_offer_what_the_owner_accepts() {
    let mut state = catalog_state(ViewSource::AllPhotographs);
    state.catalog.years.insert(YearGroup::Year(2025), true);
    let menu_of = |state: &SelectState, name: &str| {
        super::sources(state)
            .folders
            .into_iter()
            .find(|row| row.name == name)
            .and_then(|row| row.menu)
            .unwrap_or_default()
    };
    state.catalog.menu = Some(CatalogMenu::Folder(folder_id("bodensee01")));
    let menu = menu_of(&state, "Bodensee");
    let delete = menu.last().unwrap();
    assert_eq!(delete.label, "Delete folder");
    assert!(delete.action.is_none() && delete.reason.as_deref().unwrap().contains("224"));
    state.catalog.menu = Some(CatalogMenu::Folder(folder_id("empty00001")));
    let menu = menu_of(&state, "Empty for now");
    assert_eq!(
        menu.last().unwrap().action,
        Some(CatalogAction::DeleteFolder(folder_id("empty00001")))
    );
    // Move to…: the top level (where it is, so refused and checked) and every folder but itself
    // and those inside it.
    state.catalog.menu = Some(CatalogMenu::FolderMove(folder_id("bodensee01")));
    let menu = menu_of(&state, "Bodensee");
    assert!(menu[0].checked && menu[0].action.is_none());
    let labels: Vec<&str> = menu[1..]
        .iter()
        .map(|choice| choice.label.as_str())
        .collect();
    assert!(!labels.iter().any(|label| label.starts_with("Bodensee")));
    assert!(labels.contains(&"Konstanz \u{b7} Sep 2026"));
    state.catalog.menu = Some(CatalogMenu::FolderMove(folder_id("lake000001")));
    state.catalog.open_folders.insert(folder_id("bodensee01"));
    let menu = menu_of(&state, "Lake");
    assert_eq!(
        menu[0].action,
        Some(CatalogAction::MoveFolder {
            folder: folder_id("lake000001"),
            parent: None,
        })
    );
    let bodensee = menu
        .iter()
        .find(|choice| choice.label == "Bodensee")
        .unwrap();
    assert!(
        bodensee.checked && bodensee.action.is_none(),
        "already in it"
    );
    let reichenau = menu
        .iter()
        .find(|choice| choice.label == "Bodensee \u{203a} Reichenau \u{b7} Aug 2025")
        .unwrap();
    assert_eq!(
        reichenau.action,
        Some(CatalogAction::MoveFolder {
            folder: folder_id("lake000001"),
            parent: Some(folder_id("reichenau1")),
        })
    );
    state.catalog.menu = Some(CatalogMenu::FolderMerge(folder_id("bodensee01")));
    let menu = menu_of(&state, "Bodensee");
    assert!(
        menu.iter()
            .all(|choice| !choice.label.starts_with("Bodensee"))
    );
    // A group with collections in it is not deleted.
    state.catalog.menu = Some(CatalogMenu::Collection(collection_id("portfolio1")));
    let group = super::sources(&state).collections[0].menu.clone().unwrap();
    assert!(group.last().unwrap().action.is_none());
}

fn facet(value: Option<&str>, label: Option<&str>, count: u32) -> FacetValue {
    FacetValue {
        value: value.map(str::to_owned),
        label: label.map(str::to_owned),
        count,
    }
}

fn board_facets() -> Facets {
    Facets {
        counts: [
            (
                Facet::Date,
                vec![
                    facet(Some("2026-09-12"), None, 40),
                    facet(Some("2026-09-14"), None, 15),
                    facet(Some("2026-08-22"), None, 71),
                    facet(Some("2026-07-03"), None, 38),
                    facet(Some("2025-05-01"), None, 612),
                    facet(None, None, 3),
                ],
            ),
            (
                Facet::Place,
                vec![
                    facet(Some("Brighton"), None, 31),
                    facet(Some("Konstanz"), None, 18),
                    facet(Some("Sa Pa"), None, 6),
                    facet(None, None, 0),
                ],
            ),
            (
                Facet::Camera,
                vec![
                    facet(Some("SONY|ILCE-6700|"), Some("Sony ILCE-6700"), 31),
                    facet(Some("LEICA CAMERA AG|LEICA Q2|"), Some("Leica Q2"), 9),
                ],
            ),
            (
                Facet::Lens,
                vec![
                    facet(Some("Summilux 28 mm"), None, 9),
                    facet(None, None, 31),
                ],
            ),
            (Facet::Kind, vec![facet(Some("raw"), Some("RAW"), 40)]),
        ]
        .into_iter()
        .collect(),
    }
}

/// The Metadata browser: Date folds the days into years and the open year's months, newest first,
/// Undated last and inert; each other column says All (their sum, the view with its condition
/// cleared) and each value by its label, the value absent last and inert; the chosen value is
/// selected, and pressing it again clears it.
#[test]
fn catalog_browse_the_metadata_browser_counts_what_the_facets_answered() {
    let mut state = catalog_state(ViewSource::AllPhotographs);
    let september = month_range(Month {
        year: 2026,
        month: 9,
    })
    .unwrap();
    let query = changed(
        &changed(
            state.query.as_ref().unwrap(),
            &CatalogChange::Dates(Some(september)),
        ),
        &CatalogChange::Camera(Some(BodyKey("LEICA CAMERA AG|LEICA Q2|".into()))),
    );
    state.query = Some(query);
    state.facets = Some(board_facets());
    let columns = metadata(&state);
    let rows = |column: usize| {
        columns[column]
            .rows
            .iter()
            .map(|row| {
                (
                    row.label.as_str(),
                    row.count.as_str(),
                    row.indent,
                    row.selected,
                    row.change.is_some(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        rows(0),
        vec![
            ("2026", "164", false, false, true),
            ("September", "55", true, true, true),
            ("August", "71", true, false, true),
            ("July", "38", true, false, true),
            ("2025", "612", false, false, true),
            ("Undated", "3", false, false, false),
        ]
    );
    assert_eq!(
        columns[0].rows[1].change,
        Some(CatalogChange::Dates(None)),
        "the chosen month clears"
    );
    assert_eq!(
        columns[0].rows[4].change,
        Some(CatalogChange::Dates(year_range(2025)))
    );
    assert_eq!(
        rows(1),
        vec![
            ("All", "55", false, true, true),
            ("Brighton", "31", false, false, true),
            ("Konstanz", "18", false, false, true),
            ("Sa Pa", "6", false, false, true),
            ("No location", "0", false, false, false),
        ]
    );
    assert_eq!(
        rows(2),
        vec![
            ("All", "40", false, false, true),
            ("Leica Q2", "9", false, true, true),
            ("Sony ILCE-6700", "31", false, false, true),
        ]
    );
    assert_eq!(
        columns[2].rows[2].change,
        Some(CatalogChange::Camera(Some(BodyKey(
            "SONY|ILCE-6700|".into()
        ))))
    );
    assert_eq!(columns[2].rows[0].change, Some(CatalogChange::Camera(None)));
    assert_eq!(
        rows(3),
        vec![
            ("All", "40", false, true, true),
            ("Summilux 28 mm", "9", false, false, true),
            ("Unknown", "31", false, false, false),
        ]
    );
    // Its chips, in the accent with their clear ✕, and the count against the source's total.
    state.summary = Some(summary_of(state.query.clone().unwrap(), 9));
    state.catalog.total = Some(SourceTotal {
        source: ViewSource::AllPhotographs,
        sequence: 4,
        count: 842,
    });
    let bar = filter_bar(&state);
    assert_eq!(
        bar.conditions
            .iter()
            .map(|chip| (chip.glyph, chip.label.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (ConditionGlyph::Date, "September 2026"),
            (ConditionGlyph::Camera, "Leica Q2"),
        ]
    );
    assert_eq!(bar.count, "9 of 842");
    assert_eq!(bar.save_refused, None);
    // Without a filter the count is the view's.
    state.query = Some(super::super::select::source_query(
        ViewSource::AllPhotographs,
    ));
    state.summary = Some(summary_of(state.query.clone().unwrap(), 842));
    assert_eq!(filter_bar(&state).count, "842 photographs");
    // A smart collection is not saved from another.
    state.query = Some(view_query(
        &state.catalog,
        ViewSource::Collection {
            collection_id: collection_id("drone00001"),
        },
    ));
    state.summary = Some(summary_of(state.query.clone().unwrap(), 6));
    assert!(filter_bar(&state).save_refused.is_some());
}

/// The batch form: the selection's folders and collections with how many of it each holds, Move
/// to… with the folder they are all in refused, Add to… every plain collection, a collection
/// chip's menu removing them or adding the rest; one photograph's Organize band the same.
#[test]
fn catalog_browse_the_batch_form_counts_the_selection() {
    let mut state = catalog_state(ViewSource::AllPhotographs);
    with_rows(
        &mut state,
        vec![
            photo_row(0, "konstanz01", &["prints0001"], true),
            photo_row(1, "konstanz01", &["prints0001", "landscape1"], true),
            photo_row(2, "brighton01", &[], false),
            photo_row(3, "brighton01", &["prints0001"], true),
            photo_row(4, "sapa000001", &[], true),
            photo_row(5, "sapa000001", &[], true),
        ],
    );
    let selection = selected(0, 5, 1);
    let info = info(&state, &selection).unwrap();
    assert_eq!(info.title, "5 selected");
    assert_eq!(info.active.as_deref(), Some("L1000001.DNG"));
    assert_eq!(info.previews, vec![0, 1, 2, 3, 4]);
    let chips = |chips: &[OrganizeChip]| {
        chips
            .iter()
            .map(|chip| (chip.label.clone(), chip.partial.clone()))
            .collect::<Vec<_>>()
    };
    let mut folders = chips(&info.folders);
    folders.sort();
    assert_eq!(
        folders,
        vec![
            ("Brighton \u{b7} Sep 2026".into(), Some("2 of 5".into())),
            ("Konstanz \u{b7} Sep 2026".into(), Some("2 of 5".into())),
            ("Sa Pa \u{b7} Sep 2026".into(), Some("1 of 5".into())),
        ]
    );
    assert_eq!(
        chips(&info.collections),
        vec![
            ("Print order".into(), Some("3 of 5".into())),
            (
                "Portfolio \u{203a} Landscapes".into(),
                Some("1 of 5".into())
            ),
        ]
    );
    assert_eq!(info.edited.as_deref(), Some("4 of 5"));
    assert_eq!(info.export, "Export 5\u{2026}");
    let metadata: Vec<&str> = info
        .metadata
        .iter()
        .map(|(label, _)| label.as_str())
        .collect();
    assert_eq!(
        metadata,
        ["Captured", "Places", "Cameras", "Kinds", "Originals"]
    );
    assert_eq!(info.metadata[0].1, "4\u{2013}8 Sep 2026");
    assert_eq!(info.metadata[3].1, "4 RAW \u{b7} 1 JPEG");
    // Add to…: the plain collections; Print order's chip removes the three or adds the other two.
    state.catalog.menu = Some(CatalogMenu::AddTo);
    let add = super::info(&state, &selection).unwrap().add_menu.unwrap();
    assert_eq!(
        add.iter()
            .map(|choice| (choice.label.as_str(), choice.action.clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                "Print order",
                Some(CatalogAction::AddPhotos(collection_id("prints0001")))
            ),
            (
                "Portfolio \u{203a} Landscapes",
                Some(CatalogAction::AddPhotos(collection_id("landscape1")))
            ),
        ]
    );
    state.catalog.menu = Some(CatalogMenu::Member(collection_id("prints0001")));
    let chip = super::info(&state, &selection)
        .unwrap()
        .collections
        .into_iter()
        .find(|chip| chip.label == "Print order")
        .unwrap();
    assert_eq!(
        chip.menu
            .unwrap()
            .iter()
            .map(|choice| (choice.label.as_str(), choice.action.clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                "Remove 3 from Print order",
                Some(CatalogAction::RemovePhotos(collection_id("prints0001")))
            ),
            (
                "Add the other 2 to Print order",
                Some(CatalogAction::AddPhotos(collection_id("prints0001")))
            ),
        ]
    );
    // Two photographs in Konstanz: Move to… refuses Konstanz, where both are.
    state.catalog.menu = Some(CatalogMenu::MovePhotos);
    let two = super::info(&state, &selected(0, 2, 0)).unwrap();
    assert_eq!(
        chips(&two.folders),
        vec![("Konstanz \u{b7} Sep 2026".into(), None)]
    );
    let konstanz = two
        .move_menu
        .unwrap()
        .into_iter()
        .find(|choice| choice.label == "Konstanz \u{b7} Sep 2026")
        .unwrap();
    assert!(konstanz.checked && konstanz.action.is_none());
    // One photograph: its folder and collections, its metadata and whether it is edited.
    state.catalog.menu = None;
    let one = super::info(&state, &selected(2, 1, 2)).unwrap();
    assert_eq!(one.title, "L1000002.DNG");
    assert_eq!(
        chips(&one.folders),
        vec![("Brighton \u{b7} Sep 2026".into(), None)]
    );
    assert!(one.collections.is_empty());
    assert_eq!(one.edited.as_deref(), Some("No"));
    assert_eq!(one.previews, vec![2]);
}

/// A selection whose rows are not all near the screen: the ranges the Info panel reads, and past
/// the bound the note instead of the chips.
#[test]
fn catalog_browse_the_batch_form_reads_what_it_does_not_hold() {
    let mut state = catalog_state(ViewSource::AllPhotographs);
    with_rows(
        &mut state,
        (0..6)
            .map(|position| photo_row(position, "konstanz01", &[], false))
            .collect(),
    );
    // The rows held are 0..6 of a larger view.
    state.summary.as_mut().unwrap().count = 5000;
    let selection = selected(4, 10, 4);
    assert_eq!(missing_rows(&state, &selection), vec![(6, 14)]);
    let info = info(&state, &selection).unwrap();
    assert!(info.folders.is_empty());
    assert_eq!(
        info.organize_note.as_deref(),
        Some("Reading the selected photographs\u{2026}")
    );
    // Once read, they are the selection's.
    state.catalog.selection_rows = Some(SelectionRows {
        revision: 7,
        ranges: vec![(6, 14)],
        rows: (6..14)
            .map(|position| (position, photo_row(position, "brighton01", &[], false)))
            .collect(),
    });
    assert!(missing_rows(&state, &selection).is_empty());
    let info = super::info(&state, &selection).unwrap();
    assert_eq!(info.folders.len(), 2);
    // Rows read for another revision are not the selection's.
    state.catalog.selection_rows.as_mut().unwrap().revision = 6;
    assert_eq!(missing_rows(&state, &selection), vec![(6, 14)]);
    // Past the bound nothing is read, and the panel says why the chips are not shown.
    let many = selected(0, MAX_SELECTION_ROWS + 1, 0);
    assert!(missing_rows(&state, &many).is_empty());
    assert!(
        super::info(&state, &many)
            .unwrap()
            .organize_note
            .unwrap()
            .contains("1,000")
    );
    // Over files there is nothing to read.
    state.query = Some(super::super::select::source_query(ViewSource::Folder {
        path: "/p".into(),
        subfolders: true,
    }));
    assert!(missing_rows(&state, &selection).is_empty());
}
