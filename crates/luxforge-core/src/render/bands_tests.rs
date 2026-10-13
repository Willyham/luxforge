//! The reference renderer's banded mode against its whole-frame render: every output family —
//! colour, spatial, masked, geometry and repair stacks, alone and together — banded byte for byte
//! as the whole frame renders it, at the default budget and at budgets that cut every band into
//! pieces; its first pass's statistics against the whole stage's; its reduced view and linear
//! reads; and its limits and cancellation.
use super::{
    LinearSettings, RenderContext, SpatialMode, WhiteBalanceApproximation,
    bands::{BAND_WINDOW_BYTES, Banded, MAX_BAND_ROWS, WholeSource, Windows},
    limits::MAX_FRAME_BYTES,
    repair_tests::{Op, repair_layer, repair_registry},
    spatial::{Tiling, build_reduction_banded, build_reduction_cancellable},
    testing::{linear_evaluation, render_linear},
    tests::{fitted_crop, turn, varied},
};
use crate::{
    Cancel, ComponentMode, ErrorKind, Layer, LinearImage, Mask, ModuleRegistry, ProxyBounds,
    Recipe, SnapshotId, Transform,
    modules::{Region, RepairKind, Stage, estimate_reduction},
};
use serde_json::json;
use std::sync::OnceLock;

const WIDTH: u32 = 150;
const HEIGHT: u32 = 100;

/// A textured linear source: a smooth gradient under a fine pattern, with values below zero and
/// above one, so every spatial operation has edges and every colour unit a range to act on.
fn textured(width: u32, height: u32) -> LinearImage {
    let pattern = varied(width, height);
    let reader = pattern.reader();
    let pixels: Vec<[f32; 3]> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let fine = reader.pixel(x, y).unwrap();
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            [
                0.6 * u + 0.25 * fine[0],
                0.5 * v + 0.2 * fine[1],
                0.4 * (u + v) + 0.15 * fine[2],
            ]
        })
        .collect();
    super::tests::image(width, height, &pixels)
}

/// The linear mask, the same mask (and id) every call.
fn linear_mask() -> Mask {
    static MASK: OnceLock<Mask> = OnceLock::new();
    MASK.get_or_init(|| {
        let mut mask = Mask::new("Mask 1");
        mask.components.push(crate::Component::new(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            json!({"x0": 0.2, "y0": 0.1, "x1": 0.8, "y1": 0.9}),
        ));
        mask
    })
    .clone()
}

/// The luminance-range mask, the same mask (and id) every call.
fn range_mask() -> Mask {
    static MASK: OnceLock<Mask> = OnceLock::new();
    MASK.get_or_init(|| {
        let mut mask = Mask::new("Mask 2");
        mask.components.push(crate::Component::new(
            "Luminance range 1",
            ComponentMode::Add,
            "luminance-range",
            json!({"low": 20.0, "low_feather": 10.0, "high": 70.0, "high_feather": 15.0}),
        ));
        mask
    })
    .clone()
}

/// One layer of a stack, a geometry layer built against the stage it receives.
#[derive(Clone, Copy, Debug)]
enum Step {
    Basic,
    Curve,
    Detail,
    Clarity,
    Dehaze,
    Texture,
    /// Clarity, Texture and Dehaze in one Presence layer.
    Presence,
    Vignette,
    Turn,
    Crop,
    Straighten,
    Perspective,
    Lens,
    Repair,
    /// Basic under the linear mask.
    MaskedBasic,
    /// Detail under the linear mask.
    MaskedDetail,
    /// Texture under the luminance-range mask.
    RangeTexture,
}

