//! Finished-build, headless Detail measurements through the public renderer and catalog API.
//!
//! Run sequentially after feature and native verification, on a quiet host. Each source gets its
//! own new evidence directory and process, so the OS process high-water mark is attributable to
//! that invocation. The default thirty samples support nearest-rank p50/p95; fewer samples remain
//! diagnostics. This launches no GUI. Export includes production durable writes.
//!
//! ```sh
//! cargo run --release --locked -p xtask -- detail-performance \
//!   --source fixtures/generated/24mp.jpg --output NEW --samples 30
//! ```
//!
//! `--case render|export|points|cancel|sharing` isolates a workload. Prepared-source decode is
//! outside render, export and point latency; cold picks reopen and prepare the owner outside the
//! timer. Point tiles are query-local, so a warm pick means the second request on that owner,
//! rather than a cross-query tile-cache hit. Colour-limited later ticks use the same draft memo.
use crate::*;
use luxforge_core::{
    ApiRequest, AssetId, ClientId, DETAIL_EFFECT, Layer, ModuleRegistry, PrefixUse, PreviewIntent,
    PreviewJob, PreviewQueue, PreviewRequest, PreviewSource, ProxyBounds, Recipe, RenderContext,
    RenderOptions, SnapshotId, render, resources,
};
use luxforge_testkit::client::{self, Owner};
use std::{
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const FIT: ProxyBounds = ProxyBounds {
    width: 2880,
    height: 1800,
};

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

struct Fixture {
    owner: Owner,
    client: ClientId,
    asset: Value,
}
impl Fixture {
    fn import(catalog: &Path, source: &Path) -> Result<Self> {
        let owner = Owner::start(catalog, ModuleRegistry::builtin(), "detail-performance")?;
        let client = owner.client();
        let state = owner.import(client, source)?;
        let fixture = Self {
            owner,
            client,
            asset: state["asset"]["id"].clone(),
        };
        fixture.mutate(
            "edit.set-detail",
            json!({"luminance":25.0,"colour":25.0,"sharpening":40.0}),
        )?;
        fixture.mutate("edit.set-basic", json!({"exposure":0.5}))?;
        Ok(fixture)
    }
    fn reopen(catalog: &Path, asset: Value) -> Result<Self> {
        let owner = Owner::start(catalog, ModuleRegistry::builtin(), "detail-performance")?;
        let client = owner.client();
        Ok(Self {
            owner,
            client,
            asset,
        })
    }
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        Ok(self.owner.call(self.client, method, params)?)
    }
    fn mutate(&self, method: &str, mut params: Value) -> Result<Value> {
        params["asset_id"] = self.asset.clone();
        params["mutation"] = client::mutation(
            self.owner.revision(self.client, &self.asset)?,
            &client::request_id(method),
            "detail-performance",
        );
        self.call(method, params)
    }
    fn job(&self) -> Result<PreviewJob> {
        let asset: AssetId = serde_json::from_value(self.asset.clone())?;
        Ok(self
            .owner
            .preview_job(PreviewRequest::new(self.client, asset))?)
    }
    fn resource_report(&self) -> Result<Value> {
        self.call("resources.read", json!({}))
    }
    fn close(self) -> Result {
        Ok(self.owner.close()?)
    }
}

fn released(report: &resources::ResourceReport) -> Result {
    ensure(
        report.budgets.colour_scratch.in_use_bytes == 0 && report.budgets.spatial.in_use_bytes == 0,
        "A completed or cancelled job retained a working-set reservation",
    )
}

