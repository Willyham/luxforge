//! The GPU preview qualification harness the program tests share (`docs/design/gpu-preview.md`,
//! "Qualifying a program"): a deterministic synthetic grid of linear pixels, the report helpers,
//! and the corpus at Fit, which draws a recipe's CPU frame through the desktop's own Fit job and
//! the GPU frame of the same plan through the photo surface's own shader, and judges each pair by
//! its recipe's class. [`corpus_at_fit`] takes the families to run, so a test of any program class
//! runs the same corpus with its own.
//!
//! [`reference`] is the release gate's harness: the same cells at every view the corpus lists, each
//! GPU frame against the reference renderer's whole frame at the view's size.
use super::gpu_plan::{WarpGrid, surface_plan_at};
use luxforge_core::{CompileStage, GpuAnswer, GpuPlanRequest, Layer, Processing, Stage, gpu_plan};
use luxforge_reference::{
    preview_error::{self, Class, Rgb8, Statistics},
    srgb,
};
use luxforge_ui::photo_surface::{
    BoundaryFormat, GpuPlan, GpuProgram, GpuStep, MaskedColour, TexelMap,
    gpu_preview::qualification::{Qualifier, boundary, boundary_as, held},
};
use serde_json::{Value, json};

mod reference;

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

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

// ---- The corpus at Fit ------------------------------------------------------------------------

/// The Fit bounds of an evidence run's window, 1440 × 900 logical at 2× with both panels open,
/// as the desktop computes them ([`crate::app::Editor::proxy_bounds`]).
pub(crate) fn fit_bounds() -> luxforge_core::ProxyBounds {
    let surface = crate::layout::photo_surface((1440.0, 900.0), true, true);
    let inset = crate::layout::FIT_INSET;
    crate::app::preview::bounds_of(((surface.0 - inset.0) * 2.0, (surface.1 - inset.1) * 2.0))
        .expect("room for a photograph")
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

/// What one cell measured, or why it has no figures. A cell is built once and read at once, so its
/// figures are not boxed to make a gap smaller.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Cell {
    Measured {
        /// The stage the Fit frame was rendered at, and whether it was a proxy; at a percentage
        /// zoom the region's size.
        stage: (u32, u32),
        proxy: bool,
        /// At a percentage zoom, the visible region of the output stage both frames hold.
        region: Option<luxforge_core::Region>,
        /// The codes the stage draws against the CPU's: the figures the limits judge.
        statistics: Statistics,
        /// The GPU's `f32` output through the reference quantizer against the CPU's, which leaves
        /// the hardware encoder's rounding out.
        program: Statistics,
        passed: bool,
        /// What the photo surface's slot drawing the plan charges the GPU-preview budget.
        charged: u64,
        /// At a percentage zoom, the shape a restoration or spatial layer is drawn in: `gpu`,
        /// every unit, or `cpu`, the units its values need, when only that one fits the budget.
        shape: Option<&'static str>,
        /// The codes the stage draws and the CPU frame's, three bytes a pixel, row by row, at
        /// `stage`.
        gpu: Vec<u8>,
        cpu: Vec<u8>,
        /// What the figures cannot say, such as a mask that selects nothing on the source.
        notes: Vec<String>,
    },
    Gap(String),
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

/// What a cell writes and how it treats what it cannot judge.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CellOptions<'a> {
    /// The name the cell's frames are written under as PNGs in the output directory, or `None`
    /// to write none.
    pub(crate) frames: Option<&'a str>,
    /// Whether a cell whose first mask selects nothing on its source is measured, with a note,
    /// rather than named a gap: its picture is still a picture, though it says nothing about the
    /// mask's coverage.
    pub(crate) empty_mask: bool,
}

impl<'a> CellOptions<'a> {
    /// The program qualification's cells: frames written under `name`, and a mask that selects
    /// nothing a gap.
    pub(crate) fn qualifying(name: &'a str) -> Self {
        Self {
            frames: Some(name),
            empty_mask: false,
        }
    }
}

/// The CPU Fit frame and the GPU frame of one recipe on one source, both written as PNGs in
/// `output` under `name`, and their figures held to `class`'s limits.
pub(crate) fn corpus_cell(
    qualifier: &Qualifier,
    source: &CorpusSource,
    steps: &[Value],
    output: &std::path::Path,
    name: &str,
    class: Class,
) -> Result<Cell, String> {
    let opened = Opened::new(source, steps, &output.join(format!("{name}.sqlite")))?;
    proxy_cell(
        qualifier,
        &opened,
        fit_bounds(),
        output,
        CellOptions::qualifying(name),
        class,
    )
}

