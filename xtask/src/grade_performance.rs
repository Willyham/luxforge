//! Paired HSL-only and grading costs through the public owner and whole-frame reference renderer.
use crate::*;
use luxforge_core::{AssetId, ModuleRegistry, PreviewRequest, RenderOptions, render};
use luxforge_testkit::client::{self, Owner};
use std::time::Instant;

const ACTOR: &str = "xtask-grade-performance";

fn payload(graded: bool, index: usize) -> Value {
    let mut fields = json!({"red-hue":20.0,"blue-saturation":15.0});
    if graded {
        fields["grade-shadows-hue"] = json!(210.0);
        fields["grade-shadows-saturation"] = json!(30.0);
        fields["grade-highlights-hue"] = json!(45.0);
        fields["grade-highlights-saturation"] = json!(20.0);
        fields["grade-global-luminance"] = json!(5.0);
    }
    // Alternate one HSL field so every commit is a real change in both workloads.
    fields["red-hue"] = json!(20.0 + (index % 2) as f64);
    fields
}

pub fn run(root: &Path, source: &Path, out: &Path, samples: usize) -> Result {
    ensure(!out.exists(), "Grading measurement output must be new")?;
    ensure(
        (1..=100).contains(&samples),
        "Grading samples must be 1..100",
    )?;
    fs::create_dir_all(out)?;
    let source = source.canonicalize()?;
    let source_hash = hash(&source)?;
    let load_start = launch::load_average(root);
    let mut rows = Vec::new();
    let mut workloads = Vec::new();
    // Each workload has its own catalog and prepared source. Only one owner is alive at a time.
    for graded in [false, true] {
        let name = if graded { "hsl_grading" } else { "hsl_only" };
        let owner = Owner::start(
            &out.join(format!("{name}.sqlite")),
            ModuleRegistry::builtin(),
            ACTOR,
        )?;
        let client = owner.client();
        let opened = owner.open(client, &source)?;
        let asset: AssetId = serde_json::from_value(opened["asset"]["id"].clone())?;
        owner.prepare(client, &json!(asset))?;
        let mut commits = Vec::new();
        let mut renders = Vec::new();
        let mut picks = Vec::new();
        let mut exports = Vec::new();
        let mut rss = Vec::new();
        let mut export_jobs = Vec::new();
        for index in 0..=samples {
            let mut params = payload(graded, index);
            params["asset_id"] = json!(asset);
            params["mutation"] = client::mutation(
                owner.revision(client, &json!(asset))?,
                &client::request_id(ACTOR),
                ACTOR,
            );
            let started = Instant::now();
            let committed = owner.call(client, "edit.set-mixer", params)?;
            let commit_ms = started.elapsed().as_secs_f64() * 1000.0;
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
            let render_ms = started.elapsed().as_secs_f64() * 1000.0;
            let (x, y) = (frame.width / 2, frame.height / 2);
            let expected = frame.pixel(x, y).ok_or("No centre pixel")?;
            let started = Instant::now();
            let picked = owner.sample(client, &json!(asset), (x, y), None)?;
            let pick_ms = started.elapsed().as_secs_f64() * 1000.0;
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
            let completed = client::settle(&owner, client, &queued["job_id"])?;
            let export_ms = started.elapsed().as_secs_f64() * 1000.0;
            ensure(
                completed["status"] == "ready" && destination.exists(),
                "Measured export failed",
            )?;
            fs::remove_file(&destination)?;
            if index > 0 {
                commits.push(commit_ms);
                renders.push(render_ms);
                picks.push(pick_ms);
                exports.push(export_ms);
                export_jobs.push(completed);
                let resources = owner.call(client, "resources.read", json!({}))?;
                if let Some(bytes) = resources["memory"]["resident_bytes"].as_u64() {
                    rss.push(bytes as f64 / 1048576.0);
                }
            }
        }
        let recipe = owner.recipe(client, &json!(asset))?;
        let resources = owner.call(client, "resources.read", json!({}))?;
        for (metric, samples) in [
            ("commit", commits),
            ("reference_render", renders),
            ("reference_sample", picks),
            ("reference_export", exports),
        ] {
            rows.push(stats::row(&format!("{name}_{metric}"), "ms", samples));
        }
        rows.push(stats::row(
            &format!("{name}_rss_between_operations"),
            "MiB",
            rss,
        ));
        workloads
            .push(json!({"name":name,"source_dimensions":[opened["asset"]["width"],opened["asset"]["height"]],"recipe":recipe,"resources":resources,"exports":export_jobs}));
        owner.close()?;
    }
    ensure(
        hash(&source)? == source_hash,
        "Grading measurement changed the source",
    )?;
    write_json(
        &out.join("measurements.json"),
        &json!({
            "status":"passed","platform":host(root)?,"profile":if cfg!(debug_assertions) {"debug"} else {"release"},
            "source":source,"source_sha256":source_hash,"samples":samples,
        "binary_sha256":hash(&std::env::current_exe()?)?,"lockfile_sha256":hash(&root.join("Cargo.lock"))?,
            "load":launch::load(load_start),"load_average_1m_end":launch::load_average(root),
            "method":"One warm-up per workload, sequential HSL-only then HSL plus grading; original prepared once per owner. Commits through edit.set-mixer, whole-frame CPU reference render, render.sample checked against it, serial export.jpeg completion. Headless owner has no GPU tile service: samples and exports use the reference renderer. RSS is sampled between operations, not peak memory. No desktop presentation measured.",
            "rows":rows,"workloads":workloads,
        }),
    )?;
    println!("PASS grading performance: {}", out.display());
    Ok(())
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
        let report = read_json(&out.join("measurements.json")).unwrap();
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
    }

    #[test]
    fn grading_workload_keeps_the_same_hsl_baseline_and_changes_each_commit() {
        let baseline = payload(false, 0);
        let mut graded = payload(true, 0);
        graded
            .as_object_mut()
            .unwrap()
            .retain(|name, _| !name.starts_with("grade-"));
        assert_eq!(baseline, graded);
        assert_ne!(payload(false, 0), payload(false, 1));
        assert_ne!(payload(true, 0), payload(true, 1));
    }
}
