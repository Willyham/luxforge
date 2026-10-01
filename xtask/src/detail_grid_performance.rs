//! Dispatch-to-answer measurements of the production Detail mask input-grid paths.
//!
//! A public `Latest` worker owns one `InputGridCache`, as the desktop workers do. Each pair first
//! clears that cache and fills coverage, then changes only the inspected mask's amount and fills
//! coverage again with the retained restoration input. This exercises the actual dense overlay
//! and sparse thumbnail APIs, without launching a GUI or creating a new production worker.
use crate::*;
use luxforge_core::{
    AssetId, ClientId, Evaluation, INPUT_GRID_MAX_CELLS, InputGridCache, MaskCoverage,
    MaskCoverageTarget, MaskId, ModuleRegistry, PreviewRequest, PreviewSource, latest::Latest,
    resources,
};
use luxforge_testkit::client::{self, Owner};
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const DENSE: (u32, u32) = (2880, 1800);
const SPARSE: (u32, u32) = (28, 19);
const ACTOR: &str = "detail-grid-performance";

struct Fixture {
    owner: Owner,
    client: ClientId,
    asset: Value,
    mask: MaskId,
}
impl Fixture {
    fn import(catalog: &Path, source: &Path) -> Result<Self> {
        let owner = Owner::start(catalog, ModuleRegistry::builtin(), ACTOR)?;
        let client = owner.client();
        let state = owner.import(client, source)?;
        let mut fixture = Self {
            owner,
            client,
            asset: state["asset"]["id"].clone(),
            mask: MaskId::new(),
        };
        fixture.mutate(
            "edit.set-detail",
            json!({"luminance":25.0,"colour":25.0,"sharpening":40.0}),
        )?;
        let created = fixture.mutate(
            "mask.create-luminance-range",
            json!({"low":10.0,"high":80.0,"low_feather":5.0,"high_feather":5.0}),
        )?;
        fixture.mask = serde_json::from_value(created["mask"].clone())?;
        fixture.mutate(
            "edit.set-basic",
            json!({"mask":fixture.mask,"exposure":0.5}),
        )?;
        Ok(fixture)
    }
    fn mutate(&self, method: &str, mut params: Value) -> Result<Value> {
        params["asset_id"] = self.asset.clone();
        params["mutation"] = client::mutation(
            self.owner.revision(self.client, &self.asset)?,
            &client::request_id(method),
            ACTOR,
        );
        Ok(self.owner.call(self.client, method, params)?)
    }
    fn evaluation(&self, amount: f64) -> Result<Evaluation> {
        self.mutate("mask.set-amount", json!({"mask":self.mask,"amount":amount}))?;
        let asset: AssetId = serde_json::from_value(self.asset.clone())?;
        Ok(self
            .owner
            .preview_job(PreviewRequest::new(self.client, asset))?
            .evaluation)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Overlay,
    Thumbnail,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Overlay => "dense_overlay",
            Self::Thumbnail => "sparse_thumbnail",
        }
    }
    fn cells(self) -> (u32, u32) {
        match self {
            Self::Overlay => DENSE,
            Self::Thumbnail => SPARSE,
        }
    }
}

struct GridJob {
    evaluation: Evaluation,
    mask: MaskId,
    kind: Kind,
    cells: (u32, u32),
    clear: bool,
}
struct Answer {
    coverage: MaskCoverage,
    cache_cells: usize,
    cache_bytes: usize,
}
type AnswerResult = std::result::Result<Answer, luxforge_core::Error>;

