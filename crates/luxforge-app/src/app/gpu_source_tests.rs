//! The prepared source the photo surface holds on the GPU, held to the CPU (`docs/design/
//! gpu-preview.md`, "The GPU source"): a boundary the surface derives from it is the boundary the
//! CPU renders.
//!
//! - **Cut.** A window of the source at full scale, through the view's orientation, is bit for bit
//!   the CPU's boundary at the source (`Render::boundary`): a JPEG's codes decoded through the
//!   CPU's own table and held as the nearest half float, and a RAW's planes, viewed from a crop
//!   window under each of the eight orientations, held as the `f32` they are.
//! - **Reduce.** The source's area average at a proxy plan, a crop's window of it included, is the
//!   CPU's proxy within a code on a JPEG, quantized and decoded again as the CPU's is, and within
//!   the `f32` sum's rounding of the CPU's `f64` one on a RAW.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing.
use super::gpu_qualification::headless;
use luxforge_core::{
    BoundaryFormat, Cancel, CropStage, Layer, LinearImage, LinearSettings, ModuleRegistry,
    PreviewSource, ProxyBounds, ProxyCoverage, ProxyPlan, Recipe, RenderContext, RenderOptions,
    RenderSource, SourceImage, qualification, render,
};
use luxforge_ui::photo_surface::{
    AxisCoverage, Derivation, GpuBoundary, GpuSource, Reduction,
    gpu_preview::qualification as qualification_ui,
};
use std::sync::Arc;

/// Codes that vary along both axes in every channel, so a texel read from the wrong place, or
/// decoded through another table, is told apart.
fn jpeg(width: u32, height: u32) -> SourceImage {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend([
                ((x * 7 + y * 3) % 256) as u8,
                ((x * 2 + y * 11 + 5) % 256) as u8,
                ((x * 13) ^ (y * 5)) as u8,
                255,
            ]);
        }
    }
    SourceImage {
        width,
        height,
        rgba: Arc::new(rgba),
        fingerprint: "sha256:gpu-source-fixture".into(),
        orientation: 1,
        capture: Default::default(),
    }
}

/// Planes of `base` values each, red then green then blue, some below black and past white, and
/// near-black values a half float would round.
fn planes((width, height): (u32, u32)) -> Vec<f32> {
    let plane = (width * height) as usize;
    let mut values = vec![0f32; 3 * plane];
    for channel in 0..3 {
        for y in 0..height {
            for x in 0..width {
                let (fx, fy) = (x as f32, y as f32);
                values[channel * plane + (y * width + x) as usize] =
                    (fx * 0.37 + fy * 0.11 + channel as f32).sin() * 0.7
                        + 0.3
                        + 1.0e-5 * (x % 7) as f32
                        - 0.2 * channel as f32;
            }
        }
    }
    values
}

/// A straightened crop of a `width` × `height` stage, so a proxy of it holds a window.
fn crop(width: u32, height: u32) -> Layer {
    let stage = CropStage {
        width,
        height,
        angle: 5.0,
    };
    let (box_width, box_height) = stage.bounding_box();
    let fitted = stage.fit_about_center(luxforge_core::BoxRect {
        x: 0.2 * box_width,
        y: 0.15 * box_height,
        width: 0.5 * box_width,
        height: 0.6 * box_height,
    });
    Layer::crop(fitted.normalized(&stage))
}

/// The windows of a `stage` a cut is checked over: the whole stage, and a part of it away from
/// every edge.
fn windows((width, height): (u32, u32)) -> [[u32; 4]; 2] {
    [
        [0, 0, width, height],
        [width / 5, height / 4, width / 2, height / 3],
    ]
}

