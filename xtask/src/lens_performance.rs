//! Lens-only request and exact-render diagnostics, over the owner's prepared JPEG or RAW source.
//! One export is outstanding at a time; the RAII owner cancels and joins its lanes on every exit.
use crate::*;
use luxforge_core::{
    ApiRequest, AssetId, ClientId, LENS_EFFECT, ModuleRegistry, OwnerHandle, PreviewJob,
    PreviewRequest, PreviewSource, Raster, RenderOptions, render,
};
use luxforge_testkit::client::{self, Owner};
use std::time::{Duration, Instant};

const ACTOR: &str = "xtask-lens-performance";
const MAX_SAMPLES: usize = 100;
const JOB_DEADLINE: Duration = Duration::from_secs(180);
const JOB_POLL: Duration = Duration::from_millis(2);
const INDEX_DEADLINE: Duration = Duration::from_secs(5);
const PERSPECTIVE: (i64, i64) = (20, -10);
const CROP_DEGREES: f64 = 2.5;
const METRICS: [&str; 7] = [
    "lens_profiles_query",
    "lens_profile_selection_commit",
    "warm_baseline_full_exact_render",
    "lens_perspective_crop_full_exact_render",
    "lens_perspective_crop_point_pick",
    "lens_perspective_crop_export_completion",
    "lens_perspective_crop_cancelled_export_completion",
];

fn milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn envelope(revision: Option<u64>) -> Value {
    let mut value = json!({"request_id":client::request_id(ACTOR),"actor":ACTOR});
    if let Some(revision) = revision {
        value["expected_revision"] = json!(revision);
    }
    value
}

/// A terminal read follows the worker's cleanup, including a cancelled running export's staged
/// file removal. Merely acknowledging `job.cancel` would measure a different operation.
fn terminal(owner: &Owner, client: ClientId, job: &Value) -> Result<Value> {
    let started = Instant::now();
    loop {
        let read = owner.call(client, "job.read", json!({"job_id":job}))?;
        match read["status"].as_str() {
            Some("ready" | "failed" | "cancelled" | "superseded") => return Ok(read),
            Some("queued" | "running") => {
                ensure(
                    started.elapsed() < JOB_DEADLINE,
                    format!("Job timed out: {read}"),
                )?;
                std::thread::sleep(JOB_POLL);
            }
            _ => return Err(format!("Unknown job status: {read}").into()),
        }
    }
}

fn require_status(read: &Value, status: &str) -> Result {
    ensure(
        read["status"] == status,
        format!("Expected {status} job: {read}"),
    )
}

/// Selection must use the key of the profile the query detected, never a guessed database record.
/// A supported RAW's import already applied it; selecting it again with the acknowledgement still
/// writes the workload's own entry.
fn candidate(query: &Value) -> Result<Value> {
    Some(&query["status"]["detected"])
        .filter(|detected| {
            detected["eligible"] == true
                && detected["key"].as_str().is_some_and(|key| !key.is_empty())
        })
        .cloned()
        .ok_or_else(|| format!("Lens workload needs an eligible detected profile: {query}").into())
}

fn frozen_profile(state: &Value) -> Result<Value> {
    state["current_entry"]["snapshot"]["recipe"]["layers"]
        .as_array()
        .and_then(|layers| {
            layers
                .iter()
                .find(|layer| layer["effect_id"] == LENS_EFFECT)
        })
        .map(|layer| layer["payload"].clone())
        .filter(|payload| payload["profile"].is_object())
        .ok_or_else(|| "Selection did not freeze a lens profile".into())
}

/// Module construction starts the one background index parse. Wait only during setup, before
/// any query timing; a missing or failed index still refuses the workload explicitly.
fn initial_query(owner: &Owner, client: ClientId, asset: &AssetId) -> Result<Value> {
    let started = Instant::now();
    loop {
        let response = OwnerHandle::call(
            owner,
            client,
            ApiRequest {
                id: client::request_id("lens-performance-warm-query"),
                method: "query.lens-profiles".into(),
                params: json!({"asset_id":asset,"assume-uncorrected":true}),
                token: None,
            },
        )?;
        match response.error {
            Some(error) if error.code == "not-ready" && started.elapsed() < INDEX_DEADLINE => {
                std::thread::sleep(JOB_POLL);
            }
            Some(error) => {
                return Err(format!("Initial lens query: {} {}", error.code, error.message).into());
            }
            None => {
                return response
                    .result
                    .ok_or_else(|| "Initial lens query answered no result".into());
            }
        }
    }
}

