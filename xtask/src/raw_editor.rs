//! Reproducible full-editor RAW evidence. Every app run uses the background-only smoke launcher.
use crate::*;
use lightwell_core::{EditorService, SourceKind};
use serde::Deserialize;
use std::time::{Duration, Instant};

type PhotoSamples = Vec<[u8; 3]>;
type VerifiedFrames = (Value, Vec<Value>, Vec<PhotoSamples>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    sources: Vec<Source>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    id: String,
    path: PathBuf,
    sha256: String,
    mode: String,
    make: String,
    model: String,
    /// Full unpacked sensor dimensions, before default crop and orientation.
    sensor_dimensions: Option<[u32; 2]>,
    /// Absolute sensor-space rectangle [x, y, width, height].
    active_area: Option<[u32; 4]>,
    /// Absolute sensor-space rectangle [x, y, width, height].
    default_crop: Option<[u32; 4]>,
    source_dimensions: [u32; 2],
    orientation: u8,
    /// A fixture-verified unclipped, non-dark upright content point for the sensor picker.
    neutral_point: [u32; 2],
}

const JOURNEY_STEPS: usize = 13;
const HISTORICAL_FRAME: usize = 10;
const CURRENT_FRAME: usize = 11;
const FINAL_FRAME: usize = 13;

fn script(source: &Source) -> Value {
    json!([
        {"api":{"method":"edit.set-raw-exposure","params":{"ev":1.0}}},
        {"api":{"method":"edit.set-raw-red-gain","params":{"gain":3.0}}},
        {"api":{"method":"edit.set-raw-blue-gain","params":{"gain":0.9}}},
        {"api":{"method":"edit.set-raw-temperature","params":{"kelvin":5500.0}}},
        {"api":{"method":"edit.set-raw-tint","params":{"tint":10.0}}},
        {"api":{"method":"edit.pick-raw-neutral","params":{"x":source.neutral_point[0],"y":source.neutral_point[1]}}},
        {"api":{"method":"edit.transform","params":{"transform":"rotate-right"}}},
        {"api":{"method":"edit.crop-fit","params":{"aspect":"4:3","angle":0.0}}},
        {"api":{"method":"history.undo","params":{}}},
        {"preview":{"sequence":0}},
        {"preview":"current"},
        {"view":{"zoom":100.0}},
        {"view":{"zoom":"fit"}}
    ])
}

fn profile<'a>(catalog: &'a Value, source: &Source) -> Result<&'a Value> {
    catalog["cameras"]
        .as_array()
        .and_then(|cameras| {
            cameras.iter().find(|camera| {
                camera["make"] == source.make
                    && camera["model"] == source.model
                    && camera["modes"]
                        .as_array()
                        .is_some_and(|modes| modes.iter().any(|mode| mode["id"] == source.mode))
            })
        })
        .ok_or_else(|| {
            format!(
                "RAW mode {} is absent or mismatched in camera catalog",
                source.mode
            )
            .into()
        })
}

