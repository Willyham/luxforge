//! The steps that time this process: a module's first use on its parts (`first-use`) and the
//! whole-frame reference render against the native vignette (`reference`).
use super::{
    Options, Report, milliseconds,
    store::{self, DataRoot, Packages},
};
use crate::*;
use luxforge_core::{
    AssetId, EntryId, PreviewRequest, RenderOptions,
    module_package::{ModulePackage, ProgramState},
    render,
};
use luxforge_gpu::external_program::{Harness, Registered, register};
use luxforge_testkit::client::{self, Owner};
use luxforge_wasm::{Compiled, MAX_ROW_PIXELS, ProgramLoader, Programs, WasmHost, entry_of};
use std::{sync::Arc, time::Instant};

/// Registers a package's program through the GPU backend's own check, as the desktop's loader
/// does.
struct NagaLoader;

impl ProgramLoader for NagaLoader {
    fn register(&self, source: &str, entry: &str) -> std::result::Result<String, String> {
        register(source, entry).map(|registered| registered.source)
    }
}

/// The probe's value set this step's calls use: the manifest's last, a mixed set, so the words
/// and the row are not the identity.
fn values(package: &ModulePackage) -> Result<Vec<luxforge_core::module_package::FieldEntry>> {
    let sets = package
        .manifest
        .probe_values()
        .map_err(|error| error.detail)?;
    Ok(sets
        .into_iter()
        .last()
        .ok_or("the manifest has no probe value sets")?
        .values)
}

/// `first-use`: in this process, a module's first use on its parts, each repeated `samples`
/// times on fresh state: the component compiled with an empty cache, its compiled code read back
/// from the cache, the first call (which instantiates it) against a warm call and a warm
/// `apply-row` of the largest row a call takes; then the program's registration, and on the local
/// adapter the probe harness's pipeline and the probe's every value set.
pub(super) fn first_use(
    report: &mut Report,
    out: &Path,
    packages: &Packages,
    options: &Options,
) -> Result {
    let start = report.load();
    let package = &packages.first().package;
    let scratch = out.join("first-use");
    fs::create_dir_all(&scratch)?;
    let n = options.samples;
    // The process's first load creates the one engine and compiles with no cache.
    let started = Instant::now();
    WasmHost::new(None)
        .load(package)
        .map_err(|error| error.detail)?;
    let process_first = milliseconds(started);
    let values = values(package)?;
    let row = vec![[0.62_f32, 0.41, 0.18]; MAX_ROW_PIXELS];
    let mut figures: [Vec<f64>; 5] = Default::default();
    for index in 0..n {
        let cache = scratch.join(format!("compiled-{index}"));
        let started = Instant::now();
        let fresh = WasmHost::new(Some(cache.clone()))
            .load(package)
            .map_err(|error| error.detail)?;
        figures[0].push(milliseconds(started));
        ensure(
            fresh.compiled() == Compiled::Compiled,
            "an empty cache did not compile the component",
        )?;
        drop(fresh);
        let started = Instant::now();
        let adjustment = WasmHost::new(Some(cache.clone()))
            .load(package)
            .map_err(|error| error.detail)?;
        figures[1].push(milliseconds(started));
        ensure(
            adjustment.compiled() == Compiled::Cached,
            "the written cache was not read back",
        )?;
        let started = Instant::now();
        let words = adjustment
            .coefficients(&values)
            .map_err(|error| error.detail)?;
        figures[2].push(milliseconds(started));
        let started = Instant::now();
        adjustment
            .coefficients(&values)
            .map_err(|error| error.detail)?;
        figures[3].push(milliseconds(started));
        let mut rgb = row.clone();
        let started = Instant::now();
        adjustment
            .apply_row(&words, 0, 0, &mut rgb)
            .map_err(|error| error.detail)?;
        figures[4].push(milliseconds(started));
        ensure(rgb != row, "the mixed value set's row came back unchanged")?;
        fs::remove_dir_all(&cache)?;
    }
    let load = report.span(start);
    let scope = format!(
        "this process (release xtask), the example's package; {n} repetitions, each on a new \
         engine host and a new cache directory, the one Wasmtime engine already created"
    );
    report.row(
        "first_use.process_first_load_ms",
        "ms",
        vec![process_first],
        "this process's first load: the Wasmtime engine's creation and the component compiled \
         with no cache",
        &load,
    );
    for (metric, samples, what) in [
        (
            "first_use.compile_ms",
            &figures[0],
            "load with an empty cache: compile and write the cache",
        ),
        (
            "first_use.cached_load_ms",
            &figures[1],
            "load reading the compiled code back from the cache",
        ),
        (
            "first_use.instantiate_and_first_call_ms",
            &figures[2],
            "the first coefficients call, which creates the instance (and starts the epoch \
             ticker the first time)",
        ),
        (
            "first_use.warm_call_ms",
            &figures[3],
            "a coefficients call on the idle instance",
        ),
        (
            "first_use.warm_apply_row_ms",
            &figures[4],
            "one apply-row of 16,384 pixels on the idle instance",
        ),
    ] {
        report.row(
            metric,
            "ms",
            samples.clone(),
            &format!("{what}; {scope}"),
            &load,
        );
    }
    report.write()?;
    gpu_first_use(report, &scratch, package, n)
}