/// A cut of the source the surface holds is bit for bit the CPU's boundary at the source: a JPEG's
/// over the whole stage and a window of it, and a RAW's from a crop window of its planes under each
/// of the eight orientations, over the whole viewed stage and a window of it.
#[test]
fn a_cut_boundary_is_the_cpus_boundary_bit_for_bit() {
    let test = "a_cut_boundary_is_the_cpus_boundary_bit_for_bit";
    let Some(qualifier) = headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let context = RenderContext::new();
    let cancel = Cancel::never();
    let image = jpeg(97, 61);
    let source =
        GpuSource::codes(1, Arc::clone(&image.rgba), image.width, image.height).expect("a source");
    let rendered = render(
        &registry,
        RenderSource::Byte(&image),
        &Recipe::default(),
        RenderOptions::exact(&cancel),
        &context,
    )
    .expect("the stack compiles");
    for window in windows((image.width, image.height)) {
        let cpu = qualification::source_boundary(&rendered, window, BoundaryFormat::Half)
            .expect("the CPU's boundary");
        let boundary = GpuBoundary::derived(
            &source,
            Derivation::Cut {
                origin: (window[0], window[1]),
            },
            window[2],
            window[3],
            1,
        )
        .expect("a derived boundary");
        let gpu = qualifier.derive(&source, &boundary).expect("the GPU's");
        assert_eq!(gpu.len(), cpu.texels.len(), "a JPEG over {window:?}");
        assert!(gpu == *cpu.texels, "a JPEG over {window:?}: bit for bit");
    }
    let base = (53u32, 37u32);
    let crop = [4u32, 3, 41, 29];
    let values = Arc::new(planes(base));
    let developed =
        LinearImage::new(base.0, base.1, values.as_ref().clone()).expect("finite planes");
    for orientation in 1..=8u8 {
        let viewed = qualification::viewed(&developed, crop, orientation).expect("a view");
        let rendered = render(
            &registry,
            RenderSource::Linear {
                image: &viewed,
                settings: LinearSettings::default(),
            },
            &Recipe::default(),
            RenderOptions::exact(&cancel),
            &context,
        )
        .expect("the stack compiles");
        let source = GpuSource::planes(1, Arc::clone(&values), base, crop, orientation)
            .expect("a viewed source");
        assert_eq!(source.stage(), (viewed.width(), viewed.height()));
        for window in windows(source.stage()) {
            let cpu = qualification::source_boundary(&rendered, window, BoundaryFormat::Float)
                .expect("the CPU's boundary");
            let boundary = GpuBoundary::derived(
                &source,
                Derivation::Cut {
                    origin: (window[0], window[1]),
                },
                window[2],
                window[3],
                1,
            )
            .expect("a derived boundary");
            let gpu = qualifier.derive(&source, &boundary).expect("the GPU's");
            assert!(
                gpu == *cpu.texels,
                "a RAW under orientation {orientation} over {window:?}: bit for bit"
            );
        }
    }
}

/// The surface's coverage of one axis, from the core's.
fn axis(coverage: &ProxyCoverage) -> AxisCoverage {
    AxisCoverage {
        first: coverage.first.clone(),
        offsets: coverage.offsets.clone(),
        weights: coverage
            .weights
            .iter()
            .map(|weight| *weight as f32)
            .collect(),
    }
}

/// The reduction of a `source`-sized source at `plan`, as the desktop hands the surface the
/// core's coverage.
fn reduction(plan: ProxyPlan, source: (u32, u32)) -> (Derivation, (u32, u32)) {
    let [across, down] = plan.coverage(source).expect("the plan fits the source");
    let [x, y, width, height] = plan.held();
    (
        Derivation::Reduce(Arc::new(Reduction {
            origin: (x, y),
            across: axis(&across),
            down: axis(&down),
        })),
        (width, height),
    )
}

/// The proxy plan a job's worker builds for `recipe` over `source` at `bounds`, with the window of
/// the proxy stage it holds.
fn plan_of(source: RenderSource<'_>, recipe: &Recipe, bounds: ProxyBounds) -> ProxyPlan {
    let registry = ModuleRegistry::builtin();
    let context = RenderContext::new();
    let rendered = render(
        &registry,
        source,
        recipe,
        RenderOptions::exact(&Cancel::never()),
        &context,
    )
    .expect("the stack compiles");
    qualification::fit_proxy(&rendered, &registry, recipe, bounds)
        .expect("a proxy at the bounds")
        .0
}

