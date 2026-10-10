//! Paired HSL-only and grading costs through the public owner and whole-frame reference renderer.
//! The two workloads alternate sample by sample on one owner and one prepared photo, so a drift in
//! the host's load or the process's state lands on both sides alike rather than on whichever ran
//! second.
use crate::*;
use luxforge_core::{AssetId, ModuleRegistry, PreviewRequest, RenderOptions, render};
use luxforge_testkit::client::{self, Owner};
use std::time::Instant;

const ACTOR: &str = "xtask-grade-performance";

/// The grading fields the graded workload sets, and their values there; the HSL-only workload
/// sets each back to its default (zero), so every commit changes the photo whichever came before.
const GRADE: [(&str, f64); 5] = [
    ("grade-shadows-hue", 210.0),
    ("grade-shadows-saturation", 30.0),
    ("grade-highlights-hue", 45.0),
    ("grade-highlights-saturation", 20.0),
    ("grade-global-luminance", 5.0),
];

/// One workload's complete `edit.set-mixer` fields: the shared HSL edit, and the grade or its
/// defaults.
fn payload(graded: bool) -> Value {
    let mut fields = json!({"red-hue":20.0,"blue-saturation":15.0});
    for (name, value) in GRADE {
        fields[name] = json!(if graded { value } else { 0.0 });
    }
    fields
}

/// What one workload measured, sample by sample.
#[derive(Default)]
struct Workload {
    commits: Vec<f64>,
    renders: Vec<f64>,
    picks: Vec<f64>,
    exports: Vec<f64>,
    rss: Vec<f64>,
    export_jobs: Vec<Value>,
    /// The stored recipe after the workload's last commit.
    recipe: Value,
}

fn milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// The one owner both workloads run through, its client, the prepared photo and the folder its
/// exports pass through.
struct Session<'a> {
    owner: &'a Owner,
    client: luxforge_core::ClientId,
    asset: &'a AssetId,
    out: &'a Path,
}

/// One sample of one workload: commit its fields, render the whole frame, check a centre sample
/// against it and export it; recorded unless it is the warm-up.
fn sample(
    session: &Session,
    (graded, name): (bool, &str),
    index: usize,
    workload: &mut Workload,
) -> Result {
    let Session {
        owner,
        client,
        asset,
        out,
    } = *session;
    let mut params = payload(graded);
    params["asset_id"] = json!(asset);
    params["mutation"] = client::mutation(
        owner.revision(client, &json!(asset))?,
        &client::request_id(ACTOR),
        ACTOR,
    );
    let started = Instant::now();
    let committed = owner.call(client, "edit.set-mixer", params)?;
    let commit_ms = milliseconds(started);
    ensure(
        committed["outcome"] == "applied",
        "Measured mixer commit was a no-op",
    )?;
    let job = owner.preview_job(PreviewRequest::new(client, asset.clone()))?;
    let e = &job.evaluation;
    let started = Instant::now();
    let frame = render(
        e.registry(),
        e.source(),
        e.recipe(),
        RenderOptions::default(),
        e.context(),
    )?
    .frame(e.entry().snapshot.id.clone())?;
    let render_ms = milliseconds(started);
    let (x, y) = (frame.width / 2, frame.height / 2);
    let expected = frame.pixel(x, y).ok_or("No centre pixel")?;
    let started = Instant::now();
    let picked = owner.sample(client, &json!(asset), (x, y), None)?;
    let pick_ms = milliseconds(started);
    ensure(
        picked == json!(expected),
        "Measured sample differs from the whole-frame reference",
    )?;
    drop(frame);
    let destination = out.join(format!("{name}-{index}.jpg"));
    let started = Instant::now();
    let queued = owner.call(
        client,
        "export.jpeg",
        json!({"asset_id":asset,"destination":destination,
        "mutation":{"request_id":client::request_id(ACTOR),"actor":ACTOR}}),
    )?;
    let completed = client::settle(owner, client, &queued["job_id"])?;
    let export_ms = milliseconds(started);
    ensure(
        completed["status"] == "ready" && destination.exists(),
        "Measured export failed",
    )?;
    fs::remove_file(&destination)?;
    if index > 0 {
        workload.commits.push(commit_ms);
        workload.renders.push(render_ms);
        workload.picks.push(pick_ms);
        workload.exports.push(export_ms);
        workload.export_jobs.push(completed);
        let resources = owner.call(client, "resources.read", json!({}))?;
        if let Some(bytes) = resources["memory"]["resident_bytes"].as_u64() {
            workload.rss.push(bytes as f64 / 1048576.0);
        }
    }
    Ok(())
}

