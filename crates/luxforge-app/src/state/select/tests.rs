//! The Select view model against synthetic owner answers: the group layout's blocks, the sources
//! panel, the query each filter and sort makes, the rows window and its bound, the selection and
//! the `browse.select` request each gesture sends, the Info panel and the status line.
use super::*;
use luxforge_core::{
    AssetId,
    catalog_types::{
        BrowseSession, CameraGroup, DayGroup, Dimensions, EventId, ExifOrientation, FacetValue,
        FileId, GroupLayout, LibraryChangeSeq, MomentRef, MonthCount, PositionRange, Thresholds,
        ViewFilter, ViewSelection, ViewSort,
    },
};
use std::collections::BTreeMap;

fn day(year: i32, month: u32, date: u32) -> LocalDay {
    LocalDay::from_ymd(year, month, date).unwrap()
}

fn event_source() -> ViewSource {
    ViewSource::Event {
        event_id: EventId::parse(format!("event-{}", "a".repeat(32))).unwrap(),
    }
}

fn summary(count: u32, groups: GroupLayout) -> ViewSummary {
    ViewSummary {
        revision: 7,
        query: ViewQuery::of(event_source()),
        count,
        picked: 3,
        in_catalog: 0,
        unavailable: 0,
        groups,
        library_sequence: LibraryChangeSeq(1),
        index_revision: 1,
    }
}

fn burst(start: u32, len: u32, span_ms: u64) -> Moment {
    Moment {
        kind: MomentKind::Burst,
        evidence: None,
        steps_ev: Vec::new(),
        span_ms,
        start,
        len,
    }
}

fn bracket(start: u32, steps: Vec<f32>, evidence: BracketEvidence) -> Moment {
    Moment {
        kind: MomentKind::Bracket,
        evidence: Some(evidence),
        len: steps.len() as u32,
        steps_ev: steps,
        span_ms: 600,
        start,
    }
}

/// Twenty frames over two days: the first with two cameras, a burst under the first and a bracket
/// from the metadata under the second; the second day with one camera and a burst.
fn trip() -> ViewSummary {
    summary(
        20,
        GroupLayout {
            days: vec![
                DayGroup {
                    day: Some(day(2026, 9, 12)),
                    start: 0,
                    len: 12,
                },
                DayGroup {
                    day: Some(day(2026, 9, 13)),
                    start: 12,
                    len: 8,
                },
            ],
            cameras: vec![
                CameraGroup {
                    body: BodyKey("LEICA|Q2|1".into()),
                    label: "Leica Q2".into(),
                    start: 0,
                    len: 7,
                },
                CameraGroup {
                    body: BodyKey("NIKON|Z 8|2".into()),
                    label: "Nikon Z 8".into(),
                    start: 7,
                    len: 5,
                },
            ],
            moments: vec![
                burst(1, 3, 1400),
                bracket(7, vec![-2.0, 0.0, 2.0], BracketEvidence::Metadata),
                burst(14, 2, 300),
            ],
        },
    )
}

fn moment(
    bracket: bool,
    title: &str,
    detail: &str,
    evidence: Option<&str>,
    frames: u32,
    collapsed: bool,
) -> Block {
    Block::Moment {
        bracket,
        title: title.into(),
        detail: detail.into(),
        evidence: evidence.map(Into::into),
        frames,
        collapsed,
    }
}

fn heading(day: bool, title: &str, detail: &str) -> Block {
    if day {
        Block::Day {
            title: title.into(),
            detail: detail.into(),
        }
    } else {
        Block::Camera {
            title: title.into(),
            detail: detail.into(),
        }
    }
}

/// Days, cameras, a burst, a bracket with its steps and evidence, singles between them, and a
/// collapsed burst: exactly the group layout the owner answered, every item covered once.
#[test]
fn a_select_group_layout_becomes_days_cameras_moments_and_singles() {
    let trip = trip();
    let content = grid_content(&trip, &BTreeSet::from([2]));
    assert_eq!(
        content.blocks,
        vec![
            heading(true, "Saturday 12 September 2026", "12 photographs"),
            heading(false, "Leica Q2", "7"),
            Block::Singles(1),
            moment(false, "Burst", "3 frames in 1.4 s", None, 3, false),
            Block::Singles(3),
            heading(false, "Nikon Z 8", "5"),
            moment(
                true,
                "Bracket",
                "3 exposures \u{b7} \u{2212}2 \u{b7} 0 \u{b7} +2 EV",
                Some("from metadata"),
                3,
                false
            ),
            Block::Singles(2),
            heading(true, "Sunday 13 September 2026", "8 photographs"),
            Block::Singles(2),
            moment(false, "Burst", "2 frames in 0.3 s", None, 2, true),
            Block::Singles(4),
        ]
    );
    assert_eq!(content.items(), trip.count);
    // The bracket's frames carry their steps as footers; nothing else has one.
    assert_eq!(content.label(7), Some("\u{2212}2 EV"));
    assert_eq!(content.label(8), Some("0 EV"));
    assert_eq!(content.label(9), Some("+2 EV"));
    assert_eq!(content.label(1), None);
    // Collapsing names a burst by its index; a bracket never collapses.
    let bracket_named = grid_content(&trip, &BTreeSet::from([1]));
    assert!(bracket_named.blocks.iter().all(|block| !matches!(
        block,
        Block::Moment {
            collapsed: true,
            ..
        }
    )));
}

