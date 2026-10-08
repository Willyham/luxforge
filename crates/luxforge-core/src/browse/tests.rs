//! Views, facets, rows, selection and events over a generated catalog and index, each against the
//! plain model in [`super::testing`].
use super::{
    Context, EventCache, Probe, SelectRequest, View, current_stamp, evaluate, event_list, facets,
    previews::index_grid_states,
    refresh_stale, rows, select, selected_items, source_files,
    testing::{self, Fixture, Item},
    view::Evaluation,
};
use crate::{
    EditorService, ErrorKind, SourceTag,
    catalog_types::{
        BodyKey, BrowseSession, DateRange, Facet, FileAvailability, Grouping, ItemRef, LocalDay,
        MAX_VIEW_ITEMS, MomentRef, Month, NoProbe, PositionRange, PreviewState, RowItem,
        SelectionMode, SortKey, ViewFilter, ViewItem, ViewQuery, ViewSelection, ViewSort,
        ViewSource,
    },
};
use rusqlite::Connection;
use std::path::PathBuf;

fn run(service: &EditorService, events: &mut EventCache, query: &ViewQuery) -> Evaluation {
    try_run(service, events, query, MAX_VIEW_ITEMS)
        .unwrap_or_else(|error| panic!("{query:?}: {error:?}"))
}

fn try_run(
    service: &EditorService,
    events: &mut EventCache,
    query: &ViewQuery,
    limit: usize,
) -> Result<Evaluation, crate::Error> {
    evaluate(
        Context {
            service,
            events,
            probe: Probe::Given(&NoProbe),
            limit,
        },
        query,
    )
}

fn day(month: u32, day: u32) -> LocalDay {
    LocalDay::from_ymd(2026, month, day).unwrap()
}

fn query(source: ViewSource, filter: ViewFilter, sort: ViewSort, grouping: Grouping) -> ViewQuery {
    ViewQuery {
        filter,
        sort,
        grouping,
        ..ViewQuery::of(source)
    }
}

/// The view agrees with the model: its items in order, its layout with its days' and moments' pick
/// counts, its counts; and the layout keeps the promises its signature makes whatever the organize
/// rules become.
fn agree(fx: &Fixture, service: &EditorService, events: &mut EventCache, query: &ViewQuery) {
    let got = run(service, events, query);
    let want = fx.view(query);
    assert_eq!(got.items, want.order(), "order of {query:?}");
    assert_eq!(got.layout, want.layout, "layout of {query:?}");
    assert_eq!(
        (got.picked, got.in_catalog, got.unavailable),
        (want.picked, want.in_catalog, want.unavailable),
        "counts of {query:?}"
    );
    let by_item = |item: &ViewItem| want.items.iter().find(|model| model.item == *item).unwrap();
    // Days partition a grouped view, each one day; cameras sit inside days, each one body.
    if query.effective_grouping() != Grouping::None {
        let mut next = 0;
        for group in &got.layout.days {
            assert_eq!(group.start, next, "{query:?}");
            for item in &got.items[group.start as usize..(group.start + group.len) as usize] {
                assert_eq!(by_item(item).day(), group.day, "{query:?}");
            }
            next += group.len;
        }
        assert_eq!(next as usize, got.items.len(), "{query:?}");
        for camera in &got.layout.cameras {
            for item in &got.items[camera.start as usize..(camera.start + camera.len) as usize] {
                assert_eq!(by_item(item).camera_key(), camera.body, "{query:?}");
            }
        }
        // Each day and moment counts exactly the picked frames it covers, and together the days
        // count the view's picks.
        let picked = |start: u32, len: u32| {
            got.items[start as usize..(start + len) as usize]
                .iter()
                .filter(|item| by_item(item).picked)
                .count() as u32
        };
        for group in &got.layout.days {
            assert_eq!(group.picked, picked(group.start, group.len), "{query:?}");
        }
        for moment in &got.layout.moments {
            assert_eq!(moment.picked, picked(moment.start, moment.len), "{query:?}");
        }
        assert_eq!(
            got.layout.days.iter().map(|day| day.picked).sum::<u32>(),
            got.picked,
            "{query:?}"
        );
    } else {
        assert_eq!(got.layout, Default::default(), "{query:?}");
    }
    // Undated items come last in every order but the file-name ones, where they only break ties.
    if matches!(query.sort.key, SortKey::CaptureTime) {
        let dated: Vec<bool> = got
            .items
            .iter()
            .map(|item| by_item(item).instant().is_some())
            .collect();
        assert!(
            dated.windows(2).all(|pair| pair[0] || !pair[1]),
            "undated last: {query:?}"
        );
        if query.effective_grouping() == Grouping::None {
            let instants: Vec<i64> = got
                .items
                .iter()
                .filter_map(|item| by_item(item).instant())
                .collect();
            let ordered = if query.sort.descending {
                instants.windows(2).all(|pair| pair[0] >= pair[1])
            } else {
                instants.windows(2).all(|pair| pair[0] <= pair[1])
            };
            assert!(ordered, "{query:?}");
        }
    }
}

fn sorts(photos: bool) -> Vec<ViewSort> {
    let mut keys = vec![SortKey::CaptureTime, SortKey::FileName];
    if photos {
        keys.extend([SortKey::DateDeveloped, SortKey::LastEdited]);
    }
    keys.into_iter()
        .flat_map(|key| [false, true].map(|descending| ViewSort { key, descending }))
        .collect()
}

fn groupings(sort: &ViewSort) -> Vec<Grouping> {
    if sort.key == SortKey::CaptureTime {
        Grouping::ALL.to_vec()
    } else {
        vec![Grouping::DayCameraMoment]
    }
}

fn file_filters() -> Vec<ViewFilter> {
    let z8a = testing::z8("3001").key();
    vec![
        ViewFilter::default(),
        ViewFilter {
            text: Some("leica".into()),
            ..Default::default()
        },
        ViewFilter {
            text: Some("dsc_000".into()),
            ..Default::default()
        },
        ViewFilter {
            text: Some("SUMMILUX".into()),
            ..Default::default()
        },
        ViewFilter {
            picked: Some(true),
            ..Default::default()
        },
        ViewFilter {
            picked: Some(false),
            ..Default::default()
        },
        ViewFilter {
            without_pick: true,
            ..Default::default()
        },
        ViewFilter {
            cameras: vec![z8a.clone()],
            ..Default::default()
        },
        ViewFilter {
            cameras: vec![BodyKey::default()],
            ..Default::default()
        },
        ViewFilter {
            lenses: vec![testing::NIKKOR.into()],
            ..Default::default()
        },
        ViewFilter {
            kinds: vec![SourceTag::Jpeg],
            ..Default::default()
        },
        ViewFilter {
            dates: Some(DateRange {
                from: day(9, 12),
                to: day(9, 12),
            }),
            ..Default::default()
        },
        ViewFilter {
            cameras: vec![z8a, testing::q3().key()],
            without_pick: true,
            dates: Some(DateRange {
                from: day(9, 1),
                to: day(9, 30),
            }),
            kinds: vec![SourceTag::Raw],
            ..Default::default()
        },
    ]
}