fn validate_manifest(
    manifest_path: &Path,
    manifest: &Manifest,
    catalog: &Value,
) -> Result<Vec<PathBuf>> {
    ensure(manifest.format == 1, "RAW editor manifest format must be 1")?;
    ensure(
        !manifest.sources.is_empty(),
        "RAW editor manifest needs sources",
    )?;
    let base = manifest_path.parent().ok_or("Manifest has no directory")?;
    let mut ids = std::collections::HashSet::new();
    let mut paths = Vec::new();
    for source in &manifest.sources {
        ensure(
            !source.id.is_empty()
                && source
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && ids.insert(source.id.clone()),
            "RAW source id must be unique ASCII letters, digits, hyphens or underscores",
        )?;
        ensure(
            profile(catalog, source).is_ok(),
            format!(
                "RAW mode {} is absent or mismatched in camera catalog",
                source.mode
            ),
        )?;
        ensure(
            source.sha256.len() == 64 && source.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            format!("Invalid SHA-256 for {}", source.id),
        )?;
        ensure(
            source.source_dimensions.iter().all(|side| *side > 0)
                && (1..=8).contains(&source.orientation)
                && source
                    .neutral_point
                    .iter()
                    .zip(source.source_dimensions)
                    .all(|(coordinate, side)| {
                        *coordinate >= 6 && coordinate.checked_add(6).is_some_and(|end| end < side)
                    }),
            format!("Invalid geometry for {}", source.id),
        )?;
        if source.mode == "DjiAir2sDng16" {
            let (Some(sensor), Some(active), Some(crop)) = (
                source.sensor_dimensions,
                source.active_area,
                source.default_crop,
            ) else {
                return Err(format!(
                    "DJI source {} needs sensor, active and crop geometry",
                    source.id
                )
                .into());
            };
            ensure(
                sensor.iter().all(|side| *side > 0)
                    && [active, crop].into_iter().all(|rect| {
                        rect[2] > 0
                            && rect[3] > 0
                            && rect[0]
                                .checked_add(rect[2])
                                .is_some_and(|end| end <= sensor[0])
                            && rect[1]
                                .checked_add(rect[3])
                                .is_some_and(|end| end <= sensor[1])
                    })
                    && crop[0] >= active[0]
                    && crop[1] >= active[1]
                    && crop[0] + crop[2] <= active[0] + active[2]
                    && crop[1] + crop[3] <= active[1] + active[3]
                    && (if source.orientation >= 5 {
                        [crop[3], crop[2]]
                    } else {
                        [crop[2], crop[3]]
                    }) == source.source_dimensions,
                format!("DJI source {} geometry is inconsistent", source.id),
            )?;
        }
        let path = if source.path.is_absolute() {
            source.path.clone()
        } else {
            base.join(&source.path)
        }
        .canonicalize()?;
        ensure(
            hash(&path)?.eq_ignore_ascii_case(&source.sha256),
            format!("RAW source {} differs from manifest SHA-256", source.id),
        )?;
        paths.push(path);
    }
    Ok(paths)
}

fn rss_mib(root: &Path, pid: u32) -> Result<f64> {
    let raw = output(root, "ps", &["-o", "rss=", "-p", &pid.to_string()])?;
    Ok(raw.trim().parse::<f64>()? / 1024.0)
}

