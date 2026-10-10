//! The repair units against the independent `f64` reference (`luxforge_reference::repair`), bit for
//! bit: coverage, Clone, Heal and their order over flat, ramped, textured and high-contrast inputs,
//! tiny, large and self-crossing strokes, far and out-of-frame sources, the frame's edge, overlap,
//! and a developed RAW's continuous range; and the automatic source's ranking, rejections and
//! known answers.
use super::{
    RepairFrame, RepairKind, RepairOp, apply,
    search::{self, Search},
};
use crate::{
    Cancel, ErrorKind,
    modules::{Region, Stage},
    path,
};
use luxforge_reference::{SplitMix64, repair as reference};

/// One operation as a test writes it: a stored path, radius in content heights, feather and
/// opacity `0..=100`, offset in pixels of the test's stage (put on the grid).
#[derive(Clone, Debug)]
struct Op {
    kind: RepairKind,
    points: Vec<[f64; 2]>,
    radius: f64,
    feather: f64,
    opacity: f64,
    offset: [i32; 2],
}

impl Op {
    fn new(kind: RepairKind, stage: Stage, points_px: &[[f64; 2]], radius_px: f64) -> Self {
        let (w, h) = (f64::from(stage.width), f64::from(stage.height));
        Self {
            kind,
            points: points_px
                .iter()
                .map(|[x, y]| {
                    [
                        path::steps_to_units(path::quantize(x / w)),
                        path::steps_to_units(path::quantize(y / h)),
                    ]
                })
                .collect(),
            radius: path::steps_to_units(path::quantize(radius_px / h)),
            feather: 50.0,
            opacity: 100.0,
            offset: [0, 0],
        }
    }

    fn feather(mut self, feather: f64) -> Self {
        self.feather = feather;
        self
    }

    fn opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity;
        self
    }

    /// The source `dx, dy` pixels away, on the grid.
    fn source(mut self, stage: Stage, dx: f64, dy: f64) -> Self {
        self.offset = [
            path::quantize(dx / f64::from(stage.width)),
            path::quantize(dy / f64::from(stage.height)),
        ];
        self
    }

    fn core(&self, stage: Stage) -> RepairOp {
        RepairOp::new(
            self.kind,
            &self.points,
            self.radius,
            self.feather,
            self.opacity,
            self.offset,
            stage,
        )
    }

    fn reference(&self) -> reference::Operation {
        reference::Operation {
            kind: match self.kind {
                RepairKind::Clone => reference::Kind::Clone,
                RepairKind::Heal => reference::Kind::Heal,
                RepairKind::Patch => unreachable!("a stroke operation"),
            },
            paths: vec![self.points.clone()],
            radius: self.radius,
            feather: self.feather,
            opacity: self.opacity,
            offset: [
                path::steps_to_units(self.offset[0]),
                path::steps_to_units(self.offset[1]),
            ],
        }
    }
}

/// A planar frame of `stage` from a function of the pixel.
fn planes(stage: Stage, f: impl Fn(u32, u32) -> [f32; 3]) -> Vec<f32> {
    let n = (stage.width * stage.height) as usize;
    let mut values = vec![0.0; 3 * n];
    for y in 0..stage.height {
        for x in 0..stage.width {
            let at = (y * stage.width + x) as usize;
            let p = f(x, y);
            values[at] = p[0];
            values[n + at] = p[1];
            values[2 * n + at] = p[2];
        }
    }
    values
}

fn to_reference(stage: Stage, values: &[f32]) -> reference::Image {
    let n = (stage.width * stage.height) as usize;
    reference::Image::new(
        stage.width as usize,
        stage.height as usize,
        (0..n)
            .map(|at| {
                [
                    f64::from(values[at]),
                    f64::from(values[n + at]),
                    f64::from(values[2 * n + at]),
                ]
            })
            .collect(),
    )
}

/// The core's output over the whole stage.
fn core_apply(stage: Stage, input: &[f32], ops: &[Op]) -> Vec<f32> {
    let mut values = input.to_vec();
    let mut frame = RepairFrame::new(stage, Region::whole(stage), &mut values).unwrap();
    let compiled: Vec<RepairOp> = ops.iter().map(|op| op.core(stage)).collect();
    apply(&mut frame, &compiled, &Cancel::never()).unwrap();
    values
}