struct WorkerState {
    cache: InputGridCache,
    stopped: mpsc::SyncSender<()>,
}
impl Drop for WorkerState {
    fn drop(&mut self) {
        self.cache.clear();
        let _ = self.stopped.try_send(());
    }
}
struct Worker {
    queue: Option<Latest<GridJob, AnswerResult>>,
    wake: mpsc::Receiver<()>,
    stopped: mpsc::Receiver<()>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(queue) = self.queue.take() {
            // Error paths abandon active work too, and wait for its cache to be released.
            drop(queue);
            let _ = self.stopped.recv_timeout(Duration::from_secs(60));
        }
    }
}
impl Worker {
    fn new() -> Self {
        let (wake_tx, wake) = mpsc::sync_channel(1);
        let (stop_tx, stopped) = mpsc::sync_channel(1);
        let mut state = WorkerState {
            cache: InputGridCache::default(),
            stopped: stop_tx,
        };
        let queue = Latest::new(
            ACTOR,
            move |job: GridJob,
                  running: &luxforge_core::latest::Running<'_, GridJob, AnswerResult>| {
                if job.clear {
                    state.cache.clear();
                }
                let cancel = running.superseded();
                let coverage = match job.kind {
                    Kind::Overlay => job.evaluation.mask_overlay_coverage_with_cache(
                        &MaskCoverageTarget::Existing {
                            mask: job.mask,
                            component: None,
                        },
                        job.cells,
                        None,
                        None,
                        cancel,
                        &mut state.cache,
                    ),
                    Kind::Thumbnail => job.evaluation.mask_coverage_with_cache(
                        &job.mask,
                        job.cells,
                        None,
                        cancel,
                        &mut state.cache,
                    ),
                };
                Some(coverage.map(|coverage| Answer {
                    coverage,
                    cache_cells: state.cache.cells(),
                    cache_bytes: state.cache.bytes(),
                }))
            },
        );
        queue.set_waker(Arc::new(move || {
            let _ = wake_tx.try_send(());
        }));
        Self {
            queue: Some(queue),
            wake,
            stopped,
        }
    }

    fn dispatch(&mut self, job: GridJob) -> Result<(u64, f64, Answer)> {
        let queue = self.queue.as_mut().unwrap();
        let start = Instant::now();
        let requested = queue.request(job);
        ensure(
            requested.replaced.is_none(),
            "Sequential grid jobs replaced a pending job",
        )?;
        loop {
            if let Some((generation, result)) = queue.poll() {
                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                ensure(
                    generation == requested.generation,
                    "Grid answer had a stale generation",
                )?;
                return Ok((generation, elapsed_ms, result?));
            }
            if start.elapsed() >= Duration::from_secs(360) {
                queue.cancel();
                return Err("Grid worker did not answer within six minutes".into());
            }
            // The production primitive wakes on delivery; this adds no polling interval to the
            // measured duration. A bounded wait also detects a panicked worker that delivered none.
            match self.wake.recv_timeout(Duration::from_secs(60)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("Grid worker's delivery signal disconnected".into());
                }
            }
        }
    }
    fn close(mut self) -> Result {
        drop(self.queue.take());
        self.stopped.recv_timeout(Duration::from_secs(60))?;
        Ok(())
    }
}

fn observe(
    evaluation: &Evaluation,
    generation: u64,
    duration: f64,
    answer: &Answer,
    cells: (u32, u32),
) -> Result<Value> {
    let outcome = answer
        .coverage
        .outcome
        .as_ref()
        .ok_or("A grid job returned only its cached key")?;
    ensure(
        outcome.absent.is_none(),
        format!("Grid was refused: {:?}", outcome.absent),
    )?;
    let grid = outcome
        .grid
        .as_ref()
        .ok_or("A grid job returned no coverage")?;
    ensure(
        (grid.cells_w, grid.cells_h) == cells,
        "Grid answer changed the requested dimensions",
    )?;
    let count = cells.0 as usize * cells.1 as usize;
    ensure(
        answer.cache_cells == count,
        "Detail input grid was not retained at the requested cell count",
    )?;
    ensure(
        count as u64 <= INPUT_GRID_MAX_CELLS && answer.cache_bytes <= count * 7,
        "The retained JPEG input grid exceeded its public cell/byte bound",
    )?;
    let memory = resources::read(evaluation.context());
    ensure(
        memory.budgets.colour_scratch.in_use_bytes == 0 && memory.budgets.spatial.in_use_bytes == 0,
        "A completed grid job retained a working-set reservation",
    )?;
    Ok(json!({
        "generation":generation,"ms":duration,"identity":evaluation.identity()?,
        "source_identity":format!("{:?}",evaluation.source().identity()),
        "grid_key":answer.coverage.key,"coverage_sha256":format!("{:x}",Sha256::digest(&grid.coverage)),
        "coverage_sum":grid.coverage.iter().map(|value|u64::from(*value)).sum::<u64>(),
        "cells":[cells.0,cells.1],"retained_input_grid":{"cells":answer.cache_cells,"bytes":answer.cache_bytes},
        "resources_while_coverage_held":memory
    }))
}

