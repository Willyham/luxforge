//! The brush component against the frozen study.
//!
//! The mathematics is `docs/design/mask-study.md#the-brush` and the oracle is the independent `f64`
//! reference at `crates/luxforge-reference/src/mask.rs`, which shares no code with production. What is asserted
//! here is **bit-identity**, not a tolerance: the production unit transcribes the study's
//! expressions in the study's order, so the two agree in their last bits or the transcription is
//! wrong.
//!
//! What is the brush's alone, beside the checklist every kind passes (`mask::kinds`, where randomized
//! strokes at every size, feather, flow and erase flag are held to the reference, the conservative
//! rectangle is checked exhaustively and the ramp is measured): a one-point stroke and a doubled-back
//! path; the accumulation properties the design claims; the occupancy cap and the stroke limits; and
//! the measurements that the grid index is doing its job.

use super::*;
use luxforge_core::{
    Component, ComponentMode, ErrorKind, Mask,
    mask::Stroke,
    mask::{CompiledMask, SEGMENTS_PER_PIXEL, STROKES_PER_COMPONENT},
    path::StrokeTable,
};
use luxforge_reference::mask::{Brush, Stage as RefStage, brush_coverage};
use serde_json::json;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A mask holding one add brush component over these strokes, with the table they resolve through.
fn brush_mask(strokes: &[Stroke]) -> (Mask, StrokeTable) {
    let mut table = StrokeTable::new("the brush tests");
    let addresses: Vec<String> = strokes
        .iter()
        .map(|stroke| table.insert(stroke.clone()).to_string())
        .collect();
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("brush");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "brush",
        json!({ "strokes": addresses }),
    ));
    (mask, table)
}

// ---------------------------------------------------------------------------
// The shapes the design names
// ---------------------------------------------------------------------------

/// The two shapes the design names by hand: a one-point stroke, which is a disc, and a doubled-back
/// path, which is one pass and not two. Both against the reference, bit for bit.
#[test]
fn a_one_point_stroke_and_a_doubled_back_path_match_the_reference() {
    let reference_stage = RefStage::new(80, 60);
    let cases = [
        vec![[0.5, 0.5]],
        vec![[0.3, 0.5], [0.7, 0.5], [0.3, 0.5]],
        vec![[0.3, 0.3], [0.7, 0.7], [0.3, 0.7], [0.7, 0.3]],
    ];
    for points in cases {
        for (feather, flow) in [(0.0, 100.0), (50.0, 100.0), (100.0, 40.0)] {
            let stroke = Stroke::capture(&points, 0.09, feather, flow, false).unwrap();
            let (mask, table) = brush_mask(std::slice::from_ref(&stroke));
            let compiled = CompiledMask::new(&mask, stage(80, 60), &table).unwrap();
            let expected = Brush {
                strokes: vec![reference_stroke(&stroke)],
            };
            for y in 0..60 {
                for x in 0..80 {
                    let (u, v) = reference_stage.pixel_uv(x, y);
                    assert_eq!(
                        compiled.coverage(x, y, ANY_PIXEL).to_bits(),
                        brush_coverage(&expected, &reference_stage, u, v, ANY_PIXEL).to_bits(),
                        "{points:?} feather {feather} flow {flow} at ({x}, {y})"
                    );
                }
            }
        }
    }
    // A one-point stroke is a disc: equal coverage at equal distance in every direction.
    let stroke = Stroke::capture(&[[0.5, 0.5]], 0.1, 60.0, 100.0, false).unwrap();
    let (mask, table) = brush_mask(std::slice::from_ref(&stroke));
    let square = CompiledMask::new(&mask, stage(101, 101), &table).unwrap();
    // Within the arithmetic of the pixel grid itself: `(x + 0.5)/H` rounds, so two pixels the same
    // number of steps either side of the centre are not exactly equidistant from it, and the field
    // is a disc to that precision and not beyond it.
    for offset in [5u32, 17, 33] {
        let north = square.coverage(50, 50 - offset, ANY_PIXEL);
        let south = square.coverage(50, 50 + offset, ANY_PIXEL);
        let east = square.coverage(50 + offset, 50, ANY_PIXEL);
        let west = square.coverage(50 - offset, 50, ANY_PIXEL);
        for (a, b) in [(north, south), (east, west), (north, east)] {
            assert!((a - b).abs() < 1e-12, "{a} against {b} at offset {offset}");
        }
    }
}

// ---------------------------------------------------------------------------
// The accumulation properties
// ---------------------------------------------------------------------------

