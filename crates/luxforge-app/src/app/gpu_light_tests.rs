//! The per-frame light on this host's device (`docs/design/gpu-preview.md`, "The global
//! estimate"): Dehaze's atmospheric light computed by a light link — the reduction of its whole
//! input stage at full resolution into the stage's 16-pixel block means, then the selection of the
//! light into the slot's light plane — through the photo surface's own code
//! (`luxforge_ui::photo_surface::gpu_preview::light::LightBench`), held to the CPU's.
//!
//! - **The CPU's preparation.** Over a synthetic photograph and the corpus's Presence fixture, on
//!   the byte and linear paths, a plain stack, an exposure, tone and curve prefix, a prefix masked
//!   by a radial and a luminance range, and three stops either way with Whites and Blacks at the
//!   same end: the light against the CPU's preparation of the same stage, within the existing
//!   atmospheric light test's 2e-5.
//! - **Tiles.** A stage past the device's largest texture, held in two source textures each way,
//!   reduced in four tiles into one block plane: the light and the block plane are the one-texture
//!   ones, bit for bit, and the light twice is the same bits.
//! - **Detail left out.** A colour drag between Detail and Presence lights the frame without
//!   Detail: the light is the light of the stack without its Detail layer, bit for bit, and the
//!   CPU's for the prefix without Detail.
//! - **The plane.** The Presence link reading its light from the slot's light plane draws what the
//!   qualification harness draws with that light read back and written into its own plane, bit for
//!   bit.
//! - **The global rule.** A light that changes redraws the link reading it whole, whatever the
//!   tick's own change, every scratch plane starting from NaN.
//! - **Bounds.** The light link, the light plane and the light the pipeline keeps are charged to
//!   the GPU-preview budget and leave it when released.
//! - **Refits.** A slot refitted to another shape copies its unchanged light in from the lights
//!   the pipeline keeps, encoding no light pass, and draws a fresh slot's frame; a changed light is
//!   encoded; the pipeline keeps at most `LIGHT_CACHE`, charged.
//! - **The tile runner.** A tile runner, which holds a window of the source for each tile it draws,
//!   computes a light over a stage of several of the link's tiles from a window of the source for
//!   each: the slot's light over the whole source, bit for bit, on its own device, and once.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing: the skip is the
//! report, and `cargo test` counting it as passed does not make it GPU evidence.
use super::gpu_plan::{install_output_encoding, surface_light, surface_plan};
use super::gpu_qualification::{Stream, headless as qualifier};
use luxforge_core::{
    BASIC_EFFECT, CURVE_EFFECT, Cancel, Component, ComponentMode, DETAIL_EFFECT, GpuAnswer,
    GpuLightRestoration, GpuPlanRequest, Layer, LinearImage, LinearSettings, Mask, ModuleRegistry,
    PRESENCE_EFFECT, Recipe, RenderContext, RenderOptions, RenderSource, SourceImage, Stage,
    gpu_lights, gpu_plan, qualification, render,
};
use luxforge_reference::srgb;
use luxforge_ui::photo_surface::{
    BoundaryFormat, Derivation, GpuBoundary, GpuChange, GpuPlan, GpuRegion, GpuSource, TexelMap,
    gpu_preview::{
        light::{GpuLight, LIGHT_CACHE, LightBench, Lit, light_charge, lights_charge},
        qualification::Qualifier,
    },
};
use serde_json::{Value, json};
use std::sync::Arc;

/// A bench on this host's device, its largest texture `texture_limit` when given, with the core's
/// output encoding installed first, which the surface's passes that derive a boundary from the
/// source decode through: `None`, having printed that `test` was skipped, without an adapter.
fn on_device(test: &str, texture_limit: Option<u32>) -> Option<(LightBench, String)> {
    assert!(
        install_output_encoding(),
        "the surface holds the core's output encoding"
    );
    LightBench::headless(test, texture_limit)
}

/// The light's tolerance: what the existing test holds the GPU's selection to against the CPU's
/// preparation of the same reduction (`gpu_presence_atmospheric_light_matches_the_cpu`), largest
/// over the three channels.
const LIGHT_TOLERANCE: f64 = 2.0e-5;

/// The largest storage binding of a device of default limits, which a light link's buffers are
/// sized within.
const BINDING: u64 = 1 << 27;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    Byte,
    Linear,
}

/// A stage's pixels on both paths: a JPEG's 8-bit codes, and a RAW's linear planes.
struct Photo {
    width: u32,
    height: u32,
    /// RGBA, row by row.
    codes: Arc<Vec<u8>>,
    /// Red, then green, then blue.
    planes: Arc<Vec<f32>>,
}

impl Photo {
    /// The synthetic photograph the Presence tests draw: a hazy sky over a darker ground, a hard
    /// horizon, fine texture of a few pixels, a soft gradient and noise. Its codes encode its
    /// linear values; its planes hold them a quarter brighter, the sky past white as a RAW's is.
    fn synthetic(width: u32, height: u32, seed: u64) -> Self {
        let mut stream = Stream(seed);
        let len = (width * height) as usize;
        let mut planes = vec![0f32; 3 * len];
        let mut codes = Vec::with_capacity(4 * len);
        for index in 0..len {
            let (x, y) = (index as u32 % width, index as u32 / width);
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let base: [f32; 3] = if v < 0.4 {
                [0.55 + 0.2 * u, 0.62 + 0.15 * u, 0.75]
            } else {
                [0.12 + 0.1 * v, 0.16 + 0.05 * u, 0.08]
            };
            let texture = 0.04 * ((x as f32 * 1.7).sin() * (y as f32 * 2.3).cos());
            let noise = (stream.unit() as f32 - 0.5) * 0.03;
            let haze = 0.25 * (1.0 - v);
            for (channel, base) in base.into_iter().enumerate() {
                let value = (base * (1.0 - haze) + 0.7 * haze + texture + noise).max(0.0);
                codes.push(srgb::code(f64::from(value)));
                planes[channel * len + index] = value * 1.25;
            }
            codes.push(255);
        }
        Self {
            width,
            height,
            codes: Arc::new(codes),
            planes: Arc::new(planes),
        }
    }