fn full_render(fixture: &Fixture, samples: usize) -> Result<Value> {
    let job = fixture.job()?;
    let evaluation = &job.evaluation;
    let context = RenderContext::new();
    let mut durations = Vec::with_capacity(samples);
    let mut observations = Vec::with_capacity(samples);
    let mut first_hash = None;
    for sample in 0..samples {
        let start = Instant::now();
        let frame = render(
            evaluation.registry(),
            evaluation.source(),
            evaluation.recipe(),
            RenderOptions::default(),
            &context,
        )?
        .frame(evaluation.entry().snapshot.id.clone())?;
        let duration = ms(start);
        ensure(
            (frame.width, frame.height) == evaluation.source().dimensions(),
            "Full render changed the stage",
        )?;
        if first_hash.is_none() {
            first_hash = Some(format!("{:x}", Sha256::digest(frame.rgba.as_slice())));
        }
        durations.push(duration);
        let memory = resources::read(&context);
        released(&memory)?;
        observations
            .push(json!({"sample":sample,"ms":duration,"resources_while_result_held":memory}));
        drop(frame);
    }
    Ok(json!({
        "rows":[stats::row("detail.full_render", "ms", durations)],
        "observations":observations,"frame_sha256":first_hash,
        "recipe":evaluation.recipe(),"identity":job.identity,
        "scope":"Prepared JPEG, full-resolution renderer plus compilation, shared empty-then-warm render context; no export, histogram or display reduction. Resource reads and first-frame hash excluded."
    }))
}

fn export(fixture: &Fixture, out: &Path, samples: usize) -> Result<Value> {
    let directory = out.join("exports");
    fs::create_dir(&directory)?;
    let mut durations = Vec::with_capacity(samples);
    let mut observations = Vec::with_capacity(samples);
    let mut first_hash = None;
    for sample in 0..samples {
        let destination = directory.join(format!("detail-{sample:02}.jpg"));
        let request_id = client::request_id("detail-export");
        let start = Instant::now();
        let accepted = fixture.call(
            "export.jpeg",
            json!({
                "asset_id":fixture.asset,"destination":destination,"keep_metadata":false,
                "mutation":{"request_id":request_id,"actor":"detail-performance"}
            }),
        )?;
        let settled = client::settle(&fixture.owner, fixture.client, &accepted["job_id"])?;
        let duration = ms(start);
        ensure(
            settled["status"] == "ready",
            format!("Export did not finish: {settled}"),
        )?;
        let encoded_hash = hash(&destination)?;
        match &first_hash {
            Some(expected) => ensure(
                &encoded_hash == expected,
                "Unchanged recipe exported different bytes",
            )?,
            None => first_hash = Some(encoded_hash.clone()),
        }
        durations.push(duration);
        observations.push(json!({
            "sample":sample,"ms":duration,"accepted":accepted,"result":settled,
            "sha256":encoded_hash,"resources":fixture.resource_report()?
        }));
        // Retain one exact artifact, without accumulating thirty large JPEGs beside the catalog.
        if sample != 0 {
            fs::remove_file(destination)?;
        }
    }
    Ok(json!({
        "rows":[stats::row("detail.export_jpeg_request_to_ready", "ms", durations)],
        "observations":observations,"first_export":directory.join("detail-00.jpg"),
        "scope":"Public owner export request through ready: owner admission, worker render, quality-90 encoding, durable publish and client job polling (1 ms interval). Prepared JPEG; metadata off; hashes/deletions/resource reads excluded."
    }))
}

fn source_sharing(fixture: &Fixture) -> Result<Value> {
    let first = fixture.job()?;
    let second = fixture.job()?;
    let (PreviewSource::Jpeg(source), PreviewSource::Jpeg(other)) =
        (first.evaluation.source(), second.evaluation.source())
    else {
        return Err(
            "This utility measures a JPEG; RAW source sharing is qualified separately".into(),
        );
    };
    ensure(
        Arc::ptr_eq(&source.rgba, &other.rgba),
        "Two planned jobs copied the prepared JPEG",
    )?;
    let context = RenderContext::new();
    let mut cases = Vec::new();
    for (label, payload) in [
        ("all-default", json!({})),
        ("ancillary-only", json!({"radius":2.0})),
    ] {
        let recipe = Recipe {
            layers: vec![Layer::new(DETAIL_EFFECT, payload)],
            ..Recipe::default()
        };
        let frame = render(
            first.evaluation.registry(),
            source,
            &recipe,
            RenderOptions::default(),
            &context,
        )?
        .frame(SnapshotId::new())?;
        let shared = Arc::ptr_eq(&source.rgba, &frame.rgba);
        ensure(
            shared,
            format!("Neutral Detail {label} copied the source buffer"),
        )?;
        cases
            .push(json!({"case":label,"recipe":recipe,"source_and_result_same_allocation":shared}));
    }
    let memory = resources::read(&context);
    released(&memory)?;
    Ok(json!({
        "rows":[],"cases":cases,"prepared_jobs_same_allocation":true,
        "decoded_source_bytes":source.rgba.len(),"resources":memory,
        "scope":"Arc allocation identity, not value equality or an RSS inference; prepared jobs and both neutral Detail shapes."
    }))
}

