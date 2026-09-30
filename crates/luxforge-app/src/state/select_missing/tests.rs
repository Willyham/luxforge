//! Missing originals' view model: what each row says for each result the owner can answer, the
//! filters, Relink's pairs and every request against what an independent JSON client sends.
use super::*;
use crate::state::select::{SelectState, Shown};
use luxforge_core::catalog_types::{CatalogFolderId, FolderRef, ViewQuery, Volume, VolumeId};

/// Select showing Missing originals, with nothing read yet.
pub(crate) fn showing() -> SelectState {
    SelectState {
        shown: Shown::Select,
        query: Some(ViewQuery::of(ViewSource::MissingOriginals)),
        ..SelectState::default()
    }
}

const LAKE: &str = "/Volumes/Photos SSD/2026/2026-08 Lake";
const WINTER: &str = "/Volumes/Photos SSD/2025/Winter";
const DUMP: &str = "/Users/anna/Pictures/Card dumps/2026-09-10";
const ARCHIVE: &str = "/Volumes/Archive";

fn ssd() -> VolumeId {
    VolumeId::parse("volume-ssd-00000000").unwrap()
}

fn startup() -> VolumeId {
    VolumeId::parse("volume-startup-0000").unwrap()
}

fn folder(name: &str) -> FolderRef {
    FolderRef {
        id: CatalogFolderId::new(),
        name: name.into(),
    }
}

/// Three groups: one on a drive that is not connected in two catalog folders, one on the same drive
/// in one, and one whose folder is gone from the startup disk.
fn list() -> MissingOriginals {
    MissingOriginals {
        groups: vec![
            MissingGroup {
                source_folder: DUMP.into(),
                volume_id: startup(),
                count: 2,
                catalog_folders: vec![folder("Brighton · Sep 2026")],
                reason: MissingReason::FolderGone,
            },
            MissingGroup {
                source_folder: WINTER.into(),
                volume_id: ssd(),
                count: 38,
                catalog_folders: vec![folder("Winter · Jan 2025")],
                reason: MissingReason::VolumeOffline {
                    label: "Photos SSD".into(),
                },
            },
            MissingGroup {
                source_folder: LAKE.into(),
                volume_id: ssd(),
                count: 6,
                catalog_folders: vec![folder("Reichenau · Aug 2026"), folder("Lake walks")],
                reason: MissingReason::VolumeOffline {
                    label: "Photos SSD".into(),
                },
            },
        ],
        count: 46,
    }
}

fn volumes() -> Vec<VolumeState> {
    vec![VolumeState {
        volume: Volume {
            id: startup(),
            mount_point: "/".into(),
            label: "Macintosh HD".into(),
            removable: false,
            platform_id: None,
            last_seen_ms: 1,
        },
        offline: false,
        card: false,
        startup: true,
    }]
}

fn under(path: &str) -> PathBuf {
    Path::new(ARCHIVE).join(path)
}

/// One row of each result, in the order the board draws them, with their photographs.
fn rows() -> (Vec<FindRow>, Vec<AssetId>) {
    let assets: Vec<AssetId> = (0..7).map(|_| AssetId::new()).collect();
    let row = |index: usize, name: &str, result| FindRow {
        asset_id: assets[index].clone(),
        file_name: name.into(),
        result,
    };
    (
        vec![
            row(
                0,
                "DSC_6617.NEF",
                FindResult::Found {
                    path: under("Photographs/2026-08 Lake/DSC_6617.NEF"),
                },
            ),
            row(
                1,
                "DSC_6618.NEF",
                FindResult::Found {
                    path: under("Photographs/2026-08 Lake/DSC_6618.NEF"),
                },
            ),
            row(
                2,
                "DSC_6620.NEF",
                FindResult::DifferentBytes {
                    path: under("Photographs/2026-08 Lake/DSC_6620.NEF"),
                },
            ),
            row(
                3,
                "DSC06736.ARW",
                FindResult::SeveralIdentical {
                    paths: vec![under("a/DSC06736.ARW"), under("b/DSC06736.ARW")],
                },
            ),
            row(4, "P6343.ORF", FindResult::NotFound),
            row(
                5,
                "P6345.ORF",
                FindResult::Claimed {
                    path: under("c/P6345.ORF"),
                    by: AssetId::new(),
                },
            ),
            row(6, "P6344.ORF", FindResult::Checking),
        ],
        assets,
    )
}

