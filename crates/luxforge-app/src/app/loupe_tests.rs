//! The loupe against a real owner over the Select tests' seeded catalog (a Nikon burst of three,
//! singles, a Leica bracket of three, and a second day of singles): opened over the grid, the keys
//! the loupe answers, stepping frames and moments and jumping within a moment through the session's
//! own selection, the look-ahead wanted in the direction of travel, compare and the focus check,
//! `P`'s hook and P7, and back to the grid. And over a catalog of imported photographs: the strip
//! reads and draws each photograph's grid tier, while a file's strip frame is the grid's preview.
use super::*;
use crate::app::select_owner_tests::{evaluate, finish, read_rows, selecting};
use crate::state::select::SourcePress;
use iced::{Size, event::Status, keyboard::Event as KeyEvent, keyboard::key::Physical};
use luxforge_core::catalog_types::MomentKind;
use std::path::PathBuf;

/// The editor with the seeded event viewed, every row read.
fn viewing() -> (Editor, PathBuf) {
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
    (editor, catalog)
}

fn send(editor: &mut Editor, message: LoupeMessage) {
    let _ = editor.update(loupe_message(message));
}

fn active(editor: &Editor) -> Option<u32> {
    editor.session.browse.selection.active
}

/// The first frame of the view's moment of `kind`, and its length.
fn moment(editor: &Editor, kind: MomentKind) -> (u32, u32) {
    let summary = editor.select.state.summary.as_ref().expect("a view");
    let moment = summary
        .groups
        .moments
        .iter()
        .find(|moment| moment.kind == kind)
        .unwrap_or_else(|| panic!("a {kind:?} in {:?}", summary.groups.moments));
    (moment.start, moment.len)
}

/// A key pressed through the key table, as the keyboard sends it.
fn key(editor: &Editor, key: Key, repeat: bool) -> Option<String> {
    let event = iced::Event::Keyboard(KeyEvent::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
        location: iced::keyboard::Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat,
    });
    crate::app::keymap::keymap(&event, Status::Ignored, &editor.key_context())
        .map(|message| format!("{message:?}"))
}