/// A view grouped by day alone has headings and singles; one grouped by nothing, or sorted by
/// anything but capture time, is one run of singles; a bracket measured on the previews says so.
#[test]
fn select_day_and_no_grouping_follow_the_layout_they_are_given() {
    let days = summary(
        5,
        GroupLayout {
            days: vec![
                DayGroup {
                    day: Some(day(2026, 9, 12)),
                    start: 0,
                    len: 3,
                },
                DayGroup {
                    day: None,
                    start: 3,
                    len: 2,
                },
            ],
            ..GroupLayout::default()
        },
    );
    assert_eq!(
        grid_content(&days, &BTreeSet::new()).blocks,
        vec![
            heading(true, "Saturday 12 September 2026", "3 photographs"),
            Block::Singles(3),
            heading(true, "Undated", "2 photographs"),
            Block::Singles(2),
        ]
    );
    let flat = summary(1000, GroupLayout::default());
    assert_eq!(
        grid_content(&flat, &BTreeSet::new()).blocks,
        vec![Block::Singles(1000)]
    );
    let previews = summary(
        4,
        GroupLayout {
            moments: vec![bracket(1, vec![-0.7, 0.0, 0.66], BracketEvidence::Previews)],
            ..GroupLayout::default()
        },
    );
    let content = grid_content(&previews, &BTreeSet::new());
    assert_eq!(
        content.blocks,
        vec![
            Block::Singles(1),
            moment(
                true,
                "Bracket",
                "3 exposures \u{b7} \u{2212}0.7 \u{b7} 0 \u{b7} +0.7 EV",
                Some("from previews"),
                3,
                false
            ),
        ]
    );
    // An empty view has no blocks; a layout reaching past the count is cut at it.
    assert!(
        grid_content(&summary(0, GroupLayout::default()), &BTreeSet::new())
            .blocks
            .is_empty()
    );
    let cut = summary(
        3,
        GroupLayout {
            moments: vec![burst(2, 5, 900)],
            ..GroupLayout::default()
        },
    );
    assert_eq!(grid_content(&cut, &BTreeSet::new()).items(), 3);
}

#[test]
fn select_words_for_days_spans_and_steps() {
    assert_eq!(
        day_title(Some(day(2026, 9, 12))),
        "Saturday 12 September 2026"
    );
    assert_eq!(day_title(Some(day(1970, 1, 1))), "Thursday 1 January 1970");
    assert_eq!(
        day_title(Some(day(2024, 2, 29))),
        "Thursday 29 February 2024"
    );
    assert_eq!(
        dates(Some(day(2026, 9, 14)), None, false).unwrap(),
        "14 Sep"
    );
    assert_eq!(
        dates(Some(day(2026, 9, 12)), Some(day(2026, 9, 13)), false).unwrap(),
        "12\u{2013}13 Sep"
    );
    assert_eq!(
        dates(Some(day(2026, 9, 12)), Some(day(2026, 9, 13)), true).unwrap(),
        "12\u{2013}13 Sep 2026"
    );
    assert_eq!(
        dates(Some(day(2026, 9, 30)), Some(day(2026, 10, 2)), false).unwrap(),
        "30 Sep \u{2013} 2 Oct"
    );
    assert_eq!(
        dates(Some(day(2025, 12, 30)), Some(day(2026, 1, 2)), false).unwrap(),
        "30 Dec 2025 \u{2013} 2 Jan 2026"
    );
    assert_eq!(dates(None, None, true), None);
    assert_eq!(span_text(1400), "1.4 s");
    assert_eq!(span_text(300), "0.3 s");
    assert_eq!(span_text(12_400), "12 s");
    assert_eq!(span_text(65_000), "1 min 5 s");
    assert_eq!(ev_step(-2.0), "\u{2212}2");
    assert_eq!(ev_step(0.02), "0");
    assert_eq!(ev_step(1.0 / 3.0), "+0.3");
    assert_eq!(ev_step(f32::NAN), "0");
    assert_eq!(thousands(1042), "1,042");
    assert_eq!(thousands(1_000_000), "1,000,000");
    assert_eq!(photographs(1), "1 photograph");
    assert_eq!(
        exposure_text(&Exposure {
            time_s: Some(1.0 / 2000.0),
            f_number: Some(5.6),
            iso: Some(100),
            bias_ev: Some(0.7),
            focal_mm: Some(28.0),
            focal_35mm_mm: None,
        })
        .unwrap(),
        "1/2000 s \u{b7} f/5.6 \u{b7} ISO 100 \u{b7} +0.7 EV \u{b7} 28 mm"
    );
    assert_eq!(exposure_text(&Exposure::default()), None);
    assert_eq!(
        shown_path(
            Path::new("/Users/w/Pictures/Dumps"),
            Some(Path::new("/Users/w"))
        ),
        "~/Pictures/Dumps"
    );
    assert_eq!(
        shown_path(Path::new("/Volumes/CARD"), Some(Path::new("/Users/w"))),
        "/Volumes/CARD"
    );
}