    /// The corpus's Presence fixture as its generator draws it before encoding (`cargo xtask
    /// generate-fixtures`): a smooth gradient, a hard step edge, a low-amplitude checker and a flat
    /// grey, one in each quadrant, every channel alike, so its brightest blocks tie. Its planes are
    /// its codes' linear values.
    fn presence_fixture() -> Self {
        let (width, height) = (1440u32, 960u32);
        let (hw, hh) = (width / 2, height / 2);
        let level = |x: u32, y: u32| -> u8 {
            match (x < hw, y < hh) {
                (true, true) => (40.0 + x as f32 / hw as f32 * (255.0 - 40.0)).round() as u8,
                (false, true) if x - hw < hw / 2 => 70,
                (false, true) => 210,
                (true, false) if (x / 4 + (y - hh) / 4).is_multiple_of(2) => 118,
                (true, false) => 138,
                (false, false) => 128,
            }
        };
        let len = (width * height) as usize;
        let mut codes = Vec::with_capacity(4 * len);
        let mut planes = vec![0f32; 3 * len];
        for index in 0..len {
            let code = level(index as u32 % width, index as u32 / width);
            codes.extend([code, code, code, 255]);
            for channel in 0..3 {
                planes[channel * len + index] = srgb::decode(code) as f32;
            }
        }
        Self {
            width,
            height,
            codes: Arc::new(codes),
            planes: Arc::new(planes),
        }
    }

    fn jpeg(&self) -> SourceImage {
        SourceImage {
            width: self.width,
            height: self.height,
            rgba: Arc::clone(&self.codes),
            fingerprint: "sha256:gpu-light".into(),
            orientation: 1,
            capture: Default::default(),
        }
    }

    fn linear(&self) -> LinearImage {
        LinearImage::with_fingerprint(
            self.width,
            self.height,
            self.planes.to_vec(),
            "sha256:gpu-light",
        )
        .expect("a linear image")
    }

    /// The source the surface holds, of `version`.
    fn gpu(&self, path: Path, version: u64) -> GpuSource {
        match path {
            Path::Byte => {
                GpuSource::codes(version, Arc::clone(&self.codes), self.width, self.height)
            }
            Path::Linear => GpuSource::planes(
                version,
                Arc::clone(&self.planes),
                (self.width, self.height),
                [0, 0, self.width, self.height],
                1,
            ),
        }
        .expect("a whole source")
    }

    fn stage(&self) -> Stage {
        Stage {
            width: self.width,
            height: self.height,
        }
    }
}

/// The CPU's source of `path`.
fn cpu_source<'a>(path: Path, jpeg: &'a SourceImage, linear: &'a LinearImage) -> RenderSource<'a> {
    match path {
        Path::Byte => RenderSource::Byte(jpeg),
        Path::Linear => RenderSource::Linear {
            image: linear,
            settings: LinearSettings::default(),
        },
    }
}

/// A light link's request: from the source over its whole content stage at full scale.
fn request(photo: &Photo, path: Path) -> GpuPlanRequest {
    let request = GpuPlanRequest::exact(0, photo.stage()).from_source();
    match path {
        Path::Byte => request,
        Path::Linear => request.linear(),
    }
}

fn stack(layers: &[(&str, Value)]) -> Recipe {
    Recipe {
        layers: layers
            .iter()
            .map(|(effect, payload)| Layer::new(*effect, payload.clone()))
            .collect(),
        ..Recipe::default()
    }
}

/// A radial component: an ellipse a little off centre, tilted, with a broad feather.
fn radial() -> Component {
    Component::new(
        "Radial 1",
        ComponentMode::Add,
        "radial",
        json!({"x": 0.45, "y": 0.55, "radius_x": 0.3, "radius_y": 0.22, "angle": 18.0,
               "feather": 45.0}),
    )
}

/// The corpus's luminance band, intersected.
fn luminance_range() -> Component {
    Component::new(
        "Luminance 1",
        ComponentMode::Intersect,
        "luminance-range",
        json!({"low": 20.0, "low_feather": 10.0, "high": 80.0, "high_feather": 10.0}),
    )
}

/// `recipe` with layer `layer` masked by one mask of `components`.
fn masked(mut recipe: Recipe, layer: usize, components: &[Component]) -> Recipe {
    let mut mask = Mask::new("Mask 1");
    mask.components.extend(components.iter().cloned());
    recipe.layers[layer].mask = Some(mask.id.clone());
    recipe.masks.push(mask);
    recipe
}

/// The one light link of `recipe` for `request`, the core's and the surface's.
fn light_of(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
    restoration: GpuLightRestoration,
) -> (luxforge_core::GpuLight, GpuLight) {
    let lights = gpu_lights(registry, recipe, request, restoration).expect("a stack");
    let [light] = &lights[..] else {
        panic!("one light link, not {}", lights.len());
    };
    let surface = surface_light(light, 0).expect("a light the surface runs");
    (light.clone(), surface)
}

/// The plan of `recipe` reading its light, as the surface's over `boundary`: its reading
/// operation's light plane the slot's light 0, its light link the one it reads.
fn reading_plan(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
    boundary: GpuBoundary,
) -> GpuPlan {
    let plan = match gpu_plan(registry, recipe, request).expect("a stack") {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("{reason}"),
    };
    surface_plan(&plan, boundary).expect("a runnable plan")
}

/// The cut of `source`'s whole stage the plans here start from.
fn whole_cut(source: &GpuSource, version: u64) -> GpuBoundary {
    let (width, height) = source.stage();
    GpuBoundary::derived(
        source,
        Derivation::Cut { origin: (0, 0) },
        width,
        height,
        version,
    )
    .expect("a cut")
}

/// The light the CPU's reference render prepares for layer `layer` from its own reduction of the
/// layer's input, as its frame stores it: on the byte path, its 16-bit frame.
fn reference_light(
    registry: &ModuleRegistry,
    source: RenderSource<'_>,
    recipe: &Recipe,
    layer: usize,
) -> [f64; 3] {
    let context = RenderContext::new();
    let rendered = render(
        registry,
        source,
        recipe,
        RenderOptions::exact(&Cancel::never()),
        &context,
    )
    .expect("the reference render");
    let estimates = qualification::frame_estimates(&rendered, layer).expect("a spatial layer");
    let values = estimates
        .into_iter()
        .flatten()
        .find(|values| values.len() == 3)
        .expect("Dehaze's light");
    [values[0], values[1], values[2]]
}

