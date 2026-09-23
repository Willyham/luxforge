//! TASK-018: independent proofs for the frozen brush coverage mathematics — the
//! capsule profile, the per-stroke maximum, the screen union across add strokes
//! and the multiply-complement for erase strokes.
//!
//! This binary shares no code with `lightwell-core`'s production sources. The
//! frozen equations live in `tests/reference/mask.rs` beside the gradients'; the
//! mathematics, the rejected spellings and every measured figure quoted below
//! are written out in full in `docs/design/mask-study.md#the-brush`, which this
//! file's test names track.
//!
//! The study's dense figures are printed by the one ignored test at the end:
//!
//! ```sh
//! cargo test --release --locked --package lightwell-core --test mask_brush_reference \
//!     -- --ignored --nocapture brush_study_figures
//! ```

mod reference;

use reference::mask::{
    Brush, BrushStroke, DISTANCE_MAX, DISTANCE_MIN, Stage, accumulate, brush_bounds,
    brush_coverage, brush_segments, brush_size_is_legal, capsule_profile, segment_distance2,
    smooth, stroke_coverage, stroke_coverage_max_form,
};

// ---------------------------------------------------------------------------
// Test-only helpers, kept local to this file per the reference tree's
// convention: a parallel study's own copy merges cleanly with no shared state.
// ---------------------------------------------------------------------------