fn listed(name: &str, months: Vec<Month>, count: u32, picked: u32, offline: u32) -> Event {
    Event {
        id: EventId::new(),
        name: name.into(),
        place: None,
        first_day: months.first().map(|month| day(month.year, month.month, 12)),
        last_day: months.last().map(|month| day(month.year, month.month, 13)),
        undated: months.is_empty(),
        months,
        cameras: vec!["Leica Q2".into(), "Nikon Z 8".into()],
        count,
        picked,
        offline,
        roots: vec!["/Users/w/Pictures".into(), "/Volumes/NIKON Z 8".into()],
        volumes: Vec::new(),
    }
}

fn month(year: i32, month: u32) -> Month {
    Month { year, month }
}

fn month_count(at: Month) -> MonthCount {
    MonthCount {
        month: at,
        events: 1,
        files: 1,
        picked: 0,
    }
}

/// Events by month, newest first, with picks over the total, a hollow dot when files are offline,
/// an event spanning two months under both, the Undated at the end, and the chosen event selected.
#[test]
fn the_select_sources_panel_lists_events_by_month() {
    let konstanz = listed("Konstanz", vec![month(2026, 9)], 1042, 18, 0);
    let reichenau = listed(
        "Reichenau",
        vec![month(2026, 8), month(2026, 9)],
        212,
        0,
        212,
    );
    let undated = listed("Undated", Vec::new(), 4, 0, 0);
    let state = SelectState {
        shown: Shown::Select,
        events: Some(EventList {
            events: vec![konstanz.clone(), reichenau.clone(), undated],
            months: vec![month_count(month(2026, 9)), month_count(month(2026, 8))],
        }),
        query: Some(ViewQuery::of(ViewSource::Event {
            event_id: konstanz.id.clone(),
        })),
        ..SelectState::default()
    };
    let model = sources(&state);
    let names = |rows: &[SourceRow]| rows.iter().map(|row| row.name.clone()).collect::<Vec<_>>();
    assert_eq!(
        model
            .months
            .iter()
            .map(|month| (month.label.clone(), names(&month.rows)))
            .collect::<Vec<_>>(),
        vec![
            (
                "September 2026".into(),
                vec!["Konstanz".to_owned(), "Reichenau".into()]
            ),
            ("August 2026".into(), vec!["Reichenau".to_owned()]),
            ("Undated".into(), vec!["Undated".to_owned()]),
        ]
    );
    let first = &model.months[0].rows[0];
    assert!(first.selected && !first.offline);
    assert_eq!(first.secondary.as_deref(), Some("12\u{2013}13 Sep"));
    assert_eq!(
        first.count,
        Count::Picks {
            picked: "18".into(),
            total: "1,042".into()
        }
    );
    assert_eq!(
        first.press,
        SourcePress::View(ViewSource::Event {
            event_id: konstanz.id.clone()
        })
    );
    let second = &model.months[0].rows[1];
    assert!(second.offline && !second.selected);
    assert_eq!(second.count, Count::Total("212".into()));
    assert_eq!(model.events_note, None);
    // On disk offers the folder dialog; Catalog its four views, Removed dimmed, counts blank.
    assert_eq!(
        names(&model.on_disk),
        vec!["Browse a folder\u{2026}".to_owned()]
    );
    assert_eq!(model.on_disk[0].press, SourcePress::BrowseFolder);
    assert_eq!(
        names(&model.catalog),
        vec![
            "All photographs".to_owned(),
            "Recently developed".into(),
            "Missing originals".into(),
            "Removed".into()
        ]
    );
    assert!(model.catalog.iter().all(|row| row.count == Count::None));
    assert_eq!(
        model
            .catalog
            .iter()
            .map(|row| row.dimmed)
            .collect::<Vec<_>>(),
        vec![false, false, false, true]
    );
    // A folder browsed on disk is listed and selected while it is viewed.
    let folder = SelectState {
        folder: Some("/Users/w/Pictures/Dumps".into()),
        query: Some(ViewQuery::of(ViewSource::Folder {
            path: "/Users/w/Pictures/Dumps".into(),
            subfolders: true,
        })),
        ..state.clone()
    };
    let model = sources(&folder);
    assert_eq!(model.on_disk[1].name, "Dumps");
    assert!(model.on_disk[1].selected);
    assert!(
        model
            .months
            .iter()
            .all(|month| month.rows.iter().all(|row| !row.selected))
    );
    // Before the list arrives, when it fails and when nothing matches, the panel says so.
    let note = |state: SelectState| sources(&state).events_note;
    assert_eq!(
        note(SelectState::default()).as_deref(),
        Some("Reading events\u{2026}")
    );
    assert_eq!(
        note(SelectState {
            events_error: Some("validation: unknown method event.list".into()),
            ..SelectState::default()
        })
        .as_deref(),
        Some("Events unavailable")
    );
    assert_eq!(
        note(SelectState {
            events: Some(EventList::default()),
            search: "Konst".into(),
            ..SelectState::default()
        })
        .as_deref(),
        Some("No events match")
    );
    assert_eq!(events_params("  Konst "), json!({"query": "Konst"}));
    assert_eq!(events_params(""), json!({}));
}

