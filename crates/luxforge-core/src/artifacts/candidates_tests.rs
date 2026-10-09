//! The candidate store on its own and the accept seam through the editor service: admission before
//! allocation and every bound, the stored form against an independent conversion, publication
//! refusals, releases and their reasons, pins under releases, budgets and cancels, chunk reads,
//! sweeping what a crash left, closing, and an accept that publishes a pinned candidate as an
//! artifact for an ordinary commit by a test-only consumer, including a commit that is refused.
use super::{
    ArtifactId, LiveArtifacts, PreparedArtifact,
    candidates::{
        CandidateBasis, CandidateLimits, CandidatePart, CandidatePatch, CandidateShape,
        CandidateSink, CandidateState, Candidates, PATCH_COLOUR, PATCH_KIND, PatchColour,
        ReleaseReason, base64,
    },
    store::ArtifactWriter,
};
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, AssetId, CapabilityModule, EditorService,
    EffectDescriptor, EffectStage, Error, ErrorKind, JobId, LayerReport, ModuleDescriptor,
    ModuleRegistry, ParameterDescriptor, Processing, StageContext, ToolModule,
    capabilities::{context::ModuleContext, secrets::MemorySecretStore},
    editor::mutation,
    jobs::JobControl,
    modules::{ColorOperation, NewLayer},
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "luxforge-candidates-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).unwrap();
    directory.canonicalize().unwrap()
}

/// An 8 × 8 patch with a 4 × 4 preview holds 512 + 48 bytes.
const SHAPE: CandidateShape = CandidateShape {
    width: 8,
    height: 8,
    preview_width: 4,
    preview_height: 4,
};
const SHAPE_BYTES: u64 = 8 * 8 * 8 + 4 * 4 * 3;

/// A store over its own root, with three 8 × 8 candidates' room across jobs.
fn store(root: &Path) -> Arc<Candidates> {
    Candidates::with_limits(
        ArtifactWriter::new(
            root.join("catalog.artifacts"),
            "catalog-a".into(),
            LiveArtifacts::default(),
        ),
        CandidateLimits {
            per_job: 4,
            candidate_bytes: 4096,
            held_bytes: 3 * SHAPE_BYTES,
        },
    )
}

fn basis() -> CandidateBasis {
    CandidateBasis {
        asset_id: AssetId::new(),
        revision: 7,
        width: 100,
        height: 50,
    }
}

fn sink(store: &Arc<Candidates>) -> CandidateSink {
    CandidateSink {
        store: store.clone(),
        job_id: JobId::new(),
        module_id: "test.producer".into(),
        task_id: "make-patches".into(),
        basis: basis(),
        control: JobControl::new(),
    }
}

/// The deterministic content of one 8 × 8 candidate: linear values from -0.25 to past 1, and the
/// coverage stepping through 0 to 1.
struct Content {
    linear: Vec<f32>,
    coverage: Vec<f32>,
    preview: Vec<u8>,
}

fn content(shape: CandidateShape, tag: u8) -> Content {
    let pixels = (shape.width * shape.height) as usize;
    Content {
        linear: (0..pixels * 3)
            .map(|index| (index as f32 * 0.37 + f32::from(tag)) % 1.6 - 0.25)
            .collect(),
        coverage: (0..pixels).map(|index| (index % 5) as f32 / 4.0).collect(),
        preview: (0..(shape.preview_width * shape.preview_height * 3) as usize)
            .map(|index| (index as u8).wrapping_add(tag))
            .collect(),
    }
}

fn patch<'a>(shape: CandidateShape, at: [u32; 2], content: &'a Content) -> CandidatePatch<'a> {
    CandidatePatch {
        rect: [at[0], at[1], shape.width, shape.height],
        colour: PatchColour::LinearSrgb(&content.linear),
        coverage: &content.coverage,
        preview: &content.preview,
        preview_width: shape.preview_width,
        preview_height: shape.preview_height,
        label: "Patch",
        provenance: Map::from_iter([("seed".to_owned(), json!(3))]),
    }
}