/// The core against the reference, every value bit for bit, and how many pixels changed.
fn assert_exact(name: &str, stage: Stage, input: &[f32], ops: &[Op]) -> usize {
    let core = core_apply(stage, input, ops);
    let ops_ref: Vec<reference::Operation> = ops.iter().map(Op::reference).collect();
    let expected = reference::apply(&to_reference(stage, input), &ops_ref);
    let n = (stage.width * stage.height) as usize;
    let mut changed = 0;
    for at in 0..n {
        for c in 0..3 {
            let (got, want) = (core[c * n + at], expected.pixels[at][c] as f32);
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "{name}: pixel ({}, {}) channel {c}: core {got} reference {want}",
                at % stage.width as usize,
                at / stage.width as usize
            );
        }
        if (0..3).any(|c| core[c * n + at].to_bits() != input[c * n + at].to_bits()) {
            changed += 1;
        }
    }
    changed
}

const STAGE: Stage = Stage {
    width: 240,
    height: 160,
};

fn texture(stage: Stage, seed: u64, base: f32, amplitude: f32) -> Vec<f32> {
    let mut rng = SplitMix64(seed);
    let noise: Vec<f32> = (0..stage.width * stage.height)
        .map(|_| rng.next_noise() as f32)
        .collect();
    planes(stage, |x, y| {
        let n = noise[(y * stage.width + x) as usize];
        let ramp = base * (1.0 + 0.5 * x as f32 / stage.width as f32);
        [
            ramp * (1.0 + amplitude * n),
            ramp * 0.8 * (1.0 + amplitude * n),
            ramp * 1.2,
        ]
    })
}

fn flat() -> Vec<f32> {
    planes(STAGE, |_, _| [0.18, 0.2, 0.22])
}

fn gradient() -> Vec<f32> {
    planes(STAGE, |x, y| {
        [
            0.02 + 0.7 * x as f32 / 240.0,
            0.05 + 0.5 * y as f32 / 160.0,
            0.3,
        ]
    })
}

fn edge() -> Vec<f32> {
    planes(
        STAGE,
        |x, _| if x < 120 { [0.01; 3] } else { [0.9, 0.85, 0.8] },
    )
}

/// A developed RAW's continuous range: highlights past 1 and a little below 0.
fn raw_range() -> Vec<f32> {
    let mut rng = SplitMix64(41);
    let noise: Vec<f32> = (0..STAGE.width * STAGE.height)
        .map(|_| rng.next_noise() as f32 * 0.02)
        .collect();
    planes(STAGE, |x, y| {
        let n = noise[(y * STAGE.width + x) as usize];
        let hot = if (x as i32 - 150).pow(2) + (y as i32 - 70).pow(2) < 900 {
            3.5
        } else {
            0.0
        };
        [
            -0.004 + 0.4 * x as f32 / 240.0 + hot + n,
            0.3 + hot * 0.8 - n,
            1.2 + n,
        ]
    })
}

use RepairKind::{Clone, Heal};

/// Coverage is the reference's, bit for bit, wherever the index sends a pixel and at every pixel
/// outside the support.
#[test]
fn coverage_is_the_reference_bit_for_bit() {
    let stage = STAGE;
    let cases = [
        Op::new(Heal, stage, &[[120.0, 80.0]], 20.0),
        Op::new(Heal, stage, &[[120.0, 80.0]], 20.0).feather(0.0),
        Op::new(Clone, stage, &[[3.0, 5.0]], 1.0).opacity(35.0),
        Op::new(
            Heal,
            stage,
            &[[20.0, 20.0], [200.0, 30.0], [60.0, 140.0], [230.0, 150.0]],
            6.0,
        )
        .feather(100.0),
        Op::new(Clone, stage, &[[-30.0, 80.0], [300.0, 90.0]], 12.0),
    ];
    for (n, op) in cases.iter().enumerate() {
        let core = op.core(stage);
        let compiled = reference::Compiled::new(&op.reference(), &reference_stage(stage));
        let support = core.writes();
        for y in 0..stage.height {
            for x in 0..stage.width {
                let want = compiled.coverage(&reference_stage(stage), x as usize, y as usize);
                let got = core.footprint.coverage(x, y);
                assert_eq!(got.to_bits(), want.to_bits(), "case {n} at ({x}, {y})");
                if !support.contains(x, y) {
                    assert_eq!(want, 0.0, "case {n}: support misses ({x}, {y})");
                }
            }
        }
    }
}

