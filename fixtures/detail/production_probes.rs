//! Standalone production qualification probes. This includes only the independent study's
//! constructors/metrics and injects the real full render as its evaluator; it does not replace
//! the committed f64 oracle or hide failed measurements.
#![allow(dead_code)]
include!("../../crates/luxforge-reference/tests/studies/detail.rs");

use luxforge_core as core;
use std::sync::Arc;

fn payload(p: Params) -> serde_json::Value {
    serde_json::json!({"sharpening":p.sharpening,"radius":p.radius,
        "sharpen-detail":p.sharpen_detail,"sharpen-masking":p.sharpen_masking,
        "luminance":p.luminance,"luminance-detail":p.luminance_detail,
        "colour":p.colour,"colour-detail":p.colour_detail})
}
fn recipe(p: Params, basic: bool) -> core::Recipe {
    let mut layers = vec![core::Layer::new(core::DETAIL_EFFECT, payload(p))];
    if basic {
        layers.push(core::Layer::new(
            core::BASIC_EFFECT,
            serde_json::json!({"exposure":2.0,"shadows":100.0}),
        ));
    }
    core::Recipe {
        layers,
        ..core::Recipe::default()
    }
}
fn source(input: &Image, jpeg: bool) -> core::PreviewSource {
    if jpeg {
        core::PreviewSource::Jpeg(core::SourceImage {
            width: input.width as u32,
            height: input.height as u32,
            rgba: Arc::new(
                input
                    .pixels
                    .iter()
                    .flat_map(|p| {
                        let c = p.map(srgb::code);
                        [c[0], c[1], c[2], 255]
                    })
                    .collect(),
            ),
            fingerprint: "detail-production-probe".into(),
            orientation: 1,
            capture: Default::default(),
        })
    } else {
        let planes: Vec<f32> = (0..3)
            .flat_map(|c| input.pixels.iter().map(move |p| p[c] as f32))
            .collect();
        core::PreviewSource::Raw {
            image: core::LinearImage::new(input.width as u32, input.height as u32, planes).unwrap(),
            settings: core::LinearSettings::default(),
        }
    }
}
fn decode(raster: &core::Raster) -> Image {
    Image::new(
        raster.width as usize,
        raster.height as usize,
        raster
            .rgba
            .chunks_exact(4)
            .map(|p| [srgb::decode(p[0]), srgb::decode(p[1]), srgb::decode(p[2])])
            .collect(),
    )
}
fn full(input: &Image, jpeg: bool, recipe: &core::Recipe) -> core::Raster {
    let source = source(input, jpeg);
    core::render(
        &core::ModuleRegistry::builtin(),
        &source,
        recipe,
        core::RenderOptions::default(),
        &core::RenderContext::new(),
    )
    .unwrap()
    .frame(core::SnapshotId::new())
    .unwrap()
}
fn rendered(input: &Image, jpeg: bool, p: Params) -> Image {
    decode(&full(input, jpeg, &recipe(p, false)))
}