/// The loupe opens over the view's first frame when none is active; the arrows step the view's
/// frames and moments through `browse.select`, each in its own update, as the session reports
/// them; `1`–`9` jump within a moment only; the look-ahead follows the direction of travel.
#[test]
fn loupe_steps_frames_and_moments_through_the_session() {
    let (mut editor, catalog) = viewing();
    let (burst, burst_len) = moment(&editor, MomentKind::Burst);
    let (bracket, bracket_len) = moment(&editor, MomentKind::Bracket);
    assert_eq!((burst_len, bracket_len), (3, 3));
    send(&mut editor, LoupeMessage::Open);
    assert!(editor.loupe_open());
    assert_eq!(active(&editor), Some(0), "the view's first frame, selected");
    let model = &editor.workspace.select.loupe;
    assert_eq!(model.subject.unwrap().position, 0);
    // The keys the loupe answers while it is open.
    for (pressed, expected) in [
        (Key::Named(Named::ArrowRight), "Frame(Forward)"),
        (Key::Named(Named::ArrowLeft), "Frame(Back)"),
        (Key::Named(Named::ArrowDown), "Moment(Forward)"),
        (Key::Named(Named::ArrowUp), "Moment(Back)"),
        (Key::Character("3".into()), "Jump(2)"),
        (Key::Character("z".into()), "ToggleFocus"),
        (Key::Character("c".into()), "ToggleCompare"),
        (Key::Character("p".into()), "Pick"),
    ] {
        assert_eq!(
            key(&editor, pressed, false).as_deref(),
            Some(format!("Select(Loupe({expected}))").as_str())
        );
    }
    assert!(
        key(&editor, Key::Named(Named::ArrowRight), true).is_some(),
        "a held arrow repeats"
    );
    assert_eq!(key(&editor, Key::Character("z".into()), true), None);
    assert_eq!(key(&editor, Key::Character("0".into()), false), None);

    // Down to the burst: every stop on the way is a moment or a single frame.
    let mut stops = Vec::new();
    while active(&editor) != Some(burst) {
        send(&mut editor, LoupeMessage::Moment(Travel::Forward));
        stops.push(active(&editor).unwrap());
        assert!(stops.len() < 12, "never reached the burst: {stops:?}");
    }
    let session = crate::app::select::session_now(&editor.owner, editor.client).unwrap();
    assert_eq!(
        session.browse.selection.active,
        Some(burst),
        "the owner's own"
    );
    let model = &editor.workspace.select.loupe;
    assert!(
        model.info.moment.ends_with("\u{b7} burst"),
        "{:?}",
        model.info
    );
    assert!(
        model.info.frame.starts_with("Frame 1 of 3"),
        "{:?}",
        model.info
    );
    assert_eq!(model.strip.as_ref().unwrap().frames.len(), 3);
    assert_eq!(model.frame.as_ref().unwrap().name, "DSC_0001.NEF");
    // The look-ahead: the next three frames, which reach the next moment's first, nearest first.
    let wants = editor.loupe_wants();
    let positions: Vec<u32> = model::wanted(&editor.select.state, &editor.session.browse)
        .iter()
        .map(|frame| frame.position)
        .collect();
    assert_eq!(positions, vec![burst, burst + 1, burst + 2, burst + 3]);
    assert!(wants[0].shown && wants[1..].iter().all(|want| !want.shown));
    // Frames across the burst's end, and a jump back within it.
    send(&mut editor, LoupeMessage::Frame(Travel::Forward));
    send(&mut editor, LoupeMessage::Frame(Travel::Forward));
    assert_eq!(active(&editor), Some(burst + 2));
    assert!(
        editor
            .workspace
            .select
            .loupe
            .info
            .frame
            .starts_with("Frame 3 of 3 \u{b7} +0.60 s"),
        "{:?}",
        editor.workspace.select.loupe.info
    );
    send(&mut editor, LoupeMessage::Jump(0));
    assert_eq!(
        active(&editor),
        Some(burst),
        "1 jumps to the moment's first frame"
    );
    send(&mut editor, LoupeMessage::Jump(5));
    assert_eq!(
        active(&editor),
        Some(burst),
        "past the moment's frames: nothing"
    );
    send(&mut editor, LoupeMessage::Frame(Travel::Back));
    assert_eq!(editor.select.state.loupe.travel, Travel::Back);
    let back: Vec<u32> = model::wanted(&editor.select.state, &editor.session.browse)
        .iter()
        .map(|frame| frame.position)
        .collect();
    assert!(back.windows(2).all(|pair| pair[0] > pair[1]), "{back:?}");
    // Up to the bracket and back down: moments either way.
    while active(&editor) != Some(bracket) {
        send(&mut editor, LoupeMessage::Moment(Travel::Forward));
    }
    assert!(
        editor
            .workspace
            .select
            .loupe
            .info
            .moment
            .ends_with("\u{b7} bracket")
    );
    send(&mut editor, LoupeMessage::Moment(Travel::Back));
    assert!(active(&editor) < Some(bracket));
    // Esc: back to the grid on the active frame, the loupe's frames released.
    let at = active(&editor);
    send(&mut editor, LoupeMessage::Close);
    assert!(!editor.loupe_open());
    assert_eq!(
        active(&editor),
        at,
        "the grid shows the frame the loupe left"
    );
    assert_eq!(editor.select.loupe.frames.summary()["wanted"], 0);
    finish(editor, catalog);
}