fn measure(fixture: &Fixture, kind: Kind, samples: usize) -> Result<Value> {
    let mut worker = Worker::new();
    let mut build = Vec::with_capacity(samples);
    let mut reuse = Vec::with_capacity(samples);
    let mut observations = Vec::with_capacity(samples);
    for sample in 0..samples {
        let before = fixture.evaluation(50.0)?;
        let after = fixture.evaluation(60.0)?;
        let (PreviewSource::Jpeg(source), PreviewSource::Jpeg(other)) =
            (before.source(), after.source())
        else {
            return Err("Grid measurements require a JPEG source".into());
        };
        ensure(
            Arc::ptr_eq(&source.rgba, &other.rgba),
            "Mask-only editing replaced the prepared source allocation",
        )?;
        ensure(
            before.recipe().layers == after.recipe().layers
                && before.recipe().masks.len() == after.recipe().masks.len()
                && before.recipe().masks.len() == 1,
            "The reuse job changed the restoration prefix",
        )?;
        let mut restored_masks = after.recipe().masks.clone();
        restored_masks[0].amount = before.recipe().masks[0].amount;
        ensure(
            restored_masks == before.recipe().masks,
            "The reuse job changed more than mask amount",
        )?;
        let cells = kind.cells();
        let (first_gen, first_ms, first) = worker.dispatch(GridJob {
            evaluation: before.clone(),
            mask: fixture.mask.clone(),
            kind,
            cells,
            clear: true,
        })?;
        let first_observation = observe(&before, first_gen, first_ms, &first, cells)?;
        let (next_gen, next_ms, next) = worker.dispatch(GridJob {
            evaluation: after.clone(),
            mask: fixture.mask.clone(),
            kind,
            cells,
            clear: false,
        })?;
        let next_observation = observe(&after, next_gen, next_ms, &next, cells)?;
        ensure(
            next_observation["coverage_sum"].as_u64().unwrap()
                > first_observation["coverage_sum"].as_u64().unwrap(),
            "The mask-only measurement did not produce a meaningful coverage change",
        )?;
        ensure(
            first.coverage.key != next.coverage.key,
            "Mask-only editing reused stale output coverage",
        )?;
        ensure(
            first.cache_cells == next.cache_cells && first.cache_bytes == next.cache_bytes,
            "Mask-only editing changed the retained grid's bounds",
        )?;
        build.push(first_ms);
        reuse.push(next_ms);
        observations.push(
            json!({"sample":sample,"build":first_observation,"mask_only_reuse":next_observation}),
        );
    }
    worker.close()?;
    Ok(json!({
        "rows":[stats::row(&format!("detail.{}.first_input_grid_build_dispatch_to_answer",kind.name()),"ms",build),
            stats::row(&format!("detail.{}.mask_only_input_grid_reuse_dispatch_to_answer",kind.name()),"ms",reuse)],
        "observations":observations,"cells":[kind.cells().0,kind.cells().1],
        "scope":"One public Latest worker and one per-worker InputGridCache, using the production coverage API. First job clears the input cache; second changes only inspected mask amount 50 to 60. Both recompute output coverage with no output-cache shortcut. Owner mutations, planning, decode, resource reads, validation and hashing are excluded from dispatch-to-answer latency; queue wakeup, mask keying, grid work, allocation and prior input-grid release on a first job are included. Same-prefix cache reuse is the production path selected by these immutable jobs; private hit counters and per-job residency are unavailable."
    }))
}

