//! The `export` scenario's sheet launch: the Export sheet end to end in the real editor, over the
//! generated 24 MP and 60 MP JPEGs, a 16,384 × 64 JPEG the run writes into its own directory and,
//! when `--source` names one, a supplied RAW.
//!
//! The launch opens the 60 MP JPEG, the wide JPEG, the RAW and the 24 MP JPEG, in that order, so
//! the 24 MP one is displayed. Over it: the title bar's Export menu, with Export again disabled and
//! the three built-in presets; the sheet, Web chosen and Choose… answered with the run's evidence
//! directory (no step ever writes beside an original), exported; then Print and Master the same
//! way. Web sharpens for the screen and Print for glossy paper, both at Standard, and Master not at
//! all. A changed Web is saved as the preset `Mine`, changed again and updated, its plan naming the
//! taken `-web` file and writing `-web-2`; Export again repeats it as `-web-3`; and `Mine` is
//! deleted from its card's menu. Web is then written as an 8-bit TIFF twice, sharpened and with
//! Sharpen set to None: the sharpened file must be the unsharpened one sharpened here by the
//! reference, within the spatial display limit, with every pixel's chroma unchanged, and Print's
//! JPEG nearer Master's TIFF sharpened for glossy paper than Master's itself. The RAW, when
//! supplied, is opened and exported with Web, Print and Master; without one those frames are
//! recorded as pending. The wide JPEG is opened: an optimised full-size WebP, wider than the
//! format's 16,383 pixels, is refused by the plan's `webp-too-large` notice. The 60 MP JPEG is
//! opened: the same WebP is written, but warned with `large-image`, as the optimised JPEG is; that
//! is exported with the Performance section closed, its progress bar shown in the status bar from
//! the start and rising, pressed to open the section, and cancelled from the section's row,
//! leaving no file. Last, every photograph is selected in Select and Export N… opens the sheet in
//! batch mode: the same optimised WebP leaves the wide photograph out, which the sheet says before
//! the batch runs and its report names after.
//!
//! The sharpened and unsharpened Web TIFFs are written by a second launch over the same
//! photographs, `pair/`, since the journey's script holds the editor's 64 steps.
//!
//! Every written file is decoded here and must be the plan's output, the result's length and
//! format and the structure its settings ask for; each request the sheet sent is the sheet's own
//! settings, as an agent writes them (the window's screen density standing in for none, unless
//! they sharpen for paper); the plan is asked only for the settings it depends on, at most one at
//! a time with the newest pending; and no sheet gesture asked the owner for an asset's state, its
//! history, a preview or an upload.
use super::{decode_api_jpeg, decode_api_tiff, decode_api_webp};
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance},
    smoke::{self, APP, LaunchSpec, PANELLED, Scenario, Source},
    *,
};
use luxforge_core::{
    Cancel, Sharpen, SharpenAmount, SharpenMedium,
    tiles::{OutputSharpen, Pixels, sharpen},
};
use luxforge_evidence::{
    self as script, CatalogStep, ExportStep, ExportWrite, SelectStep, SelectWorkspace, SheetEdit,
    SheetFolder,
};
use luxforge_reference::preview_error::{self, Class, Rgb8};

/// The photograph most of the journey edits, and the large one the warning, the refusal and the
/// cancel are about.
pub const PHOTO: &str = "fixtures/generated/24mp.jpg";
pub const LARGE: &str = "fixtures/generated/60mp.jpg";
/// The JPEG the run writes into its own directory, wider than a WebP can be: the refusal and the
/// photograph the batch leaves out.
pub const WIDE: &str = "wide.jpg";
/// Its size: one pixel past WebP's largest side.
const WIDE_SIZE: (u32, u32) = (16_384, 64);
/// The second launch's evidence directory: Web written as an 8-bit TIFF sharpened and not.
const PAIR: &str = "pair";
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