/// Missing originals listed, with the lake folder searched in the archive, `status` as it stands.
fn searched(status: SearchStatus) -> (SelectState, Vec<AssetId>) {
    let mut state = showing();
    state.missing.listed(list(), volumes());
    let (rows, assets) = rows();
    state.missing.searches.insert(
        LAKE.into(),
        Search {
            root: ARCHIVE.into(),
            status,
            progress: Some("5 of 7 photographs checked".into()),
            rows,
            chosen: BTreeMap::new(),
        },
    );
    (state, assets)
}

fn lake(model: &MissingModel) -> &GroupModel {
    model
        .groups
        .iter()
        .find(|group| group.folder == Path::new(LAKE))
        .expect("the lake group")
}

fn mutation() -> MutationRequest {
    MutationRequest {
        request_id: "desktop-1".into(),
        actor: "desktop".into(),
    }
}

/// Each request is exactly what an independent JSON client sends for the gesture.
#[test]
fn resolve_missing_requests_are_what_a_json_client_sends() {
    let (_, assets) = rows();
    assert_eq!(
        find_params(Path::new(ARCHIVE), Path::new(LAKE)),
        serde_json::from_str::<Value>(
            r#"{"search_root": "/Volumes/Archive", "source_folder": "/Volumes/Photos SSD/2026/2026-08 Lake"}"#
        )
        .unwrap()
    );
    assert_eq!(
        cancel_params("job-1"),
        serde_json::from_str::<Value>(r#"{"job_id": "job-1"}"#).unwrap()
    );
    let pairs = vec![RelinkPair {
        asset_id: assets[0].clone(),
        path: under("x.NEF"),
    }];
    assert_eq!(
        relink_params(&pairs, &mutation()),
        serde_json::from_str::<Value>(&format!(
            r#"{{"pairs": [{{"asset_id": "{}", "path": "/Volumes/Archive/x.NEF"}}],
                "mutation": {{"request_id": "desktop-1", "actor": "desktop"}}}}"#,
            assets[0]
        ))
        .unwrap()
    );
    assert_eq!(
        locate_params(&assets[1], &under("y.NEF"), &mutation()),
        serde_json::from_str::<Value>(&format!(
            r#"{{"asset_id": "{}", "path": "/Volumes/Archive/y.NEF",
                "mutation": {{"request_id": "desktop-1", "actor": "desktop"}}}}"#,
            assets[1]
        ))
        .unwrap()
    );
    assert_eq!(
        newest_entry_params(&assets[2]),
        serde_json::from_str::<Value>(&format!(
            r#"{{"asset_id": "{}", "before_sequence": null, "limit": 1}}"#,
            assets[2]
        ))
        .unwrap()
    );
}

/// Relink sends exactly what a finished search verified — its found files and the file chosen
/// among several identical ones — and nothing from a search that is still running, stopping or
/// failed, nor a different file, a claimed one, one not found or one still checking.
#[test]
fn resolve_missing_relinks_only_what_a_finished_search_verified() {
    for status in [
        SearchStatus::Starting,
        SearchStatus::Running { job: "j".into() },
        SearchStatus::Stopping { job: "j".into() },
        SearchStatus::Failed("read-error".into()),
    ] {
        let (state, _) = searched(status.clone());
        assert!(relink_pairs(&state.missing).is_empty(), "{status:?}");
    }
    let (mut state, assets) = searched(SearchStatus::Ended);
    let pairs = relink_pairs(&state.missing);
    assert_eq!(
        pairs,
        vec![
            RelinkPair {
                asset_id: assets[0].clone(),
                path: under("Photographs/2026-08 Lake/DSC_6617.NEF"),
            },
            RelinkPair {
                asset_id: assets[1].clone(),
                path: under("Photographs/2026-08 Lake/DSC_6618.NEF"),
            },
        ]
    );
    // A choice among the identical files joins them; a path the search did not answer never does.
    let search = state.missing.searches.get_mut(Path::new(LAKE)).unwrap();
    search
        .chosen
        .insert(assets[3].clone(), under("b/DSC06736.ARW"));
    search
        .chosen
        .insert(assets[4].clone(), under("elsewhere/P6343.ORF"));
    let pairs = relink_pairs(&state.missing);
    assert_eq!(pairs.len(), 3);
    assert_eq!(
        pairs[2],
        RelinkPair {
            asset_id: assets[3].clone(),
            path: under("b/DSC06736.ARW"),
        }
    );
}

