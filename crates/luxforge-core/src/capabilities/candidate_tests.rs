//! The candidate seam end to end through the catalog owner's JSON methods, driven by the developer
//! producer (`luxforge.candidates`) exactly as a client drives a task: publication through the
//! task, `job.read`'s listing, `job.read-candidate`'s chunks against an independent conversion,
//! `job.release-candidates` with retries and from another client, a disconnect that releases
//! nothing, cancels of a running and of a finished task, the bounds a producer meets, a held
//! `job.wait` that a candidate change answers, discovery, and the files a closed or crashed process
//! leaves. The store's own budgets, pins and the accept seam are tested in
//! `artifacts/candidates_tests.rs`.
use super::{host::HostConfig, testing::Owner, testing::temp};
use crate::{
    AssetId, EditorService, ModuleRegistry,
    jobs::{JOB_CANCEL, JOB_READ},
    modules::{
        CANDIDATE_PROOF_MODULE, CANDIDATE_PROOF_TASK, CandidateProofModule, PROOF_PATCH_LABEL,
        proof_code, proof_coverage, proof_linear, proof_preview,
    },
};
use luxforge_testbase::{Gate, wait_for};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

const READ_CANDIDATE: &str = "job.read-candidate";
const RELEASE: &str = "job.release-candidates";
const CHUNK: u64 = 768 * 1024;

fn task() -> String {
    format!("task.{CANDIDATE_PROOF_TASK}")
}

/// A catalog with one imported photograph, and the gate the producer passes after each candidate.
struct Fixture {
    root: PathBuf,
    gate: Arc<Gate>,
    asset: AssetId,
    revision: u64,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = temp(name);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let gate = Arc::new(Gate::new());
        let photo =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
        let mut service =
            EditorService::open_with(&root.join("catalog.sqlite"), Self::registry_with(&gate))
                .unwrap();
        let state = service.import(&photo).unwrap();
        Self {
            root,
            gate,
            asset: state.asset.id,
            revision: state.revision,
        }
    }

    fn registry_with(gate: &Arc<Gate>) -> Arc<ModuleRegistry> {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(Arc::new(CandidateProofModule::gated(gate.clone())))
            .unwrap();
        Arc::new(registry)
    }

    fn start(&self) -> Owner {
        Owner::start(
            &self.root.join("catalog.sqlite"),
            Self::registry_with(&self.gate),
            HostConfig::unconfigured(),
        )
    }

    fn candidates(&self) -> PathBuf {
        self.root
            .join("catalog.artifacts")
            .join("tmp")
            .join("candidates")
    }

    /// The job's candidate files, sorted.
    fn files(&self, job: &Value) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.candidates().join(job.as_str().unwrap()))
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Queue the producer for this photograph with `parameters`; its `{job_id, status}`.
    fn generate(&self, owner: &Owner, parameters: Value) -> Value {
        let mut params = parameters;
        params["asset_id"] = json!(self.asset);
        owner.ok(&task(), params)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.gate.open();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn decode64(text: &str) -> Vec<u8> {
    let value = |byte: u8| match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        other => panic!("not base64: {other}"),
    };
    let mut bytes = Vec::new();
    for group in text.as_bytes().chunks(4) {
        let digits: Vec<u8> = group
            .iter()
            .take_while(|byte| **byte != b'=')
            .map(|byte| value(*byte))
            .collect();
        let word = digits
            .iter()
            .fold(0_u32, |word, digit| word << 6 | u32::from(*digit))
            << (6 * (4 - digits.len()));
        bytes.extend_from_slice(&word.to_be_bytes()[1..digits.len()]);
    }
    bytes
}

/// A whole part, read in chunks of the default length as a client reads it.
fn read_all(
    owner: &Owner,
    client: crate::ClientId,
    job: &Value,
    candidate: u8,
    part: &str,
) -> (Vec<u8>, usize) {
    let mut bytes = Vec::new();
    let mut chunks = 0;
    loop {
        let chunk = owner.ok_as(
            client,
            READ_CANDIDATE,
            json!({"job_id": job, "candidate": candidate, "part": part, "offset": bytes.len()}),
        );
        let data = decode64(chunk["data"].as_str().unwrap());
        assert_eq!(chunk["length"], json!(data.len()));
        assert!(data.len() as u64 <= CHUNK);
        assert_eq!(chunk["offset"], json!(bytes.len()));
        assert_eq!(chunk["part"], json!(part));
        assert_eq!(
            chunk["format"],
            json!(if part == "patch" {
                "rgba16f-le"
            } else {
                "rgb8-srgb"
            })
        );
        bytes.extend_from_slice(&data);
        chunks += 1;
        if bytes.len() as u64 == chunk["total"].as_u64().unwrap() {
            return (bytes, chunks);
        }
        assert!(!data.is_empty());
    }
}

