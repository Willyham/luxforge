//! The minimap's overview from the picture at rest (`docs/design/minimap.md`, "Overview rendering
//! and lifetime"): a second reduction of the same final tiles, drawn by the photo surface's own
//! drawing on a headless device. It is the GPU's own full-resolution stage reduced as the reference
//! reduces a frame, within a code; it is the reference renderer's whole frame reduced within the
//! declared display tolerance of the stack's class; and it changes neither the picture at rest's
//! codes nor its counts. Over pointwise, geometry and spatial stacks, on the byte and linear paths,
//! chained, in staged sweeps and after light sweeps. An exhausted budget refuses the overview and
//! never the picture at rest, and every overview byte is released with it.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing.
use super::{
    gpu_rest_tests::{at_full_size, committed},
    gpu_window_tests::{HEIGHT, WIDTH, source},
};
use luxforge_core::{
    BASIC_EFFECT, BoundaryFormat, Cancel, Layer, OVERVIEW_BOUNDS, PERSPECTIVE_EFFECT, ProxyBounds,
    Recipe, RenderContext, RenderOptions, RestTiles, SnapshotId, overview_size, reduce_overview,
    render,
};
use luxforge_gpu::{GpuFallback, GpuRest, GpuSource, headless::HeadlessSurface};
use luxforge_reference::{
    preview_error::{self, Class, Rgb8},
    tolerance,
};
use serde_json::json;

/// The Fit bounds the picture at rest is planned at, and the tiles' side: several tiles over the
/// 360 × 240 stage, so the overview is accumulated across them.
const BOUNDS: ProxyBounds = ProxyBounds {
    width: 160,
    height: 120,
};
const SIDE: u32 = 96;

/// How a family's tiles are planned.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Planned {
    /// As the editor plans them: chained or in staged sweeps.
    AsPlanned,
    /// A light behind a spatial layer computed by light sweeps, the tiles chained.
    StageFree,
}

/// The families the overview is held to: colour alone, two geometry stacks, the staged families
/// and the stacks whose light the light sweeps compute.
fn families() -> Vec<(String, Recipe, Class, Planned)> {
    let basic = || Layer::new(BASIC_EFFECT, json!({"exposure": 0.4, "contrast": 20.0}));
    let mut families = vec![
        (
            "Basic".to_owned(),
            Recipe {
                layers: vec![basic()],
                ..Recipe::default()
            },
            Class::Pointwise,
            Planned::AsPlanned,
        ),
        (
            "Basic and a lens".to_owned(),
            Recipe {
                layers: vec![
                    basic(),
                    luxforge_core::qualification::lens_layer(-0.004, (WIDTH, HEIGHT)),
                ],
                ..Recipe::default()
            },
            Class::Pointwise,
            Planned::AsPlanned,
        ),
        (
            "Basic and a perspective".to_owned(),
            Recipe {
                layers: vec![
                    basic(),
                    Layer::new(
                        PERSPECTIVE_EFFECT,
                        json!({"horizontal": 25, "vertical": -15}),
                    ),
                ],
                ..Recipe::default()
            },
            Class::Pointwise,
            Planned::AsPlanned,
        ),
    ];
    for (name, recipe) in super::gpu_rest_tests::staged_families() {
        families.push((name.to_owned(), recipe, Class::Spatial, Planned::AsPlanned));
    }
    for (name, recipe, _) in super::gpu_light_tests::behind_spatial() {
        families.push((name.to_owned(), recipe, Class::Spatial, Planned::StageFree));
    }
    families
}

/// `codes` as RGBA bytes.
fn rgba(codes: &[[u8; 4]]) -> Vec<u8> {
    codes.iter().flatten().copied().collect()
}