fn cancellation(fixture: &Fixture, samples: usize) -> Result<Value> {
    let base = fixture.job()?;
    let context = fixture.owner.render_context();
    let mut durations = Vec::with_capacity(samples);
    let mut observations = Vec::with_capacity(samples);
    for sample in 0..samples {
        let mut queue = PreviewQueue::default();
        let mut job = base.clone();
        job.intent = PreviewIntent::Interactive;
        job.proxy = Some(FIT);
        let queued = queue.request(job.clone());
        // A live spatial reservation proves the cancel reaches active restoration work. This
        // public resource read adds system-call overhead before the timer, never inside it.
        luxforge_testbase::try_wait_for("Detail proxy to reserve a tile", || {
            let report = resources::read(&context);
            if report.budgets.spatial.in_use_bytes > 0 {
                Some(Ok(()))
            } else if queue.ready() || !queue.is_busy() {
                Some(Err(
                    "Proxy completed before active restoration was observed",
                ))
            } else {
                None
            }
        })??;
        let start = Instant::now();
        let floor = queue.cancel();
        let mut stale_results = 0;
        luxforge_testbase::try_wait_for("cancelled Detail proxy to clean up", || {
            while queue.poll().is_some() {
                stale_results += 1;
            }
            (!queue.is_busy()).then_some(())
        })?;
        let duration = ms(start);
        let memory = resources::read(&context);
        released(&memory)?;
        ensure(
            stale_results == 0,
            "Cancelled preview delivered a stale frame",
        )?;
        // A successful next job on the same queue proves the cancelled prefix was discarded,
        // rather than an incomplete frame adopted as a hit. These pixels are outside the timer.
        let recovery_generation = queue.request(job);
        let recovery = luxforge_testbase::try_wait_for("next Detail proxy", || queue.poll())?;
        ensure(
            recovery.generation == recovery_generation,
            "Recovery delivered another generation",
        )?;
        let proxy = recovery.proxy().ok_or("Recovery produced no proxy")?;
        ensure(
            recovery.restoration_prefix == Some(PrefixUse::Built),
            "Cancelled prefix was reused",
        )?;
        luxforge_testbase::try_wait_for("recovered proxy to become idle", || {
            (!queue.is_busy()).then_some(())
        })?;
        let after = resources::read(&context);
        released(&after)?;
        // This recipe has one leading Detail and only colour after it. Its uncropped boundary is
        // RGB16 at the actual proxy dimensions. This is a derived frame size, not a private cache
        // counter or an OS allocation estimate.
        let held_bytes = u64::from(proxy.dimensions.0) * u64::from(proxy.dimensions.1) * 6;
        observations.push(json!({
            "sample":sample,"cancel_to_idle_ms":duration,"requested_generation":queued,
            "cancel_floor":floor,"stale_results":stale_results,"resources_after_cancel":memory,
            "recovery":{"generation":recovery_generation,"prefix":recovery.restoration_prefix,
                "proxy_dimensions":proxy.dimensions,"source_proxy_built":proxy.built,
                "derived_restoration_prefix_bytes":held_bytes,"resources_with_caches_held":after}
        }));
        durations.push(duration);
    }
    Ok(json!({
        "rows":[stats::row("detail.active_proxy_cancel_to_worker_idle", "ms", durations)],
        "observations":observations,"display_bounds":FIT,
        "scope":"Cancel during a live Detail tile on a fresh public PreviewQueue; latency includes worker cleanup and client polling (1 ms interval). Recovery runs on the same queue outside the timer, builds its prefix and retains caches until queue drop."
    }))
}

