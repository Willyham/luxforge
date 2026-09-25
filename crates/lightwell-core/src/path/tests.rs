//! The host path primitives against their contract: what the generic parameter check accepts and
//! what it names when it refuses, that decimation is deterministic and bounded, that a stroke's
//! address is its contents, and that a broken reference is never an empty stroke.
use super::*;
use crate::{ParameterDescriptor, modules::check_value};
use serde_json::json;

/// A small, fixed linear congruential generator, so "randomized" here means a lot of different
/// shapes and not a different test on every run: a failure is reproducible from the seed printed
/// with it.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }

    /// A finite value in `[low, high)`.
    fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (self.next() % 1_000_000) as f64 / 1_000_000.0 * (high - low)
    }
}

fn points_parameter(points_min: usize, points_max: usize) -> ParameterDescriptor {
    ParameterDescriptor::points("path", points_min, points_max)
        .required(true)
        .notes("the drawn path")
}

/// A captured drag: a smooth arc with a little jitter on it, which is what a pointer produces and
/// what decimation has to shorten without moving.
fn captured(rng: &mut Rng, count: usize) -> Vec<[f64; 2]> {
    let (cx, cy) = (rng.range(0.2, 0.8), rng.range(0.2, 0.8));
    let radius = rng.range(0.05, 0.3);
    let sweep = rng.range(0.5, 6.0);
    (0..count)
        .map(|index| {
            let t = index as f64 / count as f64 * sweep;
            let wobble = rng.range(-0.0005, 0.0005);
            [
                (cx + (radius + wobble) * t.cos()).clamp(COORDINATE_MIN, COORDINATE_MAX),
                (cy + (radius + wobble) * t.sin()).clamp(COORDINATE_MIN, COORDINATE_MAX),
            ]
        })
        .collect()
}

#[test]
fn the_generic_check_accepts_a_path_and_names_every_refusal() {
    let declared = points_parameter(1, 64);
    let mut rng = Rng(0x5eed);
    // Whatever shape it has, a path of legal positions passes the one check every other parameter
    // kind passes through.
    for count in 1..=64 {
        let path = json!(captured(&mut rng, count));
        check_value(&declared, &path).expect("a legal path");
    }
    // The bounds of the coordinate range are inclusive on both sides.
    for edge in [COORDINATE_MIN, COORDINATE_MAX] {
        check_value(&declared, &json!([[edge, edge]])).expect("the range is closed");
    }

    let refusal = |value: serde_json::Value| check_value(&declared, &value).unwrap_err();
    assert_eq!(
        refusal(json!("a path")).detail,
        "parameter path must be a path"
    );
    assert_eq!(
        refusal(json!([])).detail,
        "parameter path must hold at least 1 positions"
    );
    assert_eq!(
        refusal(json!([[0.5]])).detail,
        "parameter path has a malformed position 0"
    );
    assert_eq!(
        refusal(json!([[0.5, 0.5], ["a", 0.5]])).detail,
        "parameter path has a malformed position 1"
    );
    assert_eq!(
        refusal(json!([[0.5, 0.5], [0.5, 0.5, 0.5]])).detail,
        "parameter path has a malformed position 1"
    );
    for (index, bad) in [
        (1, json!([[0.5, 0.5], [2.5, 0.5]])),
        (0, json!([[-1.5, 0.5]])),
        (0, json!([[0.5, 9.0]])),
        (2, json!([[0.5, 0.5], [0.4, 0.4], [0.5, f64::MAX]])),
    ] {
        assert_eq!(
            refusal(bad).detail,
            format!("parameter path position {index} must hold two numbers within -1..=2"),
            "the first illegal position is the one named",
        );
    }
    // A JSON integer is a number here as it is everywhere else in the API.
    check_value(&declared, &json!([[0, 1], [1, 0]])).expect("integers are numbers");
}

#[test]
fn a_path_over_the_declared_count_names_the_points_per_stroke_limit() {
    let declared = points_parameter(1, POINTS_PER_STROKE);
    let mut rng = Rng(7);
    let legal = captured(&mut rng, POINTS_PER_STROKE);
    check_value(&declared, &json!(legal)).expect("exactly the bound is legal");

    let one_more = captured(&mut rng, POINTS_PER_STROKE + 1);
    let error = check_value(&declared, &json!(one_more)).unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::ResourceLimit);
    assert_eq!(
        error.detail,
        format!(
            "parameter path has {} positions; the limit is {POINTS_PER_STROKE} points per stroke",
            POINTS_PER_STROKE + 1
        )
    );
}

#[test]
fn decimation_is_deterministic_and_stays_within_the_stated_deviation() {
    let mut rng = Rng(0xd1ce);
    for _ in 0..200 {
        let count = 2 + (rng.next() % 600) as usize;
        let path = captured(&mut rng, count);
        let once = decimate(&path).expect("a legal path");
        let twice = decimate(&path).expect("a legal path");
        assert_eq!(once, twice, "decimation reads nothing but its input");
        // Idempotent: the desktop decimates before it posts, and the host decimates what it was
        // posted, so the two must agree or a stored stroke would depend on who prepared it.
        assert_eq!(
            once,
            decimate(&once).expect("a decimated path is a legal path"),
            "decimating a decimated path changes nothing"
        );
        assert!(
            once.len() <= path.len(),
            "decimation never lengthens a path"
        );
        // The ends of a drag are never decimated away: what a person pressed and released on is
        // where the stored path starts and stops, snapped to the grid and nothing more.
        let on_grid = |[x, y]: [f64; 2]| {
            [
                (x * COORDINATE_STEPS_PER_UNIT).round() / COORDINATE_STEPS_PER_UNIT,
                (y * COORDINATE_STEPS_PER_UNIT).round() / COORDINATE_STEPS_PER_UNIT,
            ]
        };
        assert_eq!(once.first().copied(), path.first().copied().map(on_grid));
        assert_eq!(once.last().copied(), path.last().copied().map(on_grid));

        // Every captured position is within the stated deviation of the stored polyline. That is
        // the whole claim a tolerance makes, and it is checked against the positions that were
        // captured rather than against the ones that were kept.
        for point in &path {
            let distance = distance_to_polyline(*point, &once);
            assert!(
                distance <= STORED_DEVIATION,
                "a captured position moved {distance:e}, past the stated {STORED_DEVIATION:e}",
            );
        }
        // And every stored position is on the stored grid, at the declared precision and no finer.
        for [x, y] in &once {
            for value in [x, y] {
                let steps = value * COORDINATE_STEPS_PER_UNIT;
                assert_eq!(
                    steps,
                    steps.round(),
                    "a stored position is a whole grid step"
                );
            }
        }
    }
}