/// The producer's patch of `candidate`, stored as RGBA half floats with the coverage as A,
/// converted here with the half crate.
fn expected_patch(seed: u64, candidate: u64, width: u64, height: u64) -> Vec<u8> {
    (0..height)
        .flat_map(|row| (0..width).map(move |column| (column, row)))
        .flat_map(|(column, row)| {
            let rgb = (0..3).map(move |channel| {
                proof_linear(proof_code(seed, candidate, column, row, channel))
            });
            rgb.chain(std::iter::once(proof_coverage(candidate, column, row)))
        })
        .flat_map(|value| half::f16::from_f32(value).to_le_bytes())
        .collect()
}

fn expected_preview(seed: u64, candidate: u64, width: u32, height: u32) -> Vec<u8> {
    let (preview_width, preview_height) = proof_preview(width, height);
    (0..u64::from(preview_height))
        .flat_map(|row| (0..u64::from(preview_width)).map(move |column| (column, row)))
        .flat_map(|(column, row)| {
            let (x, y) = (
                column * u64::from(width) / u64::from(preview_width),
                row * u64::from(height) / u64::from(preview_height),
            );
            (0..3).map(move |channel| proof_code(seed, candidate, x, y, channel))
        })
        .collect()
}

#[test]
fn a_task_publishes_candidates_that_any_client_lists_reads_and_releases() {
    let fixture = Fixture::new("candidates-journey");
    let owner = fixture.start();
    let other = owner.handle.register();
    let started = fixture.generate(
        &owner,
        json!({"count": 2, "width": 400, "height": 300, "x": 5, "y": 6, "seed": 9}),
    );
    let job = started["job_id"].clone();
    let done = owner.finished(&job);
    assert_eq!(done["status"], json!("ready"), "{done}");
    assert_eq!(
        done["result"],
        json!({"result": {"candidates": [0, 1]}, "artifacts": []})
    );
    let bytes = 400 * 300 * 8 + 64 * 48 * 3;
    // What the candidates were generated against: the input a new top correction of the photograph
    // has, a JPEG's content stage with no development before it and no correction yet.
    let input = &done["candidates"][0]["input"];
    assert_eq!(input["dimensions"], json!([480, 320]), "{input}");
    for component in ["source", "upstream", "earlier"] {
        assert_eq!(input[component].as_str().map(str::len), Some(64), "{component}");
    }
    let row = |candidate: u64| {
        json!({
            "candidate": candidate, "state": "live", "width": 400, "height": 300, "bytes": bytes,
            "rect": [5, 6, 400, 300], "preview": {"width": 64, "height": 48},
            "asset_id": fixture.asset, "revision": fixture.revision, "label": PROOF_PATCH_LABEL,
            "provenance": {"module": CANDIDATE_PROOF_MODULE, "task": CANDIDATE_PROOF_TASK, "seed": 9 + 11 * candidate},
            "input": input,
        })
    };
    assert_eq!(done["candidates"], json!([row(0), row(1)]));
    assert_eq!(
        fixture.files(&job),
        ["0.patch", "0.preview", "1.patch", "1.preview"]
    );
    // Pixels are read in chunks of at most 768 KiB, by any client, and are exactly the producer's
    // patch in the stored form; nothing of them is in the job's JSON.
    assert!(done.to_string().len() < 2048);
    let (patch, chunks) = read_all(&owner, other, &job, 1, "patch");
    assert_eq!(chunks, 2, "960000 bytes are two chunks");
    assert_eq!(patch, expected_patch(9, 1, 400, 300));
    let (preview, _) = read_all(&owner, owner.edit, &job, 0, "preview");
    assert_eq!(preview, expected_preview(9, 0, 400, 300));
    let slice = owner.ok(
        READ_CANDIDATE,
        json!({"job_id": job, "candidate": 0, "part": "patch", "offset": 8, "length": 16}),
    );
    assert_eq!(
        decode64(slice["data"].as_str().unwrap()),
        expected_patch(9, 0, 400, 300)[8..24]
    );
    assert_eq!(
        (
            slice["width"].clone(),
            slice["height"].clone(),
            slice["total"].clone()
        ),
        (json!(400), json!(300), json!(960000))
    );
    for (params, code) in [
        (
            json!({"job_id": job, "candidate": 0, "part": "patch", "offset": 960001}),
            "validation",
        ),
        (
            json!({"job_id": job, "candidate": 0, "part": "patch", "length": CHUNK + 1}),
            "validation",
        ),
        (
            json!({"job_id": job, "candidate": 2, "part": "patch"}),
            "validation",
        ),
        (
            json!({"job_id": job, "candidate": 4, "part": "patch"}),
            "validation",
        ),
        (
            json!({"job_id": job, "candidate": 0, "part": "mask"}),
            "validation",
        ),
        (
            json!({"job_id": crate::JobId::new(), "candidate": 0, "part": "patch"}),
            "validation",
        ),
    ] {
        assert_eq!(
            owner.fail(READ_CANDIDATE, params.clone()).code,
            code,
            "{params}"
        );
    }
    let collect = owner.ok("artifact.collect", json!({}));
    let refused = owner.fail(
        READ_CANDIDATE,
        json!({"job_id": collect["job_id"], "candidate": 0, "part": "patch"}),
    );
    assert!(
        refused.message.contains("is not a task"),
        "{}",
        refused.message
    );

    // Another client releases one; every client reads the release and its reason, a retry is
    // answered from the request table and a second release changes nothing.
    let release = json!({"job_id": job, "candidates": [1], "mutation": {"request_id": "release-1", "actor": "agent"}});
    let released = owner.ok_as(other, RELEASE, release.clone());
    assert_eq!(released["outcome"], json!("applied"));
    assert_eq!(released["released"], json!([1]));
    assert_eq!(released["deduplicated"], json!(false));
    let mut gone = row(1);
    gone["state"] = json!("released");
    gone["released"] = json!("released");
    assert_eq!(released["candidates"], json!([row(0), gone]));
    assert_eq!(owner.job(&job)["candidates"], json!([row(0), gone]));
    assert_eq!(
        owner.ok_as(other, RELEASE, release)["deduplicated"],
        json!(true)
    );
    let again = owner.ok(RELEASE, json!({"job_id": job, "candidates": [1]}));
    assert_eq!(
        (again["outcome"].clone(), again["released"].clone()),
        (json!("no-op"), json!([]))
    );
    assert_eq!(fixture.files(&job), ["0.patch", "0.preview"]);
    let refused = owner.fail(
        READ_CANDIDATE,
        json!({"job_id": job, "candidate": 1, "part": "preview"}),
    );
    assert_eq!(refused.code, "conflict");
    assert_eq!(
        refused.data,
        Some(json!({"state": "released", "released": "released"}))
    );
    assert_eq!(
        owner
            .fail(RELEASE, json!({"job_id": job, "candidates": [3]}))
            .code,
        "validation",
        "a number the job never published"
    );
    assert_eq!(
        owner
            .methods()
            .iter()
            .filter(|method| *method == RELEASE)
            .count(),
        1,
        "only the release that released something is announced"
    );

    // A client leaving releases nothing; a cancel of the finished job releases what it holds and
    // leaves its outcome as it was.
    owner.handle.disconnect(other);
    assert_eq!(owner.job(&job)["candidates"][0]["state"], json!("live"));
    let cancelled = owner.ok(JOB_CANCEL, json!({"job_id": job}));
    assert_eq!(cancelled["status"], json!("ready"));
    assert_eq!(cancelled["candidates"][0]["released"], json!("cancelled"));
    assert!(fixture.files(&job).is_empty());

    // Both methods are discoverable with their schemas.
    let schema = owner.ok("schema.list", json!({}));
    let read = &schema["methods"][READ_CANDIDATE];
    assert_eq!(read["mutates"], json!(false));
    assert_eq!(read["required"], json!(["job_id", "candidate", "part"]));
    assert!(read["optional"].get("offset").is_some() && read["optional"].get("length").is_some());
    let release = &schema["methods"][RELEASE];
    assert_eq!(release["mutation"], json!("request"));
    assert_eq!(release["required"], json!(["mutation", "job_id"]));
    assert!(release["optional"].get("candidates").is_some());
    assert!(
        schema["methods"][JOB_READ]["notes"]
            .as_str()
            .unwrap()
            .contains("candidates")
    );
    // Closing the catalog removes every candidate's files.
    let left = owner.ok(&task(), json!({"asset_id": fixture.asset}));
    assert_eq!(owner.finished(&left["job_id"])["status"], json!("ready"));
    assert_eq!(fixture.files(&left["job_id"]).len(), 2);
    owner.stop();
    assert_eq!(fs::read_dir(fixture.candidates()).unwrap().count(), 0);
}

