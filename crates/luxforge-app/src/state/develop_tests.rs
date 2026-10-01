//! The confirmation and the development set as plain data: what the confirmation proposes and
//! says, the `pick.develop` it sends for each choice — new and existing folders, a card's copies
//! used or not — the set's steps and window, and the model the views draw.
use super::*;
use luxforge_core::catalog_types::{
    CatalogFolder, DevelopOutcome, DevelopedPick, EventId, ItemFailure, LibraryChangeSeq,
    RemovablePicks, VolumeId,
};
use std::path::{Path, PathBuf};

fn event(
    name: &str,
    count: u32,
    folder: FolderChoice,
    removable: Vec<RemovablePicks>,
) -> PlannedEvent {
    PlannedEvent {
        event_id: Some(EventId::of(Path::new(&format!("/photos/{name}")), 1_000)),
        name: name.into(),
        count,
        folder,
        folder_name: None,
        removable,
    }
}

fn new_folder(name: &str) -> FolderChoice {
    FolderChoice::New {
        name: name.into(),
        parent_id: None,
    }
}

fn card(count: u32, with_copy: u32) -> RemovablePicks {
    RemovablePicks {
        volume_id: VolumeId::parse("volume-0123456789").unwrap(),
        label: "NIKON Z 8".into(),
        count,
        with_copy,
    }
}

fn folder(name: &str, parent: Option<&CatalogFolderId>) -> CatalogFolder {
    CatalogFolder {
        id: CatalogFolderId::new(),
        name: name.into(),
        parent_id: parent.cloned(),
        created_ms: 1,
        event: None,
        count: 3,
        year: Some(2026),
    }
}

fn confirmation(events: Vec<PlannedEvent>, folders: Vec<CatalogFolder>) -> Confirmation {
    let count = events.iter().map(|event| event.count).sum();
    Confirmation::new(
        DevelopPlan {
            events,
            count,
            offline: 0,
        },
        CatalogFolders { folders },
        "Konstanz".into(),
    )
}

fn mutation() -> MutationRequest {
    MutationRequest {
        request_id: "desktop-1".into(),
        actor: "desktop".into(),
    }
}

/// One event proposing a new folder: its name is the field's, Develop sends exactly the plan's
/// folder by the event's identity, and the request is what an agent writes for the same choice.
#[test]
fn filmstrip_confirmation_proposes_the_plans_new_folder() {
    let plan_event = event(
        "Konstanz",
        18,
        new_folder("Konstanz \u{b7} Sep 2026"),
        vec![],
    );
    let event_id = plan_event.event_id.clone();
    let confirm = confirmation(vec![plan_event], vec![]);
    let model = confirm_model(&confirm);
    assert_eq!(model.title, "Develop 18 picks");
    assert_eq!(model.from, "from Konstanz");
    assert_eq!(model.heading, "Into the catalog folder");
    assert_eq!(model.events.len(), 1);
    let row = &model.events[0];
    assert_eq!(row.label, None, "one event needs no label");
    assert_eq!(row.field.as_deref(), Some("Konstanz \u{b7} Sep 2026"));
    assert_eq!(row.tag, "new folder");
    assert_eq!((row.existing.as_ref(), row.menu.as_ref()), (None, None));
    assert_eq!(model.notes, Vec::<String>::new());
    assert_eq!(model.copies, None);
    assert_eq!(model.refusal, None);
    assert_eq!(
        develop_params(&confirm, &mutation()),
        json!({
            "into": [{"event_id": event_id, "folder": {"kind": "new", "name": "Konstanz \u{b7} Sep 2026"}}],
            "mutation": {"request_id": "desktop-1", "actor": "desktop"},
        })
    );
}

/// Typing names the new folder (trimmed when sent); an empty name refuses Develop with its reason;
/// choosing an existing folder sends it by identity, listed and shown by its path.
#[test]
fn filmstrip_confirmation_renames_or_chooses_an_existing_folder() {
    let travel = folder("Travel", None);
    let alps = folder("Alps", Some(&travel.id));
    let mut confirm = confirmation(
        vec![event("Lake", 2, new_folder("Lake \u{b7} Sep 2026"), vec![])],
        vec![travel.clone(), alps.clone()],
    );
    confirm.events[0].name = "  Lake trip ".into();
    assert_eq!(
        develop_params(&confirm, &mutation())["into"][0]["folder"],
        json!({"kind": "new", "name": "Lake trip"})
    );
    confirm.events[0].name = "   ".into();
    assert_eq!(
        confirm_model(&confirm).refusal.as_deref(),
        Some("Name the new catalog folder")
    );
    confirm.events[0].name = "x".repeat(MAX_LIBRARY_NAME + 1);
    assert!(confirm.refusal().is_some());
    confirm.menu = Some(0);
    let menu = confirm_model(&confirm).events[0]
        .menu
        .clone()
        .expect("open");
    assert_eq!(
        menu.iter()
            .map(|(_, label)| label.as_str())
            .collect::<Vec<_>>(),
        ["Travel", "Travel \u{203a} Alps"]
    );
    confirm.events[0].existing = Some(alps.id.clone());
    confirm.menu = None;
    let model = confirm_model(&confirm);
    let row = &model.events[0];
    assert_eq!(row.field, None);
    assert_eq!(row.existing.as_deref(), Some("Travel \u{203a} Alps"));
    assert_eq!(row.tag, "existing folder");
    assert_eq!(model.refusal, None, "an existing folder needs no name");
    assert_eq!(
        develop_params(&confirm, &mutation())["into"][0]["folder"],
        json!({"kind": "existing", "folder_id": alps.id})
    );
}