/// SplitMix64: dependency-free and fully reproducible, so every fixed-seed
/// figure below is the same on any machine and any Rust version.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_range(&mut self, lo: f64, hi: f64) -> f64 {
        let u = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + u * (hi - lo)
    }

    fn next_usize(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// The two stages every geometric claim is checked on: a 3:2 landscape frame and
/// its portrait transpose, both at 24 MP, exactly as the gradients' proofs use.
const LANDSCAPE: Stage = Stage::new(6000, 4000);
const PORTRAIT: Stage = Stage::new(4000, 6000);

/// A random stroke with a usable radius: `[0.01, 0.2]` mask-space units is the
/// range a brush is actually drawn at, well inside the legal `[1e-4, 64]`.
fn random_stroke(rng: &mut SplitMix64, erase: bool) -> BrushStroke {
    let count = 1 + rng.next_usize(6);
    let mut points = Vec::with_capacity(count);
    let mut x = rng.next_range(0.05, 0.95);
    let mut y = rng.next_range(0.05, 0.95);
    for _ in 0..count {
        points.push([x, y]);
        x = (x + rng.next_range(-0.2, 0.2)).clamp(-1.0, 2.0);
        y = (y + rng.next_range(-0.2, 0.2)).clamp(-1.0, 2.0);
    }
    BrushStroke {
        points,
        size: rng.next_range(0.01, 0.2),
        feather: rng.next_range(0.0, 100.0),
        flow: rng.next_range(1.0, 100.0),
        erase,
    }
}

/// A grid of sample points over mask space, in the pixel-centre spelling, which
/// is the one a rasterizing pass uses.
fn sample_points(stage: &Stage, step: u32) -> Vec<(f64, f64)> {
    let mut points = Vec::new();
    let mut y = 0;
    while y < stage.height {
        let mut x = 0;
        while x < stage.width {
            points.push(stage.pixel_uv(x, y));
            x += step;
        }
        y += step;
    }
    points
}

// ---------------------------------------------------------------------------
// The capsule profile
// ---------------------------------------------------------------------------

/// The two spellings of "one stroke's coverage" agree **bit for bit**: the
/// profile of the smallest distance is the largest profile over the segments,
/// because the profile is nonincreasing in `d` and `max` returns one of its
/// operands exactly. The frozen spelling is the first, which evaluates one
/// `sqrt` and one `smooth` per stroke rather than one per segment.
#[test]
fn the_minimum_distance_form_equals_the_maximum_profile_form_bit_for_bit() {
    let mut rng = SplitMix64(0x0018_B005);
    let mut compared = 0usize;
    for stage in [&LANDSCAPE, &PORTRAIT] {
        for _ in 0..400 {
            let stroke = random_stroke(&mut rng, false);
            let segments = brush_segments(&stroke, stage);
            for _ in 0..60 {
                let u =
                    rng.next_range(-0.2, f64::from(stage.width) / f64::from(stage.height) + 0.2);
                let v = rng.next_range(-0.2, 1.2);
                let frozen = stroke_coverage(&stroke, &segments, u, v);
                let design = stroke_coverage_max_form(&stroke, &segments, u, v);
                assert_eq!(
                    frozen.to_bits(),
                    design.to_bits(),
                    "the two spellings differed at ({u}, {v}) on {stroke:?}"
                );
                compared += 1;
            }
        }
    }
    assert!(compared >= 48_000, "compared only {compared} points");
}

/// The profile is exactly `1.0` on the core and exactly `0.0` at and beyond the
/// radius on the feathered branch, for every feather, so the support rectangle
/// and the grid index are exactly right rather than nearly right.
#[test]
fn the_profile_is_exactly_one_on_the_core_and_exactly_zero_past_the_radius() {
    let mut rng = SplitMix64(0x0018_C0DE);
    for _ in 0..2000 {
        let mut stroke = random_stroke(&mut rng, false);
        stroke.flow = 100.0;
        let r = stroke.size;
        let band = r * (stroke.feather / 100.0);
        // Exactly one at the centre of the capsule, whatever the feather.
        assert_eq!(capsule_profile(&stroke, 0.0), 1.0);
        // Exactly zero past the radius on either branch.
        assert_eq!(capsule_profile(&stroke, r * 1.000_001), 0.0);
        assert_eq!(capsule_profile(&stroke, r + 1.0), 0.0);
        if band == 0.0 {
            // The hard edge is closed at `d = R`: this is the one branch on
            // which the radius itself is covered.
            assert_eq!(capsule_profile(&stroke, r), 1.0);
        } else {
            // The feathered branch reaches exactly zero at the radius, which is
            // what makes a segment box grown by `R` conservative.
            assert_eq!(capsule_profile(&stroke, r), 0.0);
            // And exactly one at the inner edge of the ramp.
            assert_eq!(capsule_profile(&stroke, (r - band).max(0.0)), 1.0);
        }
    }
}

/// `feather = 0` is an explicit hard edge and not a limit, and a feather so
/// small that `R · f/100` underflows to exactly zero is the same edge reached by
/// rounding rather than by an equality against zero. A representable small
/// feather is the smooth branch, checked here so the boundary is not asserted
/// from one side only.
#[test]
fn feather_zero_is_a_hard_edge_and_no_vanishing_band_is_divided_by() {
    let base = BrushStroke {
        points: vec![[0.4, 0.5], [0.6, 0.5]],
        size: 0.1,
        feather: 0.0,
        flow: 100.0,
        erase: false,
    };
    // `0.0` is the ordinary route; `1e-323` is the other one — a feather whose
    // band `R · f/100` underflows to exactly zero, so the branch is taken by the
    // computed band rather than by an equality against the stored feather.
    for feather in [0.0, 1e-323] {
        let stroke = BrushStroke {
            feather,
            ..base.clone()
        };
        assert_eq!(stroke.size * (feather / 100.0), 0.0, "feather {feather}");
        for d in [0.0, 0.05, 0.099_999, 0.1] {
            assert_eq!(capsule_profile(&stroke, d), 1.0, "feather {feather}, d {d}");
        }
        assert_eq!(capsule_profile(&stroke, 0.100_001), 0.0);
    }
    // A representable small feather is the smooth branch and is not the edge.
    let soft = BrushStroke {
        feather: 0.01,
        ..base
    };
    let mid = capsule_profile(&soft, 0.099_997);
    assert!(mid > 0.0 && mid < 1.0, "{mid}");
}

/// A one-point stroke is the distance to the point, and it is reached by the
/// same expression a real segment is: the degenerate normalization sets
/// `ex = ey = 0` and `len2 = 1`, so `t` is exactly `0`, `q` is exactly `A`, and
/// there is no branch on the per-pixel path.
#[test]
fn a_one_point_stroke_is_the_distance_to_the_point_with_no_branch() {
    let stage = LANDSCAPE;
    let stroke = BrushStroke {
        points: vec![[0.5, 0.5]],
        size: 0.1,
        feather: 50.0,
        flow: 100.0,
        erase: false,
    };
    let segments = brush_segments(&stroke, &stage);
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].ex, 0.0);
    assert_eq!(segments[0].ey, 0.0);
    assert_eq!(segments[0].len2, 1.0);
    let (cu, cv) = stage.position_uv(0.5, 0.5);
    let mut rng = SplitMix64(0x0018_1007);
    for _ in 0..2000 {
        let u = rng.next_range(cu - 0.3, cu + 0.3);
        let v = rng.next_range(cv - 0.3, cv + 0.3);
        let du = u - cu;
        let dv = v - cv;
        assert_eq!(
            segment_distance2(&segments[0], u, v).to_bits(),
            (du * du + dv * dv).to_bits()
        );
    }
    // A capsule around a single point is a disc: the same coverage in every
    // direction at the same distance.
    let north = stroke_coverage(&stroke, &segments, cu, cv - 0.05);
    let east = stroke_coverage(&stroke, &segments, cu + 0.05, cv);
    assert!((north - east).abs() < 1e-15, "{north} against {east}");
}

