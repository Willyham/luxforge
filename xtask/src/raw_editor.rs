//! The `raw-editor` smoke scenario: the authentic RAW editing journey over one supplied file, then
//! a reopen. The edit launch sets Basic's exposure, the RAW channel gains, a custom temperature and
//! tint, picks the sensor neutral at the source's manifest point, rotates, crops and undoes the
//! crop, previews Original from history and returns to current twice — the second pair is the
//! hold-`\` compare once both developments have been made — then views 100% and Fit. The
//! reopen launch opens the same file through the edit launch's catalog and must show the committed
//! current entry and the same picture.
//!
//! Every frame of both launches passes the plan's universal checks. The scenario's own are what the
//! RAW manifest says about the source — its upright dimensions, orientation, camera and mode, and,
//! where its camera profile declares DNG corrections, the sensor geometry and correction provenance
//! the import recorded — and, over every frame, a ready render of the displayed entry whose RAW
//! controls show the layer they mirror; the displayed recipe against history; and that each edit
//! changes the photograph while the historical Original and the reopen show the same picture. The
//! request-to-display times of one run are recorded as observations, not measured as a timing.
//!
//! No RAW photograph is checked in (see `fixtures/README.md`), so the row is outside the rendered
//! tier: it takes the file from `--source` and the manifest that lists it, by its SHA-256, from
//! `--manifest`. The run keeps a copy of the manifest, which a replay reads again.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance},
    smoke::{self, Scenario},
    *,
};
use luxforge_core::{EditorService, SourceKind};
use luxforge_evidence::{self as script, PreviewStep, ViewStep};
use raw::{EditorSource, Entry};
use std::time::Duration;

pub const SCENARIO: &str = "raw-editor";
/// The two launches: the scripted edit, and the reopen through its catalog.
pub const EDIT: &str = "edit";
pub const REOPEN: &str = "reopen";
/// Each launch's process deadline: large RAWs and required DNG corrections redevelop the mosaic on
/// several of the journey's steps.
pub const DEADLINE: Duration = Duration::from_secs(70);
pub const NOTE: &str = "The edit launch runs the journey over the supplied RAW file with its own catalog; the reopen launch opens the same file through that catalog (`edit/catalog.sqlite`). `manifest.json` is the RAW manifest the run was given, which lists the file by its SHA-256.";

/// The RAW development's effect, whose layer the displayed controls mirror.
const RAW_EFFECT: &str = "luxforge.raw";

/// The steps the checks read by name.
mod names {
    pub const OPENED: &str = "opened";
    pub const EXPOSURE: &str = "exposure";
    pub const RED_GAIN: &str = "red-gain";
    pub const BLUE_GAIN: &str = "blue-gain";
    pub const TEMPERATURE: &str = "temperature";
    pub const TINT: &str = "tint";
    pub const NEUTRAL: &str = "neutral-pick";
    pub const ROTATE: &str = "rotate";
    pub const CROP: &str = "crop";
    pub const UNDO: &str = "undo";
    pub const HISTORICAL: &str = "historical-original";
    pub const CURRENT: &str = "current";
    pub const HISTORICAL_AGAIN: &str = "historical-original-again";
    pub const CURRENT_AGAIN: &str = "current-again";
    pub const ZOOM: &str = "zoom-100";
    pub const FIT: &str = "fit";
    pub const REOPENED: &str = "reopened";
}
use names::*;

/// The steps that commit, each of which must present a render before the next step starts.
const MUTATIONS: [&str; 9] = [
    EXPOSURE,
    RED_GAIN,
    BLUE_GAIN,
    TEMPERATURE,
    TINT,
    NEUTRAL,
    ROTATE,
    CROP,
    UNDO,
];