/// A base query with a condition of every kind set, so a change that touched another part would
/// show.
fn base() -> ViewQuery {
    ViewQuery {
        source: event_source(),
        filter: ViewFilter {
            cameras: vec![BodyKey("LEICA|Q2|1".into())],
            kinds: vec![SourceTag::Raw],
            picked: Some(true),
            ..ViewFilter::default()
        },
        sort: ViewSort::default(),
        grouping: Grouping::Day,
        thresholds: Thresholds::default(),
    }
}

/// Every chip, segment, grouping and sort changes its own part of the query and nothing else.
#[test]
fn every_select_filter_and_sort_changes_its_own_part_of_the_query() {
    let base = base();
    let expect = |change: fn(&mut ViewQuery)| {
        let mut query = base.clone();
        change(&mut query);
        query
    };
    let cases: Vec<(QueryChange, ViewQuery)> = vec![
        (
            QueryChange::Pick(PickFilter::All),
            expect(|query| query.filter.picked = None),
        ),
        (QueryChange::Pick(PickFilter::Picked), base.clone()),
        (
            QueryChange::Pick(PickFilter::WithoutPick),
            expect(|query| {
                query.filter.picked = None;
                query.filter.without_pick = true;
            }),
        ),
        (
            QueryChange::Camera(None),
            expect(|query| query.filter.cameras.clear()),
        ),
        (
            QueryChange::Camera(Some(BodyKey("NIKON|Z 8|2".into()))),
            expect(|query| query.filter.cameras = vec![BodyKey("NIKON|Z 8|2".into())]),
        ),
        (
            QueryChange::Kind(None),
            expect(|query| query.filter.kinds.clear()),
        ),
        (
            QueryChange::Kind(Some(SourceTag::Jpeg)),
            expect(|query| query.filter.kinds = vec![SourceTag::Jpeg]),
        ),
        (
            QueryChange::Group(Grouping::DayCameraMoment),
            expect(|query| query.grouping = Grouping::DayCameraMoment),
        ),
        (
            QueryChange::Group(Grouping::None),
            expect(|query| query.grouping = Grouping::None),
        ),
        (
            QueryChange::Sort(SortKey::FileName),
            expect(|query| query.sort.key = SortKey::FileName),
        ),
    ];
    for (change, expected) in cases {
        let query = changed(&base, &change);
        assert_eq!(query, expected, "{change:?}");
        query.validate().unwrap();
        // What is sent is the whole query as fields, and nothing else.
        let params = view_params(&query);
        assert_eq!(
            params.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["filter", "grouping", "sort", "source", "thresholds"]
        );
        assert_eq!(serde_json::from_value::<ViewQuery>(params).unwrap(), query);
    }
    // Over the catalog, the photographs are newest first and ungrouped, and a sort keeps newest
    // first except by name.
    let catalog = source_query(ViewSource::AllPhotographs);
    assert_eq!(
        catalog.sort,
        ViewSort {
            key: SortKey::CaptureTime,
            descending: true
        }
    );
    assert_eq!(catalog.grouping, Grouping::None);
    for key in sorts(false) {
        let query = changed(&catalog, &QueryChange::Sort(*key));
        assert_eq!(query.sort.descending, *key != SortKey::FileName);
        assert_eq!(query.filter, catalog.filter);
        query.validate().unwrap();
    }
    for key in sorts(true) {
        changed(&ViewQuery::of(event_source()), &QueryChange::Sort(*key))
            .validate()
            .unwrap();
    }
    assert_eq!(source_query(event_source()), ViewQuery::of(event_source()));
    assert_eq!(pick_filter(&base), PickFilter::Picked);
    // The facets asked for: the chips' and, over files, the pick counts.
    assert_eq!(
        facets_params(&base),
        json!({"source": base.source, "filter": base.filter, "facets": ["camera", "kind", "pick"]})
    );
    assert_eq!(facets_params(&catalog)["facets"], json!(["camera", "kind"]));
}

fn facet(value: &str, label: Option<&str>, count: u32) -> FacetValue {
    FacetValue {
        value: Some(value.into()),
        label: label.map(Into::into),
        count,
    }
}