/// Picks from an event that already has a folder default to it; several events are listed with
/// their counts, each into its own folder.
#[test]
fn filmstrip_confirmation_lists_several_events_and_an_earlier_folder() {
    let earlier = folder("Konstanz \u{b7} Sep 2026", None);
    let mut proposed = event(
        "Konstanz",
        12,
        FolderChoice::Existing {
            folder_id: earlier.id.clone(),
        },
        vec![],
    );
    proposed.folder_name = Some(earlier.name.clone());
    let confirm = confirmation(
        vec![
            proposed,
            event(
                "Brighton",
                1,
                new_folder("Brighton \u{b7} Sep 2026"),
                vec![],
            ),
        ],
        vec![earlier.clone()],
    );
    let model = confirm_model(&confirm);
    assert_eq!(model.title, "Develop 13 picks");
    assert_eq!(model.heading, "Into catalog folders");
    let labels: Vec<_> = model
        .events
        .iter()
        .map(|row| row.label.clone().unwrap())
        .collect();
    assert_eq!(
        labels,
        ["Konstanz \u{b7} 12 picks", "Brighton \u{b7} 1 pick"]
    );
    assert_eq!(
        model.events[0].existing.as_deref(),
        Some("Konstanz \u{b7} Sep 2026")
    );
    assert_eq!(
        model.events[1].field.as_deref(),
        Some("Brighton \u{b7} Sep 2026")
    );
    let into = &develop_params(&confirm, &mutation())["into"];
    assert_eq!(into[0]["folder"]["kind"], "existing");
    assert_eq!(into[1]["folder"]["kind"], "new");
    assert_ne!(into[0]["event_id"], into[1]["event_id"]);
}

/// A card's picks: their copies used once the fingerprints match when every one has a copy; the
/// card confirmed for those without one; and every one from the card when the copies are not used.
/// Offline picks are said to wait.
#[test]
fn filmstrip_confirmation_says_what_happens_to_card_picks() {
    let with = |removable: RemovablePicks, offline: u32| {
        let mut confirm = confirmation(
            vec![event(
                "Konstanz",
                18,
                new_folder("Konstanz"),
                vec![removable],
            )],
            vec![],
        );
        confirm.plan.offline = offline;
        confirm
    };
    let all_copied = with(card(12, 12), 0);
    let model = confirm_model(&all_copied);
    assert_eq!(
        model.notes,
        [
            "12 picks are on NIKON Z 8. Their copies in indexed folders will be used once the \
          fingerprints match."
        ]
    );
    assert_eq!(model.copies, Some(true));
    let params = develop_params(&all_copied, &mutation());
    assert_eq!(params["use_copies"], true);
    assert!(params.get("confirm_removable").is_none());

    let mut card_only = with(card(12, 12), 0);
    card_only.use_copies = false;
    assert_eq!(
        confirm_model(&card_only).notes,
        ["12 picks are on NIKON Z 8. They are developed from it, and the catalog points at it."]
    );
    let params = develop_params(&card_only, &mutation());
    assert!(params.get("use_copies").is_none());
    assert_eq!(params["confirm_removable"], true);

    let none_copied = with(card(3, 0), 2);
    let model = confirm_model(&none_copied);
    assert_eq!(model.copies, None, "nothing to use");
    assert_eq!(
        model.notes,
        [
            "3 picks are on NIKON Z 8. They are developed from it, and the catalog points at it.",
            "2 picks are offline: they stay picked until their volume is connected.",
        ]
    );
    let params = develop_params(&none_copied, &mutation());
    assert!(params.get("use_copies").is_none());
    assert_eq!(params["confirm_removable"], true);

    let mixed = with(card(12, 8), 0);
    assert_eq!(
        confirm_model(&mixed).notes,
        [
            "12 picks are on NIKON Z 8. 8 have copies in indexed folders, used once the \
          fingerprints match; the other 4 are developed from it, and the catalog points at it."
        ]
    );
    let params = develop_params(&mixed, &mutation());
    assert_eq!(params["use_copies"], true);
    assert_eq!(params["confirm_removable"], true);
}

fn developed(name: &str, asset: &AssetId, used: Option<&str>) -> DevelopedPick {
    DevelopedPick {
        path: PathBuf::from(format!("/Volumes/CARD/DCIM/{name}")),
        used: used.map(PathBuf::from),
        asset_id: asset.clone(),
        outcome: DevelopOutcome::Created,
    }
}