fn run_app(
    root: &Path,
    binary: &Path,
    evidence: &Path,
    catalog: &Path,
    source: &Path,
    script_path: Option<&Path>,
) -> Result<Value> {
    let mut args = vec![
        "--data-root".into(),
        evidence.join("data").into_os_string(),
        "--catalog".into(),
        catalog.as_os_str().to_owned(),
        "--evidence-dir".into(),
        evidence.as_os_str().to_owned(),
        "--open".into(),
        source.as_os_str().to_owned(),
    ];
    if let Some(path) = script_path {
        args.extend(["--evidence-script".into(), path.as_os_str().to_owned()]);
    }
    let log = evidence.with_extension("log");
    let started = Instant::now();
    let mut child = smoke::spawn_editor(root, binary, &args, &log)?;
    let mut rss = Vec::new();
    let status = loop {
        if let Some(status) = child.child.try_wait()? {
            break status;
        }
        ensure(
            started.elapsed() < Duration::from_secs(70),
            "RAW editor app deadline exceeded",
        )?;
        if let Ok(sample) = rss_mib(root, child.child.id()) {
            rss.push(json!([started.elapsed().as_secs_f64() * 1000.0, sample]));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    ensure(
        status.success(),
        format!("RAW editor exited {status}; see {}", log.display()),
    )?;
    let peak = rss
        .iter()
        .filter_map(|pair| pair[1].as_f64())
        .fold(0.0_f64, f64::max);
    Ok(json!({
        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "sampled_peak_rss_mib":peak,
        "rss_samples":rss,
        "exit_code":status.code(),
        "log":log.file_name().and_then(|s|s.to_str()),
    }))
}

fn photo_samples(path: &Path, frame: &Value) -> Result<Vec<[u8; 3]>> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let columns = smoke::columns(frame)?.ok_or("Missing photo surface columns")?;
    ensure(
        columns[1] <= width && columns[0] < columns[1],
        "Invalid photo surface",
    )?;
    let cx = (columns[0] + columns[1]) / 2;
    let cy = height / 2;
    let dx = (columns[1] - columns[0]) / 5;
    let dy = height / 5;
    let mut pixels = Vec::new();
    for iy in 0..17 {
        for ix in 0..17 {
            let x = cx - dx + (2 * dx * ix) / 16;
            let y = cy - dy + (2 * dy * iy) / 16;
            pixels.push(image.get_pixel(x, y).0);
        }
    }
    Ok(pixels)
}

fn pixel_difference(a: &[[u8; 3]], b: &[[u8; 3]]) -> Result<Value> {
    ensure(a.len() == b.len(), "Photo sample layouts differ")?;
    let mut changed = 0usize;
    let mut any_changed = 0usize;
    let mut absolute = 0u64;
    for (left, right) in a.iter().zip(b) {
        let distance: u64 = left
            .iter()
            .zip(right)
            .map(|(x, y)| u64::from(x.abs_diff(*y)))
            .sum();
        changed += usize::from(distance > 3);
        any_changed += usize::from(distance > 0);
        absolute += distance;
    }
    Ok(
        json!({"changed_samples":changed,"any_changed_samples":any_changed,"mean_absolute_channel_difference":absolute as f64/(a.len()*3) as f64}),
    )
}

fn displayed_entry(frame: &Value) -> Result<&str> {
    frame["state"]["stack"]["displayed"]["entry"]
        .as_str()
        .ok_or_else(|| "Captured frame lacks displayed entry identity".into())
}

/// How far a displayed RAW control may sit from the payload it mirrors: half of the last digit the
/// parameter declares it shows. The control text is the payload rounded to that precision, so the
/// only honest tolerance is the rounding itself, read from the descriptor rather than written here.
fn displayed_tolerance(action: &str, parameter: &str) -> Result<f64> {
    let registry = lightwell_core::ModuleRegistry::builtin();
    let precision = registry
        .descriptors()
        .into_iter()
        .find_map(|module| module.action(action))
        .and_then(|action| action.parameter(parameter))
        .and_then(|parameter| parameter.precision)
        .ok_or_else(|| format!("{action}.{parameter} declares no display precision"))?;
    Ok(0.5 * 10f64.powi(-i32::from(precision)) + 1e-9)
}

fn verify_displayed_raw_controls(frame: &Value) -> Result {
    let state = &frame["state"];
    let displayed = &state["stack"]["displayed"];
    let layers = displayed["layers"]
        .as_array()
        .ok_or("Displayed layers missing")?;
    let raw = layers
        .iter()
        .find(|layer| layer["effect"] == "lightwell.raw")
        .ok_or("Displayed RAW layer missing")?;
    let payload = &raw["payload"];
    let controls = &state["controls"];
    let effective_gains = if payload["wb_mode"] == "as-shot" {
        &payload["as_shot_gains"]
    } else {
        &payload["gains"]
    };
    for (action, parameter, value) in [
        ("set-raw-exposure", "ev", &payload["exposure_ev"]),
        ("set-raw-red-gain", "gain", &effective_gains[0]),
        ("set-raw-blue-gain", "gain", &effective_gains[2]),
    ] {
        let key = format!("{action}.{parameter}");
        let shown: f64 = controls[&key]
            .as_str()
            .ok_or("Displayed RAW control value missing")?
            .parse()?;
        let expected = value
            .as_f64()
            .ok_or("Displayed RAW payload value missing")?;
        ensure(
            (shown - expected).abs() <= displayed_tolerance(action, parameter)?,
            format!("Displayed {key} control differs from displayed RAW layer"),
        )?;
    }
    for (action, parameter, expected) in [
        (
            "set-raw-temperature",
            "kelvin",
            payload["temperature_kelvin"].as_f64().unwrap_or(6504.0),
        ),
        (
            "set-raw-tint",
            "tint",
            payload["tint"].as_f64().unwrap_or(0.0),
        ),
    ] {
        let key = format!("{action}.{parameter}");
        let shown: f64 = controls[&key]
            .as_str()
            .ok_or("Displayed RAW control value missing")?
            .parse()?;
        ensure(
            (shown - expected).abs() <= displayed_tolerance(action, parameter)?,
            format!("Displayed {key} control differs from displayed RAW layer"),
        )?;
    }
    Ok(())
}

fn verify_frames(evidence: &Path, expected_frames: usize) -> Result<VerifiedFrames> {
    let (app, events) = smoke::preamble(evidence, expected_frames)?;
    ensure(app["had_input_errors"] == false, "RAW script input failed")?;
    let frames = app["frames"].as_array().ok_or("Missing captured frames")?;
    let mut pixels = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let file = smoke::frame_identity(evidence, &app, frame)?;
        let state = &frame["state"];
        ensure(
            state["phase"] == "ready"
                && state["displayed_generation"] == state["requested_generation"]
                && state["render_error"].is_null()
                && state["error_code"].is_null(),
            format!("Frame {index} is not a ready rendered RAW image"),
        )?;
        let _ = displayed_entry(frame)?;
        verify_displayed_raw_controls(frame)?;
        pixels.push(photo_samples(&file, frame)?);
    }
    Ok((app, events, pixels))
}

