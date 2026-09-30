//! The preview bracket check's cost per run, which `cargo xtask catalog-measure` measures against
//! the design's provisional target ("Bracket detection from previews: under 1 ms a run, from the
//! grid tiers' fingerprints"). The check is internal to the core, so the harness runs this ignored
//! bench over a catalog it prepared: a folder whose files the index lane listed and whose grid
//! tiers (and so their fingerprints) the preview lane wrote. The folder is viewed through the
//! browse lane's own evaluation, organizing asks the preview lane's probe over the index about
//! each run its metadata cannot classify, and each such run is timed from the probe's call to its
//! answer: one indexed query for the run's fingerprints and the measure over them. Nothing here
//! computes a percentile; the harness reads the samples through the one `Distribution`.
use crate::{
    EditorService,
    browse::{self, Context, EventCache, Probe},
    catalog_types::{BracketProbe, MAX_VIEW_ITEMS, ViewItem, ViewQuery},
    previews,
};
use serde_json::{Value, json};
use std::{cell::RefCell, path::PathBuf, time::Instant};

/// The environment the harness names the prepared catalog, the folder, how many times to evaluate
/// its view and where to write the samples by.
const CATALOG: &str = "LUXFORGE_BRACKET_CATALOG";
const FOLDER: &str = "LUXFORGE_BRACKET_FOLDER";
const EVALUATIONS: &str = "LUXFORGE_BRACKET_EVALUATIONS";
const OUT: &str = "LUXFORGE_BRACKET_OUT";

/// The preview lane's probe over the service's index, as the owner's `browse.view` makes it,
/// recording each run it is asked about: its frames, how long the answer took and whether the run
/// measured as a bracket.
struct Timed<'a> {
    service: &'a EditorService,
    runs: RefCell<Vec<Value>>,
}

impl BracketProbe for Timed<'_> {
    fn measure(&self, frames: &[ViewItem]) -> Option<Vec<f32>> {
        // The index is borrowed per run, as nothing else holds it while organizing asks.
        let index = self.service.index().ok()?;
        let probe = previews::bracket_probe(index.connection());
        let started = Instant::now();
        let answer = probe.measure(frames);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        self.runs.borrow_mut().push(json!({
            "frames": frames.len(),
            "ms": ms,
            "bracket": answer.is_some(),
        }));
        answer
    }
}

#[test]
#[ignore = "a timing bench: cargo xtask catalog-measure runs it over a catalog it prepares"]
fn bracket_probe_per_run() {
    let var = |name: &str| {
        std::env::var(name)
            .unwrap_or_else(|_| panic!("{name} is set by cargo xtask catalog-measure"))
    };
    let catalog = PathBuf::from(var(CATALOG));
    let out = PathBuf::from(var(OUT));
    let evaluations: usize = var(EVALUATIONS).parse().expect("a count of evaluations");
    let query: ViewQuery = serde_json::from_value(json!({
        "source": {"kind": "folder", "path": var(FOLDER), "subfolders": true},
    }))
    .expect("a folder's query");
    let service = EditorService::open(&catalog).expect("the prepared catalog opens");
    let timed = Timed {
        service: &service,
        runs: RefCell::new(Vec::new()),
    };
    let mut events = EventCache::default();
    let mut items = 0;
    for _ in 0..evaluations {
        let evaluation = browse::evaluate(
            Context {
                service: &service,
                events: &mut events,
                probe: Probe::Given(&timed),
                limit: MAX_VIEW_ITEMS,
            },
            &query,
        )
        .expect("the folder's view");
        items = evaluation.items.len();
    }
    let report = json!({
        "evaluations": evaluations,
        "items": items,
        "runs": timed.runs.into_inner(),
    });
    std::fs::write(&out, serde_json::to_vec_pretty(&report).expect("json"))
        .expect("the samples are written");
}