/// Compare shows up to four frames of a moment and nothing for a single, which the status bar
/// says; the focus check asks for the rectangle under the pointer in the frame's upright header
/// size, and the two are exclusive. `P` picks nothing until lane D's picks land, and says so;
/// P7 moves on from a picked burst frame to the next moment, and not from a bracket's.
#[test]
fn loupe_compares_checks_focus_and_moves_on_after_a_burst_pick() {
    let (mut editor, catalog) = viewing();
    let (burst, _) = moment(&editor, MomentKind::Burst);
    let (bracket, _) = moment(&editor, MomentKind::Bracket);
    send(&mut editor, LoupeMessage::Open);
    // A single frame has nothing to compare.
    let single = (0..12)
        .find(|at| {
            let summary = editor.select.state.summary.as_ref().unwrap();
            unit_of(summary, *at).moment.is_none()
        })
        .unwrap();
    assert!(editor.loupe_select(single));
    send(&mut editor, LoupeMessage::ToggleCompare);
    assert!(!editor.select.state.loupe.compare);
    assert!(
        editor.status.text.contains("single frame"),
        "{}",
        editor.status.text
    );
    // The burst's three frames side by side, the active one marked, each its own frame.
    assert!(editor.loupe_select(burst + 1));
    send(&mut editor, LoupeMessage::ToggleCompare);
    let compare = &editor.workspace.select.loupe.compare;
    assert_eq!(compare.len(), 3);
    assert_eq!(
        compare.iter().map(|cell| cell.position).collect::<Vec<_>>(),
        vec![burst, burst + 1, burst + 2]
    );
    assert!(compare[1].active && !compare[0].active);
    assert_eq!(
        editor
            .loupe_wants()
            .iter()
            .filter(|want| want.shown)
            .count(),
        3
    );
    // Z: the focus check replaces compare; its rectangle is in the header's 6000 × 4000 frame.
    send(&mut editor, LoupeMessage::ToggleFocus);
    assert!(editor.select.state.loupe.focus && !editor.select.state.loupe.compare);
    let request = editor
        .loupe_region()
        .expect("a rectangle under the pointer");
    assert_eq!(request.frame.width, 6000);
    assert_eq!(
        request.item,
        editor.workspace.select.loupe.frame.as_ref().unwrap().item
    );
    assert!(editor.select.loupe.focus.pending(), "the region is out");
    send(&mut editor, LoupeMessage::Pointer(Some((0.1, 0.1))));
    let moved = editor.loupe_region().unwrap();
    assert!(moved.rect.x < request.rect.x, "it follows the pointer");
    assert!(!editor.loupe_settled(), "the region has not landed");
    send(&mut editor, LoupeMessage::ToggleFocus);
    assert!(editor.loupe_region().is_none());
    // P: the active frame picked through Select's own pick (`pick.set` naming its file), answered
    // in this update; a burst's pick moves on to the next moment's first frame (P7).
    assert_eq!(active(&editor), Some(burst + 1));
    send(&mut editor, LoupeMessage::Pick);
    let sent = editor.select.library.clone().expect("a library request");
    assert_eq!(sent["method"], "pick.set", "{sent}");
    assert_eq!(sent["params"]["picked"], true, "{sent}");
    assert!(sent["error"].is_null(), "{sent}");
    assert_eq!(
        active(&editor),
        Some(burst + 3),
        "the next moment's first frame"
    );
    evaluate(&mut editor);
    read_rows(&mut editor);
    assert_eq!(
        active(&editor),
        Some(burst + 3),
        "kept once the view is read again"
    );
    assert!(
        editor
            .select
            .state
            .rows
            .row(burst + 1)
            .is_some_and(|row| row.picked),
        "the burst's frame is picked"
    );
    assert!(editor.loupe_select(bracket + 1));
    editor.loupe_picked(&[bracket + 1], true, true);
    assert_eq!(
        active(&editor),
        Some(bracket + 1),
        "a bracket's frame stays"
    );
    assert!(editor.loupe_select(burst));
    editor.loupe_picked(&[burst], false, true);
    editor.loupe_picked(&[burst], true, false);
    editor.loupe_picked(&[burst + 2], true, true);
    assert_eq!(
        active(&editor),
        Some(burst),
        "a clear, a failure, another frame's pick"
    );
    finish(editor, catalog);
}

/// The loupe builds in each of its states: nothing to show, a frame reading, a frame with the
/// focus check, and compare.
#[test]
fn loupe_view_builds_in_every_state() {
    use crate::state::loupe::{
        Area, FocusModel, FrameModel, InfoText, LoupeModel, StripFrame, StripModel,
    };
    use luxforge_core::catalog_types::{Dimensions, FileId, PixelRect};
    let loupe = crate::app::loupe::Loupe::default();
    let grid = crate::app::select_previews::SelectPreviews::default();
    let images = loupe.images(&grid);
    let rect = Area {
        x: 200.0,
        y: 100.0,
        width: 900.0,
        height: 600.0,
    };
    let frame = |position: u32, active: bool| FrameModel {
        position,
        item: PreviewItem::File {
            file_id: FileId(i64::from(position)),
        },
        name: format!("DSC_{position:04}.JPG"),
        number: position + 1,
        active,
        picture: None,
        rect,
        note: Some("Reading the preview\u{2026}".into()),
    };
    let empty = LoupeModel {
        open: true,
        info: InfoText {
            source: "Choose a frame in the grid".into(),
            ..InfoText::default()
        },
        ..LoupeModel::default()
    };
    let _ = loupe_view(&empty, images);
    let single = LoupeModel {
        frame: Some(frame(1, true)),
        strip: Some(StripModel {
            first: 0,
            frames: (0..3)
                .map(|position| StripFrame {
                    position,
                    item: None,
                    thumbnail: None,
                    picked: position == 1,
                })
                .collect(),
            active: 1,
            start: 0,
            previous: false,
            next: true,
        }),
        hints: vec![("Z".into(), "100%".into())],
        focus: Some(FocusModel {
            rect: PixelRect {
                x: 0,
                y: 0,
                width: 616,
                height: 418,
            },
            frame: Dimensions {
                width: 6000,
                height: 4000,
            },
            region_box: Area {
                x: 10.0,
                y: 10.0,
                width: 92.0,
                height: 62.0,
            },
            inset: Area {
                x: 776.0,
                y: 451.0,
                width: 308.0,
                height: 233.0,
            },
            region: None,
            region_points: (308.0, 209.0),
            developed: true,
            pending: true,
            error: None,
        }),
        ..empty.clone()
    };
    let _ = loupe_view(&single, images);
    let compare = LoupeModel {
        compare: (0..4)
            .map(|position| frame(position, position == 1))
            .collect(),
        ..single.clone()
    };
    let _ = loupe_view(&compare, images);
}