/// The filter bar shows the query's conditions, the pick count from the Pick facet, and menus from
/// the facets, each item the change it makes.
#[test]
fn the_select_filter_bar_reads_its_query_and_facets() {
    let mut state = SelectState {
        shown: Shown::Select,
        query: Some(base()),
        summary: Some(summary(18, GroupLayout::default())),
        facets: Some(Facets {
            counts: BTreeMap::from([
                (
                    Facet::Camera,
                    vec![
                        facet("LEICA|Q2|1", Some("Leica Q2"), 618),
                        facet("NIKON|Z 8|2", Some("Nikon Z 8"), 424),
                    ],
                ),
                (
                    Facet::Kind,
                    vec![facet("raw", None, 1000), facet("jpeg", None, 42)],
                ),
                (
                    Facet::Pick,
                    vec![facet("picked", None, 18), facet("not-picked", None, 1024)],
                ),
            ]),
        }),
        menu: Some(SelectMenu::Camera),
        ..SelectState::default()
    };
    state.summary.as_mut().unwrap().query = base();
    let bar = filter_bar(&state);
    assert!(bar.enabled);
    assert_eq!(
        bar.pick,
        Some(PickSegments {
            selected: PickFilter::Picked,
            picked: Some("18".into())
        })
    );
    assert_eq!(bar.camera.label, "Leica Q2");
    assert!(bar.camera.set);
    let menu = bar.camera.menu.unwrap();
    assert_eq!(
        menu.iter()
            .map(|choice| (
                choice.label.as_str(),
                choice.trailing.as_deref(),
                choice.checked
            ))
            .collect::<Vec<_>>(),
        vec![
            ("All cameras", None, false),
            ("Leica Q2", Some("618"), true),
            ("Nikon Z 8", Some("424"), false),
        ]
    );
    assert_eq!(
        menu[2].change,
        Some(QueryChange::Camera(Some(BodyKey("NIKON|Z 8|2".into()))))
    );
    assert_eq!(bar.kind.label, "RAW");
    assert!(bar.kind.menu.is_none());
    assert_eq!(bar.group.as_ref().unwrap().label, "Group: Day");
    assert_eq!(bar.count, "18 photographs");
    // The kind menu counts from the facets; the group menu is the three groupings.
    state.menu = Some(SelectMenu::Kind);
    let kinds = filter_bar(&state).kind.menu.unwrap();
    assert_eq!(kinds[1].trailing.as_deref(), Some("1,000"));
    assert!(kinds[1].checked);
    state.menu = Some(SelectMenu::Group);
    let groups = filter_bar(&state).group.unwrap().menu.unwrap();
    assert_eq!(
        groups
            .iter()
            .map(|choice| choice.change.clone())
            .collect::<Vec<_>>(),
        Grouping::ALL
            .map(|grouping| Some(QueryChange::Group(grouping)))
            .to_vec()
    );
    // Without facets the camera menu says it cannot list them.
    state.facets = None;
    state.menu = Some(SelectMenu::Camera);
    let menu = filter_bar(&state).camera.menu.unwrap();
    assert_eq!(menu.last().unwrap().change, None);
    // Over the catalog there are no pick segments and no Group chip, and the strip's sorts are the
    // catalog's.
    state.query = Some(source_query(ViewSource::AllPhotographs));
    state.menu = Some(SelectMenu::Sort);
    let bar = filter_bar(&state);
    assert!(bar.pick.is_none() && bar.group.is_none());
    let strip = strip(&state);
    assert_eq!(strip.sort, "Capture time");
    assert_eq!(strip.sort_menu.unwrap().len(), 4);
    assert_eq!(strip.cell_width, CATALOG_CELL_WIDTH);
}

fn row(position: u32) -> ViewRow {
    ViewRow {
        position,
        item: RowItem::File {
            file_id: FileId(i64::from(position)),
        },
        path: format!("/Users/w/Pictures/Dumps/DSC_{position:04}.NEF").into(),
        file_name: format!("DSC_{position:04}.NEF"),
        kind: SourceTag::Raw,
        dimensions: Some(Dimensions {
            width: 6048,
            height: 4024,
        }),
        orientation: None,
        capture: Some("2026-09-12T16:05:12.41+02:00".into()),
        place: Some("Konstanz".into()),
        camera: Some("Nikon Z 8".into()),
        lens: None,
        exposure: Exposure {
            time_s: Some(0.002),
            f_number: Some(8.0),
            iso: Some(64),
            ..Exposure::default()
        },
        moment: None,
        picked: false,
        developed_as: None,
        edited: false,
        availability: FileAvailability::Available,
        preview: PreviewState::Pending,
    }
}

fn block(from: u32, len: u32) -> Vec<ViewRow> {
    (from..from + len).map(row).collect()
}