/// Reserve and publish one candidate of `tag`'s content for the sink's job.
fn publish(sink: &CandidateSink, tag: u8) -> Result<u8, Error> {
    let content = content(SHAPE, tag);
    sink.store
        .reserve(sink, SHAPE)?
        .publish(patch(SHAPE, [10, 20], &content))
}

/// The stored form of linear RGB and coverage, built here with the half crate.
fn stored(linear: &[f32], coverage: &[f32]) -> Vec<u8> {
    coverage
        .iter()
        .enumerate()
        .flat_map(|(pixel, coverage)| {
            [
                linear[pixel * 3],
                linear[pixel * 3 + 1],
                linear[pixel * 3 + 2],
                *coverage,
            ]
        })
        .flat_map(|value| half::f16::from_f32(value).to_le_bytes())
        .collect()
}

fn files(store: &Candidates, job: &JobId) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(store.directory().join(job.as_str()))
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn state(
    store: &Candidates,
    job: &JobId,
    candidate: u8,
) -> (CandidateState, Option<ReleaseReason>) {
    let row = store
        .read_job(job)
        .into_iter()
        .find(|row| row.candidate == candidate)
        .unwrap();
    (row.state, row.released)
}

#[test]
fn a_published_candidate_is_stored_in_its_form_listed_and_read_in_chunks() {
    let root = directory("publish");
    let store = store(&root);
    let sink = sink(&store);
    let content = content(SHAPE, 1);
    let change = Arc::clone(&sink.control);
    let number = store
        .reserve(&sink, SHAPE)
        .unwrap()
        .publish(patch(SHAPE, [10, 20], &content))
        .unwrap();
    assert_eq!(number, 0);
    let rows = store.read_job(&sink.job_id);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        serde_json::to_value(row).unwrap(),
        json!({
            "candidate": 0, "state": "live", "width": 8, "height": 8, "bytes": SHAPE_BYTES,
            "rect": [10, 20, 8, 8], "preview": {"width": 4, "height": 4},
            "asset_id": sink.basis.asset_id, "revision": 7, "label": "Patch",
            "provenance": {"module": "test.producer", "task": "make-patches", "seed": 3},
        })
    );
    assert_eq!(store.held_bytes(), SHAPE_BYTES);
    assert_eq!(files(&store, &sink.job_id), ["0.patch", "0.preview"]);
    // The patch is RGBA half floats with the coverage as A, exactly as the half crate converts.
    let expected = stored(&content.linear, &content.coverage);
    let reader = store.reader(&sink.job_id, 0).unwrap();
    assert_eq!(reader.total(CandidatePart::Patch), expected.len() as u64);
    assert_eq!(reader.read(CandidatePart::Patch, 0, 512).unwrap(), expected);
    assert_eq!(
        reader.read(CandidatePart::Patch, 100, 28).unwrap(),
        expected[100..128]
    );
    assert_eq!(
        reader.read(CandidatePart::Preview, 0, 48).unwrap(),
        content.preview
    );
    assert_eq!(
        reader.read(CandidatePart::Patch, 500, 13).unwrap_err().kind,
        ErrorKind::Validation,
        "past the part's end"
    );
    drop(reader);
    let _ = change;
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_srgb_patch_is_decoded_to_linear_light() {
    let root = directory("srgb");
    let store = store(&root);
    let sink = sink(&store);
    let codes: Vec<u8> = (0..8 * 8 * 3)
        .map(|index| (index * 5 % 256) as u8)
        .collect();
    let coverage = vec![1.0; 64];
    let preview = vec![0; 48];
    store
        .reserve(&sink, SHAPE)
        .unwrap()
        .publish(CandidatePatch {
            colour: PatchColour::Srgb8(&codes),
            coverage: &coverage,
            preview: &preview,
            ..patch(SHAPE, [0, 0], &content(SHAPE, 0))
        })
        .unwrap();
    // The sRGB transfer function, written out here.
    let linear: Vec<f32> = codes
        .iter()
        .map(|code| {
            let encoded = f64::from(*code) / 255.0;
            (if encoded <= 0.04045 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            }) as f32
        })
        .collect();
    let reader = store.reader(&sink.job_id, 0).unwrap();
    assert_eq!(
        reader.read(CandidatePart::Patch, 0, 512).unwrap(),
        stored(&linear, &coverage)
    );
    drop(reader);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn publication_refuses_what_it_cannot_store_and_returns_the_reservation() {
    let root = directory("refusals");
    let store = store(&root);
    let sink = sink(&store);
    let good = content(SHAPE, 0);
    let refuse = |patch: CandidatePatch<'_>, fragment: &str| {
        let error = store
            .reserve(&sink, SHAPE)
            .unwrap()
            .publish(patch)
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation, "{error}");
        assert!(error.detail.contains(fragment), "{fragment}: {error}");
        assert_eq!(store.held_bytes(), 0, "a refused publish returns its bytes");
        assert!(files(&store, &sink.job_id).is_empty(), "and leaves no file");
    };
    refuse(patch(SHAPE, [93, 0], &good), "leaves the 100×50 photograph");
    refuse(patch(SHAPE, [0, 43], &good), "leaves the 100×50 photograph");
    let mut bad = content(SHAPE, 0);
    bad.linear[17] = f32::NAN;
    refuse(patch(SHAPE, [0, 0], &bad), "not a finite half float");
    let mut bad = content(SHAPE, 0);
    bad.linear[2] = 70000.0;
    refuse(patch(SHAPE, [0, 0], &bad), "not a finite half float");
    let mut bad = content(SHAPE, 0);
    bad.coverage[5] = 1.5;
    refuse(patch(SHAPE, [0, 0], &bad), "outside 0 to 1");
    let mut bad = content(SHAPE, 0);
    bad.coverage.pop();
    refuse(patch(SHAPE, [0, 0], &bad), "coverages");
    let mut bad = content(SHAPE, 0);
    bad.preview.push(0);
    refuse(patch(SHAPE, [0, 0], &bad), "preview holds");
    for label in ["", "   ", "a label that is longer than thirty-two"] {
        refuse(
            CandidatePatch {
                label,
                ..patch(SHAPE, [0, 0], &good)
            },
            "label",
        );
    }
    for (key, value, fragment) in [
        ("module", json!("other"), "the host's"),
        ("seed", json!([1]), "a string or a number"),
        ("bad key", json!(1), "printable"),
        ("notes", json!("x".repeat(2100)), "at most 2048"),
    ] {
        refuse(
            CandidatePatch {
                provenance: Map::from_iter([(key.to_owned(), value)]),
                ..patch(SHAPE, [0, 0], &good)
            },
            fragment,
        );
    }
    refuse(
        patch(CandidateShape { width: 4, ..SHAPE }, [0, 0], &good),
        "was reserved at",
    );
    // Shapes are refused at the reservation, before anything is made.
    for (shape, kind) in [
        (CandidateShape { width: 0, ..SHAPE }, ErrorKind::Validation),
        (
            CandidateShape {
                preview_width: 1025,
                ..SHAPE
            },
            ErrorKind::Validation,
        ),
        (
            CandidateShape {
                width: 24,
                height: 24,
                ..SHAPE
            },
            ErrorKind::ResourceLimit,
        ),
    ] {
        assert_eq!(
            store.reserve(&sink, shape).unwrap_err().kind,
            kind,
            "{shape:?}"
        );
    }
    assert_eq!(store.held_bytes(), 0);
    // Every refusal returned its number: the next candidate is still the first.
    assert_eq!(publish(&sink, 0).unwrap(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_job_publishes_four_candidates_and_the_oldest_finished_job_makes_room() {
    let root = directory("budgets");
    let store = Candidates::with_limits(
        ArtifactWriter::new(
            root.join("catalog.artifacts"),
            "catalog-a".into(),
            LiveArtifacts::default(),
        ),
        CandidateLimits {
            per_job: 4,
            candidate_bytes: 4096,
            held_bytes: 6 * SHAPE_BYTES,
        },
    );
    // Four per job, never reused even after a release.
    let first = sink(&store);
    for tag in 0..4 {
        assert_eq!(publish(&first, tag).unwrap(), tag);
    }
    store.release(&first.job_id, Some(&[1])).unwrap();
    let error = publish(&first, 9).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert!(error.detail.contains("at most 4 candidates"), "{error}");
    store.finished(&first.job_id, false);
    let second = sink(&store);
    publish(&second, 0).unwrap();
    store.finished(&second.job_id, false);
    // Three live of the first job and one of the second hold four of six candidates' room.
    assert_eq!(store.held_bytes(), 4 * SHAPE_BYTES);
    let third = sink(&store);
    publish(&third, 0).unwrap();
    publish(&third, 1).unwrap();
    // Past the room, the oldest finished job's live candidates go first, in number order, and only
    // as many as the new one needs; a candidate an accept holds is never taken.
    let pinned = store.pin(&first.job_id, 0).unwrap();
    publish(&third, 2).unwrap();
    assert_eq!(
        state(&store, &first.job_id, 0),
        (CandidateState::Live, None)
    );
    assert_eq!(
        state(&store, &first.job_id, 1),
        (CandidateState::Released, Some(ReleaseReason::Released))
    );
    assert_eq!(
        state(&store, &first.job_id, 2),
        (CandidateState::Released, Some(ReleaseReason::Budget))
    );
    assert_eq!(
        state(&store, &first.job_id, 3),
        (CandidateState::Live, None)
    );
    assert_eq!(
        files(&store, &first.job_id),
        ["0.patch", "0.preview", "3.patch", "3.preview"]
    );
    publish(&third, 3).unwrap();
    assert_eq!(
        state(&store, &first.job_id, 3),
        (CandidateState::Released, Some(ReleaseReason::Budget))
    );
    assert_eq!(store.held_bytes(), 6 * SHAPE_BYTES);
    // Nothing else can go: a running job's candidates and the pinned one stay, and the request is
    // refused without releasing anything.
    let fourth = sink(&store);
    store.release(&second.job_id, None).unwrap();
    publish(&fourth, 0).unwrap();
    let refused = publish(&fourth, 1).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::ResourceLimit, "{refused}");
    assert_eq!(
        state(&store, &first.job_id, 0),
        (CandidateState::Live, None)
    );
    assert_eq!(store.held_bytes(), 6 * SHAPE_BYTES);
    drop(pinned);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn releases_cancels_and_pins_settle_without_releasing_a_candidate_under_an_accept() {
    let root = directory("pins");
    let store = store(&root);
    let job = sink(&store);
    for tag in 0..3 {
        publish(&job, tag).unwrap();
    }
    // An explicit release of a candidate an accept holds is refused whole.
    let accepting = store.pin(&job.job_id, 1).unwrap();
    let conflict = store.release(&job.job_id, Some(&[0, 1])).unwrap_err();
    assert_eq!(conflict.kind, ErrorKind::Conflict, "{conflict}");
    assert_eq!(state(&store, &job.job_id, 0), (CandidateState::Live, None));
    assert_eq!(
        store.release(&job.job_id, Some(&[3])).unwrap_err().kind,
        ErrorKind::Validation,
        "a number the job never published"
    );
    // A read in flight does not stop a release: the files stay until the read is done.
    let reading = store.reader(&job.job_id, 0).unwrap();
    assert_eq!(store.release(&job.job_id, Some(&[0, 0])).unwrap(), [0]);
    assert_eq!(
        store.release(&job.job_id, Some(&[0])).unwrap(),
        Vec::<u8>::new(),
        "converges"
    );
    assert_eq!(
        state(&store, &job.job_id, 0),
        (CandidateState::Released, Some(ReleaseReason::Released))
    );
    assert_eq!(
        reading.read(CandidatePart::Preview, 0, 48).unwrap().len(),
        48
    );
    assert!(files(&store, &job.job_id).contains(&"0.patch".to_owned()));
    drop(reading);
    assert!(!files(&store, &job.job_id).contains(&"0.patch".to_owned()));
    let refused = store.reader(&job.job_id, 0).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Conflict);
    assert_eq!(
        refused.data.as_deref(),
        Some(&json!({"state": "released", "released": "released"}))
    );
    // A cancel releases what nothing accepts now, and the accepted one once its accept settles
    // without committing; the job publishes nothing more.
    store.cancel(&job.job_id);
    assert_eq!(
        state(&store, &job.job_id, 2),
        (CandidateState::Released, Some(ReleaseReason::Cancelled))
    );
    assert_eq!(state(&store, &job.job_id, 1), (CandidateState::Live, None));
    drop(accepting);
    assert_eq!(
        state(&store, &job.job_id, 1),
        (CandidateState::Released, Some(ReleaseReason::Cancelled))
    );
    assert_eq!(store.held_bytes(), 0);
    assert!(files(&store, &job.job_id).is_empty());
    assert_eq!(publish(&job, 9).unwrap_err().kind, ErrorKind::Cancelled);
    // An accept that commits wins over a cancel that waited for it.
    let other = sink(&store);
    publish(&other, 0).unwrap();
    let pin = store.pin(&other.job_id, 0).unwrap();
    store.cancel(&other.job_id);
    pin.accepted();
    assert_eq!(
        state(&store, &other.job_id, 0),
        (CandidateState::Accepted, None)
    );
    assert_eq!(
        store.pin(&other.job_id, 0).unwrap_err().kind,
        ErrorKind::Conflict
    );
    assert!(files(&store, &other.job_id).is_empty());
    assert_eq!(store.held_bytes(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_cancel_during_publication_publishes_nothing() {
    let root = directory("racing");
    let store = store(&root);
    let job = sink(&store);
    let content = content(SHAPE, 0);
    let reservation = store.reserve(&job, SHAPE).unwrap();
    assert_eq!(
        store.held_bytes(),
        SHAPE_BYTES,
        "charged before anything is made"
    );
    job.control.cancel("the job was cancelled");
    store.cancel(&job.job_id);
    let error = reservation
        .publish(patch(SHAPE, [0, 0], &content))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(store.held_bytes(), 0);
    assert!(store.read_job(&job.job_id).is_empty());
    assert!(files(&store, &job.job_id).is_empty());
    // A reservation dropped unpublished returns its bytes too.
    let other = sink(&store);
    drop(store.reserve(&other, SHAPE).unwrap());
    assert_eq!(store.held_bytes(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn forgetting_a_job_sweeping_and_closing_remove_its_files() {
    let root = directory("lifetimes");
    let store = store(&root);
    let kept = sink(&store);
    let forgotten = sink(&store);
    publish(&kept, 0).unwrap();
    publish(&forgotten, 0).unwrap();
    let reading = store.reader(&forgotten.job_id, 0).unwrap();
    store.retain(|job_id| job_id == &kept.job_id);
    // Its read keeps the entry and files until it is done.
    assert_eq!(
        state(&store, &forgotten.job_id, 0).0,
        CandidateState::Released
    );
    assert_eq!(reading.read(CandidatePart::Patch, 0, 8).unwrap().len(), 8);
    drop(reading);
    assert!(store.read_job(&forgotten.job_id).is_empty());
    assert!(!store.directory().join(forgotten.job_id.as_str()).exists());
    // What an earlier process left is swept; what this one holds is kept.
    let stale = store.directory().join(JobId::new().as_str());
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("0.patch"), b"left").unwrap();
    assert_eq!(store.sweep(&AtomicBool::new(false)).unwrap(), 1);
    assert!(!stale.exists());
    assert_eq!(files(&store, &kept.job_id), ["0.patch", "0.preview"]);
    assert_eq!(
        store.sweep(&AtomicBool::new(true)).unwrap(),
        0,
        "nothing left to sweep, so a cancel finds nothing to stop"
    );
    // Closing the catalog removes every candidate's files.
    let directory = store.directory();
    drop(kept);
    drop(forgotten);
    drop(store);
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn base64_is_the_standard_alphabet_with_padding() {
    for (bytes, text) in [
        (b"".as_slice(), ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg=="),
        (b"\xff\xfe\x00", "//4A"),
    ] {
        assert_eq!(base64(bytes), text);
    }
}

// The accept seam, through the editor service and a test-only consumer.

const CONSUMER: &str = "test.consumer";
const CONSUMER_EFFECT: &str = "test.consumer.patch";
const APPLY_PATCH: &str = "apply-test-patch";

/// A colour-stage effect that binds one `repair-patch` artifact and checks it, rendering nothing:
/// what the repair layer's accept will commit, without its operation.
struct Consumer(ModuleDescriptor);

impl Consumer {
    fn shared() -> Arc<dyn ToolModule> {
        Arc::new(Self(ModuleDescriptor {
            id: CONSUMER.into(),
            title: "Patch consumer".into(),
            effects: vec![EffectDescriptor {
                artifacts: true,
                ..EffectDescriptor::new(CONSUMER_EFFECT, EffectStage::Color)
            }],
            actions: vec![ActionDescriptor {
                id: APPLY_PATCH.into(),
                title: "Apply patch".into(),
                notes: "test".into(),
                patch: false,
                preset: false,
                analysis: None,
                shortcut: None,
                parameters: vec![
                    ParameterDescriptor::artifact("artifact")
                        .required(true)
                        .notes("test"),
                ],
            }],
            ..ModuleDescriptor::default()
        }))
    }
}

impl ToolModule for Consumer {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.0
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: parameters.clone(),
        })
    }
    fn plan(&self, input: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let artifact =
            ArtifactId::parse(input.parameters["artifact"].as_str().unwrap_or_default())?;
        Ok(ActionPlan::Commit(
            NewLayer::new(CONSUMER_EFFECT, json!({"artifact": artifact}))
                .with_artifacts(vec![artifact]),
        ))
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe(&self, _: &str, _: u32, _: &Value) -> Result<LayerReport, Error> {
        Ok(LayerReport::new("Patch"))
    }
    fn compile(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
        Err(Error::validation("a patch is compiled with its bytes"))
    }
    fn capabilities(&self) -> Option<&dyn CapabilityModule> {
        Some(self)
    }
}

impl CapabilityModule for Consumer {
    fn compile_bound(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
        artifacts: &[Arc<PreparedArtifact>],
    ) -> Result<Processing, Error> {
        let [patch] = artifacts else {
            return Err(Error::validation("one patch"));
        };
        let (width, height) = (patch.width.unwrap_or(0), patch.height.unwrap_or(0));
        if patch.kind != PATCH_KIND
            || patch.bytes.len() as u64 != u64::from(width) * u64::from(height) * 8
        {
            return Err(Error::validation("not a repair patch"));
        }
        Ok(Processing::Color(ColorOperation::neutral()))
    }
}

fn consumer_registry() -> Arc<ModuleRegistry> {
    let mut registry = ModuleRegistry::builtin();
    registry.register(Consumer::shared()).unwrap();
    Arc::new(registry)
}

fn count(service: &EditorService, table: &str) -> i64 {
    service
        .connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// Publish one candidate of the photograph through a task's context, as the module lane does.
fn publish_through_context(service: &EditorService, asset: &AssetId, tag: u8) -> (JobId, Content) {
    let control = JobControl::new();
    let job_id = JobId::new();
    let context = ModuleContext::new(
        "test.producer",
        Arc::new(MemorySecretStore::new()),
        control.clone(),
    )
    .with_candidates(CandidateSink {
        store: service.candidates().clone(),
        job_id: job_id.clone(),
        module_id: "test.producer".into(),
        task_id: "make-patches".into(),
        basis: service.candidate_basis(asset).unwrap(),
        control,
    });
    let content = content(SHAPE, tag);
    assert_eq!(
        context
            .publish_candidate(patch(SHAPE, [2, 3], &content))
            .unwrap(),
        0
    );
    service.candidates().finished(&job_id, false);
    (job_id, content)
}

#[test]
fn an_accept_publishes_the_pinned_candidate_for_one_ordinary_commit() {
    let root = directory("accept");
    let catalog = root.join("catalog.sqlite");
    let mut service = EditorService::open_with(&catalog, consumer_registry()).unwrap();
    let photo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
    let opened = service.import(&photo).unwrap();
    let asset = opened.asset.id.clone();
    let (job, content) = publish_through_context(&service, &asset, 1);
    let store = service.candidates().clone();
    let pin = store.pin(&job, 0).unwrap();
    let candidate = pin.candidate().clone();
    assert_eq!(
        (candidate.asset_id.clone(), candidate.revision),
        (asset.clone(), opened.revision)
    );
    assert_eq!(candidate.rect, [2, 3, 8, 8]);
    assert_eq!(
        (pin.module_id(), pin.task_id(), pin.job_id()),
        ("test.producer", "make-patches", &job)
    );
    // The worker publishes the stored bytes through the artifact writer; the owner records the row,
    // commits one ordinary entry that references it, then settles the pin.
    let writer = service.artifact_writer().unwrap();
    let (record, prepared) = std::thread::scope(|scope| {
        scope
            .spawn(|| pin.publish(&writer, &AtomicBool::new(false)))
            .join()
            .unwrap()
    })
    .unwrap();
    let expected = stored(&content.linear, &content.coverage);
    assert_eq!(&*prepared.bytes, expected.as_slice());
    assert_eq!(record.sha256, format!("{:x}", Sha256::digest(&expected)));
    assert_eq!(
        (record.meta.kind.as_str(), record.meta.colour.as_deref()),
        (PATCH_KIND, Some(PATCH_COLOUR))
    );
    assert_eq!((record.meta.width, record.meta.height), (Some(8), Some(8)));
    assert_eq!(record.module_id, "test.producer");
    let artifact = record.id.clone();
    service.register_artifact(record, prepared, true).unwrap();
    let entries = count(&service, "entries");
    let committed = service
        .apply_action(
            &asset,
            mutation(opened.revision, "accept-1"),
            APPLY_PATCH,
            json!({"artifact": artifact}),
        )
        .unwrap();
    assert_eq!(count(&service, "entries"), entries + 1, "one entry");
    pin.accepted();
    assert_eq!(state(&store, &job, 0), (CandidateState::Accepted, None));
    assert!(files(&store, &job).is_empty());
    assert_eq!(store.held_bytes(), 0);
    let inspected = service.inspect_artifact(&artifact).unwrap();
    assert_eq!(inspected["references"], json!(1));
    // A second accept of it is refused: it is no longer live.
    assert_eq!(store.pin(&job, 0).unwrap_err().kind, ErrorKind::Conflict);

    // A late commit that is refused leaves the candidate live and only an unreferenced artifact,
    // which a collection removes once no process holds it live.
    let (late, _) = publish_through_context(&service, &asset, 2);
    let pin = store.pin(&late, 0).unwrap();
    let (record, prepared) = pin.publish(&writer, &AtomicBool::new(false)).unwrap();
    let orphan = record.id.clone();
    service.register_artifact(record, prepared, true).unwrap();
    let refused = service
        .apply_action(
            &asset,
            mutation(opened.revision, "accept-2"),
            APPLY_PATCH,
            json!({"artifact": orphan}),
        )
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Conflict, "{refused}");
    assert_eq!(count(&service, "entries"), entries + 1, "nothing partial");
    drop(pin);
    assert_eq!(state(&store, &late, 0), (CandidateState::Live, None));
    // A corrupt candidate is never published.
    let damaged = store.directory().join(late.as_str()).join("0.patch");
    let mut bytes = fs::read(&damaged).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&damaged, &bytes).unwrap();
    let error = store
        .pin(&late, 0)
        .unwrap()
        .publish(&writer, &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable, "{error}");
    assert_eq!(
        store
            .pin(&late, 0)
            .unwrap()
            .publish(&writer, &AtomicBool::new(true))
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    let current = committed.current_entry_id.clone();
    drop((service, store));
    let mut reopened = EditorService::open_with(&catalog, consumer_registry()).unwrap();
    assert!(
        !reopened
            .candidates()
            .directory()
            .join(late.as_str())
            .exists(),
        "closing the catalog removed the candidates"
    );
    let collection = reopened.plan_collection().unwrap();
    super::collect_files(&collection, &AtomicBool::new(false)).unwrap();
    assert!(!super::object_path(&root.join("catalog.artifacts"), &orphan).exists());
    assert!(super::object_path(&root.join("catalog.artifacts"), &artifact).exists());
    assert_eq!(reopened.state(&asset).unwrap().current_entry.id, current);
    fs::remove_dir_all(root).unwrap();
}