/// Deleting one stroke from the middle of a component is indistinguishable from one never made, bit
/// for bit at every pixel, with an erase stroke after it so the property is not tested only where
/// it is trivial.
///
/// The deletion is the real one: `mask.delete-stroke` through the action path, on a catalog that
/// painted every stroke. Its component is compiled against a table that still holds the deleted
/// stroke — the store keeps it, because earlier entries reference it — and compared with a second
/// catalog that painted only the kept strokes. So the two sides are built from different stroke
/// lists and differ unless the compiled mask folds exactly the strokes its component still lists.
#[test]
fn deleting_a_stroke_is_indistinguishable_from_one_never_made() {
    use luxforge_core::mask::commands::{DELETE_STROKE, MaskTarget};
    let mut rng = SplitMix64(0x0018_DE1E);
    let paths: Vec<Vec<[f64; 2]>> = (0..5)
        .map(|_| {
            let mut at = [rng.next_range(0.3, 0.5), rng.next_range(0.3, 0.5)];
            (0..6)
                .map(|_| {
                    let point = at;
                    at = [
                        at[0] + rng.next_range(-0.08, 0.12),
                        at[1] + rng.next_range(-0.08, 0.12),
                    ];
                    point
                })
                .collect()
        })
        .collect();
    let erase = |index: usize| index == 3;
    let removed = 2;
    let paint = |name: &str, which: &[usize]| -> Painting {
        let mut painting = Painting::open(name);
        let mut target = MaskTarget::default();
        for &index in which {
            let painted = painting
                .paint_stroke(
                    &target,
                    &paths[index],
                    0.08,
                    erase(index),
                    &format!("s{index}"),
                )
                .expect("a stroke under the cap");
            target = MaskTarget {
                mask: painted.mask,
                component: painted.component,
                ..MaskTarget::default()
            };
        }
        painting
    };

    let mut deleting = paint("delete-stroke", &[0, 1, 2, 3, 4]);
    let painted = deleting.recipe();
    let mask = painted.masks[0].clone();
    let listed = &mask.components[0].payload["strokes"];
    let doomed = listed[removed]
        .as_str()
        .expect("a stroke address")
        .to_owned();
    let mutation = deleting.mutation("delete");
    deleting
        .service
        .run_action(
            &deleting.asset,
            mutation,
            DELETE_STROKE,
            MaskTarget {
                mask: Some(mask.id.clone()),
                component: Some(mask.components[0].id.clone()),
                stroke: Some(luxforge_core::path::StrokeId::parse(doomed.clone()).unwrap()),
                ..MaskTarget::default()
            }
            .request(json!({})),
        )
        .expect("a stroke deleted");
    let gapped = deleting.recipe();
    // The store still holds what the entries before the deletion reference, the deleted stroke
    // among them.
    let store = &painted.strokes;
    assert!(store.strokes().any(|(id, _)| id.as_str() == doomed));

    let never = paint("never-made", &[0, 1, 3, 4]).recipe();
    assert_eq!(
        gapped.masks[0].components[0].payload, never.masks[0].components[0].payload,
        "the component lists the kept strokes, in order"
    );

    let size = stage(64, 48);
    let with_gap = CompiledMask::new(&gapped.masks[0], size, store).unwrap();
    let never_made = CompiledMask::new(&never.masks[0], size, &never.strokes).unwrap();
    let with_every = CompiledMask::new(&mask, size, store).unwrap();
    let mut changed = 0usize;
    for y in 0..size.height {
        for x in 0..size.width {
            let gap = with_gap.coverage(x, y, ANY_PIXEL);
            assert_eq!(
                gap.to_bits(),
                never_made.coverage(x, y, ANY_PIXEL).to_bits(),
                "at ({x}, {y})"
            );
            if gap != with_every.coverage(x, y, ANY_PIXEL) {
                changed += 1;
            }
        }
    }
    assert!(
        changed > 0,
        "the deleted stroke covered nothing, so its deletion proves nothing"
    );
}

/// A stroke's density is its own, whatever the pointer's rate: painting the same path twice in one
/// stroke is one density, while painting it as two strokes builds up. Both against the field, not
/// against prose.
#[test]
fn one_pass_is_one_density_and_a_second_pass_builds_up() {
    let size = stage(64, 48);
    let path = [[0.25, 0.5], [0.75, 0.5]];
    let once = Stroke::capture(&path, 0.08, 60.0, 40.0, false).unwrap();
    // The same path posted at eight times the sampling rate: the decimation puts it back on the
    // same stored positions, so it is literally the same stored stroke and therefore the same
    // address.
    let dense: Vec<[f64; 2]> = (0..=8)
        .map(|step| {
            let t = f64::from(step) / 8.0;
            [0.25 + t * 0.5, 0.5]
        })
        .collect();
    let sampled = Stroke::capture(&dense, 0.08, 60.0, 40.0, false).unwrap();
    assert_eq!(once.id(), sampled.id(), "one path is one stored stroke");

    let (single, single_table) = brush_mask(std::slice::from_ref(&once));
    let (doubled, doubled_table) = brush_mask(&[once.clone(), once]);
    let single = CompiledMask::new(&single, size, &single_table).unwrap();
    let doubled = CompiledMask::new(&doubled, size, &doubled_table).unwrap();
    let mut grew = 0usize;
    for y in 0..size.height {
        for x in 0..size.width {
            let a = single.coverage(x, y, ANY_PIXEL);
            let b = doubled.coverage(x, y, ANY_PIXEL);
            assert!(b >= a, "a second pass removed coverage at ({x}, {y})");
            if b > a {
                grew += 1;
            }
        }
    }
    assert!(grew > 0, "a second pass changed nothing anywhere");
    // One pass reaches exactly the flow, and a second reaches the screen union of two.
    assert_eq!(single.coverage(32, 24, ANY_PIXEL), 0.4);
    assert_eq!(doubled.coverage(32, 24, ANY_PIXEL), 0.4 + (1.0 - 0.4) * 0.4);
}