fn photo_filters() -> Vec<ViewFilter> {
    vec![
        ViewFilter::default(),
        ViewFilter {
            text: Some("konstanz".into()),
            ..Default::default()
        },
        ViewFilter {
            text: Some("ZÜR".into()),
            ..Default::default()
        },
        ViewFilter {
            text: Some("k3".into()),
            ..Default::default()
        },
        ViewFilter {
            edited: Some(true),
            ..Default::default()
        },
        ViewFilter {
            edited: Some(false),
            ..Default::default()
        },
        ViewFilter {
            cameras: vec![testing::q3().key()],
            ..Default::default()
        },
        ViewFilter {
            kinds: vec![SourceTag::Raw],
            ..Default::default()
        },
        ViewFilter {
            places: vec!["Zürich".into()],
            ..Default::default()
        },
        ViewFilter {
            lenses: vec![testing::SUMMILUX.into()],
            ..Default::default()
        },
        ViewFilter {
            dates: Some(DateRange {
                from: day(9, 13),
                to: day(9, 13),
            }),
            ..Default::default()
        },
        ViewFilter {
            places: vec!["Konstanz".into(), "Oslo".into()],
            edited: Some(false),
            text: Some("l".into()),
            ..Default::default()
        },
    ]
}

fn file_sources(fx: &Fixture) -> Vec<ViewSource> {
    let folder = |path: &str, subfolders| ViewSource::Folder {
        path: path.into(),
        subfolders,
    };
    let mut sources = vec![
        folder(testing::PICTURES, true),
        folder(testing::PICTURES, false),
        folder(testing::LAKE, true),
        folder(testing::CARD_1, false),
        folder(testing::OLD, false),
        folder(testing::ARCHIVE, true),
        ViewSource::Card {
            volume_id: testing::volume("card"),
        },
    ];
    sources.extend(
        fx.events()
            .into_iter()
            .map(|(group, _)| ViewSource::Event { event_id: group.id }),
    );
    sources
}

fn photo_sources(fx: &Fixture) -> Vec<ViewSource> {
    let folder = |id: &str, subfolders| ViewSource::CatalogFolder {
        folder_id: testing::folder_id(id),
        subfolders,
    };
    vec![
        ViewSource::AllPhotographs,
        ViewSource::RecentlyDeveloped { days: 30 },
        ViewSource::RecentlyDeveloped { days: 7 },
        folder("konstanz-2026", true),
        folder("konstanz-2026", false),
        folder("konstanz-selects", true),
        folder("empty-folder", true),
        ViewSource::Collection {
            collection_id: fx.portfolio.clone(),
        },
        ViewSource::Collection {
            collection_id: fx.raw_konstanz.clone(),
        },
        ViewSource::Collection {
            collection_id: fx.edited_portfolio.clone(),
        },
        ViewSource::MissingOriginals,
        ViewSource::Removed,
    ]
}