pub fn run(root: &Path, source: &Path, out: &Path, samples: usize) -> Result {
    ensure(
        !cfg!(debug_assertions),
        "Detail timing requires a release xtask build",
    )?;
    ensure(samples > 0, "Detail samples must be positive")?;
    ensure(!out.exists(), "Detail grid performance output must be new")?;
    let source = source.canonicalize()?;
    let source_hash = hash(&source)?;
    let executable = std::env::current_exe()?;
    let diff = output(root, "git", &["diff", "--binary", "HEAD"])?;
    let mut report = json!({
        "format":1,"status":"running","run":{"started_unix_ns":SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos().to_string(),
            "pid":std::process::id(),"output":out},
        "build":{"executable":executable,"sha256":hash(&executable)?,"cargo_lock_sha256":hash(&root.join("Cargo.lock"))?,
            "git_head":output(root,"git",&["rev-parse","HEAD"])?.trim(),"git_status":output(root,"git",&["status","--porcelain"])?,
            "tracked_diff_sha256":format!("{:x}",Sha256::digest(diff.as_bytes())),"rustc":output(root,"rustc",&["-vV"])?,"release":true},
        "host":{"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,
            "system":output(root,"uname",&["-a"]).ok(),"start_load":launch::load(launch::load_average(root))},
        "source":{"path":source,"sha256":source_hash},"samples":samples,"cases":{},"rows":[],
        "scope":"Headless CPU queue/coverage measurements on prepared authentic-size JPEGs. No native UI/GPU latency claim. OS memory levels and process high-water marks are cumulative; render-context budgets and the cache's retained cells/bytes are public exact counters. GPU residency, backend staging, private source/prefix cache counters and per-case OS peaks are unavailable. No performance budget verdict is inferred."
    });
    fs::create_dir_all(out)?;
    write_json(&out.join("result.json"), &report)?;
    let outcome = (|| -> Result {
        let fixture = Fixture::import(&out.join("catalog.sqlite"), &source)?;
        let evaluation = fixture.evaluation(50.0)?;
        ensure(
            matches!(evaluation.source(), PreviewSource::Jpeg(_)),
            "Use a JPEG source for this utility",
        )?;
        let (width, height) = evaluation.source().dimensions();
        ensure(
            matches!(
                u64::from(width) * u64::from(height),
                24_000_000 | 60_000_000
            ),
            "Use the planned 24 MP or 60 MP JPEG workload",
        )?;
        report["source"]["dimensions"] = json!([width, height]);
        report["recipe"] = serde_json::to_value(evaluation.recipe())?;
        report["identity"] = serde_json::to_value(evaluation.identity()?)?;
        drop(evaluation);
        for kind in [Kind::Overlay, Kind::Thumbnail] {
            println!(
                "Detail {}: {samples} build/reuse pairs on {width}x{height}",
                kind.name()
            );
            let start_load = launch::load(launch::load_average(root));
            let mut result = measure(&fixture, kind, samples)?;
            result["start_load"] = start_load;
            result["end_load"] = launch::load(launch::load_average(root));
            report["rows"]
                .as_array_mut()
                .unwrap()
                .extend(stats::rows(&result).iter().cloned());
            report["cases"][kind.name()] = result;
            write_json(&out.join("result.json"), &report)?;
        }
        fixture.owner.close()?;
        ensure(
            hash(&source)? == source_hash,
            "The original changed during the Detail grid measurement",
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

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::{
        BASIC_EFFECT, Component, ComponentMode, DETAIL_EFFECT, EntryId, HistoryEntry, Layer, Mask,
        Recipe, RenderContext, Snapshot, SnapshotId, SourceImage,
    };

    #[test]
    fn grid_performance_worker_correlates_build_and_mask_only_reuse() {
        let asset = AssetId::new();
        let mut mask = Mask::new("Range");
        mask.amount = 50.0;
        mask.components.push(Component::new(
            "Range 1",
            ComponentMode::Add,
            "luminance-range",
            json!({"low":0.0,"high":100.0,"low_feather":0.0,"high_feather":0.0}),
        ));
        let mut basic = Layer::new(BASIC_EFFECT, json!({"exposure":0.5}));
        basic.mask = Some(mask.id.clone());
        let recipe = Recipe {
            layers: vec![Layer::new(DETAIL_EFFECT, json!({"sharpening":40.0})), basic],
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset.clone(),
            sequence: 0,
            action_id: "test".into(),
            label: "Test".into(),
            parameters: json!({}),
            actor: "test".into(),
            timestamp_ms: 0,
            request_id: None,
            base_revision: 0,
            result_revision: 0,
            snapshot: Snapshot {
                id: SnapshotId::new(),
                asset_id: asset,
                recipe: recipe.clone(),
            },
            undo_parent: None,
            restore_target: None,
        };
        let registry = Arc::new(ModuleRegistry::builtin());
        let context = RenderContext::new();
        let source = PreviewSource::Jpeg(SourceImage {
            width: 60,
            height: 40,
            rgba: vec![128; 60 * 40 * 4].into(),
            fingerprint: "sha256:detail-grid-worker-test".into(),
            orientation: 1,
            capture: Default::default(),
        });
        let before = Evaluation::new(
            registry.clone(),
            context.clone(),
            source.clone(),
            entry.clone(),
            recipe.clone(),
            None,
        );
        let mut updated = recipe;
        updated.masks[0].amount = 60.0;
        let after = Evaluation::new(registry, context, source, entry, updated, None);
        for kind in [Kind::Overlay, Kind::Thumbnail] {
            let mut worker = Worker::new();
            let cells = SPARSE;
            let (first_gen, first_ms, first) = worker
                .dispatch(GridJob {
                    evaluation: before.clone(),
                    mask: mask.id.clone(),
                    kind,
                    cells,
                    clear: true,
                })
                .unwrap();
            let first_observation = observe(&before, first_gen, first_ms, &first, cells).unwrap();
            let (next_gen, next_ms, next) = worker
                .dispatch(GridJob {
                    evaluation: after.clone(),
                    mask: mask.id.clone(),
                    kind,
                    cells,
                    clear: false,
                })
                .unwrap();
            let next_observation = observe(&after, next_gen, next_ms, &next, cells).unwrap();
            assert!(next_gen > first_gen);
            assert_ne!(first.coverage.key, next.coverage.key);
            assert!(
                next_observation["coverage_sum"].as_u64().unwrap()
                    > first_observation["coverage_sum"].as_u64().unwrap()
            );
            assert_ne!(first_observation["identity"], next_observation["identity"]);
            assert_eq!(first.cache_cells, next.cache_cells);
            assert_eq!(first.cache_bytes, next.cache_bytes);
            worker.close().unwrap();
        }
    }
}
