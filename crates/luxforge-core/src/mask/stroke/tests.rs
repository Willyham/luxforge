//! The mask brush's stroke type against its contract: that a stroke's address is its contents and
//! stays the address it had before the path primitives were shared, that its settings and its
//! colour limit are held to the brush's own ranges by every reader, and that the host's store holds
//! it and refuses a broken reference to it by name.
use super::*;
use crate::path::{
    COORDINATE_STEPS_PER_UNIT, POINTS_PER_STROKE, POSTED_POINTS_PER_STROKE, StrokeFault,
    StrokeTable, decimate,
    tests::{Rng, captured},
};
use serde_json::json;

/// The one command that paints declares exactly the host's posted bound for its path.
#[test]
fn the_brush_declares_the_posted_bound_for_its_path() {
    let declared = crate::mask::commands::find(crate::mask::commands::ADD_STROKE)
        .and_then(|command| command.action.parameter("points").cloned())
        .expect("mask.add-stroke declares its path");
    assert_eq!(
        declared.kind,
        crate::ParameterKind::Points {
            points_min: 1,
            points_max: POSTED_POINTS_PER_STROKE
        }
    );
}

#[test]
fn one_captured_path_has_one_content_address() {
    let mut rng = Rng(0xa11);
    for _ in 0..100 {
        let count = 2 + (rng.next() % 300) as usize;
        let path = captured(&mut rng, count);
        let once = Stroke::capture(&path, 0.05, 50.0, 100.0, false).expect("a legal stroke");
        let twice = Stroke::capture(&path, 0.05, 50.0, 100.0, false).expect("a legal stroke");
        assert_eq!(once.id(), twice.id(), "the same path is the same stroke");
        assert_eq!(once, twice);

        // Posting an already-decimated path reaches the same stored bytes as posting the raw one,
        // which is what lets the desktop decimate before it sends without changing what is stored.
        let predecimated = decimate(&path, 0.05).expect("a legal path");
        let posted =
            Stroke::capture(&predecimated, 0.05, 50.0, 100.0, false).expect("a legal stroke");
        assert_eq!(once.id(), posted.id());

        // A jitter that leaves every coordinate inside its own grid cell is below the stored
        // precision and is the same stroke. It is applied to the on-grid path, because a jitter on
        // the raw one could carry a coordinate across a cell boundary and that is a different
        // stored position, not a rounding question.
        let nudged: Vec<[f64; 2]> = predecimated
            .iter()
            .map(|[x, y]| {
                let eighth = 1.0 / (COORDINATE_STEPS_PER_UNIT * 8.0);
                [x + eighth, y - eighth]
            })
            .collect();
        assert_eq!(
            once.id(),
            Stroke::capture(&nudged, 0.05, 50.0, 100.0, false)
                .expect("a legal stroke")
                .id(),
            "a jitter below the stored precision is the same stroke",
        );

        // The settings are part of the contents, so changing one is a different stroke.
        assert_ne!(
            once.id(),
            Stroke::capture(&path, 0.05, 50.0, 100.0, true)
                .expect("a legal stroke")
                .id(),
            "an erase of the same path is a different stroke",
        );
        assert_ne!(
            once.id(),
            Stroke::capture(&path, 0.06, 50.0, 100.0, false)
                .expect("a legal stroke")
                .id(),
        );
    }
}