fn points(
    fixture: Fixture,
    catalog: &Path,
    width: u32,
    height: u32,
    samples: usize,
) -> Result<Value> {
    let stroke = json!({"points":[[0.5,0.5]],"size":0.1,"feather":40.0,"flow":80.0,
        "erase":false,"limit_to_colour":false,"colour_refine":50.0});
    let created = fixture.mutate("mask.add-stroke", stroke.clone())?;
    fixture.mutate(
        "edit.set-basic",
        json!({"mask":created["mask"],"exposure":0.5}),
    )?;
    let asset = fixture.asset.clone();
    fixture.close()?;
    let mut cold = Vec::with_capacity(samples);
    let mut warm = Vec::with_capacity(samples);
    let mut first = Vec::with_capacity(samples);
    let mut later = Vec::with_capacity(samples);
    let mut preparations = Vec::with_capacity(samples);
    let mut observations = Vec::with_capacity(samples);
    for sample in 0..samples {
        let fixture = Fixture::reopen(catalog, asset.clone())?;
        let start = Instant::now();
        client::prepare(&fixture.owner, fixture.client, &asset)?;
        let prepare_ms = ms(start);
        preparations.push(prepare_ms);
        let mut picks = Vec::new();
        for durations in [&mut cold, &mut warm] {
            let start = Instant::now();
            let answer = (*fixture.owner).call(
                fixture.client,
                ApiRequest {
                    id: client::request_id("neutral25"),
                    method: "query.neutral-sample".into(),
                    params: json!({"asset_id":asset,"x":width/2,"y":height/2}),
                    token: None,
                },
            )?;
            let duration = ms(start);
            if let Some(error) = &answer.error {
                ensure(
                    error.message.starts_with("clipped:")
                        || error.message.starts_with("near-black:")
                        || error.message.starts_with("out-of-range:"),
                    format!("Unexpected neutral query error: {error:?}"),
                )?;
            }
            durations.push(duration);
            picks.push(json!({"ms":duration,"response":answer}));
        }
        let draft = fixture.call(
            "draft.begin",
            json!({"asset_id":asset,"action":"mask.add-stroke",
            "mask":created["mask"],"component":created["component"]}),
        )?;
        let mut fields = stroke.clone();
        fields["limit_to_colour"] = json!(true);
        let start = Instant::now();
        let first_answer = fixture.call(
            "draft.set",
            json!({"draft_id":draft["draft_id"],"fields":fields}),
        )?;
        let first_ms = ms(start);
        first.push(first_ms);
        let start = Instant::now();
        let later_answer = fixture.call(
            "draft.set",
            json!({"draft_id":draft["draft_id"],"fields":{
            "points":[[0.5,0.5],[0.52,0.55]],"size":0.11,"flow":81.0}}),
        )?;
        let later_ms = ms(start);
        later.push(later_ms);
        fixture.call("draft.cancel", json!({"draft_id":draft["draft_id"]}))?;
        observations.push(json!({"sample":sample,"source_prepare_ms":prepare_ms,
            "neutral_picks":picks,"first_tick":{"ms":first_ms,"response":first_answer},
            "later_tick":{"ms":later_ms,"response":later_answer},"resources":fixture.resource_report()?}));
        fixture.close()?;
    }
    Ok(json!({
        "rows":[stats::row("detail.neutral25_first_request_on_reopened_prepared_owner", "ms", cold),
            stats::row("detail.neutral25_second_request_on_same_owner", "ms", warm),
            stats::row("detail.colour_limited_first_tick_owner_to_worker", "ms", first),
            stats::row("detail.colour_limited_later_tick_same_draft", "ms", later),
            stats::row("detail.reopened_owner_source_prepare", "ms", preparations)],
        "observations":observations,"neutral_point":[width/2,height/2],
        "scope":"Each pair uses a newly reopened owner/context and verified prepared source. Neutral query reads its declared 25 points; tiles are query-local. First limited tick seeds through the point worker; later tick uses the same draft seed memo. No commit, overlay, preview or GPU work is requested. Owner startup/preparation and draft begin/cancel excluded from query/tick durations."
    }))
}