fn reference_stage(stage: Stage) -> luxforge_reference::mask::Stage {
    luxforge_reference::mask::Stage::new(stage.width, stage.height)
}

/// Clone over every input kind, at every edge setting, with near, far and out-of-frame sources and
/// sources that overlap their own destination in each direction.
#[test]
fn clone_is_the_reference_bit_for_bit() {
    let s = STAGE;
    let inputs = [
        ("flat", flat()),
        ("gradient", gradient()),
        ("texture", texture(s, 3, 0.2, 0.4)),
        ("edge", edge()),
        ("raw range", raw_range()),
    ];
    let spot = |x, y, r| Op::new(Clone, s, &[[x, y]], r);
    let ops = [
        spot(60.0, 60.0, 12.0).source(s, 40.0, 0.0),
        spot(60.0, 60.0, 12.0).source(s, -30.0, 25.0).feather(0.0),
        spot(120.0, 80.0, 30.0).source(s, 9.0, -7.0),
        spot(120.0, 80.0, 30.0).source(s, -9.0, 0.0).opacity(60.0),
        spot(20.0, 140.0, 15.0).source(s, 200.0, -120.0),
        spot(200.0, 30.0, 18.0).source(s, 50.0, 0.0),
        spot(5.0, 5.0, 1.0).source(s, 3.0, 2.0),
        Op::new(
            Clone,
            s,
            &[[30.0, 30.0], [210.0, 130.0], [30.0, 130.0], [210.0, 30.0]],
            5.0,
        )
        .source(s, 0.0, 9.0),
    ];
    for (name, input) in &inputs {
        for (n, op) in ops.iter().enumerate() {
            let changed = assert_exact(&format!("{name} {n}"), s, input, std::slice::from_ref(op));
            if *name == "texture" {
                assert!(changed > 0, "{name} {n} repaired nothing");
            }
        }
    }
}

/// Heal over every input kind: spots from one pixel to most of the frame, hard and soft edges,
/// half and full opacity, a stroke that crosses itself, sources far away, beyond the frame and
/// overlapping the destination, and a footprint on the frame's edge.
#[test]
fn heal_is_the_reference_bit_for_bit() {
    let s = STAGE;
    let inputs = [
        ("flat", flat()),
        ("gradient", gradient()),
        ("texture", texture(s, 5, 0.25, 0.5)),
        ("edge", edge()),
        ("raw range", raw_range()),
    ];
    let spot = |x, y, r| Op::new(Heal, s, &[[x, y]], r);
    let ops = [
        spot(120.0, 80.0, 1.0).source(s, 5.0, 0.0),
        spot(118.0, 80.0, 14.0).source(s, 0.0, 40.0),
        spot(118.0, 80.0, 14.0).source(s, 0.0, 40.0).feather(0.0),
        spot(118.0, 80.0, 14.0).source(s, -50.0, 3.0).opacity(50.0),
        spot(120.0, 80.0, 70.0).source(s, 100.0, 0.0),
        spot(4.0, 80.0, 20.0).source(s, 60.0, 0.0),
        spot(200.0, 120.0, 25.0).source(s, 30.0, 30.0),
        spot(100.0, 70.0, 22.0).source(s, 11.0, 4.0),
        spot(60.0, 40.0, 10.0).source(s, -180.0, 100.0),
        Op::new(
            Heal,
            s,
            &[
                [20.0, 30.0],
                [220.0, 40.0],
                [120.0, 150.0],
                [100.0, 10.0],
                [20.0, 120.0],
            ],
            7.0,
        )
        .source(s, 4.0, 16.0)
        .feather(30.0),
    ];
    for (name, input) in &inputs {
        for (n, op) in ops.iter().enumerate() {
            assert_exact(&format!("{name} {n}"), s, input, std::slice::from_ref(op));
        }
    }
}