/// The CPU frame the preview worker renders at `bounds` and the GPU frame of the same plan over
/// the same proxy, of `opened`'s stack, their figures held to `class`'s limits: the Fit view, and a
/// percentage below 100%, whose displayed-size proxy is planned as Fit's is with other bounds.
pub(crate) fn proxy_cell(
    qualifier: &Qualifier,
    opened: &Opened,
    bounds: luxforge_core::ProxyBounds,
    output: &std::path::Path,
    options: CellOptions<'_>,
    class: Class,
) -> Result<Cell, String> {
    use luxforge_core::{
        Cancel, PhaseOutcome, PreviewIntent, PreviewQueue, PreviewRequest, PreviewSource,
        RenderContext, RenderOptions, render,
    };
    let (owner, client, asset) = (&opened.owner, opened.client, &opened.asset);
    {
        // The CPU frame: the desktop's own moving Fit frame, through the preview worker's proxy
        // phase, the frame a gesture shows on the CPU path.
        let mut job = crate::app::tasks::ready_preview_job(
            owner,
            PreviewRequest::new(client, asset.clone()).proxy(bounds),
        )?;
        job.intent = PreviewIntent::Interactive;
        let evaluation = job.evaluation.clone();
        // The recipe the frame is rendered from, its painted strokes resolved.
        let recipe = evaluation.recipe().clone();
        let mut queue = PreviewQueue::default();
        let generation = queue.request(job);
        let outcome = luxforge_testbase::wait_for("the Fit frame", || {
            let result = queue.poll()?;
            (result.generation == generation).then_some(result.outcome)
        });
        let registry = evaluation.registry().clone();
        let full = evaluation.source().dimensions();
        // The Fit frame and the source it was rendered from: the proxy phase's frame over the
        // proxy the worker built — a window of the proxy stage when the stack reads less than all
        // of it, as a crop does — or, for a photograph that fits the bounds at its own size, the
        // exact phase's frame over the source itself. With the stage the plan addresses and where
        // in it the source's first texel is.
        let (cpu, proxied, is_proxy, (stage_width, stage_height), origin) = match outcome {
            PhaseOutcome::Proxy(proxy) => {
                let context = RenderContext::new();
                let exact = render(
                    &registry,
                    evaluation.source(),
                    evaluation.recipe(),
                    RenderOptions::exact(&Cancel::never()),
                    &context,
                )
                .map_err(|error| error.to_string())?;
                let (plan, window) = luxforge_core::qualification::fit_proxy(
                    &exact,
                    &registry,
                    evaluation.recipe(),
                    bounds,
                )
                .ok_or("a proxy frame without a proxy plan")?;
                let proxied = evaluation
                    .source()
                    .proxy(plan)
                    .map_err(|error| error.to_string())?;
                if proxied.dimensions() != proxy.dimensions {
                    return Ok(Cell::Gap(format!(
                        "the worker's proxy is {:?}, not this {:?}",
                        proxy.dimensions,
                        proxied.dimensions()
                    )));
                }
                let origin = window.map_or((0, 0), |[x, y, _, _]| (x, y));
                let stage = (plan.width, plan.height);
                (proxy.raster, proxied, true, stage, origin)
            }
            PhaseOutcome::Exact(exact) => (
                exact.result.map_err(|error| error.to_string())?,
                evaluation.source().clone(),
                false,
                full,
                (0, 0),
            ),
            PhaseOutcome::Region(_) => return Ok(Cell::Gap("a region at Fit".into())),
        };
        let (width, height) = proxied.dimensions();
        // The boundary: the input of the stack's first layer that processes pixels. Where every
        // layer before it is the identity over this source, that is the source the frame was
        // rendered from; where one is not, a lens warp before a finishing layer, it is that layer's
        // input at the frame's stage, which the worker's boundary job renders through the stack's
        // prefix, and so does this cell.
        let (boundary_layer, through_prefix) =
            match boundary_layer(&registry, &recipe, (width, height))? {
                Ok(layer) => (layer, false),
                Err(_) => (first_pixel_layer(&registry, &recipe)?, true),
            };
        if let PreviewSource::Raw { settings, .. } = &proxied
            && settings.white_balance.is_some()
        {
            return Ok(Cell::Gap("an approximated white balance".into()));
        }
        let texels: Vec<[f32; 3]> = match &proxied {
            _ if through_prefix => Vec::new(),
            PreviewSource::Jpeg(image) => {
                let table = luxforge_core::colour::srgb::decode_table();
                image
                    .rgba
                    .chunks_exact(4)
                    .map(|pixel| [0, 1, 2].map(|c| table[usize::from(pixel[c])]))
                    .collect()
            }
            PreviewSource::Raw { image, .. } => (0..height)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .map(|(x, y)| image.pixel(x, y).expect("a viewed pixel"))
                .collect(),
        };
        let request = if is_proxy {
            GpuPlanRequest::fit(
                boundary_layer,
                stage(stage_width, stage_height),
                stage(full.0, full.1),
            )
        } else {
            GpuPlanRequest::exact(boundary_layer, stage(stage_width, stage_height))
        }
        .qualifying();
        // A RAW photograph's frames are the linear path's, which clamps no stage boundary.
        let request = match &proxied {
            PreviewSource::Raw { .. } => request.linear(),
            PreviewSource::Jpeg(_) => request,
        };
        // A windowed proxy's spatial operations are handed the exact stage's estimates, which the
        // job's exact phase stored: the plan reads them, as a drag's does once its committed frame
        // is drawn. Any other proxy takes them over the stage it holds, as its CPU frame does.
        let estimates = (origin != (0, 0) || (width, height) != (stage_width, stage_height))
            .then(|| luxforge_core::GpuEstimates {
                context: evaluation.context(),
                source: luxforge_core::EstimateSource::Whole {
                    source: evaluation.source().into(),
                    stage: stage(full.0, full.1),
                },
            })
            .filter(|_| is_proxy);
        let plan = match luxforge_core::gpu_plan_with(&registry, &recipe, request, estimates)
            .map_err(|e| e.to_string())?
        {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => {
                return Ok(Cell::Gap(format!("{}: {reason}", reason.code())));
            }
        };
        // A RAW's boundary holds its values as `f32`, as the worker's boundary job writes it.
        let format = match &proxied {
            PreviewSource::Raw { .. } => BoundaryFormat::Float,
            PreviewSource::Jpeg(_) => BoundaryFormat::Half,
        };
        // The frame is the plan's output stage: the boundary's, or its geometry tail's.
        let output_stage = plan.geometry.output();
        let (out_width, out_height) = (output_stage.width, output_stage.height);
        if (cpu.width, cpu.height) != (out_width, out_height) {
            return Ok(Cell::Gap(format!(
                "the frame is {}x{}, not the plan's output {out_width}x{out_height}",
                cpu.width, cpu.height
            )));
        }
        // At the exact stage a Fit drag's boundary holds the window of its stage the whole output
        // reads, as the worker renders it, unless the plan takes a global estimate on the GPU,
        // which keeps the whole stage; a proxy's is the window the proxy source holds. One past
        // the bound on a boundary is the CPU path, as the desktop finds before it asks.
        let core_format = match format {
            BoundaryFormat::Float => luxforge_core::BoundaryFormat::Float,
            BoundaryFormat::Half => luxforge_core::BoundaryFormat::Half,
        };
        let (held, origin) = if through_prefix
            || (!is_proxy && !plan.spatial.iter().any(|spatial| spatial.estimated))
        {
            let context = RenderContext::new();
            let exact = render(
                &registry,
                evaluation.source(),
                evaluation.recipe(),
                RenderOptions::exact(&Cancel::never()),
                &context,
            )
            .map_err(|error| error.to_string())?;
            // Through the prefix at the proxy stage, the boundary the worker's proxy phase reads
            // from its own render; at the exact stage, the exact render's.
            let frame = if is_proxy {
                luxforge_core::qualification::proxy_boundary(
                    &exact,
                    &registry,
                    evaluation.recipe(),
                    bounds,
                    (&proxied).into(),
                    boundary_layer,
                    core_format,
                )
            } else {
                luxforge_core::qualification::region_boundary(
                    &exact,
                    boundary_layer,
                    [0, 0, out_width, out_height],
                    core_format,
                )
            };
            let frame = match frame {
                Ok(frame) => frame,
                Err(error) if error.kind == luxforge_core::ErrorKind::ResourceLimit => {
                    return Ok(Cell::Gap(format!(
                        "budget-exceeded: {}, so the drag takes the CPU path",
                        error.detail
                    )));
                }
                Err(error) => return Err(error.to_string()),
            };
            let held = luxforge_ui::photo_surface::GpuBoundary::new(
                frame.texels.clone(),
                frame.width,
                frame.height,
                1,
                format,
            )
            .ok_or("a boundary")?;
            (held, frame.origin)
        } else {
            let bytes = u64::from(width) * u64::from(height) * core_format.texel_bytes() as u64;
            if bytes > luxforge_core::BOUNDARY_MAX_BYTES {
                return Ok(Cell::Gap(format!(
                    "budget-exceeded: a {width}x{height} boundary of {bytes} B passes the {} B \
                     bound on one, so the drag takes the CPU path",
                    luxforge_core::BOUNDARY_MAX_BYTES
                )));
            }
            let held = boundary_as(format, width, height, 1, &texels).ok_or("a boundary")?;
            (held, origin)
        };
        // A lens warp's coordinate grid over the whole output stage, as the boundary's job
        // computes it; none for an affine or perspective tail, which the surface evaluates exactly.
        let grid = plan
            .geometry
            .grid(
                luxforge_core::Region {
                    x0: 0,
                    y0: 0,
                    width: out_width,
                    height: out_height,
                },
                1.0,
            )
            .map_err(|error| error.to_string())?
            .map(|grid| WarpGrid::new(&grid));
        let converted = match surface_plan_at(&plan, held, origin, grid.as_ref()) {
            Ok(converted) => converted,
            Err(reason) => {
                return Ok(Cell::Gap(format!(
                    "{}: the surface cannot run {reason:?} yet",
                    reason.code()
                )));
            }
        };
        // A mask that selects nothing on this source measures nothing about its coverage: the
        // program qualification names such a cell a gap, not a pass. Its first operation's mask is
        // read back over the boundary it reads.
        let mut notes = Vec::new();
        if let Some(GpuStep::Masked(masked)) = converted.steps.first()
            && selects_nothing(qualifier, &converted.boundary, masked)?
        {
            if !options.empty_mask {
                return Ok(Cell::Gap(
                    "the mask selects nothing on this source".to_owned(),
                ));
            }
            notes.push(EMPTY_MASK.to_owned());
        }
        let charged = qualifier
            .charged_bytes(&converted)
            .map_err(|reason| format!("{reason:?}"))?;
        let (held_width, held_height) = converted.boundary.size();
        eprintln!(
            "{}: boundary {held_width}x{held_height} at {origin:?} of {}x{}, {} B",
            options.frames.unwrap_or("the cell"),
            plan.boundary.stage.width,
            plan.boundary.stage.height,
            u64::from(held_width) * u64::from(held_height) * core_format.texel_bytes() as u64
        );
        let budget = luxforge_ui::photo_surface::gpu_preview::GPU_PREVIEW_BUDGET;
        if charged > budget {
            return Ok(Cell::Gap(format!(
                "budget-exceeded: the slot over a {}x{} boundary would charge {charged} B of the \
                 {budget} B GPU-preview budget, so the drag takes the CPU path",
                converted.boundary.size().0,
                converted.boundary.size().1
            )));
        }
        let drawn = qualifier.evaluate_codes(&converted)?;
        let gpu: Vec<u8> = drawn
            .iter()
            .flat_map(|code| [code[0], code[1], code[2]])
            .collect();
        let program = codes(
            qualifier
                .evaluate(&converted)?
                .iter()
                .map(|texel| [texel[0], texel[1], texel[2]]),
        );
        let reference: Vec<u8> = cpu
            .rgba
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        let (width, height) = (out_width, out_height);
        if let Some(name) = options.frames {
            for (suffix, bytes) in [("gpu", &gpu), ("cpu", &reference)] {
                write_png(output, &format!("{name}-{suffix}"), (width, height), bytes)?;
            }
        }
        let frame = |bytes| Rgb8::new(width, height, bytes);
        let statistics =
            preview_error::compare(frame(&gpu)?, frame(&reference)?, [0, 0, width, height])?;
        let program =
            preview_error::compare(frame(&program)?, frame(&reference)?, [0, 0, width, height])?;
        Ok(Cell::Measured {
            stage: (width, height),
            proxy: is_proxy,
            region: None,
            passed: preview_error::verdict(&statistics, class).passed(),
            statistics,
            program,
            charged,
            shape: None,
            gpu,
            cpu: reference,
            notes,
        })
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

/// The boundary of `recipe` over a source of `size`: the input of the first layer that processes
/// pixels (restoration, colour, spatial or finish), which is the source the frame was rendered from
/// when every layer before it compiles to the identity there, as a RAW development does. A geometry
/// layer before a content layer is part of the plan's geometry tail, which the CPU runs after the
/// content operations, so it does not change the boundary; before a finishing layer, which runs
/// after the tail, it does, and only the worker's boundary job holds that input. `Err` inside names
/// the gap.
pub(crate) fn boundary_layer(
    registry: &luxforge_core::ModuleRegistry,
    recipe: &luxforge_core::Recipe,
    (width, height): (u32, u32),
) -> Result<Result<usize, String>, String> {
    use luxforge_core::EffectStage;
    let stage_of = |layer: &Layer| registry.effect(&layer.effect_id).map(|(_, e)| e.stage);
    let boundary_layer = recipe
        .layers
        .iter()
        .position(|layer| {
            matches!(
                stage_of(layer),
                Some(
                    EffectStage::Restoration
                        | EffectStage::Color
                        | EffectStage::Spatial
                        | EffectStage::Finish
                )
            )
        })
        .ok_or("no layer that processes pixels")?;
    let content = stage_of(&recipe.layers[boundary_layer]) != Some(EffectStage::Finish);
    for layer in &recipe.layers[..boundary_layer] {
        if content && stage_of(layer) == Some(EffectStage::Geometry) {
            continue;
        }
        let (module, _) = registry
            .effect(&layer.effect_id)
            .ok_or("an unknown effect")?;
        let compiled = module
            .compile(
                &layer.effect_id,
                layer.effect_format,
                &layer.payload,
                CompileStage::exact(stage(width, height)),
            )
            .map_err(|error| error.to_string())?;
        let identity = matches!(
            compiled,
            Processing::ExactGeometry(g)
                if (g.a, g.b, g.c, g.d, g.tx, g.ty) == (1, 0, 0, 1, 0, 0)
                    && (g.output_width, g.output_height) == (width, height)
        );
        if !identity {
            return Ok(Err(format!(
                "{} before the boundary is not the identity",
                layer.effect_id
            )));
        }
    }
    Ok(Ok(boundary_layer))
}

/// The window an evidence-sized photograph is largest in on the owner's M4 MacBook Pro: its whole
/// 3024 × 1964 display, 1512 × 982 logical at 2×, with both panels closed.
pub(crate) const LARGEST_WINDOW: (f32, f32) = (1512.0, 982.0);

/// The visible region of an output stage of `stage` at `zoom` percent in [`LARGEST_WINDOW`],
/// scrolled to its centre, as the desktop computes it ([`crate::app::preview::viewport_rect`]).
pub(crate) fn largest_view(stage: (u32, u32), zoom: f32) -> Option<luxforge_core::Region> {
    let surface = crate::layout::photo_surface(LARGEST_WINDOW, false, false);
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

/// The CPU's exact visible region and the GPU frame of the same plan over the region's own
/// boundary, of one recipe on one source at `zoom` percent in [`LARGEST_WINDOW`]: the region the
/// shared quiet policy settles a percentage view to, through the preview worker's own viewport
/// job, against the plan a drag from the recipe's first pixel layer draws there, over the boundary
/// the worker renders for that region at full scale. Both written as PNGs in `output` under
/// `name`, their figures held to `class`'s limits. A plan whose slot would pass the GPU-preview
/// budget is the CPU path, a gap naming the budget.
pub(crate) fn region_cell(
    qualifier: &Qualifier,
    source: &CorpusSource,
    steps: &[Value],
    output: &std::path::Path,
    name: &str,
    class: Class,
    zoom: f32,
) -> Result<Cell, String> {
    let opened = Opened::new(source, steps, &output.join(format!("{name}.sqlite")))?;
    region_cell_in(
        qualifier,
        &opened,
        zoom,
        output,
        CellOptions::qualifying(name),
        class,
    )
}

/// [`region_cell`] of `opened`'s stack.
pub(crate) fn region_cell_in(
    qualifier: &Qualifier,
    opened: &Opened,
    zoom: f32,
    output: &std::path::Path,
    options: CellOptions<'_>,
    class: Class,
) -> Result<Cell, String> {
    use luxforge_core::{
        Cancel, PhaseOutcome, PreviewIntent, PreviewQueue, PreviewRequest, RenderContext,
        RenderOptions, render,
    };
    let (owner, client, asset) = (&opened.owner, opened.client, &opened.asset);
    let mut job =
        crate::app::tasks::ready_preview_job(owner, PreviewRequest::new(client, asset.clone()))?;
    let output_stage = (job.identity.width, job.identity.height);
    let rect = largest_view(output_stage, zoom).ok_or("no visible region")?;
    // The CPU frame: the worker's exact region of the view, as the quiet settle asks for it.
    job.viewport = Some(rect);
    job.intent = PreviewIntent::Settle;
    let evaluation = job.evaluation.clone();
    let mut queue = PreviewQueue::default();
    let generation = queue.request(job);
    // `Err` holding the whole exact frame when the worker declines the region and renders that
    // instead, which the view then draws its region from.
    let cpu = luxforge_testbase::wait_for("the exact visible region", || {
        let result = queue.poll()?;
        if result.generation != generation {
            return None;
        }
        match result.outcome {
            PhaseOutcome::Region(region) if region.frame.stage == region.frame.full_stage => {
                Some(Ok(Ok(region.frame)))
            }
            PhaseOutcome::Exact(exact) => {
                Some(exact.result.map(Err).map_err(|error| error.to_string()))
            }
            _ => None,
        }
    })?;
    if let Ok(cpu) = &cpu
        && cpu.full_rect != rect
    {
        return Ok(Cell::Gap(format!(
            "the exact region is {:?}, not the view's {rect:?}",
            cpu.full_rect
        )));
    }
    // The region's boundary, as the worker renders it for a job carrying its request.
    let context = RenderContext::new();
    let exact = render(
        evaluation.registry(),
        evaluation.source(),
        evaluation.recipe(),
        RenderOptions::exact(&Cancel::never()),
        &context,
    )
    .map_err(|error| error.to_string())?;
    let drawn = draw_region(
        qualifier,
        &evaluation,
        &exact,
        opened.raw,
        rect,
        zoom,
        options,
        true,
    )?;
    // A stack whose region the worker cannot cut, an estimate behind an earlier spatial layer,
    // is drawn from the exact whole frame at a percentage zoom: its GPU frame is measured
    // against that frame's region, the frame the view settles to. Its region plan reads the
    // estimate the whole frame stored, as a drag's does once the view has settled.
    let declined = match cpu {
        Ok(_) => None,
        Err(ref whole) => match &drawn {
            RegionDraw::NoBoundary(error) => {
                return Ok(Cell::Gap(format!(
                    "region-declined: the worker renders this stack's whole exact frame at a \
                     percentage zoom, and the region's boundary cannot be planned: {}",
                    error.detail
                )));
            }
            _ => Some(region_of(whole, rect)),
        },
    };
    let drawn = match drawn {
        RegionDraw::Drawn(drawn) => drawn,
        RegionDraw::Gap(reason) => return Ok(Cell::Gap(reason)),
        // A boundary past the bound on one, which the desktop's tick finds before it asks.
        RegionDraw::NoBoundary(error) if error.kind == luxforge_core::ErrorKind::ResourceLimit => {
            return Ok(Cell::Gap(format!(
                "budget-exceeded: {}, so the drag takes the CPU path",
                error.detail
            )));
        }
        RegionDraw::NoBoundary(error) => return Err(error.to_string()),
    };
    let (width, height) = (rect.width, rect.height);
    let raster = match (&cpu, &declined) {
        (Ok(cpu), _) => &cpu.raster,
        (Err(_), Some(region)) => region,
        (Err(_), None) => unreachable!("a declined region is measured over the whole frame's"),
    };
    let reference: Vec<u8> = raster
        .rgba
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect();
    if (raster.width, raster.height) != (width, height) {
        return Ok(Cell::Gap(format!(
            "the exact region's raster is {}x{}, not the region's {width}x{height}",
            raster.width, raster.height
        )));
    }
    if let Some(name) = options.frames {
        for (suffix, bytes) in [("gpu", &drawn.gpu), ("cpu", &reference)] {
            write_png(output, &format!("{name}-{suffix}"), (width, height), bytes)?;
        }
    }
    let frame = |bytes| Rgb8::new(width, height, bytes);
    let rect_px = [0, 0, width, height];
    let statistics = preview_error::compare(frame(&drawn.gpu)?, frame(&reference)?, rect_px)?;
    let program = drawn.program.as_deref().ok_or("the program's own output")?;
    let program = preview_error::compare(frame(program)?, frame(&reference)?, rect_px)?;
    // A spatial estimate the store does not hold, which the GPU would take from the region
    // alone, is the CPU's path at a percentage zoom (`region-estimate`): measured, so the
    // reason stands on figures.
    if drawn.approximate {
        return Ok(Cell::Gap(format!(
            "region-estimate: the GPU takes the global estimate from the region alone, so the \
             drag takes the CPU path; measured over the region: {}",
            figures(&statistics)
        )));
    }
    Ok(Cell::Measured {
        stage: (width, height),
        proxy: false,
        region: Some(rect),
        passed: preview_error::verdict(&statistics, class).passed(),
        statistics,
        program,
        charged: drawn.charged,
        shape: drawn.shape,
        gpu: drawn.gpu,
        cpu: reference,
        notes: drawn.notes,
    })
}

/// What [`draw_region`] drew of one region of the output stage, or why it drew nothing.
pub(crate) enum RegionDraw {
    /// The region's boundary could not be rendered.
    NoBoundary(luxforge_core::Error),
    /// The GPU path does not draw the region: a fallback, a plan the surface cannot run yet, a
    /// mask that selects nothing where that is a gap, or a slot past the budget, with why.
    Gap(String),
    Drawn(DrawnRegion),
}

/// A region drawn on the device.
pub(crate) struct DrawnRegion {
    /// The codes the stage draws over the region, three bytes a pixel, row by row.
    pub(crate) gpu: Vec<u8>,
    /// The plan's `f32` output through the reference quantizer, when it was asked for.
    pub(crate) program: Option<Vec<u8>>,
    /// The plan takes a global estimate from the region alone, which the store did not hold
    /// (`region-estimate`).
    pub(crate) approximate: bool,
    /// What the slot charges the GPU-preview budget, and the shape a restoration or spatial
    /// layer is drawn in.
    pub(crate) charged: u64,
    pub(crate) shape: Option<&'static str>,
    pub(crate) notes: Vec<String>,
    /// The harness's own clock: rendering the region's boundary on the CPU, and drawing and
    /// reading back its codes on the device. Not timing measurements.
    pub(crate) boundary_time: std::time::Duration,
    pub(crate) draw_time: std::time::Duration,
}

/// The plan a drag from `evaluation`'s first pixel layer draws over `rect` of its output stage at
/// `zoom` percent, drawn on `qualifier`'s device: over the boundary the worker renders for that
/// region at full scale from `exact` (a render of `evaluation`'s stack at the exact phase), reading
/// the global estimates `evaluation`'s context holds, as a drag's plan reads them once the view has
/// settled. A RAW photograph's (`linear`) plan is the linear path's over an `f32` boundary. A
/// restoration or spatial layer's drag draws its GPU shape, every unit, while that slot fits the
/// budget, and its CPU shape when only that one does (`GpuPreview::cpu_shape`). `program` also reads
/// the plan's `f32` output back.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_region(
    qualifier: &Qualifier,
    evaluation: &luxforge_core::Evaluation,
    exact: &luxforge_core::Render<'_>,
    linear: bool,
    rect: luxforge_core::Region,
    zoom: f32,
    options: CellOptions<'_>,
    program: bool,
) -> Result<RegionDraw, String> {
    let (recipe, registry) = (evaluation.recipe(), evaluation.registry());
    // The boundary: the input of the first layer that processes pixels, whatever runs before
    // it — a RAW's lens warp before its vignette included — since the worker's region boundary
    // holds that layer's own input.
    let boundary_layer = first_pixel_layer(registry, recipe)?;
    let format = if linear {
        luxforge_core::BoundaryFormat::Float
    } else {
        luxforge_core::BoundaryFormat::Half
    };
    let started = std::time::Instant::now();
    let frame = match luxforge_core::qualification::region_boundary(
        exact,
        boundary_layer,
        [rect.x0, rect.y0, rect.width, rect.height],
        format,
    ) {
        Ok(frame) => frame,
        Err(error) => return Ok(RegionDraw::NoBoundary(error)),
    };
    let boundary_time = started.elapsed();
    // The plan from that layer at the exact stage, over the whole stage the layer receives.
    let request = GpuPlanRequest::exact(boundary_layer, frame.stage).qualifying();
    let request = if linear { request.linear() } else { request };
    let estimates = luxforge_core::GpuEstimates {
        context: evaluation.context(),
        source: luxforge_core::EstimateSource::Render(evaluation.source().into()),
    };
    let spatial = matches!(
        registry
            .effect(&recipe.layers[boundary_layer].effect_id)
            .map(|(_, effect)| effect.stage),
        Some(luxforge_core::EffectStage::Restoration | luxforge_core::EffectStage::Spatial)
    );
    let shapes = if spatial {
        vec![
            (request.drafted(boundary_layer), Some("gpu")),
            (request, Some("cpu")),
        ]
    } else {
        vec![(request, None)]
    };
    let budget = luxforge_ui::photo_surface::gpu_preview::GPU_PREVIEW_BUDGET;
    let mut over = String::new();
    let mut chosen = None;
    for (request, shape) in shapes {
        let plan = match luxforge_core::gpu_plan_with(registry, recipe, request, Some(estimates))
            .map_err(|e| e.to_string())?
        {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => {
                return Ok(RegionDraw::Gap(format!("{}: {reason}", reason.code())));
            }
        };
        let held = luxforge_ui::photo_surface::GpuBoundary::new(
            frame.texels.clone(),
            frame.width,
            frame.height,
            1,
            super::gpu_plan::boundary_format(frame.format),
        )
        .ok_or("a boundary")?;
        // A lens warp's coordinate grid over the region at the zoom, as the boundary's job
        // computes it; none for an affine or perspective tail.
        let grid = plan
            .geometry
            .grid(rect, f64::from(zoom) / 100.0)
            .map_err(|error| error.to_string())?
            .map(|grid| WarpGrid::new(&grid));
        let converted = match super::gpu_plan::surface_plan_over(
            &plan,
            held,
            frame.origin,
            grid.as_ref(),
            Some(rect),
        ) {
            Ok(converted) => converted,
            Err(reason) => {
                return Ok(RegionDraw::Gap(format!(
                    "{}: the surface cannot run {reason:?} yet",
                    reason.code()
                )));
            }
        };
        let mut notes = Vec::new();
        if let Some(GpuStep::Masked(masked)) = converted.steps.first()
            && selects_nothing(qualifier, &converted.boundary, masked)?
        {
            if !options.empty_mask {
                return Ok(RegionDraw::Gap(
                    "the mask selects nothing on this source".to_owned(),
                ));
            }
            notes.push(EMPTY_MASK.to_owned());
        }
        let charged = qualifier
            .charged_bytes(&converted)
            .map_err(|reason| format!("{reason:?}"))?;
        if charged > budget {
            // Every shape's charge, the GPU's first, so the gap names what each would take.
            let charge = format!(
                "{charged} B{}",
                shape.map_or(String::new(), |shape| format!(" in the {shape} shape"))
            );
            over = if over.is_empty() {
                format!(
                    "budget-exceeded: the slot for the {}x{} region over a {}x{} boundary \
                     would charge {charge}",
                    rect.width, rect.height, frame.width, frame.height,
                )
            } else {
                format!("{over} and {charge}")
            };
            continue;
        }
        chosen = Some((plan, converted, charged, shape, notes));
        break;
    }
    let Some((plan, converted, charged, shape, notes)) = chosen else {
        return Ok(RegionDraw::Gap(format!(
            "{over} of the {budget} B GPU-preview budget, so the drag takes the CPU path"
        )));
    };
    let started = std::time::Instant::now();
    let gpu: Vec<u8> = qualifier
        .evaluate_codes(&converted)?
        .iter()
        .flat_map(|code| [code[0], code[1], code[2]])
        .collect();
    let draw_time = started.elapsed();
    let program = if program {
        Some(codes(
            qualifier
                .evaluate(&converted)?
                .iter()
                .map(|texel| [texel[0], texel[1], texel[2]]),
        ))
    } else {
        None
    };
    Ok(RegionDraw::Drawn(DrawnRegion {
        gpu,
        program,
        approximate: plan.approximate(),
        charged,
        shape,
        notes,
        boundary_time,
        draw_time,
    }))
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

/// `rect` of `whole`, a whole frame: what a view draws of its region from the exact whole frame.
fn region_of(whole: &luxforge_core::Raster, rect: luxforge_core::Region) -> luxforge_core::Raster {
    let row = |y: u32| {
        let start = ((y * whole.width + rect.x0) * 4) as usize;
        &whole.rgba[start..start + rect.width as usize * 4]
    };
    let rgba = (rect.y0..rect.y1()).flat_map(row).copied().collect();
    luxforge_core::Raster {
        width: rect.width,
        height: rect.height,
        rgba: std::sync::Arc::new(rgba),
        ..whole.clone()
    }
}

/// Whether `masked`'s mask covers no pixel of `boundary`, its operation's input: its coverage read
/// back through a unit that adds one to every channel, so the output less the input is the
/// coverage.
fn selects_nothing(
    qualifier: &Qualifier,
    boundary: &luxforge_ui::photo_surface::GpuBoundary,
    masked: &MaskedColour,
) -> Result<bool, String> {
    let add_one = GpuProgram::new(
        "lf_test_add_one",
        "fn lf_test_add_one(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
         -> vec3<f32> {\n    return rgb + vec3<f32>(1.0);\n}\n",
    );
    let plan = GpuPlan {
        boundary: boundary.clone(),
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::Masked(MaskedColour {
            units: vec![add_one],
            ..masked.clone()
        })],
        region: None,
    };
    let input = GpuPlan {
        steps: Vec::new(),
        ..plan.clone()
    };
    let (covered, held) = (qualifier.evaluate(&plan)?, qualifier.evaluate(&input)?);
    Ok(covered
        .iter()
        .zip(&held)
        .all(|(out, input)| out[1] == input[1]))
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

/// The qualification corpus's recipes of `families` at Fit: for each source this host has, the CPU
/// frame the preview worker renders and the GPU frame of the same plan over the same source the
/// worker rendered from, written as `<recipe>--<source>-{cpu,gpu}.png` with the commands that run
/// `cargo xtask preview-error --class CLASS` over each pair, each recipe held to its own class's
/// limits. A RAW photograph's first open commits its lens profile, whose warp the geometry tail
/// draws through its coordinate grid. A cell the surface cannot run yet is a gap with its reason,
/// never a pass.
///
/// Reads `LUXFORGE_GPU_CORPUS_OUTPUT` (a new directory), `LUXFORGE_GENERATED_FIXTURES` (the
/// generated JPEGs, `fixtures/generated` by default), for the RAWs `LUXFORGE_RAW_MANIFEST`, and,
/// to run only some recipes, `LUXFORGE_GPU_CORPUS_RECIPES` (their ids, comma-separated).
/// Without an adapter it prints that `test` was skipped.
pub(crate) fn corpus_at_fit(test: &str, families: &[&str]) {
    corpus_at(test, families, None);
}

/// The qualification corpus's recipes of `families` at `zoom` percent, as [`corpus_at_fit`] runs
/// them at Fit, each cell the worker's exact visible region in [`LARGEST_WINDOW`] against the GPU
/// frame of the region plan over the region's own boundary ([`region_cell`]).
pub(crate) fn corpus_at_percent(test: &str, families: &[&str], zoom: f32) {
    corpus_at(test, families, Some(zoom));
}

/// The corpus at Fit, or at a percentage `zoom`.
fn corpus_at(test: &str, families: &[&str], zoom: Option<f32>) {
    let output = std::path::PathBuf::from(
        std::env::var("LUXFORGE_GPU_CORPUS_OUTPUT").expect("LUXFORGE_GPU_CORPUS_OUTPUT"),
    );
    assert!(
        !output.exists(),
        "{} exists: use a new directory",
        output.display()
    );
    let Some(qualifier) = headless(test) else {
        return;
    };
    std::fs::create_dir_all(&output).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &std::path::Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let corpus = read(&root.join("fixtures/preview/corpus.json"));
    let generated = std::env::var("LUXFORGE_GENERATED_FIXTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("fixtures/generated"));
    let manifest = std::env::var("LUXFORGE_RAW_MANIFEST")
        .ok()
        .map(|path| read(std::path::Path::new(&path)));
    let sources = corpus_sources(&corpus, &generated, manifest.as_ref());
    // `LUXFORGE_GPU_CORPUS_RECIPES`, comma-separated recipe ids, runs only those of the families.
    let only: Option<Vec<String>> = std::env::var("LUXFORGE_GPU_CORPUS_RECIPES")
        .ok()
        .map(|ids| ids.split(',').map(str::to_owned).collect());
    match zoom {
        None => eprintln!(
            "{test}: adapter {}, Fit bounds {:?}",
            qualifier.adapter(),
            fit_bounds()
        ),
        Some(zoom) => eprintln!(
            "{test}: adapter {}, {zoom}% in a {LARGEST_WINDOW:?} window at 2x",
            qualifier.adapter()
        ),
    }
    let (mut cells, mut commands, mut missed) = (Vec::new(), Vec::new(), Vec::new());
    for recipe in corpus["recipes"].as_array().expect("recipes") {
        let family = recipe["family"].as_str().unwrap_or_default();
        if !families.contains(&family)
            || only
                .as_ref()
                .is_some_and(|ids| !ids.iter().any(|id| recipe["id"] == id.as_str()))
        {
            continue;
        }
        let class =
            Class::parse(recipe["class"].as_str().unwrap_or_default()).expect("a recipe's class");
        let steps = recipe["steps"].as_array().expect("steps").clone();
        for source in &sources {
            if !recipe["sources"]
                .as_array()
                .expect("sources")
                .iter()
                .any(|id| id == source.id.as_str())
            {
                continue;
            }
            {
                let name = format!("{}--{}", recipe["id"].as_str().unwrap(), source.id);
                let cell = match zoom {
                    None => corpus_cell(&qualifier, source, &steps, &output, &name, class),
                    Some(zoom) => {
                        region_cell(&qualifier, source, &steps, &output, &name, class, zoom)
                    }
                }
                .unwrap_or_else(|error| Cell::Gap(format!("failed: {error}")));
                match &cell {
                    Cell::Measured {
                        stage: proxy,
                        proxy: is_proxy,
                        region,
                        statistics,
                        program,
                        passed,
                        charged,
                        shape,
                        ..
                    } => {
                        eprintln!(
                            "{name} at {}x{}{}{}: drawn {} | program {} | charged {charged} B{}",
                            proxy.0,
                            proxy.1,
                            if *is_proxy { "" } else { " (exact)" },
                            shape.map_or(String::new(), |shape| format!(", {shape} shape")),
                            figures(statistics),
                            figures(program),
                            if *passed { "" } else { " MISS" }
                        );
                        commands.push(format!(
                            "cargo xtask preview-error --candidate {dir}/{name}-gpu.png \
                             --reference {dir}/{name}-cpu.png --photo-rect 0,0,{w},{h} \
                             --class {class} --output {dir}/{name}.json",
                            class = class.name(),
                            dir = output.display(),
                            w = proxy.0,
                            h = proxy.1
                        ));
                        if !passed {
                            missed.push(name.clone());
                        }
                        let stats = |s: &Statistics| {
                            json!({
                                "mean": s.mean, "worst_block": s.worst_block, "p99": s.p99,
                                "mean_delta_l": s.mean_delta_l, "max": s.max
                            })
                        };
                        let cell = json!({
                            "cell": name, "class": class.name(),
                            "stage": [proxy.0, proxy.1], "proxy": is_proxy,
                            "region": region.map(|rect| [rect.x0, rect.y0, rect.width, rect.height]),
                            "drawn": stats(statistics), "program": stats(program),
                            "passed": passed, "charged_bytes": charged, "shape": shape
                        });
                        cells.push(cell);
                    }
                    Cell::Gap(reason) => {
                        eprintln!("{name}: gap: {reason}");
                        cells.push(json!({"cell": name, "gap": reason}));
                    }
                }
            }
        }
    }
    std::fs::write(
        output.join("cells.json"),
        serde_json::to_string_pretty(&json!({
            "adapter": qualifier.adapter(),
            "bounds": [fit_bounds().width, fit_bounds().height],
            "zoom": zoom,
            "window": zoom.map(|_| [LARGEST_WINDOW.0, LARGEST_WINDOW.1]),
            "cells": cells
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(output.join("commands.sh"), commands.join("\n") + "\n").unwrap();
    eprintln!("{test}: {} cells in {}", cells.len(), output.display());
    assert!(
        missed.is_empty(),
        "cells missing their class's limits: {missed:?}"
    );
}
