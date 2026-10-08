//! Local, consent-gated fitting against rendered Lightroom exports. Only figures are written.
use crate::*;
use luxforge_core::{
    BASIC_EFFECT, Cancel, ColorOperation, CompileStage, EffectStage, LOOK_EFFECT, Layer,
    ModuleRegistry, OwnerHandle, PreviewRequest, Stage,
    auto_tone::{self, AnalysisSample, AutoToneTargets, AutoToneValues, Statistics},
};
use luxforge_testkit::client;
use serde::Deserialize;
use std::{collections::BTreeSet, sync::Arc, thread::JoinHandle};

const MAX_PHOTOS: usize = 100;
const MAX_RETAINED: usize = MAX_PHOTOS * 1024 * 1024 * 13;
const TARGET_NAMES: [&str; 8] = [
    "median",
    "bright_guard",
    "highlights_scale",
    "shadows_scale",
    "spread",
    "clipping",
    "vibrance_chroma",
    "saturation_chroma",
];
const STEPS: [f64; 8] = [0.04, 0.01, 25., 25., 0.04, 0.00025, 0.01, 0.02];
const RANGES: [(f64, f64); 8] = [
    (0.3, 0.65),
    (0.001, 0.1),
    (50., 400.),
    (50., 300.),
    (0.3, 0.8),
    (0.00001, 0.005),
    (0.02, 0.15),
    (0.05, 0.3),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    photos: Vec<Photo>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Photo {
    id: String,
    original: PathBuf,
    lightroom_auto: PathBuf,
    xmp: PathBuf,
    /// Optional separate export with Lightroom's Auto disabled; without it no base-gap claim.
    lightroom_base: Option<PathBuf>,
}

struct Prepared {
    id: String,
    sample: Arc<AnalysisSample>,
    look: Option<Layer>,
    target: Statistics,
    base: Statistics,
    lightroom_base: Option<Statistics>,
    lightroom_values: Value,
}

/// A temporary private catalog; neither its source paths nor its source cache survives the run.
struct Scratch {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    path: PathBuf,
}
impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "luxforge-auto-fit-{}-{}",
            std::process::id(),
            client::request_id("scratch")
        ));
        fs::create_dir(&path)?;
        let (owner, join) = OwnerHandle::start(&path.join("catalog.sqlite"))?;
        Ok(Self {
            owner,
            join: Some(join),
            path,
        })
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn compile(
    registry: &ModuleRegistry,
    look: Option<&Layer>,
    values: AutoToneValues,
) -> std::result::Result<Vec<ColorOperation>, luxforge_core::Error> {
    let stage = CompileStage::exact(Stage {
        width: 32,
        height: 32,
    });
    let basic = registry
        .module("luxforge.basic")
        .ok_or_else(|| luxforge_core::Error::incompatible("Basic is unavailable"))?;
    let luxforge_core::Processing::Color(unit) = basic.compile(
        BASIC_EFFECT,
        luxforge_core::EFFECT_FORMAT,
        &Value::Object(values.fields()),
        stage,
    )?
    else {
        return Err(luxforge_core::Error::internal("Basic is not pointwise"));
    };
    let mut units = vec![unit];
    if let Some(layer) = look {
        let provider = registry
            .module("luxforge.look")
            .ok_or_else(|| luxforge_core::Error::incompatible("Look is unavailable"))?;
        let luxforge_core::Processing::Color(unit) =
            provider.compile(LOOK_EFFECT, layer.effect_format, &layer.payload, stage)?
        else {
            return Err(luxforge_core::Error::internal("Look is not pointwise"));
        };
        units.push(unit);
    }
    Ok(units)
}

fn rendered_statistics(sample: &AnalysisSample, units: &[ColorOperation]) -> Result<Statistics> {
    let mut output = sample.clone();
    output.source_white.fill(false);
    for chunk in output.rgb.chunks_mut(4096) {
        for operation in units {
            for unit in operation.units() {
                unit.apply_row(0, 0, chunk);
            }
        }
    }
    Ok(auto_tone::picture_statistics(&output, &Cancel::never())?)
}