/// Operations evaluate in stored order, each reading only what the ones before it left: a later
/// operation painted over an earlier one repairs the repair, a source over an earlier destination
/// samples the earlier output, and nothing a later operation does reaches an earlier one's output
/// outside the later one's own pixels.
#[test]
fn later_operations_never_feed_earlier_ones() {
    let s = STAGE;
    let input = texture(s, 9, 0.3, 0.6);
    let first = Op::new(Heal, s, &[[80.0, 80.0]], 16.0).source(s, 60.0, 0.0);
    let second = Op::new(Clone, s, &[[140.0, 80.0]], 16.0).source(s, -60.0, 0.0);
    let third = Op::new(Heal, s, &[[95.0, 82.0], [130.0, 78.0]], 9.0).source(s, 0.0, 35.0);
    let both = [first.clone(), second.clone(), third.clone()];
    assert_exact("stack", s, &input, &both);
    // Reordered, the result differs, and each order is still the reference's.
    let reordered = [third.clone(), second.clone(), first.clone()];
    assert_exact("reordered", s, &input, &reordered);
    let a = core_apply(s, &input, &both);
    let b = core_apply(s, &input, &reordered);
    assert_ne!(a, b, "order matters where operations overlap");
    // The first operation's output outside the later ones' pixels is what it is alone.
    let alone = core_apply(s, &input, std::slice::from_ref(&first));
    let later: Vec<Region> = [&second, &third]
        .iter()
        .map(|op| op.core(s).writes())
        .collect();
    let n = (s.width * s.height) as usize;
    for y in 0..s.height {
        for x in 0..s.width {
            if later.iter().any(|r| r.contains(x, y)) {
                continue;
            }
            let at = (y * s.width + x) as usize;
            for c in 0..3 {
                assert_eq!(a[c * n + at].to_bits(), alone[c * n + at].to_bits());
            }
        }
    }
    // The same operation twice heals its own repair.
    assert_exact("twice", s, &input, &[first.clone(), first]);
}

/// Every pixel outside an operation's support is its input exactly, and the declared reads are
/// enough: a frame holding just the union of them evaluates to the whole frame's answer, and one
/// that misses a declared read is refused before anything is written.
#[test]
fn reads_are_declared_and_the_rest_is_untouched() {
    let s = STAGE;
    let input = texture(s, 13, 0.2, 0.3);
    let op = Op::new(Heal, s, &[[60.0, 50.0], [90.0, 70.0]], 10.0).source(s, 120.0, 60.0);
    let whole = core_apply(s, &input, std::slice::from_ref(&op));
    let compiled = op.core(s);
    let [destination, source] = compiled.reads();
    let x0 = destination.x0.min(source.x0);
    let y0 = destination.y0.min(source.y0);
    let x1 = destination.x1().max(source.x1());
    let y1 = destination.y1().max(source.y1());
    let region = Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    };
    let n = (s.width * s.height) as usize;
    let mut held: Vec<f32> = Vec::new();
    for c in 0..3 {
        for y in region.y0..region.y1() {
            for x in region.x0..region.x1() {
                held.push(input[c * n + (y * s.width + x) as usize]);
            }
        }
    }
    let mut frame = RepairFrame::new(s, region, &mut held).unwrap();
    apply(
        &mut frame,
        std::slice::from_ref(&compiled),
        &Cancel::never(),
    )
    .unwrap();
    let support = compiled.writes();
    let m = region.pixels() as usize;
    for y in region.y0..region.y1() {
        for x in region.x0..region.x1() {
            let at = (y * s.width + x) as usize;
            let local = ((y - region.y0) * region.width + (x - region.x0)) as usize;
            for c in 0..3 {
                assert_eq!(held[c * m + local].to_bits(), whole[c * n + at].to_bits());
                if !support.contains(x, y) {
                    assert_eq!(whole[c * n + at].to_bits(), input[c * n + at].to_bits());
                }
            }
        }
    }
    // A frame without the source is refused, unchanged.
    let mut short = held.clone();
    let narrow = Region {
        width: region.width - 1,
        ..region
    };
    short.truncate(3 * narrow.pixels() as usize);
    let mut frame = RepairFrame::new(s, narrow, &mut short).unwrap();
    let error = apply(&mut frame, &[compiled], &Cancel::never()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Internal);
}