/// The edit launch's plan over a source whose neutral point is `[x, y]`.
fn journey([x, y]: [u32; 2], monochrome: bool, [width, height]: [u32; 2]) -> Plan {
    let crop_aspect = if height * 3 == width * 4 {
        "1:1"
    } else {
        "4:3"
    };
    // Each call commits one entry with its own label.
    let call = |name: &str, method: &str, params: Value, label: &str| {
        let step = Step::new(name, script::Step::call(method, params));
        if monochrome && [RED_GAIN, BLUE_GAIN, TEMPERATURE, TINT, NEUTRAL].contains(&name) {
            step.commits(0).refused("validation")
        } else {
            step.commits(1).label(label)
        }
    };
    Plan::new(vec![
        // The opened entry is the Original, or the import's first-open Lens entry; verify checks which.
        Step::opened(OPENED),
        call(
            EXPOSURE,
            "edit.set-basic",
            json!({"exposure":1.0}),
            "Exposure +1.00 EV",
        ),
        call(
            RED_GAIN,
            "edit.set-raw-red-gain",
            json!({"gain":3.0}),
            "Red gain",
        ),
        call(
            BLUE_GAIN,
            "edit.set-raw-blue-gain",
            json!({"gain":0.9}),
            "Blue gain",
        ),
        call(
            TEMPERATURE,
            "edit.set-raw",
            json!({"temperature":5500.0}),
            "Temperature 5500 K",
        ),
        call(TINT, "edit.set-raw", json!({"tint":10.0}), "Tint +10"),
        call(
            NEUTRAL,
            "edit.pick-raw-neutral",
            json!({"x":x,"y":y}),
            "White balance",
        ),
        call(
            ROTATE,
            "edit.transform",
            json!({"transform":"rotate-right"}),
            "Rotate right",
        ),
        call(
            CROP,
            "edit.crop-fit",
            json!({"aspect":crop_aspect,"angle":0.0}),
            &format!("Crop {crop_aspect}"),
        ),
        // Undo is one more revision, back at the rotation's entry.
        Step::new(UNDO, script::Step::api("history.undo"))
            .commits(1)
            .label("Rotate right"),
        Step::new(HISTORICAL, PreviewStep::Sequence(0)).commits(0),
        Step::new(CURRENT, PreviewStep::Current).commits(0),
        // The same pair again, once both entries' developments have been made.
        Step::new(HISTORICAL_AGAIN, PreviewStep::Sequence(0)).commits(0),
        Step::new(CURRENT_AGAIN, PreviewStep::Current).commits(0),
        Step::new(ZOOM, ViewStep::Percent(100.0))
            .commits(0)
            .percent(100.0),
        Step::new(FIT, ViewStep::Fit).commits(0).fit(),
    ])
}

/// The neutral point the table's plan names, for `smoke --list`, the plan checks and the script
/// dumps: a run plans over its own source's manifest point instead.
const LISTED_POINT: [u32; 2] = [123, 456];

/// The edit launch's plan as the table lists it.
pub fn edit_plan(_: &[PathBuf]) -> Plan {
    journey(LISTED_POINT, false, [6000, 4000])
}

/// The reopen launch: the open alone, at the undo's committed entry.
pub fn reopen_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![Step::opened(REOPENED).label("Rotate right")])
}

/// The row's own run: the table's two launches, the edit launch planned over its source's own
/// manifest point.
pub fn run(run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    smoke::launch_planned(run, scenario, sources, |run, sources| {
        let (listed, _) = listed(run)?;
        Ok(vec![
            journey(
                listed.neutral_point,
                listed.mode.is_monochrome(),
                listed.source_dimensions,
            ),
            reopen_plan(sources),
        ])
    })
}

