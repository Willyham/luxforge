//! The host path primitives against their contract: what the generic parameter check accepts and
//! what it names when it refuses, that decimation is deterministic and bounded, that a stroke's
//! address is its contents, and that a broken reference is never an empty stroke.
use super::*;
use crate::{ParameterDescriptor, modules::check_value};
use serde_json::json;

/// A small, fixed linear congruential generator, so "randomized" here means a lot of different
/// shapes and not a different test on every run: a failure is reproducible from the seed printed
/// with it.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }

    /// A finite value in `[low, high)`.
    pub(crate) fn range(&mut self, low: f64, high: f64) -> f64 {
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
pub(crate) fn captured(rng: &mut Rng, count: usize) -> Vec<[f64; 2]> {
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

/// A posted path is bounded before decimation by the posted bound, which is its own number and
/// its own sentence: generous, so a raw path past the stored bound is posted, and named as the
/// posted bound rather than as the stored one when it is passed.
#[test]
fn a_posted_path_over_its_declared_count_names_the_posted_bound() {
    const { assert!(POSTED_POINTS_PER_STROKE > POINTS_PER_STROKE) };
    let declared = points_parameter(1, POSTED_POINTS_PER_STROKE);
    let mut rng = Rng(7);
    let raw = captured(&mut rng, POINTS_PER_STROKE * 2);
    check_value(&declared, &json!(raw)).expect("a raw path past the stored bound is posted");
    let legal = captured(&mut rng, POSTED_POINTS_PER_STROKE);
    check_value(&declared, &json!(legal)).expect("exactly the bound is legal");

    let one_more = captured(&mut rng, POSTED_POINTS_PER_STROKE + 1);
    let error = check_value(&declared, &json!(one_more)).unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::ResourceLimit);
    assert_eq!(
        error.detail,
        format!(
            "parameter path has {} positions; the limit is {POSTED_POINTS_PER_STROKE} positions \
             posted per path",
            POSTED_POINTS_PER_STROKE + 1
        )
    );
}

#[test]
fn live_capture_stops_at_its_bound_and_never_returns_a_truncated_success() {
    let mut capture = PathCapture::default();
    for index in 0..CAPTURED_POINTS_PER_STROKE {
        capture.push([index as f64 / COORDINATE_STEPS_PER_UNIT, 0.25]);
    }
    assert_eq!(capture.grid.len(), CAPTURED_POINTS_PER_STROKE);
    assert!(capture.capture_error().is_none());
    assert_eq!(capture.decimated(0.1).unwrap().len(), 2);
    // Consecutive repeats store nothing and do not consume the distinct-position bound.
    capture.push([
        (CAPTURED_POINTS_PER_STROKE - 1) as f64 / COORDINATE_STEPS_PER_UNIT,
        0.25,
    ]);
    assert!(capture.capture_error().is_none());
    capture.push([1.1, 0.25]);
    let error = capture.decimated(0.1).unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::ResourceLimit);
    assert!(error.detail.contains("16384 captured positions"));
    for _ in 0..10_000 {
        capture.push([0.5, 0.8]);
    }
    assert_eq!(capture.grid.len(), CAPTURED_POINTS_PER_STROKE);
    assert_eq!(capture.decimated(0.1).unwrap_err().detail, error.detail);
}

#[test]
fn live_capture_keeps_the_first_invalid_coordinate_reason() {
    for value in [f64::NAN, f64::INFINITY, -1.01, 2.01] {
        let mut capture = PathCapture::default();
        capture.push([0.2, 0.3]);
        capture.push([value, 0.4]);
        capture.push([0.4, 0.5]);
        let error = capture.decimated(0.1).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Validation);
        assert_eq!(
            error.detail,
            "path position 1 x must be a number within -1..=2"
        );
        assert_eq!(capture.grid.len(), 1);
        assert_eq!(capture.capture_error().unwrap().detail, error.detail);
    }
}