/// The CPU's light at full resolution for layer `layer` (`qualification::twin_lights` at f = 1):
/// the stage before its colour run rendered exactly, the run per pixel in `f32`, clamped on the
/// byte path, each 16-pixel block averaged in `f64` and the light selected, with no 16-bit hand-off;
/// restoration layers left out when `skip_restoration`.
fn full_resolution_light(
    registry: &ModuleRegistry,
    source: RenderSource<'_>,
    recipe: &Recipe,
    layer: usize,
    skip_restoration: bool,
) -> [f64; 3] {
    let lights =
        qualification::twin_lights(registry, source, recipe, &[layer], &[1], skip_restoration)
            .expect("the CPU's light at full resolution");
    let values = lights[0][0].clone().expect("a light");
    [values[0], values[1], values[2]]
}

/// The CPU's preparation of the light from the very stage the light link reads: the source's cut
/// as the surface derives it, through the link's colour steps on this device, clamped where the
/// byte path clamps; then the CPU's 16-pixel reduction of it in `f64` and Dehaze's selection
/// (`qualification::presence::atmosphere`).
fn over_held_stage(
    qualifier: &Qualifier,
    source: &GpuSource,
    light: &GpuLight,
    path: Path,
) -> [f64; 3] {
    let (width, height) = light.stage;
    let cut = whole_cut(source, 1);
    let bytes = qualifier.derive(source, &cut).expect("the cut");
    let boundary = GpuBoundary::new(Arc::new(bytes), width, height, 1, source.kind().boundary())
        .expect("the held stage");
    let colour = light.steps[..light.steps.len() - 1].to_vec();
    let values = qualifier
        .evaluate(&GpuPlan {
            boundary,
            texels: TexelMap::IDENTITY,
            steps: colour,
            region: None,
            lights: Vec::new(),
        })
        .expect("the colour run");
    let len = values.len();
    let mut planes = vec![0f32; 3 * len];
    for (index, texel) in values.iter().enumerate() {
        for channel in 0..3 {
            planes[channel * len + index] = match path {
                Path::Byte => texel[channel].clamp(0.0, 1.0),
                Path::Linear => texel[channel],
            };
        }
    }
    qualification::presence::atmosphere(width, height, &planes)
}

/// The largest difference of a channel between the GPU's light and a CPU one.
fn largest(gpu: [f32; 4], cpu: [f64; 3]) -> f64 {
    (0..3)
        .map(|channel| (f64::from(gpu[channel]) - cpu[channel]).abs())
        .fold(0.0, f64::max)
}

/// A light read back as bits: its value's and its block plane's.
fn bits(lit: &Lit) -> ([u32; 4], Vec<[u32; 4]>) {
    (
        lit.light.map(f32::to_bits),
        lit.blocks
            .iter()
            .map(|block| block.map(f32::to_bits))
            .collect(),
    )
}

/// The stacks the light is held to the CPU over: a plain one, an exposure, tone and curve prefix,
/// a prefix masked by a radial and a luminance range, and three stops either way with Whites and
/// Blacks at the same end under Dehaze at the same end. Dehaze is each one's last layer.
fn stacks() -> Vec<(&'static str, Recipe)> {
    vec![
        (
            "a plain source",
            stack(&[(PRESENCE_EFFECT, json!({"dehaze": 60, "clarity": 20}))]),
        ),
        (
            "exposure, tone and a curve",
            stack(&[
                (
                    BASIC_EFFECT,
                    json!({"exposure": 0.7, "contrast": 35.0, "highlights": -40.0,
                           "shadows": 30.0}),
                ),
                (
                    CURVE_EFFECT,
                    json!({"luminance": [[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]]}),
                ),
                (PRESENCE_EFFECT, json!({"dehaze": 45})),
            ]),
        ),
        (
            "masked by a radial and a luminance range",
            masked(
                stack(&[
                    (BASIC_EFFECT, json!({"exposure": 1.2, "contrast": 20.0})),
                    (PRESENCE_EFFECT, json!({"dehaze": 50})),
                ]),
                0,
                &[radial(), luminance_range()],
            ),
        ),
        (
            "+3 EV, Whites and Blacks +100",
            stack(&[
                (
                    BASIC_EFFECT,
                    json!({"exposure": 3.0, "whites": 100.0, "blacks": 100.0}),
                ),
                (PRESENCE_EFFECT, json!({"dehaze": 100})),
            ]),
        ),
        (
            "-3 EV, Whites and Blacks -100",
            stack(&[
                (
                    BASIC_EFFECT,
                    json!({"exposure": -3.0, "whites": -100.0, "blacks": -100.0}),
                ),
                (PRESENCE_EFFECT, json!({"dehaze": -100})),
            ]),
        ),
    ]
}