pub fn run(root: &Path, source: &Path, out: &Path, samples: usize, case: &str) -> Result {
    ensure(
        !cfg!(debug_assertions),
        "Detail timing requires a release xtask build",
    )?;
    ensure(samples > 0, "Detail samples must be positive")?;
    ensure(
        matches!(
            case,
            "all" | "render" | "export" | "points" | "cancel" | "sharing"
        ),
        "Unknown Detail performance case",
    )?;
    ensure(!out.exists(), "Detail performance output must be new")?;
    let source = source.canonicalize()?;
    let source_hash = hash(&source)?;
    let executable = std::env::current_exe()?;
    let started_unix_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    let diff = output(root, "git", &["diff", "--binary", "HEAD"])?;
    let mut report = json!({
        "format":1,"status":"running","run":{"started_unix_ns":started_unix_ns,"pid":std::process::id(),"output":out},
        "build":{"executable":executable,"sha256":hash(&executable)?,"cargo_lock_sha256":hash(&root.join("Cargo.lock"))?,
            "git_head":output(root,"git",&["rev-parse","HEAD"])?.trim(),
            "git_status":output(root,"git",&["status","--porcelain"])?,
            "tracked_diff_sha256":format!("{:x}",Sha256::digest(diff.as_bytes())),
            "rustc":output(root,"rustc",&["-vV"])?,"release":true},
        "host":{"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,
            "system":output(root,"uname",&["-a"]).ok(),"start_load":launch::load(launch::load_average(root))},
        "source":{"path":source,"sha256":source_hash},"samples":samples,"selected_case":case,
        "cases":{},"rows":[],
        "scope":"Headless functional measurement utility. OS memory/high-water marks include this process's earlier workloads and are not per-case peaks; budgets are render-context counters. GPU residency, backend staging and private source/prefix cache counters are unmeasured. No budget verdict is inferred."
    });
    fs::create_dir_all(out)?;
    write_json(&out.join("result.json"), &report)?;
    let outcome = (|| -> Result {
        let catalog = out.join("catalog.sqlite");
        let fixture = Fixture::import(&catalog, &source)?;
        let job = fixture.job()?;
        ensure(
            matches!(job.evaluation.source(), PreviewSource::Jpeg(_)),
            "Use a JPEG source for this utility",
        )?;
        let (width, height) = job.evaluation.source().dimensions();
        ensure(
            matches!(
                u64::from(width) * u64::from(height),
                24_000_000 | 60_000_000
            ),
            "Use the planned 24 MP or 60 MP JPEG workload",
        )?;
        report["source"]["dimensions"] = json!([width, height]);
        report["recipe"] = serde_json::to_value(job.evaluation.recipe())?;
        report["identity"] = serde_json::to_value(&job.identity)?;
        drop(job);
        for name in ["sharing", "render", "export", "cancel"] {
            if case != "all" && case != name {
                continue;
            }
            println!("Detail {name}: {samples} samples on {width}x{height}");
            let start_load = launch::load(launch::load_average(root));
            let mut result = match name {
                "sharing" => source_sharing(&fixture)?,
                "render" => full_render(&fixture, samples)?,
                "export" => export(&fixture, out, samples)?,
                "cancel" => cancellation(&fixture, samples)?,
                _ => unreachable!(),
            };
            result["start_load"] = start_load;
            result["end_load"] = launch::load(launch::load_average(root));
            report["rows"]
                .as_array_mut()
                .unwrap()
                .extend(stats::rows(&result).iter().cloned());
            report["cases"][name] = result;
            write_json(&out.join("result.json"), &report)?;
        }
        if case == "all" || case == "points" {
            println!("Detail points: {samples} reopened owners on {width}x{height}");
            let start_load = launch::load(launch::load_average(root));
            let mut result = points(fixture, &catalog, width, height, samples)?;
            result["start_load"] = start_load;
            result["end_load"] = launch::load(launch::load_average(root));
            report["rows"]
                .as_array_mut()
                .unwrap()
                .extend(stats::rows(&result).iter().cloned());
            report["cases"]["points"] = result;
        } else {
            fixture.close()?;
        }
        ensure(
            hash(&source)? == source_hash,
            "The original changed during the Detail measurement",
        )?;
        Ok(())
    })();
    report["status"] = json!(if outcome.is_ok() {
        "completed"
    } else {
        "failed"
    });
    if let Err(error) = &outcome {
        report["error"] = json!(error.to_string());
    }
    report["host"]["end_load"] = launch::load(launch::load_average(root));
    write_json(&out.join("result.json"), &report)?;
    outcome
}