// ---------------------------------------------------------------------------
// Limits and refusals
// ---------------------------------------------------------------------------

/// A compiled mask reports the densest cell of its index — the most segments one pixel tests, which
/// is what the cap bounds — and compiling never refuses for it: the cap is checked where a stroke
/// is painted. The index is exact at any occupancy, so even a component far over the cap is
/// bit-identical to the reference, which has no index.
#[test]
fn a_compiled_mask_reports_its_densest_cell_and_never_refuses_for_it() {
    // Many long zig-zag strokes through one small area: every segment's grown box reaches the same
    // cells, so the occupancy climbs with the stroke count.
    let strokes: Vec<Stroke> = (0..STROKES_PER_COMPONENT as u32)
        .map(|index| Stroke::capture(&dense_zig_zag(index), 0.05, 50.0, 100.0, false).unwrap())
        .collect();
    let (mask, table) = brush_mask(&strokes);
    let size = stage(80, 60);
    let compiled =
        CompiledMask::new(&mask, size, &table).expect("compiling never refuses for occupancy");
    let densest = compiled.densest_cell();
    assert!(
        densest > SEGMENTS_PER_PIXEL,
        "a component this dense lists {densest} segments in its densest cell"
    );
    let reference_stage = RefStage::new(size.width, size.height);
    let expected = Brush {
        strokes: strokes.iter().map(reference_stroke).collect(),
    };
    let mut tested = 0usize;
    for y in 0..size.height {
        for x in 0..size.width {
            let (u, v) = reference_stage.pixel_uv(x, y);
            assert_eq!(
                compiled.coverage(x, y, ANY_PIXEL).to_bits(),
                brush_coverage(&expected, &reference_stage, u, v, ANY_PIXEL).to_bits(),
                "at ({x}, {y})"
            );
            tested = tested.max(compiled.segments_at(x, y));
        }
    }
    assert!(
        tested <= densest,
        "a pixel tested {tested} segments, more than the densest cell's {densest}"
    );
    // The rule the painted stroke is refused through names the mask, the component and the limit,
    // and admits a component exactly at the cap.
    let refusal = luxforge_core::mask::rules::segments_per_pixel("Mask 1", "Brush 1", densest)
        .expect_err("over the cap");
    assert_eq!(refusal.kind, ErrorKind::ResourceLimit);
    assert_eq!(
        refusal.detail,
        format!(
            "this stroke would put {densest} stroke segments over one pixel of mask Mask 1 \
             component Brush 1; the limit is {SEGMENTS_PER_PIXEL} segments tested per pixel by a \
             brush component"
        )
    );
    luxforge_core::mask::rules::segments_per_pixel("Mask 1", "Brush 1", SEGMENTS_PER_PIXEL)
        .expect("exactly the cap is legal");

    // A sparse component of the same stroke count is well under it, so occupancy is about density
    // and not about the count.
    let spread: Vec<Stroke> = (0..STROKES_PER_COMPONENT as u32)
        .map(|index| {
            let t = f64::from(index) / f64::from(STROKES_PER_COMPONENT as u32);
            Stroke::capture(&[[0.02 + 0.96 * t, 0.5]], 0.004, 50.0, 100.0, false).unwrap()
        })
        .collect();
    let (sparse, sparse_table) = brush_mask(&spread);
    let sparse = CompiledMask::new(&sparse, stage(400, 300), &sparse_table).unwrap();
    assert!(sparse.densest_cell() <= SEGMENTS_PER_PIXEL);
}

/// A catalog with one imported photograph, driven through the one action path every client reaches.
struct Painting {
    service: luxforge_core::EditorService,
    asset: luxforge_core::AssetId,
}

impl Painting {
    fn open(name: &str) -> Self {
        let dir = temp(name);
        let source = dir.join("orientation-1.jpg");
        std::fs::copy(luxforge_testbase::paths::jpeg(), &source).unwrap();
        let mut service = luxforge_core::EditorService::open(&dir.join("catalog.sqlite")).unwrap();
        let asset = service.import(&source).unwrap().asset.id;
        Self { service, asset }
    }