fn event_times(events: &[Value]) -> Value {
    let mut result = serde_json::Map::new();
    for name in ["decoded", "render_ready", "frame_captured"] {
        if let Some(event) = events.iter().find(|event| event["event"] == name) {
            result.insert(name.into(), event["elapsed_ms"].clone());
            if name == "decoded" {
                result.insert(
                    "open_to_raster_ms".into(),
                    event["detail"]["open_to_raster_ms"].clone(),
                );
            }
        }
    }
    Value::Object(result)
}

fn script_stage_times(events: &[Value]) -> Result<Vec<Value>> {
    let starts: Vec<_> = events
        .iter()
        .filter(|event| event["event"] == "script_step")
        .collect();
    ensure(
        starts.len() == JOURNEY_STEPS,
        "RAW scripted event count differs from journey",
    )?;
    let mut result = Vec::new();
    for (index, start) in starts.iter().enumerate() {
        let began = start["elapsed_ms"]
            .as_f64()
            .ok_or("RAW script step lacks elapsed time")?;
        let ended = starts
            .get(index + 1)
            .and_then(|event| event["elapsed_ms"].as_f64())
            .unwrap_or(f64::INFINITY);
        let in_step = |event: &&Value| {
            event["elapsed_ms"]
                .as_f64()
                .is_some_and(|time| time >= began && time < ended)
        };
        let event = |name: &str| {
            events
                .iter()
                .filter(&in_step)
                .find(|event| event["event"] == name)
        };
        let captured = event("frame_captured")
            .and_then(|event| event["elapsed_ms"].as_f64())
            .ok_or("RAW script step has no correlated frame capture event")?;
        let displayed = event("preview_displayed")
            .or_else(|| event("render_ready"))
            .and_then(|event| event["elapsed_ms"].as_f64());
        ensure(
            index >= 9 || displayed.is_some(),
            "RAW mutation has no correlated display event",
        )?;
        let decoded =
            event("decoded").and_then(|event| event["detail"]["open_to_raster_ms"].as_f64());
        result.push(json!({
            "step":index+1,
            "request":start["detail"]["request"],
            "request_to_display_ms":displayed.map(|time|time-began),
            "request_to_capture_ms":captured-began,
            "source_to_raster_ms":decoded,
        }));
    }
    Ok(result)
}