/// The run's one source and its entry in the run's copy of the manifest: the entry with the
/// source's own SHA-256, as the run hashed it.
fn listed(run: &Run) -> Result<(EditorSource, PathBuf)> {
    let [source] = run.sources() else {
        return Err("The raw-editor scenario opens one RAW source".into());
    };
    let hashes = run
        .recorded_value("fixture_hashes")
        .as_object()
        .ok_or("The run hashed no source")?;
    let sha256 = hashes
        .values()
        .next()
        .and_then(Value::as_str)
        .ok_or("The run hashed no source")?;
    let manifest = raw::manifest::<EditorSource>(&run.out().join(smoke::MANIFEST))?;
    let entry = manifest
        .sources
        .into_iter()
        .find(|entry| entry.sha256() == sha256)
        .ok_or_else(|| {
            format!(
                "The manifest lists no source with {}'s SHA-256 {sha256}",
                source.display()
            )
        })?;
    Ok((entry, source.clone()))
}

/// What the catalog records once both launches have run.
struct Catalogued {
    original: String,
    current: String,
    revision: u64,
    /// The import's first-open Lens entry and its label: a RAW whose detected profile applies is
    /// imported with it right after its Original.
    first_open: Option<(String, String)>,
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The catalog holds the one source, at its own path, imported as the manifest says: its upright
/// dimensions, orientation, camera and mode, decoded by LibRaw; where its camera profile declares
/// DNG corrections, the manifest's sensor geometry and a correction provenance for every opcode
/// applied, and otherwise none. Returns its Original entry, current entry and revision.
fn catalog(path: &Path, entry: &EditorSource, source: &Path) -> Result<Catalogued> {
    let service = EditorService::open(path)?;
    // Two identities say whether a second photograph exists; the one photograph's whole record,
    // with its RAW interpretation, is read by itself.
    let assets = service.asset_ids(2)?;
    ensure(
        assets.len() == 1,
        "RAW run developed an unexpected photograph count",
    )?;
    let listed = service.state(&assets[0])?;
    let asset = &listed.asset;
    ensure(
        asset.locator.canonicalize()? == source.canonicalize()?,
        "RAW catalog locator changed",
    )?;
    ensure(
        [asset.width, asset.height] == entry.source_dimensions,
        "RAW catalog source dimensions differ from manifest",
    )?;
    let SourceKind::Raw { metadata } = &asset.source else {
        return Err("RAW source was imported as JPEG".into());
    };
    let crop = &metadata.default_crop;
    let upright = if entry.orientation >= 5 {
        [crop.height, crop.width]
    } else {
        [crop.width, crop.height]
    };
    ensure(
        metadata.mode == entry.mode
            && metadata.make == entry.make
            && metadata.model == entry.model
            && metadata.exif_orientation == entry.orientation
            && upright == entry.source_dimensions
            && metadata.backend.contains("LibRaw"),
        format!(
            "RAW source metadata differs from manifest: {:?} {} {} orientation {} crop {upright:?} by {}",
            metadata.mode,
            metadata.make,
            metadata.model,
            metadata.exif_orientation,
            metadata.backend
        ),
    )?;
    match (entry.dng_geometry(), &metadata.dng_corrections) {
        (Some((sensor, active, crop)), Some(corrections)) if entry.dng() => {
            let rect = |r: &luxforge_raw::RawRect| [r.x, r.y, r.width, r.height];
            ensure(
                [metadata.sensor_width, metadata.sensor_height] == sensor
                    && rect(&metadata.active_area) == active
                    && rect(&metadata.default_crop) == crop,
                "DNG sensor, active area or default crop differs from manifest",
            )?;
            let calibration = &corrections.calibration;
            // Each applied opcode carries provenance; a profile that requires none, such as a
            // lossless DNG without opcode lists, applies none. The required set is the adapter's.
            ensure(
                corrections
                    .applied
                    .iter()
                    .all(|opcode| opcode.flags == 0 && is_sha256(&opcode.payload_sha256))
                    && (entry.mode.is_monochrome()
                        || (is_sha256(&calibration.color_matrix1_sha256)
                            && is_sha256(&calibration.color_matrix2_sha256))),
                "DNG correction or calibration provenance is incomplete",
            )?;
        }
        (_, None) if !entry.dng() => {}
        _ => {
            return Err(format!(
                "The import's DNG corrections disagree with the camera profile for {:?}",
                entry.mode
            )
            .into());
        }
    }
    let state = service.state(&asset.id)?;
    let history = service.history(&asset.id, None, 32)?;
    let original = history
        .entries
        .iter()
        .find(|entry| entry.sequence == 0)
        .ok_or("RAW Original history entry missing")?;
    let first_open = history
        .entries
        .iter()
        .find(|entry| {
            entry.sequence == 1
                && entry.actor == "system"
                && entry.action_id == "select-lens-profile"
        })
        .map(|entry| (entry.id.as_str().to_owned(), entry.label.clone()));
    Ok(Catalogued {
        original: original.id.as_str().to_owned(),
        current: state.current_entry.id.as_str().to_owned(),
        revision: state.revision,
        first_open,
    })
}

fn displayed_entry(frame: &Frame) -> Result<&str> {
    frame.state()["stack"]["displayed"]["entry"]
        .as_str()
        .ok_or_else(|| "Captured frame lacks displayed entry identity".into())
}

/// How far a displayed RAW control may sit from the payload it mirrors: half of the last digit the
/// parameter declares it shows. The control text is the payload rounded to that precision, so the
/// only honest tolerance is the rounding itself, read from the descriptor rather than written here.
fn displayed_tolerance(action: &str, parameter: &str) -> Result<f64> {
    let registry = luxforge_core::ModuleRegistry::builtin();
    let precision = registry
        .descriptors()
        .into_iter()
        .find_map(|module| module.action(action))
        .and_then(|action| action.parameter(parameter))
        .and_then(|parameter| parameter.precision)
        .ok_or_else(|| format!("{action}.{parameter} declares no display precision"))?;
    Ok(0.5 * 10f64.powi(-i32::from(precision)) + 1e-9)
}

/// The frame is a ready render of the entry it displays, and Basic's Exposure, Temperature and Tint
/// show the displayed recipe: exposure from Basic's global layer, the rest from the RAW layer.
fn ready_raw_frame(frame: &Frame, refused: bool) -> Result {
    let state = frame.state();
    // A refused mutation advances the request generation but preserves the previous photo.
    // Its unchanged pixels/entry are checked below; there is no replacement render to await.
    let expected_refusal = refused
        && state["phase"] == "error"
        && state["error_code"] == "validation"
        && state["displayed_generation"].as_u64().is_some_and(|shown| {
            state["requested_generation"]
                .as_u64()
                .is_some_and(|requested| shown < requested)
        });
    ensure(
        (expected_refusal
            || (state["phase"] == "ready"
                && state["displayed_generation"] == state["requested_generation"]))
            && state["render_error"].is_null()
            && (state["error_code"].is_null() || (refused && state["error_code"] == "validation")),
        "not a ready rendered RAW image",
    )?;
    displayed_entry(frame)?;
    let layers = state["stack"]["displayed"]["layers"]
        .as_array()
        .ok_or("Displayed layers missing")?;
    let raw = layers
        .iter()
        .find(|layer| layer["effect"] == RAW_EFFECT)
        .ok_or("Displayed RAW layer missing")?;
    let payload: luxforge_core::RawPayload = serde_json::from_value(raw["payload"].clone())?;
    // Exposure is Basic's on every kind: the global Basic layer's, or its 0 EV default.
    let exposure = layers
        .iter()
        .find(|layer| layer["effect"] == luxforge_core::BASIC_EFFECT && layer["mask"].is_null())
        .and_then(|layer| layer["payload"]["exposure"].as_f64())
        .unwrap_or(0.0);
    // The core's own answer for the displayed development: a custom temperature and tint, or
    // under As shot the temperature and tint whose gains are the camera's as-shot gains, which
    // the forward map must reproduce.
    // At the ±100 tint limit the core also answers a white up to half a tint unit beyond it, held
    // to the limit, so that answer selects gains within that half unit rather than exactly.
    let [kelvin, tint] = payload.white_balance_controls();
    if payload.wb_mode == luxforge_core::WhiteBalanceMode::AsShot
        && tint.abs() < 100.0
        && let Ok(gains) = luxforge_core::gains_from_temperature_tint(kelvin, tint, payload.cam_xyz)
    {
        ensure(
            gains
                .iter()
                .zip(payload.as_shot_gains)
                .all(|(gain, shot)| (gain - shot).abs() <= 1.0e-6 * shot),
            format!(
                "The as-shot equivalent {kelvin} K, {tint} does not reproduce the as-shot gains"
            ),
        )?;
    }
    // Basic's section shows the RAW variants of Temperature and Tint on a RAW photo.
    for (action, parameter, expected) in [
        ("set-basic", "exposure", exposure),
        ("set-raw", "temperature", kelvin),
        ("set-raw", "tint", tint),
    ] {
        let shown: f64 = frame.field(action, parameter)?.parse()?;
        ensure(
            (shown - expected).abs() <= displayed_tolerance(action, parameter)?,
            format!("Displayed {action}.{parameter} control differs from displayed RAW layer"),
        )?;
    }
    Ok(())
}

/// How far a mean channel of the photograph's central window may move and still be the same
/// picture, and how far it must move for an edit to have changed it, in 8-bit codes. The window
/// holds hundreds of thousands of pixels: the same recipe rendered twice reads exactly equal, and
/// the smallest edit here, a tint of +10, moves a channel by about 0.7 on the Z6.
const SAME: f64 = 0.5;
const CHANGED: f64 = 0.25;

/// The largest move of any mean channel of the photograph's central window between two frames.
fn moved(before: &Frame, after: &Frame) -> Result<f64> {
    let (a, b) = (before.window_rgb()?, after.window_rgb()?);
    Ok(a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f64::max))
}

