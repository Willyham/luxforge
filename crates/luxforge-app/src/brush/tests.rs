//! The shared brush: one binding rule and one capture for the mask brush and a declared brush, the
//! workspace's gesture and hit rules, frame correlation, and the same request from a desktop
//! capture and from semantic JSON through nonlinear warps, crop, straighten and orientation — the
//! last against a running owner with the test consumer registered ([`super::testing`]).
use super::*;
use crate::{
    mask_draft::{BRUSH, MaskDraft, NEUTRAL_BRUSH},
    view::{canvas_view::CanvasView, mask_canvas::Placement},
};
use luxforge_core::{
    DraftStamp, EntryId, GeometryMap, MappingDescriptor, SnapshotId, check_parameters,
    path::{COORDINATE_STEPS_PER_UNIT, decimate},
};
use serde_json::{Map, Value, json};

fn declared() -> BrushBinding {
    BrushBinding::declared(&testing::descriptor()).expect("the consumer declares a brush")
}

/// The stored grid position nearest `value`: half a step from either rounding boundary, so a
/// position that crosses a map and back is stored at the same position whatever rounding it met
/// on the way.
fn cell(value: f64) -> f64 {
    (value * COORDINATE_STEPS_PER_UNIT).round() / COORDINATE_STEPS_PER_UNIT
}

const NONE: Modifiers = Modifiers {
    shift: false,
    command: false,
    alt: false,
};

#[test]
fn the_mask_brush_and_a_declared_brush_clamp_and_step_by_one_rule() {
    let mask = BrushBinding::mask();
    assert_eq!(mask.module, None);
    assert_eq!(mask.method, luxforge_core::mask::commands::ADD_STROKE);
    assert_eq!(
        (mask.points.as_str(), mask.radius.as_str()),
        ("points", "size")
    );
    assert_eq!(mask.feather.as_deref(), Some("feather"));
    assert!(mask.opacity.is_none() && mask.mode.is_none() && mask.source.is_none());
    assert!(mask.edit.is_none(), "a mask stroke has no pins");
    // The mask editor's own setter and the shared binding land on the same value.
    for value in [-1.0, 0.0, 0.05, 0.3, 99.0, f64::NAN] {
        let mut brush = NEUTRAL_BRUSH;
        let mut settings = mask.settings();
        settings.radius = brush.size;
        assert_eq!(
            brush.set("size", value),
            mask.set(&mut settings, "size", value)
        );
        assert_eq!(brush.size, settings.radius, "size = {value}");
    }
    let mut brush = NEUTRAL_BRUSH;
    let mut settings = mask.settings();
    settings.radius = brush.size;
    assert_eq!(
        brush.nudge("size", 2.0),
        mask.nudge(&mut settings, "size", 2.0)
    );
    assert_eq!(brush.size, settings.radius);
    // The mask brush's flow and colour limit are its own: the shared binding binds neither.
    assert!(!mask.set(&mut settings, "flow", 50.0));

    let brush = declared();
    assert_eq!(brush.module.as_deref(), Some(testing::MODULE));
    assert_eq!(brush.method, format!("edit.{}", testing::ADD));
    let edit = brush.edit.as_ref().expect("the declared edit");
    assert_eq!(edit.method, format!("edit.{}", testing::UPDATE));
    assert_eq!(edit.operation, testing::OPERATION);
    assert_eq!(
        brush.source,
        Some((
            "source-x".to_owned(),
            "source-y".to_owned(),
            "repair-sources".to_owned()
        ))
    );
    let mut settings = brush.settings();
    assert_eq!(
        settings,
        BrushSettings {
            radius: 0.05,
            feather: Some(50.0),
            opacity: Some(100.0),
            mode: Some("heal".into()),
        }
    );
    // An integer setting is rounded and clamped as its declaration says.
    assert!(brush.set(&mut settings, "opacity", 80.6));
    assert_eq!(settings.opacity, Some(81.0));
    assert!(brush.set(&mut settings, "feather", 140.0));
    assert_eq!(settings.feather, Some(100.0));
    assert!(brush.nudge(&mut settings, "radius", 1.0));
    assert!((settings.radius - 0.055).abs() < 1e-12);
    assert!(!brush.set(&mut settings, "flow", 1.0));
    assert!(brush.set_mode(&mut settings, "clone"));
    assert!(!brush.set_mode(&mut settings, "smudge"));
    assert_eq!(settings.mode.as_deref(), Some("clone"));
}