/// A cancelled token stops a Heal before it writes; a Heal past its cell bound is refused with
/// `resource-limit` naming the operation, the frame unchanged.
#[test]
fn heal_is_cancellable_and_bounded() {
    let s = STAGE;
    let input = texture(s, 17, 0.2, 0.3);
    let op = Op::new(Heal, s, &[[120.0, 80.0]], 40.0).source(s, 60.0, 0.0);
    let mut values = input.clone();
    let mut frame = RepairFrame::new(s, Region::whole(s), &mut values).unwrap();
    let cancel = Cancel::new();
    cancel.cancel();
    let error = apply(&mut frame, &[op.core(s)], &cancel).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(values, input);
    let mut frame = RepairFrame::new(s, Region::whole(s), &mut values).unwrap();
    let error =
        super::heal::heal_bounded(&mut frame, &op.core(s), &Cancel::never(), 1024).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert_eq!(values, input);
}

// ---- The automatic source ---------------------------------------------------------------------

/// The core's ranking over the whole stage against the reference's, and the reference's answer.
fn ranked(stage: Stage, input: &[f32], op: &Op) -> Search {
    let mut values = input.to_vec();
    let frame = RepairFrame::new(stage, Region::whole(stage), &mut values).unwrap();
    let plan = search::plan(&op.points, op.radius, stage);
    let r = plan.r;
    let core = if plan.region.is_empty() {
        search::rank(&plan, &search::Reduced::empty(r))
    } else {
        let reduced = search::reduce(&frame, plan.region, r, &Cancel::never()).unwrap();
        search::rank(&plan, &reduced)
    };
    // The reference over the whole reduced stage.
    let image = to_reference(stage, input);
    let reference_reduced = reference::reduced(&image, r as usize);
    let expected = reference::search(
        &reference_reduced,
        &op.reference(),
        &reference_stage(stage),
        r as usize,
        [
            0,
            0,
            reference_reduced.width as i64,
            reference_reduced.height as i64,
        ],
    );
    match (&core, &expected) {
        (Search::NoSource, reference::Search::NoSource) => {}
        (Search::Ranked(core), reference::Search::Ranked(expected)) => {
            assert_eq!(core.len(), expected.len());
            for (c, e) in core.iter().zip(expected) {
                assert_eq!(
                    (c.ring, c.direction, c.delta),
                    (e.ring, e.direction, e.delta)
                );
                assert_eq!(c.score.to_bits(), e.score.to_bits());
                assert_eq!(c.offset.map(f64::to_bits), e.offset.map(f64::to_bits));
            }
        }
        _ => panic!("core {core:?} against reference {expected:?}"),
    }
    core
}

fn best(search: Search) -> search::Candidate {
    match search {
        Search::Ranked(ranked) => ranked[0],
        Search::NoSource => panic!("no source"),
    }
}