/// The radii decimation is measured at: the mask brush's declared minimum and maximum and the
/// desktop's default brush between them, written out here because the brush's range is the brush's
/// and not this file's.
const SIZE_MIN: f64 = 1e-4;
const SIZE_MAX: f64 = 2.0;
const SIZES: [f64; 3] = [SIZE_MIN, 0.1, SIZE_MAX];

#[test]
fn decimation_is_deterministic_and_stays_within_the_stated_deviation() {
    let mut rng = Rng(0xd1ce);
    for index in 0..300 {
        let size = SIZES[index % SIZES.len()];
        let count = 2 + (rng.next() % 600) as usize;
        let path = captured(&mut rng, count);
        let once = decimate(&path, size).expect("a legal path");
        let twice = decimate(&path, size).expect("a legal path");
        assert_eq!(once, twice, "decimation reads nothing but its input");
        // Idempotent: the desktop decimates before it posts, and the host decimates what it was
        // posted, so the two must agree or a stored stroke would depend on who prepared it.
        assert_eq!(
            once,
            decimate(&once, size).expect("a decimated path is a legal path"),
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
        let bound = stored_deviation(size);
        for point in &path {
            let distance = distance_to_polyline(*point, &once);
            assert!(
                distance <= bound,
                "a captured position moved {distance:e}, past the stated {bound:e} at size {size}",
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

/// Whole-path decimation written out in one pass, as a stepwise reference for [`PathCapture`]: check
/// and snap every position of the slice, drop consecutive repeats, then reduce. It shares only the
/// reduction and the grid rules with the code under test, never the capture.
fn reference_decimate(points: &[[f64; 2]], size: f64) -> Result<Vec<[f64; 2]>, String> {
    if !radius_is_decimable(size) {
        return Err("illegal radius".into());
    }
    if points.is_empty() {
        return Err("empty".into());
    }
    let mut snapped: Vec<[i32; 2]> = Vec::new();
    for (index, [x, y]) in points.iter().enumerate() {
        for (axis, value) in [("x", *x), ("y", *y)] {
            if !value.is_finite() || !(COORDINATE_MIN..=COORDINATE_MAX).contains(&value) {
                return Err(format!("position {index} {axis}"));
            }
        }
        let point = [
            (x * COORDINATE_STEPS_PER_UNIT).round() as i32,
            (y * COORDINATE_STEPS_PER_UNIT).round() as i32,
        ];
        if snapped.last() != Some(&point) {
            snapped.push(point);
        }
    }
    let steps = (size * COORDINATE_STEPS_PER_UNIT).round() as i32;
    Ok(reduce(&snapped, tolerance_steps(steps))
        .into_iter()
        .map(|[x, y]| {
            [
                f64::from(x) / COORDINATE_STEPS_PER_UNIT,
                f64::from(y) / COORDINATE_STEPS_PER_UNIT,
            ]
        })
        .collect())
}

/// What a refusal says, reduced to what the reference names: which position, on which axis.
fn refusal(error: &Error) -> String {
    if error.detail == "a path must hold at least one position" {
        return "empty".into();
    }
    let words: Vec<&str> = error.detail.split(' ').collect();
    match words.as_slice() {
        ["path", "position", index, axis, ..] => format!("position {index} {axis}"),
        _ => error.detail.clone(),
    }
}

/// A random walk: long jumps, steps inside one grid cell that snap onto the position before them,
/// a pointer held still, and turns in every direction.
fn wandering(rng: &mut Rng, count: usize) -> Vec<[f64; 2]> {
    let mut at = [rng.range(0.2, 1.2), rng.range(0.2, 0.8)];
    (0..count)
        .map(|_| {
            let step = match rng.next() % 4 {
                0 => 0.0,
                1 => 0.2 / COORDINATE_STEPS_PER_UNIT,
                2 => rng.range(0.0, 0.004),
                _ => rng.range(0.0, 0.05),
            };
            let angle = rng.range(0.0, std::f64::consts::TAU);
            at = [
                (at[0] + step * angle.cos()).clamp(0.0, 1.5),
                (at[1] + step * angle.sin()).clamp(0.0, 1.0),
            ];
            at
        })
        .collect()
}

/// A sine across the frame, the shape `editor-latency --mode paint` paints, at a random amplitude
/// and frequency.
fn sine(rng: &mut Rng, count: usize) -> Vec<[f64; 2]> {
    let (amplitude, turns) = (rng.range(0.01, 0.3), rng.range(0.5, 6.0));
    (0..count)
        .map(|index| {
            let t = index as f64 / count.max(2) as f64;
            [
                0.2 + 0.8 * t,
                0.5 + amplitude * (std::f64::consts::TAU * turns * t).sin(),
            ]
        })
        .collect()
}

/// The capture a gesture takes one position at a time decimates, after every position, exactly as
/// the whole path it has drawn so far decimates in one pass: over curved, wandering and randomized
/// paths, at every declared radius and random ones between, repeats and a still pointer included.
/// So the stroke the desktop posts after each pointer event is the one whole-path decimation posts,
/// bit for bit, and its content address and its coverage are unchanged.
#[test]
fn a_capture_taken_one_position_at_a_time_decimates_as_the_whole_path_does() {
    let mut rng = Rng(0x0005_ca97);
    for case in 0..240 {
        let size = if case % 4 == 3 {
            rng.range(SIZE_MIN, 0.4)
        } else {
            SIZES[case % SIZES.len()]
        };
        let count = 1 + (rng.next() % 400) as usize;
        let path = match case % 3 {
            0 => captured(&mut rng, count),
            1 => wandering(&mut rng, count),
            _ => sine(&mut rng, count),
        };
        let mut capture = PathCapture::default();
        for (index, point) in path.iter().enumerate() {
            capture.push(*point);
            assert_eq!(capture.len(), index + 1);
            assert_eq!(
                capture.decimated(size).map_err(|error| refusal(&error)),
                reference_decimate(&path[..=index], size),
                "case {case} at position {index} of {count}, size {size}"
            );
        }
        assert_eq!(
            decimate(&path, size).map_err(|error| refusal(&error)),
            reference_decimate(&path, size),
            "case {case}: the whole path at once"
        );
    }
}

/// A capture refuses what whole-path decimation refuses, in the same words: nothing at all, a
/// position outside the stored range — named by its index and axis, and kept refused whatever comes
/// after it, since the stroke it would store is not the one drawn — and a radius no path is decimated for.
#[test]
fn a_capture_refuses_what_whole_path_decimation_refuses() {
    let empty = PathCapture::default();
    assert!(empty.is_empty());
    assert_eq!(
        refusal(&empty.decimated(0.1).expect_err("nothing to decimate")),
        "empty"
    );
    let path = [
        [0.2, 0.2],
        [0.3, 0.3],
        [0.4, COORDINATE_MAX + 0.5],
        [0.5, 0.5],
    ];
    let mut capture = PathCapture::default();
    for (index, point) in path.iter().enumerate() {
        capture.push(*point);
        let whole = decimate(&path[..=index], 0.1);
        let taken = capture.decimated(0.1);
        match (whole, taken) {
            (Ok(whole), Ok(taken)) => assert_eq!(whole, taken),
            (Err(whole), Err(taken)) => {
                assert_eq!(whole.kind, taken.kind);
                assert_eq!(whole.detail, taken.detail);
            }
            (whole, taken) => panic!("at {index}: {whole:?} against {taken:?}"),
        }
    }
    assert_eq!(
        refusal(
            &capture
                .decimated(0.1)
                .expect_err("an out-of-range position")
        ),
        "position 2 y"
    );
    let mut nan = PathCapture::default();
    nan.push([f64::NAN, 0.5]);
    assert_eq!(
        refusal(&nan.decimated(0.1).expect_err("not a number")),
        "position 0 x"
    );
    let mut legal = PathCapture::default();
    legal.push([0.5, 0.5]);
    assert_eq!(
        legal
            .decimated(SIZE_MAX * 2.0)
            .expect_err("too large")
            .detail,
        decimate(&[[0.5, 0.5]], SIZE_MAX * 2.0)
            .expect_err("too large")
            .detail
    );
}

/// The tolerance is a share of the stroke's radius as stored, and never under two grid steps: at the
/// declared minimum it is the floor, at the default and the maximum it is four per cent of the radius.
/// So one jagged path keeps fewer positions the larger the brush it is drawn with, and a bump a
/// small brush keeps is one a large brush drops.
#[test]
fn the_tolerance_is_a_share_of_the_radius_and_never_under_two_grid_steps() {
    let stored = |size: f64| (size * COORDINATE_STEPS_PER_UNIT).round() / COORDINATE_STEPS_PER_UNIT;
    assert_eq!(decimation_tolerance(SIZE_MIN), DECIMATION_TOLERANCE_MIN);
    assert_eq!(DECIMATION_TOLERANCE_MIN * COORDINATE_STEPS_PER_UNIT, 2.0);
    for size in [0.1, SIZE_MAX] {
        assert_eq!(
            decimation_tolerance(size),
            DECIMATION_TOLERANCE_OF_RADIUS * stored(size),
            "size {size}"
        );
    }
    assert_eq!(
        stored_deviation(0.1),
        decimation_tolerance(0.1) + GRID_ROUNDING
    );

    // A bump of ten grid steps across a straight run: past the floor and past four per cent of a
    // small brush, inside four per cent of the default one.
    let bump = 10.0 / COORDINATE_STEPS_PER_UNIT;
    let path = [[0.1, 0.5], [0.3, 0.5 + bump], [0.5, 0.5]];
    assert_eq!(
        decimate(&path, SIZE_MIN).unwrap().len(),
        3,
        "the floor keeps it"
    );
    assert_eq!(
        decimate(&path, 0.01).unwrap().len(),
        3,
        "a small brush keeps it"
    );
    assert_eq!(
        decimate(&path, 0.1).unwrap().len(),
        2,
        "the default brush drops it"
    );

    // A pointer's whole-pixel staircase stores fewer positions the larger the brush.
    let mut rng = Rng(0x57a1);
    let jagged: Vec<[f64; 2]> = (0..400)
        .map(|index| {
            let t = index as f64 / 400.0;
            let wobble = rng.range(-1.0, 1.0);
            [
                ((0.2 + 0.6 * t) * 1000.0 + wobble).round() / 1000.0,
                ((0.5 + 0.1 * (t * 6.0).sin()) * 1000.0 + wobble).round() / 1000.0,
            ]
        })
        .collect();
    let kept: Vec<usize> = SIZES
        .iter()
        .map(|size| decimate(&jagged, *size).unwrap().len())
        .collect();
    assert!(
        kept[0] > kept[1] && kept[1] > kept[2],
        "stored positions at the minimum, default and maximum sizes: {kept:?}"
    );

    // A radius no path can be decimated for is refused by name, whatever a consumer allows.
    for size in [0.0, f64::NAN, SIZE_MAX * 2.0] {
        assert!(
            decimate(&path, size)
                .unwrap_err()
                .detail
                .starts_with("path radius must be a positive number"),
            "{size}"
        );
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
fn a_payload_declares_its_references_through_one_reserved_field() {
    let id = RepairStroke::capture(&[[0.1, 0.2], [0.4, 0.45]], 0.05, [0.0, 0.1])
        .unwrap()
        .id();
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

/// A second painting consumer, declared only here: a repair stroke of the kind the Corrections
/// proposal describes, with a payload of its own — the offset its source is taken from — and a
/// radius range of its own, narrower than the mask brush's. Nothing in production declares it; it
/// proves that the store holds a consumer-declared stroke type and commits it by the same rules as
/// the brush's, and it names no mask.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepairStroke {
    points: Vec<[i32; 2]>,
    radius: i32,
    source: [i32; 2],
}

/// The repair consumer's own radius range, which is not the brush's.
const REPAIR_RADIUS_MIN: f64 = 0.01;
const REPAIR_RADIUS_MAX: f64 = 0.5;

impl RepairStroke {
    /// Capture a repair stroke: its own radius and source offset checked by name first, then the
    /// path captured onto the host's grid exactly as the brush's is.
    pub(crate) fn capture(
        points: &[[f64; 2]],
        radius: f64,
        source: [f64; 2],
    ) -> Result<Self, Error> {
        if !radius.is_finite() || !(REPAIR_RADIUS_MIN..=REPAIR_RADIUS_MAX).contains(&radius) {
            return Err(Error::validation(format!(
                "repair radius must be a number within {REPAIR_RADIUS_MIN}..={REPAIR_RADIUS_MAX}"
            )));
        }
        if source
            .iter()
            .any(|value| !value.is_finite() || !(-1.0..=1.0).contains(value))
        {
            return Err(Error::validation(
                "repair source offset must be two numbers within -1..=1",
            ));
        }
        let radius = quantize(radius);
        Ok(Self {
            points: capture_grid(points, radius)?,
            radius,
            source: [quantize(source[0]), quantize(source[1])],
        })
    }

    pub(crate) fn radius(&self) -> f64 {
        steps_to_units(self.radius)
    }

    /// The reference a recipe part carrying this stroke would declare.
    pub(crate) fn reference(&self, what: &str) -> StrokeReference {
        StrokeReference {
            what: what.to_owned(),
            id: self.id(),
            kind: StrokeType::of::<Self>(),
        }
    }
}

impl StrokeKind for RepairStroke {
    const NAME: &'static str = "test repair";

    fn canonical(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a repair stroke is serializable")
    }

    fn parse_stored(id: &StrokeId, stored: &[u8]) -> Result<Self, Error> {
        let stroke: Self = serde_json::from_slice(stored).map_err(|error| {
            Error::incompatible(format!("stored stroke {id} is malformed: {error}"))
        })?;
        let bad = |what: &str| {
            Err(Error::incompatible(format!(
                "stored stroke {id} has an invalid {what}"
            )))
        };
        if let Some(what) = stored_path_fault(&stroke.points) {
            return bad(what);
        }
        if !(REPAIR_RADIUS_MIN..=REPAIR_RADIUS_MAX).contains(&stroke.radius()) {
            return bad("radius");
        }
        let reach = quantize(1.0);
        if stroke.source.iter().any(|value| value.abs() > reach) {
            return bad("source offset");
        }
        Ok(stroke)
    }
}

/// A second consumer declares its own stroke type against the same grid, decimation and store: it
/// captures a path onto the grid as the brush does, holds its own radius range and its own payload,
/// is addressed by its own canonical bytes, round-trips through them, and is held to its own ranges
/// when it is read back — none of which this file or the store knows anything about.
#[test]
fn a_second_consumer_declares_its_own_stroke_type_against_the_same_store() {
    let mut rng = Rng(0x2e9a);
    let path = captured(&mut rng, 200);
    let stroke = RepairStroke::capture(&path, 0.05, [0.1, -0.2]).expect("a legal repair");
    // The path is the host's: the same decimation at the same radius a brush stroke would take.
    assert_eq!(
        from_grid(&stroke.points).collect::<Vec<_>>(),
        decimate(&path, 0.05).unwrap()
    );
    assert_eq!(
        stroke.id(),
        RepairStroke::capture(&path, 0.05, [0.1, -0.2])
            .unwrap()
            .id(),
        "one captured path has one address, for this consumer too"
    );
    assert_ne!(
        stroke.id(),
        RepairStroke::capture(&path, 0.05, [0.1, 0.2]).unwrap().id(),
        "the consumer's own payload is part of what is addressed"
    );

    // Its limits are its own: a radius the brush takes is refused here by this consumer's name.
    assert_eq!(
        RepairStroke::capture(&path, 1.5, [0.0, 0.0])
            .unwrap_err()
            .detail,
        "repair radius must be a number within 0.01..=0.5"
    );
    // And the host's rules on a captured path hold for it as for every consumer.
    let error = RepairStroke::capture(&[], 0.05, [0.0, 0.0]).unwrap_err();
    assert_eq!(error.detail, "a path must hold at least one position");

    // Stored bytes round-trip to the same stroke, and are checked against the address and against
    // the consumer's own ranges on the way back in.
    let id = stroke.id();
    assert_eq!(
        RepairStroke::from_stored(&id, &stroke.canonical()).unwrap(),
        stroke
    );
    let mut corrupt = stroke.canonical();
    corrupt.push(b' ');
    assert_eq!(
        RepairStroke::from_stored(&id, &corrupt).unwrap_err().detail,
        format!("stored stroke {id} does not match its content address")
    );
    let wide = br#"{"points":[[1638,3277]],"radius":16384,"source":[0,0]}"#;
    let wide_id = StrokeId::of(wide);
    assert_eq!(
        RepairStroke::from_stored(&wide_id, wide)
            .unwrap_err()
            .detail,
        format!("stored stroke {wide_id} has an invalid radius")
    );

    // The store holds it as the type it was declared, refuses a broken reference to it by name,
    // and shares on clone and copies on write exactly as it does for any stroke.
    let mut table = StrokeTable::new("entry entry-3");
    assert_eq!(table.insert(stroke.clone()), id);
    assert_eq!(table.get::<RepairStroke>(&id), Some(&stroke));
    assert_eq!(table.resolve::<RepairStroke>(&id).unwrap(), &stroke);
    assert_eq!(table.stored_bytes(&id), Some(stroke.canonical()));
    let absent = RepairStroke::capture(&[[0.9, 0.9]], 0.05, [0.0, 0.0])
        .unwrap()
        .id();
    assert_eq!(
        table.resolve::<RepairStroke>(&absent).unwrap_err().detail,
        format!("stroke {absent} of entry entry-3 is not in the stroke store")
    );
    let shared = table.clone();
    assert!(shared.shares(&table));
    let mut written = table.clone();
    written.insert(RepairStroke::capture(&[[0.2, 0.2]], 0.02, [0.0, 0.0]).unwrap());
    assert!(!written.shares(&table), "a write copies on write");
    assert!(std::ptr::eq(
        written.get::<RepairStroke>(&id).unwrap(),
        table.get::<RepairStroke>(&id).unwrap()
    ));
}

/// A stroke read from the store is read as the type its reference declares and is marked known
/// stored; bytes that are not a legal stroke of that type are corrupt, and absent bytes are missing.
#[test]
fn a_reference_loads_as_the_type_it_declares() {
    let stroke = RepairStroke::capture(&[[0.1, 0.1], [0.3, 0.2]], 0.05, [0.0, 0.1]).unwrap();
    let reference = stroke.reference("repair 1");
    let mut table = StrokeTable::new("entry entry-4");
    assert!(!table.knows(&reference.id));
    table.load(
        reference.id.clone(),
        reference.kind,
        Some(&stroke.canonical()),
    );
    assert!(table.knows(&reference.id));
    assert!(table.is_known_stored(&reference.id));
    assert_eq!(table.get::<RepairStroke>(&reference.id), Some(&stroke));
    table.check_reference(&reference).unwrap();

    // Bytes that hash to their address but are not a stroke of the declared type are corrupt.
    let other = br#"{"not":"a repair"}"#;
    let other_id = StrokeId::of(other);
    table.load(other_id.clone(), reference.kind, Some(other));
    assert_eq!(
        table.resolve::<RepairStroke>(&other_id).unwrap_err().detail,
        format!("stroke {other_id} of entry entry-4 does not match its stored content address")
    );
    let gone = StrokeId::of(b"gone");
    table.load(gone.clone(), reference.kind, None);
    assert!(table.has_missing());
    assert!(!table.is_known_stored(&gone));
}