/// The light a light link computes is the CPU's preparation of the same stage: Dehaze's selection
/// from the 16-pixel block means of the whole stage at full resolution, the prefix's colour run per
/// pixel and clamped where the byte path clamps. Held within the light's tolerance to the CPU's
/// preparation over the very stage the link reads, on both paths; and on the linear path, whose
/// stage is the CPU's own `f32` frame, to the reference render's own light and to the CPU's light
/// at full resolution. On the byte path the surface holds the source's cut in half floats, as every
/// boundary of a JPEG is held, and the reference hands its operation a 16-bit frame: its distance
/// from both is reported beside it.
#[test]
fn gpu_light_the_gpu_light_is_the_cpus_preparation_of_the_same_stage() {
    let test = "gpu_light_the_gpu_light_is_the_cpus_preparation_of_the_same_stage";
    let Some((mut bench, adapter)) = on_device(test, None) else {
        return;
    };
    let Some(qualifier) = qualifier(test) else {
        return;
    };
    eprintln!("{test}: adapter {adapter}");
    let registry = ModuleRegistry::builtin();
    let photos = [
        (
            "a 1001 x 667 synthetic photograph",
            Photo::synthetic(1001, 667, 0x11),
        ),
        ("the 1440 x 960 Presence fixture", Photo::presence_fixture()),
    ];
    let mut missed = Vec::new();
    let mut worst = [0.0_f64; 3];
    let mut version = 0;
    for (name, photo) in &photos {
        for path in [Path::Byte, Path::Linear] {
            version += 1;
            let (jpeg, linear) = (photo.jpeg(), photo.linear());
            let source = cpu_source(path, &jpeg, &linear);
            let gpu = photo.gpu(path, version);
            for (case, recipe) in stacks() {
                let layer = recipe.layers.len() - 1;
                let (_, light) = light_of(
                    &registry,
                    &recipe,
                    request(photo, path),
                    GpuLightRestoration::LeftOut,
                );
                let lit = bench.light(&gpu, &light).expect("the light");
                let held = largest(lit.light, over_held_stage(&qualifier, &gpu, &light, path));
                let reference = largest(
                    lit.light,
                    reference_light(&registry, source, &recipe, layer),
                );
                let full = largest(
                    lit.light,
                    full_resolution_light(&registry, source, &recipe, layer, false),
                );
                eprintln!(
                    "{name}, {path:?}, {case}: light {:?}; against the CPU over the held stage \
                     {held:.3e}, the reference {reference:.3e}, the CPU at full resolution \
                     {full:.3e}",
                    &lit.light[..3]
                );
                worst = [
                    worst[0].max(held),
                    worst[1].max(reference),
                    worst[2].max(full),
                ];
                let gated = match path {
                    Path::Byte => held,
                    Path::Linear => held.max(reference).max(full),
                };
                if gated >= LIGHT_TOLERANCE {
                    missed.push(format!("{name}, {path:?}, {case}: {gated:.3e}"));
                }
            }
        }
    }
    eprintln!(
        "{test}: largest against the held stage {:.3e}, the reference {:.3e}, the CPU at full \
         resolution {:.3e}",
        worst[0], worst[1], worst[2]
    );
    assert!(
        missed.is_empty(),
        "lights past {LIGHT_TOLERANCE:e}: {missed:?}"
    );
}

/// A stage wider and taller than the device's largest texture — 300 × 200 at 160, held in two
/// source textures each way, as the source tests hold one — is cut and reduced in four tiles into
/// one block plane: the light and every block mean are the ones a device holding the stage in one
/// texture reduces in one tile, bit for bit, and a second link reducing it again draws the same
/// bits. The tiled link holds a cut's words for each of its four tiles.
#[test]
fn gpu_light_a_stage_wider_than_the_texture_limit_reduces_into_one_block_plane() {
    let test = "gpu_light_a_stage_wider_than_the_texture_limit_reduces_into_one_block_plane";
    let Some((mut whole, adapter)) = on_device(test, None) else {
        return;
    };
    let Some((mut tiled, _)) = on_device(test, Some(160)) else {
        return;
    };
    eprintln!("{test}: adapter {adapter}");
    let mut again = whole.another();
    let registry = ModuleRegistry::builtin();
    let photo = Photo::synthetic(300, 200, 0x22);
    let recipe = stack(&[
        (BASIC_EFFECT, json!({"exposure": 0.5, "contrast": 20.0})),
        (PRESENCE_EFFECT, json!({"dehaze": 40})),
    ]);
    for (version, path) in [(1, Path::Byte), (2, Path::Linear)] {
        let gpu = photo.gpu(path, version);
        let format = match path {
            Path::Byte => BoundaryFormat::Half,
            Path::Linear => BoundaryFormat::Float,
        };
        let (_, light) = light_of(
            &registry,
            &recipe,
            request(&photo, path),
            GpuLightRestoration::LeftOut,
        );
        let one = whole.light(&gpu, &light).expect("one tile");
        let four = tiled.light(&gpu, &light).expect("four tiles");
        let twice = again.light(&gpu, &light).expect("one tile again");
        eprintln!("{path:?}: light {:?}", &one.light[..3]);
        assert_eq!(one.grid, (19, 13), "{path:?}: the stage's blocks");
        assert_eq!(
            bits(&four),
            bits(&one),
            "{path:?}: four tiles reduce into the one-texture block plane"
        );
        assert_eq!(bits(&twice), bits(&one), "{path:?}: the light twice");
        assert_eq!(
            tiled.light_bytes(),
            light_charge(&light, format, 160, BINDING),
            "{path:?}"
        );
        assert_eq!(
            whole.light_bytes(),
            light_charge(&light, format, 8192, BINDING),
            "{path:?}"
        );
        assert_ne!(tiled.light_bytes(), whole.light_bytes(), "{path:?}");
    }
}

