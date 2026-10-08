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
use serde_json::json;
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
            registry.clone(),
            context.clone(),
            source,
            entry(&AssetId::new(), 0, None),
            recipe,
            None,
        );
        // The starting Look a RAW takes, at amount 200: above 100 it is not monotonic in
        // Exposure, so it is solved coarse to fine.
        let mut look = registry
            .module("luxforge.look")
            .unwrap()
            .original(&luxforge_core::OriginalContext {
                source: luxforge_core::SourceTag::Raw,
                raw: None,
                header: &luxforge_core::catalog_types::HeaderMetadata::default(),
                preferences: luxforge_core::OriginalPreferences::default(),
            })
            .unwrap()
            .unwrap()
            .payload;
        look["amount"] = json!(200.);
        let models = [
            ("neutral", vec![Layer::new(BASIC_EFFECT, json!({}))]),
            (
                "look-200",
                vec![
                    Layer::new(BASIC_EFFECT, json!({})),
                    Layer::new(luxforge_core::LOOK_EFFECT, look),
                ],
            ),
        ];
        let gpu = GpuTiles::new(Some((backend.clone(), name.clone())), false);
        let reference = ReferenceTiles::new();
        for (renderer, service) in [
            ("gpu", &gpu as &dyn TileService),
            ("reference", &reference as &dyn TileService),
        ] {
            for ((model_name, layers), cold) in
                [(&models[0], true), (&models[0], false), (&models[1], false)]
            {
                let mut rows = Vec::new();
                let tiles_before = gpu.figures().tiles;
                for _ in 0..30 {
                    if cold {
                        context.release_grids(&evaluation.entry().asset_id);
                    }
                    let held = evaluation.clone();
                    let (sender, receiver) = mpsc::sync_channel(1);
                    let (grids, grid) = mpsc::sync_channel(1);
                    let queued = Instant::now();
                    // The tile service reads the grid; the solve runs after it off the service's
                    // thread, here as on the core's analysis worker.
                    service.submit(TileCall::caller(client, Cancel::new(), move |reads, cancel| {
                        let read_started = Instant::now();
                        let sampled = luxforge_core::tiles::read_grid(&held, 0, reads, cancel)?;
                        let read_ms = read_started.elapsed().as_secs_f64() * 1000.;
                        let row = json!({"read_ms":read_ms,"sample_bytes":sampled.sample.bytes(),"samples":sampled.sample.rgb.len(),"renderer":sampled.answered.record});
                        let _ = grids.send(sampled);
                        Ok(row)
                    }, move |answer| { let _ = sender.send(answer); }));
                    let mut row = receiver.recv_timeout(HANG).unwrap().unwrap();
                    let sampled = grid.recv_timeout(HANG).unwrap();
                    let started = Instant::now();
                    let model = auto_tone::forward_model(
                        evaluation.registry(),
                        layers,
                        0,
                        CompileStage::exact(Stage {
                            width: 32,
                            height: 32,
                        }),
                    )
                    .unwrap();
                    let report = model
                        .solve(&sampled.sample, Default::default(), &sampled.cancel)
                        .unwrap();
                    row["exposure_search"] = json!(report.exposure_search);
                    row["solve_ms"] = json!(started.elapsed().as_secs_f64() * 1000.);
                    row["values"] = json!(report.values);
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
                let case = json!({"source":filename,"model":model_name,"renderer":renderer,"sample_cache":if cold {"cold"} else {"warm"},"samples":30,"sample_bytes":rows[0]["sample_bytes"],"grid_points":rows[0]["samples"],"grid_scratch_peak_bytes":context.scratch().peak(),"solver_scratch_bytes":auto_tone::scratch_bytes(rows[0]["samples"].as_u64().unwrap() as usize),"pool_threads":std::thread::available_parallelism().map_or(0, |n| n.get()),"gpu_tiles":gpu.figures().tiles-tiles_before,"summary":summary,"runs":rows});
                eprintln!("{filename} {model_name} {renderer} cold={cold}: {summary}");
                cases.push(case);
            }
        }
        reference.stop();
    }
    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(catalog);
    serde_json::to_writer_pretty(file, &json!({"scope":"native headless release; prepared JPEG; neutral Basic prefix; the model Basic alone, cold and warm, and Basic then the starting Look at amount 200, warm; engine includes the tile queue, the grid read on the tile service and the solve after it off the tile service's thread; excludes the analysis worker's hand-off, source preparation, catalog commit and preview; cold means sample cache only, GPU device remains warm after its first call","adapter":name,"cases":cases})).unwrap();
    Ok(())
}