/// Every view of files agrees with the model: every source, filter, sort and grouping, including
/// picks, Moments without a pick, subfolders, a folder whose name extends another's, offline and
/// undated files and every sort's tie-breaks.
#[test]
fn browse_views_of_files_agree_with_the_model() {
    let fx = testing::fixture("files-agree");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut events = EventCache::default();
    let mut checked = 0;
    for source in file_sources(&fx) {
        for filter in file_filters() {
            for sort in sorts(false) {
                for grouping in groupings(&sort) {
                    agree(
                        &fx,
                        &service,
                        &mut events,
                        &query(source.clone(), filter.clone(), sort, grouping),
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 500, "{checked}");
    // The sibling whose name extends the indexed folder's is never inside it.
    let pictures = run(
        &service,
        &mut events,
        &ViewQuery::of(ViewSource::Folder {
            path: testing::PICTURES.into(),
            subfolders: true,
        }),
    );
    let old = fx.file(&format!("{}/OLD_0001.JPG", testing::OLD)).id;
    assert!(!pictures.items.contains(&ViewItem::File(old)));
    assert_eq!(
        pictures.items.len(),
        11,
        "the lake, its selects, the dump and the root"
    );
}

/// Every view of photographs agrees with the model: all, recent, catalog folders with and without
/// subfolders, a plain collection with a removed member, smart collections, missing originals and
/// removed, under every filter and sort.
#[test]
fn browse_views_of_photographs_agree_with_the_model() {
    let fx = testing::fixture("photos-agree");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut events = EventCache::default();
    for source in photo_sources(&fx) {
        for filter in photo_filters() {
            for sort in sorts(true) {
                for grouping in groupings(&sort) {
                    agree(
                        &fx,
                        &service,
                        &mut events,
                        &query(source.clone(), filter.clone(), sort, grouping),
                    );
                }
            }
        }
    }
    // Spot checks the model cannot get wrong with the implementation: the removed photograph is
    // only under Removed, and the empty folder is empty.
    let all = run(
        &service,
        &mut events,
        &ViewQuery::of(ViewSource::AllPhotographs),
    );
    assert_eq!(all.items.len(), 14);
    let removed = run(&service, &mut events, &ViewQuery::of(ViewSource::Removed));
    assert_eq!(
        removed.items,
        [fx.photos[12].row, fx.photos[6].row].map(ViewItem::Photo),
        "2025 before 2026"
    );
    assert!(removed.items.iter().all(|item| !all.items.contains(item)));
    let portfolio = run(
        &service,
        &mut events,
        &ViewQuery::of(ViewSource::Collection {
            collection_id: fx.portfolio.clone(),
        }),
    );
    assert_eq!(portfolio.items.len(), 4, "its removed member is hidden");
    // Date developed ties (K3 and k3.JPG, developed together) fall to capture time, which ties,
    // then the name ignoring case, which ties, then the row.
    let developed = run(
        &service,
        &mut events,
        &query(
            ViewSource::AllPhotographs,
            ViewFilter {
                text: Some("k3".into()),
                ..Default::default()
            },
            ViewSort {
                key: SortKey::DateDeveloped,
                descending: true,
            },
            Grouping::DayCameraMoment,
        ),
    );
    assert_eq!(
        developed.items,
        [fx.photos[2].row, fx.photos[15].row].map(ViewItem::Photo)
    );
}

/// Picks come from the catalog by path; a file whose path is a developed photograph's original is
/// in the catalog unless that photograph is removed; an offline root's files are offline.
#[test]
fn browse_a_file_carries_its_pick_catalog_state_and_availability() {
    let fx = testing::fixture("file-state");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut events = EventCache::default();
    let card = run(
        &service,
        &mut events,
        &ViewQuery::of(ViewSource::Card {
            volume_id: testing::volume("card"),
        }),
    );
    assert_eq!(card.picked, 2);
    // The burst DSC_0001–0003 has its pick in DSC_0002, and the first day holds both picks.
    let burst = card
        .layout
        .moments
        .iter()
        .find(|moment| moment.start == 0)
        .expect("the card's burst");
    assert_eq!((burst.len, burst.picked), (3, 1));
    assert_eq!(card.layout.days[0].picked, 2);
    assert_eq!(
        card.in_catalog, 1,
        "DSC_0003; DSC_0005's photograph is removed"
    );
    assert_eq!(card.unavailable, 0);
    let archive = run(
        &service,
        &mut events,
        &ViewQuery::of(ViewSource::Folder {
            path: testing::ARCHIVE.into(),
            subfolders: true,
        }),
    );
    assert_eq!(archive.unavailable, 4);
    assert_eq!(archive.picked, 1);
}

/// `event.list` lists the model's events newest first with their counts, picks, offline files,
/// roots, volumes, months and cameras; `month` narrows them; `query` matches dates and cameras;
/// and an event's view holds exactly its files.
#[test]
fn browse_events_agree_with_the_model() {
    let fx = testing::fixture("events");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut cache = EventCache::default();
    let list = event_list(&service, &mut cache, None, None).unwrap();
    let model = fx.events();
    assert_eq!(list.events.len(), model.len());
    let items = fx.file_items();
    for event in &list.events {
        let (group, files) = model
            .iter()
            .find(|(group, _)| group.id == event.id)
            .expect("the model's event");
        assert_eq!(event.count as usize, files.len(), "{}", event.name);
        assert_eq!(event.name, group.name);
        assert_eq!(
            (event.first_day, event.last_day),
            (group.first_day, group.last_day)
        );
        assert_eq!(event.undated, group.undated());
        let members: Vec<&Item> = items
            .iter()
            .filter(|item| matches!(item.item, ViewItem::File(file) if files.contains(&file)))
            .collect();
        assert_eq!(
            event.picked as usize,
            members.iter().filter(|item| item.picked).count()
        );
        assert_eq!(
            event.offline as usize,
            members
                .iter()
                .filter(|item| item.availability == FileAvailability::Offline)
                .count()
        );
        let mut months: Vec<Month> = members
            .iter()
            .filter_map(|item| item.day().map(LocalDay::month))
            .collect();
        months.sort();
        months.dedup();
        assert_eq!(event.months, months);
        let view = run(
            &service,
            &mut cache,
            &ViewQuery::of(ViewSource::Event {
                event_id: event.id.clone(),
            }),
        );
        let mut held: Vec<ViewItem> = view.items.clone();
        held.sort();
        assert_eq!(
            held,
            files
                .iter()
                .map(|file| ViewItem::File(*file))
                .collect::<Vec<_>>()
        );
        for root in &event.roots {
            assert!(
                members
                    .iter()
                    .any(|item| PathBuf::from(&item.path).starts_with(root))
            );
        }
    }
    // Newest first, the Undated events last.
    let starts: Vec<Option<i64>> = list
        .events
        .iter()
        .map(|event| {
            model
                .iter()
                .find(|(group, _)| group.id == event.id)
                .unwrap()
                .0
                .start_ms
        })
        .collect();
    let dated: Vec<i64> = starts.iter().flatten().copied().collect();
    assert!(dated.windows(2).all(|pair| pair[0] >= pair[1]));
    assert!(
        starts
            .windows(2)
            .all(|pair| pair[0].is_some() || pair[1].is_none())
    );
    // The browsed folder is in no event; the offline archive is.
    let old = fx.file(&format!("{}/OLD_0001.JPG", testing::OLD)).id;
    assert!(model.iter().all(|(_, files)| !files.contains(&old)));
    assert!(list.events.iter().any(|event| event.offline > 0));
    // Months: newest first, with the files and picks taken in each.
    let september = Month {
        year: 2026,
        month: 9,
    };
    assert_eq!(list.months[0].month, september);
    let in_september = items
        .iter()
        .filter(|item| fx.event_items().iter().any(|event| event.item == item.item))
        .filter(|item| item.day().map(LocalDay::month) == Some(september))
        .collect::<Vec<_>>();
    assert_eq!(list.months[0].files as usize, in_september.len());
    assert_eq!(
        list.months[0].picked as usize,
        in_september.iter().filter(|item| item.picked).count()
    );
    let june = event_list(
        &service,
        &mut cache,
        Some(Month {
            year: 2025,
            month: 6,
        }),
        None,
    )
    .unwrap();
    assert!(!june.events.is_empty());
    assert!(
        june.events
            .iter()
            .all(|event| event.months.contains(&Month {
                year: 2025,
                month: 6
            }))
    );
    assert_eq!(
        june.months, list.months,
        "months count every month the query matches"
    );
    // A query matches a day an event spans, and a camera.
    let dated = event_list(&service, &mut cache, None, Some("2026-09-13")).unwrap();
    assert!(!dated.events.is_empty());
    assert!(
        dated
            .events
            .iter()
            .all(|event| event.first_day <= Some(day(9, 13)) && Some(day(9, 13)) <= event.last_day)
    );
    let nikon = event_list(&service, &mut cache, None, Some("nikon z 8")).unwrap();
    assert!(!nikon.events.is_empty());
    assert!(
        nikon
            .events
            .iter()
            .all(|event| event.cameras.iter().any(|camera| camera == "NIKON Z 8"))
    );
    let fuji = event_list(&service, &mut cache, None, Some("X100")).unwrap();
    assert!(
        fuji.events
            .iter()
            .all(|event| event.cameras.contains(&"FUJIFILM X100VI".to_owned()))
    );
    assert!(
        event_list(&service, &mut cache, None, Some("no such thing"))
            .unwrap()
            .events
            .is_empty()
    );
    // An unknown event is refused.
    let unknown = crate::catalog_types::EventId::of(std::path::Path::new("/nowhere"), 0);
    let error = try_run(
        &service,
        &mut cache,
        &ViewQuery::of(ViewSource::Event { event_id: unknown }),
        MAX_VIEW_ITEMS,
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
}

/// A query matches an event's place, which only a gazetteer names, as it matches its name.
#[test]
fn browse_an_event_query_matches_its_place() {
    let fx = testing::fixture("event-place");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut cache = EventCache::default();
    let mut event = event_list(&service, &mut cache, None, None)
        .unwrap()
        .events
        .remove(0);
    event.place = Some("Konstanz".into());
    assert!(super::events::matches(&event, "konst"));
    assert!(!super::events::matches(&event, "oslo"));
}

/// The facet a query's own condition replaced by each value predicts that view exactly.
fn predicted(source: &ViewSource, filter: &ViewFilter, facet: Facet, value: &str) -> ViewQuery {
    let mut filter = filter.clone();
    match facet {
        Facet::Date => {
            let day: LocalDay = serde_json::from_value(serde_json::json!(value)).unwrap();
            filter.dates = Some(DateRange { from: day, to: day });
        }
        Facet::Place => filter.places = vec![value.into()],
        Facet::Camera => filter.cameras = vec![BodyKey(value.into())],
        Facet::Lens => filter.lenses = vec![value.into()],
        Facet::Kind => {
            filter.kinds = vec![serde_json::from_value(serde_json::json!(value)).unwrap()]
        }
        Facet::Pick => filter.picked = Some(value == "picked"),
    }
    ViewQuery {
        filter,
        ..ViewQuery::of(source.clone())
    }
}

/// Every facet count equals the size of the view it predicts, and the count of items recording no
/// value equals the model's.
#[test]
fn browse_facets_equal_the_views_they_predict() {
    let fx = testing::fixture("facets");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut events = EventCache::default();
    let cases: Vec<(ViewSource, Vec<ViewFilter>)> = vec![
        (
            ViewSource::Folder {
                path: testing::PICTURES.into(),
                subfolders: true,
            },
            file_filters(),
        ),
        (
            ViewSource::Card {
                volume_id: testing::volume("card"),
            },
            file_filters(),
        ),
        (ViewSource::AllPhotographs, photo_filters()),
        (
            ViewSource::Collection {
                collection_id: fx.portfolio.clone(),
            },
            photo_filters(),
        ),
    ];
    let mut compared = 0;
    for (source, filters) in cases {
        let asked: Vec<Facet> = Facet::ALL
            .into_iter()
            .filter(|facet| source.over_files() || *facet != Facet::Pick)
            .collect();
        for filter in filters {
            let counts = facets(
                Context {
                    service: &service,
                    events: &mut events,
                    probe: Probe::Given(&NoProbe),
                    limit: MAX_VIEW_ITEMS,
                },
                &source,
                &filter,
                &asked,
            )
            .unwrap();
            assert_eq!(counts.counts.len(), asked.len());
            for (facet, values) in &counts.counts {
                assert!(
                    values.windows(2).all(|pair| pair[0].count >= pair[1].count),
                    "most frequent first"
                );
                let mut total = 0;
                for value in values {
                    total += value.count;
                    match &value.value {
                        Some(text) => {
                            let view = run(
                                &service,
                                &mut events,
                                &predicted(&source, &filter, *facet, text),
                            );
                            assert_eq!(
                                view.items.len() as u32,
                                value.count,
                                "{facet:?} {text} of {filter:?}"
                            );
                            compared += 1;
                        }
                        None => {
                            let items = fx.source_items(&source);
                            let without = ViewFilter {
                                dates: None,
                                places: vec![],
                                cameras: vec![],
                                lenses: vec![],
                                ..filter.clone()
                            };
                            let decided = fx.view(&ViewQuery {
                                filter: without.clone(),
                                ..ViewQuery::of(source.clone())
                            });
                            let none = items
                                .iter()
                                .filter(|item| {
                                    decided.items.iter().any(|kept| kept.item == item.item)
                                })
                                .filter(|item| match facet {
                                    Facet::Date => {
                                        item.day().is_none() && filter.dates.is_none_or(|_| true)
                                    }
                                    Facet::Place => item.place.is_none(),
                                    Facet::Camera => item.header.camera.is_none(),
                                    Facet::Lens => item.header.lens.is_none(),
                                    Facet::Kind | Facet::Pick => false,
                                })
                                .filter(|item| {
                                    // The other list conditions still apply.
                                    let others = ViewFilter {
                                        dates: if *facet == Facet::Date {
                                            None
                                        } else {
                                            filter.dates
                                        },
                                        places: if *facet == Facet::Place {
                                            vec![]
                                        } else {
                                            filter.places.clone()
                                        },
                                        cameras: if *facet == Facet::Camera {
                                            vec![]
                                        } else {
                                            filter.cameras.clone()
                                        },
                                        lenses: if *facet == Facet::Lens {
                                            vec![]
                                        } else {
                                            filter.lenses.clone()
                                        },
                                        ..ViewFilter::default()
                                    };
                                    testing::passes(item, &others, false)
                                })
                                .count();
                            assert_eq!(value.count as usize, none, "{facet:?} none of {filter:?}");
                        }
                    }
                }
                // Together the values count the view without the facet's own condition.
                let mut without = filter.clone();
                match facet {
                    Facet::Date => without.dates = None,
                    Facet::Place => without.places.clear(),
                    Facet::Camera => without.cameras.clear(),
                    Facet::Lens => without.lenses.clear(),
                    Facet::Kind => without.kinds.clear(),
                    Facet::Pick => without.picked = None,
                }
                let whole = run(
                    &service,
                    &mut events,
                    &ViewQuery {
                        filter: without,
                        ..ViewQuery::of(source.clone())
                    },
                );
                assert_eq!(total as usize, whole.items.len(), "{facet:?} of {filter:?}");
            }
        }
    }
    assert!(compared > 200, "{compared}");
    // Labels: a camera by its body, a kind and a pick by name.
    let counts = facets(
        Context {
            service: &service,
            events: &mut events,
            probe: Probe::Given(&NoProbe),
            limit: MAX_VIEW_ITEMS,
        },
        &ViewSource::Card {
            volume_id: testing::volume("card"),
        },
        &ViewFilter::default(),
        &[Facet::Camera, Facet::Kind, Facet::Pick, Facet::Camera],
    )
    .unwrap();
    assert_eq!(counts.counts.len(), 3, "each facet once");
    let cameras = &counts.counts[&Facet::Camera];
    assert_eq!(
        cameras[0].value.as_deref(),
        Some("NIKON CORPORATION|NIKON Z 8|3001")
    );
    assert_eq!(cameras[0].label.as_deref(), Some("NIKON Z 8"));
    assert_eq!(
        cameras.last().unwrap().value,
        None,
        "the unreadable file records no camera"
    );
    assert_eq!(
        cameras.last().unwrap().label.as_deref(),
        Some("Unknown camera")
    );
    assert_eq!(
        counts.counts[&Facet::Pick][1].value.as_deref(),
        Some("picked")
    );
    let refused = facets(
        Context {
            service: &service,
            events: &mut events,
            probe: Probe::Given(&NoProbe),
            limit: MAX_VIEW_ITEMS,
        },
        &ViewSource::AllPhotographs,
        &ViewFilter::default(),
        &[Facet::Pick],
    )
    .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Validation);
}

fn held(evaluation: &Evaluation, over_files: bool) -> View {
    View {
        revision: 1,
        over_files,
        items: evaluation.items.clone(),
        layout: evaluation.layout.clone(),
        selection: ViewSelection::default(),
        stale: false,
    }
}

/// Seed preview records: DSC_0001's grid tier, the second photograph's grid render of its current
/// entry, and the third's of another entry.
fn seed_previews(fx: &Fixture) {
    let index =
        Connection::open(crate::index::index_dir(&fx.catalog).join(crate::INDEX_FILE)).unwrap();
    let file = fx.file(&format!("{}/DSC_0001.NEF", testing::CARD_1)).id;
    index
        .execute(
            "INSERT INTO previews (file_id, tier, byte_len, modified_ns, path, width, height, bytes,
                 origin, last_used_ms)
             VALUES (?1, 'grid', 1, 1, '/p/a.jpg', 160, 120, 5000, 'exif-thumbnail', 1)",
            [file.0],
        )
        .unwrap();
    for (at, entry) in [
        (1usize, None),
        (2, Some("entry-another0000000000000000000")),
    ] {
        let asset = fx.photos[at].seed.id.as_str();
        let entry = entry
            .map(str::to_owned)
            .unwrap_or_else(|| format!("entry-{}", &asset["asset-".len()..]));
        index
            .execute(
                "INSERT INTO photo_previews (asset_id, entry_id, tier, renderer, path, width, height,
                     bytes, origin, last_used_ms, drawn_by)
                 VALUES (?1, ?2, 'grid', 1, ?3, 512, 341, 9000, 'rendered', 1, 'gpu')",
                rusqlite::params![asset, entry, format!("/p/{at}.jpg")],
            )
            .unwrap();
    }
}

/// `browse.rows` reads windows by position: every field of every row agrees with the model item at
/// that position, windows end at the view's end, and a window past it is refused.
#[test]
fn browse_rows_read_windows_by_position() {
    let fx = testing::fixture("rows");
    seed_previews(&fx);
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut events = EventCache::default();
    let files = ViewQuery::of(ViewSource::Folder {
        path: "/Volumes/NIKON Z 8/DCIM".into(),
        subfolders: true,
    });
    let evaluation = run(&service, &mut events, &files);
    let want = fx.view(&files);
    let view = held(&evaluation, true);
    let len = view.items.len() as u32;
    for (from, count) in [(0, 3), (3, 10), (len - 2, 10), (0, 1000)] {
        let window = rows(&service, &view, from, count, &|items| {
            index_grid_states(&service, items)
        })
        .unwrap();
        assert_eq!(window.len() as u32, count.min(len - from));
        for row in &window {
            let item = &want.items[row.position as usize];
            let file = fx
                .files
                .iter()
                .find(|file| ViewItem::File(file.id) == item.item)
                .unwrap();
            assert_eq!(row.item, RowItem::File { file_id: file.id });
            assert_eq!(row.path, file.record.path);
            assert_eq!(row.file_name, file.record.name);
            assert_eq!(row.kind, file.record.kind);
            assert_eq!(row.picked, file.picked, "{}", row.file_name);
            assert_eq!(row.developed_as, file.developed_as);
            assert_eq!(row.availability, FileAvailability::Available);
            assert!(!row.edited);
            let header = file.record.header.header();
            assert_eq!(
                row.capture,
                header
                    .and_then(|header| header.capture.as_ref())
                    .map(|time| time.text.clone())
            );
            assert_eq!(
                row.camera,
                header
                    .and_then(|header| header.camera.as_ref())
                    .map(|camera| camera.label())
            );
            assert_eq!(row.lens, header.and_then(|header| header.lens.clone()));
            assert_eq!(row.dimensions, header.and_then(|header| header.dimensions));
            let moment = want
                .layout
                .moments
                .iter()
                .enumerate()
                .find(|(_, moment)| {
                    (moment.start..moment.start + moment.len).contains(&row.position)
                })
                .map(|(index, moment)| MomentRef {
                    index: index as u32,
                    frame: row.position - moment.start,
                });
            assert_eq!(row.moment, moment, "the model's moment covering it");
            let preview = match &file.record.header {
                _ if file.record.name == "DSC_0001.NEF" => PreviewState::Ready,
                crate::catalog_types::HeaderState::Ok(header) if header.thumbnail.is_some() => {
                    PreviewState::Thumbnail
                }
                crate::catalog_types::HeaderState::Unreadable(_) => PreviewState::Unavailable,
                _ => PreviewState::Pending,
            };
            assert_eq!(row.preview, preview, "{}", row.file_name);
        }
    }
    assert!(
        rows(&service, &view, len, 5, &|items| index_grid_states(
            &service, items
        ))
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        rows(&service, &view, len + 1, 5, &|items| index_grid_states(
            &service, items
        ))
        .unwrap_err()
        .kind,
        ErrorKind::Validation
    );
    // A window's moments come from the layout.
    let mut grouped = view.clone();
    grouped.layout.moments = vec![crate::catalog_types::Moment {
        kind: crate::catalog_types::MomentKind::Burst,
        evidence: None,
        steps_ev: vec![],
        span_ms: 600,
        start: 1,
        len: 3,
        picked: 0,
    }];
    let window = rows(&service, &grouped, 0, 5, &|items| {
        index_grid_states(&service, items)
    })
    .unwrap();
    let moments: Vec<Option<(u32, u32)>> = window
        .iter()
        .map(|row| row.moment.map(|at| (at.index, at.frame)))
        .collect();
    assert_eq!(
        moments,
        [None, Some((0, 0)), Some((0, 1)), Some((0, 2)), None]
    );
    // Offline files say so.
    let archive = run(
        &service,
        &mut events,
        &ViewQuery::of(ViewSource::Folder {
            path: testing::OSLO.into(),
            subfolders: false,
        }),
    );
    let window = rows(&service, &held(&archive, true), 0, 10, &|items| {
        index_grid_states(&service, items)
    })
    .unwrap();
    assert!(
        window
            .iter()
            .all(|row| row.availability == FileAvailability::Offline)
    );
    // Photographs: their original's path, edits, availability and rendered previews.
    let photos = ViewQuery {
        sort: ViewSort {
            key: SortKey::DateDeveloped,
            descending: false,
        },
        ..ViewQuery::of(ViewSource::AllPhotographs)
    };
    let evaluation = run(&service, &mut events, &photos);
    let window = rows(&service, &held(&evaluation, false), 0, 100, &|items| {
        index_grid_states(&service, items)
    })
    .unwrap();
    assert_eq!(window.len(), 14);
    for row in &window {
        let photo = fx
            .photos
            .iter()
            .find(|photo| {
                RowItem::Photo {
                    asset_id: photo.seed.id.clone(),
                } == row.item
            })
            .unwrap();
        assert_eq!(
            ViewItem::Photo(photo.row),
            evaluation.items[row.position as usize]
        );
        assert_eq!(row.path, photo.seed.locator);
        assert_eq!(row.edited, photo.edited_ms.is_some());
        assert_eq!(row.availability, photo.seed.availability);
        assert_eq!(row.place, photo.seed.place);
        // Its catalog folder and its plain collections, for the Info panel's Organize band.
        assert_eq!(row.folder_id.as_ref(), Some(&photo.seed.catalog_folder_id));
        assert_eq!(row.collections, photo.collections);
        assert!(!row.picked && row.developed_as.is_none());
        let ready = photo.seed.id == fx.photos[1].seed.id;
        assert_eq!(
            row.preview == PreviewState::Ready,
            ready,
            "{}",
            row.file_name
        );
    }
}

/// A stale view still answers for its items, and refuses a window whose item has gone.
#[test]
fn browse_a_stale_views_gone_item_is_a_conflict() {
    let fx = testing::fixture("gone");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut events = EventCache::default();
    let query = ViewQuery::of(ViewSource::Folder {
        path: testing::LAKE.into(),
        subfolders: false,
    });
    let view = held(&run(&service, &mut events, &query), true);
    let gone = fx.file(&format!("{}/L1000003.JPG", testing::LAKE)).id;
    let position = view
        .items
        .iter()
        .position(|item| *item == ViewItem::File(gone))
        .unwrap() as u32;
    {
        let index = service.index().unwrap();
        index
            .connection()
            .execute("DELETE FROM files WHERE id = ?1", [gone.0])
            .unwrap();
        testing::set_index_revision(index.connection(), 1);
    }
    assert_eq!(
        rows(&service, &view, 0, position, &|items| index_grid_states(
            &service, items
        ))
        .unwrap()
        .len() as u32,
        position
    );
    let error = rows(&service, &view, 0, 10, &|items| {
        index_grid_states(&service, items)
    })
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(error.detail.contains("stale"), "{}", error.detail);
}

/// Replace, add, remove and toggle over items, ranges and all; the active item; and every refusal
/// leaving the selection as it was.
#[test]
fn browse_selection_modes_ranges_items_all_and_active() {
    let fx = testing::fixture("select");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut events = EventCache::default();
    let evaluation = run(
        &service,
        &mut events,
        &ViewQuery::of(ViewSource::Card {
            volume_id: testing::volume("card"),
        }),
    );
    let mut view = held(&evaluation, true);
    let len = view.items.len() as u32;
    assert_eq!(len, 15);
    let apply = |view: &mut View, request: SelectRequest| {
        let mut selection = view.selection.clone();
        let result = select(&service, view, &mut selection, request);
        if result.is_ok() {
            view.selection = selection;
        }
        result
    };
    let range = |start, len| PositionRange { start, len };
    let file = |position: usize| match view_item(&evaluation, position) {
        ViewItem::File(file_id) => ItemRef::File { file_id },
        ViewItem::Photo(_) => unreachable!(),
    };
    apply(
        &mut view,
        SelectRequest {
            range: Some(range(2, 4)),
            active: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        view.selection,
        ViewSelection {
            count: 4,
            ranges: vec![range(2, 4)],
            active: Some(3)
        }
    );
    apply(
        &mut view,
        SelectRequest {
            mode: SelectionMode::Add,
            items: Some(vec![file(6), file(9)]),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view.selection.ranges, [range(2, 5), range(9, 1)]);
    assert_eq!(view.selection.count, 6);
    assert_eq!(
        view.selection.active,
        Some(3),
        "active moves only when named"
    );
    apply(
        &mut view,
        SelectRequest {
            mode: SelectionMode::Remove,
            range: Some(range(3, 2)),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        view.selection.ranges,
        [range(2, 1), range(5, 2), range(9, 1)]
    );
    apply(
        &mut view,
        SelectRequest {
            mode: SelectionMode::Toggle,
            range: Some(range(0, 6)),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        view.selection.ranges,
        [range(0, 2), range(3, 2), range(6, 1), range(9, 1)]
    );
    assert_eq!(
        selected_items(&view),
        [0, 1, 3, 4, 6, 9].map(|at| view_item(&evaluation, at))
    );
    apply(
        &mut view,
        SelectRequest {
            all: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view.selection.ranges, [range(0, len)]);
    assert_eq!(view.selection.count, len);
    apply(
        &mut view,
        SelectRequest {
            mode: SelectionMode::Toggle,
            items: Some(vec![file(0)]),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view.selection.ranges, [range(1, len - 1)]);
    apply(
        &mut view,
        SelectRequest {
            active: Some(7),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        (view.selection.count, view.selection.active),
        (len - 1, Some(7)),
        "only active moved"
    );
    apply(
        &mut view,
        SelectRequest {
            all: Some(false),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        view.selection,
        ViewSelection {
            count: 0,
            ranges: vec![],
            active: Some(7)
        }
    );
    // Refusals change nothing.
    let before = view.selection.clone();
    for request in [
        SelectRequest {
            range: Some(range(len - 1, 2)),
            ..Default::default()
        },
        SelectRequest {
            active: Some(len),
            ..Default::default()
        },
        SelectRequest {
            items: Some(vec![ItemRef::File {
                file_id: crate::catalog_types::FileId(999_999),
            }]),
            ..Default::default()
        },
        SelectRequest {
            items: Some(vec![ItemRef::Photo {
                asset_id: fx.photos[0].seed.id.clone(),
            }]),
            ..Default::default()
        },
    ] {
        assert_eq!(
            apply(&mut view, request).unwrap_err().kind,
            ErrorKind::Validation
        );
        assert_eq!(view.selection, before);
    }
    // Photographs are named by their asset.
    let photos = run(
        &service,
        &mut events,
        &ViewQuery::of(ViewSource::AllPhotographs),
    );
    let mut view = held(&photos, false);
    let at = photos
        .items
        .iter()
        .position(|item| *item == ViewItem::Photo(fx.photos[3].row))
        .unwrap() as u32;
    apply(
        &mut view,
        SelectRequest {
            items: Some(vec![ItemRef::Photo {
                asset_id: fx.photos[3].seed.id.clone(),
            }]),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view.selection.ranges, [range(at, 1)]);
}

fn view_item(evaluation: &Evaluation, position: usize) -> ViewItem {
    evaluation.items[position]
}

/// A view is stale once a library change or an index revision follows its evaluation, and stays
/// stale.
#[test]
fn browse_a_view_goes_stale_after_a_library_change_or_an_index_revision() {
    let fx = testing::fixture("stale");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut events = EventCache::default();
    let query = ViewQuery::of(ViewSource::AllPhotographs);
    let session = |evaluation: &Evaluation| BrowseSession {
        evaluated_at: Some(evaluation.stamp),
        ..Default::default()
    };
    let first = run(&service, &mut events, &query);
    assert_eq!(first.stamp, current_stamp(&service).unwrap());
    let mut browse = session(&first);
    assert!(!refresh_stale(&service, &mut browse).unwrap());
    testing::library_change(&service, 1);
    assert!(refresh_stale(&service, &mut browse).unwrap());
    let second = run(&service, &mut events, &query);
    assert_eq!(second.stamp.library_sequence.0, 1);
    let mut browse = session(&second);
    assert!(!refresh_stale(&service, &mut browse).unwrap());
    testing::set_index_revision(service.index().unwrap().connection(), 4);
    assert!(refresh_stale(&service, &mut browse).unwrap());
    assert!(
        refresh_stale(&service, &mut browse).unwrap(),
        "stale until evaluated again"
    );
    let third = run(&service, &mut events, &query);
    assert_eq!(third.stamp.index_revision, 4);
    // No view reads nothing and is never stale.
    assert!(!refresh_stale(&service, &mut BrowseSession::default()).unwrap());
}

/// The event cache follows the index revision: a file added under a new revision joins its event.
#[test]
fn browse_events_follow_the_index_revision() {
    let fx = testing::fixture("event-cache");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut cache = EventCache::default();
    let before = event_list(&service, &mut cache, None, None).unwrap();
    let total = |list: &crate::catalog_types::EventList| {
        list.events.iter().map(|event| event.count).sum::<u32>()
    };
    {
        let mut index = service.index().unwrap();
        let tx = index.connection_mut().transaction().unwrap();
        let mut record = fx
            .file(&format!("{}/L1000003.JPG", testing::LAKE))
            .record
            .clone();
        record.path = PathBuf::from(testing::LAKE).join("L1000005.JPG");
        record.name = "L1000005.JPG".into();
        crate::index::upsert_file(&tx, &record).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(
        total(&event_list(&service, &mut cache, None, None).unwrap()),
        total(&before),
        "cached until the revision moves"
    );
    testing::set_index_revision(service.index().unwrap().connection(), 1);
    assert_eq!(
        total(&event_list(&service, &mut cache, None, None).unwrap()),
        total(&before) + 1
    );
}

/// Refusals: a query no view can answer, a source that does not exist, a smart collection that
/// names another or is over files, a group, and a source past the limit.
#[test]
fn browse_queries_are_validated_and_bounded() {
    let fx = testing::fixture("limits");
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut events = EventCache::default();
    let refused = |events: &mut EventCache, query: ViewQuery, limit| {
        try_run(&service, events, &query, limit)
            .map(|_| ())
            .unwrap_err()
    };
    let files = ViewQuery::of(ViewSource::Folder {
        path: testing::PICTURES.into(),
        subfolders: true,
    });
    assert_eq!(
        refused(&mut events, files.clone(), 10).kind,
        ErrorKind::ResourceLimit
    );
    assert!(
        try_run(&service, &mut events, &files, 11).is_ok(),
        "exactly the limit"
    );
    assert_eq!(
        refused(&mut events, ViewQuery::of(ViewSource::AllPhotographs), 13).kind,
        ErrorKind::ResourceLimit
    );
    let validation: Vec<ViewQuery> = vec![
        ViewQuery {
            filter: ViewFilter {
                edited: Some(true),
                ..Default::default()
            },
            ..files.clone()
        },
        ViewQuery {
            filter: ViewFilter {
                picked: Some(true),
                ..Default::default()
            },
            ..ViewQuery::of(ViewSource::AllPhotographs)
        },
        ViewQuery {
            sort: ViewSort {
                key: SortKey::LastEdited,
                descending: false,
            },
            ..files.clone()
        },
        ViewQuery {
            filter: ViewFilter {
                dates: Some(DateRange {
                    from: day(9, 13),
                    to: day(9, 12),
                }),
                ..Default::default()
            },
            ..files.clone()
        },
        ViewQuery {
            filter: ViewFilter {
                text: Some("x".repeat(257)),
                ..Default::default()
            },
            ..files.clone()
        },
        ViewQuery {
            thresholds: crate::catalog_types::Thresholds {
                bracket_min_frames: 1,
                ..Default::default()
            },
            ..files.clone()
        },
        ViewQuery::of(ViewSource::Folder {
            path: "relative/path".into(),
            subfolders: false,
        }),
        ViewQuery::of(ViewSource::Card {
            volume_id: testing::volume("ssd"),
        }),
        ViewQuery::of(ViewSource::CatalogFolder {
            folder_id: testing::folder_id("no-such-folder"),
            subfolders: true,
        }),
        ViewQuery::of(ViewSource::Collection {
            collection_id: crate::catalog_types::CollectionId::parse("collection-no-such-one")
                .unwrap(),
        }),
        ViewQuery::of(ViewSource::Collection {
            collection_id: fx.prints.clone(),
        }),
        ViewQuery::of(ViewSource::Collection {
            collection_id: fx.nested.clone(),
        }),
        ViewQuery::of(ViewSource::Collection {
            collection_id: fx.of_files.clone(),
        }),
    ];
    for query in validation {
        let error = refused(&mut events, query.clone(), MAX_VIEW_ITEMS);
        assert_eq!(error.kind, ErrorKind::Validation, "{query:?}: {error:?}");
    }
    // A folder the index does not know is simply empty.
    let nowhere = run(
        &service,
        &mut events,
        &ViewQuery::of(ViewSource::Folder {
            path: "/nowhere".into(),
            subfolders: true,
        }),
    );
    assert!(nowhere.items.is_empty());
}

/// A smart collection whose stored query names a catalog folder or a collection merged away or
/// deleted since finds nothing from it: an empty view, not the error the same source named directly
/// is refused with, so the smart collection still opens.
#[test]
fn browse_a_smart_collection_naming_a_deleted_source_is_empty() {
    let fx = testing::fixture("smart-gone");
    let gone_folder = testing::folder_id("merged-away");
    let gone_collection =
        crate::catalog_types::CollectionId::parse("collection-deleted-since").unwrap();
    let smart = |id: &str, source: serde_json::Value| {
        (
            crate::catalog_types::CollectionId::parse(id).unwrap(),
            serde_json::json!({"source": source}).to_string(),
        )
    };
    let smarts = [
        smart(
            "collection-names-a-gone-folder",
            serde_json::json!({"kind": "catalog-folder", "folder_id": gone_folder}),
        ),
        smart(
            "collection-names-a-gone-collection",
            serde_json::json!({"kind": "collection", "collection_id": gone_collection}),
        ),
    ];
    {
        let connection = Connection::open(&fx.catalog).unwrap();
        for (id, query) in &smarts {
            connection
                .execute(
                    "INSERT INTO collections (id, name, parent_id, kind, query_json, created_ms)
                     VALUES (?1, ?1, NULL, 'smart', ?2, 1)",
                    rusqlite::params![id.as_str(), query],
                )
                .unwrap();
        }
    }
    let service = EditorService::open(&fx.catalog).unwrap();
    let mut events = EventCache::default();
    for (id, _) in &smarts {
        let view = run(
            &service,
            &mut events,
            &ViewQuery::of(ViewSource::Collection {
                collection_id: id.clone(),
            }),
        );
        assert!(view.items.is_empty(), "{id}");
    }
    for direct in [
        ViewQuery::of(ViewSource::CatalogFolder {
            folder_id: gone_folder,
            subfolders: true,
        }),
        ViewQuery::of(ViewSource::Collection {
            collection_id: gone_collection,
        }),
    ] {
        let error = try_run(&service, &mut events, &direct, MAX_VIEW_ITEMS).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation, "{direct:?}");
    }
}

/// The files a source over files covers, whatever a filter would say.
#[test]
fn browse_source_files_lists_what_a_source_covers() {
    let fx = testing::fixture("source-files");
    let service = EditorService::open(&fx.catalog).unwrap();
    let ids = |source: &ViewSource| {
        let mut ids: Vec<_> = fx
            .source_items(source)
            .iter()
            .map(|item| match item.item {
                ViewItem::File(file) => file,
                ViewItem::Photo(_) => unreachable!(),
            })
            .collect();
        ids.sort();
        ids
    };
    let mut sources = file_sources(&fx);
    sources.push(ViewSource::Folder {
        path: format!("{}/", testing::PICTURES).into(),
        subfolders: false,
    });
    for source in sources {
        assert_eq!(
            source_files(&service, &source).unwrap(),
            ids(&source),
            "{source:?}"
        );
    }
    assert_eq!(
        source_files(&service, &ViewSource::AllPhotographs)
            .unwrap_err()
            .kind,
        ErrorKind::Validation
    );
}

/// At the design's scale — 10,000 files in view and 100,000 photographs — views, windows, facets
/// and selection work. Functional only: the timing is measured on the integrated build.
#[test]
fn browse_works_at_the_design_scale() {
    use crate::catalog_types::{
        CameraBody, CaptureTime, CatalogFolder, CatalogFolderId, FileRecord, FileSignature,
        HeaderMetadata, HeaderState, IndexRoot, RootKind, Volume,
    };
    use crate::seed::{CatalogSeeder, IndexSeeder, SeedAsset, SeedKind};
    let dir = luxforge_testbase::paths::temp_dir("browse-scale");
    let catalog = dir.join("catalog.sqlite");
    let ssd = testing::volume("ssd");
    let bodies = [testing::z8("1"), testing::z8("2"), testing::q3()];
    let header = |n: usize| {
        let second = n % 60;
        let minute = (n / 60) % 60;
        let hour = 6 + (n / 3600) % 14;
        let day = 1 + (n / 50_400) % 28;
        HeaderMetadata {
            capture: (!n.is_multiple_of(97)).then(|| {
                CaptureTime::from_exif(
                    &format!("2026:05:{day:02} {hour:02}:{minute:02}:{second:02}"),
                    Some("250"),
                    Some("+02:00"),
                )
                .unwrap()
            }),
            camera: Some::<CameraBody>(bodies[n % 3].clone()),
            lens: Some(
                if n.is_multiple_of(2) {
                    testing::NIKKOR
                } else {
                    testing::SUMMILUX
                }
                .into(),
            ),
            ..HeaderMetadata::default()
        }
    };
    let mut seeder = CatalogSeeder::create(&catalog, testing::CATALOG_ID).unwrap();
    seeder
        .volumes(&[Volume {
            id: ssd.clone(),
            mount_point: "/Volumes/SSD".into(),
            label: "SSD".into(),
            removable: false,
            platform_id: None,
            last_seen_ms: 1,
        }])
        .unwrap();
    let folder = CatalogFolder {
        id: CatalogFolderId::parse("folder-everything-here").unwrap(),
        name: "Everything".into(),
        parent_id: None,
        created_ms: 1,
        event: None,
        count: 0,
        year: None,
    };
    seeder.folders(std::slice::from_ref(&folder)).unwrap();
    const PHOTOS: usize = 100_000;
    for chunk in (0..PHOTOS).collect::<Vec<_>>().chunks(20_000) {
        let assets: Vec<SeedAsset> = chunk
            .iter()
            .map(|n| SeedAsset {
                id: crate::AssetId::parse(format!("asset-{:032x}", n + 1)).unwrap(),
                kind: SeedKind::Jpeg,
                locator: format!("/Volumes/SSD/Developed/{}/P{n:06}.JPG", n / 1000).into(),
                fingerprint: format!("{:064x}", n + 1),
                file_identity: format!("unix:1:{n}"),
                byte_len: 1000,
                width: 6000,
                height: 4000,
                catalog_folder_id: folder.id.clone(),
                volume_id: ssd.clone(),
                developed_ms: 1_000 + *n as i64,
                removed_ms: None,
                availability: FileAvailability::Available,
                checked_ms: 1,
                develop_moment: None,
                header: header(*n),
                place: Some(format!("Place {}", n % 40)),
            })
            .collect();
        seeder.assets(&assets).unwrap();
    }
    seeder.finish().unwrap();
    const FILES: usize = 10_000;
    let root = "/Volumes/SSD/Trip";
    let mut index = IndexSeeder::create(&catalog, testing::CATALOG_ID).unwrap();
    index
        .roots(&[IndexRoot {
            path: root.into(),
            kind: RootKind::Indexed,
            volume_id: ssd.clone(),
            listed_ms: Some(1),
            file_count: Some(FILES as u32),
            offline: false,
        }])
        .unwrap();
    let records: Vec<FileRecord> = (0..FILES)
        .map(|n| {
            let folder = format!("{root}/{:03}", n / 500);
            let name = format!("DSC_{n:05}.NEF");
            FileRecord {
                path: PathBuf::from(&folder).join(&name),
                folder: folder.into(),
                name,
                volume_id: ssd.clone(),
                signature: FileSignature {
                    len: 1,
                    modified_ns: 1,
                    identity: None,
                },
                kind: SourceTag::Raw,
                header: HeaderState::Ok(Box::new(header(n * 5))),
                last_seen_ms: 1,
                born_ns: None,
            }
        })
        .collect();
    index.files(&records).unwrap();
    index.finish().unwrap();
    let service = EditorService::open(&catalog).unwrap();
    let mut events = EventCache::default();
    let trip = ViewQuery::of(ViewSource::Folder {
        path: root.into(),
        subfolders: true,
    });
    let evaluation = run(&service, &mut events, &trip);
    assert_eq!(evaluation.items.len(), FILES);
    assert_eq!(
        evaluation
            .layout
            .days
            .iter()
            .map(|day| day.len)
            .sum::<u32>() as usize,
        FILES
    );
    let view = held(&evaluation, true);
    let window = rows(&service, &view, 5_000, 200, &|items| {
        index_grid_states(&service, items)
    })
    .unwrap();
    assert_eq!(window.len(), 200);
    assert!(
        window
            .iter()
            .enumerate()
            .all(|(at, row)| row.position == 5_000 + at as u32)
    );
    let list = event_list(&service, &mut events, None, None).unwrap();
    assert_eq!(
        list.events
            .iter()
            .map(|event| event.count as usize)
            .sum::<usize>(),
        FILES
    );
    let first = run(
        &service,
        &mut events,
        &ViewQuery::of(ViewSource::Event {
            event_id: list.events[0].id.clone(),
        }),
    );
    assert_eq!(first.items.len(), list.events[0].count as usize);
    let all = ViewQuery {
        sort: ViewSort {
            key: SortKey::CaptureTime,
            descending: true,
        },
        ..ViewQuery::of(ViewSource::AllPhotographs)
    };
    let photos = run(&service, &mut events, &all);
    assert_eq!(photos.items.len(), PHOTOS);
    let view = held(&photos, false);
    let window = rows(&service, &view, 50_000, 200, &|items| {
        index_grid_states(&service, items)
    })
    .unwrap();
    assert_eq!(window.len(), 200);
    let counts = facets(
        Context {
            service: &service,
            events: &mut events,
            probe: Probe::Given(&NoProbe),
            limit: MAX_VIEW_ITEMS,
        },
        &ViewSource::AllPhotographs,
        &ViewFilter::default(),
        &[
            Facet::Date,
            Facet::Place,
            Facet::Camera,
            Facet::Lens,
            Facet::Kind,
        ],
    )
    .unwrap();
    assert_eq!(counts.counts[&Facet::Place].len(), 40);
    assert_eq!(counts.counts[&Facet::Kind][0].count as usize, PHOTOS);
    let mut selection = ViewSelection::default();
    select(
        &service,
        &view,
        &mut selection,
        SelectRequest {
            all: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(selection.count as usize, PHOTOS);
    let named = run(
        &service,
        &mut events,
        &ViewQuery {
            filter: ViewFilter {
                text: Some("p00001".into()),
                ..Default::default()
            },
            ..ViewQuery::of(ViewSource::AllPhotographs)
        },
    );
    assert_eq!(named.items.len(), 10);
    drop(service);
    std::fs::remove_dir_all(dir).unwrap();
}

mod helpers {
    use super::super::*;

    #[test]
    fn browse_subtree_bounds_cover_the_folder_and_nothing_beside_it() {
        let (lower, upper) = subtree_bounds("/a/b");
        assert_eq!(lower, "/a/b/");
        assert_eq!(upper, "/a/b0");
        for inside in ["/a/b", "/a/b/c", "/a/b/c/d.jpg", "/a/b/-"] {
            assert!(within(inside, "/a/b"), "{inside}");
        }
        for outside in ["/a/b-x", "/a/bc", "/a", "/a/b0", "/a/c"] {
            assert!(!within(outside, "/a/b"), "{outside}");
        }
        assert_eq!(subtree_bounds("/").0, "/");
        assert!(within("/x", "/"));
    }

    #[test]
    fn browse_local_days_follow_the_cameras_clock() {
        // 2026-09-12T23:30 at +02:00 is 21:30 UTC the same day; at -05:00 it is the next day in UTC
        // but still the 12th on the camera.
        let day = LocalDay::from_ymd(2026, 9, 12).unwrap();
        let local = i64::from(day.0) * 86_400_000 + (23 * 60 + 30) * 60_000;
        assert_eq!(local_day(local - 120 * 60_000, Some(120)), day);
        assert_eq!(local_day(local + 300 * 60_000, Some(-300)), day);
        assert_eq!(local_day(local, None), day);
        assert_eq!(caseless("dsc_0001.JPG", "DSC_0001.jpg"), Ordering::Equal);
        assert_eq!(caseless("a", "B"), Ordering::Less);
    }
}