impl Step {
    fn layer(self, stage: Stage) -> Layer {
        let masked = |layer: Layer, mask: Mask| Layer {
            mask: Some(mask.id),
            ..layer
        };
        let crop = |angle: f64| {
            Layer::crop(fitted_crop(
                stage.width,
                stage.height,
                angle,
                [0.1, 0.12, 0.8, 0.75],
            ))
        };
        match self {
            Self::Basic => Layer::new(
                crate::BASIC_EFFECT,
                json!({"exposure": 0.4, "contrast": 20.0, "highlights": -30.0, "shadows": 25.0}),
            ),
            Self::Curve => Layer::new(
                crate::CURVE_EFFECT,
                json!({"luminance": [[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]]}),
            ),
            Self::Detail => Layer::new(
                crate::DETAIL_EFFECT,
                json!({"sharpening": 40.0, "luminance": 20.0}),
            ),
            Self::Clarity => Layer::new(crate::PRESENCE_EFFECT, json!({"clarity": 30.0})),
            Self::Dehaze => Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": 40.0})),
            Self::Texture => Layer::new(crate::PRESENCE_EFFECT, json!({"texture": 25.0})),
            Self::Presence => Layer::new(
                crate::PRESENCE_EFFECT,
                json!({"clarity": 30.0, "texture": 15.0, "dehaze": 40.0}),
            ),
            Self::Vignette => Layer::new(crate::VIGNETTE_EFFECT, json!({"amount": -40.0})),
            Self::Turn => turn(Transform::RotateRight),
            Self::Crop => crop(0.0),
            Self::Straighten => crop(3.0),
            Self::Perspective => Layer::new(
                crate::PERSPECTIVE_EFFECT,
                json!({"horizontal": 40, "vertical": -30}),
            ),
            Self::Lens => super::testing::frozen_lens(stage.width, stage.height, 24.0),
            Self::Repair => repair_layer(&[
                Op::new(
                    1,
                    RepairKind::Clone,
                    stage,
                    &[[0.3 * f64::from(stage.width), 0.4 * f64::from(stage.height)]],
                    9.0,
                    [0.35 * f64::from(stage.width), 0.2 * f64::from(stage.height)],
                ),
                Op::new(
                    2,
                    RepairKind::Heal,
                    stage,
                    &[
                        [0.6 * f64::from(stage.width), 0.7 * f64::from(stage.height)],
                        [0.8 * f64::from(stage.width), 0.6 * f64::from(stage.height)],
                    ],
                    7.0,
                    [
                        -0.4 * f64::from(stage.width),
                        -0.5 * f64::from(stage.height),
                    ],
                ),
            ]),
            Self::MaskedBasic => masked(
                Layer::new(crate::BASIC_EFFECT, json!({"exposure": -0.3})),
                linear_mask(),
            ),
            Self::MaskedDetail => masked(
                Layer::new(crate::DETAIL_EFFECT, json!({"sharpening": 25.0})),
                linear_mask(),
            ),
            Self::RangeTexture => masked(
                Layer::new(crate::PRESENCE_EFFECT, json!({"texture": 40.0})),
                range_mask(),
            ),
        }
    }
}

/// `steps` added in turn, each where the host places a new layer of its effect and target.
fn stack(registry: &ModuleRegistry, width: u32, height: u32, steps: &[Step]) -> Recipe {
    let mut recipe = Recipe {
        format: crate::RECIPE_FORMAT,
        masks: vec![linear_mask(), range_mask()],
        ..Recipe::default()
    };
    for step in steps {
        let probe = step.layer(Stage { width, height });
        let at = registry.insertion_index_for_target(
            &recipe.layers,
            &probe.effect_id,
            probe.mask.as_ref(),
            &recipe.masks,
        );
        let before = Recipe {
            layers: recipe.layers[..at].to_vec(),
            ..recipe.clone()
        };
        let stage = registry.compile(width, height, &before).unwrap().stage();
        recipe.layers.insert(at, step.layer(stage));
    }
    recipe
}

/// How a banded output was produced.
#[derive(Debug)]
struct Cut {
    bands: usize,
    pieces: usize,
}