#[test]
fn a_stroke_round_trips_through_its_stored_bytes() {
    let stroke = Stroke::capture(
        &[[0.1, 0.2], [0.4, 0.45], [0.8, 0.2]],
        0.05,
        37.5,
        80.0,
        true,
    )
    .expect("a legal stroke");
    let id = stroke.id();
    let stored = stroke.canonical();
    assert_eq!(Stroke::from_stored(&id, &stored).unwrap(), stroke);
    assert_eq!(
        String::from_utf8(stored.clone()).unwrap(),
        r#"{"points":[[1638,3277],[6554,7373],[13107,3277]],"size":819,"feather":38,"flow":80,"erase":true}"#,
        "every stored field is a whole step and never a decimal, because the bytes are the identity",
    );
    assert!(stroke.erase());
    assert_eq!(stroke.point_count(), 3);
    // 37.5 was requested; the declared control moves in whole units, so 38 is what is stored.
    assert_eq!(stroke.feather(), 38.0);
    assert_eq!(stroke.flow(), 80.0);

    // Bytes that are not the bytes the address names are refused rather than parsed.
    let mut corrupt = stored.clone();
    let at = corrupt.iter().position(|b| *b == b'8').expect("a digit");
    corrupt[at] = b'9';
    let error = Stroke::from_stored(&id, &corrupt).unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Incompatible);
    assert_eq!(
        error.detail,
        format!("stored stroke {id} does not match its content address")
    );

    // And bytes that hash correctly but hold a number outside the stored range are refused too, so
    // a legal address is never a licence to evaluate an illegal stroke.
    let illegal = br#"{"points":[[1638,3277]],"size":819,"feather":250,"flow":80,"erase":false}"#;
    let illegal_id = StrokeId::of(illegal);
    assert_eq!(
        Stroke::from_stored(&illegal_id, illegal)
            .unwrap_err()
            .detail,
        format!("stored stroke {illegal_id} has an invalid feather")
    );
}

#[test]
fn a_stroke_is_refused_by_name_outside_its_declared_bounds() {
    let path = [[0.1, 0.2], [0.4, 0.45]];
    for (field, size, feather, flow) in [
        ("size", 0.0, 50.0, 50.0),
        ("size", 3.0, 50.0, 50.0),
        ("feather", 0.05, -1.0, 50.0),
        ("feather", 0.05, 101.0, 50.0),
        ("flow", 0.05, 50.0, f64::NAN),
        ("flow", 0.05, 50.0, 100.5),
    ] {
        let error = Stroke::capture(&path, size, feather, flow, false).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Validation);
        assert!(
            error.detail.starts_with(&format!("stroke {field} must be")),
            "{}",
            error.detail
        );
    }
    assert_eq!(
        Stroke::capture(&[], 0.05, 50.0, 50.0, false)
            .unwrap_err()
            .detail,
        "a path must hold at least one position"
    );
    assert_eq!(
        Stroke::capture(&[[0.5, 3.0]], 0.05, 50.0, 50.0, false)
            .unwrap_err()
            .detail,
        "path position 0 y must be a number within -1..=2"
    );
}

/// A path that survives decimation at full length for a brush small enough to take the two-step
/// floor — every other position eight steps off the line between its neighbours — so the per-stroke
/// bound is reached rather than decimated away, and `Stroke::capture` refuses it one past the bound
/// by name.
fn incompressible(count: usize) -> Vec<[f64; 2]> {
    (0..count)
        .map(|index| {
            let along = index as f64 * 8.0 / COORDINATE_STEPS_PER_UNIT;
            let across = if index % 2 == 0 { 0.0 } else { 8.0 } / COORDINATE_STEPS_PER_UNIT;
            [0.1 + along, 0.5 + across]
        })
        .collect()
}

#[test]
fn a_captured_stroke_over_the_bound_names_the_points_per_stroke_limit() {
    let at_bound = incompressible(POINTS_PER_STROKE);
    let stroke = Stroke::capture(&at_bound, 0.001, 50.0, 100.0, false).expect("the bound is legal");
    assert_eq!(stroke.point_count(), POINTS_PER_STROKE);

    let error = Stroke::capture(
        &incompressible(POINTS_PER_STROKE + 1),
        0.001,
        50.0,
        100.0,
        false,
    )
    .unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::ResourceLimit);
    assert_eq!(
        error.detail,
        format!(
            "stroke has {} positions after decimation; the limit is {POINTS_PER_STROKE} points \
             per stroke",
            POINTS_PER_STROKE + 1
        )
    );
}

