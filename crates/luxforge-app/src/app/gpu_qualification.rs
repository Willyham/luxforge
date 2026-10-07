//! The GPU preview qualification helpers the program tests share (`docs/design/gpu-preview.md`,
//! "Qualifying a program"): a deterministic synthetic grid of linear pixels, the report helpers,
//! the corpus's sources and stacks opened in catalogs of their own, and the views an evidence
//! run's window draws.
//!
//! [`reference`] is the release gate's harness, and the one corpus harness: every stack of the
//! corpus at every view it lists, each GPU frame against the reference renderer's whole frame at
//! the view's size. `cargo xtask gpu-qualification --families F,... --zoom fit|33|50|100` runs a
//! program class's families alone.
use luxforge_core::{GpuAnswer, GpuPlanRequest, gpu_plan};
use luxforge_gpu::{
    GpuPlan, qualification::Qualifier, qualification::boundary, qualification::held,
};
use luxforge_reference::{preview_error::Statistics, srgb};
use serde_json::{Value, json};

mod reference;

// ---- The synthetic grid -----------------------------------------------------------------------

/// A deterministic `splitmix64` stream: the grid is the same on every host and every run.
pub(crate) struct Stream(pub(crate) u64);

impl Stream {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The values every grid draws its out-of-range extremes from: the largest half floats of both
/// signs, the smallest normal ones, zeros of both signs, and values past white and below black.
pub(crate) const EXTREMES: [f32; 10] = [
    65504.0, -65504.0, 6.104e-5, -6.104e-5, 0.0, -0.0, 2048.0, -16.0, 1.0e-7, 1.0,
];

/// One linear channel: in range (seventy percent, uniform in encoded sRGB so the shadows are as
/// dense as the highlights), past white up to 16, below black down to -0.5, or an extreme.
pub(crate) fn channel(stream: &mut Stream) -> f32 {
    let kind = stream.unit();
    let value = stream.unit();
    if kind < 0.70 {
        srgb::decode_encoded(value) as f32
    } else if kind < 0.82 {
        (1.0 + 15.0 * value * value) as f32
    } else if kind < 0.94 {
        (-0.5 * value) as f32
    } else {
        EXTREMES[(value * EXTREMES.len() as f64) as usize % EXTREMES.len()]
    }
}

/// A dense synthetic grid of `width × height` linear pixels, each channel as the boundary holds it
/// (the nearest half float): one pixel in ten an exact grey, the rest three independent channels.
pub(crate) fn grid(width: u32, height: u32, seed: u64) -> Vec<[f32; 3]> {
    let mut stream = Stream(seed);
    (0..width * height)
        .map(|_| {
            if stream.unit() < 0.1 {
                let grey = held(channel(&mut stream));
                [grey; 3]
            } else {
                [
                    held(channel(&mut stream)),
                    held(channel(&mut stream)),
                    held(channel(&mut stream)),
                ]
            }
        })
        .collect()
}

// ---- Reports ----------------------------------------------------------------------------------

/// `pixels` as 8-bit sRGB, three bytes a pixel, through the independent reference's quantizer.
pub(crate) fn codes(pixels: impl Iterator<Item = [f32; 3]>) -> Vec<u8> {
    pixels
        .flat_map(|rgb| rgb.map(|value| srgb::code(f64::from(value))))
        .collect()
}

/// The four statistics and the largest difference, as a report line.
pub(crate) fn figures(s: &Statistics) -> String {
    format!(
        "mean {:.4} worst block {:.4} p99 {:.4} mean dL* {:+.4} max {:.4}",
        s.mean, s.worst_block, s.p99, s.mean_delta_l, s.max
    )
}

/// The worst of each statistic over several cases, the signed ΔL* by its magnitude.
pub(crate) fn worst(statistics: &[Statistics]) -> Statistics {
    let largest = |of: fn(&Statistics) -> f64| statistics.iter().map(of).fold(0.0, f64::max);
    let delta_l = statistics
        .iter()
        .map(|s| s.mean_delta_l)
        .fold(0.0, |a: f64, b| if b.abs() > a.abs() { b } else { a });
    Statistics {
        pixels: statistics.iter().map(|s| s.pixels).sum(),
        mean: largest(|s| s.mean),
        worst_block: largest(|s| s.worst_block),
        worst_block_origin: [0, 0],
        p99: largest(|s| s.p99),
        mean_delta_l: delta_l,
        max: largest(|s| s.max),
    }
}

// ---- The corpus -------------------------------------------------------------------------------

/// The Fit bounds of an evidence run's window, 1440 × 900 logical at 2× with both panels open,
/// as the desktop computes them ([`crate::app::Editor::proxy_bounds`]).
pub(crate) fn fit_bounds() -> luxforge_core::ProxyBounds {
    let surface = crate::layout::photo_surface((1440.0, 900.0), true, true, false);
    let inset = crate::layout::FIT_INSET;
    crate::app::preview::bounds_of(((surface.0 - inset.0) * 2.0, (surface.1 - inset.1) * 2.0))
        .expect("room for a photograph")
}

/// The size of `evaluation`'s frame at Fit in an evidence run's window ([`fit_bounds`]), as the
/// editor plans its picture at rest there ([`luxforge_core::qualification::rest_plan`]): its tiles'
/// reduction to the view, or, where the view draws the output stage at its own size or the tiles
/// are not drawn, the view plan's output stage.
pub(crate) fn fit_size(evaluation: &luxforge_core::Evaluation) -> Result<(u32, u32), String> {
    let rest = luxforge_core::qualification::rest_plan(
        evaluation,
        luxforge_core::GpuView::Fit(fit_bounds()),
    )
    .map_err(|error| error.to_string())?;
    if let Some(Ok(tiles)) = &rest.tiles {
        return Ok(tiles
            .reduction
            .as_ref()
            .map_or((tiles.output.width, tiles.output.height), |reduction| {
                reduction.view
            }));
    }
    match rest.view.answer {
        GpuAnswer::Plan(plan) => {
            let output = plan.geometry.output();
            Ok((output.width, output.height))
        }
        GpuAnswer::Fallback(reason) => Err(format!(
            "{}: the GPU does not plan this stack at Fit",
            reason.code()
        )),
    }
}

/// One corpus cell's photograph: its file and the corpus's id for it.
pub(crate) struct CorpusSource {
    pub(crate) id: String,
    pub(crate) path: std::path::PathBuf,
    pub(crate) raw: bool,
}

/// The corpus's sources this host has: the generated JPEGs under `generated`, and the RAWs the
/// private manifest at `manifest` resolves, when it is given. A source without a file here is
/// named and skipped.
pub(crate) fn corpus_sources(
    corpus: &Value,
    generated: &std::path::Path,
    manifest: Option<&Value>,
) -> Vec<CorpusSource> {
    let mut found = Vec::new();
    for source in corpus["sources"].as_array().expect("sources") {
        let id = source["id"].as_str().expect("an id").to_owned();
        let path = match source["kind"].as_str() {
            Some("generated-jpeg") => source["path"]
                .as_str()
                .and_then(|path| std::path::Path::new(path).file_name())
                .map(|name| generated.join(name)),
            Some("raw") => manifest.and_then(|manifest| {
                manifest["sources"]
                    .as_array()?
                    .iter()
                    .find(|entry| entry["id"] == source["manifest_id"])
                    .and_then(|entry| entry["path"].as_str())
                    .map(std::path::PathBuf::from)
            }),
            _ => None,
        };
        match path.filter(|path| path.is_file()) {
            Some(path) => found.push(CorpusSource {
                raw: source["kind"] == "raw",
                id,
                path,
            }),
            None => eprintln!("gap: {id} has no file on this host"),
        }
    }
    found
}

/// The note on a cell whose first mask selects nothing on its source.
pub(crate) const EMPTY_MASK: &str =
    "the mask selects nothing on this source, so the cell says nothing about the mask's coverage";

/// A step's `mask` and `component` parameters given as `{"name": ...}`, as the corpus's evidence
/// script names them, replaced by the identities the asset's recipe holds under those names.
fn resolve_names(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    asset: &Value,
    params: &mut Value,
) -> Result<(), String> {
    let named = |key: &str| params.get(key).and_then(|value| value["name"].as_str());
    if named("mask").is_none() && named("component").is_none() {
        return Ok(());
    }
    let recipe = luxforge_testkit::client::recipe(owner, client, asset)?;
    let mask = match named("mask") {
        Some(name) => Some(
            recipe
                .masks
                .iter()
                .find(|mask| mask.name == name)
                .ok_or_else(|| format!("no mask is named {name}"))?,
        ),
        None => None,
    };
    if let Some(name) = named("component") {
        let mask = mask.ok_or("a component named by name needs its mask named")?;
        let component = mask
            .components
            .iter()
            .find(|component| component.name == name)
            .ok_or_else(|| format!("{} holds no component named {name}", mask.name))?;
        params["component"] = json!(component.id.as_str());
    }
    if let Some(mask) = mask {
        params["mask"] = json!(mask.id.as_str());
    }
    Ok(())
}

/// A query choice's Apply, as the section's suggestion card sends it (`query_choice.rs`): the
/// action's declared query asked over the photograph's current entry, waiting while the module's
/// index loads, then the action called with the suggestion's key and its own parameters.
fn apply_suggestion(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    asset: &Value,
    controls: &Value,
) -> Result<(), String> {
    use luxforge_testkit::client::{call, mutation, request_id, revision, state};
    if controls["gesture"] != "query-choice-apply" {
        return Err(format!("{controls} is not a gesture the corpus applies"));
    }
    let action = controls["action"]
        .as_str()
        .ok_or("a query choice names its action")?;
    let mut listed = call(owner, client, "module.list", json!({}))?;
    let modules: Vec<luxforge_core::ModuleDescriptor> =
        serde_json::from_value(listed["modules"].take()).map_err(|error| error.to_string())?;
    let control = super::query_choice::declaration(&modules, action)
        .ok_or_else(|| format!("no module declares a query choice for {action}"))?;
    let entry = state(owner, client, asset)?["current_entry"]["id"].clone();
    let mut query = serde_json::Map::new();
    query.insert("asset_id".into(), asset.clone());
    query.insert("entry_id".into(), entry);
    query.insert(control.text.clone(), json!(""));
    query.insert(control.page.clone(), json!(0));
    let method = format!("query.{}", control.query);
    let answer = luxforge_testbase::try_wait_for(&format!("{method} to answer"), || {
        match call(owner, client, &method, Value::Object(query.clone())) {
            Err(error) if error.contains("not-ready") => None,
            answer => Some(answer),
        }
    })??;
    let suggestion = &answer["status"]["suggestion"];
    if suggestion["eligible"] != true {
        return Err(format!("{method} suggests nothing eligible: {answer}"));
    }
    let mut params = suggestion["parameters"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    params.insert(control.key.clone(), suggestion["key"].clone());
    params.insert("asset_id".into(), asset.clone());
    params.insert(
        "mutation".into(),
        mutation(
            revision(owner, client, asset)?,
            &request_id("corpus"),
            "agent",
        ),
    );
    call(
        owner,
        client,
        &format!("edit.{action}"),
        Value::Object(params),
    )?;
    Ok(())
}

/// `steps`, the corpus recipe's evidence-script steps, applied through the API to `asset`, a mask
/// or component named by name resolved to its identity. A `section` step only opens a panel and
/// changes no recipe, so it is passed over; a query choice's Apply selects the suggestion its query
/// offers ([`apply_suggestion`]).
pub(crate) fn apply_steps(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    asset: &luxforge_core::AssetId,
    steps: &[Value],
) -> Result<(), String> {
    use luxforge_testkit::client::{call, mutation, request_id, revision};
    let id = json!(asset.as_str());
    for step in steps {
        if step.get("section").is_some() {
            continue;
        }
        if let Some(controls) = step.get("controls") {
            apply_suggestion(owner, client, &id, controls)?;
            continue;
        }
        let Some(api) = step.get("api") else {
            return Err(format!("{step} is not an API step"));
        };
        let mut params = api["params"].clone();
        resolve_names(owner, client, &id, &mut params)?;
        params["asset_id"] = id.clone();
        params["mutation"] = mutation(
            revision(owner, client, &id)?,
            &request_id("corpus"),
            "agent",
        );
        call(
            owner,
            client,
            api["method"].as_str().expect("a method"),
            params,
        )?;
    }
    Ok(())
}

/// A corpus cell's photograph imported into a catalog of its own, its recipe's steps applied: what
/// a cell's CPU and GPU frames are rendered from, at one view or several. Dropping it stops the
/// catalog owner and removes the catalog.
pub(crate) struct Opened {
    pub(crate) owner: luxforge_core::OwnerHandle,
    join: Option<std::thread::JoinHandle<()>>,
    pub(crate) client: luxforge_core::ClientId,
    pub(crate) asset: luxforge_core::AssetId,
    /// Whether the photograph is a RAW, whose frames are the linear path's.
    pub(crate) raw: bool,
    catalog: std::path::PathBuf,
}

impl Opened {
    /// `source` imported into a new catalog at `catalog` and `steps` applied to it.
    pub(crate) fn new(
        source: &CorpusSource,
        steps: &[Value],
        catalog: &std::path::Path,
    ) -> Result<Self, String> {
        let (owner, join) =
            luxforge_core::OwnerHandle::start(catalog).map_err(|error| error.to_string())?;
        let client = owner.register();
        let asset = crate::app::testing::import_and_adopt(&owner, client, &source.path);
        let opened = Self {
            owner,
            join: Some(join),
            client,
            asset,
            raw: source.raw,
            catalog: catalog.to_path_buf(),
        };
        apply_steps(&opened.owner, opened.client, &opened.asset, steps)?;
        Ok(opened)
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.catalog.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The index of `recipe`'s first layer that processes pixels: restoration, colour, spatial or
/// finish.
pub(crate) fn first_pixel_layer(
    registry: &luxforge_core::ModuleRegistry,
    recipe: &luxforge_core::Recipe,
) -> Result<usize, String> {
    use luxforge_core::EffectStage;
    recipe
        .layers
        .iter()
        .position(|layer| {
            matches!(
                registry
                    .effect(&layer.effect_id)
                    .map(|(_, effect)| effect.stage),
                Some(
                    EffectStage::Restoration
                        | EffectStage::Color
                        | EffectStage::Spatial
                        | EffectStage::Finish
                )
            )
        })
        .ok_or_else(|| "no layer that processes pixels".to_owned())
}

/// The window an evidence-sized photograph is largest in on the owner's M4 MacBook Pro: its whole
/// 3024 × 1964 display, 1512 × 982 logical at 2×, with both panels closed.
pub(crate) const LARGEST_WINDOW: (f32, f32) = (1512.0, 982.0);

/// The visible region of an output stage of `stage` at `zoom` percent in [`LARGEST_WINDOW`],
/// scrolled to its centre, as the desktop computes it ([`crate::app::preview::viewport_rect`]).
pub(crate) fn largest_view(stage: (u32, u32), zoom: f32) -> Option<luxforge_core::Region> {
    let surface = crate::layout::photo_surface(LARGEST_WINDOW, false, false, false);
    // Logical pixels an output pixel takes at 2×.
    let scale = zoom / 100.0 / 2.0;
    let pan = (
        ((stage.0 as f32 * scale - surface.0) / 2.0).max(0.0),
        ((stage.1 as f32 * scale - surface.1) / 2.0).max(0.0),
    );
    crate::app::preview::viewport_rect(
        stage,
        &luxforge_core::Zoom::Percent { value: zoom },
        2.0,
        surface,
        pan,
    )
}

/// `rgb`, a `width × height` frame of three bytes a pixel, written as `output/<name>.png`.
pub(crate) fn write_png(
    output: &std::path::Path,
    name: &str,
    (width, height): (u32, u32),
    rgb: &[u8],
) -> Result<(), String> {
    image::RgbImage::from_raw(width, height, rgb.to_vec())
        .ok_or("a whole frame")?
        .save(output.join(format!("{name}.png")))
        .map_err(|error| error.to_string())
}

/// The lights `plan` reads, each computed from `source` as the photo surface's own light link
/// computes it on `qualifier`'s device (`Qualifier::light_bench`), read back, and given to
/// `qualifier` for its next evaluations (`Qualifier::set_lights`): the light planes a slot's light
/// links write before its chain runs. Each light is computed once for its source and steps and
/// kept for the harness's run, at most [`LIT`] of them.
pub(crate) fn lit(
    qualifier: &Qualifier,
    source: &luxforge_core::PreviewSource,
    plan: &GpuPlan,
) -> Result<(), String> {
    use std::hash::{Hash, Hasher};
    static KEPT: std::sync::Mutex<Vec<(u64, [f32; 4])>> = std::sync::Mutex::new(Vec::new());
    let (width, height) = source.dimensions();
    let identity = {
        let mut hasher = std::hash::DefaultHasher::new();
        (format!("{:?}", source.identity()), width, height).hash(&mut hasher);
        hasher.finish()
    };
    let mut lights = Vec::with_capacity(plan.lights.len());
    for light in &plan.lights {
        let key = {
            let mut hasher = std::hash::DefaultHasher::new();
            identity.hash(&mut hasher);
            format!("{:?}", light.steps).hash(&mut hasher);
            hasher.finish()
        };
        let kept = KEPT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(held, _)| *held == key)
            .map(|(_, light)| *light);
        let value = match kept {
            Some(value) => value,
            None => {
                let gpu = super::gpu_preview::gpu_source_of(identity, source)
                    .ok_or("the source as the surface holds it")?;
                let value = qualifier
                    .light_bench()
                    .light(&gpu, light)
                    .map_err(|fallback| format!("the light: {fallback:?}"))?
                    .light;
                let mut kept = KEPT
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if kept.len() >= LIT {
                    kept.remove(0);
                }
                kept.push((key, value));
                value
            }
        };
        lights.push(value);
    }
    qualifier.set_lights(lights);
    Ok(())
}

/// The most lights [`lit`] keeps across cells, the oldest let go first.
const LIT: usize = 256;

/// The light [`lit_fixed`] gives every light plane: a plausible atmospheric light, `[r, g, b, 1]`.
pub(crate) const FIXED_LIGHT: [f32; 4] = [0.82, 0.86, 0.91, 1.0];

/// [`FIXED_LIGHT`] for every light `plan` reads, given to `qualifier`: for a test that holds GPU
/// frames to one another, never to the CPU's, where any light the planes hold is the one both
/// read.
pub(crate) fn lit_fixed(qualifier: &Qualifier, plan: &GpuPlan) {
    qualifier.set_lights(vec![FIXED_LIGHT; plan.lights.len()]);
}

/// A headless qualifier, with the core's output encoding installed for its passes. `None`, having
/// printed that `test` was skipped, without an adapter.
pub(crate) fn headless(test: &str) -> Option<Qualifier> {
    assert!(
        super::gpu_plan::install_output_encoding(),
        "the surface holds the core's output encoding"
    );
    Qualifier::headless(test)
}

/// How many texels of `drawn` differ from `whole`'s in the bits of a colour channel. Bits, not a
/// largest difference of values, which a NaN would leave out of a maximum.
pub(crate) fn differing(drawn: &[[f32; 4]], whole: &[[f32; 4]]) -> usize {
    assert_eq!(drawn.len(), whole.len(), "two frames of one size");
    drawn
        .iter()
        .zip(whole)
        .filter(|(drawn, whole)| {
            (0..3).any(|channel| drawn[channel].to_bits() != whole[channel].to_bits())
        })
        .count()
}

/// `ticks` drawn as one slot draws a gesture's ([`Qualifier::evaluate_sequence`]), every tick's
/// frame held bit for bit to `whole`'s, the frame a whole evaluation of the same plan draws; then
/// drawn again with the poison on ([`Qualifier::set_poison`]), every link's passes starting from NaN
/// in every texture of its pool, and held to the same frames, which shows that no pass read scratch
/// beyond the cone its unit's reach bounds. Answers each tick's passes without the poison, link by
/// link, `name` naming a tick in a failure.
pub(crate) fn held_to_whole(
    qualifier: &Qualifier,
    ticks: &[(GpuPlan, Option<[u32; 4]>)],
    whole: &[Vec<[f32; 4]>],
    name: &dyn Fn(usize) -> String,
) -> Vec<Vec<u64>> {
    let mut passes = Vec::new();
    let mut totals = [0; 2];
    for poisoned in [false, true] {
        qualifier.set_poison(poisoned);
        let drawn = qualifier.evaluate_sequence(ticks);
        qualifier.set_poison(false);
        for (number, (tick, whole)) in drawn
            .expect("a readback of every tick")
            .iter()
            .zip(whole)
            .enumerate()
        {
            let differ = differing(&tick.output, whole);
            assert_eq!(
                differ,
                0,
                "{}, poisoned {poisoned}: texels that differ from the whole evaluation",
                name(number)
            );
            totals[usize::from(poisoned)] += tick.ran();
            if !poisoned {
                passes.push(tick.passes.clone());
            }
        }
    }
    // The poison forgets every record, so a tick reuses no scratch: at least as many passes.
    eprintln!(
        "{} ticks bit for bit: {} passes, {} with the poison",
        ticks.len(),
        totals[0],
        totals[1]
    );
    assert!(totals[1] >= totals[0], "the poison reuses no scratch");
    passes
}

/// What a stack's first layer draws in its drafted GPU shape against its CPU shape's plan, over a
/// linear boundary of `pixels`.
#[derive(Debug)]
pub(crate) struct Drafted {
    /// The drafted shape's output values, bit for bit the boundary's.
    pub(crate) input: bool,
    /// The linear values that differ from the CPU shape's plan's, of how many, and the largest
    /// difference.
    pub(crate) values: usize,
    pub(crate) of: usize,
    pub(crate) largest: f32,
    /// The largest difference in the codes the two draw.
    pub(crate) code: u8,
}

/// [`Drafted`] for `recipe`'s first layer, planned for `request` (on the linear path), over a
/// `width × height` boundary of `pixels`.
pub(crate) fn drafted_against_cpu(
    qualifier: &Qualifier,
    recipe: &luxforge_core::Recipe,
    request: GpuPlanRequest,
    (width, height): (u32, u32),
    pixels: &[[f32; 3]],
) -> Drafted {
    let registry = luxforge_core::ModuleRegistry::builtin();
    let draw = |request: GpuPlanRequest| {
        let plan = match gpu_plan(&registry, recipe, request).expect("the stack compiles") {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
        let input = boundary(width, height, 1, pixels).expect("a boundary");
        let plan = super::gpu_plan::surface_plan(&plan, input).expect("a runnable plan");
        // Both shapes read one light, whichever it is: neither is held to the CPU's here.
        lit_fixed(qualifier, &plan);
        (
            qualifier.evaluate(&plan).expect("a readback"),
            qualifier.evaluate_codes(&plan).expect("a readback"),
        )
    };
    let (drafted, drafted_codes) = draw(request.drafted(0));
    let (cpu, cpu_codes) = draw(request);
    let values = |texels: &[[f32; 4]]| -> Vec<f32> {
        texels
            .iter()
            .flat_map(|texel| texel[..3].to_vec())
            .collect()
    };
    let (drafted, cpu) = (values(&drafted), values(&cpu));
    let pairs = || drafted.iter().zip(&cpu);
    Drafted {
        input: drafted
            .iter()
            .zip(pixels.iter().flatten())
            .all(|(drawn, given)| drawn.to_bits() == given.to_bits()),
        values: pairs().filter(|(a, b)| a.to_bits() != b.to_bits()).count(),
        of: drafted.len(),
        largest: pairs().map(|(a, b)| (a - b).abs()).fold(0.0, f32::max),
        code: drafted_codes
            .iter()
            .zip(&cpu_codes)
            .flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c])))
            .max()
            .unwrap_or(0),
    }
}