fn verify_catalog(
    catalog: &Path,
    source: &Source,
    path: &Path,
    camera_profile: &Value,
) -> Result<(String, String, u64)> {
    let service = EditorService::open(catalog)?;
    let assets = service.assets()?;
    ensure(assets.len() == 1, "RAW run imported unexpected asset count")?;
    let asset = &assets[0];
    ensure(asset.locator == path, "RAW catalog locator changed")?;
    ensure(
        [asset.width, asset.height] == source.source_dimensions,
        "RAW catalog source dimensions differ from manifest",
    )?;
    let SourceKind::Raw { metadata } = &asset.source else {
        return Err("RAW source was imported as JPEG".into());
    };
    let crop = &metadata["default_crop"];
    let crop_dimensions = if source.orientation >= 5 {
        [crop["height"].as_u64(), crop["width"].as_u64()]
    } else {
        [crop["width"].as_u64(), crop["height"].as_u64()]
    };
    ensure(
        metadata["mode"] == source.mode
            && metadata["make"] == source.make
            && metadata["model"] == source.model
            && metadata["exif_orientation"] == source.orientation
            && crop_dimensions == source.source_dimensions.map(|side| Some(u64::from(side)))
            && metadata["backend"]
                .as_str()
                .is_some_and(|s| s.contains("LibRaw")),
        format!("RAW source metadata differs from manifest: {metadata}"),
    )?;
    if camera_profile["dng"].is_object() {
        let rect =
            |name: &str| ["x", "y", "width", "height"].map(|field| metadata[name][field].as_u64());
        ensure(
            [
                metadata["sensor_width"].as_u64(),
                metadata["sensor_height"].as_u64(),
            ] == source
                .sensor_dimensions
                .unwrap()
                .map(|side| Some(u64::from(side)))
                && rect("active_area")
                    == source
                        .active_area
                        .unwrap()
                        .map(|side| Some(u64::from(side)))
                && rect("default_crop")
                    == source
                        .default_crop
                        .unwrap()
                        .map(|side| Some(u64::from(side))),
            "DJI sensor, active area or default crop differs from manifest",
        )?;
        let correction = &metadata["dng_corrections"];
        let applied = correction["applied"]
            .as_array()
            .ok_or("DNG correction provenance missing")?;
        let required = camera_profile["dng"]["required_opcodes"]
            .as_array()
            .ok_or("DNG profile required opcodes missing")?;
        ensure(
            correction["interpretation"] == camera_profile["dng"]["interpretation"]
                && applied.len() == required.len()
                && applied.iter().zip(required).all(|(opcode, expected)| {
                    opcode["id"] == expected["id"]
                        && opcode["list"] == expected["list"]
                        && opcode["version"] == expected["version"]
                        && opcode["flags"] == expected["flags"]
                        && opcode["flags"] == 0
                        && opcode["payload_sha256"].as_str().is_some_and(|hash| {
                            hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                        })
                })
                && correction["skipped_optional"].is_array(),
            "DNG required correction order or provenance differs",
        )?;
        let calibration = &correction["calibration"];
        ensure(
            calibration["illuminants"] == camera_profile["dng"]["illuminants"]
                && calibration["selected"] == camera_profile["dng"]["calibration_identity"]
                && ["color_matrix1_sha256", "color_matrix2_sha256"]
                    .into_iter()
                    .all(|field| {
                        calibration[field].as_str().is_some_and(|hash| {
                            hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                        })
                    }),
            "DNG camera calibration provenance differs",
        )?;
    } else {
        ensure(
            metadata["dng_corrections"].is_null(),
            "Non-DNG RAW unexpectedly has correction metadata",
        )?;
    }
    let state = service.state(&asset.id)?;
    let history = service.history(&asset.id, None, 32)?;
    let original = history
        .entries
        .iter()
        .find(|entry| entry.sequence == 0)
        .ok_or("RAW Original history entry missing")?;
    Ok((
        original.id.as_str().to_owned(),
        state.current_entry.id.as_str().to_owned(),
        state.revision,
    ))
}