#[test]
fn a_broken_reference_is_named_and_is_never_an_empty_stroke() {
    let stroke = Stroke::capture(&[[0.1, 0.2], [0.4, 0.45]], 0.05, 50.0, 100.0, false).unwrap();
    let absent = Stroke::capture(&[[0.9, 0.9], [0.1, 0.1]], 0.05, 50.0, 100.0, false).unwrap();
    let mut table = StrokeTable::new("entry entry-7");
    let known = table.insert(stroke.clone());
    assert_eq!(table.resolve::<Stroke>(&known).unwrap(), &stroke);
    assert_eq!(table.get::<Stroke>(&known), Some(&stroke));

    let missing = absent.id();
    let error = table.resolve::<Stroke>(&missing).unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Incompatible);
    assert_eq!(
        error.detail,
        format!("stroke {missing} of entry entry-7 is not in the stroke store")
    );

    table.fault(missing.clone(), StrokeFault::Corrupt);
    assert_eq!(
        table.resolve::<Stroke>(&missing).unwrap_err().detail,
        format!("stroke {missing} of entry entry-7 does not match its stored content address")
    );
    assert_eq!(
        table.get::<Stroke>(&missing),
        None,
        "a fault is never a stroke"
    );
}

/// A stroke's identity is the hash of its bytes, so every stored field has to survive a round trip
/// through JSON exactly. `serde_json` does not promise that for an arbitrary `f64` — the value below
/// reads back one ulp away — so the settings are stored as whole units, like the positions and the
/// size. A fractional request is quantized at capture rather than stored and later reparsed into a
/// different content address than the recipe references.
#[test]
fn a_stroke_keeps_its_content_address_through_a_round_trip_of_any_setting() {
    let path = [[0.10, 0.20], [0.40, 0.55], [0.80, 0.30]];
    for (feather, flow) in [
        (0.0, 100.0),
        (55.0, 45.0),
        // Values a client may legitimately post that no declared control offers.
        (55.333_333_333_333_33, 2.624_122_239_649_296),
        (99.999_999_999, 0.000_000_001),
    ] {
        let stroke = Stroke::capture(&path, 0.05, feather, flow, false).expect("capture");
        let id = stroke.id();
        let bytes = serde_json::to_vec(&stroke).expect("serialize");
        let back: Stroke = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(
            back.id(),
            id,
            "a reparsed stroke must keep the address its recipe references \
             (feather {feather}, flow {flow})"
        );
        assert_eq!(back, stroke, "and must be the same stroke");
        assert_eq!(
            (back.feather(), back.flow()),
            (feather.round(), flow.round()),
            "the stored setting is the declared control's own step"
        );
    }
}

/// Cloning a recipe shares its stroke table instead of copying every stroke's positions, and a
/// stroke added to a clone copies the table's pointers only: the recipe it was cloned from keeps
/// its table unchanged, and both still hold the one allocation of each stroke they share.
#[test]
fn cloning_a_recipe_shares_its_strokes_and_a_write_copies_only_pointers() {
    let drawn = Stroke::capture(&[[0.1, 0.1], [0.4, 0.3]], 0.05, 50.0, 100.0, false).unwrap();
    let mut table = StrokeTable::new("entry entry-1");
    let first = table.insert(drawn);
    let recipe = crate::Recipe {
        strokes: table,
        ..crate::Recipe::default()
    };
    let clone = recipe.clone();
    assert!(
        clone.strokes.shares(&recipe.strokes),
        "a clone shares the table"
    );
    assert!(std::ptr::eq(
        clone.strokes.get::<Stroke>(&first).unwrap(),
        recipe.strokes.get::<Stroke>(&first).unwrap()
    ));

    let mut next = recipe.clone();
    let second = next
        .strokes
        .insert(Stroke::capture(&[[0.6, 0.6]], 0.05, 50.0, 100.0, true).unwrap());
    assert!(
        !next.strokes.shares(&recipe.strokes),
        "a write copies on write"
    );
    assert!(
        recipe.strokes.get::<Stroke>(&second).is_none(),
        "and leaves the original alone"
    );
    assert_eq!(recipe.strokes.strokes().count(), 1);
    assert_eq!(next.strokes.strokes().count(), 2);
    assert!(
        std::ptr::eq(
            next.strokes.get::<Stroke>(&first).unwrap(),
            recipe.strokes.get::<Stroke>(&first).unwrap()
        ),
        "the copy holds the same stroke, not a copy of its positions"
    );
}