fn exported(path: &Path) -> Result<Statistics> {
    // The core reader enforces the same file, dimension, colour-space and orientation limits.
    let image = luxforge_core::open_source(path)?;
    let longest = image.width.max(image.height);
    let grid = [image.width, image.height].map(|side| {
        if longest <= 1024 {
            side
        } else {
            ((u64::from(side) * 1024) / u64::from(longest)).max(1) as u32
        }
    });
    let mut sample = AnalysisSample {
        grid,
        rgb: Vec::with_capacity((grid[0] * grid[1]) as usize),
        source_white: Vec::with_capacity((grid[0] * grid[1]) as usize),
    };
    for y in 0..grid[1] {
        for x in 0..grid[0] {
            let sx = ((2 * u64::from(x) + 1) * u64::from(image.width) / (2 * u64::from(grid[0])))
                as usize;
            let sy = ((2 * u64::from(y) + 1) * u64::from(image.height) / (2 * u64::from(grid[1])))
                as usize;
            let at = (sy * image.width as usize + sx) * 4;
            sample.rgb.push(std::array::from_fn(|channel| {
                luxforge_core::colour::srgb::decode_table()[image.rgba[at + channel] as usize]
            }));
            // Clipping in a rendered export is an output statistic, never a source exclusion.
            sample.source_white.push(false);
        }
    }
    Ok(auto_tone::picture_statistics(&sample, &Cancel::never())?)
}

fn xmp_values(path: &Path, registry: &ModuleRegistry) -> Result<Value> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(luxforge_core::MAX_PRESET_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure(
        bytes.len() <= luxforge_core::MAX_PRESET_BYTES,
        "XMP exceeds the preset size limit",
    )?;
    let imported = luxforge_core::inspect_preset(std::str::from_utf8(&bytes)?, None, registry)?;
    let mut values = serde_json::Map::new();
    for (setting, field) in [
        "Exposure2012",
        "Contrast2012",
        "Highlights2012",
        "Shadows2012",
        "Whites2012",
        "Blacks2012",
        "Vibrance",
        "Saturation",
    ]
    .into_iter()
    .zip(auto_tone::FIELDS)
    {
        let raw = imported
            .report
            .mapped
            .iter()
            .find(|row| row.setting == setting)
            .map(|row| row.value.as_str())
            .or_else(|| {
                imported
                    .report
                    .neutral
                    .iter()
                    .find(|row| row.setting == setting)
                    .map(|row| row.value.as_str())
            });
        let value = raw
            .ok_or_else(|| format!("Lightroom XMP is missing {setting}"))?
            .parse::<f64>()?;
        ensure(
            value.is_finite(),
            format!("Lightroom XMP {setting} is not finite"),
        )?;
        values.insert(field.into(), json!(value));
    }
    Ok(Value::Object(values))
}

fn prepare(manifest: &Manifest, base: &Path) -> Result<Vec<Prepared>> {
    let scratch = Scratch::new()?;
    let client_id = scratch.owner.register();
    let registry = ModuleRegistry::builtin();
    let mut prepared = Vec::new();
    let mut retained = 0usize;
    let mut ids = BTreeSet::new();
    for (index, photo) in manifest.photos.iter().enumerate() {
        ensure(
            !photo.id.is_empty() && photo.id.len() <= 128 && ids.insert(photo.id.clone()),
            "photo ids must be unique and bounded",
        )?;
        let opened = client::open(
            &scratch.owner,
            client_id,
            &base.join(&photo.original),
            "auto-tone-fit",
        )?;
        let asset = serde_json::from_value(opened["asset"]["id"].clone())?;
        let job = scratch
            .owner
            .preview_job(PreviewRequest::new(client_id, asset))?;
        let evaluation = &job.evaluation;
        let recipe = evaluation.recipe();
        ensure(
            recipe.masks.is_empty(),
            "fit inputs must be originals without masks",
        )?;
        let index_basic = registry.insertion_index_for(&recipe.layers, BASIC_EFFECT);
        ensure(
            !recipe.layers.iter().any(|layer| {
                registry
                    .effect(&layer.effect_id)
                    .map(|(_, effect)| effect.stage)
                    == Some(EffectStage::Color)
                    && layer.effect_id != LOOK_EFFECT
            }),
            "fit originals may carry the starting Look but no creative colour edits",
        )?;
        let read = luxforge_core::tiles::read_analysis(
            evaluation,
            index_basic,
            &luxforge_core::tiles::ReferenceReads,
            &Cancel::never(),
        )?;
        retained += read.sample.bytes();
        ensure(
            retained <= MAX_RETAINED,
            "fitting samples exceed their aggregate memory bound",
        )?;
        let look = recipe
            .layers
            .iter()
            .find(|layer| layer.effect_id == LOOK_EFFECT)
            .cloned();
        let base_stats = rendered_statistics(
            &read.sample,
            &compile(&registry, look.as_ref(), AutoToneValues::default())?,
        )?;
        prepared.push(Prepared {
            id: photo.id.clone(),
            sample: read.sample,
            look,
            target: exported(&base.join(&photo.lightroom_auto))?,
            base: base_stats,
            lightroom_base: photo
                .lightroom_base
                .as_ref()
                .map(|path| exported(&base.join(path)))
                .transpose()?,
            lightroom_values: xmp_values(&base.join(&photo.xmp), &registry)?,
        });
        eprintln!(
            "Prepared Auto fitting photo {} of {}",
            index + 1,
            manifest.photos.len()
        );
    }
    // Input ordering cannot move a chosen photo into or out of the held-out quarter.
    prepared.sort_by(|a, b| {
        let hash = |id: &str| Sha256::digest(id.as_bytes());
        hash(&a.id).cmp(&hash(&b.id)).then_with(|| a.id.cmp(&b.id))
    });
    Ok(prepared)
}