/// Every result reads as the board draws it, with its action.
#[test]
fn resolve_missing_rows_say_each_result_with_its_action() {
    let (mut state, assets) = searched(SearchStatus::Ended);
    state.missing.menu = Some(assets[3].clone());
    let model = derive(&state);
    let rows = &lake(&model).rows;
    let said: Vec<(ResultGlyph, &str, Option<&str>)> = rows
        .iter()
        .map(|row| (row.glyph, row.text.as_str(), row.detail.as_deref()))
        .collect();
    assert_eq!(
        said,
        vec![
            (
                ResultGlyph::Found,
                "Found, same bytes",
                Some("\u{2026}/Photographs/2026-08 Lake/")
            ),
            (
                ResultGlyph::Found,
                "Found, same bytes",
                Some("\u{2026}/Photographs/2026-08 Lake/")
            ),
            (
                ResultGlyph::Refused,
                "Different bytes at the same name",
                Some("\u{2026}/Photographs/2026-08 Lake/")
            ),
            (
                ResultGlyph::Several,
                "2 files with the same bytes",
                Some("choose one")
            ),
            (
                ResultGlyph::NotFound,
                "Not found under /Volumes/Archive",
                None
            ),
            (
                ResultGlyph::Refused,
                "Another photograph uses this file",
                Some("\u{2026}/c/")
            ),
            (ResultGlyph::Checking, "Checking\u{2026}", None),
        ]
    );
    let actions: Vec<Option<&RowAction>> = rows.iter().map(|row| row.action.as_ref()).collect();
    assert_eq!(actions[0], None);
    assert_eq!(actions[2], Some(&RowAction::Locate { enabled: true }));
    assert_eq!(
        actions[3],
        Some(&RowAction::Choose {
            open: true,
            choices: vec![
                (
                    "\u{2026}/a/DSC06736.ARW".into(),
                    under("a/DSC06736.ARW"),
                    false
                ),
                (
                    "\u{2026}/b/DSC06736.ARW".into(),
                    under("b/DSC06736.ARW"),
                    false
                ),
            ],
        })
    );
    assert_eq!(actions[4], Some(&RowAction::Locate { enabled: true }));
    assert_eq!(actions[6], None);
    // The group spans two catalog folders, so no row says which it is in.
    assert!(rows.iter().all(|row| row.folder.is_none()));

    // Chosen, the row reads as found there, and its menu checks the choice.
    state.missing.menu = None;
    state
        .missing
        .searches
        .get_mut(Path::new(LAKE))
        .unwrap()
        .chosen
        .insert(assets[3].clone(), under("b/DSC06736.ARW"));
    let model = derive(&state);
    let chosen = &lake(&model).rows[3];
    assert_eq!(
        (chosen.glyph, chosen.text.as_str(), chosen.detail.as_deref()),
        (
            ResultGlyph::Found,
            "Chosen, same bytes",
            Some("\u{2026}/b/")
        )
    );

    // While one photograph is being located, its row says so and no other offers Locate….
    state.missing.locating = Some(Locating {
        asset_id: assets[4].clone(),
        file_name: "P6343.ORF".into(),
        path: "/elsewhere/P6343.ORF".into(),
        from: LocateFrom::Missing,
        job: None,
    });
    let model = derive(&state);
    let rows = &lake(&model).rows;
    assert_eq!(rows[4].text, "Locating\u{2026}");
    assert_eq!(rows[4].action, Some(RowAction::Locate { enabled: false }));
    assert_eq!(rows[2].action, Some(RowAction::Locate { enabled: false }));
}