fn job(input: &Image, jpeg: bool, p: Params, bounds: core::ProxyBounds) -> core::PreviewJob {
    let asset = core::AssetId::new();
    let r = recipe(p, false);
    let mut snapshot = core::Snapshot::original(asset.clone());
    snapshot.recipe = r.clone();
    let entry = core::HistoryEntry {
        id: core::EntryId::new(),
        asset_id: asset,
        sequence: 1,
        action_id: "set-detail".into(),
        label: "Detail qualification".into(),
        parameters: payload(p),
        actor: "detail-study".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 1,
        snapshot,
        undo_parent: None,
        restore_target: None,
    };
    let evaluation = core::Evaluation::new(
        Arc::new(core::ModuleRegistry::builtin()),
        core::RenderContext::new(),
        source(input, jpeg),
        entry,
        r,
        None,
    );
    let mut job = core::PreviewJob::new(evaluation).unwrap();
    job.proxy = Some(bounds);
    job
}
fn production_proxy() -> Vec<Measurement> {
    let input = proxy_probe();
    let mut results = vec![];
    for jpeg in [false, true] {
        let mut queue = core::PreviewQueue::default();
        let (tx, rx) = std::sync::mpsc::channel();
        queue.set_waker(Arc::new(move || {
            let _ = tx.send(());
        }));
        for denominator in [2, 3, 4, 6, 8] {
            queue.request(job(
                &input,
                jpeg,
                moderate(),
                core::ProxyBounds {
                    width: (input.width / denominator) as u32,
                    height: (input.height / denominator) as u32,
                },
            ));
            let mut proxy = None;
            loop {
                if let Some(result) = queue.poll() {
                    if let Some(p) = result.proxy() {
                        proxy = Some(p.raster.clone());
                    }
                    if let Some(e) = result.exact() {
                        e.result.as_ref().unwrap();
                        let target = e.display.as_ref().expect("Detail exact settlement");
                        let proxy = proxy.take().expect("moving proxy");
                        assert_eq!((proxy.width, proxy.height), (target.width, target.height));
                        let mut errors: Vec<f64> = proxy
                            .rgba
                            .chunks_exact(4)
                            .zip(target.rgba.chunks_exact(4))
                            .flat_map(|(a, b)| {
                                (0..3).map(move |c| (f64::from(a[c]) - f64::from(b[c])).abs())
                            })
                            .collect();
                        let rms = (errors.iter().map(|v| v * v).sum::<f64>() / errors.len() as f64)
                            .sqrt();
                        errors.sort_by(f64::total_cmp);
                        let p99 = errors[((errors.len() - 1) as f64 * 0.99).ceil() as usize];
                        let domain = if jpeg { "jpeg" } else { "raw" };
                        results.push(Measurement {
                            name: format!("production-{domain}-proxy-1over{denominator}-rms"),
                            value: rms,
                            minimum: None,
                            maximum: Some(1.2),
                        });
                        results.push(Measurement {
                            name: format!("production-{domain}-proxy-1over{denominator}-p99"),
                            value: p99,
                            minimum: None,
                            maximum: Some(3.0),
                        });
                        break;
                    }
                } else {
                    rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap();
                }
            }
        }
    }
    results
}
fn production_banding() -> Vec<Measurement> {
    let input = banding_probe();
    let nr = Params {
        luminance: 40.0,
        colour: 40.0,
        ..Params::default()
    };
    let first = full(&input, true, &recipe(nr, false));
    let eight = decode(&first);
    let basic = core::Recipe {
        layers: vec![core::Layer::new(
            core::BASIC_EFFECT,
            serde_json::json!({"exposure":2.0,"shadows":100.0}),
        )],
        ..core::Recipe::default()
    };
    [
        ("forced-8bit", full(&eight, true, &basic)),
        ("jpeg-16bit", full(&input, true, &recipe(nr, true))),
        ("raw-float", full(&input, false, &recipe(nr, true))),
    ]
    .into_iter()
    .map(|(label, raster)| {
        let w = raster.width as usize;
        let value = (32..96)
            .flat_map(|y| {
                raster.rgba[y * w * 4..(y + 1) * w * 4]
                    .chunks_exact(4)
                    .map(|p| p[1])
                    .collect::<Vec<_>>()
                    .windows(2)
                    .map(|p| (f64::from(p[1]) - f64::from(p[0])).abs())
                    .collect::<Vec<_>>()
            })
            .fold(0.0, f64::max);
        Measurement {
            name: format!("production-banding-{label}-max-neighbour-code-jump"),
            value,
            minimum: None,
            maximum: if label == "jpeg-16bit" {
                Some(1.0)
            } else {
                None
            },
        }
    })
    .collect()
}
fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("fresh measurement output JSON");
    // Callback extraction must leave the frozen independent oracle byte-for-byte unchanged.
    let frozen: serde_json::Value =
        serde_json::from_slice(&fs::read("fixtures/detail/measurements.json").unwrap()).unwrap();
    assert_eq!(serde_json::to_value(measurements()).unwrap(), frozen);
    let nr = Params {
        sharpening: 0.0,
        ..moderate()
    };
    let mut m = flat_measurements_with(|image, jpeg| rendered(image, jpeg, nr));
    let mut combined = flat_measurements_with(|image, jpeg| rendered(image, jpeg, moderate()));
    for row in &mut combined {
        row.name = format!("combined-{}", row.name);
    }
    m.extend(combined);
    let correlated: Vec<_> = correlated_measurements_with(|image, _| rendered(image, false, nr))
        .into_iter()
        .filter(|row| row.name.contains("levels4"))
        .collect();
    let original: Vec<_> =
        correlated_measurements_with(|image, _| rendered(image, false, Params::default()))
            .into_iter()
            .filter(|row| row.name.contains("levels4"))
            .collect();
    // This same-boundary comparison is a diagnostic alongside the prescribed full-precision
    // input gate. It does not replace that gate or change either of its thresholds.
    for (processed, baseline) in correlated.iter().zip(&original) {
        m.push(Measurement {
            name: format!("{}-terminal-original-relative-diagnostic", processed.name),
            value: 1.0 - (1.0 - processed.value) / (1.0 - baseline.value),
            minimum: None,
            maximum: None,
        });
    }
    m.extend(correlated);
    m.extend(edge_measurements_with(|image| {
        rendered(
            image,
            false,
            Params {
                sharpening: 50.0,
                ..Params::default()
            },
        )
    }));
    m.extend(production_proxy());
    m.extend(production_banding());
    fs::write(&out, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    for row in &m {
        if !row.passed() {
            println!("FAIL {}: {}", row.name, row.value);
        }
    }
    println!("Recorded {} full-pipeline measurements", m.len());
}