/// A doubled-back path — out and back along the same line — covers exactly what
/// the single pass covers, because the maximum along a stroke takes the nearest
/// segment and the return segment is never nearer than the outward one at the
/// same distance. It is one stroke and therefore one density, not two.
#[test]
fn a_doubled_back_path_covers_exactly_what_the_single_pass_covers() {
    let stage = LANDSCAPE;
    let out = BrushStroke {
        points: vec![[0.3, 0.5], [0.7, 0.5]],
        size: 0.08,
        feather: 60.0,
        flow: 70.0,
        erase: false,
    };
    let back = BrushStroke {
        points: vec![[0.3, 0.5], [0.7, 0.5], [0.3, 0.5]],
        ..out.clone()
    };
    let out_segments = brush_segments(&out, &stage);
    let back_segments = brush_segments(&back, &stage);
    assert_eq!(back_segments.len(), 2);
    let mut rng = SplitMix64(0x0001_8DB1);
    for _ in 0..20_000 {
        let u = rng.next_range(0.0, 1.5);
        let v = rng.next_range(0.0, 1.0);
        assert_eq!(
            stroke_coverage(&out, &out_segments, u, v).to_bits(),
            stroke_coverage(&back, &back_segments, u, v).to_bits(),
            "doubling back changed coverage at ({u}, {v})"
        );
    }
}

/// One pass of the brush has one density whatever the pointer's sampling rate.
/// Resampling a stroke's own polyline at more positions is the same geometric
/// path, so it is the same coverage; the residual is the two expressions'
/// rounding and nothing else, and it is recorded rather than asserted to be
/// zero.
#[test]
fn a_strokes_density_does_not_depend_on_its_point_sampling() {
    let stage = LANDSCAPE;
    let mut rng = SplitMix64(0x0018_5A11);
    let mut worst = 0.0f64;
    for _ in 0..200 {
        let coarse = random_stroke(&mut rng, false);
        // The same polyline, with every segment split into four collinear
        // pieces: the path a slower hand or a finer pointer would have posted.
        let mut fine_points = Vec::new();
        for pair in coarse.points.windows(2) {
            let [ax, ay] = pair[0];
            let [bx, by] = pair[1];
            for step in 0..4 {
                let t = f64::from(step) / 4.0;
                fine_points.push([ax + t * (bx - ax), ay + t * (by - ay)]);
            }
        }
        fine_points.push(*coarse.points.last().unwrap());
        let fine = BrushStroke {
            points: fine_points,
            ..coarse.clone()
        };
        let coarse_segments = brush_segments(&coarse, &stage);
        let fine_segments = brush_segments(&fine, &stage);
        for _ in 0..200 {
            let u = rng.next_range(-0.1, 1.6);
            let v = rng.next_range(-0.1, 1.1);
            let a = stroke_coverage(&coarse, &coarse_segments, u, v);
            let b = stroke_coverage(&fine, &fine_segments, u, v);
            worst = worst.max((a - b).abs());
        }
    }
    assert!(
        worst <= 1e-12,
        "resampling a stroke moved its coverage by {worst:e}"
    );
    println!("resampling a stroke's own path moves coverage by at most {worst:e}");
}

// ---------------------------------------------------------------------------
// Accumulation
// ---------------------------------------------------------------------------