struct Workload {
    owner: Owner,
    client: ClientId,
    asset: AssetId,
    baseline: PreviewJob,
    corrected: PreviewJob,
    query: Value,
    candidate: Value,
    frozen: Value,
}

impl Workload {
    fn new(source: &Path, catalog: &Path) -> Result<Self> {
        let owner = Owner::start(catalog, ModuleRegistry::builtin(), ACTOR)?;
        let client = owner.client();
        // Developed and prepared as a client opens a file (`pick.develop`, then `source.prepare`).
        let opened = owner.open(client, source)?;
        let asset: AssetId = serde_json::from_value(opened["asset"]["id"].clone())?;
        let prepared = owner.call(client, "source.prepare", json!({"asset_id":asset}))?;
        if !prepared["job_id"].is_null() {
            require_status(&terminal(&owner, client, &prepared["job_id"])?, "ready")?;
        }
        let query = initial_query(&owner, client, &asset)?;
        let candidate = candidate(&query)?;
        let revision = owner.revision(client, &json!(asset))?;
        let selected = owner.call(
            client,
            "edit.select-lens-profile",
            json!({"asset_id":asset,"profile":candidate["key"],"assume-uncorrected":true,"mutation":envelope(Some(revision))}),
        )?;
        ensure(
            selected["outcome"] == "applied",
            "Initial lens selection did not commit",
        )?;
        let frozen = frozen_profile(&owner.state(client, &json!(asset))?)?;
        for (method, mut parameters) in [
            (
                "edit.set-perspective",
                json!({"horizontal":PERSPECTIVE.0,"vertical":PERSPECTIVE.1}),
            ),
            (
                "edit.crop-fit",
                json!({"aspect":"original","angle":CROP_DEGREES}),
            ),
        ] {
            parameters["asset_id"] = json!(asset);
            parameters["mutation"] = envelope(Some(owner.revision(client, &json!(asset))?));
            let changed = owner.call(client, method, parameters)?;
            ensure(
                changed["outcome"] == "applied",
                format!("{method} did not commit"),
            )?;
        }
        let corrected = owner.preview_job(PreviewRequest::new(client, asset.clone()))?;
        let reset = owner.call(client, "edit.reset-lens-profile", json!({"asset_id":asset,"mutation":envelope(Some(owner.revision(client,&json!(asset))?))}))?;
        ensure(
            reset["outcome"] == "applied",
            "Warm baseline lens reset did not commit",
        )?;
        let baseline = owner.preview_job(PreviewRequest::new(client, asset.clone()))?;
        let restored = owner.call(client, "edit.select-lens-profile", json!({"asset_id":asset,"profile":candidate["key"],"assume-uncorrected":true,"mutation":envelope(Some(owner.revision(client,&json!(asset))?))}))?;
        ensure(
            restored["outcome"] == "applied",
            "Warm corrected selection did not commit",
        )?;
        let state = owner.state(client, &json!(asset))?;
        ensure(
            frozen_profile(&state)? == frozen
                && state["current_entry"]["snapshot"]["recipe"]
                    == serde_json::to_value(corrected.evaluation.recipe())?,
            "Warm corrected recipe changed when restoring Lens",
        )?;
        ensure(
            corrected.evaluation.source().dimensions() == baseline.evaluation.source().dimensions()
                && (corrected.identity.width, corrected.identity.height)
                    == (baseline.identity.width, baseline.identity.height),
            "Lens changed the baseline content or output dimensions",
        )?;
        Ok(Self {
            owner,
            client,
            asset,
            baseline,
            corrected,
            query,
            candidate,
            frozen,
        })
    }

    fn state(&self) -> Result<Value> {
        Ok(self.owner.state(self.client, &json!(self.asset))?)
    }

    fn edit(&self, method: &str, mut parameters: Value) -> Result<Value> {
        parameters["asset_id"] = json!(self.asset);
        parameters["mutation"] =
            envelope(Some(self.owner.revision(self.client, &json!(self.asset))?));
        Ok(self.owner.call(self.client, method, parameters)?)
    }

    fn query(&self) -> Result<Value> {
        Ok(self.owner.call(
            self.client,
            "query.lens-profiles",
            json!({"asset_id":self.asset,"assume-uncorrected":true}),
        )?)
    }

    fn point(&self) -> (u32, u32) {
        (
            self.corrected.identity.width / 2,
            self.corrected.identity.height / 2,
        )
    }