/// A reduction of the source the surface holds is the CPU's proxy: on a JPEG each texel within a
/// code of the proxy's own, its average quantized and decoded again as the CPU's is, over the whole
/// proxy stage and over the window of it a straightened crop reads; on a RAW, under an orientation,
/// within the rounding of an `f32` sum against the CPU's `f64` one.
#[test]
fn a_reduced_boundary_is_the_cpu_proxy_within_a_code() {
    let test = "a_reduced_boundary_is_the_cpu_proxy_within_a_code";
    let Some(qualifier) = headless(test) else {
        return;
    };
    let image = jpeg(311, 197);
    let source =
        GpuSource::codes(1, Arc::clone(&image.rgba), image.width, image.height).expect("a source");
    let bounds = ProxyBounds {
        width: 120,
        height: 90,
    };
    // A half float of each code's linear value, as the boundary holds it, back to its code.
    let table = luxforge_core::colour::srgb::decode_table();
    let code_of = |value: f32| {
        (0..=255u8).find(|code| {
            qualification_ui::held(table[usize::from(*code)]).to_bits() == value.to_bits()
        })
    };
    for recipe in [
        Recipe::default(),
        Recipe {
            layers: vec![crop(image.width, image.height)],
            ..Recipe::default()
        },
    ] {
        let plan = plan_of(RenderSource::Byte(&image), &recipe, bounds);
        let (derivation, size) = reduction(plan, (image.width, image.height));
        let windowed = plan.held() != [0, 0, plan.width, plan.height];
        assert_eq!(windowed, !recipe.layers.is_empty(), "a crop's window");
        let PreviewSource::Jpeg(proxy) = PreviewSource::Jpeg(image.clone())
            .proxy(plan)
            .expect("the CPU's proxy")
        else {
            panic!("a JPEG proxy");
        };
        assert_eq!((proxy.width, proxy.height), size);
        let boundary = GpuBoundary::derived(&source, derivation, size.0, size.1, 1)
            .expect("a derived boundary");
        let gpu = qualifier.derive(&source, &boundary).expect("the GPU's");
        let (mut exact, mut texels) = (0, 0);
        for (index, (texel, pixel)) in gpu
            .chunks_exact(8)
            .zip(proxy.rgba.chunks_exact(4))
            .enumerate()
        {
            for channel in 0..3 {
                let value =
                    qualification_ui::half_value([texel[2 * channel], texel[2 * channel + 1]]);
                let code = code_of(value).unwrap_or_else(|| {
                    panic!("texel {index}, channel {channel}: {value} is no code's value")
                });
                assert!(
                    code.abs_diff(pixel[channel]) <= 1,
                    "texel {index}, channel {channel}: {code} against the CPU proxy's {}",
                    pixel[channel]
                );
                exact += usize::from(code == pixel[channel]);
                texels += 1;
            }
            assert_eq!(
                qualification_ui::half_value([texel[6], texel[7]]),
                1.0,
                "opaque"
            );
        }
        eprintln!("{test}: windowed {windowed}, {exact} of {texels} channels the proxy's code");
    }
    // A RAW, viewed from a crop window under orientation 6.
    let base = (241u32, 173u32);
    let view = [5u32, 4, 231, 163];
    let values = Arc::new(planes(base));
    let developed =
        LinearImage::new(base.0, base.1, values.as_ref().clone()).expect("finite planes");
    let viewed = qualification::viewed(&developed, view, 6).expect("a view");
    let source = GpuSource::planes(1, Arc::clone(&values), base, view, 6).expect("a source");
    let settings = LinearSettings::default();
    let plan = plan_of(
        RenderSource::Linear {
            image: &viewed,
            settings,
        },
        &Recipe::default(),
        bounds,
    );
    let (derivation, size) = reduction(plan, source.stage());
    let PreviewSource::Raw { image: proxy, .. } = (PreviewSource::Raw {
        image: viewed.clone(),
        settings,
    })
    .proxy(plan)
    .expect("the CPU's proxy") else {
        panic!("a RAW proxy");
    };
    let boundary =
        GpuBoundary::derived(&source, derivation, size.0, size.1, 1).expect("a derived boundary");
    let gpu = qualifier.derive(&source, &boundary).expect("the GPU's");
    let mut largest = 0f32;
    for (index, texel) in gpu.chunks_exact(16).enumerate() {
        let (x, y) = (index as u32 % size.0, index as u32 / size.0);
        let cpu = proxy.pixel(x, y).expect("inside the proxy");
        for channel in 0..3 {
            let value = f32::from_le_bytes(texel[4 * channel..4 * channel + 4].try_into().unwrap());
            let difference = (value - cpu[channel]).abs();
            largest = largest.max(difference);
            assert!(
                difference <= 4.0e-6 * cpu[channel].abs().max(1.0),
                "texel ({x}, {y}), channel {channel}: {value} against the CPU proxy's {}",
                cpu[channel]
            );
        }
    }
    eprintln!("{test}: a RAW's largest difference from the CPU proxy {largest:e}");
}