/// The two frozen folds are exact identities at both ends of `s`, which is the
/// property the whole grid index rests on: a stroke the pixel is nowhere near
/// contributes exactly nothing, so skipping it is bit-identical to folding it.
#[test]
fn the_accumulation_is_an_exact_identity_at_zero_and_saturates_exactly_at_one() {
    let mut rng = SplitMix64(0x0018_ACC0);
    for _ in 0..200_000 {
        let c = rng.next_range(0.0, 1.0);
        assert_eq!(accumulate(c, 0.0, false).to_bits(), c.to_bits());
        assert_eq!(accumulate(c, 0.0, true).to_bits(), c.to_bits());
        assert_eq!(accumulate(c, 1.0, false), 1.0);
        assert_eq!(accumulate(c, 1.0, true), 0.0);
    }
    // The ends of the range themselves, which the sampler never hits exactly.
    for c in [0.0, 1.0, f64::MIN_POSITIVE, 1.0 - f64::EPSILON] {
        assert_eq!(accumulate(c, 0.0, false).to_bits(), c.to_bits());
        assert_eq!(accumulate(c, 0.0, true).to_bits(), c.to_bits());
        assert_eq!(accumulate(c, 1.0, false), 1.0);
        assert_eq!(accumulate(c, 1.0, true), 0.0);
    }
    // The rejected spelling of the same screen union is *not* an identity at
    // `s = 0`, which is the whole reason the frozen one is written as it is. It
    // fails where a coverage is small — the tail of a feather, which is exactly
    // where a brush spends most of its area — so the sampling is logarithmic
    // rather than uniform: a uniform draw over `[0, 1]` essentially never lands
    // there and would report a clean sheet the property does not have.
    let mut rejected = 0usize;
    let mut worst = 0.0f64;
    let mut rng = SplitMix64(0x0018_ACC1);
    for _ in 0..200_000 {
        let c = 10f64.powf(rng.next_range(-30.0, 0.0));
        let round_tripped = 1.0 - (1.0 - c) * (1.0 - 0.0);
        if round_tripped.to_bits() != c.to_bits() {
            rejected += 1;
            worst = worst.max((round_tripped - c).abs() / c);
        }
        // The frozen spelling holds on every one of them.
        assert_eq!(accumulate(c, 0.0, false).to_bits(), c.to_bits());
        assert_eq!(accumulate(c, 0.0, true).to_bits(), c.to_bits());
    }
    assert!(rejected > 0, "the rejected spelling never failed");
    println!(
        "the rejected spelling 1 - (1 - c)(1 - s) fails the s = 0 identity on {rejected} of 200000 \
         log-sampled coverages, by up to {:.1}% of the coverage it was given",
        100.0 * worst
    );
}

/// A second pass of the brush is a second object and does build up: the same
/// stroke folded twice covers more than once, which is what the design claims
/// and the opposite of the component algebra's idempotence.
#[test]
fn a_second_pass_of_the_same_stroke_builds_up() {
    let stage = LANDSCAPE;
    let stroke = BrushStroke {
        points: vec![[0.3, 0.5], [0.7, 0.5]],
        size: 0.08,
        feather: 60.0,
        flow: 40.0,
        erase: false,
    };
    let once = Brush {
        strokes: vec![stroke.clone()],
    };
    let twice = Brush {
        strokes: vec![stroke.clone(), stroke],
    };
    let mut grew = 0usize;
    let mut worst = 0.0f64;
    for (u, v) in sample_points(&stage, 137) {
        let a = brush_coverage(&once, &stage, u, v);
        let b = brush_coverage(&twice, &stage, u, v);
        assert!(b >= a, "a second pass removed coverage at ({u}, {v})");
        if b > a {
            grew += 1;
            worst = worst.max(b - a);
        }
    }
    assert!(grew > 0, "a second pass changed nothing anywhere");
    println!("a second pass of one stroke adds up to {worst:.9} of coverage on {grew} samples");
}