/// The sheet launch's row: the journey's launch, then the sharpening pair's, whose steps the
/// journey's script has no room for, and their checks.
static SHEET: Scenario = Scenario {
    name: "export",
    about: "The Export sheet's journey",
    launches: &[
        LaunchSpec {
            // Four photographs opened, twelve exports written and a batch of optimised WebPs, the
            // 60 MP one among them.
            deadline: Some(std::time::Duration::from_secs(360)),
            ..APP
        },
        LaunchSpec {
            name: PAIR,
            script: "pair-script.json",
            deadline: Some(std::time::Duration::from_secs(180)),
            ..APP
        },
    ],
    verify,
    source: Source::Fixtures(&[]),
    window: Some(PANELLED),
    note: None,
    own: None,
};

/// Launch the sheet's journey in `run`, over the generated JPEGs, the wide one it writes (a replay
/// reads the recorded one) and `raw` when one is supplied.
pub(super) fn run(run: Run, raw: Option<PathBuf>) -> Result {
    let root = run.root().to_owned();
    // A replay opens the recorded run's own, the path its script named.
    let wide = match run.recorded(&run.out().join(WIDE)) {
        Some(recorded) => recorded,
        None => {
            let wide = run.out().join(WIDE);
            write_wide(&wide)?;
            wide
        }
    };
    let mut sources = vec![root.join(LARGE), wide];
    sources.extend(raw);
    sources.push(root.join(PHOTO));
    smoke::launch_planned(run, &SHEET, sources, |_, sources| {
        Ok(vec![plan(sources), pair_plan(sources)])
    })
}