    fn mutation(&self, request: &str) -> luxforge_core::Mutation {
        luxforge_core::Mutation {
            expected_revision: self.service.state(&self.asset).unwrap().revision,
            request_id: request.to_owned(),
            actor: "brush-occupancy".to_owned(),
        }
    }

    /// One `mask.add-stroke` through the action path: onto a new mask when `target` names none, and
    /// appended to the named brush otherwise.
    fn paint(
        &mut self,
        target: &luxforge_core::mask::commands::MaskTarget,
        points: &[[f64; 2]],
        request: &str,
    ) -> Result<luxforge_core::ActionResult, luxforge_core::Error> {
        self.paint_at(target, points, 0.05, request)
    }

    /// [`Self::paint`] with a brush of radius `size`.
    fn paint_at(
        &mut self,
        target: &luxforge_core::mask::commands::MaskTarget,
        points: &[[f64; 2]],
        size: f64,
        request: &str,
    ) -> Result<luxforge_core::ActionResult, luxforge_core::Error> {
        self.paint_stroke(target, points, size, false, request)
    }

    /// [`Self::paint_at`], adding or erasing.
    fn paint_stroke(
        &mut self,
        target: &luxforge_core::mask::commands::MaskTarget,
        points: &[[f64; 2]],
        size: f64,
        erase: bool,
        request: &str,
    ) -> Result<luxforge_core::ActionResult, luxforge_core::Error> {
        let mutation = self.mutation(request);
        self.service.run_action(
            &self.asset,
            mutation,
            luxforge_core::mask::commands::ADD_STROKE,
            target.request(json!({
                "points": points, "size": size, "feather": 50.0, "flow": 100.0,
                "erase": erase, "colour_refine": 50.0,
            })),
        )
    }

    /// The current entry's stack, with the strokes it references resolved.
    fn recipe(&self) -> luxforge_core::Recipe {
        self.service
            .state(&self.asset)
            .unwrap()
            .current_entry
            .snapshot
            .recipe
    }

    fn masks(&self) -> Value {
        let entry = self.service.state(&self.asset).unwrap().current_entry.id;
        serde_json::to_value(self.service.mask_listing(&self.asset, &entry).unwrap()).unwrap()
    }
}

/// A zig-zag through one small area: every segment's grown box reaches the same few cells, so each
/// stroke adds about forty segments to them and the second one already passes the cap.
fn dense_zig_zag(index: u32) -> Vec<[f64; 2]> {
    let base = 0.45 + f64::from(index) * 0.0001;
    (0..40)
        .map(|step| {
            let t = f64::from(step);
            [base + 0.02 * (t % 2.0), 0.45 + 0.0005 * t]
        })
        .collect()
}

/// Paint dense strokes onto one brush on a mask no layer draws, until one is refused or the
/// component is full. Answers the mask, the component, the strokes that committed and the refusal.
fn paint_until_refused(
    painting: &mut Painting,
) -> (
    luxforge_core::MaskId,
    luxforge_core::ComponentId,
    usize,
    Option<(luxforge_core::Error, Value, u64)>,
) {
    use luxforge_core::mask::commands::MaskTarget;
    let first = painting
        .paint(&MaskTarget::default(), &dense_zig_zag(0), "stroke-0")
        .expect("one stroke alone is under the cap");
    let target = MaskTarget {
        mask: first.mask.clone(),
        component: first.component.clone(),
        ..MaskTarget::default()
    };
    let mut committed = 1;
    for index in 1..STROKES_PER_COMPONENT as u32 {
        let masks = painting.masks();
        let revision = painting.service.state(&painting.asset).unwrap().revision;
        match painting.paint(&target, &dense_zig_zag(index), &format!("stroke-{index}")) {
            Ok(_) => committed += 1,
            Err(error) => {
                return (
                    target.mask.unwrap(),
                    target.component.unwrap(),
                    committed,
                    Some((error, masks, revision)),
                );
            }
        }
    }
    (
        target.mask.unwrap(),
        target.component.unwrap(),
        committed,
        None,
    )
}

