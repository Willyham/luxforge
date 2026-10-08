//! Deliberate release measurements of Auto's engine, separate from normal correctness tests.
use super::{
    gpu_plan::install_output_encoding, gpu_tiles::GpuTiles, gpu_tiles_tests::host_adapter,
    testing::entry,
};
use luxforge_core::{
    AssetId, BASIC_EFFECT, Cancel, CompileStage, Evaluation, Layer, ModuleRegistry, PreviewSource,
    Recipe, RenderContext, Stage, auto_tone,
    tiles::{ReferenceTiles, TileCall, TileService},
};
use luxforge_testbase::{Distribution, HANG, paths};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::Instant,
};

#[test]
#[ignore = "release measurement; set LUXFORGE_GENERATED_FIXTURES and LUXFORGE_AUTO_TONE_OUTPUT to a new JSON file"]
fn auto_tone_measure_photo_sized_inputs() -> Result<(), &'static str> {
    if cfg!(debug_assertions) {
        return Err("measure an optimized release build");
    }
    let (backend, name) =
        host_adapter("auto_tone_measure_photo_sized_inputs").expect("a native adapter is required");
    assert!(install_output_encoding());
    let fixtures = std::env::var("LUXFORGE_GENERATED_FIXTURES").unwrap();
    let output = std::env::var("LUXFORGE_AUTO_TONE_OUTPUT").unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .unwrap();
    let catalog = paths::temp_catalog("auto-tone-measure");
    let (owner, join) = luxforge_core::OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let mut cases = Vec::new();
    for filename in ["24mp.jpg", "60mp.jpg"] {
        let source = PreviewSource::Jpeg(
            luxforge_core::open_source(&Path::new(&fixtures).join(filename)).unwrap(),
        );
        let registry = Arc::new(ModuleRegistry::builtin());
        let recipe = Recipe {
            layers: vec![Layer::new(BASIC_EFFECT, json!({}))],
            ..Recipe::default()
        };
        let context = RenderContext::new();
        let evaluation = Evaluation::new(
            registry,
            context.clone(),
            source,
            entry(&AssetId::new(), 0, None),
            recipe,
            None,
        );
        let gpu = GpuTiles::new(Some((backend.clone(), name.clone())), false);
        let reference = ReferenceTiles::new();
        for (renderer, service) in [
            ("gpu", &gpu as &dyn TileService),
            ("reference", &reference as &dyn TileService),
        ] {
            for cold in [true, false] {
                let mut rows = Vec::new();
                let tiles_before = gpu.figures().tiles;
                for _ in 0..30 {
                    if cold {
                        context.retain_analysis_for(None);
                    }
                    context.retain_analysis_for(Some(&evaluation.entry().asset_id));
                    let held = evaluation.clone();
                    let (sender, receiver) = mpsc::sync_channel(1);
                    let queued = Instant::now();
                    service.submit(TileCall::caller(client, Cancel::new(), move |reads, cancel| {
                        let read_started = Instant::now();
                        let sampled = luxforge_core::tiles::read_analysis(&held, 0, reads, cancel)?;
                        let read_ms = read_started.elapsed().as_secs_f64() * 1000.;
                        let started = Instant::now();
                        let basic = held.registry().module("luxforge.basic").unwrap();
                        let report = auto_tone::solve(&sampled.sample, Default::default(), |values| {
                            let luxforge_core::Processing::Color(unit) = basic.compile(BASIC_EFFECT, luxforge_core::EFFECT_FORMAT, &Value::Object(values.fields()), CompileStage::exact(Stage { width:32, height:32 }))? else { unreachable!() };
                            Ok(vec![unit])
                        }, cancel)?;
                        Ok(json!({"read_ms":read_ms,"solve_ms":started.elapsed().as_secs_f64()*1000.,"sample_bytes":sampled.sample.bytes(),"samples":sampled.sample.rgb.len(),"renderer":sampled.answered.record,"values":report.values}))
                    }, move |answer| { let _ = sender.send(answer); }));
                    let mut row = receiver.recv_timeout(HANG).unwrap().unwrap();
                    row["engine_ms"] = json!(queued.elapsed().as_secs_f64() * 1000.);
                    assert_eq!(row["renderer"], renderer);
                    rows.push(row);
                }
                let mut summary = json!({});
                for field in ["read_ms", "solve_ms", "engine_ms"] {
                    let numbers = rows.iter().map(|row| row[field].as_f64().unwrap());
                    let distribution = Distribution::of(numbers).expect("thirty runs");
                    summary[field] = json!({"p50":distribution.p50,"p95":distribution.p95});
                }
                let case = json!({"source":filename,"renderer":renderer,"sample_cache":if cold {"cold"} else {"warm"},"samples":30,"sample_bytes":rows[0]["sample_bytes"],"grid_points":rows[0]["samples"],"grid_scratch_peak_bytes":context.scratch().peak(),"solver_scratch_bytes":rows[0]["samples"].as_u64().unwrap()*8+4096*12,"gpu_tiles":gpu.figures().tiles-tiles_before,"summary":summary,"runs":rows});
                eprintln!("{filename} {renderer} cold={cold}: {summary}");
                cases.push(case);
            }
        }
        reference.stop();
    }
    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(catalog);
    serde_json::to_writer_pretty(file, &json!({"scope":"native headless release; prepared JPEG; neutral Basic prefix; engine includes worker queue, grid and solver; excludes source preparation, catalog commit and preview; cold means sample cache only, GPU device remains warm after its first call","adapter":name,"cases":cases})).unwrap();
    Ok(())
}