/// A 10,000-item view reads only the blocks near what is shown, one request at a time, keeps at
/// most its bound, drops the furthest first, ignores rows of an older revision, and does not ask
/// again for a block the owner refused.
#[test]
fn the_select_rows_window_reads_near_the_screen_within_its_bound() {
    assert_eq!(wanted_blocks(0..0, 100), 0..0);
    assert_eq!(wanted_blocks(4000..4150, 10_000), 20..21);
    assert_eq!(wanted_blocks(4150..4250, 10_000), 20..22);
    assert_eq!(wanted_blocks(9_990..10_400, 10_000), 49..50);
    assert_eq!(wanted_blocks(0..100_000, 10_000).len(), ROW_BLOCKS_KEPT);

    let mut rows = RowCache::default();
    rows.reset(3, 10_000);
    let request = rows.next_request(4000..4150).unwrap();
    assert_eq!(
        request,
        RowsRequest {
            revision: 3,
            from: 4000,
            count: ROW_BLOCK
        }
    );
    assert_eq!(
        rows_params(&request),
        json!({"from": 4000, "count": 200, "revision": 3})
    );
    // One in flight at a time.
    assert_eq!(rows.next_request(4000..4150), None);
    // An older revision's rows are no answer.
    assert!(!rows.answered(2, 4000, block(4000, 200), 4000..4150));
    assert_eq!(rows.in_flight(), Some(20));
    assert!(rows.answered(3, 4000, block(4000, 200), 4000..4150));
    assert_eq!(rows.row(4123).unwrap().position, 4123);
    assert!(rows.row(3999).is_none());
    assert_eq!(rows.next_request(4000..4150), None, "the block is held");
    assert_eq!(rows.len(), 200);
    // The last block is short.
    let last = rows.next_request(9_990..10_000).unwrap();
    assert_eq!((last.from, last.count), (9_800, 200));
    rows.failed(3, 9_800);
    assert_eq!(
        rows.next_request(9_990..10_000),
        None,
        "a refused block is not asked again"
    );
    // Scrolling the whole view keeps at most the bound, the blocks nearest the screen.
    for block_index in 0..50u32 {
        let from = block_index * ROW_BLOCK;
        if let Some(request) = rows.next_request(from..from + 10) {
            assert!(rows.answered(
                3,
                request.from,
                block(request.from, request.count),
                from..from + 10
            ));
        }
        assert!(rows.blocks() <= ROW_BLOCKS_KEPT);
    }
    assert!(
        rows.row(48 * ROW_BLOCK).is_some(),
        "the block on screen is kept"
    );
    assert!(rows.row(0).is_none(), "the furthest block went first");
    // A view evaluated again starts over, and refused blocks may be asked for again.
    rows.reset(4, 10_000);
    assert_eq!(rows.len(), 0);
    assert_eq!(rows.next_request(9_990..10_000).unwrap().from, 9_800);
    let mut small = RowCache::default();
    small.reset(1, 30);
    assert_eq!(
        small.next_request(0..30),
        Some(RowsRequest {
            revision: 1,
            from: 0,
            count: 30
        })
    );
}

fn session(revision: u64, ranges: &[(u32, u32)], active: Option<u32>) -> BrowseSession {
    BrowseSession {
        query: None,
        revision,
        count: 20,
        stale: false,
        selection: ViewSelection {
            count: ranges.iter().map(|(_, len)| len).sum(),
            ranges: ranges
                .iter()
                .map(|&(start, len)| PositionRange { start, len })
                .collect(),
            active,
        },
        ..BrowseSession::default()
    }
}

/// The grid draws the session's selection and active item for the revision on screen, and nothing
/// for another.
#[test]
fn the_select_selection_is_the_sessions_for_the_revision_drawn() {
    let browse = session(7, &[(2, 3), (10, 1)], Some(3));
    let selection = SelectionModel::of(&browse, Some(7));
    assert_eq!(selection.ranges, vec![(2, 5), (10, 11)]);
    assert!(!selection.selected(0, 2));
    assert!(
        selection.selected(1, 2),
        "a collapsed cell with one selected frame"
    );
    assert!(selection.selected(4, 1));
    assert!(!selection.selected(5, 5));
    assert!(selection.selected(9, 3));
    assert!(!selection.selected(11, 1));
    assert!(selection.active_in(3, 1) && selection.active_in(0, 4) && !selection.active_in(4, 1));
    assert_eq!(selection.focus(), Some(3));
    assert_eq!(
        SelectionModel::of(&browse, Some(6)),
        SelectionModel::default()
    );
    assert_eq!(SelectionModel::of(&browse, None), SelectionModel::default());
}