/// The distance from a point to a polyline, in normalized units. Test arithmetic only: nothing on a
/// production path takes a square root of a distance.
fn distance_to_polyline(point: [f64; 2], polyline: &[[f64; 2]]) -> f64 {
    let mut best = f64::INFINITY;
    if polyline.len() == 1 {
        let (dx, dy) = (point[0] - polyline[0][0], point[1] - polyline[0][1]);
        return (dx * dx + dy * dy).sqrt();
    }
    for pair in polyline.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let length2 = dx * dx + dy * dy;
        let t = if length2 > 0.0 {
            (((point[0] - a[0]) * dx + (point[1] - a[1]) * dy) / length2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (px, py) = (a[0] + t * dx, a[1] + t * dy);
        let (ex, ey) = (point[0] - px, point[1] - py);
        best = best.min((ex * ex + ey * ey).sqrt());
    }
    best
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
        let predecimated = decimate(&path).expect("a legal path");
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

/// A path that survives decimation at full length — every third position far enough off the line
/// between its neighbours to be kept — so the per-stroke bound is reached rather than decimated
/// away, and `Stroke::capture` refuses it one past the bound by name.
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
    let stroke = Stroke::capture(&at_bound, 0.05, 50.0, 100.0, false).expect("the bound is legal");
    assert_eq!(stroke.point_count(), POINTS_PER_STROKE);

    let error = Stroke::capture(
        &incompressible(POINTS_PER_STROKE + 1),
        0.05,
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
    assert_eq!(table.resolve(&known).unwrap(), &stroke);
    assert_eq!(table.get(&known), Some(&stroke));

    let missing = absent.id();
    let error = table.resolve(&missing).unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Incompatible);
    assert_eq!(
        error.detail,
        format!("stroke {missing} of entry entry-7 is not in the stroke store")
    );

    table.fault(missing.clone(), StrokeFault::Corrupt);
    assert_eq!(
        table.resolve(&missing).unwrap_err().detail,
        format!("stroke {missing} of entry entry-7 does not match its stored content address")
    );
    assert_eq!(table.get(&missing), None, "a fault is never a stroke");
}

#[test]
fn a_payload_declares_its_references_through_one_reserved_field() {
    let stroke = Stroke::capture(&[[0.1, 0.2], [0.4, 0.45]], 0.05, 50.0, 100.0, false).unwrap();
    let id = stroke.id();
    assert_eq!(
        references(&json!({"amount": 5}), "layer x").unwrap(),
        vec![]
    );
    assert_eq!(
        references(&json!({"strokes": [id.to_string()]}), "layer x").unwrap(),
        vec![id.clone()]
    );
    // The same stroke twice is two references to one object, which is the case the whole store
    // exists for.
    assert_eq!(
        references(
            &json!({"strokes": [id.to_string(), id.to_string()]}),
            "layer x"
        )
        .unwrap()
        .len(),
        2
    );
    for malformed in [
        json!({"strokes": "abc"}),
        json!({"strokes": [5]}),
        json!({"strokes": ["not-an-address"]}),
        json!({"strokes": [format!("{id}0")]}),
    ] {
        let error = references(&malformed, "component Brush 1").unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Validation);
        assert_eq!(
            error.detail,
            "component Brush 1 has a malformed strokes field: expected a list of stroke addresses"
        );
    }
}

#[test]
fn an_address_is_thirty_two_lowercase_hexadecimal_characters() {
    let id = StrokeId::of(b"anything");
    assert_eq!(id.as_str().len(), 32);
    assert!(id.as_str().bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(StrokeId::parse(id.to_string()).unwrap(), id);
    for bad in ["", "ABC", &id.to_string()[..31], &format!("{id}f")] {
        assert_eq!(
            StrokeId::parse(bad).unwrap_err().detail,
            "invalid StrokeId: expected 32 lowercase hexadecimal characters"
        );
    }
    // One reference is 35 bytes of an entry's JSON — the quoted address and its comma — which is
    // the whole of what the store buys against embedding a stroke's positions.
    assert_eq!(serde_json::to_string(&id).unwrap().len() + 1, 35);
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
        clone.strokes.get(&first).unwrap(),
        recipe.strokes.get(&first).unwrap()
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
        recipe.strokes.get(&second).is_none(),
        "and leaves the original alone"
    );
    assert_eq!(recipe.strokes.strokes().count(), 1);
    assert_eq!(next.strokes.strokes().count(), 2);
    assert!(
        std::ptr::eq(
            next.strokes.get(&first).unwrap(),
            recipe.strokes.get(&first).unwrap()
        ),
        "the copy holds the same stroke, not a copy of its positions"
    );
}