/// A colour drag between Detail and Presence lights its frame with Detail left out, the recorded
/// default for motion: the light is the light of the same stack without its Detail layer, bit for
/// bit, computed by another link; and it is the CPU's light at full resolution for the prefix
/// without Detail (`qualification::twin_lights` skipping restoration at f = 1) within the light's
/// tolerance on the linear path, and the CPU's preparation over the stage the link reads on both.
/// How far Detail moves the light — the reference's, with Detail — is reported beside it.
#[test]
fn gpu_light_a_light_with_detail_left_out_is_the_cpus_for_the_prefix_without_detail() {
    let test = "gpu_light_a_light_with_detail_left_out_is_the_cpus_for_the_prefix_without_detail";
    let Some((mut bench, adapter)) = on_device(test, None) else {
        return;
    };
    let Some(qualifier) = qualifier(test) else {
        return;
    };
    eprintln!("{test}: adapter {adapter}");
    let mut other = bench.another();
    let registry = ModuleRegistry::builtin();
    // Sharpen stress under Dehaze at -100, the cells the measurement found Detail moves the light
    // in, a masked colour layer between them.
    let full = masked(
        stack(&[
            (BASIC_EFFECT, json!({"exposure": 0.4, "contrast": 15.0})),
            (
                DETAIL_EFFECT,
                json!({"sharpening": 150, "radius": 3, "sharpen-detail": 100,
                       "sharpen-masking": 0}),
            ),
            (BASIC_EFFECT, json!({"contrast": 25.0, "highlights": -20.0})),
            (PRESENCE_EFFECT, json!({"dehaze": -100})),
        ]),
        2,
        &[radial()],
    );
    let mut without = full.clone();
    without.layers.remove(1);
    let photo = Photo::synthetic(1001, 667, 0x33);
    let mut missed = Vec::new();
    for (version, path) in [(1, Path::Byte), (2, Path::Linear)] {
        let (jpeg, linear) = (photo.jpeg(), photo.linear());
        let source = cpu_source(path, &jpeg, &linear);
        let gpu = photo.gpu(path, version);
        let (core, left_out) = light_of(
            &registry,
            &full,
            request(&photo, path),
            GpuLightRestoration::LeftOut,
        );
        assert_eq!(core.left_out, [1], "{path:?}: Detail left out");
        let (_, plain) = light_of(
            &registry,
            &without,
            request(&photo, path),
            GpuLightRestoration::LeftOut,
        );
        let lit = bench.light(&gpu, &left_out).expect("the light");
        let plain = other.light(&gpu, &plain).expect("the light without Detail");
        assert_eq!(
            bits(&lit),
            bits(&plain),
            "{path:?}: the light with Detail left out is the light without Detail"
        );
        let held = largest(
            lit.light,
            over_held_stage(&qualifier, &gpu, &left_out, path),
        );
        let twin = largest(
            lit.light,
            full_resolution_light(&registry, source, &full, 3, true),
        );
        let detail = largest(lit.light, reference_light(&registry, source, &full, 3));
        eprintln!(
            "{path:?}: light {:?}; against the CPU over the held stage {held:.3e}, the CPU at full \
             resolution without Detail {twin:.3e}; Detail moves the reference's light by \
             {detail:.3e}",
            &lit.light[..3]
        );
        let gated = match path {
            Path::Byte => held,
            Path::Linear => held.max(twin),
        };
        if gated >= LIGHT_TOLERANCE {
            missed.push(format!("{path:?}: {gated:.3e}"));
        }
    }
    assert!(
        missed.is_empty(),
        "lights past {LIGHT_TOLERANCE:e}: {missed:?}"
    );
}

/// The Presence link reading its light from the slot's light plane, which a light link wrote,
/// draws what the qualification harness draws with that light read back and written into its own
/// light plane, bit for bit: every Presence unit with Dehaze removing a veil, and Dehaze adding one
/// alone, on both paths. So a frame the harness measures with a light it is given is the frame the
/// slot draws with the light its link computes.
#[test]
fn gpu_light_a_dehaze_pass_reading_the_light_plane_draws_what_the_harness_draws() {
    let test = "gpu_light_a_dehaze_pass_reading_the_light_plane_draws_what_the_harness_draws";
    let Some((mut bench, adapter)) = on_device(test, None) else {
        return;
    };
    let Some(qualifier) = qualifier(test) else {
        return;
    };
    eprintln!("{test}: adapter {adapter}");
    let registry = ModuleRegistry::builtin();
    let photo = Photo::synthetic(640, 432, 0x44);
    for (version, path) in [(1, Path::Byte), (2, Path::Linear)] {
        let gpu = photo.gpu(path, version);
        for payload in [
            json!({"dehaze": 70, "texture": 30, "clarity": -20}),
            json!({"dehaze": -60}),
        ] {
            let recipe = stack(&[(PRESENCE_EFFECT, payload.clone())]);
            let request = request(&photo, path);
            let (_, light) = light_of(&registry, &recipe, request, GpuLightRestoration::LeftOut);
            let lit = bench.light(&gpu, &light).expect("the light");
            let reading = reading_plan(&registry, &recipe, request, whole_cut(&gpu, version));
            let drawn = bench
                .draw(&gpu, Some(&light), &reading, None)
                .expect("the light read from the plane");
            // The harness over the same cut, its light plane holding the light read back.
            let cut = qualifier.derive(&gpu, &reading.boundary).expect("the cut");
            let held = GpuBoundary::new(
                Arc::new(cut),
                photo.width,
                photo.height,
                version,
                gpu.kind().boundary(),
            )
            .expect("the held stage");
            qualifier.set_lights(vec![lit.light]);
            let given = qualifier
                .evaluate_codes(&GpuPlan {
                    boundary: held,
                    ..reading.clone()
                })
                .expect("the harness's frame");
            assert_eq!(drawn.len(), (photo.width * photo.height) as usize);
            let differing = drawn
                .iter()
                .zip(&given)
                .filter(|(slot, harness)| slot != harness)
                .count();
            eprintln!("{path:?}, {payload}: {differing} pixels differ");
            assert_eq!(differing, 0, "{path:?}, {payload}");
        }
    }
}

