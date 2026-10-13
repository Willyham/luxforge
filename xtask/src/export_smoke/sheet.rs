//! The `export` scenario's sheet launch: the Export sheet end to end in the real editor, over the
//! generated 24 MP and 60 MP JPEGs and, when `--source` names one, a supplied RAW.
//!
//! The launch opens the 60 MP JPEG, the RAW and the 24 MP JPEG, in that order, so the 24 MP one is
//! displayed. Over it: the title bar's Export menu, with Export again disabled and the three
//! built-in presets; the sheet, Web chosen and Choose… answered with the run's evidence directory
//! (no step ever writes beside an original), exported; then Print and Master the same way. A
//! changed Web is saved as the preset `Mine`, changed again and updated, its plan naming the taken
//! `-web` file and writing `-web-2`; Export again repeats it as `-web-3`; and `Mine` is deleted
//! from its card's menu. The RAW, when supplied, is opened and exported with Web, Print and Master;
//! without one those frames are recorded as pending. The 60 MP JPEG is opened: an optimised
//! full-size WebP is refused by the plan's `webp-too-large` notice, and the optimised JPEG warns
//! with `large-image`; it is exported with the Performance section closed, its progress bar shown
//! in the status bar from the start and rising, pressed to open the section, and cancelled from the
//! section's row, leaving no file. Last, every photograph is selected in Select and Export N…
//! opens the sheet in batch mode: the same optimised WebP leaves the 60 MP photograph out, which
//! the sheet says before the batch runs and its report names after.
//!
//! Every written file is decoded here and must be the plan's output, the result's length and
//! format and the structure its settings ask for; each request the sheet sent is the sheet's own
//! settings, as an agent writes them (the window's screen density standing in for none); the plan
//! is asked only for the settings it depends on, at most one at a time with the newest pending;
//! and no sheet gesture asked the owner for an asset's state, its history, a preview or an upload.
use super::{decode_api_jpeg, decode_api_tiff, decode_api_webp};
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, plan::only},
    smoke::{self, APP, LaunchSpec, PANELLED, Scenario, Source},
    *,
};
use luxforge_evidence::{
    self as script, CatalogStep, ExportStep, ExportWrite, SelectStep, SelectWorkspace, SheetEdit,
    SheetFolder,
};

/// The photograph most of the journey edits, and the large one the warning, the refusal and the
/// cancel are about.
pub const PHOTO: &str = "fixtures/generated/24mp.jpg";
pub const LARGE: &str = "fixtures/generated/60mp.jpg";
/// The preset the journey makes, changes and deletes.
const MINE: &str = "Mine";
/// What the batch adds to each original's name.
const BATCH_SUFFIX: &str = "-batch";
/// How long the frame after the export starts waits, so its progress can rise.
const PROGRESS_WAIT_MS: u64 = 700;
/// How long the frame after the cancel waits for the job to end.
const CANCEL_WAIT_MS: u64 = 1500;
/// Methods no sheet gesture may ask the owner for: an asset's state, its history, a preview, a
/// render or an upload.
const UNASKED: [&str; 5] = ["asset.state", "history.", "preview.", "render.", "upload"];

/// The sheet launch's row: its one launch, and its checks.
static SHEET: Scenario = Scenario {
    name: "export",
    about: "The Export sheet's journey",
    launches: &[LaunchSpec {
        // Three photographs opened, twelve exports written and a batch of optimised WebPs.
        deadline: Some(std::time::Duration::from_secs(280)),
        ..APP
    }],
    verify,
    source: Source::Fixtures(&[]),
    window: Some(PANELLED),
    note: None,
    own: None,
};