/// A [`WIDE_SIZE`] JPEG of diagonal stripes at `path`.
fn write_wide(path: &Path) -> Result {
    let (width, height) = WIDE_SIZE;
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        let band = ((x + y) / 8 % 2) as u8;
        image::Rgb([60 + 120 * band, 90 + (x % 128) as u8, 140 - 60 * band])
    });
    let file = std::io::BufWriter::new(fs::File::create(path)?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(file, 90).encode_image(&image)?;
    Ok(())
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

/// Web as an 8-bit TIFF named `-{name}`, Sharpen `sharpen`: four frames named `{name}-sheet`,
/// `{name}-web`, `{name}-edit` and `{name}-written`.
fn web_tiff(name: &str, sharpen: Option<&str>) -> Vec<Step> {
    vec![
        export(
            &format!("{name}-sheet"),
            ExportStep::Sheet(Default::default()),
        ),
        export(&format!("{name}-web"), ExportStep::Preset("Web".into())),
        edit(
            &format!("{name}-edit"),
            SheetEdit {
                format: Some("tiff".into()),
                bit_depth: Some(8),
                sharpen: sharpen.map(Into::into),
                folder: Some(SheetFolder::Evidence),
                suffix: Some(format!("-{name}")),
                ..SheetEdit::default()
            },
        ),
        export(
            &format!("{name}-written"),
            ExportStep::Write(ExportWrite::default()),
        )
        .status_starts("Exported "),
    ]
}

/// The second launch, over the same sources: Web's sharpening in the file, the same TIFF of the
/// 24 MP JPEG sharpened, then not.
fn pair_plan(sources: &[PathBuf]) -> Plan {
    let mut steps: Vec<Step> = sources
        .iter()
        .map(|source| Step::opened(format!("opened-{}", stem(source))))
        .collect();
    steps.extend(web_tiff("screen", None));
    steps.extend(web_tiff("plain", Some("none")));
    Plan::new(steps)
}

/// The journey, over the sources as the run opens them: the 60 MP JPEG, the wide JPEG, the RAW
/// when supplied, then the 24 MP JPEG.
fn plan(sources: &[PathBuf]) -> Plan {
    let (large, wide) = (&sources[0], &sources[1]);
    let raw = (sources.len() == 4).then(|| &sources[2]);
    // Another photograph opened: its own entry is shown, which no commit made, so the step
    // expects no count of commits against the photograph before it.
    let open = |name: &str, path: &Path| {
        Step::new(
            name,
            script::Step::Open {
                path: path.display().to_string(),
            },
        )
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
    let full_webp = || SheetEdit {
        format: Some("webp".into()),
        size: Some("full".into()),
        optimise: Some(true),
        ..SheetEdit::default()
    };
    steps.extend([
        // Opened after the RAW, whose initial development is an entry: the revision shown moves
        // from the RAW's to the wide JPEG's, which no commit made.
        Step::new(
            "wide-open",
            script::Step::Open {
                path: wide.display().to_string(),
            },
        ),
        // Captured once the photograph has settled, as the RAW's is.
        Step::new("wide-sheet", ExportStep::Sheet(Default::default())),
        export("wide-web", ExportStep::Preset("Web".into())),
        edit("wide-folder", evidence_folder()),
        edit("wide-webp", full_webp()),
        export("wide-closed", ExportStep::Close),
        open("large-open", large),
        Step::new("large-sheet", ExportStep::Sheet(Default::default())),
        export("large-web", ExportStep::Preset("Web".into())),
        edit("large-folder", evidence_folder()),
        // Three changes in one step: the plan coalesces them.
        edit("large-webp", full_webp()),
        // An optimised full-chroma JPEG at quality 100 under the smallest size limit: the limit
        // search encodes the whole frame several times, so the export runs for seconds however
        // quickly this host encodes one flat frame, and it is cancelled before it ends.
        edit(
            "large-warned",
            SheetEdit {
                format: Some("jpeg".into()),
                quality: Some(100),
                chroma: Some("full".into()),
                limit: Some(true),
                limit_kb: Some("11".into()),
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
    // The batch: every photograph selected in Select, an optimised WebP the wide one cannot be.
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
    let for_paper = matches!(
        expected["sharpen"]["medium"].as_str(),
        Some("matte" | "glossy")
    );
    if expected["pixels_per_inch"].is_null() && cfg!(target_os = "macos") && !for_paper {
        // The window's own density stands in for none, as the screen shows the file, unless the
        // file is sharpened for paper, for 300 ppi.
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
    let [launch, pair] = launches else {
        return Err(format!("Expected two launches, found {}", launches.len()).into());
    };
    let sources = run.sources().to_vec();
    let (large, wide) = (stem(&sources[0]), stem(&sources[1]));
    // The catalog names a photograph by its resolved path, so a linked RAW by its target's name.
    let raw = (sources.len() == 4)
        .then(|| stem(&fs::canonicalize(&sources[2]).unwrap_or_else(|_| sources[2].clone())));
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
    let (a, b) = (launch.at("large-folder")?, launch.at("large-webp")?);
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
    let mut named = serde_json::Map::new();
    for (before, step) in exports {
        let (record, settings) = written(launch, before, step, &mut checks)?;
        sent.insert(step.into(), settings);
        named.insert(step.into(), record["file"].clone());
        files.push(record);
    }
    for (before, step) in [
        ("screen-edit", "screen-written"),
        ("plain-edit", "plain-written"),
    ] {
        let (mut record, settings) = written(pair, before, step, &mut checks)?;
        sent.insert(step.into(), settings);
        named.insert(step.into(), record["file"].clone());
        record["launch"] = json!(PAIR);
        files.push(record);
    }
    sharpened(launch, pair, &sent, &named, &mut checks)?;
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

    // The wide JPEG's WebP refused; the 60 MP JPEG's warned, as its JPEG is, and the bar from the
    // start.
    let refused = launch.at("wide-webp")?;
    ensure(
        model(refused)["can_export"] == false
            && model(refused)["refusals"]
                .as_array()
                .is_some_and(|refusals| !refusals.is_empty())
            && sheet(refused)["plan"]["notices"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|notice| notice["code"] == "webp-too-large" && notice["refuses"] == true),
        format!(
            "The optimised WebP of {wide} is not refused: {}",
            sheet(refused)
        ),
    )?;
    checks.note(
        refused,
        "the WebP of a 16,384-pixel-wide photograph refused",
        json!({"refusals": model(refused)["refusals"], "notices": sheet(refused)["plan"]["notices"]}),
    );
    let carries = |frame: &Frame, code: &str| {
        sheet(frame)["plan"]["notices"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|notice| notice["code"] == code)
    };
    for (step, what) in [
        ("large-webp", "optimised 60 MP WebP"),
        ("large-warned", "optimised 60 MP JPEG"),
    ] {
        let warned = launch.at(step)?;
        ensure(
            model(warned)["large_image"].is_string()
                && carries(warned, "large-image")
                && !carries(warned, "webp-too-large")
                && model(warned)["can_export"] == true,
            format!("The {what} does not warn, or is refused: {}", sheet(warned)),
        )?;
    }
    let warned = launch.at("large-warned")?;
    // The notice shows exactly when the plan carries it.
    for step in [
        "sheet",
        "web-folder",
        "print-folder",
        "own-quality",
        "wide-webp",
        "large-webp",
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
        "the large-image notice, on the 60 MP WebP and JPEG",
        json!({"webp": model(launch.at("large-webp")?)["large_image"], "jpeg": model(warned)["large_image"]}),
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
            && model(edited)["can_export"] == true
            && model(edited)["export_label"] == format!("Export {}", count - 1)
            && model(edited)["summary"].as_str().is_some_and(|summary| {
                summary.starts_with(&format!(
                    "{} files \u{b7} 1 left out \u{b7} WebP",
                    count - 1
                ))
            }),
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
    ensure(
        done.iter().any(|file| {
            file["path"]
                .as_str()
                .is_some_and(|path| path.contains(&large))
        }),
        format!("The batch did not write the 60 MP photograph's WebP: {report}"),
    )?;
    let shown = &launch.at("batch-report")?.state()["select"]["catalog"]["sheet"];
    ensure(
        shown.to_string().contains(&wide),
        format!("The batch report does not name {wide}: {shown}"),
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
            .filter(|file| file.ends_with("-master.tif"))
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
            "scope": "Each file the sheet, Export again and the batch wrote, decoded here with the image, tiff and image-webp crates: the plan's output, the result's length and the structure its settings ask for; each request the sheet's settings as an agent writes them; the sharpened Web TIFF against the unsharpened one sharpened here by the core's reference, by the spatial display limit (CIEDE2000, luxforge_reference::preview_error) and every pixel's chroma, and Print's JPEG against Master's TIFF sharpened the same way, by mean luma difference where that sharpening changes a pixel; the plan's calls per gesture from the sheet's own count; the owner calls each sheet gesture made, by method, from the desktop's own count. Master's TIFFs are removed once decoded",
        }),
    )
}

/// A written file's 8-bit codes: a TIFF's 16-bit samples rounded to 8 bits, a JPEG's as decoded.
fn codes(path: &Path) -> Result<((u32, u32), Vec<u8>)> {
    use tiff::decoder::{Decoder, DecodingResult};
    let bytes = fs::read(path)?;
    if path.extension().is_some_and(|extension| extension == "tif") {
        let mut decoder = Decoder::new(std::io::Cursor::new(&bytes))?;
        let size = decoder.dimensions()?;
        let rgb = match decoder.read_image()? {
            DecodingResult::U8(samples) => samples,
            DecodingResult::U16(samples) => samples
                .iter()
                .map(|&sample| ((u32::from(sample) * 255 + 32_767) / 65_535) as u8)
                .collect(),
            _ => return Err(format!("{} is neither 8 nor 16 bits", path.display()).into()),
        };
        return Ok((size, rgb));
    }
    let image = image::load_from_memory(&bytes)?.to_rgb8();
    Ok((image.dimensions(), image.into_raw()))
}

/// `rgb` sharpened by the core's reference, as an export with `sharpen` at `pixels_per_inch` is.
fn sharpened_here(
    rgb: &[u8],
    size: (u32, u32),
    sharpen: Sharpen,
    pixels_per_inch: Option<u16>,
) -> Result<Vec<u8>> {
    // The output's codes as the core holds them, four bytes a pixel.
    let mut pixels = Pixels::Eight(
        rgb.chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
    );
    let kernel = OutputSharpen::new(sharpen, pixels_per_inch).kernel();
    sharpen::sharpen(&mut pixels, size, &kernel, &Cancel::new())
        .map_err(|error| format!("The reference's sharpening failed: {error:?}"))?;
    match pixels {
        Pixels::Eight(codes) => Ok(codes
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect()),
        Pixels::Sixteen(_) => Err("The reference sharpened 8 bits into 16".into()),
    }
}

/// The mean absolute difference of two images' Rec. 709 luma, in 8-bit codes, over the pixels
/// where `sharpened` differs from `unsharpened`: the ones sharpening changes. With their count.
fn luma_difference(a: &[u8], b: &[u8], sharpened: &[u8], unsharpened: &[u8]) -> (f64, usize) {
    let luma = |pixel: &[u8]| {
        (2126.0 * f64::from(pixel[0]) + 7152.0 * f64::from(pixel[1]) + 722.0 * f64::from(pixel[2]))
            / 10_000.0
    };
    let (sum, count) = a
        .chunks_exact(3)
        .zip(b.chunks_exact(3))
        .zip(sharpened.chunks_exact(3).zip(unsharpened.chunks_exact(3)))
        .filter(|(_, (sharpened, unsharpened))| sharpened != unsharpened)
        .fold((0.0, 0usize), |(sum, count), ((a, b), _)| {
            (sum + (luma(a) - luma(b)).abs(), count + 1)
        });
    (sum / count.max(1) as f64, count)
}

/// Web's and Print's sharpening, read from the files they wrote: their requests carry it; the Web
/// TIFF sharpened is the one with Sharpen None sharpened here by the core's reference, within the
/// spatial display limit, every pixel's chroma unchanged; and Print's JPEG is nearer Master's TIFF
/// sharpened for glossy paper at 300 ppi than Master's itself.
fn sharpened(
    launch: &Checked,
    pair: &Checked,
    sent: &serde_json::Map<String, Value>,
    named: &serde_json::Map<String, Value>,
    checks: &mut Checks,
) -> Result {
    let standard = |medium: SharpenMedium| Sharpen {
        medium,
        amount: SharpenAmount::Standard,
    };
    for (step, expected) in [
        (
            "web-written",
            json!({"medium": "screen", "amount": "standard"}),
        ),
        (
            "print-written",
            json!({"medium": "glossy", "amount": "standard"}),
        ),
        ("master-written", Value::Null),
        (
            "screen-written",
            json!({"medium": "screen", "amount": "standard"}),
        ),
        ("plain-written", Value::Null),
    ] {
        ensure(
            sent[step]["sharpen"] == expected,
            format!(
                "{step} sent sharpen {}, not {expected}",
                sent[step]["sharpen"]
            ),
        )?;
    }
    let row = &model(launch.at("print-folder")?)["sharpen"];
    ensure(
        row["medium"] == 3 && row["amount"] == 1 && row["amount_enabled"] == true,
        format!("Print's Sharpen row shows {row}"),
    )?;
    let none = &model(pair.at("plain-edit")?)["sharpen"];
    ensure(
        none["medium"] == 0
            && none["amount_enabled"] == false
            && none["changed"] == "Changed from Web (Screen, Standard)",
        format!("Sharpen None shows {none}"),
    )?;
    let file = |launch: &Checked, step: &str| {
        launch
            .evidence
            .join(named[step].as_str().unwrap_or_default())
    };

    let (size, screen) = codes(&file(pair, "screen-written"))?;
    let (plain_size, plain) = codes(&file(pair, "plain-written"))?;
    ensure(
        size == plain_size,
        format!("The sharpened Web TIFF is {size:?}, the unsharpened {plain_size:?}"),
    )?;
    let reference = sharpened_here(&plain, size, standard(SharpenMedium::Screen), None)?;
    let statistics = preview_error::compare(
        Rgb8::new(size.0, size.1, &screen)?,
        Rgb8::new(size.0, size.1, &reference)?,
        [0, 0, size.0, size.1],
    )?;
    let figures = json!({
        "pixels": statistics.pixels, "mean": statistics.mean,
        "worst_block": statistics.worst_block, "p99": statistics.p99,
        "mean_delta_l": statistics.mean_delta_l, "max": statistics.max,
    });
    ensure(
        preview_error::verdict(&statistics, Class::Spatial).passed(),
        format!(
            "The sharpened Web TIFF is past the spatial display limit of the unsharpened one sharpened by the reference: {figures}"
        ),
    )?;
    let pairs = || screen.chunks_exact(3).zip(plain.chunks_exact(3));
    let chroma = |pixel: &[u8]| {
        [
            i16::from(pixel[0]) - i16::from(pixel[1]),
            i16::from(pixel[2]) - i16::from(pixel[1]),
        ]
    };
    let shifted = pairs().filter(|(a, b)| chroma(a) != chroma(b)).count();
    let pixels = screen.len() / 3;
    // Where the reference's sharpening changes a pixel, the file is its sharpened value, not the
    // unsharpened one.
    let (nearer, changed) = luma_difference(&screen, &reference, &reference, &plain);
    let (unsharpened, _) = luma_difference(&screen, &plain, &reference, &plain);
    ensure(
        changed > 0 && nearer * 4.0 < unsharpened && shifted == 0,
        format!(
            "Where the reference's sharpening changes {changed} of {pixels} pixels, the file's luma is {nearer} codes from it and {unsharpened} from the unsharpened file, and {shifted} pixels' chroma changed"
        ),
    )?;
    let limits = Class::Spatial.limits();
    checks.note(
        pair.at("plain-written")?,
        "Web's sharpening in the file: the sharpened TIFF against the unsharpened one sharpened by the reference, within the spatial display limit, no pixel's chroma changed",
        json!({
            "files": [named["screen-written"], named["plain-written"]],
            "statistics": figures,
            "limits": {"mean": limits.mean, "worst_block": limits.worst_block, "p99": limits.p99,
                "mean_delta_l_abs": limits.mean_delta_l},
            "changed_pixels": changed, "pixels": pixels, "chroma_changed": shifted,
            "luma_difference_where_sharpened": {"against_reference": nearer,
                "against_unsharpened": unsharpened},
        }),
    );

    // Print's JPEG against Master's TIFF, which is not sharpened, sharpened as Print is.
    let (size, print) = codes(&file(launch, "print-written"))?;
    let (master_size, master) = codes(&file(launch, "master-written"))?;
    ensure(
        size == master_size,
        format!("Print's JPEG is {size:?}, Master's TIFF {master_size:?}"),
    )?;
    let glossy = sharpened_here(&master, size, standard(SharpenMedium::Glossy), Some(300))?;
    let (nearer, changed) = luma_difference(&print, &glossy, &glossy, &master);
    let (unsharpened, _) = luma_difference(&print, &master, &glossy, &master);
    ensure(
        changed > 0 && nearer < unsharpened,
        format!(
            "Where sharpening Master's TIFF for glossy paper changes {changed} pixels, Print's JPEG is no nearer it ({nearer}) than Master's own ({unsharpened})"
        ),
    )?;
    checks.note(
        launch.at("print-written")?,
        "Print's sharpening in the file: its JPEG nearer Master's TIFF sharpened for glossy paper at 300 ppi than Master's own, by mean luma difference in 8-bit codes where that sharpening changes a pixel",
        json!({"changed_pixels": changed, "against_sharpened": nearer, "against_unsharpened": unsharpened}),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sheet_plan_is_well_formed_with_and_without_a_raw() {
        let large = PathBuf::from("/generated/60mp.jpg");
        let wide = PathBuf::from("/run/wide.jpg");
        let photo = PathBuf::from("/generated/24mp.jpg");
        let raw = PathBuf::from("/raw/DSC_0001.NEF");
        let without = plan(&[large.clone(), wide.clone(), photo.clone()]);
        without.validate().unwrap();
        assert!(without.scripted());
        let sources = [large, wide, raw, photo];
        let with = plan(&sources);
        with.validate().unwrap();
        assert!(with.scripted());
        assert!(with.len() > without.len());
        let pair = pair_plan(&sources);
        pair.validate().unwrap();
        // Each launch's script within what the editor runs.
        for plan in [&with, &pair] {
            let steps = plan.script().as_array().map_or(0, Vec::len);
            assert!(
                steps <= luxforge_evidence::MAX_SCRIPT_STEPS,
                "{steps} steps"
            );
        }
    }
}