#[test]
fn a_producer_meets_the_bounds_before_it_allocates() {
    let fixture = Fixture::new("candidates-bounds");
    let owner = fixture.start();
    // A fifth candidate is refused; the four before it stay live and readable.
    let five = fixture.generate(&owner, json!({"count": 5, "width": 8, "height": 8}));
    let failed = owner.finished(&five["job_id"]);
    assert_eq!(failed["status"], json!("failed"));
    assert_eq!(failed["error"]["code"], json!("resource-limit"), "{failed}");
    let states: Vec<&Value> = failed["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| &row["state"])
        .collect();
    assert_eq!(states, [&json!("live"); 4]);
    let (patch, _) = read_all(&owner, owner.edit, &five["job_id"], 3, "patch");
    assert_eq!(patch, expected_patch(0, 3, 8, 8));
    // A candidate past 64 MiB is refused at its reservation: nothing is made or written.
    let large = fixture.generate(&owner, json!({"width": 4096, "height": 4096}));
    let refused = owner.finished(&large["job_id"]);
    assert_eq!(
        refused["error"]["code"],
        json!("resource-limit"),
        "{refused}"
    );
    assert_eq!(refused["candidates"], json!([]));
    assert!(fixture.files(&large["job_id"]).is_empty());
    // A rectangle outside the photograph is refused by name.
    let outside = fixture.generate(&owner, json!({"x": 65535}));
    let refused = owner.finished(&outside["job_id"]);
    assert_eq!(refused["error"]["code"], json!("validation"));
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("leaves the"),
        "{refused}"
    );
    // A task that takes no asset publishes no candidates: every non-task job lists none.
    let collect = owner.ok("artifact.collect", json!({}));
    assert!(owner.job(&collect["job_id"]).get("candidates").is_none());
}