/// A stroke's radius has one legal range, and every reader holds it: the declared `size` of the one
/// command that paints, [`Stroke::capture`], the stored-stroke recheck and the brush's compile. A
/// radius just outside it is refused by each of them, naming the range, and one just inside is taken
/// by each.
#[test]
fn one_stroke_radius_range_is_read_by_capture_the_stored_recheck_and_compile() {
    let range = format!("{SIZE_MIN}..={SIZE_MAX}");
    let declared = crate::mask::commands::find(crate::mask::commands::ADD_STROKE)
        .and_then(|command| command.action.parameter("size").cloned())
        .expect("mask.add-stroke declares its size");
    assert_eq!(
        declared.kind,
        crate::ParameterKind::Number {
            min: SIZE_MIN,
            max: SIZE_MAX
        },
        "the declared size is the one range"
    );

    // Capture, on the posted size.
    let path = [[0.4, 0.5], [0.6, 0.5]];
    for inside in [SIZE_MIN, SIZE_MAX] {
        Stroke::capture(&path, inside, 50.0, 100.0, false).expect("the range is closed");
    }
    for outside in [SIZE_MIN * (1.0 - 1e-9), SIZE_MAX * (1.0 + 1e-9)] {
        let error = Stroke::capture(&path, outside, 50.0, 100.0, false).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Validation);
        assert_eq!(
            error.detail,
            format!("stroke size must be a number within {range}")
        );
    }

    // The stored recheck and the compile, on the size as stored: whole grid steps, so the legal
    // ones are the steps inside the range and the illegal ones the steps either side of it.
    let lowest = (SIZE_MIN * COORDINATE_STEPS_PER_UNIT).ceil() as i32;
    let highest = (SIZE_MAX * COORDINATE_STEPS_PER_UNIT).floor() as i32;
    let stored = |size: i32| Stroke {
        points: vec![[6554, 8192], [9830, 8192]],
        size,
        feather: 50,
        flow: 100,
        erase: false,
        colour: None,
    };
    let compiled = |stroke: &Stroke| {
        let mut table = StrokeTable::new("the range test");
        let id = table.insert(stroke.clone());
        let mut mask = crate::Mask::new("Mask 1");
        mask.components.push(crate::Component::new(
            "Brush 1",
            crate::ComponentMode::Add,
            crate::mask::BRUSH,
            json!({ "strokes": [id.to_string()] }),
        ));
        crate::mask::CompiledMask::new(
            &mask,
            crate::modules::Stage {
                width: 60,
                height: 40,
            },
            &table,
        )
        .map(|_| ())
    };
    for inside in [lowest, highest] {
        let stroke = stored(inside);
        Stroke::from_stored(&stroke.id(), &stroke.canonical())
            .unwrap_or_else(|error| panic!("{inside} steps is a legal stored size: {error}"));
        compiled(&stroke)
            .unwrap_or_else(|error| panic!("{inside} steps is a legal compiled size: {error}"));
    }
    for outside in [lowest - 1, highest + 1] {
        let stroke = stored(outside);
        let id = stroke.id();
        let error = Stroke::from_stored(&id, &stroke.canonical()).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Incompatible);
        assert_eq!(
            error.detail,
            format!("stored stroke {id} has an invalid size; a stroke size is within {range}"),
            "{outside} steps"
        );
        let error = compiled(&stroke).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Validation);
        assert_eq!(
            error.detail,
            format!("component Brush 1 stroke {id} size must be a number within {range}"),
            "{outside} steps"
        );
    }
}