/// The banded output at `budget`, every band concatenated, with how many bands and pieces it was
/// evaluated in.
fn banded_frame(
    registry: &ModuleRegistry,
    image: &LinearImage,
    recipe: &Recipe,
    settings: LinearSettings,
    budget: u64,
) -> (Vec<u8>, Cut) {
    let compiled = registry
        .compile(image.width(), image.height(), recipe)
        .unwrap();
    let context = RenderContext::new();
    let source = WholeSource { image, settings };
    let banded =
        Banded::with_budget(&source, &compiled, &context, &Cancel::never(), budget).unwrap();
    let stage = banded.stage();
    assert!((1..=MAX_BAND_ROWS).contains(&banded.band_rows()));
    let last = compiled.segments.len() - 1;
    let mut rgba = Vec::new();
    let mut cut = Cut {
        bands: 0,
        pieces: 0,
    };
    for band in banded.bands() {
        let (height, bytes) = band.unwrap();
        assert_eq!(bytes.len(), (stage.width * height * 4) as usize);
        assert!(bytes.len() as u64 <= MAX_FRAME_BYTES);
        let rect = Region {
            x0: 0,
            y0: (rgba.len() / (stage.width as usize * 4)) as u32,
            width: stage.width,
            height,
        };
        cut.pieces += banded.pieces(last, rect, 4).unwrap().len();
        cut.bands += 1;
        rgba.extend(bytes);
    }
    assert_eq!(rgba.len(), (Region::whole(stage).pixels() * 4) as usize);
    (rgba, cut)
}

/// The bytes the windows of producing `rect` of the output hold, with its output.
fn window_bytes(compiled: &crate::render::Compiled, source: Stage, rect: Region) -> u64 {
    Windows::of(compiled, source, compiled.segments.len() - 1, rect)
        .unwrap()
        .bytes()
        + rect.pixels() * 4
}

/// Two budgets that cut a stack's output: one whose bands are about a third of the stage's
/// height, and one in which not even a full-width row fits, so every band is cut into columns.
fn cutting_budgets(registry: &ModuleRegistry, image: &LinearImage, recipe: &Recipe) -> [u64; 2] {
    let compiled = registry
        .compile(image.width(), image.height(), recipe)
        .unwrap();
    let source = Stage {
        width: image.width(),
        height: image.height(),
    };
    let stage = compiled.stage();
    let whole = window_bytes(&compiled, source, Region::whole(stage));
    let columns = window_bytes(
        &compiled,
        source,
        Region {
            x0: stage.width / 3,
            y0: stage.height / 2,
            width: stage.width / 3,
            height: 1,
        },
    );
    [whole / 3, columns]
}

/// The budget whose bands are about a third of the stage's height.
fn cutting_budget(registry: &ModuleRegistry, image: &LinearImage, recipe: &Recipe) -> u64 {
    cutting_budgets(registry, image, recipe)[0]
}

/// Every stack in `stacks` banded at the default budget, in bands and in columns is the whole
/// render's frame byte for byte, under `settings`.
fn holds(registry: &ModuleRegistry, stacks: &[&[Step]], settings: LinearSettings) {
    let image = textured(WIDTH, HEIGHT);
    for steps in stacks {
        let recipe = stack(registry, WIDTH, HEIGHT, steps);
        let whole = render_linear(registry, &image, SnapshotId::new(), &recipe, settings).unwrap();
        let (one, cut) = banded_frame(registry, &image, &recipe, settings, BAND_WINDOW_BYTES);
        assert_eq!(cut.bands, 1, "{steps:?}");
        assert!(
            one == whole.rgba[..],
            "{steps:?}: one band differs from the whole frame"
        );
        let [rows, columns] = cutting_budgets(registry, &image, &recipe);
        let (banded, cut) = banded_frame(registry, &image, &recipe, settings, rows);
        assert!(cut.bands > 1, "{steps:?}: {cut:?}");
        assert!(
            banded == whole.rgba[..],
            "{steps:?}: {cut:?} differ from the whole frame"
        );
        let (pieced, cut) = banded_frame(registry, &image, &recipe, settings, columns);
        assert!(cut.pieces > cut.bands, "{steps:?}: {cut:?}");
        assert!(
            pieced == whole.rgba[..],
            "{steps:?}: {cut:?} differ from the whole frame"
        );
    }
}