#[test]
fn one_capture_posts_the_same_path_for_the_mask_and_a_declared_brush() {
    let path = [[0.3, 0.4], [0.35, 0.42], [0.45, 0.41], [0.5, 0.5]];
    // The mask consumer, unchanged: its editor posts the shared capture at its size.
    let mut stroke = MaskDraft::creating(BRUSH, NEUTRAL_BRUSH).expect("the brush");
    stroke.paint_begin((path[0][0], path[0][1]));
    for point in &path[1..] {
        stroke.paint_to((point[0], point[1]));
    }
    stroke.paint_end();
    let masked = stroke.fields();
    let mask = BrushBinding::mask();
    let mut settings = mask.settings();
    settings.radius = NEUTRAL_BRUSH.size;
    settings.feather = Some(NEUTRAL_BRUSH.feather);
    let shared = mask
        .add_fields(&settings, &StrokeCapture::of(&path), None)
        .expect("a drawn path");
    for name in ["points", "size", "feather"] {
        assert_eq!(masked[name], shared[name], "{name}");
    }
    assert!(
        masked.contains_key("flow") && !shared.contains_key("flow"),
        "flow stays the mask brush's own"
    );
    assert!(
        mask.add_fields(&settings, &StrokeCapture::of(&path), Some([0.1, 0.1]))
            .is_err(),
        "the mask brush takes no source"
    );

    // The declared consumer: the same capture, its own names, integers as integers, and a request
    // its declaration accepts whole.
    let brush = declared();
    let settings = brush.settings();
    let fields = brush
        .add_fields(&settings, &StrokeCapture::of(&path), Some([0.6, 0.4]))
        .expect("a drawn path");
    assert_eq!(fields["points"], json!(decimate(&path, 0.05).unwrap()));
    assert_eq!(fields["radius"], json!(0.05));
    assert_eq!(fields["feather"], json!(50));
    assert_eq!(fields["opacity"], json!(100));
    assert_eq!(fields["mode"], json!("heal"));
    assert_eq!(
        (&fields["source-x"], &fields["source-y"]),
        (&json!(0.6), &json!(0.4))
    );
    check_parameters(&brush.action, &Value::Object(fields)).expect("the add accepts it");
    let without = brush
        .add_fields(&settings, &StrokeCapture::of(&path), None)
        .unwrap();
    assert!(!without.contains_key("source-x") && !without.contains_key("source-y"));

    // Nothing drawn, or a failed capture, is refused rather than posted empty.
    assert!(
        brush
            .add_fields(&settings, &StrokeCapture::new(), None)
            .is_err()
    );
    let failed = StrokeCapture::of(&[[0.2, 0.2], [f64::NAN, 0.3]]);
    assert!(brush.add_fields(&settings, &failed, None).is_err());
}

#[test]
fn a_click_is_a_spot_and_a_drag_a_stroke_by_three_points_of_travel() {
    // Jitter inside the slop is a click: one spot at the press, whatever was painted.
    let mut click = BrushGesture::press((100.0, 100.0), [0.5, 0.5], NONE, None);
    assert_eq!(click.moved((101.5, 101.5), [0.5001, 0.5001]), Moved::Path);
    assert!(!click.dragged());
    let Released::Add { path, pulled } = click.release((102.0, 101.0), [0.5002, 0.5001]) else {
        panic!("a click adds");
    };
    assert_eq!(path, StrokeCapture::of(&[[0.5, 0.5]]));
    assert_eq!(pulled, None);
    // Three points of travel is a drag, and every painted position is the stroke.
    let mut drag = BrushGesture::press((100.0, 100.0), [0.5, 0.5], NONE, None);
    drag.moved((102.0, 100.0), [0.502, 0.5]);
    drag.moved((100.0 + CLICK_SLOP, 100.0), [0.503, 0.5]);
    assert!(drag.dragged());
    // The live draft and overlay read the path so far.
    assert_eq!(drag.path().captured(), 3);
    assert!(drag.path().painting());
    let Released::Add { path, .. } = drag.release((103.0, 100.0), [0.503, 0.5]) else {
        panic!("a drag adds");
    };
    let mut expected = StrokeCapture::new();
    expected.press((0.5, 0.5));
    expected.paint((0.502, 0.5));
    expected.paint((0.503, 0.5));
    expected.release();
    assert_eq!(path, expected);
    assert_eq!(path.first(), Some([0.5, 0.5]));
    assert_eq!(path.last(), Some([0.503, 0.5]));
}