/// The groups say how many, where they are now and why they are missing; the title bar and the
/// status line say the same of the whole list.
#[test]
fn resolve_missing_groups_say_where_and_why() {
    let mut state = showing();
    state.home = Some("/Users/anna".into());
    state.missing.listed(list(), volumes());
    let model = derive(&state);
    assert!(model.shown);
    assert_eq!(
        model.heading,
        Some((
            "46 photographs whose originals are not where they were".into(),
            "grouped by the folder on disk each was developed from".into()
        ))
    );
    let details: Vec<(&str, &str)> = model
        .groups
        .iter()
        .map(|group| (group.path.as_str(), group.detail.as_str()))
        .collect();
    assert_eq!(
        details,
        vec![
            (
                "~/Pictures/Card dumps/2026-09-10",
                "2 photographs \u{b7} in Brighton \u{b7} Sep 2026 \u{b7} the folder is gone from Macintosh HD"
            ),
            (
                WINTER,
                "38 photographs \u{b7} in Winter \u{b7} Jan 2025 \u{b7} Photos SSD is not connected: connect it, or find the files elsewhere"
            ),
            (
                LAKE,
                "6 photographs \u{b7} in Reichenau \u{b7} Aug 2026 and Lake walks \u{b7} Photos SSD is not connected: connect it, or find the files elsewhere"
            ),
        ]
    );
    assert!(model.groups.iter().all(|group| group.find
        == Some(ActionModel {
            label: "Find in a folder\u{2026}".into(),
            reason: None
        })));
    assert_eq!(
        model.title,
        "46 in the catalog \u{b7} Photos SSD is not connected"
    );
    assert_eq!(model.line, "46 missing \u{b7} 0 found");
    assert_eq!(model.bar, None, "nothing searched, nothing to relink");

    for (reason, text) in [
        (
            MissingReason::FilesGone,
            "the files are gone from the folder",
        ),
        (MissingReason::Changed, "other files are at their paths now"),
        (MissingReason::FolderGone, "the folder is gone"),
    ] {
        let group = MissingGroup {
            reason,
            ..list().groups[1].clone()
        };
        assert_eq!(reason_text(&group, &volumes()), text);
    }

    // Select's own title bar and status line say Missing originals'.
    let select = crate::state::select::model(&state, &Default::default(), "", Some(0), false);
    assert_eq!(select.title.name, "Missing originals");
    assert_eq!(select.title.summary, model.title);
    assert_eq!(select.status.line, model.line);
}

/// While a search runs its group says how far it has got and offers no second search, every other
/// group's Find is refused, the bar offers Stop search, and Relink waits for what the search
/// verifies.
#[test]
fn resolve_missing_one_search_runs_at_a_time() {
    let (state, _) = searched(SearchStatus::Running { job: "j".into() });
    let model = derive(&state);
    let group = lake(&model);
    assert_eq!(
        group.status.as_deref(),
        Some("Searching /Volumes/Archive \u{b7} 5 of 7 photographs checked")
    );
    assert_eq!(group.find, None);
    for group in model
        .groups
        .iter()
        .filter(|group| group.folder != Path::new(LAKE))
    {
        assert_eq!(
            group.find.as_ref().and_then(|find| find.reason.as_deref()),
            Some("One search at a time: stop this one first")
        );
    }
    let bar = model.bar.expect("the bar");
    assert_eq!(bar.verified, "2");
    assert_eq!(
        bar.detail.as_deref(),
        Some(
            "\u{b7} 1 different \u{b7} 1 to choose \u{b7} 1 claimed \u{b7} 1 not found \u{b7} 1 checking"
        )
    );
    assert!(bar.stop);
    assert_eq!((bar.relink.as_str(), bar.pairs), ("Relink 0", 0));
    assert_eq!(
        bar.reason.as_deref(),
        Some("What the search verifies can be relinked once it ends")
    );

    let (state, _) = searched(SearchStatus::Ended);
    let model = derive(&state);
    let bar = model.bar.clone().expect("the bar");
    assert!(!bar.stop);
    assert_eq!(
        (bar.relink.as_str(), bar.pairs, bar.reason),
        ("Relink 2", 2, None)
    );
    assert_eq!(
        lake(&model).find.as_ref().map(|find| find.label.as_str()),
        Some("Find again\u{2026}")
    );
    assert_eq!(model.line, "46 missing \u{b7} 2 found");
}