    fn pick(&self) -> Result<Value> {
        let (x, y) = self.point();
        Ok(self.owner.call(
            self.client,
            "render.sample",
            json!({"asset_id":self.asset,"x":x,"y":y}),
        )?)
    }

    fn export(&self, destination: &Path) -> Result<Value> {
        Ok(self.owner.call(
            self.client,
            "export.jpeg",
            json!({"asset_id":self.asset,"destination":destination,"mutation":envelope(None)}),
        )?)
    }
}

/// The public exact-render entry over the immutable evaluation the owner planned. Both domains
/// borrow the same cached pixels; this includes stack compilation and terminal SDR rendition,
/// and excludes owner planning, desktop scheduling, GPU upload and presentation.
fn exact(job: &PreviewJob) -> Result<Raster> {
    let evaluation = &job.evaluation;
    Ok(render(
        evaluation.registry(),
        evaluation.source(),
        evaluation.recipe(),
        RenderOptions::default(),
        evaluation.context(),
    )?
    .frame(evaluation.entry().snapshot.id.clone())?)
}

fn validate_frame(job: &PreviewJob, frame: &Raster) -> Result {
    ensure(
        (frame.width, frame.height) == (job.identity.width, job.identity.height)
            && frame.source_fingerprint == job.identity.source_fingerprint
            && !frame.rgba.is_empty(),
        "Exact render does not match its frozen evaluation",
    )
}

fn validate_pick(job: &PreviewJob, picked: &Value, expected: [u8; 4]) -> Result {
    ensure(
        picked["rgba"] == json!(expected)
            && picked["source_fingerprint"] == job.identity.source_fingerprint
            && picked["width"] == job.identity.width
            && picked["height"] == job.identity.height
            && picked["source_detail_ready"] == true,
        format!("Point pick differs from the exact corrected frame: {picked}"),
    )
}

fn render_sample(job: &PreviewJob) -> Result<f64> {
    let started = Instant::now();
    let frame = exact(job)?;
    let elapsed = milliseconds(started);
    validate_frame(job, &frame)?;
    // The frame is released before the next render or export, so no diagnostic retains a second
    // full corrected output. Core allocation and source limits apply unchanged.
    drop(frame);
    Ok(elapsed)
}

#[derive(Default)]
struct Measurements {
    query: Vec<f64>,
    select: Vec<f64>,
    baseline: Vec<f64>,
    corrected: Vec<f64>,
    pick: Vec<f64>,
    export: Vec<f64>,
    cancel: Vec<f64>,
}

impl Measurements {
    fn rows(&self) -> Vec<Value> {
        [
            &self.query,
            &self.select,
            &self.baseline,
            &self.corrected,
            &self.pick,
            &self.export,
            &self.cancel,
        ]
        .into_iter()
        .zip(METRICS)
        .map(|(samples, metric)| stats::row(metric, "ms", samples.iter().copied()))
        .collect()
    }

    fn validate(&self, count: usize) -> Result {
        for row in self.rows() {
            ensure(
                row["distribution"]["count"] == count,
                format!("Missing samples: {row}"),
            )?;
        }
        Ok(())
    }
}

fn resource_rows(before: &Value, after: &Value, rss: impl IntoIterator<Item = f64>) -> Vec<Value> {
    let delta = |field: &str| {
        before["cpu"][field]
            .as_u64()
            .zip(after["cpu"][field].as_u64())
            .and_then(|(a, b)| b.checked_sub(a))
            .map(|value| value as f64 / 1e9)
    };
    let mib = |pointer: &str| {
        after
            .pointer(pointer)
            .and_then(Value::as_u64)
            .map(|value| value as f64 / 1048576.0)
    };
    vec![
        stats::scalar("process_cpu", "s", delta("time_ns")),
        stats::row("process_rss_between_operations", "MiB", rss),
        stats::scalar("process_memory_peak", "MiB", mib("/memory/peak_bytes")),
        stats::scalar(
            "core_colour_scratch_high_water",
            "MiB",
            mib("/budgets/colour_scratch/peak_bytes"),
        ),
        stats::scalar(
            "core_spatial_scratch_high_water",
            "MiB",
            mib("/budgets/spatial/peak_bytes"),
        ),
    ]
}