#[test]
fn modifiers_make_straight_strokes_pulled_sources_and_set_sources() {
    let shift = Modifiers {
        shift: true,
        ..NONE
    };
    let command = Modifiers {
        command: true,
        ..NONE
    };
    let alt = Modifiers { alt: true, ..NONE };
    // ⇧-click: a straight stroke from where the last path ended to the click.
    let straight = BrushGesture::press((10.0, 10.0), [0.6, 0.6], shift, Some([0.2, 0.3]));
    let Released::Add { path, pulled } = straight.release((10.0, 10.0), [0.6, 0.6]) else {
        panic!("a straight stroke adds");
    };
    assert_eq!(path, StrokeCapture::of(&[[0.2, 0.3], [0.6, 0.6]]));
    assert_eq!(pulled, None);
    // With no earlier path it is a spot.
    let spot = BrushGesture::press((10.0, 10.0), [0.6, 0.6], shift, None);
    assert!(matches!(
        spot.release((10.0, 10.0), [0.6, 0.6]),
        Released::Add { path, .. } if path == StrokeCapture::of(&[[0.6, 0.6]])
    ));
    // ⌘-drag: a spot at the press whose source follows the pointer out to the release.
    let mut pull = BrushGesture::press((10.0, 10.0), [0.4, 0.4], command, None);
    assert_eq!(pull.moved((11.0, 10.0), [0.401, 0.4]), Moved::Nothing);
    assert_eq!(pull.pulled(), None);
    assert_eq!(
        pull.moved((30.0, 10.0), [0.42, 0.4]),
        Moved::Source([0.42, 0.4])
    );
    let Released::Add { path, pulled } = pull.release((40.0, 12.0), [0.43, 0.41]) else {
        panic!("a pull adds");
    };
    assert_eq!(path, StrokeCapture::of(&[[0.4, 0.4]]));
    assert_eq!(pulled, Some([0.43, 0.41]));
    // A ⌘-click pulled nothing: a spot with the window's source.
    let held = BrushGesture::press((10.0, 10.0), [0.4, 0.4], command, None);
    assert!(matches!(
        held.release((10.5, 10.0), [0.4005, 0.4]),
        Released::Add { pulled: None, .. }
    ));
    // ⌥-click: the source for the presses that follow, at the press, whatever the modifiers
    // beside it.
    let set = BrushGesture::press(
        (10.0, 10.0),
        [0.7, 0.2],
        Modifiers {
            alt: true,
            shift: true,
            command: true,
        },
        Some([0.1, 0.1]),
    );
    assert!(set.sets_source());
    assert_eq!(
        set.release((50.0, 50.0), [0.9, 0.9]),
        Released::SetSource([0.7, 0.2])
    );
    let _ = alt;
}