fn verify_journey(
    evidence: &Path,
    source: &Source,
    original: &str,
    current: &str,
) -> Result<Value> {
    let (app, events, pixels) = verify_frames(evidence, JOURNEY_STEPS + 1)?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    let recorded = app["script"].as_array().ok_or("Missing RAW script")?;
    let expected = script(source);
    ensure(
        recorded.len() == JOURNEY_STEPS,
        "RAW journey step count changed",
    )?;
    for (index, step) in recorded.iter().enumerate() {
        ensure(
            step["status"] == "sent"
                && step["request"] == expected[index]
                && step["frame"] == frames[index + 1]["file"],
            format!("RAW journey step {} was not captured", index + 1),
        )?;
    }
    let initial = &frames[0]["state"];
    ensure(
        initial["source_dimensions"] == json!(source.source_dimensions)
            && initial["orientation"] == source.orientation
            && initial["stack"]["revision"] == 0
            && displayed_entry(&frames[0])? == original,
        "RAW Original dimensions/orientation/identity mismatch",
    )?;
    for (index, revision) in (1..=9).enumerate() {
        ensure(
            frames[index + 1]["state"]["stack"]["revision"] == revision,
            format!("RAW mutation {} did not commit", index + 1),
        )?;
    }
    for (index, frame) in frames.iter().enumerate() {
        if index == HISTORICAL_FRAME {
            ensure(
                frame["state"]["stack"]["displayed"]["layers"]
                    == initial["stack"]["displayed"]["layers"],
                "Historical preview did not display Original RAW recipe",
            )?;
        } else {
            ensure(
                displayed_entry(frame)? == frame["state"]["stack"]["entry"]
                    && frame["state"]["stack"]["displayed"]["layers"]
                        == frame["state"]["stack"]["layers"],
                format!("Frame {index} displayed recipe differs from current recipe"),
            )?;
        }
    }
    ensure(
        frames[HISTORICAL_FRAME]["state"]["selection"]["entry"] == original
            && displayed_entry(&frames[HISTORICAL_FRAME])? == original
            && frames[HISTORICAL_FRAME]["state"]["stack"]["entry"] == current,
        "Historical RAW preview did not display Original without changing current state",
    )?;
    for frame in &frames[CURRENT_FRAME..=FINAL_FRAME] {
        ensure(
            frame["state"]["selection"] == "current" && displayed_entry(frame)? == current,
            "RAW preview did not return to committed current entry",
        )?;
    }
    let original_to_exposure = pixel_difference(&pixels[0], &pixels[1])?;
    let exposure_to_red = pixel_difference(&pixels[1], &pixels[2])?;
    let red_to_blue = pixel_difference(&pixels[2], &pixels[3])?;
    let blue_to_temperature = pixel_difference(&pixels[3], &pixels[4])?;
    let temperature_to_tint = pixel_difference(&pixels[4], &pixels[5])?;
    let tint_to_picker = pixel_difference(&pixels[5], &pixels[6])?;
    let original_history = pixel_difference(&pixels[0], &pixels[HISTORICAL_FRAME])?;
    let step_times = script_stage_times(&events)?;
    for (name, diff) in [
        ("exposure", &original_to_exposure),
        ("red WB", &exposure_to_red),
        ("blue WB", &red_to_blue),
        ("temperature", &blue_to_temperature),
        ("neutral picker", &tint_to_picker),
    ] {
        ensure(
            diff["changed_samples"].as_u64().unwrap_or(0) >= 8,
            format!("{name} did not change sampled photo pixels"),
        )?;
    }
    ensure(
        temperature_to_tint["any_changed_samples"]
            .as_u64()
            .unwrap_or(0)
            >= 8
            && temperature_to_tint["mean_absolute_channel_difference"]
                .as_f64()
                .unwrap_or(0.0)
                > 0.1,
        "tint did not change sampled photo pixels",
    )?;
    ensure(
        original_history["any_changed_samples"]
            .as_u64()
            .unwrap_or(u64::MAX)
            <= 3,
        "Historical Original pixels differ from initial Original",
    )?;
    Ok(json!({
        "status":"passed",
        "run_id":app["run_id"],
        "original_entry":original,
        "current_entry":current,
        "initial_events_ms":event_times(&events),
        "step_timings_ms":step_times,
        "pixel_checks":{"original_to_exposure":original_to_exposure,"exposure_to_red_wb":exposure_to_red,"red_to_blue_wb":red_to_blue,"blue_to_temperature":blue_to_temperature,"temperature_to_tint":temperature_to_tint,"tint_to_picker":tint_to_picker,"original_to_historical_original":original_history},
        "capture_files":frames.iter().map(|frame|frame["file"].clone()).collect::<Vec<_>>(),
    }))
}