/// The candidate set, rejections, score and tie-break on textures whose answer is known, and the
/// core's ranking the reference's on each, and on photographic-scale stages where the reduction is
/// above one.
#[test]
fn search_is_the_reference_and_finds_the_known_answers() {
    let stage = Stage {
        width: 400,
        height: 300,
    };
    // Flat: the nearest surviving ring (a spot overlaps its own 1.5 E ring), smallest angle.
    let flat = planes(stage, |_, _| [0.18; 3]);
    let spot = Op::new(Heal, stage, &[[200.0, 150.0]], 20.0);
    let best_flat = best(ranked(stage, &flat, &spot));
    assert_eq!((best_flat.ring, best_flat.direction), (1, 0));
    assert_eq!(best_flat.score, search::DISTANCE_WEIGHT * search::RINGS[1]);
    // A vertical gradient: along its level lines, the tie to the smaller angle.
    let gradient = planes(stage, |_, y| [0.05 + 0.6 * y as f32 / 300.0; 3]);
    let Search::Ranked(order) = ranked(stage, &gradient, &spot) else {
        panic!("a ranking");
    };
    assert_eq!(
        [
            (order[0].ring, order[0].direction),
            (order[1].ring, order[1].direction)
        ],
        [(1, 0), (1, 12)]
    );
    assert_eq!(order[0].score, order[1].score);
    // A horizontal stroke takes its 1.5 E ring off its own axis.
    let stroke = Op::new(Clone, stage, &[[170.0, 150.0], [230.0, 150.0]], 6.0).feather(0.0);
    let best_stroke = best(ranked(stage, &planes(stage, |_, _| [0.3; 3]), &stroke));
    assert_eq!((best_stroke.ring, best_stroke.direction), (0, 2));
    // The frame's edge, and no source at all.
    let Search::Ranked(edge) = ranked(stage, &flat, &Op::new(Heal, stage, &[[30.0, 150.0]], 15.0))
    else {
        panic!("a ranking");
    };
    assert!(
        edge.iter()
            .all(|c| search::direction(c.direction)[0] > -0.5)
    );
    let small = Stage {
        width: 100,
        height: 100,
    };
    let none = ranked(
        small,
        &planes(small, |_, _| [0.3; 3]),
        &Op::new(Heal, small, &[[50.0, 50.0]], 30.0),
    );
    assert_eq!(none, Search::NoSource);
    // Texture keeps with texture.
    let mut rng = SplitMix64(5);
    let noise: Vec<f64> = (0..200 * 120).map(|_| rng.next_noise()).collect();
    let band = Stage {
        width: 200,
        height: 120,
    };
    let textured = planes(band, |x, y| {
        let n = if (40..80).contains(&y) {
            noise[(y * 200 + x) as usize]
        } else {
            0.0
        };
        [crate::colour::srgb::decode(0.5 + 0.04 * n) as f32; 3]
    });
    let chosen = best(ranked(
        band,
        &textured,
        &Op::new(Heal, band, &[[100.0, 60.0]], 10.0),
    ));
    let y = 60 + chosen.delta[1];
    assert!(y - 10 >= 40 && y + 10 < 80, "{chosen:?}");
    // A large spot on a larger stage reads at a reduction of four, and a stroke at another.
    let big = Stage {
        width: 1200,
        height: 800,
    };
    let photo = texture(big, 23, 0.2, 0.3);
    let large = Op::new(Heal, big, &[[600.0, 400.0]], 90.0);
    assert_eq!(
        search::reduction(search::extent(&large.points, large.radius, big), big),
        4
    );
    ranked(big, &photo, &large);
    ranked(
        big,
        &photo,
        &Op::new(
            Heal,
            big,
            &[[300.0, 200.0], [520.0, 260.0], [700.0, 240.0]],
            14.0,
        ),
    );
}

// ---- Photo-sized cost ---------------------------------------------------------------------------