#[test]
fn a_press_is_caught_by_handles_then_the_topmost_pin_else_it_adds() {
    let handles = [(Handle::Source, (50.0, 50.0)), (Handle::Size, (80.0, 50.0))];
    // Top of the stack first: "upper" overlaps "lower" and wins whichever is nearer.
    let pins = [("upper", (104.0, 100.0)), ("lower", (100.0, 100.0))];
    let at = |pointer, modifiers, shown| hit(pointer, modifiers, shown, handles, pins);
    assert_eq!(at((52.0, 50.0), NONE, true), Hit::Handle(Handle::Source));
    assert_eq!(at((100.0, 100.0), NONE, true), Hit::Pin("upper"));
    assert_eq!(
        at((96.0, 100.0), NONE, true),
        Hit::Pin("upper"),
        "the topmost in reach"
    );
    assert_eq!(at((92.0, 100.0), NONE, true), Hit::Pin("lower"));
    // Nine points catch; just beyond adds.
    assert_eq!(
        at((100.0, 100.0 + HIT_RADIUS), NONE, true),
        Hit::Pin("lower")
    );
    assert_eq!(at((100.0, 100.1 + HIT_RADIUS), NONE, true), Hit::Add);
    // ⌥ and ⌘ presses bypass the pins, as adding gestures; the selected handles still catch.
    for modifiers in [
        Modifiers { alt: true, ..NONE },
        Modifiers {
            command: true,
            ..NONE
        },
    ] {
        assert_eq!(at((100.0, 100.0), modifiers, true), Hit::Add);
        assert_eq!(at((80.0, 50.0), modifiers, true), Hit::Handle(Handle::Size));
    }
    // ⇧ is not a source key: a ⇧-press on a pin still selects it.
    let shift = Modifiers {
        shift: true,
        ..NONE
    };
    assert_eq!(at((100.0, 100.0), shift, true), Hit::Pin("upper"));
    // With pins hidden there are no targets at all.
    assert_eq!(at((52.0, 50.0), NONE, false), Hit::Add);
    assert_eq!(at((100.0, 100.0), NONE, false), Hit::Add);
}

#[test]
fn a_set_source_is_measured_by_the_next_press_and_kept_by_those_after() {
    let mut source = SourceChoice::default();
    assert_eq!(source.source_for([0.5, 0.5]), None, "automatic");
    assert_eq!(source.crosshair([0.5, 0.5]), None);
    source.set([0.7, 0.2]);
    assert_eq!(source.crosshair([0.1, 0.1]), Some([0.7, 0.2]));
    // The first path samples the set point itself and measures the offset.
    assert_eq!(source.source_for([0.5, 0.4]), Some([0.7, 0.2]));
    let offset = [0.7 - 0.5, 0.2 - 0.4];
    assert_eq!(source, SourceChoice::Offset(offset));
    // Following paths keep that offset, and the crosshair follows the brush at it.
    assert_eq!(
        source.source_for([0.1, 0.6]),
        Some([0.1 + offset[0], 0.6 + offset[1]])
    );
    assert_eq!(
        source.crosshair([0.3, 0.3]),
        Some([0.3 + offset[0], 0.3 + offset[1]])
    );
    source.clear();
    assert_eq!(source, SourceChoice::Auto);
}

#[test]
fn handle_drags_change_only_what_they_hold_and_a_drag_back_writes_nothing() {
    let step = 1.0 / COORDINATE_STEPS_PER_UNIT;
    let held = HeldOperation {
        operation: "correction-5".into(),
        points: vec![[cell(0.3), cell(0.3)], [cell(0.4), cell(0.35)]],
        radius: 0.05,
        source: Some([0.6, 0.3]),
    };
    let brush = declared();
    // A pin drag translates the whole path by whole grid steps, from the press.
    let mut pin = HandleEdit::begin(Handle::Pin, held.clone(), [0.31, 0.31], 1.5);
    assert_eq!(pin.handle(), Handle::Pin);
    pin.drag([0.31 + 20.4 * step, 0.31 - 7.6 * step]);
    let Some(HandleChange::Path(moved)) = pin.change() else {
        panic!("a moved pin");
    };
    for (was, now) in held.points.iter().zip(&moved) {
        assert_eq!(now[0], was[0] + 20.0 * step);
        assert_eq!(now[1], was[1] - 8.0 * step);
    }
    let (method, (identity, operation), fields) = brush.edit_fields(&pin).expect("an edit");
    assert_eq!(method, format!("edit.{}", testing::UPDATE));
    assert_eq!(
        (identity, operation.as_str()),
        (testing::OPERATION, "correction-5")
    );
    assert_eq!(
        fields,
        Map::from_iter([("points".to_owned(), json!(moved))])
    );
    // The source stays where it is: a pin drag sends no source.
    assert!(!fields.contains_key("source-x"));
    // A drag away and back, or less than half a grid step, changes nothing and writes nothing.
    pin.drag([0.31 + 0.4 * step, 0.31]);
    assert_eq!(pin.change(), None);
    assert!(brush.edit_fields(&pin).is_none());
    // A source drag moves the source by the pointer's travel.
    let mut source = HandleEdit::begin(Handle::Source, held.clone(), [0.6, 0.3], 1.5);
    source.drag([0.65, 0.28]);
    let (_, _, fields) = brush.edit_fields(&source).expect("a moved source");
    assert!((fields["source-x"].as_f64().unwrap() - 0.65).abs() < 1e-12);
    assert!((fields["source-y"].as_f64().unwrap() - 0.28).abs() < 1e-12);
    // A size drag changes a spot's radius by the pointer's travel away from its centre, in
    // content heights, without jumping when the press is off the handle; clamped as declared.
    let spot = HeldOperation {
        points: vec![[0.5, 0.5]],
        ..held
    };
    let mut size = HandleEdit::begin(Handle::Size, spot.clone(), [0.5 + 0.052 / 1.5, 0.5], 1.5);
    assert_eq!(size.change(), None);
    size.drag([0.5 + 0.082 / 1.5, 0.5]);
    let Some(HandleChange::Radius(radius)) = size.change() else {
        panic!("a resized spot");
    };
    assert!((radius - 0.08).abs() < 1e-12, "{radius}");
    size.drag([0.5 + 9.0, 0.5]);
    let (_, _, fields) = brush.edit_fields(&size).expect("a resize");
    assert_eq!(fields["radius"], json!(0.25), "the declared maximum");
    // A mask stroke has no edit.
    assert!(BrushBinding::mask().edit_fields(&size).is_none());
}