fn measure(
    root: &Path,
    source: &Path,
    out: &Path,
    samples: usize,
    source_hash: &str,
) -> Result<Value> {
    let load_before = launch::load_average(root);
    let owner = Owner::start(
        &out.join("catalog.sqlite"),
        ModuleRegistry::builtin(),
        ACTOR,
    )?;
    let client = owner.client();
    let opened = owner.open(client, source)?;
    let asset: AssetId = serde_json::from_value(opened["asset"]["id"].clone())?;
    owner.prepare(client, &json!(asset))?;
    let session = Session {
        owner: &owner,
        client,
        asset: &asset,
        out,
    };
    let names = [(false, "hsl_only"), (true, "hsl_grading")];
    let mut workloads = [Workload::default(), Workload::default()];
    // One warm-up of each, then the samples, alternating HSL alone and HSL plus grading.
    for index in 0..=samples {
        for (kind, workload) in names.iter().zip(&mut workloads) {
            sample(&session, *kind, index, workload)?;
            if index == samples {
                workload.recipe = serde_json::to_value(owner.recipe(client, &json!(asset))?)?;
            }
        }
    }
    let resources = owner.call(client, "resources.read", json!({}))?;
    owner.close()?;
    let load_after = launch::load_average(root);
    ensure(
        hash(source)? == source_hash,
        "Grading measurement changed the source",
    )?;
    let mut rows = Vec::new();
    let mut reports = Vec::new();
    for ((graded, name), workload) in names.iter().zip(workloads) {
        for (metric, samples) in [
            ("commit", workload.commits),
            ("reference_render", workload.renders),
            ("reference_sample", workload.picks),
            ("reference_export", workload.exports),
        ] {
            rows.push(stats::row(&format!("{name}_{metric}"), "ms", samples));
        }
        rows.push(stats::row(
            &format!("{name}_rss_between_operations"),
            "MiB",
            workload.rss,
        ));
        reports.push(json!({"name":name,"payload":payload(*graded),"recipe":workload.recipe,"exports":workload.export_jobs}));
    }
    Ok(json!({
        "format":1,"status":"passed",
        "profile":if cfg!(debug_assertions) {"debug"} else {"release"},"platform":host(root)?,
        "binary_sha256":hash(&std::env::current_exe()?)?,"lockfile_sha256":hash(&root.join("Cargo.lock"))?,
        "source":source,"source_sha256":source_hash,"source_preserved":true,
        "source_dimensions":[opened["asset"]["width"],opened["asset"]["height"]],"samples":samples,
        "load":{"before":launch::load(load_before),"after":launch::load(load_after)},
        "method":"One owner and one prepared original; one warm-up of each workload, then the samples alternating HSL alone and HSL plus grading, each commit setting the workload's full fields so it changes the photo. Commits through edit.set-mixer, whole-frame CPU reference render, render.sample checked against it, serial export.jpeg completion. Headless owner has no GPU tile service: samples and exports use the reference renderer. RSS is sampled between operations, not peak memory. No desktop presentation measured.",
        "rows":rows,"workloads":reports,"resources":resources,
    }))
}

pub fn run(root: &Path, source: &Path, out: &Path, samples: usize) -> Result {
    ensure(!out.exists(), "Grading measurement output must be new")?;
    ensure(
        (1..=100).contains(&samples),
        "Grading samples must be 1..100",
    )?;
    let source = source.canonicalize()?;
    let source_hash = hash(&source)?;
    fs::create_dir_all(out)?;
    match measure(root, &source, out, samples, &source_hash) {
        Ok(result) => {
            write_json(&out.join("result.json"), &result)?;
            println!("PASS grading performance: {}", out.display());
            Ok(())
        }
        Err(error) => {
            write_json(
                &out.join("result.json"),
                &json!({"format":1,"status":"failed","source":source,"source_sha256":source_hash,"source_preserved":hash(&source).ok().as_deref()==Some(source_hash.as_str()),"error":error.to_string()}),
            )?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paired_workloads_complete_exports_and_preserve_the_source() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let source = root.join("fixtures/s0/orientation-1.jpg");
        let directory = tempfile::tempdir().unwrap();
        let out = directory.path().join("paired");
        run(&root, &source, &out, 1).unwrap();
        let report = read_json(&out.join("result.json")).unwrap();
        assert_eq!(report["status"], "passed");
        assert_eq!(report["workloads"].as_array().unwrap().len(), 2);
        assert_eq!(report["rows"].as_array().unwrap().len(), 10);
        assert!(
            report["workloads"]
                .as_array()
                .unwrap()
                .iter()
                .all(|workload| workload["exports"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|job| job["status"] == "ready"))
        );
        // Each workload's last recipe is its own: the grade stored only on the graded side.
        let stored = |index: usize| report["workloads"][index]["recipe"].to_string();
        assert!(!stored(0).contains("grade-shadows-hue"), "{}", stored(0));
        assert!(stored(1).contains("grade-shadows-hue"), "{}", stored(1));
    }

    #[test]
    fn grading_workload_keeps_the_same_hsl_baseline_and_resets_the_grade() {
        let baseline = payload(false);
        let graded = payload(true);
        let hsl = |fields: &Value| {
            let mut fields = fields.clone();
            fields
                .as_object_mut()
                .unwrap()
                .retain(|name, _| !name.starts_with("grade-"));
            fields
        };
        assert_eq!(hsl(&baseline), hsl(&graded));
        assert_ne!(baseline, graded);
        for (name, _) in GRADE {
            assert_eq!(baseline[name], 0.0, "{name} is not at its default");
        }
    }
}