fn balanced() -> LinearSettings {
    LinearSettings {
        white_balance: Some(
            WhiteBalanceApproximation::from_matrix([
                [1.3, 0.1, -0.05],
                [0.02, 0.97, 0.01],
                [-0.1, 0.05, 0.62],
            ])
            .unwrap(),
        ),
    }
}

#[test]
fn reference_bands_colour_stacks_are_the_whole_frame() {
    let registry = ModuleRegistry::developer();
    let stacks: &[&[Step]] = &[&[], &[Step::Basic, Step::Curve]];
    holds(&registry, stacks, LinearSettings::default());
    holds(&registry, stacks, balanced());
}

#[test]
#[luxforge_test_macros::slow]
fn reference_bands_spatial_stacks_are_the_whole_frame() {
    let registry = ModuleRegistry::developer();
    holds(
        &registry,
        &[
            &[Step::Detail],
            &[Step::Clarity],
            &[Step::Texture],
            &[Step::Dehaze],
            &[Step::Basic, Step::Detail, Step::Presence],
        ],
        LinearSettings::default(),
    );
    holds(&registry, &[&[Step::Dehaze, Step::Detail]], balanced());
}

#[test]
#[luxforge_test_macros::slow]
fn reference_bands_masked_stacks_are_the_whole_frame() {
    let registry = ModuleRegistry::developer();
    holds(
        &registry,
        &[
            &[Step::MaskedBasic],
            &[Step::MaskedDetail, Step::Basic],
            &[Step::RangeTexture],
            &[Step::Dehaze, Step::RangeTexture, Step::MaskedBasic],
        ],
        LinearSettings::default(),
    );
}

#[test]
#[luxforge_test_macros::slow]
fn reference_bands_geometry_stacks_are_the_whole_frame() {
    let registry = ModuleRegistry::developer();
    holds(
        &registry,
        &[
            &[Step::Turn],
            &[Step::Crop, Step::Basic],
            &[Step::Straighten],
            &[Step::Perspective],
            &[Step::Lens],
            &[Step::Vignette, Step::Crop],
            &[Step::Turn, Step::Detail, Step::Straighten],
            &[Step::Lens, Step::Clarity, Step::Crop, Step::Vignette],
            &[Step::Perspective, Step::Dehaze, Step::MaskedDetail],
        ],
        LinearSettings::default(),
    );
}

#[test]
#[luxforge_test_macros::slow]
fn reference_bands_repair_stacks_are_the_whole_frame() {
    let registry = repair_registry();
    holds(
        &registry,
        &[
            &[Step::Repair],
            &[Step::Repair, Step::Basic],
            &[Step::Detail, Step::Repair],
            &[Step::Turn, Step::Repair, Step::Crop],
            &[
                Step::Lens,
                Step::Repair,
                Step::Dehaze,
                Step::MaskedDetail,
                Step::RangeTexture,
                Step::Straighten,
                Step::Vignette,
            ],
        ],
        LinearSettings::default(),
    );
}