/// The adapter the probe runs on here: `module-check`'s choice, the first hardware adapter of the
/// primary backends, else the first software one. The desktop probes on the window's adapter.
fn gpu_first_use(report: &mut Report, scratch: &Path, package: &ModulePackage, n: usize) -> Result {
    let start = report.load();
    let source = package
        .program
        .as_deref()
        .ok_or("the example ships no GPU program")?;
    let entry = entry_of(&package.hash);
    let mut registration = Vec::with_capacity(n);
    for _ in 0..n {
        let started = Instant::now();
        register(source, &entry)?;
        registration.push(milliseconds(started));
    }
    let mut detail = json!({"entry": entry});
    let opened = crate::module_check::gpu::adapter().map(|chosen| {
        luxforge_gpu::adapters::open(
            &chosen.backend,
            &chosen.name,
            wgpu::Backends::PRIMARY,
            &[wgpu::Limits::default(), wgpu::Limits::downlevel_defaults()],
        )
        .map_err(|why| {
            format!(
                "{} ({}) opened no device: {why:?}",
                chosen.name, chosen.backend
            )
        })
    });
    let mut figures: [Vec<f64>; 4] = Default::default();
    let mut states = Vec::new();
    match opened {
        None => {
            detail["gpu"] = json!({"measured": false,
                "reason": "this host offers no Vulkan, Metal or DirectX 12 adapter"});
        }
        Some(Err(why)) => detail["gpu"] = json!({"measured": false, "reason": why}),
        Some(Ok(opened)) => {
            let adapter = &opened.adapter;
            let key = format!(
                "{} ({}, {} {})",
                adapter.name, adapter.backend, adapter.driver, adapter.driver_info
            );
            detail["gpu"] = json!({"measured": true, "adapter": key,
                "device_type": adapter.device_type});
            for index in 0..n {
                let programs = Programs::new(
                    Arc::new(NagaLoader),
                    Some(scratch.join(format!("probes-{index}"))),
                );
                let started = Instant::now();
                let adjustment = WasmHost::new(None)
                    .with_programs(programs.clone())
                    .load(package)
                    .map_err(|error| error.detail)?;
                let load_ms = milliseconds(started);
                let probe = programs
                    .probe(&entry)
                    .ok_or("the program was not registered")?;
                ensure(
                    probe.state() == ProgramState::Pending,
                    format!("a fresh program is {:?}, not pending", probe.state()),
                )?;
                let whole = Instant::now();
                let mut run = probe.begin(&key).ok_or("the probe would not begin")?;
                let program = Registered {
                    entry: probe.entry().to_owned(),
                    source: probe.source().to_owned(),
                };
                let started = Instant::now();
                let harness = Harness::new(&opened.device, &program, run.patches())?;
                figures[0].push(milliseconds(started));
                let steps = run.steps();
                let started = Instant::now();
                while run.step(&mut |words| harness.run(&opened.device, &opened.queue, words)) {}
                figures[1].push(milliseconds(started));
                figures[2].push(milliseconds(whole));
                figures[3].push(load_ms);
                states.push(json!({"steps": steps, "state": probe.state().to_json()}));
                drop((harness, run, adjustment));
            }
            ensure(
                states
                    .iter()
                    .all(|state| state["state"]["state"] == "enabled"),
                format!("a probe did not enable the program: {states:?}"),
            )?;
        }
    }
    detail["probes"] = json!(states);
    report.details.insert("first_use".into(), detail);
    let load = report.span(start);
    let scope = format!(
        "this process (release xtask), the example's program; {n} repetitions, each a new \
         registration and probe cache, on the adapter the details name"
    );
    report.row(
        "first_use.program_register_ms",
        "ms",
        registration,
        &format!(
            "naga validation, rename and validation again of program.wgsl under the prelude; {scope}"
        ),
        &load,
    );
    for (metric, index, what) in [
        (
            "first_use.probe_harness_pipeline_ms",
            0,
            "the probe harness's shader module and compute pipeline created on the device",
        ),
        (
            "first_use.probe_steps_ms",
            1,
            "every value set: the reference over the probe patches, the program on the device, \
             read back and judged; the result cached",
        ),
        (
            "first_use.probe_whole_ms",
            2,
            "the probe as the tile worker runs it: begin, pipeline and every step",
        ),
        (
            "first_use.load_with_program_ms",
            3,
            "load with the program registered, no compiled-code cache",
        ),
    ] {
        report.row(
            metric,
            "ms",
            figures[index].clone(),
            &format!("{what}; {scope}"),
            &load,
        );
    }
    report.write()
}