fn descriptor_for(
    entry: &EntryId,
    draft: Option<DraftStamp>,
    geometry: GeometryMap,
) -> MappingDescriptor {
    MappingDescriptor {
        entry_id: entry.clone(),
        snapshot_id: SnapshotId::new(),
        source_fingerprint: "fingerprint".into(),
        draft,
        geometry,
    }
}

#[test]
fn a_press_waits_for_a_map_of_the_displayed_frame_and_refuses_what_it_cannot_place() {
    let geometry = crate::state::testing::nonlinear_mapping(
        crate::state::testing::TestOrientation::NEUTRAL,
        40,
        -25,
    );
    let shown = EntryId::new();
    let other = EntryId::new();
    let displayed = Displayed {
        entry: &shown,
        draft: None,
    };
    assert_eq!(
        ContentMap::for_press(None, &displayed),
        Err(MapRefusal::Waiting)
    );
    let current =
        ContentMap::from_descriptor(descriptor_for(&shown, None, geometry.clone())).unwrap();
    assert!(ContentMap::for_press(Some(&current), &displayed).is_ok());
    // The previous entry's map keeps drawing, but a press waits for the fresh one.
    let previous =
        ContentMap::from_descriptor(descriptor_for(&other, None, geometry.clone())).unwrap();
    assert_eq!(
        ContentMap::for_press(Some(&previous), &displayed),
        Err(MapRefusal::Stale)
    );
    assert!(previous.to_output(0.5, 0.5).is_some(), "still drawable");
    // A bare geometry names no frame, so it never takes a press.
    let bare = ContentMap::new(&geometry).unwrap();
    assert_eq!(
        ContentMap::for_press(Some(&bare), &displayed),
        Err(MapRefusal::Stale)
    );
    // While a draft's preview is displayed, the map must be that draft's, at its revision or later.
    let draft = luxforge_core::DraftId::new();
    let stamp = |revision| DraftStamp {
        draft_id: draft.clone(),
        draft_revision: revision,
    };
    let drafting = stamp(3);
    let previewed = Displayed {
        entry: &shown,
        draft: Some(&drafting),
    };
    for (map_stamp, belongs) in [
        (Some(stamp(3)), true),
        (Some(stamp(4)), true),
        (Some(stamp(2)), false),
        (
            Some(DraftStamp {
                draft_id: luxforge_core::DraftId::new(),
                draft_revision: 3,
            }),
            false,
        ),
        (None, false),
    ] {
        let map = ContentMap::from_descriptor(descriptor_for(
            &shown,
            map_stamp.clone(),
            geometry.clone(),
        ))
        .unwrap();
        assert_eq!(map.belongs_to(&previewed), belongs, "{map_stamp:?}");
    }
    // A position the warped map cannot place is said, not guessed.
    let (w, h) = current.output();
    assert_eq!(current.locate(-1.0, h / 2.0), Err(MapRefusal::Outside));
    assert_eq!(current.locate(w + 1.0, 0.0), Err(MapRefusal::Outside));
    assert!(current.locate(w / 2.0, h / 2.0).is_ok());
    assert_eq!(MapRefusal::Stale.token(), "stale");
    assert_eq!(
        MapRefusal::Waiting.sentence(),
        "Waiting for photograph coordinates"
    );
}