/// `pixels`, `stride` bytes a pixel, as RGB bytes.
fn rgb(pixels: &[u8], stride: usize) -> Vec<u8> {
    pixels
        .chunks_exact(stride)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// `rest` drawn by `surface` over `gpu`, its overview storage all released after.
fn drawn(
    what: &str,
    surface: &mut HeadlessSurface,
    gpu: &GpuSource,
    rest: &GpuRest,
) -> luxforge_gpu::headless::RestDrawn {
    let picture = surface
        .rest(gpu, rest)
        .unwrap_or_else(|fallback| panic!("{what}: {fallback:?}"));
    assert_eq!(surface.overview_bytes(), 0, "{what}: the overview released");
    picture
}

/// The overview the surface drew of `picture`, which must have one.
fn overview_of(what: &str, picture: &luxforge_gpu::headless::RestDrawn) -> (Vec<u8>, (u32, u32)) {
    match &picture.overview {
        Some(Ok(drawn)) => (rgba(&drawn.codes), drawn.size),
        other => panic!("{what}: no overview: {other:?}"),
    }
}

/// The tiles `evaluation`'s picture at rest is drawn in, as `planned` says.
fn tiles_of(
    what: &str,
    evaluation: &luxforge_core::Evaluation,
    planned: Planned,
) -> Box<RestTiles> {
    match planned {
        Planned::AsPlanned => luxforge_core::qualification::rest_tiles(evaluation, BOUNDS, SIDE),
        Planned::StageFree => {
            luxforge_core::qualification::rest_tiles_stage_free(evaluation, BOUNDS, SIDE)
        }
    }
    .unwrap_or_else(|reason| panic!("{what}: {reason}"))
}

/// The overview the picture at rest reduces its tiles to is the stage the same tiles draw at full
/// resolution, reduced as the reference reduces a frame, within one code a channel; the reference
/// renderer's whole frame reduced by an independent area average of its linear light, within the
/// display limits of the stack's class; and the core's reduction of that frame, byte for byte. It
/// is the same whether the tiles are drawn in staged sweeps or chained, and whether the view
/// reduces them or not, and the picture at rest's codes and counts are those it draws with no
/// overview. On both paths, for colour, geometry and spatial stacks, staged, chained and after
/// light sweeps.
#[test]
fn gpu_rest_an_overview_is_the_whole_stage_reduced_within_the_display_tolerance() {
    let test = "gpu_rest_an_overview_is_the_whole_stage_reduced_within_the_display_tolerance";
    let Some((backend, name)) = super::gpu_tiles_tests::host_adapter(test) else {
        return;
    };
    assert!(super::gpu_plan::install_output_encoding());
    let window = crate::adapters::open(&backend, &name)
        .unwrap_or_else(|unopened| panic!("{test}: the surface's device: {unopened:?}"));
    eprintln!("{test}: adapter {name} ({backend})");
    let mut surface = HeadlessSurface::new(&window.device, &window.queue);
    let (mut version, mut staged, mut swept) = (0, 0, 0);
    for (index, format) in [BoundaryFormat::Half, BoundaryFormat::Float]
        .into_iter()
        .enumerate()
    {
        let photograph = source(format);
        let gpu = super::gpu_tiles_tests::gpu_source(index as u64 + 1, &photograph);
        for (family, recipe, class, planned) in families() {
            let what = format!("{format:?} {family}");
            let evaluation = committed(photograph.clone(), recipe.clone());
            let tiles = tiles_of(&what, &evaluation, planned);
            let stage = (tiles.output.width, tiles.output.height);
            let size = overview_size(stage, OVERVIEW_BOUNDS).expect("a stage");
            let with = Box::new((*tiles).clone()).with_overview(OVERVIEW_BOUNDS);
            assert_eq!(
                with.overview.as_ref().map(|overview| overview.view),
                Some(size),
                "{what}"
            );
            assert!(size.0 < stage.0, "{what}: the overview reduces the stage");

            // The picture at rest with no overview, and with one: the same codes and counts.
            version += 1;
            let plain = super::gpu_preview::rest_now(&gpu, &tiles, version).unwrap();
            assert!(plain.overview.is_none(), "{what}");
            let plain = drawn(&what, &mut surface, &gpu, &plain);
            assert!(plain.overview.is_none(), "{what}");
            version += 1;
            let rest = super::gpu_preview::rest_now(&gpu, &with, version).unwrap();
            assert_eq!(
                rest.overview.as_ref().map(|overview| overview.version),
                Some(version),
                "{what}: swept under the rest's version"
            );
            let picture = drawn(&what, &mut surface, &gpu, &rest);
            assert!(
                picture.codes == plain.codes && picture.counts == plain.counts,
                "{what}: the overview changes neither the picture nor its counts"
            );
            assert_eq!(picture.figures.sweeps, plain.figures.sweeps, "{what}");
            let (overview, drawn_size) = overview_of(&what, &picture);
            assert_eq!(drawn_size, size, "{what}");
            if picture.figures.sweeps > 0 {
                staged += 1;
            }
            if !with.light_sweeps.is_empty() {
                swept += 1;
            }

            // The same overview chained, and with no view reduction, as at a region view.
            let mut variants = vec![("no view reduction", None, rest.stages.clone())];
            if rest.stages.is_some() {
                variants.push(("chained", rest.reduction.clone(), None));
            }
            for (variant, reduction, stages) in variants {
                version += 1;
                let other = GpuRest {
                    version,
                    reduction,
                    stages,
                    overview: rest.overview.clone().map(|mut overview| {
                        overview.version = version;
                        overview
                    }),
                    ..rest.clone()
                };
                let other = drawn(&what, &mut surface, &gpu, &other);
                assert_eq!(other.counts, plain.counts, "{what}, {variant}: the counts");
                assert_eq!(
                    overview_of(&what, &other),
                    (overview.clone(), size),
                    "{what}, {variant}: the same overview"
                );
            }

            // The same tiles drawn at full resolution, reduced on the CPU as the reference reduces.
            version += 1;
            let full = GpuRest {
                version,
                reduction: Some(at_full_size(&tiles)),
                ..super::gpu_preview::rest_now(&gpu, &tiles, version).unwrap()
            };
            let full = rgba(&drawn(&what, &mut surface, &gpu, &full).codes);
            let own = reduce_overview(&full, stage, size, &Cancel::never())
                .unwrap_or_else(|error| panic!("{what}: {error:?}"));
            let (mut largest, mut differing) = (0u8, 0usize);
            for (gpu, cpu) in overview.chunks_exact(4).zip(own.rgba.chunks_exact(4)) {
                let most = (0..3).map(|c| gpu[c].abs_diff(cpu[c])).max().unwrap();
                largest = largest.max(most);
                differing += usize::from(most > 0);
            }
            assert!(
                largest <= 1,
                "{what}: the overview is {largest} codes from its own stage reduced"
            );

            // The reference renderer's whole frame, reduced by the core and independently.
            let frame = render(
                &super::gpu_tiles_tests::registry(),
                &photograph,
                &recipe,
                RenderOptions::exact(&Cancel::never()),
                &RenderContext::new(),
            )
            .and_then(|rendered| rendered.frame(SnapshotId::new()))
            .expect("the reference frame");
            assert_eq!((frame.width, frame.height), stage, "{what}: the stage");
            let independent = tolerance::reduce_srgb8(&frame.rgba, 4, stage, size)
                .unwrap_or_else(|error| panic!("{what}: {error}"));
            let core = reduce_overview(&frame.rgba, stage, size, &Cancel::never())
                .unwrap_or_else(|error| panic!("{what}: {error:?}"));
            // The core's reduction is the reference's view reduction byte for byte (the core's
            // tests); the independent reducer sums in another order, so a sum at a code's
            // threshold may round the other way.
            let core = rgb(&core.rgba, 4);
            let (mut apart, mut independently) = (0u8, 0usize);
            for (a, b) in core.chunks_exact(3).zip(independent.chunks_exact(3)) {
                let most = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap();
                apart = apart.max(most);
                independently += usize::from(most > 0);
            }
            assert!(
                apart <= 1,
                "{what}: the core's reduction of the reference frame is {apart} codes from the \
                 independent one"
            );
            let judged = |candidate: &[u8]| {
                preview_error::compare(
                    Rgb8::new(size.0, size.1, candidate).expect("a frame"),
                    Rgb8::new(size.0, size.1, &independent).expect("a frame"),
                    [0, 0, size.0, size.1],
                )
                .expect("the comparison")
            };
            let against = judged(&rgb(&overview, 4));
            let control = judged(&rgb(&own.rgba, 4));
            eprintln!(
                "{test}: {what}: {}x{} overview of {}x{}, {} sweeps, {} light sweeps; {differing} \
                 pixels a code from its own stage reduced; the reference reduced by the core \
                 {independently} pixels a code from the independent reduction; against that {} \
                 (its own stage reduced: {})",
                size.0,
                size.1,
                stage.0,
                stage.1,
                picture.figures.sweeps,
                with.light_sweeps.len(),
                super::gpu_qualification::figures(&against),
                super::gpu_qualification::figures(&control),
            );
            assert!(
                preview_error::verdict(&against, class).passed(),
                "{what}: against the reference {}",
                super::gpu_qualification::figures(&against)
            );
        }
    }
    assert!(staged > 0, "some families were drawn in staged sweeps");
    assert!(
        swept > 0,
        "some families' lights were computed by light sweeps"
    );
}

/// A budget the picture at rest fits and its overview does not never costs the picture: it draws
/// its own codes and counts, and the overview is refused, naming the budget, or — where the tiles'
/// refits happened to wait for fewer retirements than the least budget was found with — drawn as
/// it is unbounded. Colour alone is refused before a tile is drawn, deterministically; a staged
/// stack may instead let an overview in progress go for a tile the budget refuses. A budget both
/// fit draws both. No overview byte outlives the picture either way.
#[test]
fn gpu_rest_an_exhausted_budget_refuses_the_overview_and_not_the_picture() {
    let test = "gpu_rest_an_exhausted_budget_refuses_the_overview_and_not_the_picture";
    let Some((backend, name)) = super::gpu_tiles_tests::host_adapter(test) else {
        return;
    };
    assert!(super::gpu_plan::install_output_encoding());
    let window = crate::adapters::open(&backend, &name)
        .unwrap_or_else(|unopened| panic!("{test}: the surface's device: {unopened:?}"));
    eprintln!("{test}: adapter {name} ({backend})");
    let mut surface = HeadlessSurface::new(&window.device, &window.queue);
    let photograph = source(BoundaryFormat::Half);
    let gpu = super::gpu_tiles_tests::gpu_source(1, &photograph);
    for (family, recipe, _, planned) in families()
        .into_iter()
        .filter(|(family, ..)| ["Basic", "Detail after Presence"].contains(&family.as_str()))
    {
        let what = family;
        let evaluation = committed(photograph.clone(), recipe);
        let tiles = tiles_of(&what, &evaluation, planned);
        let with = Box::new((*tiles).clone()).with_overview(OVERVIEW_BOUNDS);
        let mut version = 0;
        let mut rest_of = |tiles: &RestTiles| {
            version += 1;
            super::gpu_preview::rest_now(&gpu, tiles, version).unwrap()
        };
        let plain = surface.rest(&gpu, &rest_of(&tiles)).expect("unbounded");
        // The least budget the picture at rest alone is drawn in, to a kibibyte.
        let fits = |surface: &mut HeadlessSurface, rest: &GpuRest, free: u64| {
            surface.leave_budget(Some(free));
            let drawn = surface.rest(&gpu, rest);
            surface.leave_budget(None);
            drawn
        };
        let (mut low, mut high) = (0u64, 512 << 10);
        while high - low > 1 {
            let middle = (low + high) / 2;
            match fits(&mut surface, &rest_of(&tiles), middle << 10) {
                Ok(_) => high = middle,
                Err(_) => low = middle,
            }
        }
        // A quarter of a mebibyte past it, for the retirements a tile's refit may wait on, which
        // vary from draw to draw; far less than the overview's own storage.
        let least = (high << 10) + (256 << 10);
        assert!(
            least < 512 << 20 && fits(&mut surface, &rest_of(&tiles), least).is_ok(),
            "{what}: the picture at rest fits {least} bytes"
        );
        // There the overview is refused, the picture not: its storage is refused before a tile is
        // drawn, or it yields to a tile the budget refuses later.
        let picture = fits(&mut surface, &rest_of(&with), least)
            .unwrap_or_else(|fallback| panic!("{what}: the picture refused: {fallback:?}"));
        let refused = match &picture.overview {
            Some(Err(fallback @ GpuFallback::BudgetExceeded { .. })) => Some(*fallback),
            Some(Ok(_)) if what != "Basic" => None,
            other => panic!("{what}: the overview at {least} bytes: {other:?}"),
        };
        assert!(
            picture.codes == plain.codes && picture.counts == plain.counts,
            "{what}: the picture at rest's own codes and counts"
        );
        assert_eq!(surface.overview_bytes(), 0, "{what}: refused, nothing held");
        // With the overview's whole budget past it, both are drawn.
        let both = fits(
            &mut surface,
            &rest_of(&with),
            least + luxforge_core::OVERVIEW_GPU_BYTES,
        )
        .unwrap_or_else(|fallback| panic!("{what}: {fallback:?}"));
        assert!(
            matches!(both.overview, Some(Ok(_))),
            "{what}: {:?}",
            both.overview
                .map(|overview| overview.map(|drawn| drawn.size))
        );
        assert!(
            both.codes == plain.codes && both.counts == plain.counts,
            "{what}"
        );
        assert_eq!(surface.overview_bytes(), 0, "{what}: released");
        if refused.is_none() {
            assert_eq!(
                picture.overview, both.overview,
                "{what}: drawn as unbounded"
            );
        }
        eprintln!(
            "{test}: {what}: the picture alone fits {} KiB; the overview there {} and drawn with \
             {} MiB more",
            least >> 10,
            refused.map_or("drawn".to_owned(), |fallback| format!(
                "refused: {fallback:?}"
            )),
            luxforge_core::OVERVIEW_GPU_BYTES >> 20
        );
    }
}