/// Each gesture sends exactly the `browse.select` request an independent JSON client would write.
#[test]
fn every_select_gesture_sends_the_request_an_api_client_would() {
    let cases = [
        (
            SelectGesture::Only { item: 4, span: 1 },
            json!({"mode": "replace", "range": {"start": 4, "len": 1}, "active": 4, "revision": 7}),
        ),
        (
            SelectGesture::Only { item: 14, span: 2 },
            json!({"mode": "replace", "range": {"start": 14, "len": 2}, "active": 14, "revision": 7}),
        ),
        (
            SelectGesture::Toggle {
                item: 9,
                span: 1,
                adding: true,
            },
            json!({"mode": "toggle", "range": {"start": 9, "len": 1}, "active": 9, "revision": 7}),
        ),
        (
            SelectGesture::Toggle {
                item: 9,
                span: 1,
                adding: false,
            },
            json!({"mode": "toggle", "range": {"start": 9, "len": 1}, "revision": 7}),
        ),
        (
            SelectGesture::Extend {
                anchor: (12, 1),
                target: (4, 3),
            },
            json!({"mode": "replace", "range": {"start": 4, "len": 9}, "active": 4, "revision": 7}),
        ),
        (
            SelectGesture::Extend {
                anchor: (4, 1),
                target: (14, 2),
            },
            json!({"mode": "replace", "range": {"start": 4, "len": 12}, "active": 14, "revision": 7}),
        ),
        (
            SelectGesture::All,
            json!({"mode": "replace", "all": true, "revision": 7}),
        ),
        (
            SelectGesture::Nothing,
            json!({"mode": "remove", "all": true, "revision": 7}),
        ),
    ];
    for (gesture, expected) in cases {
        assert_eq!(select_params(gesture, Some(7)), expected, "{gesture:?}");
    }
    assert_eq!(
        select_params(SelectGesture::All, None),
        json!({"mode": "replace", "all": true})
    );
}

/// The Info panel: the active frame's Moment band and Metadata from its row, several selected as a
/// count, and what it says before the row is read or with nothing selected.
#[test]
fn the_select_info_panel_describes_the_active_item() {
    let mut state = SelectState {
        shown: Shown::Select,
        query: Some(ViewQuery::of(event_source())),
        summary: Some(trip()),
        home: Some("/Users/w".into()),
        ..SelectState::default()
    };
    state.rows.reset(7, 20);
    let request = state.rows.next_request(0..20).unwrap();
    let mut rows = block(0, 20);
    rows[2].moment = Some(MomentRef { index: 0, frame: 1 });
    rows[2].orientation = ExifOrientation::new(6);
    rows[2].exposure.bias_ev = Some(0.0);
    rows[8].moment = Some(MomentRef { index: 1, frame: 1 });
    rows[5].developed_as = Some(AssetId::new());
    assert!(state.rows.answered(7, request.from, rows, 0..20));

    let one = |state: &SelectState, active: u32| {
        info(
            state,
            &SelectionModel::of(&session(7, &[(active, 1)], Some(active)), Some(7)),
        )
    };
    let InfoModel::One(frame) = one(&state, 2) else {
        panic!("one item");
    };
    assert_eq!(frame.name, "DSC_0002.NEF");
    assert_eq!(frame.aspect, Some(4024.0 / 6048.0), "turned upright");
    assert_eq!(
        frame.moment,
        vec![
            ("Moment".to_owned(), "Burst \u{b7} frame 2 of 3".to_owned()),
            ("Why".into(), "About 0.7 s apart".into()),
            ("Frames".into(), "3 frames in 1.4 s".into()),
        ]
    );
    assert_eq!(
        frame.metadata,
        vec![
            (
                "Captured".to_owned(),
                "2026-09-12T16:05:12.41+02:00".to_owned()
            ),
            ("Place".into(), "Konstanz".into()),
            ("Camera".into(), "Nikon Z 8".into()),
            ("Exposure".into(), "1/500 s \u{b7} f/8 \u{b7} ISO 64".into()),
            ("Dimensions".into(), "4024 \u{d7} 6048 \u{b7} RAW".into()),
            ("File".into(), "DSC_0002.NEF".into()),
            ("Folder".into(), "~/Pictures/Dumps".into()),
        ]
    );
    let InfoModel::One(bracket_frame) = one(&state, 8) else {
        panic!("one item");
    };
    assert_eq!(
        bracket_frame.moment[1].1,
        "Exposure steps, from the metadata"
    );
    let InfoModel::One(single) = one(&state, 5) else {
        panic!("one item");
    };
    assert!(single.moment.is_empty());
    assert_eq!(
        single.metadata.last().unwrap(),
        &("Catalog".to_owned(), "In the catalog".to_owned())
    );
    assert_eq!(
        info(
            &state,
            &SelectionModel::of(&session(7, &[(0, 5)], Some(2)), Some(7))
        ),
        InfoModel::Several {
            count: "5 selected".into(),
            active: Some("DSC_0002.NEF".into())
        }
    );
    assert_eq!(info(&state, &SelectionModel::default()), InfoModel::Nothing);
    state.rows.reset(7, 20);
    assert_eq!(one(&state, 2), InfoModel::Reading);
}