fn one_trial(
    root: &Path,
    binary: &Path,
    out: &Path,
    source: &Source,
    path: &Path,
    camera_profile: &Value,
) -> Result<Value> {
    fs::create_dir_all(out)?;
    let catalog = out.join("catalog.sqlite");
    let script_path = out.join("script.json");
    write_json(&script_path, &script(source))?;
    let first = out.join("first");
    let first_run = run_app(root, binary, &first, &catalog, path, Some(&script_path))?;
    let (original, current, revision) = verify_catalog(&catalog, source, path, camera_profile)?;
    ensure(revision == 9, "RAW history did not retain nine mutations")?;
    let journey = verify_journey(&first, source, &original, &current)?;
    let reopened = out.join("reopened");
    let reopened_run = run_app(root, binary, &reopened, &catalog, path, None)?;
    let (reopened_original, reopened_current, reopened_revision) =
        verify_catalog(&catalog, source, path, camera_profile)?;
    ensure(
        reopened_original == original
            && reopened_current == current
            && reopened_revision == revision,
        "RAW catalog reopen changed history identity",
    )?;
    let (reopened_app, reopened_events, reopened_pixels) = verify_frames(&reopened, 1)?;
    let reopened_frame = &reopened_app["frames"][0];
    ensure(
        displayed_entry(reopened_frame)? == current
            && reopened_frame["state"]["stack"]["revision"] == revision,
        "RAW reopen displayed a stale entry",
    )?;
    let first_app = read_json(&first.join("result.json"))?;
    let last_file = smoke::frame_identity(&first, &first_app, &first_app["frames"][FINAL_FRAME])?;
    let final_pixels = photo_samples(&last_file, &first_app["frames"][FINAL_FRAME])?;
    let reopened_difference = pixel_difference(&final_pixels, &reopened_pixels[0])?;
    ensure(
        reopened_difference["any_changed_samples"]
            .as_u64()
            .unwrap_or(u64::MAX)
            <= 3,
        "RAW reopened photo differs from committed current photo",
    )?;
    ensure(
        hash(path)?.eq_ignore_ascii_case(&source.sha256),
        "RAW original changed during editor run",
    )?;
    Ok(json!({
        "source_id":source.id,
        "source_sha256":source.sha256,
        "mode":source.mode,
        "first_process":first_run,
        "journey":journey,
        "reopened_process":reopened_run,
        "reopened_events_ms":event_times(&reopened_events),
        "reopened_pixel_difference":reopened_difference,
        "status":"passed",
    }))
}