/// Add strokes commute. The screen union is commutative and associative in the
/// reals, so a permutation is the same field; in `f64` the fold rounds
/// differently, and the deviation is measured here rather than claimed to be
/// zero.
#[test]
fn add_strokes_commute_under_the_frozen_rule() {
    let stage = LANDSCAPE;
    let mut rng = SplitMix64(0x0018_C0FF);
    let mut worst = 0.0f64;
    let mut identical = 0usize;
    let mut compared = 0usize;
    for _ in 0..300 {
        let count = 2 + rng.next_usize(5);
        let strokes: Vec<BrushStroke> =
            (0..count).map(|_| random_stroke(&mut rng, false)).collect();
        let ordered = Brush {
            strokes: strokes.clone(),
        };
        for _ in 0..4 {
            let mut shuffled = strokes.clone();
            for index in (1..shuffled.len()).rev() {
                shuffled.swap(index, rng.next_usize(index + 1));
            }
            let permuted = Brush { strokes: shuffled };
            for _ in 0..40 {
                let u = rng.next_range(0.0, 1.5);
                let v = rng.next_range(0.0, 1.0);
                let a = brush_coverage(&ordered, &stage, u, v);
                let b = brush_coverage(&permuted, &stage, u, v);
                if a.to_bits() == b.to_bits() {
                    identical += 1;
                }
                worst = worst.max((a - b).abs());
                compared += 1;
            }
        }
    }
    assert!(
        worst <= 1e-15,
        "permuting add strokes moved coverage by {worst:e}"
    );
    println!(
        "permuting add strokes: worst deviation {worst:e} over {compared} samples, \
         {identical} of them bit-identical"
    );
}

/// An erase stroke does not commute with an add, which is why a component stores
/// its strokes in order. The disagreement is a whole edit's worth of coverage,
/// not a rounding artefact.
#[test]
fn an_erase_stroke_does_not_commute_with_an_add() {
    let stage = LANDSCAPE;
    let add = BrushStroke {
        points: vec![[0.3, 0.5], [0.7, 0.5]],
        size: 0.1,
        feather: 50.0,
        flow: 100.0,
        erase: false,
    };
    let erase = BrushStroke {
        points: vec![[0.5, 0.35], [0.5, 0.65]],
        size: 0.1,
        feather: 50.0,
        flow: 100.0,
        erase: true,
    };
    let painted_then_erased = Brush {
        strokes: vec![add.clone(), erase.clone()],
    };
    let erased_then_painted = Brush {
        strokes: vec![erase, add],
    };
    let mut worst = 0.0f64;
    for (u, v) in sample_points(&stage, 37) {
        let a = brush_coverage(&painted_then_erased, &stage, u, v);
        let b = brush_coverage(&erased_then_painted, &stage, u, v);
        worst = worst.max((a - b).abs());
    }
    assert!(
        worst > 0.5,
        "the erase order changed coverage by only {worst}"
    );
    println!("reordering across an erase stroke changes coverage by up to {worst:.9}");
}

/// Deleting one stroke leaves the rest exactly as they were. This is what makes
/// `mask.delete-stroke` a forward edit rather than an approximation: the fold is
/// over the stored order, so removing an entry from the middle produces, bit for
/// bit, the field the remaining strokes would have produced had the deleted one
/// never been made.
#[test]
fn deleting_a_stroke_leaves_the_others_bit_identical() {
    let stage = PORTRAIT;
    let mut rng = SplitMix64(0x0001_8DE1);
    for _ in 0..200 {
        let count = 2 + rng.next_usize(6);
        let strokes: Vec<BrushStroke> = (0..count)
            .map(|index| random_stroke(&mut rng, index % 3 == 2))
            .collect();
        let removed = rng.next_usize(count);
        let with_gap = Brush {
            strokes: strokes
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != removed)
                .map(|(_, stroke)| stroke.clone())
                .collect(),
        };
        let never_made = Brush {
            strokes: with_gap.strokes.clone(),
        };
        for _ in 0..80 {
            let u = rng.next_range(0.0, 1.0);
            let v = rng.next_range(0.0, 1.0);
            assert_eq!(
                brush_coverage(&with_gap, &stage, u, v).to_bits(),
                brush_coverage(&never_made, &stage, u, v).to_bits()
            );
        }
    }
}