fn loupe_view<'a>(
    model: &'a crate::state::loupe::LoupeModel,
    images: LoupeImages<'a>,
) -> iced::Element<'a, Message> {
    crate::view::loupe::loupe(model, images)
}

/// Photograph `name`, at its current entry.
fn photo(name: &str) -> PreviewItem {
    PreviewItem::Photo {
        asset_id: luxforge_core::AssetId::parse(format!("asset-photo-{name}-0000")).unwrap(),
        entry_id: None,
    }
}

/// A strip frame's picture: a file's is the Select grid's preview of it, a photograph's the loupe's
/// own thumbnail, lent only under the photograph it was read for and only as the picture the model
/// names for it.
#[test]
fn loupe_strip_borrows_the_grids_files_and_holds_its_own_photographs() {
    use crate::app::select_previews::{self as grid_previews, SelectPreviews, Wanted};
    use crate::state::loupe::StripFrame;
    use luxforge_core::{
        DecodedPreview,
        catalog_types::{
            FileId, PreviewAnswer, PreviewInfo, PreviewOrigin, PreviewState, PreviewTier,
        },
    };
    let pixels = |width: u32, height: u32| DecodedPreview {
        width,
        height,
        rgba: vec![0; (width * height * 4) as usize],
    };
    let info = |item: PreviewItem, key: &str| PreviewInfo {
        item,
        tier: PreviewTier::Grid,
        path: PathBuf::from(format!("/c.index/previews/{key}.jpg")),
        width: 512,
        height: 341,
        origin: PreviewOrigin::Embedded,
        bytes: 40_000,
        key: key.into(),
        approximate: false,
    };
    // The grid holds file 7's preview, as it does for the files near the active frame.
    let file = FileId(7);
    let mut grid = SelectPreviews::default();
    grid.paused = true;
    let batch = grid
        .plan_for(Wanted {
            revision: 1,
            visible: vec![(file, PreviewState::Ready)],
            margin: Vec::new(),
            side: 240,
        })
        .expect("the grid reads file 7");
    grid.answered(grid_previews::ReadAnswers {
        serial: batch.serial,
        revision: 1,
        answers: vec![(
            file,
            Ok(PreviewAnswer::Ready {
                preview: info(PreviewItem::File { file_id: file }, "file-7-grid"),
            }),
        )],
    });
    let decode = grid.last_plan[0].clone();
    grid.adopt(grid_previews::Decoded {
        decode,
        result: Ok(pixels(240, 160)),
    });
    // The loupe holds photograph a's thumbnail.
    let (a, b) = (photo("a"), photo("b"));
    let mut loupe = Loupe::default();
    loupe.frames.paused = true;
    let batch = loupe
        .frames
        .want(vec![Want {
            slot: Slot::thumbnail(a.clone()),
            pixels: (232, 152),
            shown: true,
        }])
        .expect("the loupe reads photograph a's grid tier");
    loupe.frames.answered(loupe_frames::ReadAnswers {
        serial: batch.serial,
        answers: vec![(
            Slot::thumbnail(a.clone()),
            Ok(PreviewAnswer::Ready {
                preview: info(a.clone(), "photo-a-grid"),
            }),
        )],
    });
    let decode = loupe.frames.planned()[0].clone();
    loupe.frames.adopt(loupe_frames::Decoded {
        decode,
        result: Ok(pixels(229, 152)),
    });
    let own = loupe
        .frames
        .held(&Slot::thumbnail(a.clone()))
        .picture
        .expect("held");
    let images = loupe.images(&grid);
    let strip = |item: Option<PreviewItem>, thumbnail: Option<&Picture>| StripFrame {
        position: 0,
        item,
        thumbnail: thumbnail.cloned(),
        picked: false,
    };
    let file_frame = |id| {
        Some(PreviewItem::File {
            file_id: FileId(id),
        })
    };
    assert!(
        images.thumbnail(&strip(file_frame(7), None)).is_some(),
        "a file's is the grid's"
    );
    assert!(images.thumbnail(&strip(file_frame(8), None)).is_none());
    assert!(
        images
            .thumbnail(&strip(file_frame(8), Some(&own)))
            .is_none(),
        "a file's frame never draws the loupe's"
    );
    assert!(
        images
            .thumbnail(&strip(Some(a.clone()), Some(&own)))
            .is_some(),
        "a photograph's own"
    );
    assert!(
        images.thumbnail(&strip(Some(a.clone()), None)).is_none(),
        "nothing while it is read"
    );
    assert!(
        images
            .thumbnail(&strip(Some(b.clone()), Some(&own)))
            .is_none(),
        "never under another photograph"
    );
    assert!(
        images
            .thumbnail(&strip(
                Some(b.clone()),
                Some(&Picture {
                    item: b.clone(),
                    ..own.clone()
                })
            ))
            .is_none(),
        "nor for another photograph named with its preview"
    );
    assert!(images.thumbnail(&strip(None, Some(&own))).is_none());
    // The strip builds with both.
    let model = crate::state::loupe::LoupeModel {
        open: true,
        strip: Some(crate::state::loupe::StripModel {
            first: 0,
            frames: vec![
                strip(file_frame(7), None),
                strip(Some(a.clone()), Some(&own)),
            ],
            active: 1,
            start: 0,
            previous: false,
            next: false,
        }),
        ..crate::state::loupe::LoupeModel::default()
    };
    let _ = loupe_view(&model, images);
}