/// Paint `path` (normalized content positions) on the canvas of `placement` as a pointer would:
/// each position mapped to the canvas through the core's map, then back through the canvas's own
/// placement, exactly the round trip a real press makes.
fn painted(placement: &Placement, path: &[[f64; 2]], modifiers: Modifiers) -> Released {
    let on_canvas = |at: [f64; 2]| {
        let point = placement
            .canvas_point(at[0], at[1])
            .expect("inside the frame");
        let content = placement.content_point(point).expect("placed back");
        ((point.x, point.y), [content.0, content.1])
    };
    let (view, content) = on_canvas(path[0]);
    let mut gesture = BrushGesture::press(view, content, modifiers, None);
    for &at in &path[1..] {
        let (view, content) = on_canvas(at);
        gesture.moved(view, content);
    }
    let (view, content) = on_canvas(*path.last().unwrap());
    gesture.release(view, content)
}

/// The semantic request an agent posts for the same path, settings and source.
fn semantic(
    brush: &BrushBinding,
    path: &[[f64; 2]],
    source: Option<[f64; 2]>,
) -> Map<String, Value> {
    let settings = brush.settings();
    let mut fields = Map::from_iter([
        (
            "points".to_owned(),
            json!(decimate(path, settings.radius).unwrap()),
        ),
        ("radius".to_owned(), json!(settings.radius)),
        ("feather".to_owned(), json!(50)),
        ("opacity".to_owned(), json!(100)),
        ("mode".to_owned(), json!("heal")),
    ]);
    if let Some([x, y]) = source {
        fields.insert("source-x".into(), json!(x));
        fields.insert("source-y".into(), json!(y));
    }
    fields
}

#[test]
fn desktop_capture_and_semantic_json_agree_through_lens_perspective_and_every_orientation() {
    let brush = declared();
    let path: Vec<[f64; 2]> = (0..48)
        .map(|index| {
            let t = index as f64 / 47.0;
            [cell(0.35 + 0.3 * t), cell(0.45 + 0.08 * (t * 6.0).sin())]
        })
        .collect();
    for turns in 0..4 {
        for mirror in [false, true] {
            let geometry = crate::state::testing::nonlinear_mapping(
                crate::state::testing::TestOrientation { mirror, turns },
                40,
                -25,
            );
            let map = ContentMap::new(&geometry).unwrap();
            let (w, h) = map.output();
            for view in [
                CanvasView::fit((w, h), iced::Size::new(1200.0, 850.0)).unwrap(),
                CanvasView::percent(100.0, 2.0).unwrap(),
            ] {
                let placement = Placement {
                    map: map.clone(),
                    view,
                    scale_factor: 2.0,
                };
                let case = format!("turns {turns} mirror {mirror} {view:?}");
                // A painted stroke.
                let Released::Add {
                    path: drawn,
                    pulled,
                } = painted(&placement, &path, NONE)
                else {
                    panic!("{case}: a stroke");
                };
                assert_eq!(pulled, None);
                let fields = brush
                    .add_fields(&brush.settings(), &drawn, None)
                    .expect("a drawn stroke");
                assert_eq!(fields, semantic(&brush, &path, None), "{case}");
                // A ⌘-drag pulling its source out to a position, and a set source's offset.
                let Released::Add { path: spot, pulled } = painted(
                    &placement,
                    &[path[0], path[10], path[20]],
                    Modifiers {
                        command: true,
                        ..NONE
                    },
                ) else {
                    panic!("{case}: a pull");
                };
                let pulled = pulled.expect("a pulled source");
                assert!(
                    (pulled[0] - path[20][0]).abs() < 1e-7
                        && (pulled[1] - path[20][1]).abs() < 1e-7,
                    "{case}: {pulled:?}"
                );
                let fields = brush
                    .add_fields(&brush.settings(), &spot, Some(pulled))
                    .unwrap();
                let expected = semantic(&brush, &path[..1], Some(pulled));
                assert_eq!(fields, expected, "{case}");
                // A pin drag through the canvas moves the stored path by whole grid steps.
                let held = HeldOperation {
                    operation: "correction-1".into(),
                    points: decimate(&path, 0.05).unwrap(),
                    radius: 0.05,
                    source: Some(pulled),
                };
                let delta = [
                    64.0 / COORDINATE_STEPS_PER_UNIT,
                    -32.0 / COORDINATE_STEPS_PER_UNIT,
                ];
                let press = placement
                    .canvas_point(held.points[0][0], held.points[0][1])
                    .unwrap();
                let release = placement
                    .canvas_point(held.points[0][0] + delta[0], held.points[0][1] + delta[1])
                    .unwrap();
                let mut drag = HandleEdit::begin(
                    Handle::Pin,
                    held.clone(),
                    placement.content_point(press).map(|(x, y)| [x, y]).unwrap(),
                    map.aspect(),
                );
                drag.drag(
                    placement
                        .content_point(release)
                        .map(|(x, y)| [x, y])
                        .unwrap(),
                );
                let (_, _, fields) = brush.edit_fields(&drag).expect("a moved pin");
                let translated: Vec<[f64; 2]> = held
                    .points
                    .iter()
                    .map(|[x, y]| [x + delta[0], y + delta[1]])
                    .collect();
                assert_eq!(fields["points"], json!(translated), "{case}");
            }
        }
    }
}