/// A light that changes redraws the link reading it whole, wherever the tick's own change lies: a
/// tick whose plan changes only a small rectangle of the stage, its light written anew from a
/// brighter prefix, draws the frame a whole evaluation of the new light draws, bit for bit, every
/// scratch plane of both slots starting from NaN; and the new light changed the frame far outside
/// that rectangle.
#[test]
fn gpu_light_a_light_change_redraws_the_link_whole() {
    let test = "gpu_light_a_light_change_redraws_the_link_whole";
    let Some((mut bench, adapter)) = on_device(test, None) else {
        return;
    };
    eprintln!("{test}: adapter {adapter}");
    let mut fresh = bench.another();
    bench.set_poison(true);
    fresh.set_poison(true);
    let registry = ModuleRegistry::builtin();
    let photo = Photo::synthetic(640, 432, 0x55);
    let presence = json!({"dehaze": 70, "clarity": 30});
    let reader = stack(&[(PRESENCE_EFFECT, presence.clone())]);
    let brighter = stack(&[
        (BASIC_EFFECT, json!({"exposure": 1.0})),
        (PRESENCE_EFFECT, presence),
    ]);
    let rect = [16, 16, 48, 48];
    for (version, path) in [(1, Path::Byte), (2, Path::Linear)] {
        let gpu = photo.gpu(path, version);
        let request = request(&photo, path);
        let (_, first) = light_of(&registry, &reader, request, GpuLightRestoration::LeftOut);
        let (_, second) = light_of(&registry, &brighter, request, GpuLightRestoration::LeftOut);
        let plan = reading_plan(&registry, &reader, request, whole_cut(&gpu, version));
        let before = bench
            .draw(
                &gpu,
                Some(&first),
                &plan,
                Some(GpuChange {
                    serial: 1,
                    since: None,
                }),
            )
            .expect("the first light");
        let passes = bench.spatial_passes();
        let after = bench
            .draw(
                &gpu,
                Some(&second),
                &plan,
                Some(GpuChange {
                    serial: 2,
                    since: Some((1, rect)),
                }),
            )
            .expect("the second light, a small change");
        let ran = bench.spatial_passes() - passes;
        let whole = fresh
            .draw(&gpu, Some(&second), &plan, None)
            .expect("the second light, whole");
        let outside = before
            .iter()
            .zip(&after)
            .enumerate()
            .filter(|(index, (old, new))| {
                let (x, y) = (*index as u32 % photo.width, *index as u32 / photo.width);
                old != new && !(rect[0] <= x && x < rect[2] && rect[1] <= y && y < rect[3])
            })
            .count();
        let differing = after.iter().zip(&whole).filter(|(a, b)| a != b).count();
        eprintln!(
            "{path:?}: {ran} passes ran for the new light; {outside} pixels outside the change \
             differ from the first light's; {differing} differ from a whole evaluation"
        );
        assert!(ran > 0, "{path:?}: the passes reading the light ran");
        assert!(
            outside > 0,
            "{path:?}: the light changed the frame outside the change"
        );
        assert_eq!(differing, 0, "{path:?}: the tick drew the link whole");
    }
}

/// The light link and the light plane are charged to the GPU-preview budget when created, the
/// link's figure the one the desktop holds a plan's light links to before any exists
/// ([`light_charge`]): the tile texture and the block plane as scratch, beside the light plane's
/// one texel; and everything leaves the budget when released, the source with it.
#[test]
fn gpu_light_the_light_planes_are_charged_and_released() {
    let test = "gpu_light_the_light_planes_are_charged_and_released";
    let Some((mut bench, adapter)) = on_device(test, None) else {
        return;
    };
    eprintln!("{test}: adapter {adapter}");
    let registry = ModuleRegistry::builtin();
    // Two tiles across, one down.
    let photo = Photo::synthetic(2600, 1700, 0x66);
    let recipe = stack(&[
        (BASIC_EFFECT, json!({"exposure": 0.3})),
        (PRESENCE_EFFECT, json!({"dehaze": 50})),
    ]);
    for (version, path) in [(1, Path::Byte), (2, Path::Linear)] {
        assert_eq!(bench.charged(), (0, 0), "{path:?}: nothing held");
        let gpu = photo.gpu(path, version);
        let format = match path {
            Path::Byte => BoundaryFormat::Half,
            Path::Linear => BoundaryFormat::Float,
        };
        let (_, light) = light_of(
            &registry,
            &recipe,
            request(&photo, path),
            GpuLightRestoration::LeftOut,
        );
        bench.light(&gpu, &light).expect("the light");
        let link = light_charge(&light, format, 8192, BINDING).expect("a light link");
        assert_eq!(bench.light_bytes(), Some(link), "{path:?}");
        let textures = 2048 * 1700 * format.texel_bytes() as u64 + 163 * 107 * 16;
        let (in_use, scratch) = bench.charged();
        eprintln!(
            "{path:?}: the light link {link} B, its textures {textures} B, in use {in_use} B"
        );
        // The light plane, and the light the pipeline keeps beside it.
        assert_eq!(bench.kept_lights(), (1, 16), "{path:?}: the light kept");
        assert_eq!(
            in_use,
            gpu.bytes() + 16 + 16 + link,
            "{path:?}: the source, light, kept light and link"
        );
        assert_eq!(
            scratch,
            16 + 16 + textures,
            "{path:?}: the light plane, the kept light and the link's textures"
        );
        bench.release();
        assert_eq!(bench.charged(), (0, 0), "{path:?}: everything released");
    }
}

