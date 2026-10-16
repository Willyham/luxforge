//! Auto tone's corpus agreement (`docs/design/auto-tone.md`, "Acceptance and evidence" 2): for
//! every photograph of the qualification corpus this host has, the values `query.auto-tone`
//! answers when the owner's tile service is the desktop's GPU tile worker, and when it is the
//! reference renderer's, must agree within 0.02 EV on Exposure and 2 units on every other field.
//!
//! Each photograph is opened as the corpus opens it ([`Opened`]), once as it opens and once under
//! Detail, a perspective warp and a straightened crop, so the grid is read through a spatial prefix
//! and the output frame's geometry. Each stack is opened twice, in catalogs of its own: under a
//! host whose tile service is the GPU worker on this host's adapter, and under one whose service is
//! the reference's. A GPU answer that names another renderer than the GPU is a gap naming why,
//! never a pass. `LUXFORGE_AUTO_TONE_CORPUS_OUTPUT` names a new JSON file the cells are written to,
//! rewritten after each; `LUXFORGE_GENERATED_FIXTURES`, `LUXFORGE_RAW_MANIFEST` and
//! `LUXFORGE_GPU_CORPUS_SOURCES` are read as `cargo xtask gpu-qualification` sets them.
use super::{CorpusSource, Opened, corpus_sources};
use crate::app::gpu_tiles::GpuTiles;
use luxforge_core::auto_tone::FIELDS;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// The stacks each photograph is analysed under: as it opens, and through a spatial prefix and
/// the output frame's geometry.
fn stacks() -> [(&'static str, Vec<Value>); 2] {
    let api = |method: &str, params: Value| json!({"api": {"method": method, "params": params}});
    [
        ("as-opened", vec![]),
        (
            "detail-perspective-crop",
            vec![
                api(
                    "edit.set-detail",
                    json!({"luminance": 40, "colour": 40, "sharpening": 50, "radius": 1.0}),
                ),
                api(
                    "edit.set-perspective",
                    json!({"horizontal": 20, "vertical": -10}),
                ),
                api("edit.crop-fit", json!({"aspect": "16:9", "angle": 7.0})),
            ],
        ),
    ]
}

/// The largest difference each field may show: 0.02 EV on Exposure, two units elsewhere. The
/// committed values are on the fields' steps, so a hair over the bound is a float's rounding.
fn bound(field: &str) -> f64 {
    if field == "exposure" { 0.0200001 } else { 2.0 }
}

/// `query.auto-tone` over `source` with `steps` applied, the owner's tile service `tiles`.
fn answer(
    source: &CorpusSource,
    steps: &[Value],
    catalog: &Path,
    tiles: Option<Arc<dyn luxforge_core::tiles::TileService>>,
) -> Result<Value, String> {
    let opened = Opened::with_tiles(source, steps, catalog, tiles)?;
    luxforge_testkit::client::call(
        &opened.owner,
        opened.client,
        "query.auto-tone",
        json!({"asset_id": opened.asset.as_str()}),
    )
}

/// What a cell keeps of an answer: the values and the figures they came from.
fn kept(answer: &Value) -> Value {
    json!({
        "renderer": answer["renderer"],
        "values": answer["values"],
        "exposure_search": answer["exposure_search"],
        "source_clip_detection": answer["source_clip_detection"],
        "sample": answer["sample"],
        "input": answer["input"],
        "output": answer["output"],
        "bounded": answer["bounded"],
        "unmet": answer["unmet"],
    })
}

/// One stack on one photograph: both answers and each field's difference.
fn cell(
    gpu: &Arc<GpuTiles>,
    source: &CorpusSource,
    stack: &str,
    steps: &[Value],
    catalogs: &Path,
) -> Value {
    let mut record = json!({"source": source.id, "stack": stack});
    let run = |name: &str, tiles: Option<Arc<dyn luxforge_core::tiles::TileService>>| {
        let catalog = catalogs.join(format!("{}--{stack}--{name}.sqlite", source.id));
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            answer(source, steps, &catalog, tiles)
        }))
        .unwrap_or_else(|panic| {
            Err(panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                .unwrap_or_else(|| "a panic".to_owned()))
        })
    };
    let gpu_answer = run("gpu", Some(gpu.clone()));
    let reference_answer = run("reference", None);
    let (gpu_answer, reference_answer) = match (gpu_answer, reference_answer) {
        (Ok(gpu_answer), Ok(reference_answer)) => (gpu_answer, reference_answer),
        // The reference renderer refuses to analyse what it cannot hold, such as a paged derived
        // source's whole frame: nothing to compare the GPU's answer with, a gap and not a pass.
        (Ok(gpu_answer), Err(refused)) if refused.contains("resource-limit") => {
            record["gpu"] = kept(&gpu_answer);
            record["status"] = json!("gap");
            record["reason"] = json!(format!("the reference refuses: {refused}"));
            return record;
        }
        (gpu_answer, reference_answer) => {
            record["status"] = json!("error");
            record["gpu_error"] = json!(gpu_answer.as_ref().err());
            record["reference_error"] = json!(reference_answer.as_ref().err());
            if let Ok(answer) = &gpu_answer {
                record["gpu"] = kept(answer);
            }
            if let Ok(answer) = &reference_answer {
                record["reference"] = kept(answer);
            }
            return record;
        }
    };
    record["gpu"] = kept(&gpu_answer);
    record["reference"] = kept(&reference_answer);
    if reference_answer["renderer"]["record"] != "reference" {
        record["status"] = json!("error");
        record["error"] = json!("the reference host's answer names another renderer");
        return record;
    }
    if gpu_answer["renderer"]["record"] != "gpu" {
        record["status"] = json!("gap");
        record["reason"] = json!(format!(
            "the GPU host's grid was not read by the GPU: {}",
            gpu_answer["renderer"]
        ));
        return record;
    }
    let mut differences = json!({});
    let mut misses = Vec::new();
    for field in FIELDS {
        let (ours, theirs) = (
            gpu_answer["values"][field].as_f64(),
            reference_answer["values"][field].as_f64(),
        );
        let (Some(ours), Some(theirs)) = (ours, theirs) else {
            record["status"] = json!("error");
            record["error"] = json!(format!("no {field} in an answer"));
            return record;
        };
        let difference = ours - theirs;
        differences[field] = json!(difference);
        if difference.abs() > bound(field) {
            misses.push(field);
        }
    }
    record["differences"] = differences;
    // The statistics the values came from, which a grid within the display tolerance can move a
    // little without moving a committed value.
    let mut statistics = json!({});
    for part in ["input", "output"] {
        for (name, ours) in gpu_answer[part].as_object().into_iter().flatten() {
            if let (Some(ours), Some(theirs)) =
                (ours.as_f64(), reference_answer[part][name].as_f64())
            {
                statistics[format!("{part}.{name}")] = json!(ours - theirs);
            }
        }
    }
    record["statistic_differences"] = statistics;
    record["misses"] = json!(misses);
    record["status"] = json!(if misses.is_empty() { "agrees" } else { "miss" });
    record
}