/// The filters narrow the rows to Found, Needs you or Not found, with their counts; a group with
/// no row under the filter is left out.
#[test]
fn resolve_missing_filters_narrow_the_rows() {
    let (mut state, _) = searched(SearchStatus::Ended);
    let model = derive(&state);
    assert_eq!(
        model.filters,
        vec![
            ("All".to_owned(), None),
            ("Found".to_owned(), Some("2".to_owned())),
            ("Needs you".to_owned(), Some("3".to_owned())),
            ("Not found".to_owned(), Some("1".to_owned())),
        ]
    );
    assert_eq!(model.groups.len(), 3);
    for (filter, names) in [
        (MissingFilter::Found, vec!["DSC_6617.NEF", "DSC_6618.NEF"]),
        (
            MissingFilter::NeedsYou,
            vec!["DSC_6620.NEF", "DSC06736.ARW", "P6345.ORF"],
        ),
        (MissingFilter::NotFound, vec!["P6343.ORF"]),
    ] {
        state.missing.filter = filter;
        let model = derive(&state);
        assert_eq!(model.groups.len(), 1, "{filter:?}");
        let shown: Vec<&str> = model.groups[0]
            .rows
            .iter()
            .map(|row| row.name.as_str())
            .collect();
        assert_eq!(shown, names, "{filter:?}");
        assert_eq!(
            model.filter,
            MissingFilter::ALL
                .iter()
                .position(|each| *each == filter)
                .unwrap()
        );
    }
}

/// The Info panel describes the selected row: where its original was, where it was found, what
/// the check said, its catalog folder and its edits, kept.
#[test]
fn resolve_missing_info_describes_the_selected_row() {
    let (mut state, assets) = searched(SearchStatus::Ended);
    assert_eq!(derive(&state).info, MissingInfo::Nothing);
    state.missing.selected = Some(assets[0].clone());
    let MissingInfo::One(info) = derive(&state).info else {
        panic!("the Info panel describes the row");
    };
    assert_eq!(
        info.rows,
        vec![
            ("Was".into(), format!("{LAKE}/DSC_6617.NEF")),
            (
                "Found".into(),
                "/Volumes/Archive/Photographs/2026-08 Lake/DSC_6617.NEF".into()
            ),
            ("Check".into(), "Same size and fingerprint".into()),
            (
                "Folder".into(),
                "One of Reichenau \u{b7} Aug 2026 and Lake walks".into()
            ),
            ("Edits".into(), "Reading\u{2026}".into()),
        ]
    );
    assert!(info.verified);
    assert_eq!(info.locate.reason, None);
    state.missing.facts = Some((
        assets[0].clone(),
        Ok(PhotoFacts {
            was: "/Volumes/Photos SSD/2026/2026-08 Lake/DSC_6617.NEF".into(),
            entries: 4,
        }),
    ));
    let MissingInfo::One(info) = derive(&state).info else {
        panic!("the Info panel describes the row");
    };
    assert_eq!(
        info.rows[4],
        ("Edits".into(), "4 edits \u{b7} kept as they are".into())
    );

    state.missing.selected = Some(assets[2].clone());
    let MissingInfo::One(info) = derive(&state).info else {
        panic!("the Info panel describes the row");
    };
    assert_eq!(info.rows[2].1, "Same name, different bytes");
    assert!(!info.verified);
}