/// The desktop's slot runs a plan's light links before its chain, every frame
/// (`PhotoPipeline::prepare_gpu`): a Presence plan reading its light draws what the harness draws
/// with the light its link computes, bit for bit; drawn again unchanged, it runs no pass and
/// encodes no light; a tick whose light changes, its own change a small rectangle, draws what a
/// fresh slot draws whole; and the slot's light link is charged with it and released with it. On
/// both paths.
#[test]
fn gpu_light_the_slot_runs_its_plans_light_links_before_its_chain() {
    let test = "gpu_light_the_slot_runs_its_plans_light_links_before_its_chain";
    let Some((mut bench, adapter)) = on_device(test, None) else {
        return;
    };
    let Some(qualifier) = qualifier(test) else {
        return;
    };
    eprintln!("{test}: adapter {adapter}");
    let mut lights = bench.another();
    let registry = ModuleRegistry::builtin();
    let photo = Photo::synthetic(640, 432, 0x77);
    let presence = json!({"dehaze": 70, "clarity": 30});
    let reader = stack(&[(PRESENCE_EFFECT, presence.clone())]);
    let brighter = stack(&[
        (BASIC_EFFECT, json!({"exposure": 1.0})),
        (PRESENCE_EFFECT, presence),
    ]);
    let rect = [16, 16, 48, 48];
    for (version, path) in [(1, Path::Byte), (2, Path::Linear)] {
        let mut fresh = bench.another();
        let gpu = photo.gpu(path, version);
        let request = request(&photo, path);
        let plan = reading_plan(&registry, &reader, request, whole_cut(&gpu, version));
        assert_eq!(
            plan.lights.len(),
            1,
            "{path:?}: the plan carries its light link"
        );
        let drawn = bench
            .prepare(
                &gpu,
                &plan,
                Some(GpuChange {
                    serial: 1,
                    since: None,
                }),
            )
            .expect("the frame");
        let lit = lights.light(&gpu, &plan.lights[0]).expect("the light");
        let cut = qualifier.derive(&gpu, &plan.boundary).expect("the cut");
        let held = GpuBoundary::new(
            Arc::new(cut),
            photo.width,
            photo.height,
            version,
            gpu.kind().boundary(),
        )
        .expect("the held stage");
        qualifier.set_lights(vec![lit.light]);
        let given = qualifier
            .evaluate_codes(&GpuPlan {
                boundary: held,
                ..plan.clone()
            })
            .expect("the harness's frame");
        let differing = drawn.iter().zip(&given).filter(|(a, b)| a != b).count();
        assert_eq!(differing, 0, "{path:?}: the slot's frame is the harness's");
        assert_eq!(bench.surface_lights().0, 1, "{path:?}: one light link");
        // The same plan again: the light holds its key, and nothing runs.
        let passes = bench.spatial_passes();
        let again = bench
            .prepare(
                &gpu,
                &plan,
                Some(GpuChange {
                    serial: 2,
                    since: Some((1, [0, 0, 0, 0])),
                }),
            )
            .expect("the frame again");
        assert_eq!(again, drawn, "{path:?}");
        assert_eq!(bench.spatial_passes(), passes, "{path:?}: no pass ran");
        // A brighter prefix's light, the tick's own change a small rectangle: drawn whole.
        let (_, second) = light_of(&registry, &brighter, request, GpuLightRestoration::LeftOut);
        let changed = GpuPlan {
            lights: vec![second],
            ..plan.clone()
        };
        let after = bench
            .prepare(
                &gpu,
                &changed,
                Some(GpuChange {
                    serial: 3,
                    since: Some((2, rect)),
                }),
            )
            .expect("the new light");
        let whole = fresh.prepare(&gpu, &changed, None).expect("a fresh slot");
        let outside = drawn
            .iter()
            .zip(&after)
            .enumerate()
            .filter(|(index, (old, new))| {
                let (x, y) = (*index as u32 % photo.width, *index as u32 / photo.width);
                old != new && !(rect[0] <= x && x < rect[2] && rect[1] <= y && y < rect[3])
            })
            .count();
        let differing = after.iter().zip(&whole).filter(|(a, b)| a != b).count();
        eprintln!(
            "{path:?}: {outside} pixels outside the change moved with the light; {differing} \
             differ from a fresh slot's"
        );
        assert!(outside > 0, "{path:?}: the light changed the frame");
        assert_eq!(differing, 0, "{path:?}: the tick drew the link whole");
        fresh.release();
        bench.release();
        assert_eq!(bench.surface_lights(), (0, 0), "{path:?}: let go");
        assert_eq!(bench.charged(), (0, 0), "{path:?}: everything released");
    }
}

/// A slot refitted to another shape — a region of the same boundary, as a picture at rest's edge
/// tile refits to its window — copies its unchanged light in from the lights the pipeline keeps:
/// no light pass is encoded, and its frame is a fresh slot's, which computes the light, bit for
/// bit. A changed light is encoded. The pipeline keeps at most [`LIGHT_CACHE`] lights, each
/// charged, and lets them go with the source. On both paths.
#[test]
fn gpu_light_a_refit_slot_restores_its_unchanged_light() {
    let test = "gpu_light_a_refit_slot_restores_its_unchanged_light";
    let Some((mut bench, adapter)) = on_device(test, None) else {
        return;
    };
    eprintln!("{test}: adapter {adapter}");
    let registry = ModuleRegistry::builtin();
    let photo = Photo::synthetic(640, 432, 0x88);
    let presence = json!({"dehaze": 70, "clarity": 30});
    let reader = stack(&[(PRESENCE_EFFECT, presence.clone())]);
    let brighter = stack(&[
        (BASIC_EFFECT, json!({"exposure": 1.0})),
        (PRESENCE_EFFECT, presence),
    ]);
    for (version, path) in [(1, Path::Byte), (2, Path::Linear)] {
        let mut fresh = bench.another();
        let gpu = photo.gpu(path, version);
        let request = request(&photo, path);
        let plan = reading_plan(&registry, &reader, request, whole_cut(&gpu, version));
        let counts = bench.light_counts();
        bench.prepare(&gpu, &plan, None).expect("the whole frame");
        let after_whole = bench.light_counts();
        assert_eq!(
            (after_whole.0 - counts.0, after_whole.1 - counts.1),
            (1, 0),
            "{path:?}: the first frame encodes its light"
        );
        // A region of the same boundary: another output, so another slot and pool.
        let region = GpuPlan {
            region: Some(GpuRegion {
                rect: [0, 0, 320, 216],
                stage: (photo.width, photo.height),
                full_stage: (photo.width, photo.height),
            }),
            ..plan.clone()
        };
        let refit = bench.prepare(&gpu, &region, None).expect("the region");
        let evaluation = bench.evaluation();
        let after_refit = bench.light_counts();
        eprintln!("{path:?}: the refit's evaluation {evaluation:?}");
        assert_eq!(evaluation.refits, 1, "{path:?}: the slot was refitted");
        assert_eq!(
            (evaluation.lights_encoded, evaluation.lights_restored),
            (0, 1),
            "{path:?}: the unchanged light is copied in"
        );
        assert_eq!(after_refit.0, after_whole.0, "{path:?}: no light pass");
        let whole = fresh.prepare(&gpu, &region, None).expect("a fresh slot");
        let differing = refit.iter().zip(&whole).filter(|(a, b)| a != b).count();
        assert_eq!(
            differing, 0,
            "{path:?}: the restored light draws a fresh slot's frame"
        );
        // A changed light over the refitted slot is computed.
        let (_, second) = light_of(&registry, &brighter, request, GpuLightRestoration::LeftOut);
        let changed = GpuPlan {
            lights: vec![second],
            ..region.clone()
        };
        bench
            .prepare(&gpu, &changed, None)
            .expect("the changed light");
        assert_eq!(
            bench.light_counts().0 - after_refit.0,
            1,
            "{path:?}: the changed light is encoded"
        );
        assert_eq!(bench.kept_lights(), (2, 32), "{path:?}: both lights kept");
        // More lights than the cache holds: the least recently used are overwritten.
        for step in 0..LIGHT_CACHE {
            let exposure = stack(&[
                (BASIC_EFFECT, json!({"exposure": 0.1 * (step + 1) as f64})),
                (PRESENCE_EFFECT, json!({"dehaze": 40})),
            ]);
            let (_, light) = light_of(&registry, &exposure, request, GpuLightRestoration::LeftOut);
            bench.light(&gpu, &light).expect("a light");
        }
        assert_eq!(
            bench.kept_lights(),
            (LIGHT_CACHE, LIGHT_CACHE as u64 * 16),
            "{path:?}: the cache's bound, charged"
        );
        fresh.release();
        bench.release();
        assert_eq!(
            bench.kept_lights(),
            (0, 0),
            "{path:?}: let go with the source"
        );
        assert_eq!(bench.charged(), (0, 0), "{path:?}: everything released");
    }
}