/// An odd rectangle's bytes and linear values are the whole frame's there, and the reduced view is
/// the whole frame reduced.
#[test]
#[luxforge_test_macros::slow]
fn reference_bands_rectangles_and_reduced_views_are_the_whole_frame() {
    let registry = ModuleRegistry::developer();
    let image = textured(WIDTH, HEIGHT);
    let recipe = stack(
        &registry,
        WIDTH,
        HEIGHT,
        &[Step::Dehaze, Step::Detail, Step::Straighten, Step::Basic],
    );
    let settings = LinearSettings::default();
    let whole = render_linear(&registry, &image, SnapshotId::new(), &recipe, settings).unwrap();
    let compiled = registry.compile(WIDTH, HEIGHT, &recipe).unwrap();
    let context = RenderContext::new();
    let source = WholeSource {
        image: &image,
        settings,
    };
    let budget = cutting_budget(&registry, &image, &recipe);
    let banded =
        Banded::with_budget(&source, &compiled, &context, &Cancel::never(), budget).unwrap();
    let stage = banded.stage();
    let rect = Region {
        x0: 13,
        y0: 7,
        width: stage.width - 30,
        height: stage.height - 18,
    };
    let rgba = banded.rgba(rect).unwrap();
    for row in 0..rect.height {
        let from = (((rect.y0 + row) * stage.width + rect.x0) * 4) as usize;
        let to = (row * rect.width * 4) as usize;
        let width = (rect.width * 4) as usize;
        assert_eq!(rgba[to..to + width], whole.rgba[from..from + width]);
    }

    let evaluation = linear_evaluation(
        &context,
        &registry,
        &image,
        &recipe,
        settings,
        Tiling::Halo,
        SpatialMode::Frames,
    )
    .unwrap();
    let mut pixels = Vec::new();
    evaluation
        .region_in(
            compiled.segments.len() - 1,
            rect,
            &mut pixels,
            &mut Default::default(),
        )
        .unwrap();
    let expected: Vec<[f32; 3]> = pixels
        .iter()
        .map(|pixel| pixel.map(|channel| channel as f32))
        .collect();
    assert!(banded.linear(rect).unwrap() == expected);

    let bounds = ProxyBounds {
        width: 61,
        height: 45,
    };
    let reduced = banded
        .reduced(bounds, whole.snapshot_id.clone(), "")
        .unwrap()
        .unwrap();
    let expected = super::reduce_to_view(&whole, bounds, &Cancel::never())
        .unwrap()
        .unwrap();
    assert_eq!(
        (reduced.width, reduced.height),
        (expected.width, expected.height)
    );
    assert!(reduced.rgba == expected.rgba);
    let fits = ProxyBounds {
        width: stage.width,
        height: stage.height,
    };
    assert!(
        banded
            .reduced(fits, whole.snapshot_id, "")
            .unwrap()
            .is_none()
    );
}

/// The Dehaze light's reduction built band by band is the whole stage's, at every band height, and
/// its factor grows on a large stage so its grid stays within the bound.
#[test]
fn reference_bands_reductions_are_the_whole_stages() {
    let image = textured(97, 83);
    let stage = Stage {
        width: 97,
        height: 83,
    };
    let reader = image.reader();
    let fill = |area: Region, planes: &mut [f32]| {
        let plane = area.pixels() as usize;
        for (row, y) in (area.y0..area.y1()).enumerate() {
            for (column, x) in (area.x0..area.x1()).enumerate() {
                let at = row * area.width as usize + column;
                let pixel = reader.pixel(x, y).unwrap();
                for channel in 0..3 {
                    planes[channel * plane + at] = pixel[channel];
                }
            }
        }
        Ok(())
    };
    let whole = build_reduction_cancellable(stage, &Cancel::never(), fill).unwrap();
    for rows in [1, 16, 17, 40, 83, 500] {
        let banded = build_reduction_banded(stage, rows, &Cancel::never(), |band, planes| {
            planes.clear();
            planes.resize(band.pixels() as usize * 3, 0.0);
            fill(band, planes)
        })
        .unwrap();
        assert!(banded == whole, "{rows} rows");
    }

    let grid = |stage: Stage| {
        let factor = estimate_reduction(stage);
        (
            factor,
            u64::from(stage.width.div_ceil(factor)) * u64::from(stage.height.div_ceil(factor)),
        )
    };
    assert_eq!(grid(stage).0, 16);
    assert_eq!(
        grid(Stage {
            width: 8192,
            height: 8192
        })
        .0,
        16
    );
    for stage in [
        Stage {
            width: 40_000,
            height: 13_000,
        },
        Stage {
            width: 65_535,
            height: 8_000,
        },
    ] {
        let (factor, pixels) = grid(stage);
        assert!(factor > 16 && factor % 16 == 0, "{stage:?}: {factor}");
        assert!(pixels <= crate::modules::MAX_REDUCTION_PIXELS, "{stage:?}");
        let (_, finer) = grid_at(stage, factor - 16);
        assert!(finer > crate::modules::MAX_REDUCTION_PIXELS, "{stage:?}");
    }

    fn grid_at(stage: Stage, factor: u32) -> (u32, u64) {
        (
            factor,
            u64::from(stage.width.div_ceil(factor)) * u64::from(stage.height.div_ceil(factor)),
        )
    }
}