/// The editor over a new catalog of `count` photographs, each imported from a copy of the test
/// JPEG, Select showing All photographs with every row read.
fn developed(count: usize) -> (Editor, PathBuf) {
    use crate::{
        Config,
        app::{Boot, tasks::call_detailed},
        state::select::Shown,
    };
    use luxforge_core::{EditorService, OwnerHandle, catalog_types::ViewSource};
    let dir = luxforge_testbase::paths::temp_dir("loupe-photographs")
        .canonicalize()
        .unwrap();
    let catalog = dir.join("catalog.sqlite");
    drop(EditorService::open(&catalog).unwrap());
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let photos = dir.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    for at in 0..count {
        let path = photos.join(format!("P{at:03}.jpg"));
        std::fs::copy(luxforge_testbase::paths::jpeg(), &path).unwrap();
        let started = call_detailed(
            &owner,
            client,
            "catalog.import",
            json!({
                "path": path,
                "mutation": {"request_id": format!("loupe-import-{at}"), "actor": "setup"},
            }),
        )
        .unwrap();
        let ended = luxforge_testbase::wait_for("an import to end", || {
            let read = call_detailed(
                &owner,
                client,
                "job.read",
                json!({"job_id": started["job_id"]}),
            )
            .unwrap();
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        });
        assert_eq!(ended["status"], "ready", "{ended}");
    }
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
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        1000.0, 700.0,
    ))));
    let _ = editor.update(Message::Select(SelectMessage::Source(
        ViewSource::AllPhotographs,
    )));
    evaluate(&mut editor);
    read_rows(&mut editor);
    (editor, catalog)
}