/// A plan reading two lights runs both links one after another through the surface's one tile
/// texture: its frame is the harness's over the lights each link computes alone on a bench of its
/// own, bit for bit, and the surface holds the two links and one tile texture, what
/// `lights_charge` charges for them. On both paths.
#[test]
fn gpu_light_two_light_links_share_one_tile_texture() {
    let test = "gpu_light_two_light_links_share_one_tile_texture";
    let Some((mut bench, adapter)) = on_device(test, None) else {
        return;
    };
    let Some(qualifier) = qualifier(test) else {
        return;
    };
    eprintln!("{test}: adapter {adapter}");
    let mut alone = bench.another();
    let registry = ModuleRegistry::builtin();
    let photo = Photo::synthetic(640, 432, 0x5a);
    let recipe = masked(
        stack(&[
            (BASIC_EFFECT, json!({"exposure": 0.3})),
            (PRESENCE_EFFECT, json!({"dehaze": 60})),
            (PRESENCE_EFFECT, json!({"dehaze": -40, "clarity": 20})),
        ]),
        1,
        &[radial()],
    );
    for (version, path) in [(1, Path::Byte), (2, Path::Linear)] {
        let gpu = photo.gpu(path, version);
        let plan = reading_plan(
            &registry,
            &recipe,
            request(&photo, path),
            whole_cut(&gpu, version),
        );
        assert_eq!(plan.lights.len(), 2, "{path:?}: two light links");
        let drawn = bench.prepare(&gpu, &plan, None).expect("the frame");
        let lit: Vec<[f32; 4]> = plan
            .lights
            .iter()
            .map(|light| {
                let lit = alone.light(&gpu, light).expect("a light alone").light;
                alone.release();
                lit
            })
            .collect();
        let cut = qualifier.derive(&gpu, &plan.boundary).expect("the cut");
        let held = GpuBoundary::new(
            Arc::new(cut),
            photo.width,
            photo.height,
            version,
            gpu.kind().boundary(),
        )
        .expect("the held stage");
        qualifier.set_lights(lit);
        let given = qualifier
            .evaluate_codes(&GpuPlan {
                boundary: held,
                ..plan.clone()
            })
            .expect("the harness's frame");
        let differing = drawn.iter().zip(&given).filter(|(a, b)| a != b).count();
        assert_eq!(differing, 0, "{path:?}: the slot's frame is the harness's");
        let format = gpu.kind().boundary();
        let charge = lights_charge(&plan.lights, format, 8192, BINDING).expect("light links");
        let one = |light| light_charge(light, format, 8192, BINDING).expect("a light link");
        assert_eq!(
            bench.surface_lights(),
            (2, charge),
            "{path:?}: two links, one tile"
        );
        assert!(
            charge < one(&plan.lights[0]) + one(&plan.lights[1]),
            "{path:?}: one tile texture, not two"
        );
        bench.release();
        assert_eq!(bench.charged(), (0, 0), "{path:?}: everything released");
    }
}

/// A tile runner computes a light the way the slot does, a window of the source cut for each of the
/// link's tiles in turn rather than the whole source held: over a stage of 2300 × 2200, two of the
/// link's 2048-pixel tiles across and two down, on both paths, its light is the slot's over the
/// whole source, bit for bit, though each is on a device of its own. A second run of the same light
/// over the same source is the one the runner kept, computed once.
#[test]
fn gpu_light_a_tile_runner_computes_the_slots_light_a_window_at_a_time() {
    let test = "gpu_light_a_tile_runner_computes_the_slots_light_a_window_at_a_time";
    let Some((mut bench, adapter)) = on_device(test, None) else {
        return;
    };
    let Some((backend, name)) = super::gpu_tiles_tests::host_adapter(test) else {
        return;
    };
    let mut runner =
        luxforge_ui::photo_surface::gpu_preview::tiles::TileRunner::open(&backend, &name)
            .unwrap_or_else(|refusal| panic!("{test}: the runner: {refusal:?}"));
    eprintln!("{test}: adapter {adapter}");
    let registry = ModuleRegistry::builtin();
    let photo = Photo::synthetic(2300, 2200, 0x5a);
    let recipe = stack(&[
        (BASIC_EFFECT, json!({"exposure": 0.4, "contrast": 15.0})),
        (PRESENCE_EFFECT, json!({"dehaze": 35})),
    ]);
    for (version, path) in [(1, Path::Byte), (2, Path::Linear)] {
        let gpu = photo.gpu(path, version);
        let (_, light) = light_of(
            &registry,
            &recipe,
            request(&photo, path),
            GpuLightRestoration::LeftOut,
        );
        let slot = bench.light(&gpu, &light).expect("the slot's light");
        let computed = runner.figures().lights;
        let windowed = runner.light(&gpu, &light).expect("the runner's light");
        let again = runner
            .light(&gpu, &light)
            .expect("the runner's light again");
        eprintln!("{path:?}: light {:?}", &slot.light[..3]);
        assert_eq!(
            windowed.map(f32::to_bits),
            slot.light.map(f32::to_bits),
            "{path:?}: a window at a time, the whole source's light"
        );
        assert_eq!(again.map(f32::to_bits), windowed.map(f32::to_bits));
        assert_eq!(
            runner.figures().lights,
            computed + 1,
            "{path:?}: computed once, then kept"
        );
    }
}