/// Every piece's windows hold at most the budget, a band is at most the evaluated-frame limit and
/// a rectangle past it is refused by name.
#[test]
fn reference_bands_hold_their_limits() {
    let registry = ModuleRegistry::developer();
    let image = textured(WIDTH, HEIGHT);
    let recipe = stack(&registry, WIDTH, HEIGHT, &[Step::Detail, Step::Straighten]);
    let compiled = registry.compile(WIDTH, HEIGHT, &recipe).unwrap();
    let context = RenderContext::new();
    let source = WholeSource {
        image: &image,
        settings: LinearSettings::default(),
    };
    let budget = cutting_budget(&registry, &image, &recipe);
    let banded =
        Banded::with_budget(&source, &compiled, &context, &Cancel::never(), budget).unwrap();
    let last = compiled.segments.len() - 1;
    let stage = banded.stage();
    let pieces = banded.pieces(last, Region::whole(stage), 4).unwrap();
    assert!(pieces.len() > 1);
    let mut covered = 0;
    for piece in &pieces {
        let windows = Windows::of(
            &compiled,
            Stage {
                width: WIDTH,
                height: HEIGHT,
            },
            last,
            *piece,
        )
        .unwrap();
        assert!(windows.bytes() + piece.pixels() * 4 <= budget, "{piece:?}");
        covered += piece.pixels();
    }
    assert_eq!(covered, Region::whole(stage).pixels());
    assert!(banded.band_rows() <= stage.height);

    let wide = Region {
        x0: 0,
        y0: 0,
        width: 65_535,
        height: (MAX_FRAME_BYTES / (4 * 65_535)) as u32 + 1,
    };
    let refused = banded.rgba(wide).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::ResourceLimit);
    assert!(refused.to_string().contains("evaluated-frame limit"));
    assert_eq!(
        banded
            .rgba(Region {
                x0: stage.width - 1,
                y0: 0,
                width: 2,
                height: 1,
            })
            .unwrap_err()
            .kind,
        ErrorKind::Internal
    );
}

/// A cancelled run stops in the first pass and between bands, and names the cancellation.
#[test]
fn reference_bands_stop_when_cancelled() {
    let registry = ModuleRegistry::developer();
    let image = textured(WIDTH, HEIGHT);
    let recipe = stack(&registry, WIDTH, HEIGHT, &[Step::Dehaze, Step::Basic]);
    let compiled = registry.compile(WIDTH, HEIGHT, &recipe).unwrap();
    let context = RenderContext::new();
    let source = WholeSource {
        image: &image,
        settings: LinearSettings::default(),
    };
    let cancel = Cancel::new();
    cancel.cancel();
    let Err(error) = Banded::new(&source, &compiled, &context, &cancel) else {
        panic!("a cancelled first pass completed");
    };
    assert_eq!(error.kind, ErrorKind::Cancelled);

    let cancel = Cancel::new();
    let budget = cutting_budget(&registry, &image, &recipe);
    let banded = Banded::with_budget(&source, &compiled, &context, &cancel, budget).unwrap();
    let mut bands = banded.bands();
    assert!(bands.next().unwrap().is_ok());
    cancel.cancel();
    let error = bands.next().unwrap().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert!(bands.next().is_none(), "the bands end at their first error");
}