/// A stroke that would put more than the cap's segments over one cell is refused **where it is
/// painted**, on a mask no layer draws yet, with `resource-limit` naming the mask and the component —
/// and nothing about it commits: the revision, the entry and the mask table are what they were.
#[test]
fn a_stroke_that_would_pass_the_occupancy_cap_is_refused_where_it_is_painted() {
    let mut painting = Painting::open("occupancy-refused");
    let (_, _, committed, refused) = paint_until_refused(&mut painting);
    let (error, masks_before, revision_before) = refused.unwrap_or_else(|| {
        panic!(
            "all {committed} dense strokes committed to a mask no layer draws; the stroke that \
             crossed the {SEGMENTS_PER_PIXEL}-segment cap was not refused where it was painted"
        )
    });
    assert_eq!(error.kind, ErrorKind::ResourceLimit, "{}", error.detail);
    assert!(
        error.detail.contains("mask Mask 1")
            && error.detail.contains("component Brush 1")
            && error
                .detail
                .contains(&format!("{SEGMENTS_PER_PIXEL} segments tested per pixel")),
        "{}",
        error.detail
    );
    assert_eq!(
        painting.service.state(&painting.asset).unwrap().revision,
        revision_before,
        "a refused stroke writes no entry"
    );
    assert_eq!(
        painting.masks(),
        masks_before,
        "a refused stroke leaves the mask table as it was"
    );
    // It does not spill into a new component either: the mask still holds the one brush.
    assert_eq!(
        masks_before["masks"][0]["components"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// The cap is paid for in full by the stroke that would cross it, so a mask whose strokes all
/// committed is never refused for occupancy when a layer first draws it, and renders.
#[test]
fn an_adjustment_through_a_painted_mask_is_never_refused_for_occupancy() {
    let mut painting = Painting::open("occupancy-adjusted");
    let (mask, _, _, _) = paint_until_refused(&mut painting);
    let mutation = painting.mutation("lift");
    painting
        .service
        .apply_action(
            &painting.asset,
            mutation,
            "set-basic",
            json!({"mask": mask.as_str(), "exposure": 1.0}),
        )
        .unwrap_or_else(|error| {
            panic!("an adjustment through a committed mask was refused: {error}")
        });
    painting
        .service
        .render_current(&painting.asset)
        .expect("the masked stack renders");
}

/// The 1024-position bound is a bound on the **stored** stroke. A raw path an agent posts is checked
/// generously and decimated first, so 2000 positions along one straight drag are one two-position
/// stroke; a path that is still longer than the bound after decimation is refused by that bound's
/// own name, and nothing commits.
#[test]
fn a_raw_path_is_bounded_after_decimation_not_before() {
    use luxforge_core::{
        mask::commands::MaskTarget,
        path::{COORDINATE_STEPS_PER_UNIT, POINTS_PER_STROKE},
    };
    let mut painting = Painting::open("raw-path");
    let raw: Vec<[f64; 2]> = (0..2000)
        .map(|index| [0.2 + 0.6 * f64::from(index) / 1999.0, 0.5])
        .collect();
    let painted = painting
        .paint(&MaskTarget::default(), &raw, "raw")
        .unwrap_or_else(|error| {
            panic!("a raw 2000-position path that decimates to two was refused: {error}")
        });
    let masks = painting.masks();
    let strokes = masks["masks"][0]["components"][0]["payload"]["strokes"]
        .as_array()
        .expect("the brush lists its strokes")
        .clone();
    assert_eq!(strokes.len(), 1, "{masks}");

    // Every other position eight grid steps off the line: a small brush's two-step tolerance keeps
    // all of them, so 1100 posted positions are 1100 stored ones, over the bound.
    let incompressible: Vec<[f64; 2]> = (0..POINTS_PER_STROKE + 76)
        .map(|index| {
            let along = index as f64 * 8.0 / COORDINATE_STEPS_PER_UNIT;
            let across = if index % 2 == 0 { 0.0 } else { 8.0 } / COORDINATE_STEPS_PER_UNIT;
            [0.1 + along, 0.5 + across]
        })
        .collect();
    let target = MaskTarget {
        mask: painted.mask.clone(),
        component: painted.component.clone(),
        ..MaskTarget::default()
    };
    let revision = painting.service.state(&painting.asset).unwrap().revision;
    let error = painting
        .paint_at(&target, &incompressible, 0.001, "incompressible")
        .expect_err("a stroke over the bound after decimation");
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert_eq!(
        error.detail,
        format!(
            "stroke has {} positions after decimation; the limit is {POINTS_PER_STROKE} points \
             per stroke",
            POINTS_PER_STROKE + 76
        )
    );
    assert_eq!(
        painting.service.state(&painting.asset).unwrap().revision,
        revision
    );
}

/// More strokes than a component may hold is refused by name, without a stage and without the store,
/// so it fails reading the recipe as well as drawing it.
#[test]
fn a_component_over_the_stroke_limit_names_the_limit() {
    let mut strokes = Vec::new();
    for index in 0..=STROKES_PER_COMPONENT {
        let t = f64::from(index as u32) / f64::from(STROKES_PER_COMPONENT as u32 + 1);
        strokes.push(Stroke::capture(&[[0.02 + 0.9 * t, 0.5]], 0.01, 50.0, 100.0, false).unwrap());
    }
    let (mask, table) = brush_mask(&strokes);
    let error = CompiledMask::new(&mask, stage(64, 48), &table).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert_eq!(
        error.detail,
        format!(
            "component Brush 1 has {} strokes; the limit is {STROKES_PER_COMPONENT} strokes per \
             brush component",
            STROKES_PER_COMPONENT + 1
        )
    );
    // The stage-free half refuses it too, so a recipe carrying it fails to compile anywhere.
    assert!(luxforge_core::mask::validate_component_kinds(&mask).is_err());
}

// ---------------------------------------------------------------------------
// The grid index
// ---------------------------------------------------------------------------

/// The measurement the index exists for: at a fixed local density, the cost of evaluating one pixel
/// does not grow with the component's total stroke count. Strokes are added far from the sampled
/// region, so what changes is the count and not what the pixel can reach.
///
/// Ignored by default because it is a measurement rather than a pass/fail property; the assertion it
/// does make is deliberately loose, because a wall clock on a shared host is not a proof.
#[test]
#[ignore = "a recorded measurement, not an assertion"]
fn evaluation_cost_does_not_grow_with_stroke_count() {
    let size = stage(400, 300);
    // Two strokes the sampled region actually touches, and then padding far away.
    let near = [
        Stroke::capture(&[[0.10, 0.10], [0.16, 0.16]], 0.02, 50.0, 100.0, false).unwrap(),
        Stroke::capture(&[[0.12, 0.16], [0.16, 0.10]], 0.02, 50.0, 100.0, false).unwrap(),
    ];
    // The reference has no index and folds every stroke at every pixel, so it is the contrast: what
    // the same field costs when the cost *does* grow with the count.
    let reference_stage = RefStage::new(size.width, size.height);
    println!("strokes  indexed ns/px  unindexed ns/px  coverage at the sample");
    let mut baseline = 0.0f64;
    for count in [2usize, 8, 16, 32, 64] {
        let mut strokes: Vec<Stroke> = near.to_vec();
        for index in 0..(count - near.len()) {
            let t = f64::from(index as u32) / f64::from(count as u32);
            strokes.push(
                Stroke::capture(
                    &[[0.55 + 0.4 * t, 0.7], [0.56 + 0.4 * t, 0.9]],
                    0.01,
                    50.0,
                    100.0,
                    false,
                )
                .unwrap(),
            );
        }
        let (mask, table) = brush_mask(&strokes);
        let compiled = CompiledMask::new(&mask, size, &table).expect("under the cap");
        // The sampled window is the neighbourhood of the two near strokes, so the local density is
        // the same in every row of the table.
        let rounds = 200;
        let started = std::time::Instant::now();
        let mut total = 0.0f64;
        for _ in 0..rounds {
            for y in 20..70 {
                for x in 20..70 {
                    total += compiled.coverage(x, y, ANY_PIXEL);
                }
            }
        }
        let elapsed = started.elapsed();
        std::hint::black_box(total);
        let per_pixel = elapsed.as_secs_f64() * 1e9 / f64::from(rounds * 50 * 50);

        let unindexed = Brush {
            strokes: strokes.iter().map(reference_stroke).collect(),
        };
        let reference_rounds = 20;
        let started = std::time::Instant::now();
        let mut total = 0.0f64;
        for _ in 0..reference_rounds {
            for y in 20..70 {
                for x in 20..70 {
                    let (u, v) = reference_stage.pixel_uv(x, y);
                    total += brush_coverage(&unindexed, &reference_stage, u, v, ANY_PIXEL);
                }
            }
        }
        let reference_elapsed = started.elapsed();
        std::hint::black_box(total);
        let reference_per_pixel =
            reference_elapsed.as_secs_f64() * 1e9 / f64::from(reference_rounds * 50 * 50);

        println!(
            "{count:7}  {per_pixel:13.2}  {reference_per_pixel:15.2}  {:.6}",
            compiled.coverage(45, 45, ANY_PIXEL)
        );
        if count == 2 {
            baseline = per_pixel;
        } else {
            assert!(
                per_pixel < baseline * 4.0,
                "{count} strokes cost {per_pixel:.2} ns/pixel against a 2-stroke {baseline:.2}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The occupancy of a realistic scrub
// ---------------------------------------------------------------------------

/// The Fit view a scrub is painted in: a 6000 × 4000 photograph shown 1500 × 1000 display pixels,
/// so one display pixel is a thousandth of the content height. Pointer positions arrive as whole
/// display pixels, which is what makes a hand-drawn path jagged at the stored grid's scale.
const VIEW: (f64, f64) = (1500.0, 1000.0);

/// One pass of the pointer along an arc of `radius` display pixels about `centre`, from `from` to
/// `to` radians, one event every `spacing` pixels, each position carrying up to `jitter` pixels of
/// hand shake before it is rounded to the display pixel it lands in, and drifted `drift` pixels
/// across the arc for the whole pass. Answers content-normalized positions.
fn pointer_arc(
    rng: &mut SplitMix64,
    centre: (f64, f64),
    radius: f64,
    (from, to): (f64, f64),
    spacing: f64,
    jitter: f64,
    drift: f64,
) -> Vec<[f64; 2]> {
    let events = ((to - from).abs() * radius / spacing).max(2.0) as usize;
    (0..=events)
        .map(|event| {
            let angle = from + (to - from) * event as f64 / events as f64;
            let reach = radius + drift;
            let x = centre.0 + reach * angle.cos() + rng.next_range(-jitter, jitter);
            let y = centre.1 + reach * angle.sin() + rng.next_range(-jitter, jitter);
            [x.round() / VIEW.0, y.round() / VIEW.1]
        })
        .collect()
}

/// Back and forth along one 300 px arc of radius 400 px, `passes` times, at about 600 px/s on a
/// 120 Hz pointer, drifting up to `drift` pixels across the arc on each pass.
fn scrub_passes(rng: &mut SplitMix64, passes: usize, drift: f64) -> Vec<[f64; 2]> {
    let radius = 400.0;
    let centre = (VIEW.0 / 2.0, VIEW.1 / 2.0 + radius);
    let half = 300.0 / radius / 2.0;
    let top = -std::f64::consts::FRAC_PI_2;
    (0..passes)
        .flat_map(|pass| {
            let sweep = if pass % 2 == 0 {
                (top - half, top + half)
            } else {
                (top + half, top - half)
            };
            let drift = rng.next_range(-drift, drift);
            pointer_arc(rng, centre, radius, sweep, 5.0, 1.0, drift)
        })
        .collect()
}

/// The three realistic ways of painting one area over and over, each as the raw paths a pointer
/// posts: one stroke scrubbed six times, six overlapping single-pass strokes, and eight short arcs
/// laid across one another.
fn scrub_workloads() -> Vec<(&'static str, Vec<Vec<[f64; 2]>>)> {
    let mut rng = SplitMix64(0x5C2B_0CC0);
    let one = vec![scrub_passes(&mut rng, 6, 8.0)];
    let six = (0..6).map(|_| scrub_passes(&mut rng, 1, 20.0)).collect();
    let arcs = (0..8)
        .map(|_| {
            let radius = rng.next_range(60.0, 200.0);
            let from = rng.next_range(0.0, std::f64::consts::TAU);
            let sweep = rng.next_range(0.8, 1.6) * if rng.next_bool() { 1.0 } else { -1.0 };
            let centre = (
                VIEW.0 / 2.0 + rng.next_range(-60.0, 60.0),
                VIEW.1 / 2.0 + rng.next_range(-60.0, 60.0),
            );
            pointer_arc(
                &mut rng,
                centre,
                radius,
                (from, from + sweep),
                4.0,
                1.0,
                0.0,
            )
        })
        .collect();
    vec![
        ("one six-pass scrub", one),
        ("six overlapping strokes", six),
        ("eight short arcs", arcs),
    ]
}

/// The brush sizes a scrub is measured at: the desktop's default brush, a tenth of the frame's
/// height across, and three smaller ones down to a hundredth.
const SCRUB_SIZES: [f64; 4] = [0.1, 0.05, 0.02, 0.01];

/// One scrub workload at one brush size, as the host stores and compiles it.
struct ScrubFigures {
    posted: usize,
    stored: usize,
    densest: usize,
    mean_tested: f64,
    most_tested: usize,
}

fn measure_scrub(paths: &[Vec<[f64; 2]>], size: f64) -> ScrubFigures {
    let strokes: Vec<Stroke> = paths
        .iter()
        .map(|path| Stroke::capture(path, size, 50.0, 100.0, false).expect("a legal stroke"))
        .collect();
    let (mask, table) = brush_mask(&strokes);
    // The grid index lives in mask space, so it depends on the stage only through its aspect:
    // compiled against the view it is the index the 6000 × 4000 content stage gets, and the view's
    // pixels are the ones the per-pixel cost is read at.
    let view = stage(VIEW.0 as u32, VIEW.1 as u32);
    let compiled = CompiledMask::new(&mask, view, &table).expect("compiling never refuses");
    let (mut reached, mut total, mut most) = (0usize, 0usize, 0usize);
    for y in 0..view.height {
        for x in 0..view.width {
            let tested = compiled.segments_at(x, y);
            if tested > 0 {
                reached += 1;
                total += tested;
                most = most.max(tested);
            }
        }
    }
    ScrubFigures {
        posted: paths.iter().map(Vec::len).sum(),
        stored: strokes.iter().map(Stroke::point_count).sum(),
        densest: compiled.densest_cell(),
        mean_tested: total as f64 / reached.max(1) as f64,
        most_tested: most,
    }
}

/// The measurement the occupancy cap is sized against: a realistic back-and-forth scrub, painted
/// the way a hand paints it at Fit, captured through the host's own decimation and compiled through
/// `CompiledMask::new`. It records, for each workload and brush size, the positions posted and
/// stored, the densest cell of the index — the most segments one pixel tests, which is what the
/// cap bounds — and the per-pixel cost over every pixel the brush can reach: the mean and the most
/// segments tested. The figures are in `docs/design/masking.md`.
#[test]
fn a_realistic_scrub_records_its_occupancy_and_per_pixel_cost() {
    println!(
        "workload                  size   posted  stored  densest  mean tested/px  most tested/px"
    );
    for (name, paths) in scrub_workloads() {
        for size in SCRUB_SIZES {
            let figures = measure_scrub(&paths, size);
            println!(
                "{name:24}  {size:5.3}  {:6}  {:6}  {:7}  {:14.1}  {:14}",
                figures.posted,
                figures.stored,
                figures.densest,
                figures.mean_tested,
                figures.most_tested,
            );
            assert!(figures.stored >= paths.len() && figures.stored <= figures.posted);
            assert!(
                figures.most_tested <= figures.densest,
                "a pixel tested more segments than the densest cell lists"
            );
            // Ordinary painting stays under the cap at the default brush and the smaller ones:
            // decimation keeps a path to within a fraction of its own radius, so a larger brush
            // stores proportionally fewer positions for the cells its radius makes larger.
            assert!(
                figures.densest <= SEGMENTS_PER_PIXEL,
                "{name} at size {size} puts {} segments in one cell, over the \
                 {SEGMENTS_PER_PIXEL}-segment cap",
                figures.densest
            );
        }
    }
}

/// The property that measurement is about, stated as an assertion a test run can keep: a pixel's
/// coverage is exactly what the strokes near it give, whatever else the component holds. Padding a
/// component with strokes nowhere near the pixel changes nothing, bit for bit.
#[test]
fn strokes_nowhere_near_a_pixel_change_nothing() {
    let size = stage(200, 150);
    let near = [
        Stroke::capture(&[[0.10, 0.10], [0.16, 0.16]], 0.02, 50.0, 100.0, false).unwrap(),
        Stroke::capture(&[[0.12, 0.16], [0.16, 0.10]], 0.02, 50.0, 100.0, true).unwrap(),
    ];
    let (bare, bare_table) = brush_mask(&near);
    let bare = CompiledMask::new(&bare, size, &bare_table).unwrap();

    let mut padded_strokes: Vec<Stroke> = near.to_vec();
    for index in 0..40u32 {
        let t = f64::from(index) / 40.0;
        padded_strokes.push(
            Stroke::capture(
                &[[0.55 + 0.4 * t, 0.7], [0.56 + 0.4 * t, 0.9]],
                0.01,
                50.0,
                100.0,
                false,
            )
            .unwrap(),
        );
    }
    let (padded, padded_table) = brush_mask(&padded_strokes);
    let padded = CompiledMask::new(&padded, size, &padded_table).unwrap();

    let mut nonzero = 0usize;
    for y in 0..40 {
        for x in 0..50 {
            let a = bare.coverage(x, y, ANY_PIXEL);
            assert_eq!(
                a.to_bits(),
                padded.coverage(x, y, ANY_PIXEL).to_bits(),
                "at ({x}, {y})"
            );
            if a > 0.0 {
                nonzero += 1;
            }
        }
    }
    assert!(nonzero > 0, "the sampled window covered nothing");
}

/// The index never changes an answer: production with its index is bit-identical to the reference,
/// which has none, including where a stroke's segments straddle several cells and where a pixel sits
/// on a cell boundary. This is the whole-field version of the claim the index rests on.
#[test]
fn the_index_changes_no_answer_anywhere() {
    let mut rng = SplitMix64(0x0018_10DE);
    for (width, height) in [(53u32, 31u32), (31, 53)] {
        let reference_stage = RefStage::new(width, height);
        for _ in 0..60 {
            // Long strokes with several segments, and a mixture of radii, so a segment's grown box
            // reaches many cells and the index has work to do.
            let strokes: Vec<Stroke> = (0..4)
                .map(|index| {
                    let mut points = Vec::new();
                    let mut x = rng.next_range(-0.2, 1.2);
                    let mut y = rng.next_range(-0.2, 1.2);
                    for _ in 0..12 {
                        points.push([x, y]);
                        x = (x + rng.next_range(-0.4, 0.4)).clamp(-1.0, 2.0);
                        y = (y + rng.next_range(-0.4, 0.4)).clamp(-1.0, 2.0);
                    }
                    Stroke::capture(
                        &points,
                        rng.next_range(0.005, 0.3),
                        rng.next_range(0.0, 100.0),
                        rng.next_range(1.0, 100.0),
                        index == 2,
                    )
                    .unwrap()
                })
                .collect();
            let (mask, table) = brush_mask(&strokes);
            let compiled = CompiledMask::new(&mask, stage(width, height), &table).unwrap();
            let expected = Brush {
                strokes: strokes.iter().map(reference_stroke).collect(),
            };
            for y in 0..height {
                for x in 0..width {
                    let (u, v) = reference_stage.pixel_uv(x, y);
                    assert_eq!(
                        compiled.coverage(x, y, ANY_PIXEL).to_bits(),
                        brush_coverage(&expected, &reference_stage, u, v, ANY_PIXEL).to_bits(),
                        "at ({x}, {y}) on {width}x{height}"
                    );
                }
            }
        }
    }
}