/// Two known mask strokes' content addresses, pinned: one unlimited erase and one limited to a
/// colour. The addresses are computed from the canonical bytes, so a change to the stroke's type,
/// its field order or its serialization that would re-address every stored stroke fails here.
#[test]
fn a_known_mask_stroke_keeps_its_pinned_content_address() {
    let unlimited = Stroke::capture(
        &[[0.1, 0.2], [0.4, 0.45], [0.8, 0.2]],
        0.05,
        37.5,
        80.0,
        true,
    )
    .unwrap();
    assert_eq!(unlimited.id().as_str(), "f90567c23419da29e9a57267732c52b1");
    let limited = Stroke::capture(&[[0.25, 0.5], [0.75, 0.5]], 0.1, 50.0, 100.0, false)
        .unwrap()
        .with_colour_limit(ColourLimit::sampled([90, 130, 200], 42.25).unwrap());
    assert_eq!(
        String::from_utf8(limited.canonical()).unwrap(),
        r#"{"points":[[4096,8192],[12288,8192]],"size":1638,"feather":50,"flow":100,"erase":false,"colour":{"seed":[90,130,200],"refine":423}}"#
    );
    assert_eq!(limited.id().as_str(), "bf5aad3ef37bd74f78fc5e9945a13c97");
}

/// The brush's strokes and a second consumer's share one store: each is held as the type its
/// consumer declared, read back only as that type, and a reference that names the one as the other
/// is refused by both names rather than read.
#[test]
fn the_brush_and_a_second_consumer_share_one_store() {
    use crate::path::{StrokeKind, StrokeReference, StrokeType, tests::RepairStroke};
    let brush = Stroke::capture(&[[0.1, 0.2], [0.4, 0.45]], 0.05, 50.0, 100.0, false).unwrap();
    let repair = RepairStroke::capture(&[[0.1, 0.2], [0.4, 0.45]], 0.05, [0.1, 0.0]).unwrap();
    assert_ne!(
        brush.id(),
        StrokeKind::id(&repair),
        "the same path is two strokes"
    );
    let mut table = StrokeTable::new("entry entry-9");
    let brush_id = table.insert(brush.clone());
    let repair_id = table.insert(repair.clone());
    assert_eq!(table.strokes().count(), 2);
    assert_eq!(table.get::<Stroke>(&brush_id), Some(&brush));
    assert_eq!(table.get::<RepairStroke>(&repair_id), Some(&repair));
    assert_eq!(table.get::<Stroke>(&repair_id), None);
    assert_eq!(table.get::<RepairStroke>(&brush_id), None);
    assert_eq!(
        table.resolve::<Stroke>(&repair_id).unwrap_err().detail,
        format!(
            "stroke {repair_id} of entry entry-9 is a test repair stroke, not a mask brush stroke"
        )
    );
    let misnamed = StrokeReference {
        what: "component Brush 1 of mask Mask 1".to_owned(),
        id: repair_id.clone(),
        kind: StrokeType::of::<Stroke>(),
    };
    assert_eq!(
        table.check_reference(&misnamed).unwrap_err().kind,
        crate::ErrorKind::Incompatible
    );
    table
        .check_reference(&repair.reference("repair 1"))
        .unwrap();

    // Loaded from stored bytes, each is decoded as the type its reference declares: a brush
    // stroke's bytes read as a repair stroke are corrupt, not a repair.
    let mut loaded = StrokeTable::new("entry entry-10");
    loaded.load(
        brush_id.clone(),
        StrokeType::of::<Stroke>(),
        Some(&brush.canonical()),
    );
    loaded.load(
        repair_id.clone(),
        StrokeType::of::<RepairStroke>(),
        Some(&StrokeKind::canonical(&repair)),
    );
    assert_eq!(loaded.get::<Stroke>(&brush_id), Some(&brush));
    assert_eq!(loaded.get::<RepairStroke>(&repair_id), Some(&repair));
    assert!(loaded.is_known_stored(&brush_id) && loaded.is_known_stored(&repair_id));
    let mut crossed = StrokeTable::new("entry entry-11");
    crossed.load(
        brush_id.clone(),
        StrokeType::of::<RepairStroke>(),
        Some(&brush.canonical()),
    );
    assert_eq!(
        crossed
            .resolve::<RepairStroke>(&brush_id)
            .unwrap_err()
            .detail,
        format!("stroke {brush_id} of entry entry-11 does not match its stored content address")
    );
}