/// A relink or a Locate forgets its photographs' rows, and a list without a group forgets its
/// search; the selection and an open menu go with their rows.
#[test]
fn resolve_missing_forgets_what_was_resolved() {
    let (mut state, assets) = searched(SearchStatus::Ended);
    state.missing.selected = Some(assets[0].clone());
    state.missing.menu = Some(assets[3].clone());
    state
        .missing
        .resolved(&[assets[0].clone(), assets[3].clone()]);
    let search = &state.missing.searches[Path::new(LAKE)];
    assert_eq!(search.rows.len(), 5);
    assert_eq!(state.missing.selected, None);
    assert_eq!(state.missing.menu, None);

    let mut shorter = list();
    shorter
        .groups
        .retain(|group| group.source_folder != Path::new(LAKE));
    shorter.count = 40;
    state.missing.listed(shorter, volumes());
    assert!(state.missing.searches.is_empty());
}

/// Nothing is in flight or wanted only once the list for the evaluation on screen and the selected
/// row's facts have answered, and no search or Locate runs.
#[test]
fn resolve_missing_is_quiet_only_with_nothing_in_flight_or_wanted() {
    let (mut state, assets) = searched(SearchStatus::Ended);
    let missing = &mut state.missing;
    assert!(
        !missing.quiet(true, 3),
        "the list was not read for this evaluation"
    );
    missing.read_for = Some(3);
    assert!(missing.quiet(true, 3));
    missing.selected = Some(assets[0].clone());
    assert!(
        !missing.quiet(true, 3),
        "the selected row's facts are wanted"
    );
    missing.facts = Some((assets[0].clone(), Err("gone".into())));
    assert!(missing.quiet(true, 3));
    missing.searches.get_mut(Path::new(LAKE)).unwrap().status =
        SearchStatus::Running { job: "j".into() };
    assert!(!missing.quiet(true, 3));
    assert!(
        !missing.quiet(false, 3),
        "a running search is in flight wherever it is shown"
    );
}

/// Hidden, nothing is derived; shown before the list answers, the centre says it is reading.
#[test]
fn resolve_missing_derives_nothing_unless_shown() {
    let mut state = showing();
    state.shown = Shown::Develop;
    assert_eq!(derive(&state), MissingModel::default());
    state.shown = Shown::Select;
    state.query = Some(ViewQuery::of(ViewSource::AllPhotographs));
    assert!(!derive(&state).shown);
    let model = derive(&showing());
    assert!(model.shown);
    assert_eq!(model.note.as_deref(), Some("Reading\u{2026}"));
    let mut failed = showing();
    failed.missing.error = Some("catalog: busy".into());
    assert_eq!(
        derive(&failed).note.as_deref(),
        Some("Missing originals unavailable: catalog: busy")
    );
}

/// A group draws at most its first rows under the filter and says how many more there are, while
/// Relink still sends every verified pair.
#[test]
fn resolve_missing_draws_a_bounded_window_of_rows() {
    let (mut state, _) = searched(SearchStatus::Ended);
    let search = state.missing.searches.get_mut(Path::new(LAKE)).unwrap();
    search.rows = (0..MAX_GROUP_ROWS + 250)
        .map(|index| FindRow {
            asset_id: AssetId::new(),
            file_name: format!("DSC_{index:05}.NEF"),
            result: FindResult::Found {
                path: under(&format!("{index}.NEF")),
            },
        })
        .collect();
    let model = derive(&state);
    let group = lake(&model);
    assert_eq!(group.rows.len(), MAX_GROUP_ROWS);
    assert_eq!(
        group.more.as_deref(),
        Some("250 more: narrow them with the filters")
    );
    assert_eq!(relink_pairs(&state.missing).len(), MAX_GROUP_ROWS + 250);
    assert_eq!(model.bar.unwrap().pairs, MAX_GROUP_ROWS + 250);
}