/// One field patch on `asset`, which must change it.
fn commit(
    owner: &Owner,
    client: luxforge_core::ClientId,
    asset: &Value,
    method: &str,
    fields: Value,
) -> Result {
    let mut params = fields;
    params["asset_id"] = asset.clone();
    params["mutation"] = client::mutation(
        owner.revision(client, asset)?,
        &client::request_id(super::ACTOR),
        super::ACTOR,
    );
    let answer = owner.call(client, method, params)?;
    ensure(
        answer["outcome"] == "applied",
        format!("{method} answered {answer}"),
    )
}

/// The asset's current entry.
fn current(owner: &Owner, client: luxforge_core::ClientId, asset: &Value) -> Result<EntryId> {
    Ok(serde_json::from_value(
        owner.state(client, asset)?["current_entry"]["id"].clone(),
    )?)
}

/// One whole-frame reference render of `entry`: its time, the pixels of a 5 × 5 grid over the
/// frame (whose outer ones the vignette darkens, and which Split toning tints), and its recipe.
fn render_entry(
    owner: &Owner,
    client: luxforge_core::ClientId,
    asset: &AssetId,
    entry: &EntryId,
) -> Result<(f64, Vec<[u8; 4]>, Value)> {
    let job =
        owner.preview_job(PreviewRequest::new(client, asset.clone()).entry(Some(entry.clone())))?;
    let e = &job.evaluation;
    let started = Instant::now();
    let frame = render(
        e.registry(),
        e.source().whole()?,
        e.recipe(),
        RenderOptions::default(),
        e.context(),
    )?
    .frame(e.entry().snapshot.id.clone())?;
    let ms = milliseconds(started);
    let mut grid = Vec::with_capacity(25);
    for row in 0..5 {
        for column in 0..5 {
            let (x, y) = ((frame.width - 1) * column / 4, (frame.height - 1) * row / 4);
            grid.push(frame.pixel(x, y).ok_or("no pixel")?);
        }
    }
    Ok((ms, grid, serde_json::to_value(e.recipe())?))
}

/// The workloads `reference` alternates: no layer, the native vignette, Split toning.
const WORKLOADS: [&str; 3] = ["none", "vignette", "split_toning"];