/// The title names the view and sums it up; the status line counts it; Develop N counts the picks
/// in view; with Develop shown nothing is derived.
#[test]
fn the_select_title_and_status_line_sum_up_the_view() {
    let konstanz = listed("Konstanz", vec![month(2026, 9)], 1042, 18, 0);
    let source = ViewSource::Event {
        event_id: konstanz.id.clone(),
    };
    let mut state = SelectState {
        shown: Shown::Select,
        events: Some(EventList {
            events: vec![konstanz],
            months: vec![month_count(month(2026, 9))],
        }),
        query: Some(ViewQuery::of(source.clone())),
        home: Some("/Users/w".into()),
        ..SelectState::default()
    };
    // Before the view is read: the event's own count, and the centre says it is reading.
    let model = model(&state, &BrowseSession::default(), "Ready", Some(1), false);
    assert_eq!(model.title.name, "Konstanz");
    assert_eq!(
        model.title.summary,
        "12\u{2013}13 Sep 2026 \u{b7} 1,042 photographs \u{b7} Leica Q2, Nikon Z 8 \u{b7} from ~/Pictures and /Volumes/NIKON Z 8"
    );
    assert_eq!(model.title.picks, 0);
    assert_eq!(model.note.as_deref(), Some("Reading the view\u{2026}"));
    assert_eq!(model.status.line, "Camera previews \u{b7} auto-organized");
    assert_eq!(model.status.clients, "1 agent connected");
    assert!(model.status.agents_connected);
    // Once read: the view's count and picks.
    let mut read = trip();
    read.query = ViewQuery::of(source);
    read.count = 1040;
    read.picked = 18;
    state.summary = Some(read);
    let model = super::model(&state, &session(7, &[(0, 2)], Some(0)), "", None, false);
    assert!(model.title.summary.contains("1,040 photographs"));
    assert_eq!(model.title.picks, 18);
    assert_eq!(model.note, None);
    assert_eq!(
        model.status.line,
        "Camera previews \u{b7} auto-organized \u{b7} 1,040 in view \u{b7} 18 picked"
    );
    assert_eq!(model.selection.count, 2);
    // Over the catalog: developed photographs, counted as selected, and no picks.
    let mut catalog = summary(9, GroupLayout::default());
    catalog.query = source_query(ViewSource::AllPhotographs);
    state.query = Some(catalog.query.clone());
    state.summary = Some(catalog);
    let model = super::model(&state, &session(7, &[(0, 5)], Some(0)), "", None, false);
    assert_eq!(model.title.name, "All photographs");
    assert_eq!(model.title.summary, "9 developed photographs");
    assert_eq!(model.title.picks, 0);
    assert_eq!(model.status.line, "9 in view \u{b7} 5 selected");
    assert!(model.catalog_cells);
    // A folder: its name and where it is.
    state.query = Some(ViewQuery::of(ViewSource::Folder {
        path: "/Users/w/Pictures/Dumps".into(),
        subfolders: true,
    }));
    state.summary = None;
    let model = super::model(&state, &BrowseSession::default(), "", None, false);
    assert_eq!(model.title.name, "Dumps");
    assert_eq!(model.title.summary, "~/Pictures/Dumps");
    // Nothing chosen yet; and Develop shown derives nothing.
    state.query = None;
    let model = super::model(&state, &BrowseSession::default(), "", None, false);
    assert_eq!(
        model.note.as_deref(),
        Some("Choose an event, a folder on disk or a catalog view")
    );
    assert!(!model.filter.enabled && !model.strip.enabled);
    state.shown = Shown::Develop;
    assert_eq!(
        super::model(&state, &BrowseSession::default(), "", None, false),
        SelectModel::default()
    );
}

/// `S` finds the burst an item is a frame of, and nothing for a bracket or a single; the size
/// slider keeps its range and step per preset.
#[test]
fn select_bursts_and_cell_widths() {
    let mut state = SelectState {
        summary: Some(trip()),
        ..SelectState::default()
    };
    assert_eq!(state.burst_of(1), Some(0));
    assert_eq!(state.burst_of(3), Some(0));
    assert_eq!(state.burst_of(4), None);
    assert_eq!(state.burst_of(8), None, "a bracket");
    assert_eq!(state.burst_of(15), Some(2));
    assert_eq!(state.cell_width(), FILES_CELL_WIDTH);
    state.set_cell_width(201.0);
    assert_eq!(state.files_cell_width, 200.0);
    state.set_cell_width(10_000.0);
    assert_eq!(state.cell_width(), CELL_WIDTH_MAX);
    state.query = Some(source_query(ViewSource::AllPhotographs));
    assert_eq!(state.cell_width(), CATALOG_CELL_WIDTH);
    state.set_cell_width(0.0);
    assert_eq!(state.catalog_cell_width, CELL_WIDTH_MIN);
    assert_eq!(state.files_cell_width, CELL_WIDTH_MAX);
    assert_eq!(clamp_cell_width(f32::NAN), FILES_CELL_WIDTH);
}