fn distribution(samples: &[f64]) -> Value {
    if samples.is_empty() {
        return Value::Null;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let nearest = |p: usize| sorted[(sorted.len() * p).div_ceil(100).saturating_sub(1)];
    json!({"n":samples.len(),"first":samples[0],"p50":nearest(50),"p95":nearest(95),"max":sorted[sorted.len()-1]})
}

fn source_statistics(runs: &[Value]) -> Value {
    let field = |path: &[&str]| {
        let values: Vec<_> = runs
            .iter()
            .filter_map(|run| path.iter().fold(run, |value, key| &value[*key]).as_f64())
            .collect();
        distribution(&values)
    };
    let step = |index: usize, field_name: &str| {
        let values: Vec<_> = runs
            .iter()
            .filter_map(|run| run["journey"]["step_timings_ms"][index][field_name].as_f64())
            .collect();
        distribution(&values)
    };
    json!({
        "first_open_to_raster_ms":field(&["journey","initial_events_ms","open_to_raster_ms"]),
        "first_process_sampled_peak_rss_mib":field(&["first_process","sampled_peak_rss_mib"]),
        "reopened_process_sampled_peak_rss_mib":field(&["reopened_process","sampled_peak_rss_mib"]),
        "reopened_open_to_raster_ms":field(&["reopened_events_ms","open_to_raster_ms"]),
        "exposure_request_to_display_ms":step(0,"request_to_display_ms"),
        "red_wb_request_to_display_ms":step(1,"request_to_display_ms"),
        "blue_wb_request_to_display_ms":step(2,"request_to_display_ms"),
        "temperature_request_to_display_ms":step(3,"request_to_display_ms"),
        "tint_request_to_display_ms":step(4,"request_to_display_ms"),
        "neutral_picker_request_to_display_ms":step(5,"request_to_display_ms"),
        "rotate_request_to_display_ms":step(6,"request_to_display_ms"),
        "crop_request_to_display_ms":step(7,"request_to_display_ms"),
        "undo_request_to_display_ms":step(8,"request_to_display_ms"),
        "historical_request_to_capture_ms":step(9,"request_to_capture_ms"),
        "return_current_request_to_capture_ms":step(10,"request_to_capture_ms"),
        "zoom_100_request_to_capture_ms":step(11,"request_to_capture_ms"),
    })
}

pub fn run(root: &Path, manifest_path: &Path, out: &Path, binary: &Path, samples: usize) -> Result {
    ensure(
        cfg!(target_os = "macos"),
        "RAW editor RSS harness requires native macOS ps",
    )?;
    ensure(
        (1..=100).contains(&samples),
        "RAW editor samples must be 1..=100",
    )?;
    ensure(!out.exists(), "RAW editor output must be new")?;
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    let catalog_value = read_json(&root.join("crates/lightwell-raw/data/cameras.json"))?;
    let profiles: Vec<Value> = manifest
        .sources
        .iter()
        .map(|source| profile(&catalog_value, source).cloned())
        .collect::<Result<_>>()?;
    let sources = validate_manifest(manifest_path, &manifest, &catalog_value)?;
    ensure(
        binary.is_file(),
        format!("RAW editor binary missing: {}", binary.display()),
    )?;
    fs::create_dir_all(out)?;
    fs::copy(manifest_path, out.join("manifest.json"))?;
    let snapshot = out.join("lightwell-binary-snapshot");
    fs::copy(binary, &snapshot)?;
    let binary_hash = hash(&snapshot)?;
    let mut report = json!({
        "status":"in_progress",
        "manifest_sha256":hash(manifest_path)?,
        "binary_sha256":binary_hash,
        "lock_sha256":hash(&root.join("Cargo.lock"))?,
        "host":host(root)?,
        "launch_mode":launch::MODE,
        "samples_per_source":samples,
        "method":"Each trial launches the editor twice with one isolated persistent catalog: a scripted RAW edit journey and a reopen. Native macOS release process RSS is sampled about every 50 ms. Filesystem cache is not purged, so first launch is app-cold only. GPU memory is not isolated from process RSS and is not measured separately. Sampled photo-pixel differences prove reevaluation, not color accuracy or camera-JPEG matching.",
        "runs":[],
    });
    let checked = (|| -> Result {
        for ((source, path), camera_profile) in manifest.sources.iter().zip(sources).zip(&profiles)
        {
            for index in 0..samples {
                let trial = out.join(format!("{}-{index:02}", source.id));
                let row = one_trial(root, &snapshot, &trial, source, &path, camera_profile)?;
                report["runs"]
                    .as_array_mut()
                    .ok_or("Missing run array")?
                    .push(row);
                write_json(&out.join("result.json"), &report)?;
            }
        }
        let runs = report["runs"].as_array().ok_or("Missing run array")?;
        let mut by_source = serde_json::Map::new();
        for source in &manifest.sources {
            let selected: Vec<_> = runs
                .iter()
                .filter(|run| run["source_id"] == source.id)
                .cloned()
                .collect();
            ensure(
                selected.len() == samples,
                format!("RAW source {} sample count differs", source.id),
            )?;
            by_source.insert(source.id.clone(), source_statistics(&selected));
        }
        report["statistics_by_source"] = Value::Object(by_source);
        report["status"] = json!("passed");
        Ok(())
    })();
    if let Err(error) = &checked {
        report["status"] = json!("failed");
        report["error"] = json!(error.to_string());
    }
    write_json(&out.join("result.json"), &report)?;
    checked
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_modes_require_matching_camera_identity() {
        let catalog: Value =
            serde_json::from_str(include_str!("../../crates/lightwell-raw/data/cameras.json"))
                .unwrap();
        let source = Source {
            id: "z6".into(),
            path: PathBuf::from("z6.nef"),
            sha256: "0".repeat(64),
            mode: "NikonZ6Lossless14".into(),
            make: "Nikon".into(),
            model: "Z 6".into(),
            sensor_dimensions: None,
            active_area: None,
            default_crop: None,
            source_dimensions: [1, 1],
            orientation: 1,
            neutral_point: [0, 0],
        };
        assert!(profile(&catalog, &source).is_ok());
        let mut wrong = source;
        wrong.model = "other".into();
        assert!(profile(&catalog, &wrong).is_err());
    }
    #[test]
    fn statistics_use_nearest_rank() {
        let values = [4.0, 1.0, 2.0, 3.0];
        let result = distribution(&values);
        assert_eq!(result["p50"], 2.0);
        assert_eq!(result["p95"], 4.0);
    }
}