fn write(output: &Path, document: &Value) {
    let staged = output.with_extension("partial");
    std::fs::write(&staged, serde_json::to_string_pretty(document).unwrap()).unwrap();
    std::fs::rename(&staged, output).unwrap();
}

/// The corpus's agreement run. It writes every cell, then fails if any cell misses or errs; a cell
/// whose GPU answer the GPU did not read, and a corpus source this host lacks, are gaps it reports.
#[test]
#[ignore = "release corpus run: set LUXFORGE_AUTO_TONE_CORPUS_OUTPUT to a new JSON file, \
            LUXFORGE_GENERATED_FIXTURES and LUXFORGE_RAW_MANIFEST"]
fn auto_tone_corpus_agreement() {
    let test = "auto_tone_corpus_agreement";
    let output = PathBuf::from(
        std::env::var("LUXFORGE_AUTO_TONE_CORPUS_OUTPUT")
            .expect("LUXFORGE_AUTO_TONE_CORPUS_OUTPUT names a new JSON file"),
    );
    assert!(!output.exists(), "{} exists", output.display());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let corpus = read(&root.join("fixtures/preview/corpus.json"));
    let generated = std::env::var("LUXFORGE_GENERATED_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("fixtures/generated"));
    let manifest = std::env::var("LUXFORGE_RAW_MANIFEST")
        .ok()
        .map(|path| read(Path::new(&path)));
    let wanted: Option<Vec<String>> = std::env::var("LUXFORGE_GPU_CORPUS_SOURCES")
        .ok()
        .map(|list| list.split(',').map(str::to_owned).collect());
    let Some(adapter) = crate::app::gpu_tiles_tests::host_adapter(test) else {
        write(
            &output,
            &json!({"test": test, "skipped": "no GPU adapter: not GPU evidence"}),
        );
        return;
    };
    assert!(crate::app::gpu_plan::install_output_encoding());
    let gpu = Arc::new(GpuTiles::new(Some(adapter.clone()), false));
    let sources: Vec<CorpusSource> = corpus_sources(&corpus, &generated, manifest.as_ref())
        .into_iter()
        .filter(|source| {
            wanted
                .as_ref()
                .is_none_or(|wanted| wanted.contains(&source.id))
        })
        .collect();
    let catalogs = output.with_extension("catalogs");
    std::fs::create_dir_all(&catalogs).unwrap();
    let mut document = json!({
        "test": test,
        "adapter": {"backend": adapter.0, "name": adapter.1},
        "bounds": {"exposure": 0.02, "other": 2},
        "sources": corpus["sources"].as_array().unwrap().iter().map(|source| {
            let id = source["id"].as_str().unwrap_or_default();
            match sources.iter().find(|found| found.id == id) {
                Some(found) => json!({"id": id, "status": "found", "path": found.path}),
                None if wanted.as_ref().is_some_and(|wanted| !wanted.iter().any(|w| w == id)) => {
                    json!({"id": id, "status": "not-selected"})
                }
                None => json!({"id": id, "status": "missing", "detail": "no file on this host"}),
            }
        }).collect::<Vec<_>>(),
        "cells": [],
        "complete": false,
    });
    write(&output, &document);
    for source in &sources {
        for (stack, steps) in stacks() {
            let record = cell(&gpu, source, stack, &steps, &catalogs);
            eprintln!(
                "{test}: {} {stack}: {} {}",
                source.id, record["status"], record["differences"]
            );
            document["cells"].as_array_mut().unwrap().push(record);
            write(&output, &document);
        }
    }
    let _ = std::fs::remove_dir_all(&catalogs);
    // Each field's largest difference over the cells that were measured.
    let measured: Vec<Value> = document["cells"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|cell| cell["differences"].is_object())
        .cloned()
        .collect();
    let mut largest = json!({});
    for field in FIELDS {
        let differences: Vec<f64> = measured
            .iter()
            .filter_map(|cell| cell["differences"][field].as_f64())
            .map(f64::abs)
            .collect();
        largest[field] = json!({
            "max_abs": differences.iter().copied().fold(0.0, f64::max),
            "exact": differences.iter().filter(|difference| **difference == 0.0).count(),
            "cells": differences.len(),
        });
    }
    let mut statistics = serde_json::Map::new();
    for cell in &measured {
        for (name, difference) in cell["statistic_differences"]
            .as_object()
            .into_iter()
            .flatten()
        {
            let difference = difference.as_f64().unwrap_or(0.0).abs();
            let held = statistics.entry(name.clone()).or_insert(json!(0.0));
            if difference > held.as_f64().unwrap_or(0.0) {
                *held = json!(difference);
            }
        }
    }
    document["largest"] = largest;
    document["largest_statistic_differences"] = Value::Object(statistics);
    document["complete"] = json!(true);
    write(&output, &document);
    let failed: Vec<String> = document["cells"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|cell| cell["status"] == "miss" || cell["status"] == "error")
        .map(|cell| format!("{} {}: {}", cell["source"], cell["stack"], cell["status"]))
        .collect();
    assert!(failed.is_empty(), "{failed:?}");
}