/// The CPU units' cost on 24 and 60 MP stages, over a frame holding each operation's declared
/// reads: Heal and Clone spots of 50 and 200 pixels and of a quarter of the content height, a
/// hair-thin stroke across the frame, a broad stroke, and the automatic source for each. Prints the
/// median of three runs; asserts nothing about time. Run on request, in release, on a quiet host:
/// `cargo test -p luxforge-core --release --lib repair::tests::photo_sized_cost -- --ignored
/// --nocapture`.
#[test]
#[ignore = "a measurement, run on request in release"]
fn photo_sized_cost() {
    use std::time::{Duration, Instant};
    let typical = |runs: Vec<Duration>| {
        let runs = luxforge_testbase::Distribution::of(runs.iter().map(Duration::as_secs_f64));
        Duration::from_secs_f64(runs.expect("runs").p50)
    };
    for (name, stage) in [
        (
            "24 MP",
            Stage {
                width: 6000,
                height: 4000,
            },
        ),
        (
            "60 MP",
            Stage {
                width: 9504,
                height: 6336,
            },
        ),
    ] {
        let (w, h) = (f64::from(stage.width), f64::from(stage.height));
        let quarter = 0.25 * h;
        let cases = [
            ("spot r=50", vec![[0.5 * w, 0.5 * h]], 50.0, [300.0, 0.0]),
            ("spot r=200", vec![[0.5 * w, 0.5 * h]], 200.0, [700.0, 0.0]),
            (
                "spot r=H/4",
                vec![[0.3 * w, 0.5 * h]],
                quarter,
                [2.2 * quarter, 0.0],
            ),
            (
                "thin stroke across the frame r=12",
                vec![[0.02 * w, 0.05 * h], [0.98 * w, 0.95 * h]],
                12.0,
                [0.0, 60.0],
            ),
            (
                "broad stroke r=100 over 2000 px",
                vec![
                    [0.3 * w, 0.4 * h],
                    [0.3 * w + 700.0, 0.4 * h + 300.0],
                    [0.3 * w + 1400.0, 0.4 * h - 100.0],
                    [0.3 * w + 2000.0, 0.4 * h + 200.0],
                ],
                100.0,
                [0.0, 500.0],
            ),
        ];
        for (label, points, radius_px, [dx, dy]) in cases {
            for kind in [Heal, Clone] {
                let op = Op::new(kind, stage, &points, radius_px).source(stage, dx, dy);
                let compiled = op.core(stage);
                let [destination, source] = compiled.reads();
                let x0 = destination.x0.min(source.x0);
                let y0 = destination.y0.min(source.y0);
                let region = Region {
                    x0,
                    y0,
                    width: destination.x1().max(source.x1()) - x0,
                    height: destination.y1().max(source.y1()) - y0,
                };
                let mut rng = SplitMix64(7);
                let input: Vec<f32> = (0..3 * region.pixels())
                    .map(|_| 0.2 + 0.05 * rng.next_noise() as f32)
                    .collect();
                let mut runs = Vec::new();
                for _ in 0..3 {
                    let mut values = input.clone();
                    let mut frame = RepairFrame::new(stage, region, &mut values).unwrap();
                    let started = Instant::now();
                    apply(
                        &mut frame,
                        std::slice::from_ref(&compiled),
                        &Cancel::never(),
                    )
                    .unwrap();
                    runs.push(started.elapsed());
                }
                println!(
                    "{name} {kind:?} {label}: {:?} (frame {:.1} MP of declared reads)",
                    typical(runs),
                    region.pixels() as f64 / 1e6
                );
            }
            // The automatic source for the same destination: its plan, the reduction of the region
            // its survivors read, and the ranking.
            let op = Op::new(Heal, stage, &points, radius_px);
            let mut plan_runs = Vec::new();
            let mut reduce_runs = Vec::new();
            let mut search_runs = Vec::new();
            let mut window = Region::EMPTY;
            let mut r = 1;
            for _ in 0..3 {
                let started = Instant::now();
                let plan = search::plan(&op.points, op.radius, stage);
                plan_runs.push(started.elapsed());
                window = plan.region;
                r = plan.r;
                let mut rng = SplitMix64(9);
                let mut values: Vec<f32> = (0..3 * window.pixels())
                    .map(|_| 0.2 + 0.05 * rng.next_noise() as f32)
                    .collect();
                let started = Instant::now();
                let reduced = if window.is_empty() {
                    search::Reduced::empty(r)
                } else {
                    let frame = RepairFrame::new(stage, window, &mut values).unwrap();
                    search::reduce(&frame, window, r, &Cancel::never()).unwrap()
                };
                reduce_runs.push(started.elapsed());
                let started = Instant::now();
                let _ = search::rank(&plan, &reduced);
                search_runs.push(started.elapsed());
            }
            println!("{name} search {label}: plan {:?}", typical(plan_runs));
            println!(
                "{name} search {label}: reduce {:?} over {:.1} MP at r={r}, rank {:?}",
                typical(reduce_runs),
                window.pixels() as f64 / 1e6,
                typical(search_runs)
            );
        }
    }
}

// ---- The GPU description ------------------------------------------------------------------------