/// Each mutation's request-to-display time, and each other step's request-to-capture time, from
/// the edit launch's log: one functional run's observations. Every mutation must present a render
/// before the next step starts.
fn step_times(edit: &Checked, monochrome: bool) -> Result<Vec<Value>> {
    let starts: Vec<f64> = edit
        .events
        .iter()
        .filter(|event| event["event"] == "script_step")
        .map(|event| {
            event["elapsed_ms"]
                .as_f64()
                .ok_or_else(|| "A script step lacks its elapsed time".into())
        })
        .collect::<Result<_>>()?;
    // The frames after the open, in order, are the script's steps.
    let steps = &edit.names()[1..];
    ensure(
        starts.len() == steps.len(),
        "Script steps and frames differ",
    )?;
    let mut times = Vec::new();
    for (index, (name, began)) in steps.iter().zip(&starts).enumerate() {
        let ended = starts.get(index + 1).copied().unwrap_or(f64::INFINITY);
        let first = |event_name: &str| {
            edit.events
                .iter()
                .filter(|event| {
                    event["elapsed_ms"]
                        .as_f64()
                        .is_some_and(|time| time >= *began && time < ended)
                })
                .find(|event| event["event"] == event_name)
                .and_then(|event| event["elapsed_ms"].as_f64())
        };
        let captured = first("frame_captured")
            .ok_or_else(|| format!("Step {name:?} has no correlated frame capture event"))?;
        let displayed = first("preview_displayed").or_else(|| first("render_ready"));
        ensure(
            !MUTATIONS.contains(&name.as_str())
                || (monochrome
                    && [RED_GAIN, BLUE_GAIN, TEMPERATURE, TINT, NEUTRAL].contains(&name.as_str()))
                || displayed.is_some(),
            format!("Step {name:?} has no correlated display event"),
        )?;
        times.push(json!({
            "step":name,
            "request_to_display_ms":displayed.map(|time| time - began),
            "request_to_capture_ms":captured - began,
        }));
    }
    Ok(times)
}

