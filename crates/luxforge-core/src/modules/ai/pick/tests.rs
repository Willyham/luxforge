//! The prototype's recorded decisions (`probes/object-removal/src/stroke.rs`), ported: each test
//! is a rule of the picker, on synthetic logits whose answer the rule fixes.
use super::*;
use crate::modules::ai::segment::Logits;

const IMAGE: (u32, u32) = (512, 384);

fn logits(inside: impl Fn(f32, f32) -> bool) -> Logits {
    let cpp = 64.0 / 512.0;
    let mut data = Vec::with_capacity(64 * 64);
    for v in 0..64 {
        for u in 0..64 {
            let (x, y) = ((u as f32 + 0.5) / cpp, (v as f32 + 0.5) / cpp);
            data.push(if y >= IMAGE.1 as f32 {
                -20.0
            } else if inside(x, y) {
                8.0
            } else {
                -8.0
            });
        }
    }
    Logits {
        width: 64,
        height: 64,
        data,
        source: IMAGE,
        cells_per_px: (cpp, cpp),
    }
}

fn disc(cx: f32, cy: f32, r: f32) -> impl Fn(f32, f32) -> bool {
    move |x, y| (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> impl Fn(f32, f32) -> bool {
    move |x, y| x >= x0 && x < x1 && y >= y0 && y < y1
}

fn candidate(inside: impl Fn(f32, f32) -> bool, predicted_iou: f32) -> Candidate {
    Candidate {
        logits: logits(inside),
        predicted_iou,
    }
}

fn line(a: (f32, f32), b: (f32, f32), radius: f32) -> Stroke {
    Stroke {
        points: vec![Point::new(a.0, a.1), Point::new(b.0, b.1)],
        radius,
    }
}

#[test]
fn stroke_prompts_are_the_padded_box_then_farthest_points() {
    let stroke = line((100.0, 50.0), (200.0, 50.0), 10.0);
    let p = prompts(&stroke, IMAGE, &Params::default());
    assert_eq!(p.len(), 5);
    assert_eq!(
        p[0].bbox,
        Some((Point::new(90.0, 40.0), Point::new(210.0, 60.0)))
    );
    assert!(p[0].positive.is_empty() && p[0].negative.is_empty());
    // Seeded at the middle, then both ends (the earlier first), then the first quarter.
    let xs: Vec<f32> = p[1..].iter().map(|q| q.positive[0].x).collect();
    assert_eq!(xs, vec![150.0, 100.0, 200.0, 125.0]);
    assert!(
        p[1..]
            .iter()
            .all(|q| q.positive.len() == 1 && q.positive[0].y == 50.0 && q.bbox.is_none())
    );
}

#[test]
fn stroke_prompts_clamp_to_the_image_and_handle_a_single_point() {
    let stroke = Stroke {
        points: vec![Point::new(5.0, 380.0)],
        radius: 20.0,
    };
    let p = prompts(&stroke, IMAGE, &Params::default());
    assert_eq!(p.len(), 2, "one point prompt for a single-point stroke");
    assert_eq!(
        p[0].bbox,
        Some((Point::new(0.0, 360.0), Point::new(25.0, 384.0)))
    );
    assert_eq!(p[1].positive, vec![Point::new(5.0, 380.0)]);
    let none = Stroke {
        points: Vec::new(),
        radius: 5.0,
    };
    assert!(prompts(&none, IMAGE, &Params::default()).is_empty());
}

#[test]
fn stroke_prompts_follow_a_bent_polyline() {
    // An L: the farthest points reach both ends and the corner region.
    let stroke = Stroke {
        points: vec![
            Point::new(100.0, 100.0),
            Point::new(300.0, 100.0),
            Point::new(300.0, 300.0),
        ],
        radius: 4.0,
    };
    let p = prompts(&stroke, IMAGE, &Params::default());
    let pts: Vec<Point> = p[1..].iter().map(|q| q.positive[0]).collect();
    assert!(pts.contains(&Point::new(100.0, 100.0)));
    assert!(pts.contains(&Point::new(300.0, 300.0)));
    for q in &pts {
        let on_stroke = (q.y == 100.0 && (100.0..=300.0).contains(&q.x))
            || (q.x == 300.0 && (100.0..=300.0).contains(&q.y));
        assert!(on_stroke, "{q:?} is on the polyline");
    }
}

#[test]
fn stroke_stability_is_the_iou_of_the_offset_thresholds() {
    let mut l = logits(rect(0.0, 0.0, 256.0, 384.0));
    assert_eq!(stability(&l, 1.0), 1.0);
    // Soften half the positive cells to +0.5: they pass −1 but not +1.
    let (vw, vh) = valid_cells(&l);
    assert_eq!((vw, vh), (64, 48));
    let mut softened = 0;
    for v in 0..vh {
        for u in 0..16 {
            l.data[v * 64 + u] = 0.5;
            softened += 1;
        }
    }
    let positive = 32 * 48;
    let expected = (positive - softened) as f32 / positive as f32;
    assert!((stability(&l, 1.0) - expected).abs() < 1e-6);
    // Padding rows do not count, whatever they hold.
    for x in &mut l.data[48 * 64..] {
        *x = 0.0;
    }
    assert!((stability(&l, 1.0) - expected).abs() < 1e-6);
    assert_eq!(stability(&logits(|_, _| false), 1.0), 0.0);
}

#[test]
fn stroke_over_one_object_removes_it() {
    let stroke = line((170.0, 200.0), (230.0, 200.0), 10.0);
    let object = disc(200.0, 200.0, 60.0);
    let results = vec![
        vec![candidate(&object, 0.95)],
        vec![candidate(&object, 0.93)],
        vec![candidate(disc(200.0, 200.0, 30.0), 0.9)],
    ];
    let r = decide(&stroke, &results, &Params::default());
    let Decision::One(i) = r.decision else {
        panic!("expected one object, got {:?}", r.decision);
    };
    let s = &r.scored[i];
    assert_eq!(
        (s.prompt, s.candidate),
        (0, 0),
        "the larger object, then the higher IoU"
    );
    assert_eq!(s.stroke_coverage, 1.0);
    let area = std::f32::consts::PI * 60.0 * 60.0;
    assert!(
        (s.area as f32 - area).abs() / area < 0.06,
        "area {}",
        s.area
    );
    let stroke_area = 60.0 * 20.0 + std::f32::consts::PI * 100.0;
    assert!((s.object_coverage - stroke_area / area).abs() < 0.03);
    assert!(s.stability > 0.99);
}

#[test]
fn stroke_over_two_disjoint_objects_offers_both() {
    let stroke = line((100.0, 200.0), (400.0, 200.0), 8.0);
    let left = disc(120.0, 200.0, 50.0);
    let right = disc(360.0, 200.0, 70.0);
    let background = |x: f32, y: f32| !left(x, y) && !right(x, y);
    let results = vec![
        vec![candidate(background, 0.8)],
        vec![candidate(&left, 0.94)],
        vec![candidate(&right, 0.92)],
        vec![candidate(disc(360.0, 200.0, 30.0), 0.97)],
    ];
    let r = decide(&stroke, &results, &Params::default());
    // The background is under the stroke but the stroke covers little of it.
    assert_eq!(r.scored.len(), 3, "both objects and the right one's part");
    let Decision::Several(ref picks) = r.decision else {
        panic!("expected several, got {:?}", r.decision);
    };
    let prompts: Vec<usize> = picks.iter().map(|&i| r.scored[i].prompt).collect();
    assert_eq!(
        prompts,
        vec![2, 1],
        "listed by stroke coverage, without the part"
    );
    assert!(r.scored[picks[0]].stroke_coverage > r.scored[picks[1]].stroke_coverage);
    assert!(r.scored[picks[0]].stroke_coverage < 0.7);
}

#[test]
fn stroke_over_a_part_removes_its_whole() {
    let whole = rect(150.0, 110.0, 300.0, 290.0);
    let part = disc(200.0, 180.0, 45.0);
    let results = vec![
        vec![candidate(&whole, 0.9)],
        vec![candidate(&part, 0.96), candidate(&whole, 0.85)],
    ];
    let params = Params::default();
    let decide_one = |stroke: &Stroke| {
        let r = decide(stroke, &results, &params);
        assert_eq!(
            r.scored.len(),
            2,
            "the part and the whole survive: {:?}",
            r.scored
        );
        match r.decision {
            Decision::One(i) => (r.scored[i].clone(), r.scored),
            d => panic!("expected one, got {d:?}"),
        }
    };

    // Mostly in the part and partly beyond it: both explain the stroke, and the whole wins.
    let crossing = line((175.0, 180.0), (250.0, 180.0), 12.0);
    let (pick, scored) = decide_one(&crossing);
    assert!(
        scored.iter().all(|s| s.stroke_coverage >= 0.7),
        "{scored:?}"
    );
    assert!(pick.area > 20_000.0, "the whole wins, area {}", pick.area);

    // Crossing well beyond the part: only the whole explains it.
    let long = line((175.0, 180.0), (280.0, 270.0), 12.0);
    let (pick, _) = decide_one(&long);
    assert!(pick.area > 20_000.0);

    // Entirely within the part: both explain it fully, and the part still yields to its whole
    // although its object coverage is better.
    let inside = line((180.0, 180.0), (220.0, 180.0), 14.0);
    let (pick, scored) = decide_one(&inside);
    assert!(
        scored.iter().all(|s| s.stroke_coverage == 1.0),
        "{scored:?}"
    );
    assert!(scored[1].object_coverage > scored[0].object_coverage);
    assert!(pick.area > 20_000.0, "the whole wins, area {}", pick.area);

    // A whole the stroke barely touches, such as a table under a cup, is no target.
    let table = rect(20.0, 20.0, 500.0, 380.0);
    let r = decide(
        &inside,
        &[vec![candidate(&part, 0.96), candidate(&table, 0.9)]],
        &params,
    );
    assert_eq!(r.scored.len(), 1);
    assert!(r.scored[0].area < 10_000.0);
    assert_eq!(r.decision, Decision::One(0));
}

#[test]
fn stroke_keeps_a_whole_scored_below_its_part() {
    // SAM 2.1's three masks for points on a boy's waist: his belt, his clothes at 0.80, and the
    // whole boy at 0.61, below `min_predicted_iou`.
    let belt = rect(180.0, 190.0, 270.0, 215.0);
    let clothes = rect(170.0, 150.0, 280.0, 250.0);
    let boy = rect(150.0, 110.0, 300.0, 290.0);
    let stroke = line((190.0, 200.0), (260.0, 200.0), 14.0);
    let results = |boy_iou: f32| {
        vec![
            vec![
                candidate(&belt, 0.72),
                candidate(&clothes, 0.8),
                candidate(&boy, boy_iou),
            ],
            vec![candidate(&clothes, 0.82), candidate(&boy, boy_iou)],
        ]
    };
    let params = Params::default();
    let one = |r: &Resolution| match r.decision {
        Decision::One(i) => r.scored[i].area,
        ref d => panic!("expected one, got {d:?}"),
    };
    let r = decide(&stroke, &results(0.61), &params);
    assert!(one(&r) > 25_000.0, "the whole boy: {:?}", r.scored);
    // Below the whole's floor the boy is dropped and his clothes are the target.
    let r = decide(&stroke, &results(0.45), &params);
    let area = one(&r);
    assert!((10_000.0..12_000.0).contains(&area), "the clothes: {area}");
    // A low-IoU candidate that holds no credible part is not rescued.
    let r = decide(&stroke, &[vec![candidate(&boy, 0.61)]], &params);
    assert_eq!(r.decision, Decision::Nothing);
}

#[test]
fn stroke_over_nothing_decides_nothing() {
    let stroke = line((20.0, 20.0), (80.0, 40.0), 6.0);
    let results = vec![
        vec![candidate(disc(300.0, 250.0, 60.0), 0.95)],
        vec![candidate(disc(300.0, 250.0, 60.0), 0.95)],
    ];
    let r = decide(&stroke, &results, &Params::default());
    assert_eq!(r.decision, Decision::Nothing);
    assert!(r.scored.is_empty());
    assert_eq!(
        decide(&stroke, &[], &Params::default()).decision,
        Decision::Nothing
    );
    let outside = line((600.0, 20.0), (700.0, 40.0), 6.0);
    let r = decide(
        &outside,
        &[vec![candidate(|_, _| true, 0.99)]],
        &Params::default(),
    );
    assert_eq!(r.decision, Decision::Nothing);
}

#[test]
fn stroke_drops_unstable_and_low_iou_candidates() {
    let stroke = line((170.0, 200.0), (230.0, 200.0), 10.0);
    let object = disc(200.0, 200.0, 60.0);
    let low_iou = candidate(&object, 0.4);
    let mut unstable = candidate(&object, 0.97);
    for x in &mut unstable.logits.data {
        if *x > 0.0 {
            *x = 0.6;
        }
    }
    let r = decide(
        &stroke,
        &[vec![low_iou.clone()], vec![unstable.clone()]],
        &Params::default(),
    );
    assert_eq!(r.decision, Decision::Nothing);
    assert!(r.scored.is_empty());

    // With a good candidate beside them, only it survives.
    let good = candidate(disc(200.0, 200.0, 40.0), 0.9);
    let r = decide(
        &stroke,
        &[vec![low_iou.clone()], vec![unstable], vec![good.clone()]],
        &Params::default(),
    );
    assert_eq!(r.scored.len(), 1);
    assert_eq!(r.decision, Decision::One(0));
    assert_eq!(r.scored[0].prompt, 2);

    // At the whole's floor, the low-IoU disc holds the good one and is kept as its whole.
    let whole = Candidate {
        predicted_iou: 0.55,
        ..low_iou
    };
    let r = decide(&stroke, &[vec![whole], vec![good]], &Params::default());
    assert_eq!(r.scored.len(), 2);
    assert_eq!(r.decision, Decision::One(0));
    assert_eq!(r.scored[0].prompt, 0);
}

#[test]
fn stroke_near_duplicates_keep_the_larger() {
    let stroke = line((170.0, 200.0), (230.0, 200.0), 10.0);
    let results = vec![
        vec![candidate(disc(200.0, 200.0, 56.0), 0.97)],
        vec![candidate(disc(200.0, 200.0, 62.0), 0.9)],
        vec![candidate(disc(200.0, 200.0, 62.0), 0.9)],
    ];
    let r = decide(&stroke, &results, &Params::default());
    assert_eq!(r.scored.len(), 1, "{:?}", r.scored);
    assert_eq!((r.scored[0].prompt, r.scored[0].candidate), (1, 0));
    assert_eq!(r.decision, Decision::One(0));

    // Well apart in size, they are a part and its whole, not duplicates.
    let results = vec![
        vec![candidate(disc(200.0, 200.0, 30.0), 0.97)],
        vec![candidate(disc(200.0, 200.0, 62.0), 0.9)],
    ];
    let r = decide(&stroke, &results, &Params::default());
    assert_eq!(r.scored.len(), 2);
    assert!(
        r.scored[0].area > r.scored[1].area,
        "survivors run largest first"
    );
}

#[test]
fn stroke_scores_candidates_on_other_grids() {
    // The same disc on SAM 2.1's squashed 256² frame scores like the padded frame's.
    let stroke = line((170.0, 200.0), (230.0, 200.0), 10.0);
    let padded = candidate(disc(200.0, 200.0, 60.0), 0.9);
    let (cx, cy) = (256.0 / 512.0, 256.0 / 384.0);
    let mut data = Vec::new();
    for v in 0..256 {
        for u in 0..256 {
            let (x, y) = ((u as f32 + 0.5) / cx, (v as f32 + 0.5) / cy);
            data.push(if disc(200.0, 200.0, 60.0)(x, y) {
                8.0
            } else {
                -8.0
            });
        }
    }
    let squashed = Candidate {
        logits: Logits {
            width: 256,
            height: 256,
            data,
            source: IMAGE,
            cells_per_px: (cx, cy),
        },
        predicted_iou: 0.95,
    };
    let alone = decide(&stroke, &[vec![squashed.clone()]], &Params::default());
    let mixed = decide(&stroke, &[vec![padded], vec![squashed]], &Params::default());
    assert_eq!(alone.decision, Decision::One(0));
    assert_eq!(mixed.scored.len(), 1, "duplicates across grids");
    let (a, m) = (alone.scored[0].area, mixed.scored[0].area);
    assert!((a - m).abs() / a < 0.05, "{a} vs {m}");
}

#[test]
fn stroke_hover_prefers_the_whole_object() {
    let params = Params::default();
    // SAM 2.1 on a click at a boy's waist: his clothes score 0.80 and the whole boy 0.61.
    let belt = candidate(rect(180.0, 190.0, 270.0, 215.0), 0.7);
    let clothes = candidate(rect(170.0, 150.0, 280.0, 250.0), 0.8);
    let boy = candidate(rect(150.0, 110.0, 300.0, 290.0), 0.61);
    let three = [belt.clone(), clothes.clone(), boy.clone()];
    assert_eq!(pick_hover(&three, &params), Some(2));

    // A doubtful or unstable whole does not count, and the best IoU stands.
    let doubtful = Candidate {
        predicted_iou: 0.45,
        ..boy.clone()
    };
    assert_eq!(
        pick_hover(&[belt.clone(), clothes.clone(), doubtful], &params),
        Some(1)
    );
    let mut unstable = boy.clone();
    for x in &mut unstable.logits.data {
        if *x > 0.0 {
            *x = 0.6;
        }
    }
    assert_eq!(pick_hover(&[clothes.clone(), unstable], &params), Some(0));

    // A larger candidate that does not hold the best one is not its whole.
    let around = candidate(|x, y| !rect(170.0, 150.0, 280.0, 250.0)(x, y), 0.79);
    assert_eq!(pick_hover(&[clothes.clone(), around], &params), Some(0));

    // Equal wholes: the higher predicted IoU, then the earlier.
    let surer = Candidate {
        predicted_iou: 0.65,
        ..boy.clone()
    };
    assert_eq!(
        pick_hover(&[clothes.clone(), boy.clone(), surer], &params),
        Some(2)
    );
    assert_eq!(
        pick_hover(&[clothes.clone(), boy.clone(), boy.clone()], &params),
        Some(1)
    );
    // Equal best IoUs: the earlier.
    assert_eq!(pick_hover(&[boy.clone(), boy.clone()], &params), Some(0));
    assert_eq!(pick_hover(&[], &params), None);
    assert_eq!(pick_hover(&[candidate(|_, _| false, 0.9)], &params), None);
}

#[test]
fn stroke_decisions_are_deterministic() {
    let stroke = Stroke {
        points: vec![
            Point::new(90.0, 190.0),
            Point::new(250.0, 215.0),
            Point::new(410.0, 205.0),
        ],
        radius: 9.0,
    };
    let results = vec![
        vec![candidate(rect(60.0, 150.0, 460.0, 260.0), 0.8)],
        vec![
            candidate(disc(120.0, 200.0, 50.0), 0.94),
            candidate(disc(120.0, 200.0, 52.0), 0.94),
        ],
        vec![candidate(disc(360.0, 200.0, 70.0), 0.92)],
        vec![candidate(disc(360.0, 200.0, 69.0), 0.92)],
    ];
    let params = Params::default();
    let first = decide(&stroke, &results, &params);
    for _ in 0..3 {
        assert_eq!(decide(&stroke, &results, &params), first);
        assert_eq!(
            prompts(&stroke, IMAGE, &params),
            prompts(&stroke, IMAGE, &params)
        );
    }
}

#[test]
fn stroke_raster_is_exact_against_brute_force() {
    // The patch shortcut must count exactly what per-sub-cell evaluation counts.
    let l = logits(disc(211.0, 147.0, 77.0));
    let grid = Grid::new(&l);
    let shape = Shape::new(&l, &l, &grid).expect("non-empty");
    let mut brute = 0u64;
    for b in 0..grid.y.len() {
        for a in 0..grid.x.len() {
            brute += (grid.value(&l.data, a, b) > 0.0) as u64;
        }
    }
    assert_eq!(shape.count, brute);
    // And the grid's sub-cell centres interpolate as Logits::sample does.
    for (a, b) in [(0, 0), (5, 7), (63, 47), (40, 12)] {
        let p = Point::new(
            ((a as f64 + 0.5) * grid.x.px) as f32,
            ((b as f64 + 0.5) * grid.y.px) as f32,
        );
        assert!((grid.value(&l.data, a, b) - l.sample(p)).abs() < 1e-4);
    }
}