/// A group's description holds together: every pass reads at most four planes and never its own
/// output, each atlas tile's reverse entry is the tile its operation's map places there, each
/// level's planes hold its tiles, a Clone-only group runs the gather and the composite alone, and a
/// Heal's passes are the schedule's: gather, coarse and start passes, three V(2, 2) cycles and the
/// composite.
#[test]
fn the_gpu_description_holds_together() {
    use super::gpu::{self, ATLAS_TILES_ACROSS, OP_WORDS};
    let s = Stage {
        width: 640,
        height: 480,
    };
    let spot = |kind, x, y, r| Op::new(kind, s, &[[x, y]], r).source(s, 0.0, 120.0);
    let groups = [
        vec![spot(Clone, 100.0, 100.0, 20.0)],
        vec![spot(Heal, 300.0, 200.0, 24.0)],
        vec![
            spot(Heal, 100.0, 100.0, 9.0),
            spot(Clone, 500.0, 100.0, 30.0),
            Op::new(Heal, s, &[[50.0, 300.0], [600.0, 340.0]], 6.0).source(s, 0.0, -90.0),
        ],
    ];
    for (n, group) in groups.iter().enumerate() {
        let ops: Vec<RepairOp> = group.iter().map(|op| op.core(s)).collect();
        let d = gpu::describe(&ops, s, None);
        for pass in &d.passes {
            assert!(pass.inputs.len() <= 4 && !pass.inputs.contains(&pass.output));
            assert!(pass.output < d.planes.len());
            assert!(pass.words + 3 <= d.words.len());
        }
        let levels = d.tiles.len();
        // The reverse tables against the maps.
        let records = d.block[1] as usize;
        for k in 0..levels {
            let reverse = d.block[10 + k] as usize;
            let (width, _) = d.planes[d.passes[0].output];
            assert_eq!(width, ATLAS_TILES_ACROSS * 8);
            let capacity =
                (d.tiles[k].max(1).div_ceil(ATLAS_TILES_ACROSS) * ATLAS_TILES_ACROSS) as usize;
            let mut placed = 0;
            for tile in 0..capacity {
                let packed = d.block[reverse + tile];
                if packed == u32::MAX {
                    continue;
                }
                placed += 1;
                let (op, gy, gx) = (packed >> 24, (packed >> 12) & 0xfff, packed & 0xfff);
                let record = records + op as usize * OP_WORDS;
                let grid = record + 11 + 4 * k;
                let map = d.block[grid + 3] as usize;
                let across = d.block[grid + 2];
                assert_eq!(d.block[map + (gy * across + gx) as usize], tile as u32);
            }
            assert_eq!(placed, d.tiles[k], "group {n} level {k}");
        }
        let heals = group.iter().any(|op| op.kind == Heal);
        if !heals {
            let kernels: Vec<&str> = d.passes.iter().map(|p| p.kernel).collect();
            assert_eq!(kernels, ["lf_repair_gather", "lf_repair_composite"]);
            continue;
        }
        // Per V-cycle: at each solved level but the lowest, 4 + 1 + 1 + 4 passes; at the lowest
        // 4 + 4.
        let solved = levels.saturating_sub(1).max(1);
        let cycle = 10 * (solved - 1) + 8;
        assert_eq!(
            d.passes.len(),
            1 + 2 * (levels - 1) + 3 * cycle + 1,
            "group {n}: {levels} levels"
        );
    }
}

/// The program's transfer constants are the core's `f32` encode's, and its decode's threshold and
/// exponent the core's.
#[test]
fn the_program_restates_the_cores_transfer_function() {
    use crate::render::gpu::testing::wgsl_constant;
    let program = &super::REPAIR_PROGRAM;
    for (name, value) in crate::colour::srgb::ENCODE_F32 {
        assert_eq!(
            wgsl_constant(program, &format!("lf_repair_{name}")),
            [value],
            "{name}"
        );
    }
    assert_eq!(
        wgsl_constant(program, "lf_repair_decode_end"),
        [0.040_45_f32]
    );
    assert_eq!(
        wgsl_constant(program, "lf_repair_decode_exponent"),
        [2.4_f32]
    );
    // The decode the program spells is the core's, at the core's precision.
    for code in 0..=255u8 {
        let encoded = f32::from(code) / 255.0;
        let core = crate::colour::srgb::decode_f32(encoded);
        let spelled = if encoded <= 0.040_45 {
            encoded / wgsl_constant(program, "lf_repair_slope")[0]
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        };
        assert_eq!(core.to_bits(), spelled.to_bits(), "{code}");
    }
}