/// The open's decode, first presented raster and first capture, from a launch's log.
fn open_times(launch: &Checked) -> Value {
    let mut times = serde_json::Map::new();
    for name in ["decoded", "render_ready", "frame_captured"] {
        if let Some(event) = launch.events.iter().find(|event| event["event"] == name) {
            times.insert(name.into(), event["elapsed_ms"].clone());
            if name == "decoded" {
                times.insert(
                    "open_to_raster_ms".into(),
                    event["detail"]["open_to_raster_ms"].clone(),
                );
            }
        }
    }
    Value::Object(times)
}

/// The scenario's own checks over both launches once their plans have held.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let (entry, source) = listed(run)?;
    let monochrome = entry.mode.is_monochrome();
    let [edit, reopen] = launches else {
        return Err(format!("Expected two launches, found {}", launches.len()).into());
    };
    for launch in launches {
        for (name, frame) in launch.names().iter().zip(&launch.frames) {
            let refused = monochrome
                && [RED_GAIN, BLUE_GAIN, TEMPERATURE, TINT, NEUTRAL].contains(&name.as_str());
            ready_raw_frame(frame, refused).map_err(|error| {
                format!("{}: step {name:?}: {error}", launch.evidence.display())
            })?;
        }
    }
    if monochrome {
        for launch in launches {
            for frame in &launch.frames {
                let rgb = frame.window_rgb()?;
                ensure(
                    (rgb[0] - rgb[1]).abs() <= SAME && (rgb[1] - rgb[2]).abs() <= SAME,
                    "Monochrome rendered photograph is not grayscale",
                )?;
                let groups = frame.state()["section_controls"]["luxforge.basic"]
                    .as_array()
                    .ok_or("Basic controls missing")?;
                ensure(
                    groups.iter().any(|group| {
                        group["kind"] == "group"
                            && group["label"] == "White balance"
                            && group["enabled"] == false
                            && group["unavailable"] == "Monochrome original"
                    }),
                    "Monochrome white-balance group is not disabled",
                )?;
            }
        }
    }
    let Catalogued {
        original,
        current,
        revision,
        first_open,
    } = catalog(
        &run.out().join(EDIT).join("catalog.sqlite"),
        &entry,
        &source,
    )?;
    // A RAW whose detected lens profile applies opens on its first-open entry, one revision on.
    let imported = u64::from(first_open.is_some());
    ensure(
        revision == (if monochrome { 4 } else { 9 }) + imported,
        "RAW history did not retain exactly the accepted mutations after import",
    )?;

    let opened = edit.at(OPENED)?;
    let (opened_entry, opened_label) = first_open
        .as_ref()
        .map_or((original.as_str(), "Original"), |(id, label)| {
            (id.as_str(), label.as_str())
        });
    ensure(
        opened.state()["source_dimensions"] == json!(entry.source_dimensions)
            && opened.state()["orientation"] == entry.orientation
            && opened.revision()? == imported
            && displayed_entry(opened)? == opened_entry
            && opened.label()? == opened_label
            && (first_open.is_none() || opened_label.starts_with("Lens profile ")),
        "RAW opened dimensions/orientation/identity mismatch",
    )?;
    // The Original holds the opened recipe without the first-open profile.
    let original_layers: Vec<Value> = opened.state()["stack"]["displayed"]["layers"]
        .as_array()
        .ok_or("The opened frame records no layers")?
        .iter()
        .filter(|layer| layer["effect"] != "luxforge.lens.distortion")
        .cloned()
        .collect();
    let historical = [edit.index(HISTORICAL)?, edit.index(HISTORICAL_AGAIN)?];
    for (index, (name, frame)) in edit.names().iter().zip(&edit.frames).enumerate() {
        let stack = &frame.state()["stack"];
        if historical.contains(&index) {
            ensure(
                stack["displayed"]["layers"] == json!(original_layers),
                "Historical preview did not display Original RAW recipe",
            )?;
        } else {
            ensure(
                displayed_entry(frame)? == stack["entry"]
                    && stack["displayed"]["layers"] == stack["layers"],
                format!("Step {name:?}'s displayed recipe differs from current recipe"),
            )?;
        }
    }
    let preview = edit.at(HISTORICAL)?;
    for frame in [preview, edit.at(HISTORICAL_AGAIN)?] {
        ensure(
            frame.state()["selection"]["entry"] == original.as_str()
                && displayed_entry(frame)? == original
                && frame.entry()? == current,
            "Historical RAW preview did not display Original without changing current state",
        )?;
    }
    for name in [CURRENT, CURRENT_AGAIN, ZOOM, FIT] {
        let frame = edit.at(name)?;
        ensure(
            frame.state()["selection"] == "current" && displayed_entry(frame)? == current,
            format!("Step {name:?} did not return to the committed current entry"),
        )?;
    }
    let reopened = reopen.at(REOPENED)?;
    ensure(
        displayed_entry(reopened)? == current && reopened.revision()? == revision,
        "RAW reopen displayed a stale entry",
    )?;

    let mut checks = Checks::new();
    for (what, before, after) in [
        ("exposure", OPENED, EXPOSURE),
        ("red gain", EXPOSURE, RED_GAIN),
        ("blue gain", RED_GAIN, BLUE_GAIN),
        ("temperature", BLUE_GAIN, TEMPERATURE),
        ("tint", TEMPERATURE, TINT),
        ("neutral pick", TINT, NEUTRAL),
    ] {
        let after = edit.at(after)?;
        checks.compare(
            after,
            &format!("The {what} edit's move of the photograph"),
            moved(edit.at(before)?, after)?,
            0.0,
            if monochrome && what != "exposure" {
                Tolerance::Within(SAME)
            } else {
                Tolerance::Above(CHANGED)
            },
        )?;
    }
    for (what, before, after) in [
        ("historical Original", edit.at(OPENED)?, preview),
        (
            "historical Original again",
            edit.at(OPENED)?,
            edit.at(HISTORICAL_AGAIN)?,
        ),
        ("current again", edit.at(CURRENT)?, edit.at(CURRENT_AGAIN)?),
        ("reopened current", edit.at(FIT)?, reopened),
    ] {
        checks.compare(
            after,
            &format!("The {what}'s move of the photograph"),
            moved(before, after)?,
            0.0,
            Tolerance::Within(SAME),
        )?;
    }

    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "source":entry,
            "original_entry":original,
            "current_entry":current,
            "revision":revision,
            "thresholds":{"changed":CHANGED,"same":SAME},
            "observations":{
                "note":"One functional run's times, not a distribution.",
                "edit_open_ms":open_times(edit),
                "reopen_open_ms":open_times(reopen),
                "steps":step_times(edit,monochrome)?,
            },
            "scope": "The largest move of a mean channel of the central window of the photograph the editor records drawing, read back from the renderer",
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The journey's script is its steps in order, over the source's own neutral point.
    #[test]
    fn the_journey_picks_the_sources_own_neutral_point() {
        let plan = journey([1609, 2419], false, [6000, 4000]);
        assert!(plan.validate().is_ok());
        assert_eq!(plan.len(), 1 + MUTATIONS.len() + 6);
        let script = plan.script();
        assert_eq!(script.as_array().unwrap().len(), MUTATIONS.len() + 6);
        assert_eq!(
            script[5],
            json!({"api":{"method":"edit.pick-raw-neutral","params":{"x":1609,"y":2419}}})
        );
        assert_eq!(plan.index(NEUTRAL), Some(6));
        assert!(reopen_plan(&[]).validate().is_ok());
    }
}