#[test]
fn desktop_capture_commits_what_semantic_json_commits_through_crop_straighten_and_orientation() {
    use crate::app::tasks::call;
    use luxforge_core::{ModuleRegistry, OwnerHandle};
    use std::sync::Arc;
    let catalog = luxforge_testbase::paths::temp_catalog("brush-capture-equivalence");
    let mut registry = ModuleRegistry::builtin();
    registry.register(testing::RepairBrush::shared()).unwrap();
    let (owner, join) = OwnerHandle::start_with(&catalog, Arc::new(registry)).unwrap();
    let client = owner.register();
    let asset = crate::app::testing::import_and_adopt(
        &owner,
        client,
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0/orientation-1.jpg"),
    );
    let ask = |method: &str, params: Value| call(&owner, client, method, params).expect(method).0;
    let revision = || {
        ask("asset.state", json!({"asset_id": asset}))["revision"]
            .as_u64()
            .unwrap()
    };
    let mutate = |method: &str, mut params: Value| {
        params["asset_id"] = json!(asset);
        params["mutation"] = json!({"expected_revision": revision(), "request_id": format!("{method}-{}", revision()), "actor": "test"});
        ask(method, params)
    };
    mutate("edit.transform", json!({"transform": "rotate-right"}));
    mutate("edit.transform", json!({"transform": "mirror-horizontal"}));
    mutate(
        "edit.set-perspective",
        json!({"horizontal": 30, "vertical": -20}),
    );
    mutate(
        "edit.crop",
        json!({"x": 0.1, "y": 0.05, "width": 0.8, "height": 0.85, "angle": 4.5}),
    );
    let displayed_entry = || -> EntryId {
        serde_json::from_value(
            ask("asset.state", json!({"asset_id": asset}))["current_entry"]["id"].clone(),
        )
        .unwrap()
    };
    let transform = |asset| -> ContentMap {
        let descriptor: MappingDescriptor =
            serde_json::from_value(ask("render.transform", json!({"asset_id": asset}))).unwrap();
        ContentMap::from_descriptor(descriptor).expect("a drawable map")
    };
    let map = transform(asset.clone());
    let shown = displayed_entry();
    let map = ContentMap::for_press(
        Some(&map),
        &Displayed {
            entry: &shown,
            draft: None,
        },
    )
    .expect("the map of the frame on screen")
    .clone();
    let (w, h) = map.output();
    let placement = Placement {
        map: map.clone(),
        view: CanvasView::fit((w, h), iced::Size::new(1200.0, 850.0)).unwrap(),
        scale_factor: 2.0,
    };
    let path: Vec<[f64; 2]> = (0..24)
        .map(|index| {
            let t = index as f64 / 23.0;
            [cell(0.4 + 0.2 * t), cell(0.5 + 0.05 * (t * 5.0).sin())]
        })
        .collect();
    let brush = declared();

    // The desktop: one draft, the path painted on the canvas, one commit.
    let Released::Add { path: drawn, .. } = painted(&placement, &path, NONE) else {
        panic!("a stroke");
    };
    let fields = brush.add_fields(&brush.settings(), &drawn, None).unwrap();
    let begun = ask(
        "draft.begin",
        json!({"asset_id": asset, "action": testing::ADD}),
    );
    ask(
        "draft.set",
        json!({"draft_id": begun["draft_id"], "fields": fields}),
    );
    ask(
        "draft.commit",
        json!({"draft_id": begun["draft_id"],
               "mutation": {"expected_revision": revision(), "request_id": "desktop", "actor": "test"}}),
    );
    // The commit made a new entry: the map on hand no longer takes a press until a fresh one
    // arrives, though the repair changed no geometry.
    let shown = displayed_entry();
    let displayed = Displayed {
        entry: &shown,
        draft: None,
    };
    assert_eq!(
        ContentMap::for_press(Some(&map), &displayed),
        Err(MapRefusal::Stale)
    );
    let fresh = transform(asset.clone());
    assert!(ContentMap::for_press(Some(&fresh), &displayed).is_ok());
    assert_eq!(
        fresh.summary()["mapping_sha256"],
        map.summary()["mapping_sha256"]
    );

    // The agent: the same path as semantic JSON, one direct action.
    let mut direct = Value::Object(semantic(&brush, &path, None));
    mutate(&format!("edit.{}", testing::ADD), direct.take());
    let stored = |index: usize| {
        let state = ask("asset.state", json!({"asset_id": asset}));
        let layer = state["current_entry"]["snapshot"]["recipe"]["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|layer| layer["effect_id"] == testing::EFFECT)
            .cloned()
            .expect("the repair layer");
        let mut operation = layer["payload"]["operations"][index].clone();
        operation.as_object_mut().unwrap().remove("id");
        operation
    };
    assert_eq!(
        stored(0),
        stored(1),
        "the desktop and the agent stored the same repair"
    );

    // A pin drag on the canvas, one draft begun on the correction, against the agent's patch.
    let held = HeldOperation {
        operation: "correction-1".into(),
        points: serde_json::from_value(stored(0)["points"].clone()).unwrap(),
        radius: 0.05,
        source: None,
    };
    let delta = [
        40.0 / COORDINATE_STEPS_PER_UNIT,
        24.0 / COORDINATE_STEPS_PER_UNIT,
    ];
    let press = placement
        .canvas_point(held.points[0][0], held.points[0][1])
        .unwrap();
    let release = placement
        .canvas_point(held.points[0][0] + delta[0], held.points[0][1] + delta[1])
        .unwrap();
    let mut drag = HandleEdit::begin(
        Handle::Pin,
        held.clone(),
        placement.content_point(press).map(|(x, y)| [x, y]).unwrap(),
        map.aspect(),
    );
    drag.drag(
        placement
            .content_point(release)
            .map(|(x, y)| [x, y])
            .unwrap(),
    );
    let (method, (identity, operation), fields) = brush.edit_fields(&drag).expect("a moved pin");
    let begun = ask(
        "draft.begin",
        json!({"asset_id": asset, "action": testing::UPDATE, "operation": operation}),
    );
    assert_eq!(begun["target"], json!({identity: operation}));
    ask(
        "draft.set",
        json!({"draft_id": begun["draft_id"], "fields": fields}),
    );
    ask(
        "draft.commit",
        json!({"draft_id": begun["draft_id"],
               "mutation": {"expected_revision": revision(), "request_id": "desktop-move", "actor": "test"}}),
    );
    assert_eq!(method, format!("edit.{}", testing::UPDATE));
    let translated: Vec<[f64; 2]> = held
        .points
        .iter()
        .map(|[x, y]| [x + delta[0], y + delta[1]])
        .collect();
    mutate(
        &format!("edit.{}", testing::UPDATE),
        json!({"operation": "correction-2", "points": translated}),
    );
    assert_eq!(
        stored(0),
        stored(1),
        "the same move, from the canvas and from JSON"
    );
    owner.stop();
    join.join().unwrap();
    std::fs::remove_file(catalog).unwrap();
}