/// `reference`: for the 24 MP and 60 MP JPEGs, a data root with Split toning installed through the
/// API, the next launch's owner opening and preparing the photo, then three entries: the photo as
/// opened, a vignette (amount −30), and Split toning (highlight saturation 40, shadow saturation
/// 30, balance 20) after the vignette's reset, which a neutral layer compiles to nothing. Each is
/// rendered whole once (its first render), then `samples` rounds of the three, the order rotating
/// each round.
pub(super) fn reference(
    report: &mut Report,
    out: &Path,
    packages: &Packages,
    options: &Options,
) -> Result {
    let n = options.samples;
    let built = packages.first();
    let set = format!("edit.set-{}", built.package.manifest.name);
    let mut details = serde_json::Map::new();
    for megapixels in [24, 60] {
        let start = report.load();
        let root = DataRoot::new(&out.join(format!("reference-{megapixels}mp")));
        store::install(&root, &[built])?;
        let owner = store::owner(&root)?;
        let client = owner.client();
        let source = options.source(megapixels).canonicalize()?;
        let opened = owner.open(client, &source)?;
        let asset_value = opened["asset"]["id"].clone();
        let asset: AssetId = serde_json::from_value(asset_value.clone())?;
        owner.prepare(client, &asset_value)?;
        let none = current(&owner, client, &asset_value)?;
        commit(
            &owner,
            client,
            &asset_value,
            "edit.set-vignette",
            json!({"amount": -30}),
        )?;
        let vignette = current(&owner, client, &asset_value)?;
        commit(
            &owner,
            client,
            &asset_value,
            "edit.reset-vignette",
            json!({}),
        )?;
        commit(
            &owner,
            client,
            &asset_value,
            &set,
            json!({"highlight-saturation": 40, "shadow-saturation": 30, "balance": 20}),
        )?;
        let split = current(&owner, client, &asset_value)?;
        let entries = [none, vignette, split];
        let mut first = Vec::new();
        let mut pixels = Vec::new();
        let mut recipes = Vec::new();
        for entry in &entries {
            let (ms, pixel, recipe) = render_entry(&owner, client, &asset, entry)?;
            first.push(ms);
            pixels.push(pixel);
            recipes.push(recipe);
        }
        ensure(
            pixels[1] != pixels[0] && pixels[2] != pixels[0],
            format!("a layer left the frame's grid unchanged: {pixels:?}; recipes {recipes:?}"),
        )?;
        let mut renders: [Vec<f64>; 3] = Default::default();
        let mut added: [Vec<f64>; 2] = Default::default();
        let mut rss = Vec::new();
        for round in 0..n {
            let mut times = [0.0; 3];
            for offset in 0..3 {
                let at = (round + offset) % 3;
                let (ms, pixel, _) = render_entry(&owner, client, &asset, &entries[at])?;
                ensure(
                    pixel == pixels[at],
                    format!("{} rendered another grid", WORKLOADS[at]),
                )?;
                times[at] = ms;
            }
            for (at, ms) in times.iter().enumerate() {
                renders[at].push(*ms);
            }
            added[0].push(times[1] - times[0]);
            added[1].push(times[2] - times[0]);
            let resources = owner.call(client, "resources.read", json!({}))?;
            if let Some(bytes) = resources["memory"]["resident_bytes"].as_u64() {
                rss.push(bytes as f64 / 1048576.0);
            }
        }
        owner.close()?;
        let load = report.span(start);
        let dimensions = json!([opened["asset"]["width"], opened["asset"]["height"]]);
        let scope = format!(
            "the whole-frame CPU reference render (luxforge_core::render) of the prepared {} JPEG \
             {dimensions}, its frame allocated; this process (release xtask) on the shared Rayon \
             pool; {n} rounds of the three workloads, rotating order, after one first render \
             each; no GPU, no export; with no layer the render shares the prepared source and \
             copies nothing, so a layer's figure is its whole cost",
            source.display()
        );
        for (at, name) in WORKLOADS.iter().enumerate() {
            report.row(
                &format!("reference.{megapixels}mp.render_ms.{name}"),
                "ms",
                renders[at].clone(),
                &scope,
                &load,
            );
            report.row(
                &format!("reference.{megapixels}mp.first_render_ms.{name}"),
                "ms",
                vec![first[at]],
                &format!(
                    "the workload's first render{}; {scope}",
                    if *name == "split_toning" {
                        ", which creates an instance on each worker that calls the module"
                    } else {
                        ""
                    }
                ),
                &load,
            );
        }
        for (at, name) in ["vignette", "split_toning"].iter().enumerate() {
            report.row(
                &format!("reference.{megapixels}mp.added_ms.{name}"),
                "ms",
                added[at].clone(),
                &format!("the round's {name} render less its render with no layer; {scope}"),
                &load,
            );
        }
        report.row(
            &format!("reference.{megapixels}mp.rss_between_rounds_mib"),
            "MiB",
            rss,
            &format!("resources.read between rounds, not a peak; {scope}"),
            &load,
        );
        details.insert(
            format!("{megapixels}mp"),
            json!({"source": source, "dimensions": dimensions,
                "grid_pixels": pixels, "recipes": recipes}),
        );
        report.write()?;
    }
    report
        .details
        .insert("reference".into(), Value::Object(details));
    report.write()
}