/// The property a grid index rests on, stated as a fact about the frozen
/// accumulation rather than about any index: dropping every stroke whose
/// coverage at a point is exactly zero leaves that point's coverage
/// bit-identical.
#[test]
fn dropping_strokes_that_cover_nothing_is_bit_identical() {
    let stage = LANDSCAPE;
    let mut rng = SplitMix64(0x0018_5C19);
    let mut dropped_any = 0usize;
    for _ in 0..300 {
        let count = 2 + rng.next_usize(6);
        let strokes: Vec<BrushStroke> = (0..count)
            .map(|index| random_stroke(&mut rng, index % 4 == 3))
            .collect();
        let whole = Brush {
            strokes: strokes.clone(),
        };
        for _ in 0..60 {
            let u = rng.next_range(0.0, 1.5);
            let v = rng.next_range(0.0, 1.0);
            let near: Vec<BrushStroke> = strokes
                .iter()
                .filter(|stroke| {
                    let segments = brush_segments(stroke, &stage);
                    stroke_coverage(stroke, &segments, u, v) != 0.0
                })
                .cloned()
                .collect();
            if near.len() < strokes.len() {
                dropped_any += 1;
            }
            let pruned = Brush { strokes: near };
            assert_eq!(
                brush_coverage(&whole, &stage, u, v).to_bits(),
                brush_coverage(&pruned, &stage, u, v).to_bits(),
                "pruning zero strokes changed coverage at ({u}, {v})"
            );
        }
    }
    assert!(dropped_any > 0, "no sample ever had a stroke to drop");
}

// ---------------------------------------------------------------------------
// Bounds and legality
// ---------------------------------------------------------------------------

/// The conservative box is correct: coverage is exactly zero outside it, over a
/// dense sweep of randomized components.
#[test]
fn coverage_is_exactly_zero_outside_the_conservative_box() {
    let mut rng = SplitMix64(0x0018_B0B0);
    for stage in [&LANDSCAPE, &PORTRAIT] {
        for _ in 0..300 {
            let count = 1 + rng.next_usize(5);
            let brush = Brush {
                strokes: (0..count)
                    .map(|index| random_stroke(&mut rng, index % 3 == 2))
                    .collect(),
            };
            let box_ = brush_bounds(&brush, stage);
            for _ in 0..200 {
                let u = rng.next_range(-0.5, 2.0);
                let v = rng.next_range(-0.5, 1.5);
                let outside = match box_ {
                    None => true,
                    Some([u0, v0, u1, v1]) => u < u0 || u > u1 || v < v0 || v > v1,
                };
                if outside {
                    assert_eq!(
                        brush_coverage(&brush, stage, u, v),
                        0.0,
                        "coverage outside the box at ({u}, {v})"
                    );
                }
            }
        }
    }
}

/// A stroke's radius is a stored mask-space distance and takes the study's own
/// rule, not a second convention: the floor is what bounds the divisor
/// `band = R · f` instead of guarding it.
#[test]
fn a_stroke_radius_takes_the_studys_own_distance_rule() {
    let point = BrushStroke {
        points: vec![[0.5, 0.5]],
        size: DISTANCE_MIN,
        feather: 100.0,
        flow: 100.0,
        erase: false,
    };
    assert!(brush_size_is_legal(&point));
    assert!(brush_size_is_legal(&BrushStroke {
        size: DISTANCE_MAX,
        ..point.clone()
    }));
    for illegal in [0.0, DISTANCE_MIN / 2.0, DISTANCE_MAX * 2.0, f64::NAN] {
        assert!(
            !brush_size_is_legal(&BrushStroke {
                size: illegal,
                ..point.clone()
            }),
            "{illegal} passed the distance rule"
        );
    }
    // The smallest legal band a feathered stroke can produce still bounds the
    // divisor: `1e-4 · 0.01` is `1e-6`, so the ratio it divides is bounded by
    // `1e6` and the reciprocal never approaches overflow.
    let smallest = DISTANCE_MIN * (0.01 / 100.0);
    assert!(smallest > 0.0 && smallest.is_finite());
    assert_eq!(smallest, 1e-8);
}

/// The brush shares the study's `smooth` and nothing else: the same easing the
/// gradients and the delivered vignette take, so the editor has one falloff
/// shape.
#[test]
fn the_brush_shares_the_studys_easing() {
    let stroke = BrushStroke {
        points: vec![[0.5, 0.5]],
        size: 0.2,
        feather: 100.0,
        flow: 100.0,
        erase: false,
    };
    // With feather 100 the band is the whole radius, so the profile at distance
    // `d` is `smooth((R - d) / R)` exactly.
    for step in 0..=20 {
        let d = 0.2 * f64::from(step) / 20.0;
        assert_eq!(
            capsule_profile(&stroke, d).to_bits(),
            smooth(((0.2 - d) / 0.2).clamp(0.0, 1.0)).to_bits()
        );
    }
}