fn loss(a: Statistics, b: Statistics) -> f64 {
    let a = [
        a.p50,
        a.p10,
        a.p90,
        a.bright,
        a.dark,
        a.highlight_clip,
        a.shadow_clip,
        a.mean_chroma,
    ];
    let b = [
        b.p50,
        b.p10,
        b.p90,
        b.bright,
        b.dark,
        b.highlight_clip,
        b.shadow_clip,
        b.mean_chroma,
    ];
    a.into_iter()
        .zip(b)
        .zip([4., 2., 2., 1., 1., 10., 10., 4.])
        .map(|((a, b), weight)| (a - b).abs() * weight)
        .sum()
}
fn evaluate(
    photo: &Prepared,
    registry: &ModuleRegistry,
    targets: AutoToneTargets,
) -> Result<auto_tone::AutoToneReport> {
    let search = if photo.look.as_ref().is_some_and(|layer| {
        layer.payload["amount"]
            .as_f64()
            .is_some_and(|amount| amount > 100.)
    }) {
        auto_tone::ExposureSearch::Exhaustive
    } else {
        auto_tone::ExposureSearch::Monotone
    };
    let mut report = auto_tone::solve_with_exposure_search(
        &photo.sample,
        targets,
        |values| compile(registry, photo.look.as_ref(), values),
        search,
        &Cancel::never(),
    )?;
    report.output = rendered_statistics(
        &photo.sample,
        &compile(registry, photo.look.as_ref(), report.values)?,
    )?;
    Ok(report)
}
fn objective(
    photos: &[Prepared],
    registry: &ModuleRegistry,
    targets: AutoToneTargets,
    held_out: bool,
) -> Result<f64> {
    let errors = photos
        .iter()
        .enumerate()
        .filter(|(index, _)| (index % 4 == 0) == held_out)
        .map(|(_, photo)| {
            evaluate(photo, registry, targets).map(|report| loss(report.output, photo.target))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(luxforge_testbase::Distribution::of(errors)
        .expect("non-empty fitting partition")
        .p50)
}
fn candidate(targets: AutoToneTargets, dimension: usize, value: f64) -> Result<AutoToneTargets> {
    let mut json = serde_json::to_value(targets)?;
    json[TARGET_NAMES[dimension]] = json!(value);
    let result: AutoToneTargets = serde_json::from_value(json)?;
    result.validate()?;
    Ok(result)
}

/// Fixed coordinate order and strict improvements make ties, rounding and search reproducible.
fn fit(photos: &[Prepared], rounds: usize) -> Result<Value> {
    let registry = ModuleRegistry::builtin();
    let defaults = AutoToneTargets::default();
    let mut targets = defaults;
    let initial = objective(photos, &registry, targets, false)?;
    let mut score = initial;
    let mut evaluations = 1;
    for round in 0..rounds {
        for dimension in 0..8 {
            let current = serde_json::to_value(targets)?[TARGET_NAMES[dimension]]
                .as_f64()
                .ok_or("target is not numeric")?;
            let step = STEPS[dimension] / 2f64.powi(round as i32);
            let mut best = targets;
            for direction in [-1., 1.] {
                let value =
                    (current + direction * step).clamp(RANGES[dimension].0, RANGES[dimension].1);
                let proposed = candidate(targets, dimension, value)?;
                let error = objective(photos, &registry, proposed, false)?;
                evaluations += 1;
                if error + 1e-12 < score {
                    best = proposed;
                    score = error;
                }
            }
            targets = best;
        }
        eprintln!(
            "Auto fitting round {} of {rounds}: training weighted median {score:.6}",
            round + 1
        );
    }
    let mut rows = Vec::new();
    for (index, photo) in photos.iter().enumerate() {
        let before = evaluate(photo, &registry, defaults)?;
        let after = evaluate(photo, &registry, targets)?;
        rows.push(json!({"id":photo.id,"held_out":index % 4 == 0,"lightroom_auto":photo.target,"luxforge_default":before.output,"luxforge_fitted":after.output,"default_error":loss(before.output, photo.target),"fitted_error":loss(after.output, photo.target),"luxforge_base":photo.base,"lightroom_base":photo.lightroom_base,"base_rendering_gap":photo.lightroom_base.map(|base| loss(photo.base,base)),"values":{"lightroom":photo.lightroom_values,"luxforge_default":before.values,"luxforge_fitted":after.values},"sample_bytes":photo.sample.bytes()}));
    }
    Ok(
        json!({"format":"luxforge.auto-tone-fit/1","candidate_algorithm":"auto-tone/2","adopted":false,"targets":targets,"defaults":defaults,"evaluations":evaluations,"training":{"default":initial,"fitted":score},"held_out":{"default":objective(photos,&registry,defaults,true)?,"fitted":objective(photos,&registry,targets,true)?},"metric":"Median over photographs of weighted absolute differences: median×4, P10/P90×2, bright/dark×1, clipping fractions×10, Oklab chroma×4","split":"SHA-256 of photo id, sorted; every fourth photo held out","slider_conversion":"No fitted Lightroom alignment conversions are installed; values are informational only","base_gap_note":"Only available when a separate Lightroom base export was supplied","scope":"Reference grids and compiled Basic plus starting Look; local rendered exports only; no image content retained; targets require owner review before adoption","photos":rows}),
    )
}

pub fn run(args: &mut Args) -> Result {
    let consent = args.flag("--consent-owner-photos");
    let manifest = args.path("--manifest")?;
    let output = args.path("--output")?;
    let rounds = args
        .value("--rounds")?
        .map(|n| n.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(3);
    args.done()?;
    // Before opening even the manifest: each invocation carries its own affirmative consent.
    ensure(
        consent,
        "Auto fitting requires per-run consent: review the input manifest and pass --consent-owner-photos for this run",
    )?;
    ensure(
        (1..=6).contains(&rounds),
        "fitting rounds must be within 1..=6",
    )?;
    ensure(!output.exists(), "the fitting report output must be new")?;
    let mut bytes = Vec::new();
    fs::File::open(&manifest)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure(bytes.len() <= 1024 * 1024, "fitting manifest exceeds 1 MiB")?;
    let manifest_data: Manifest = serde_json::from_slice(&bytes)?;
    ensure(
        (40..=MAX_PHOTOS).contains(&manifest_data.photos.len()),
        "the owner fitting round takes 40..=100 photos",
    )?;
    let photos = prepare(&manifest_data, manifest.parent().unwrap_or(Path::new(".")))?;
    write_json(&output, &fit(&photos, rounds)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_tone_fit_refuses_before_reading_any_owner_input_without_consent() {
        let mut args = Args(
            [
                "--manifest",
                "/missing/private-owner-input.json",
                "--output",
                "/missing/report.json",
            ]
            .map(OsString::from)
            .into(),
        );
        assert!(
            run(&mut args)
                .unwrap_err()
                .to_string()
                .contains("per-run consent")
        );
    }

    #[test]
    fn auto_tone_fit_recovers_perturbed_picture_targets_from_synthetic_exports() {
        let temp = Scratch::new().unwrap();
        let registry = ModuleRegistry::builtin();
        let known = AutoToneTargets {
            median: 0.5,
            spread: 0.64,
            ..Default::default()
        };
        let mut manifest = Manifest { photos: Vec::new() };
        for index in 0..8 {
            let original = temp.path.join(format!("original-{index}.jpg"));
            let exported = temp.path.join(format!("auto-{index}.jpg"));
            let xmp = temp.path.join(format!("auto-{index}.xmp"));
            let image = image::RgbImage::from_fn(64, 48, |x, y| {
                let value = (15 + index * 4 + x * (100 + index * 4) / 63 + y * 20 / 47) as u8;
                image::Rgb(if index % 3 == 0 {
                    [value, value.saturating_add(15), value.saturating_sub(8)]
                } else {
                    [value; 3]
                })
            });
            image::codecs::jpeg::JpegEncoder::new_with_quality(
                fs::File::create(&original).unwrap(),
                100,
            )
            .encode_image(&image)
            .unwrap();
            let decoded = luxforge_core::open_source(&original).unwrap();
            let mut sample = AnalysisSample {
                grid: [64, 48],
                rgb: decoded
                    .rgba
                    .chunks_exact(4)
                    .map(|pixel| {
                        std::array::from_fn(|c| {
                            luxforge_core::colour::srgb::decode_table()[pixel[c] as usize]
                        })
                    })
                    .collect(),
                source_white: vec![false; 64 * 48],
            };
            let report = auto_tone::solve(
                &sample,
                known,
                |values| compile(&registry, None, values),
                &Cancel::never(),
            )
            .unwrap();
            for operation in compile(&registry, None, report.values).unwrap() {
                for unit in operation.units() {
                    unit.apply_row(0, 0, &mut sample.rgb);
                }
            }
            let rgb: Vec<u8> = sample
                .rgb
                .iter()
                .flat_map(|pixel| {
                    pixel.map(|c| {
                        (luxforge_core::colour::srgb::encode(f64::from(c.clamp(0., 1.))) * 255.)
                            .round() as u8
                    })
                })
                .collect();
            let output = image::RgbImage::from_raw(64, 48, rgb).unwrap();
            image::codecs::jpeg::JpegEncoder::new_with_quality(
                fs::File::create(&exported).unwrap(),
                100,
            )
            .encode_image(&output)
            .unwrap();
            let values = report.values.fields();
            let attributes = [
                "Exposure2012",
                "Contrast2012",
                "Highlights2012",
                "Shadows2012",
                "Whites2012",
                "Blacks2012",
                "Vibrance",
                "Saturation",
            ]
            .into_iter()
            .zip(auto_tone::FIELDS)
            .map(|(key, field)| format!("crs:{key}=\"{}\"", values[field]))
            .collect::<Vec<_>>()
            .join(" ");
            fs::write(&xmp, format!(r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:ProcessVersion="15.4" crs:AutoTone="True" {attributes}/></rdf:RDF></x:xmpmeta>"#)).unwrap();
            manifest.photos.push(Photo {
                id: format!("synthetic-{index}"),
                lightroom_base: Some(original.clone()),
                original,
                lightroom_auto: exported,
                xmp,
            });
        }
        let photos = prepare(&manifest, &temp.path).unwrap();
        let report = fit(&photos, 3).unwrap();
        eprintln!(
            "synthetic fitting: targets {}, training {}, held-out {}",
            report["targets"], report["training"], report["held_out"]
        );
        assert!((report["targets"]["median"].as_f64().unwrap() - known.median).abs() <= 0.025);
        assert!((report["targets"]["spread"].as_f64().unwrap() - known.spread).abs() <= 0.04);
        assert!(
            report["held_out"]["fitted"].as_f64().unwrap()
                < report["held_out"]["default"].as_f64().unwrap() * 0.5
        );
        assert_eq!(
            report["photos"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["held_out"] == true)
                .count(),
            2
        );
        assert!(report["photos"].as_array().unwrap().iter().all(|row| {
            row["values"]["lightroom"].as_object().unwrap().len() == 8
                && row["base_rendering_gap"].is_number()
        }));
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains(temp.path.to_str().unwrap()),
            "reports retain figures and chosen ids, not source paths"
        );
    }
    #[test]
    fn auto_tone_fit_accepts_repository_photos_with_standin_exports_and_records_holdout() {
        let temp = Scratch::new().unwrap();
        let xmp = temp.path.join("standin.xmp");
        fs::write(&xmp, r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:ProcessVersion="15.4" crs:AutoTone="True" crs:Exposure2012="0" crs:Contrast2012="0" crs:Highlights2012="0" crs:Shadows2012="0" crs:Whites2012="0" crs:Blacks2012="0" crs:Vibrance="0" crs:Saturation="0"/></rdf:RDF></x:xmpmeta>"#).unwrap();
        let photos = [
            "orientation-1.jpg",
            "orientation-2.jpg",
            "orientation-5.jpg",
            "greyscale.jpg",
        ]
        .map(|name| {
            let original = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../fixtures/s0")
                .join(name);
            Photo {
                id: name.into(),
                original: original.clone(),
                lightroom_auto: original.clone(),
                lightroom_base: Some(original),
                xmp: xmp.clone(),
            }
        });
        let prepared = prepare(
            &Manifest {
                photos: photos.into(),
            },
            &temp.path,
        )
        .unwrap();
        let report = fit(&prepared, 1).unwrap();
        assert_eq!(report["adopted"], false);
        assert_eq!(report["photos"].as_array().unwrap().len(), 4);
        for row in report["photos"].as_array().unwrap() {
            assert!(row["default_error"].as_f64().unwrap().is_finite());
            assert!(row["fitted_error"].as_f64().unwrap().is_finite());
            assert!(row["base_rendering_gap"].as_f64().unwrap() < 1e-5);
        }
        assert!(report["held_out"]["fitted"].is_number());
    }
}