fn measure(
    root: &Path,
    source: &Path,
    out: &Path,
    count: usize,
    source_hash: &str,
) -> Result<Value> {
    // Declare the temporary workspace before the owner so error unwinding stops and joins all
    // jobs before it removes their destinations. Successful runs close both explicitly below.
    let exports = tempfile::Builder::new()
        .prefix("exports-")
        .tempdir_in(out)?;
    let workload = Workload::new(source, &out.join("catalog.sqlite"))?;
    let baseline = exact(&workload.baseline)?;
    validate_frame(&workload.baseline, &baseline)?;
    let baseline_hash = format!("{:x}", Sha256::digest(baseline.rgba.as_slice()));
    drop(baseline);
    let corrected = exact(&workload.corrected)?;
    validate_frame(&workload.corrected, &corrected)?;
    let corrected_hash = format!("{:x}", Sha256::digest(corrected.rgba.as_slice()));
    let (x, y) = workload.point();
    let expected_pick = corrected.pixel(x, y).ok_or("No corrected centre pixel")?;
    drop(corrected);
    validate_pick(&workload.corrected, &workload.pick()?, expected_pick)?;
    let state = workload.state()?;
    let recipe = state["current_entry"]["snapshot"]["recipe"].clone();
    let domain = match workload.corrected.evaluation.source() {
        PreviewSource::Jpeg(_) => "jpeg-srgb-8bit",
        PreviewSource::Raw { .. } => "raw-planar-linear-float",
    };
    let before = workload
        .owner
        .call(workload.client, "resources.read", json!({}))?;
    let load_before = launch::load_average(root);
    let started = Instant::now();
    let mut measured = Measurements::default();
    let mut jobs = Vec::with_capacity(count * 2);
    let mut rss = Vec::with_capacity(count + 2);
    if let Some(bytes) = before["memory"]["resident_bytes"].as_u64() {
        rss.push(bytes as f64 / 1048576.0);
    }
    for index in 0..count {
        let query_started = Instant::now();
        let query = workload.query()?;
        measured.query.push(milliseconds(query_started));
        ensure(
            query["status"]["detected"]["key"] == workload.candidate["key"]
                && query["status"]["detected"]["eligible"] == true,
            "Frozen candidate is no longer the eligible detected profile",
        )?;
        let reset = workload.edit("edit.reset-lens-profile", json!({}))?;
        ensure(
            reset["outcome"] == "applied",
            "Lens reset did not commit between selection samples",
        )?;
        let revision = workload
            .owner
            .revision(workload.client, &json!(workload.asset))?;
        let parameters = json!({"asset_id":workload.asset,"profile":workload.candidate["key"],"assume-uncorrected":true,"mutation":envelope(Some(revision))});
        let select_started = Instant::now();
        let selected =
            workload
                .owner
                .call(workload.client, "edit.select-lens-profile", parameters)?;
        measured.select.push(milliseconds(select_started));
        ensure(
            selected["outcome"] == "applied",
            "Measured selection was a no-op",
        )?;
        let selected_state = workload.state()?;
        ensure(
            frozen_profile(&selected_state)? == workload.frozen,
            "Reselection changed frozen lens terms",
        )?;
        ensure(
            selected_state["current_entry"]["snapshot"]["recipe"] == recipe,
            "Reselection changed the corrected recipe",
        )?;
        // Alternate the paired order so one domain's baseline and corrected work share the same
        // warm prepared source without always giving one case the second pass.
        if index % 2 == 0 {
            measured.baseline.push(render_sample(&workload.baseline)?);
            measured.corrected.push(render_sample(&workload.corrected)?);
        } else {
            measured.corrected.push(render_sample(&workload.corrected)?);
            measured.baseline.push(render_sample(&workload.baseline)?);
        }
        let pick_started = Instant::now();
        let picked = workload.pick()?;
        measured.pick.push(milliseconds(pick_started));
        validate_pick(&workload.corrected, &picked, expected_pick)?;

        let destination = exports.path().join(format!("completed-{index}.jpg"));
        let export_started = Instant::now();
        let queued = workload.export(&destination)?;
        let completed = terminal(&workload.owner, workload.client, &queued["job_id"])?;
        measured.export.push(milliseconds(export_started));
        require_status(&completed, "ready")?;
        ensure(
            completed["result"]["width"] == workload.corrected.identity.width
                && completed["result"]["height"] == workload.corrected.identity.height
                && completed["result"]["bytes"]
                    .as_u64()
                    .is_some_and(|bytes| bytes > 0)
                && destination.is_file(),
            format!("Export did not publish the corrected output: {completed}"),
        )?;
        fs::remove_file(&destination)?;
        jobs.push(json!({"sample":index,"operation":"export","accepted":queued,"terminal":completed,"destination_removed":true}));

        let destination = exports.path().join(format!("cancelled-{index}.jpg"));
        let queued = workload.export(&destination)?;
        let cancel_started = Instant::now();
        let requested = workload.owner.call(
            workload.client,
            "job.cancel",
            json!({"job_id":queued["job_id"]}),
        )?;
        let cancelled = terminal(&workload.owner, workload.client, &queued["job_id"])?;
        measured.cancel.push(milliseconds(cancel_started));
        require_status(&cancelled, "cancelled")?;
        ensure(
            !destination.exists(),
            "Cancelled export published a destination",
        )?;
        ensure(
            fs::read_dir(exports.path())?.next().is_none(),
            "Export lane retained a staged or published file",
        )?;
        jobs.push(json!({"sample":index,"operation":"cancel","accepted":queued,"cancellation":requested,"terminal":cancelled,"destination_absent":true}));
        let resources = workload
            .owner
            .call(workload.client, "resources.read", json!({}))?;
        if let Some(bytes) = resources["memory"]["resident_bytes"].as_u64() {
            rss.push(bytes as f64 / 1048576.0);
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let after = workload
        .owner
        .call(workload.client, "resources.read", json!({}))?;
    let load_after = launch::load_average(root);
    measured.validate(count)?;
    ensure(
        hash(source)? == source_hash,
        "Lens performance changed the original source",
    )?;
    let mut rows = measured.rows();
    rows.extend(resource_rows(&before, &after, rss));
    rows.push(stats::scalar("measurement_duration", "s", Some(elapsed)));
    ensure(
        after["budgets"]["colour_scratch"]["in_use_bytes"] == 0
            && after["budgets"]["spatial"]["in_use_bytes"] == 0,
        "Core scratch still retained after settled work",
    )?;
    let result = json!({
        "format":1,"status":"passed","mode":"lens-only",
        "profile":if cfg!(debug_assertions){"debug"}else{"release"},"platform":host(root)?,
        "source":source,"source_sha256":source_hash,"source_preserved":true,
        "source_dimensions":workload.baseline.evaluation.source().dimensions(),"source_domain":domain,
        "samples_per_operation":count,"cache":"One imported, verified prepared source; full baseline and corrected frame warmups and one point warmup precede timing; no cold decode figures",
        "query":workload.query,"candidate":workload.candidate,"frozen_lens_payload":workload.frozen,
        "geometry":{"perspective":{"horizontal":PERSPECTIVE.0,"vertical":PERSPECTIVE.1},"crop_angle_deg":CROP_DEGREES,"crop_aspect":"original"},
        "baseline_identity":workload.baseline.identity,"corrected_identity":workload.corrected.identity,"corrected_recipe":recipe,
        "frame_sha256":{"warm_baseline":baseline_hash,"corrected":corrected_hash},"point":{"x":x,"y":y,"rgba":expected_pick,"matches_exact_frame":true},
        "load":{"before":launch::load(load_before),"after":launch::load(load_after)},
        "resources":{"before":before,"after":after,"rss_scope":"Resident set sampled between operation groups; process high-water is reported separately by the platform when available; GPU counters are unavailable for this headless diagnostic"},
        "method":{"query":"API request through owner; assume-uncorrected carried explicitly","selection":"API request and durable commit; reset and revision reads excluded; each selection must apply and freeze the same payload","render":"Current-build paired comparison: neutral Lens + Perspective + crop versus the same stack with Lens selected; public exact core entry over frozen owner evaluations, including compilation and SDR rendition; alternating paired order; one output retained at a time","pick":"One API render.sample at the centre of the current corrected output, checked against its exact rendered byte","export":"API admission through ready job.read after render, JPEG encode and publication; generated file removed outside timing","cancel":"Immediate job.cancel after API export admission through terminal cancelled job.read; admission excluded, no destination or staging file may remain","job_poll_ms":JOB_POLL.as_millis(),"job_deadline_s":JOB_DEADLINE.as_secs(),"scope":"Core and API only; excludes desktop, GPU upload and presentation"},
        "bounded_work":{"samples_max":MAX_SAMPLES,"exports_in_flight_max":1,"export_directory_empty":true,"owner_stopped_and_joined":true},
        "jobs":jobs,"rows":rows,
    });
    drop(workload.baseline);
    drop(workload.corrected);
    workload.owner.close()?;
    exports.close()?;
    Ok(result)
}

pub fn run(root: &Path, source: &Path, out: &Path, samples: usize) -> Result {
    ensure(!out.exists(), "Lens performance output must be new")?;
    ensure(
        (1..=MAX_SAMPLES).contains(&samples),
        "Lens performance samples must be 1..100",
    )?;
    let source = source.canonicalize()?;
    let source_hash = hash(&source)?;
    fs::create_dir_all(out)?;
    match measure(root, &source, out, samples, &source_hash) {
        Ok(result) => {
            write_json(&out.join("result.json"), &result)?;
            println!("Lens performance evidence: {}", out.display());
            Ok(())
        }
        Err(error) => {
            write_json(
                &out.join("result.json"),
                &json!({"format":1,"status":"failed","mode":"lens-only","source":source,"source_sha256":source_hash,"source_preserved":hash(&source).ok().as_deref()==Some(source_hash.as_str()),"error":error.to_string()}),
            )?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_selection_requires_an_eligible_returned_key() {
        let detected = |key: &str, eligible: bool| json!({"rows":[],"status":{"detected":{"key":key,"eligible":eligible}}});
        assert_eq!(
            candidate(&detected("selected", true)).unwrap()["key"],
            "selected"
        );
        assert!(candidate(&detected("refused", false)).is_err());
        assert!(candidate(&detected("", true)).is_err());
        assert!(candidate(&json!({"rows":[],"status":{"detected":null}})).is_err());
        // A listed row is never a stand-in for the detected profile.
        assert!(candidate(&json!({"rows":[{"key":"listed","eligible":true}]})).is_err());
    }

    #[test]
    fn terminal_contract_does_not_claim_a_ready_race_was_cancelled() {
        assert!(require_status(&json!({"status":"ready"}), "cancelled").is_err());
        assert!(require_status(&json!({"status":"failed"}), "ready").is_err());
        assert!(require_status(&json!({"status":"cancelled"}), "cancelled").is_ok());
    }

    #[test]
    fn every_operation_has_a_complete_distribution() {
        let values = vec![3.0, 1.0, 5.0, 4.0, 2.0];
        let mut samples = Measurements {
            query: values.clone(),
            select: values.clone(),
            baseline: values.clone(),
            corrected: values.clone(),
            pick: values.clone(),
            export: values.clone(),
            cancel: values,
        };
        samples.validate(5).unwrap();
        let rows = samples.rows();
        assert_eq!(rows.len(), METRICS.len());
        for row in rows {
            assert_eq!(row["distribution"]["p50"], 3.0);
            assert_eq!(row["distribution"]["p95"], 5.0);
        }
        samples.cancel.pop();
        assert!(samples.validate(5).is_err());
    }

    #[test]
    fn small_fixture_setup_freezes_geometry_and_pick_matches_exact_frame() {
        // This proves setup and point correctness only; it runs no timing samples or exports.
        let directory = tempfile::tempdir().unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixtures/geometry/z6-24-70-35mm-grid.jpg");
        let source_hash = hash(&source).unwrap();
        let workload = Workload::new(&source, &directory.path().join("catalog.sqlite")).unwrap();
        assert!(matches!(
            workload.corrected.evaluation.source(),
            PreviewSource::Jpeg(_)
        ));
        assert_eq!(workload.frozen["profile"]["focal"]["source"], "exif");
        let frame = exact(&workload.corrected).unwrap();
        validate_frame(&workload.corrected, &frame).unwrap();
        let (x, y) = workload.point();
        validate_pick(
            &workload.corrected,
            &workload.pick().unwrap(),
            frame.pixel(x, y).unwrap(),
        )
        .unwrap();
        let recipe = workload.corrected.evaluation.recipe();
        assert!(
            workload
                .baseline
                .evaluation
                .recipe()
                .layers
                .iter()
                .any(|layer| layer.effect_id == LENS_EFFECT && layer.payload["profile"].is_null())
        );
        assert_eq!(
            (
                workload.baseline.identity.width,
                workload.baseline.identity.height
            ),
            (
                workload.corrected.identity.width,
                workload.corrected.identity.height
            )
        );
        assert!(
            recipe
                .layers
                .iter()
                .any(|layer| layer.effect_id == luxforge_core::PERSPECTIVE_EFFECT
                    && layer.payload["horizontal"] == PERSPECTIVE.0)
        );
        assert!(
            recipe
                .layers
                .iter()
                .any(|layer| layer.effect_id == luxforge_core::CROP_EFFECT
                    && layer.payload["angle"] == CROP_DEGREES)
        );
        assert_eq!(hash(&source).unwrap(), source_hash);
    }
}