/// Launch the sheet's journey in `run`, over the generated JPEGs and `raw` when one is supplied.
pub(super) fn run(run: Run, raw: Option<PathBuf>) -> Result {
    let root = run.root().to_owned();
    let mut sources = vec![root.join(LARGE)];
    sources.extend(raw);
    sources.push(root.join(PHOTO));
    smoke::launch_planned(run, &SHEET, sources, |_, sources| Ok(vec![plan(sources)]))
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn edit(name: &str, edit: SheetEdit) -> Step {
    Step::new(name, script::Step::export_edit(edit)).commits(0)
}

fn evidence_folder() -> SheetEdit {
    SheetEdit {
        folder: Some(SheetFolder::Evidence),
        ..SheetEdit::default()
    }
}

fn export(name: &str, step: ExportStep) -> Step {
    Step::new(name, step).commits(0)
}

/// Open the sheet, press `preset`, save to the evidence directory and export: four frames named
/// `{prefix}-sheet`, `{prefix}`, `{prefix}-folder` and `{prefix}-written`.
fn with_preset(prefix: &str, preset: &str) -> Vec<Step> {
    let sheet = format!("{prefix}-sheet");
    vec![
        // The sheet opens once the photograph has settled: a RAW just opened takes its initial
        // development as an entry of its own by then, so its sheet step counts no commits.
        if prefix == "raw-web" {
            Step::new(sheet, ExportStep::Sheet(Default::default()))
        } else {
            export(&sheet, ExportStep::Sheet(Default::default()))
        },
        export(prefix, ExportStep::Preset(preset.into())),
        edit(&format!("{prefix}-folder"), evidence_folder()),
        export(
            &format!("{prefix}-written"),
            ExportStep::Write(ExportWrite::default()),
        )
        .status_starts("Exported "),
    ]
}

/// The journey, over the sources as the run opens them: the 60 MP JPEG, the RAW when supplied,
/// then the 24 MP JPEG.
fn plan(sources: &[PathBuf]) -> Plan {
    let large = &sources[0];
    let raw = (sources.len() == 3).then(|| &sources[1]);
    let open = |name: &str, path: &Path| {
        Step::new(
            name,
            script::Step::Open {
                path: path.display().to_string(),
            },
        )
        .commits(0)
    };
    let mut steps: Vec<Step> = sources
        .iter()
        .map(|source| Step::opened(format!("opened-{}", stem(source))))
        .collect();
    steps.extend([
        Step::new("menu", script::Step::export_menu()).commits(0),
        export("sheet", ExportStep::Sheet(Default::default())),
        edit("web-folder", evidence_folder()),
        export("web-written", ExportStep::Write(ExportWrite::default())).status_starts("Exported "),
    ]);
    steps.extend(with_preset("print", "Print"));
    steps.extend(with_preset("master", "Master"));
    steps.extend([
        // A changed Web saved as Mine, changed again and updated, exported to the taken name's
        // next, repeated by Export again, and deleted.
        export("own-sheet", ExportStep::Sheet(Default::default())),
        export("own-web", ExportStep::Preset("Web".into())),
        edit("own-folder", evidence_folder()),
        edit(
            "own-quality",
            SheetEdit {
                quality: Some(70),
                ..SheetEdit::default()
            },
        ),
        export("saved", ExportStep::SavePreset(MINE.into())),
        edit(
            "mine-changed",
            SheetEdit {
                quality: Some(60),
                ..SheetEdit::default()
            },
        ),
        export("updated", ExportStep::UpdatePreset),
        export("mine-written", ExportStep::Write(ExportWrite::default()))
            .status_starts("Exported "),
        export("again", ExportStep::Again(ExportWrite::default())).status_starts("Exported "),
        export("delete-sheet", ExportStep::Sheet(Default::default())),
        export("deleted", ExportStep::DeletePreset(MINE.into())),
        export("closed", ExportStep::Close),
    ]);
    if let Some(raw) = raw {
        steps.push(open("raw-open", raw));
        steps.extend(with_preset("raw-web", "Web"));
        steps.extend(with_preset("raw-print", "Print"));
        steps.extend(with_preset("raw-master", "Master"));
    }
    steps.extend([
        open("large-open", large),
        // Captured once the photograph has settled, as the RAW's is.
        Step::new("large-sheet", ExportStep::Sheet(Default::default())),
        export("large-web", ExportStep::Preset("Web".into())),
        edit("large-folder", evidence_folder()),
        // Three changes in one step: the plan coalesces them.
        edit(
            "webp",
            SheetEdit {
                format: Some("webp".into()),
                size: Some("full".into()),
                optimise: Some(true),
                ..SheetEdit::default()
            },
        ),
        edit(
            "large-warned",
            SheetEdit {
                format: Some("jpeg".into()),
                ..SheetEdit::default()
            },
        ),
        Step::new("collapsed", script::Step::performance(false)).commits(0),
        export(
            "exporting",
            ExportStep::Write(ExportWrite { background: true }),
        ),
        Step::new("progressing", script::Step::wait(PROGRESS_WAIT_MS)).commits(0),
        export("performance", ExportStep::Bar),
        Step::new("cancelled", script::Step::PerformanceCancel { row: 0 }).commits(0),
        Step::new("cancel-ended", script::Step::wait(CANCEL_WAIT_MS))
            .commits(0)
            .status("Export cancelled"),
    ]);
    // The batch: every photograph selected in Select, an optimised WebP the 60 MP one cannot be.
    let last = u32::try_from(sources.len() - 1).unwrap_or_default();
    steps.extend([
        Step::new("select", SelectStep::Switch(SelectWorkspace::Select)).commits(0),
        Step::new("all", SelectStep::Source("All photographs".into())).commits(0),
        Step::new(
            "first",
            SelectStep::Click {
                position: 0,
                shift: false,
                command: false,
            },
        )
        .commits(0),
        Step::new(
            "every",
            SelectStep::Click {
                position: last,
                shift: true,
                command: false,
            },
        )
        .commits(0),
        Step::new("batch-sheet", script::Step::Catalog(CatalogStep::Export)).commits(0),
        edit(
            "batch-edit",
            SheetEdit {
                format: Some("webp".into()),
                size: Some("full".into()),
                optimise: Some(true),
                folder: Some(SheetFolder::Evidence),
                suffix: Some(BATCH_SUFFIX.into()),
                ..SheetEdit::default()
            },
        ),
        export("batch", ExportStep::Write(ExportWrite::default())).status_starts("Exported "),
        Step::new("batch-report", script::Step::Catalog(CatalogStep::Report)).commits(0),
    ]);
    Plan::new(steps)
}

fn sheet(frame: &Frame) -> &Value {
    &frame.state()["export"]["sheet"]
}

fn model(frame: &Frame) -> &Value {
    &sheet(frame)["model"]
}

fn plans_sent(frame: &Frame) -> u64 {
    sheet(frame)["plans_sent"].as_u64().unwrap_or_default()
}

/// The owner methods asked for between two frames that no sheet gesture may ask for.
fn unasked(before: &Frame, after: &Frame) -> Vec<String> {
    let (a, b) = (
        &before.state()["export"]["owner_calls"],
        &after.state()["export"]["owner_calls"],
    );
    b.as_object()
        .into_iter()
        .flatten()
        .filter(|(method, count)| {
            UNASKED.iter().any(|prefix| method.starts_with(prefix)) && a[method.as_str()] != **count
        })
        .map(|(method, _)| method.clone())
        .collect()
}

/// An export step's frame: its job ended ready, the request was the sheet's settings in `before`
/// as an agent sends them, and the file decodes to the plan's output with its format's structure.
fn written(
    launch: &Checked,
    before: &str,
    step: &str,
    checks: &mut Checks,
) -> Result<(Value, Value)> {
    let frame = launch.at(step)?;
    let export = &frame["step"]["export"];
    ensure(
        export["record"]["status"] == "ready",
        format!("The {step} export did not end ready: {export}"),
    )?;
    let queued = &export["queued"];
    let result = &export["record"]["result"];
    let mut expected = launch.at(before)?["state"]["export"]["sheet"]["settings"].clone();
    if expected["pixels_per_inch"].is_null() && cfg!(target_os = "macos") {
        // The window's own density stands in for none, as the screen shows the file.
        let scale = frame["scale"].as_f64().unwrap_or(1.0);
        expected["pixels_per_inch"] = json!((72.0 * scale).round() as u64);
    }
    ensure(
        queued["settings"] == expected,
        format!(
            "The {step} export sent {}, the sheet held {expected}",
            queued["settings"]
        ),
    )?;
    // Written into the launch's evidence directory, the folder the sheet was given: read there,
    // which a replay's copy holds too.
    let file = Path::new(result["path"].as_str().unwrap_or_default())
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = launch.evidence.join(&file);
    ensure(
        path.is_file(),
        format!(
            "The {step} export wrote {}, not into the evidence directory",
            result["path"]
        ),
    )?;
    let bytes = fs::read(&path)?;
    let decoded = match result["format"].as_str() {
        Some("jpeg") => decode_api_jpeg(&bytes)?,
        Some("tiff") => decode_api_tiff(&bytes)?,
        _ => decode_api_webp(&bytes)?,
    };
    let output = &export["plan"]["output"];
    ensure(
        json!({"width": decoded.size.0, "height": decoded.size.1}) == *output
            && result["width"] == output["width"]
            && result["height"] == output["height"]
            && result["bytes"] == bytes.len(),
        format!(
            "{} decodes to {:?} in {} bytes; the plan's output is {output}, the result {result}",
            path.display(),
            decoded.size,
            bytes.len()
        ),
    )?;
    ensure(
        decoded.icc,
        format!("{} carries no ICC profile", path.display()),
    )?;
    let status = frame.status()?;
    let mut said = format!(
        "Exported {file} \u{b7} {} \u{d7} {} \u{b7} ",
        result["width"], result["height"]
    );
    ensure(
        status.starts_with(&said),
        format!("The {step} status reads {status:?}"),
    )?;
    if let Some(quality) = result["quality"].as_u64() {
        said = format!(" \u{b7} quality {quality}");
        ensure(
            status.ends_with(&said),
            format!("The {step} status reads {status:?}, not ending {said:?}"),
        )?;
    }
    ensure(
        sheet(frame).is_null(),
        format!("The {step} export left the sheet open"),
    )?;
    let record = json!({
        "file": file, "settings": queued["settings"], "output": output, "bytes": bytes.len(),
        "quality": result["quality"], "structure": decoded.structure, "renderer": result["renderer"],
        "status": status, "bar": export["bar"],
    });
    checks.note(
        frame,
        "an export from the sheet, written and decoded here",
        record.clone(),
    );
    Ok((record, queued["settings"].clone()))
}

fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let sources = run.sources().to_vec();
    let large = stem(&sources[0]);
    // The catalog names a photograph by its resolved path, so a linked RAW by its target's name.
    let raw = (sources.len() == 3)
        .then(|| stem(&fs::canonicalize(&sources[1]).unwrap_or_else(|_| sources[1].clone())));
    let photo = stem(&sources[sources.len() - 1]);
    let mut checks = Checks::new();
    let mut files = Vec::new();

    // The menu: Export…, Export again disabled until an export succeeds, and the presets.
    let menu = launch.at("menu")?;
    let listed = &menu.state()["export"]["menu"];
    ensure(
        menu.state()["export"]["menu_open"] == true
            && listed["again"].is_null()
            && listed["presets"].as_array().is_some_and(|rows| {
                rows.iter()
                    .map(|row| &row[0])
                    .eq(["Web", "Print", "Master"].iter())
            }),
        format!("The Export menu shows {listed}"),
    )?;
    checks.note(menu, "the Export menu", listed.clone());

    // The sheet over the 24 MP JPEG, Web chosen, and its first plan.
    let opened = launch.at("sheet")?;
    let shown = model(opened);
    ensure(
        shown["title"] == "Export"
            && shown["subtitle"] == format!("{photo}.jpg")
            && shown["cards"]
                .as_array()
                .is_some_and(|cards| cards.len() == 3 && cards[0]["selected"] == true)
            && opened.state()["export"]["menu_open"] == false
            && plans_sent(opened) == 1
            && shown["large_image"].is_null(),
        format!("The sheet opened as {}", sheet(opened)),
    )?;
    checks.note(
        opened,
        "the sheet opened from the menu, Web chosen",
        shown.clone(),
    );

    // Choose… asks for one plan; the quality slider asks for none.
    for (step, before, more) in [
        ("web-folder", "sheet", 1),
        ("own-folder", "own-web", 1),
        ("own-quality", "own-folder", 0),
        ("mine-changed", "saved", 0),
    ] {
        let (a, b) = (launch.at(before)?, launch.at(step)?);
        ensure(
            plans_sent(b) == plans_sent(a) + more && sheet(b)["plan_in_flight"] == false,
            format!(
                "{step} asked for {} plans after {before}'s {}, expected {more} more",
                plans_sent(b),
                plans_sent(a)
            ),
        )?;
    }
    // Three changes at once: one plan in flight and the newest pending, so at most two.
    let (a, b) = (launch.at("large-folder")?, launch.at("webp")?);
    let coalesced = plans_sent(b) - plans_sent(a);
    ensure(
        (1..=2).contains(&coalesced),
        format!("Three changes in one step asked for {coalesced} plans"),
    )?;
    checks.note(
        b,
        "format, size and optimise changed at once, the plans coalesced",
        json!({"plans": coalesced}),
    );

    // Each export the sheet wrote.
    let mut exports = vec![
        ("web-folder", "web-written"),
        ("print-folder", "print-written"),
        ("master-folder", "master-written"),
        ("updated", "mine-written"),
    ];
    if raw.is_some() {
        exports.extend([
            ("raw-web-folder", "raw-web-written"),
            ("raw-print-folder", "raw-print-written"),
            ("raw-master-folder", "raw-master-written"),
        ]);
    }
    let mut sent = serde_json::Map::new();
    for (before, step) in exports {
        let (record, settings) = written(launch, before, step, &mut checks)?;
        sent.insert(step.into(), settings);
        files.push(record);
    }
    // A quick Web export shows no progress bar.
    let web = launch.at("web-written")?;
    ensure(
        web["step"]["export"]["bar"] == false,
        "The quick Web export showed a progress bar",
    )?;
    // Print's density, and its print size on the sheet before it.
    let print = model(launch.at("print-folder")?);
    ensure(
        print["density"][0] == "300" && print["readout"].as_str().is_some(),
        format!("Print's sheet shows {print}"),
    )?;

    // The preset changes: saved, changed and updated, the taken name, and deleted.
    let saved = launch.at("saved")?;
    let card = |frame: &Frame, name: &str| {
        model(frame)["cards"]
            .as_array()
            .and_then(|cards| cards.iter().find(|card| card["name"] == name).cloned())
            .unwrap_or(Value::Null)
    };
    ensure(
        card(launch.at("own-quality")?, "Web")["changed"] == true
            && model(launch.at("own-quality")?)["update"].is_null(),
        "A changed built-in preset is not marked changed, or offers Update",
    )?;
    ensure(
        card(saved, MINE)["selected"] == true && card(saved, MINE)["changed"] == false,
        format!("Save as preset… left the cards {}", model(saved)["cards"]),
    )?;
    let changed = launch.at("mine-changed")?;
    ensure(
        card(changed, MINE)["changed"] == true
            && model(changed)["update"] == format!("Update \u{201c}{MINE}\u{201d}\u{2026}"),
        format!("A changed own preset shows {}", model(changed)),
    )?;
    let updated = launch.at("updated")?;
    ensure(
        card(updated, MINE)["changed"] == false
            && model(updated)["taken"]
                == format!("{photo}-web.jpg exists there; this writes {photo}-web-2.jpg"),
        format!("Update left {}", model(updated)),
    )?;
    checks.note(
        updated,
        "the preset updated, and the taken name",
        model(updated).clone(),
    );
    let deleted = launch.at("deleted")?;
    ensure(
        card(deleted, MINE).is_null()
            && !deleted.state()["export"]["presets"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|preset| preset["name"] == MINE),
        format!("Delete left {}", model(deleted)["cards"]),
    )?;

    // Export again: the last export's settings, to the next free name.
    let again = launch.at("again")?;
    let repeated = &again["step"]["export"];
    ensure(
        repeated["record"]["status"] == "ready"
            && repeated["queued"]["settings"] == sent["mine-written"]
            && repeated["record"]["result"]["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(&format!("{photo}-web-3.jpg"))),
        format!("Export again sent {repeated}"),
    )?;
    let menu_again = &again.state()["export"]["menu"]["again"];
    checks.note(
        again,
        "Export again repeated the last export",
        json!({"menu": menu_again, "export": repeated}),
    );

    // The 60 MP JPEG: the WebP refused, the large-image warning, and the bar from the start.
    let webp = launch.at("webp")?;
    ensure(
        model(webp)["can_export"] == false
            && model(webp)["refusals"]
                .as_array()
                .is_some_and(|refusals| !refusals.is_empty())
            && sheet(webp)["plan"]["notices"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|notice| notice["code"] == "webp-too-large" && notice["refuses"] == true),
        format!(
            "The optimised WebP of {large} is not refused: {}",
            sheet(webp)
        ),
    )?;
    let warned = launch.at("large-warned")?;
    let carries = |frame: &Frame, code: &str| {
        sheet(frame)["plan"]["notices"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|notice| notice["code"] == code)
    };
    ensure(
        model(warned)["large_image"].is_string()
            && carries(warned, "large-image")
            && model(warned)["can_export"] == true,
        format!("The optimised 60 MP JPEG does not warn: {}", sheet(warned)),
    )?;
    // The notice shows exactly when the plan carries it.
    for step in [
        "sheet",
        "web-folder",
        "print-folder",
        "own-quality",
        "webp",
        "large-warned",
    ] {
        let frame = launch.at(step)?;
        ensure(
            model(frame)["large_image"].is_string() == carries(frame, "large-image"),
            format!("{step}: the large-image notice and the plan disagree"),
        )?;
    }
    checks.note(
        warned,
        "the large-image notice",
        model(warned)["large_image"].clone(),
    );
    let exporting = launch.at("exporting")?;
    let progressing = launch.at("progressing")?;
    let fraction = |frame: &Frame| frame.state()["export"]["status_bar"]["fraction"].as_f64();
    let running = &exporting.state()["export"]["running"];
    ensure(
        running["warned"] == true && running["bar"] == true && fraction(exporting).is_some(),
        format!(
            "The warned export does not show its bar from the start: {running} {}",
            exporting.state()["export"]["status_bar"]
        ),
    )?;
    let (first, then) = (
        fraction(exporting).unwrap_or_default(),
        fraction(progressing).unwrap_or_default(),
    );
    checks.compare(
        progressing,
        "the export's progress after a wait, against when the step started",
        then,
        first,
        Tolerance::Beyond(0.0),
    )?;
    ensure(
        then > first,
        format!("The progress did not rise: {first} then {then}"),
    )?;
    let performance = launch.at("performance")?;
    ensure(
        performance.state()["performance"]["expanded"] == true
            && performance.state()["performance"]["jobs"]
                .as_array()
                .is_some_and(|jobs| jobs.iter().any(|job| job["running"] == true)),
        format!(
            "Pressing the bar did not open the Performance section on the export: {}",
            performance.state()["performance"]["jobs"]
        ),
    )?;
    let ended = launch.at("cancel-ended")?;
    ensure(
        ended.state()["export"]["running"].is_null()
            && !launch.evidence.join(format!("{large}-web.jpg")).exists(),
        "The cancelled export is still running or left a file",
    )?;
    checks.note(
        ended,
        "the warned export's bar, opened into Performance and cancelled from its row",
        json!({"bar": [first, then], "label": exporting.state()["export"]["status_bar"]["label"]}),
    );

    // The batch: Export N… in batch mode, one photograph left out, and the report naming it.
    let count = sources.len();
    let opened = launch.at("batch-sheet")?;
    let shown = model(opened);
    let kinds = match raw {
        Some(_) => format!("1 RAW \u{b7} {} JPEG \u{b7} current edits", count - 1),
        None => format!("{count} JPEG \u{b7} current edits"),
    };
    ensure(
        sheet(opened)["batch"] == true
            && shown["title"] == format!("Export {count} photos")
            && shown["subtitle"] == kinds,
        format!("The batch sheet shows {shown}"),
    )?;
    let edited = launch.at("batch-edit")?;
    ensure(
        model(edited)["refusals"]
            .as_array()
            .is_some_and(|refusals| {
                refusals.len() == 1
                    && refusals[0]
                        .as_str()
                        .is_some_and(|text| text.starts_with("1 photo will be left out"))
            })
            && model(edited)["can_export"] == true,
        format!(
            "The batch sheet does not say which photograph it leaves out: {}",
            model(edited)
        ),
    )?;
    let settings = sheet(edited)["settings"].clone();
    let batch = launch.at("batch")?;
    let request = crate::select_catalog_smoke::batch_sent(
        batch,
        "batch",
        "batch.export",
        json!({"targets": {"kind": "selection"}, "settings": settings}),
    )?;
    let report = crate::select_catalog_smoke::batch_ended(batch, "batch", "batch-export")?;
    let skipped = report["skipped"].as_array().cloned().unwrap_or_default();
    let done = report["written"].as_array().cloned().unwrap_or_default();
    ensure(
        skipped.len() == 1 && skipped[0]["code"] == "webp-too-large" && done.len() == count - 1,
        format!("The batch report is {report}"),
    )?;
    for file in &done {
        let written = Path::new(file["path"].as_str().unwrap_or_default());
        let path = launch
            .evidence
            .join(written.file_name().unwrap_or_default());
        let decoded = decode_api_webp(&fs::read(&path)?)?;
        ensure(
            path.file_stem()
                .is_some_and(|name| name.to_string_lossy().ends_with(BATCH_SUFFIX))
                && decoded.icc,
            format!("The batch wrote {}", path.display()),
        )?;
        files.push(
            json!({"file": path.file_name().map(|name| name.to_string_lossy().into_owned()),
            "size": [decoded.size.0, decoded.size.1], "structure": decoded.structure}),
        );
    }
    let shown = &launch.at("batch-report")?.state()["select"]["catalog"]["sheet"];
    ensure(
        shown.to_string().contains(&large),
        format!("The batch report does not name {large}: {shown}"),
    )?;
    checks.note(
        batch,
        "the batch with one photograph left out",
        json!({"request": request, "report": report, "sheet": shown}),
    );

    // No sheet gesture asked for an asset's state, its history, a preview or an upload.
    let names = launch.names().to_vec();
    for pair in names.windows(2) {
        let (before, step) = (&pair[0], &pair[1]);
        let frame = launch.at(step)?;
        let gesture = frame["step"]["request"]["export"]
            .as_object()
            .is_some_and(|step| {
                step.keys().any(|key| {
                    matches!(
                        key.as_str(),
                        "sheet"
                            | "preset"
                            | "edit"
                            | "save_preset"
                            | "update_preset"
                            | "delete_preset"
                            | "close"
                    )
                })
            });
        // A sheet opened just after a photograph waits for it to settle, which is the open's
        // own work: it is counted from the step after.
        let after_open = launch.at(before)?["step"]["request"].get("open").is_some();
        if gesture && !after_open {
            let asked = unasked(launch.at(before)?, frame);
            ensure(
                asked.is_empty(),
                format!("The sheet gesture {step} asked the owner for {asked:?}"),
            )?;
        }
    }

    // Master's TIFFs are large: decoded above, they are not kept.
    for record in &files {
        if let Some(file) = record["file"]
            .as_str()
            .filter(|file| file.ends_with(".tif"))
        {
            let _ = fs::remove_file(launch.evidence.join(file));
        }
    }
    checks.write(
        &launch.evidence,
        "export-sheet",
        json!({
            "raw": raw.map_or(json!("pending: no --source RAW was given"), |stem| json!(stem)),
            "files": files,
            "sent": sent,
            "scope": "Each file the sheet, Export again and the batch wrote, decoded here with the image, tiff and image-webp crates: the plan's output, the result's length and the structure its settings ask for; each request the sheet's settings as an agent writes them; the plan's calls per gesture from the sheet's own count; the owner calls each sheet gesture made, by method, from the desktop's own count. Master's TIFFs are removed once decoded",
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sheet_plan_is_well_formed_with_and_without_a_raw() {
        let large = PathBuf::from("/generated/60mp.jpg");
        let photo = PathBuf::from("/generated/24mp.jpg");
        let raw = PathBuf::from("/raw/DSC_0001.NEF");
        let without = plan(&[large.clone(), photo.clone()]);
        without.validate().unwrap();
        assert!(without.scripted());
        let with = plan(&[large, raw, photo]);
        with.validate().unwrap();
        assert!(with.scripted());
        assert!(with.len() > without.len());
    }
}