#[test]
fn cancelling_a_running_task_releases_its_candidates_and_stops_publication() {
    let fixture = Fixture::new("candidates-cancel");
    let owner = fixture.start();
    fixture.gate.shut();
    let started = fixture.generate(&owner, json!({"count": 3}));
    let job = started["job_id"].clone();
    fixture.gate.wait_reached(1, "the first candidate");
    let running = owner.job(&job);
    assert_eq!(running["status"], json!("running"));
    assert_eq!(running["candidates"][0]["state"], json!("live"));
    // A candidate change answers a job.wait that holds the running job's last token.
    let observed = owner.ok("job.wait", json!({"job_id": job}));
    let change = observed["change"].clone();
    owner.ok(RELEASE, json!({"job_id": job, "candidates": [0]}));
    let woken = owner.ok(
        "job.wait",
        json!({"job_id": job, "after": change, "timeout_ms": 0}),
    );
    assert_ne!(woken["change"], change);
    assert_eq!(woken["job"]["candidates"][0]["released"], json!("released"));
    // The cancel releases at once; the producer is let go and publishes nothing more.
    let cancelled = owner.ok(JOB_CANCEL, json!({"job_id": job}));
    assert_eq!(cancelled["candidates"].as_array().unwrap().len(), 1);
    fixture.gate.open();
    let done = owner.finished(&job);
    assert_eq!(done["status"], json!("cancelled"));
    assert_eq!(done["candidates"].as_array().unwrap().len(), 1, "{done}");
    assert!(fixture.files(&job).is_empty());
}

#[test]
fn opening_the_catalog_sweeps_what_a_crashed_process_left() {
    let fixture = Fixture::new("candidates-crash");
    let owner = fixture.start();
    let held = fixture.generate(&owner, json!({}));
    assert_eq!(owner.finished(&held["job_id"])["status"], json!("ready"));
    // What a process that ended without closing would leave: another job's directory.
    let left = fixture.candidates().join(crate::JobId::new().as_str());
    fs::create_dir_all(&left).unwrap();
    fs::write(left.join("0.patch"), b"left by a crash").unwrap();
    // A collection keeps what this process holds and removes the rest.
    let collect = owner.ok("artifact.collect", json!({}));
    let collected = wait_for("the collection", || {
        Some(owner.job(&collect["job_id"])).filter(|job| job["status"] == json!("ready"))
    });
    assert_eq!(collected["result"]["candidates"], json!(1));
    assert!(!left.exists());
    assert_eq!(fixture.files(&held["job_id"]).len(), 2);
    owner.stop();
    // And the next open sweeps it on its own.
    fs::create_dir_all(&left).unwrap();
    fs::write(left.join("0.preview"), b"left by a crash").unwrap();
    let owner = fixture.start();
    wait_for("the open's sweep", || (!left.exists()).then_some(()));
    owner.stop();
}