// ---------------------------------------------------------------------------
// The study's figures
// ---------------------------------------------------------------------------

/// The dense figures quoted in `docs/design/mask-study.md#the-brush`. Ignored by
/// default because a dense sweep does not belong in an ordinary run.
#[test]
#[ignore = "a recorded measurement, not an assertion"]
fn brush_study_figures() {
    let stage = LANDSCAPE;
    let mut rng = SplitMix64(0x0018_F165);

    // Permutation deviation over add-only components, densely.
    let mut worst_permutation = 0.0f64;
    let mut identical = 0usize;
    let mut compared = 0usize;
    for _ in 0..500 {
        let count = 2 + rng.next_usize(7);
        let strokes: Vec<BrushStroke> =
            (0..count).map(|_| random_stroke(&mut rng, false)).collect();
        let ordered = Brush {
            strokes: strokes.clone(),
        };
        for _ in 0..8 {
            let mut shuffled = strokes.clone();
            for index in (1..shuffled.len()).rev() {
                shuffled.swap(index, rng.next_usize(index + 1));
            }
            let permuted = Brush { strokes: shuffled };
            for _ in 0..100 {
                let u = rng.next_range(0.0, 1.5);
                let v = rng.next_range(0.0, 1.0);
                let a = brush_coverage(&ordered, &stage, u, v);
                let b = brush_coverage(&permuted, &stage, u, v);
                if a.to_bits() == b.to_bits() {
                    identical += 1;
                }
                worst_permutation = worst_permutation.max((a - b).abs());
                compared += 1;
            }
        }
    }
    println!(
        "add-stroke permutation: worst {worst_permutation:e} over {compared} samples, \
         {identical} bit-identical ({:.2}%)",
        100.0 * identical as f64 / compared as f64
    );

    // What a permutation costs in output codes, through the masked blend at
    // +1 EV on 18% grey: the deviation cannot reach a code boundary.
    let input = 0.18f64;
    let effect = 0.36f64;
    println!(
        "that deviation moves a masked +1 EV channel on 18% grey by {:e} in linear light, \
         against the 1/255 = {:e} between output codes",
        worst_permutation * (effect - input),
        1.0 / 255.0
    );

    // The build-up a second pass adds, per flow.
    for flow in [25.0f64, 50.0, 100.0] {
        let stroke = BrushStroke {
            points: vec![[0.3, 0.5], [0.7, 0.5]],
            size: 0.08,
            feather: 60.0,
            flow,
            erase: false,
        };
        let once = Brush {
            strokes: vec![stroke.clone()],
        };
        let twice = Brush {
            strokes: vec![stroke.clone(), stroke],
        };
        let (mut a_max, mut b_max) = (0.0f64, 0.0f64);
        for (u, v) in sample_points(&stage, 17) {
            a_max = a_max.max(brush_coverage(&once, &stage, u, v));
            b_max = b_max.max(brush_coverage(&twice, &stage, u, v));
        }
        println!("flow {flow:5.1}: one pass reaches {a_max:.9}, two passes reach {b_max:.9}");
    }

    // Resampling a stroke's own path.
    let mut worst_resample = 0.0f64;
    for _ in 0..500 {
        let coarse = random_stroke(&mut rng, false);
        let mut fine_points = Vec::new();
        for pair in coarse.points.windows(2) {
            let [ax, ay] = pair[0];
            let [bx, by] = pair[1];
            for step in 0..8 {
                let t = f64::from(step) / 8.0;
                fine_points.push([ax + t * (bx - ax), ay + t * (by - ay)]);
            }
        }
        fine_points.push(*coarse.points.last().unwrap());
        let fine = BrushStroke {
            points: fine_points,
            ..coarse.clone()
        };
        let coarse_segments = brush_segments(&coarse, &stage);
        let fine_segments = brush_segments(&fine, &stage);
        for _ in 0..400 {
            let u = rng.next_range(-0.1, 1.6);
            let v = rng.next_range(-0.1, 1.1);
            worst_resample = worst_resample.max(
                (stroke_coverage(&coarse, &coarse_segments, u, v)
                    - stroke_coverage(&fine, &fine_segments, u, v))
                .abs(),
            );
        }
    }
    println!("resampling a stroke's own path moves coverage by at most {worst_resample:e}");
}