/// The set a Develop answers is its photographs in order, each once, named by the file each was
/// developed from; the sentence counts them and names the first failure.
#[test]
fn filmstrip_set_is_what_the_develop_answered() {
    let (a, b) = (AssetId::new(), AssetId::new());
    let report = DevelopReport {
        developed: vec![
            developed("DSC_0001.JPG", &a, Some("/Users/x/Card dumps/DSC_0001.JPG")),
            developed("DSC_0002.JPG", &b, None),
            developed("COPY_0002.JPG", &b, None),
        ],
        failed: vec![],
        changes: vec![LibraryChangeSeq(4)],
    };
    let photos = developed_photos(&report);
    assert_eq!(
        photos,
        [
            SetPhoto {
                asset_id: a.clone(),
                name: "DSC_0001.JPG".into()
            },
            SetPhoto {
                asset_id: b.clone(),
                name: "DSC_0002.JPG".into()
            },
        ]
    );
    assert_eq!(developed_sentence(&report), "Developed 2");
    let failed = DevelopReport {
        failed: vec![ItemFailure {
            path: "/Volumes/CARD/DCIM/DSC_0003.JPG".into(),
            asset_id: None,
            code: "source-unavailable".into(),
            message: "the volume CARD is not connected".into(),
        }],
        ..report.clone()
    };
    assert_eq!(
        developed_sentence(&failed),
        "Developed 2 \u{b7} 1 could not be developed \u{b7} DSC_0003.JPG: the volume CARD is not \
         connected"
    );
    let nothing = DevelopReport {
        developed: vec![],
        ..failed
    };
    assert_eq!(
        developed_sentence(&nothing),
        "Could not develop the picks \u{b7} DSC_0003.JPG: the volume CARD is not connected"
    );
}

fn photos(count: usize) -> Vec<SetPhoto> {
    (0..count)
        .map(|at| SetPhoto {
            asset_id: AssetId::new(),
            name: format!("IMG_{at:04}.JPG"),
        })
        .collect()
}

/// `←` and `→` move one photograph and stop at either end; the strip's window moves as little as
/// keeps the active photograph in it, and never past the set's end.
#[test]
fn filmstrip_steps_and_its_window_follow_the_active_photograph() {
    let mut set = DevelopSet::new(1, photos(20), 0);
    assert_eq!(set.step(-1), None);
    assert_eq!(set.step(1), Some(1));
    set.reveal(8);
    assert_eq!(set.first, 0);
    set.active = 9;
    set.reveal(8);
    assert_eq!(set.first, 2, "the active cell is the window's last");
    set.active = 1;
    set.reveal(8);
    assert_eq!(set.first, 1, "the active cell is the window's first");
    set.active = 19;
    assert_eq!(set.step(1), None);
    set.reveal(8);
    assert_eq!(set.first, 12);
    set.reveal(30);
    assert_eq!(
        set.first, 0,
        "a window wider than the set starts at its first"
    );
    let opened = DevelopSet::new(2, photos(3), 9);
    assert_eq!(opened.active, 2, "clamped to the set");
}

/// The model: the strip's window of cells with its active photograph and buttons, nothing while
/// collapsed; Develop N busy with the Develop's progress; the render slot's words for a preview.
#[test]
fn filmstrip_model_draws_the_window_of_the_set() {
    let set = DevelopSet::new(3, photos(12), 5);
    let mut state = DevelopState {
        set: Some(set.clone()),
        capacity: 4,
        ..DevelopState::default()
    };
    let model = derive(&state, None);
    let strip = model.strip.expect("a strip");
    assert_eq!(strip.first, 5);
    assert_eq!(
        strip.cells,
        set.photos[5..9]
            .iter()
            .map(|photo| photo.asset_id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!((strip.total, strip.active), (12, Some(5)));
    assert!(strip.can_previous && strip.can_next);
    assert_eq!(strip.title, "Development set");
    assert_eq!(strip.caption, None, "the widget says the place in the set");
    assert!(state.strip_shown());
    state.collapsed = true;
    assert!(derive(&state, None).strip.is_none());
    assert!(!state.strip_shown());

    state.developing = Some(Developing {
        job: Some("job-1".into()),
        total: 18,
    });
    assert_eq!(derive(&state, Some(7.0 / 18.0)).busy, Some((7, 18)));
    assert_eq!(derive(&state, None).busy, Some((0, 18)));

    let preview = ShownPreview {
        asset: set.photos[5].asset_id.clone(),
        entry: None,
        name: "IMG_0005.JPG".into(),
        origin: PreviewOrigin::Rendered,
        approximate: true,
        size: (2048, 1365),
        version: 3,
        key: "photo:a:e:large:r1".into(),
    };
    assert_eq!(preview.render_text(), "Cached preview \u{b7} approximate");
    assert_eq!(
        preview.status_text(),
        "Showing IMG_0005.JPG \u{b7} cached preview while the original prepares"
    );
    let camera = ShownPreview {
        origin: PreviewOrigin::Embedded,
        approximate: false,
        ..preview.clone()
    };
    assert_eq!(camera.render_text(), "Camera preview");
    state.preview = Some(preview);
    assert_eq!(
        derive(&state, None).render.as_deref(),
        Some("Cached preview \u{b7} approximate")
    );
}