/// Over a view of developed photographs, a real owner and its preview lane: the loupe's strip asks
/// for each of its photographs' grid tier at the visible cells' priority beside the frames' large
/// tiers, and once the lane has rendered them and the worker decoded them, each strip frame draws
/// its own photograph's rendered grid tier, which the evidence records; stepping on asks for the
/// next frame's. The reads and decodes run as the loupe's tasks would, the owner's wake read from
/// the client's one wake.
#[test]
fn loupe_strip_over_photographs_reads_and_draws_their_grid_tiers_on_a_real_owner() {
    use luxforge_core::catalog_types::{PreviewOrigin, PreviewPriority, PreviewTier};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let (mut editor, catalog) = developed(3);
    assert_eq!(
        editor.select.state.summary.as_ref().map(|view| view.count),
        Some(3)
    );
    // This test drives the loupe's reads itself; the owner wakes it through the client's one wake.
    let woke = Arc::new(AtomicBool::new(false));
    let flag = woke.clone();
    editor.owner.watch_previews(
        editor.client,
        Arc::new(move || flag.store(true, Ordering::Release)),
    );
    send(&mut editor, LoupeMessage::Open);
    assert!(editor.loupe_open());
    for step in 0..2 {
        assert_eq!(active(&editor), Some(step));
        let wants = editor.loupe_wants();
        let strip = editor
            .workspace
            .select
            .loupe
            .strip
            .clone()
            .expect("a strip");
        let thumbnails: Vec<&Want> = wants
            .iter()
            .filter(|want| want.slot.role == Role::Thumbnail)
            .collect();
        assert!(
            strip.frames.iter().any(|frame| frame.position == step),
            "step {step}: the strip holds the active frame"
        );
        assert_eq!(
            thumbnails
                .iter()
                .map(|want| Some(want.slot.item.clone()))
                .collect::<Vec<_>>(),
            strip
                .frames
                .iter()
                .map(|frame| frame.item.clone())
                .collect::<Vec<_>>(),
            "step {step}: each strip frame's photograph"
        );
        assert!(thumbnails.iter().all(|want| {
            want.shown
                && want.slot.tier() == PreviewTier::Grid
                && want.slot.priority() == PreviewPriority::Visible
                && matches!(want.slot.item, PreviewItem::Photo { .. })
        }));
        assert!(
            strip.frames.iter().all(|frame| frame.thumbnail.is_none()),
            "step {step}: nothing drawn before it is read"
        );
        // The loupe's tasks, run here: the batch the last message planned is not run, so the
        // frames start again from what the loupe wants now.
        editor.select.loupe.frames = LoupeFrames::default();
        let mut batch = editor.select.loupe.frames.want(wants.clone());
        luxforge_testbase::wait_until("the strip's grid tiers rendered and decoded", || {
            while let Some(sent) = batch.take() {
                assert!(
                    sent.reads
                        .iter()
                        .filter(|slot| slot.role == Role::Thumbnail)
                        .all(|slot| slot.tier() == PreviewTier::Grid)
                );
                let answers = loupe_frames::read(&editor.owner, editor.client, sent);
                batch = editor.select.loupe.frames.answered(answers);
            }
            batch = editor
                .select
                .loupe
                .frames
                .woken(woke.swap(false, Ordering::AcqRel));
            batch.is_none() && editor.select.loupe.frames.settled(&wants)
        });
        // The next message mirrors what the loupe holds, and the model draws it.
        send(&mut editor, LoupeMessage::Pointer(None));
        assert!(editor.loupe_settled());
        let strip = editor.workspace.select.loupe.strip.clone().unwrap();
        for frame in &strip.frames {
            let picture = frame.thumbnail.as_ref().expect("drawn once decoded");
            assert_eq!(Some(&picture.item), frame.item.as_ref(), "its own");
            assert!(
                picture.origin == PreviewOrigin::Rendered
                    && !picture.stand_in
                    && picture.key.contains(":grid:"),
                "{picture:?}"
            );
            assert!(editor.loupe_images().thumbnail(frame).is_some());
        }
        let summary = editor.loupe_summary();
        let recorded = &summary["strip"]["thumbnails"];
        assert_eq!(recorded.as_array().map(Vec::len), Some(strip.frames.len()));
        for thumbnail in recorded.as_array().unwrap() {
            assert_eq!(thumbnail["drawn"], true, "{thumbnail}");
            assert_eq!(thumbnail["picture"]["item"], thumbnail["item"]);
        }
        assert_eq!(
            summary["frames"]["thumbnails_held"],
            summary["frames"]["thumbnails"]
        );
        send(&mut editor, LoupeMessage::Frame(Travel::Forward));
    }
    send(&mut editor, LoupeMessage::Close);
    assert_eq!(editor.select.loupe.frames.summary()["handles"], 0);
    finish(editor, catalog);
}
