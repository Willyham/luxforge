//! A warp's coordinate grid against the exact map it samples, on the strongest lens and
//! perspective fixtures (`fixtures/geometry/lens-perspective.json`): at Fit and at full size, over
//! a whole output stage and over a 100% region, the content the grid's interpolated coordinate reads
//! lies within [`GRID_SAMPLE_TOLERANCE_PX`] output pixels of where the exact map puts it, and a
//! stage drawn magnified within [`GRID_TOLERANCE_PX`] display pixels. The displacement is measured
//! through the map's own exact inverse, `to_output`, not the Jacobian estimate the grid chooses its
//! density with.
use super::{CoordinateGrid, GRID_MAX_NODES, GRID_SAMPLE_TOLERANCE_PX, GRID_TOLERANCE_PX};
use crate::{
    ErrorKind,
    modules::{Region, Stage},
    render::map::{GeometryMap, RadialModel, StageSize, WarpStep},
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../fixtures/geometry/lens-perspective.json"
    ))
    .unwrap()
}

/// The fixture's lens profile `name`, bound to a `width` × `height` stage.
fn lens(name: &str, width: u32, height: u32) -> WarpStep {
    let fixture = fixture();
    let row = fixture["mappings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("no fixture lens {name}"));
    let model = match row["model"].as_str().unwrap() {
        "Poly3" => RadialModel::Poly3,
        "Poly5" => RadialModel::Poly5,
        _ => RadialModel::PtLens,
    };
    WarpStep::radial(
        model,
        serde_json::from_value(row["terms"].clone()).unwrap(),
        row["unit_scale"].as_f64().unwrap(),
        Stage { width, height },
    )
    .unwrap()
}

fn whole(width: u32, height: u32) -> Region {
    Region {
        x0: 0,
        y0: 0,
        width,
        height,
    }
}

/// A map from a `width` × `height` content stage to an output stage of the same size.
fn map(width: u32, height: u32, steps: Vec<WarpStep>) -> GeometryMap {
    let size = StageSize { width, height };
    GeometryMap::from_steps(size, size, steps).unwrap()
}

/// The displacement, in output pixels, of the content the grid reads at the centre of output pixel
/// `(x, y)`: where the exact inverse sends the grid's coordinate, against the centre itself.
fn displacement(map: &GeometryMap, grid: &CoordinateGrid, x: u32, y: u32) -> f64 {
    let (cx, cy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
    let [u, v] = grid.sample(cx as f32, cy as f32);
    let (ox, oy) = map
        .to_output(f64::from(u), f64::from(v))
        .unwrap_or_else(|error| panic!("({x}, {y}) reads outside the map: {error:?}"));
    (ox - cx).hypot(oy - cy)
}

/// The worst displacement over `region`: every `stride`-th pixel on both axes, and every pixel of
/// the 48 × 48 blocks at its corners, where a lens bends hardest.
fn worst(map: &GeometryMap, grid: &CoordinateGrid, region: Region, stride: u32) -> f64 {
    let mut worst = 0.0_f64;
    let mut check = |x: u32, y: u32| worst = worst.max(displacement(map, grid, x, y));
    for y in (region.y0..region.y1()).step_by(stride as usize) {
        for x in (region.x0..region.x1()).step_by(stride as usize) {
            check(x, y);
        }
    }
    let corner = 48.min(region.width).min(region.height);
    for (cx, cy) in [
        (region.x0, region.y0),
        (region.x1() - corner, region.y0),
        (region.x0, region.y1() - corner),
        (region.x1() - corner, region.y1() - corner),
    ] {
        for y in cy..cy + corner {
            for x in cx..cx + corner {
                check(x, y);
            }
        }
    }
    worst
}

/// One grid of `map` over `region`, checked, with what it took printed for the record.
fn assert_within_tolerance(what: &str, map: &GeometryMap, region: Region, stride: u32) {
    let grid = CoordinateGrid::new(map, region, 1.0).unwrap();
    let error = worst(map, &grid, region, stride);
    eprintln!(
        "{what}: {}x{} region, spacing {} px, {} x {} nodes ({} KiB), worst {error:.4} px",
        region.width,
        region.height,
        grid.spacing,
        grid.columns,
        grid.rows,
        grid.bytes() / 1024
    );
    assert!(
        error <= GRID_SAMPLE_TOLERANCE_PX,
        "{what}: the grid's content lies {error} px from the map's"
    );
    assert!(grid.nodes.len() <= GRID_MAX_NODES);
}

/// The two strongest lens profiles of the fixture — the largest cover and local scale — at the
/// fixture's full 6048 × 4024 stage, at a Fit proxy of it and at a small window's Fit.
#[test]
fn a_grid_holds_the_strongest_lens_profiles_within_its_tolerance() {
    for name in [
        "Sigma 17-50 EX DC HSM at 17",
        "Sony E 10-18 at 10 (full-frame calibration)",
    ] {
        for (width, height, stride) in [(6048, 4024, 13), (2048, 1363, 5), (1024, 681, 3)] {
            let map = map(width, height, vec![lens(name, width, height)]);
            assert_within_tolerance(
                &format!("{name} at {width}x{height}"),
                &map,
                whole(width, height),
                stride,
            );
        }
    }
}

/// The fixture's strongest perspective, 100 on both axes, on its widest and its squarest stage
/// (cover 1.5), at full size and at Fit.
#[test]
fn a_grid_holds_the_strongest_perspective_within_its_tolerance() {
    for (width, height, stride) in [
        (6048, 4024, 13),
        (4000, 4000, 11),
        (2048, 1363, 5),
        (1024, 1024, 3),
    ] {
        let map = map(
            width,
            height,
            vec![WarpStep::projective(100, 100, Stage { width, height }).unwrap()],
        );
        assert_within_tolerance(
            &format!("perspective 100, 100 at {width}x{height}"),
            &map,
            whole(width, height),
            stride,
        );
    }
}

/// A lens, a perspective and a straightening rotation composed into one tail, as a stack with all
/// three compiles, over a whole Fit stage and over a 100% region away from the origin.
#[test]
fn a_grid_holds_a_composed_tail_and_a_region() {
    let (width, height) = (2048, 1363);
    let stage = Stage { width, height };
    let angle = 3.5_f64.to_radians();
    let (sin, cos) = angle.sin_cos();
    let (cx, cy) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
    // Output to input: a rotation about the stage centre, scaled in by its cover.
    let scale = 0.9;
    let rotation = WarpStep::Affine([
        cos * scale,
        -sin * scale,
        cx - (cos * cx - sin * cy) * scale,
        sin * scale,
        cos * scale,
        cy - (sin * cx + cos * cy) * scale,
    ]);
    let steps = vec![
        lens("NIKKOR Z 24-70mm f/4 S at 35", width, height),
        WarpStep::projective(35, -25, stage).unwrap(),
        rotation,
    ];
    let map = map(width, height, steps);
    assert_within_tolerance(
        "lens, perspective and rotation",
        &map,
        whole(width, height),
        5,
    );
    let full = (6048, 4024);
    let map_full = self::map(
        full.0,
        full.1,
        vec![lens("Sigma 17-50 EX DC HSM at 17", full.0, full.1)],
    );
    let region = Region {
        x0: 4100,
        y0: 2600,
        width: 1728,
        height: 1117,
    };
    assert_within_tolerance("a 100% region of the Sigma lens", &map_full, region, 3);
}

/// Up to a fourfold zoom the output-pixel bound is the tighter, so a stage drawn larger keeps the
/// grid it has at one display pixel per output pixel; past it the display bound divided by the
/// magnification is, and the grid grows denser. A request the node cap cannot meet, a region outside
/// the stage and an inadmissible magnification are refused.
#[test]
fn a_grid_follows_magnification_and_refuses_what_it_cannot_hold() {
    let (width, height) = (1024, 681);
    let map = map(
        width,
        height,
        vec![lens("Sigma 17-50 EX DC HSM at 17", width, height)],
    );
    let region = whole(width, height);
    let plain = CoordinateGrid::new(&map, region, 1.0).unwrap();
    let doubled = CoordinateGrid::new(&map, region, 2.0).unwrap();
    assert_eq!(doubled, plain, "the output-pixel bound holds at 200%");
    let magnified = CoordinateGrid::new(&map, region, 8.0).unwrap();
    assert!(magnified.spacing < plain.spacing);
    let error = worst(&map, &magnified, region, 3);
    assert!(error <= GRID_TOLERANCE_PX / 8.0, "{error}");
    assert!(magnified.nodes.len() <= GRID_MAX_NODES);
    let refused = CoordinateGrid::new(&map, region, 64.0).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::ResourceLimit);
    assert!(
        CoordinateGrid::new(
            &map,
            Region {
                x0: 1000,
                y0: 0,
                width: 100,
                height: 10
            },
            1.0
        )
        .is_err()
    );
    assert!(CoordinateGrid::new(&map, region, 0.0).is_err());
    assert!(CoordinateGrid::new(&map, region, f64::NAN).is_err());
}

/// Every window of a stage takes its part of the stage's one grid: a tile's part, and a region's,
/// interpolate every pixel of theirs from the same nodes and the same fractions as the whole
/// stage's grid, bit for bit, so tiles carry no seam. A grid of a region of its own keeps its nodes
/// on the stage's lattice of its spacing too.
#[test]
fn every_window_takes_its_part_of_the_stages_grid() {
    let (width, height) = (1200, 800);
    let map = map(
        width,
        height,
        vec![lens("Sigma 17-50 EX DC HSM at 17", width, height)],
    );
    let stage = CoordinateGrid::stage(&map, 1.0).unwrap();
    assert_eq!(stage.origin, (0, 0));
    for region in [
        whole(width, height),
        Region {
            x0: 0,
            y0: 0,
            width: 512,
            height: 512,
        },
        Region {
            x0: 512,
            y0: 512,
            width: 512,
            height: 288,
        },
        Region {
            x0: 333,
            y0: 77,
            width: 401,
            height: 299,
        },
    ] {
        let part = stage.part(region).expect("a part");
        assert_eq!(part.spacing, stage.spacing);
        assert_eq!(
            (part.origin.0 % part.spacing, part.origin.1 % part.spacing),
            (0, 0)
        );
        for y in (region.y0..region.y1()).step_by(7) {
            for x in (region.x0..region.x1()).step_by(5) {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                assert_eq!(
                    part.sample(px, py).map(f32::to_bits),
                    stage.sample(px, py).map(f32::to_bits),
                    "{region:?} at ({x}, {y})"
                );
            }
        }
    }
    let own = CoordinateGrid::new(
        &map,
        Region {
            x0: 333,
            y0: 77,
            width: 401,
            height: 299,
        },
        1.0,
    )
    .unwrap();
    assert_eq!(
        (own.origin.0 % own.spacing, own.origin.1 % own.spacing),
        (0, 0),
        "on the stage's lattice"
    );
    assert!(stage.part(whole(width + 64, height)).is_none());
}
