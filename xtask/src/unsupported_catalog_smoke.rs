//! The `unsupported-catalog` smoke scenario ([design](../../docs/design/auto-update.md#launching-past-an-unsupported-catalog)):
//! the evidence run's remembered catalog was made by another version, so the window opens on the
//! unsupported-catalog sheet instead of the launch failing.
//!
//! The run seeds `seed/catalog.sqlite`: a catalog this build makes with the fixture developed into
//! it, closed cleanly, then marked format 16 in its SQLite header as a later version's would be, with
//! an artifacts and an index directory beside it. A byte-for-byte copy of all of it is kept in
//! `seed.before/`. One launch names the seeded catalog as its remembered one
//! (`--evidence-catalog`) and Start a new catalog as the sheet's choice
//! (`--evidence-unsupported-catalog start`): it captures the sheet with its state
//! (`unsupported-catalog.png` and `.json`), creates and remembers `catalog (format 1).sqlite` beside
//! the refused catalog, and opens the fixture in the editor over it. The checks read the sheet's
//! state, its pixels, the log's events in order, the remembered preference, the new catalog's
//! header, and compare every refused file with its copy.
use crate::{
    scenario::{Checked, Launch, Plan, Run, Step, plan::only},
    smoke::Scenario,
    *,
};

pub const SCENARIO: &str = "unsupported-catalog";
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
pub const NOTE: &str = "The launch passes `--evidence-catalog seed/catalog.sqlite` and \
     `--evidence-unsupported-catalog start`: the evidence run opens the seeded catalog as its \
     remembered one rather than a catalog inside its evidence directory, captures the \
     unsupported-catalog sheet as `unsupported-catalog.png` with its state in \
     `unsupported-catalog.json`, then presses Start a new catalog. The seeded catalog is one this \
     build made, marked format 16 in its SQLite header (`user_version`), as a later version's \
     catalog is; `seed.before/` keeps a byte-for-byte copy of it, its artifacts and its index.";

const SEED: &str = "seed";
const BEFORE: &str = "seed.before";
const CATALOG: &str = "catalog.sqlite";
const NEW_CATALOG: &str = "catalog (format 1).sqlite";
const REFUSED_FORMAT: u32 = 16;
/// Where SQLite keeps `user_version` and `application_id` in a database's header.
const USER_VERSION_OFFSET: usize = 60;
const APPLICATION_ID_OFFSET: usize = 68;
/// The catalog's application mark, `LXFC`.
const APPLICATION_ID: u32 = 0x4C58_4643;
/// The sheet's capture and its state, as the desktop writes them.
const CAPTURE: &str = "unsupported-catalog.png";
const CAPTURE_STATE: &str = "unsupported-catalog.json";
const TITLE: &str = "This catalog was made by a different version of Luxforge";

pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![Step::opened("opened")])
}

/// The catalog another version made: this build's, with the fixture, closed cleanly and marked
/// [`REFUSED_FORMAT`], an artifacts and an index directory beside it, and a copy of all of it in
/// `before`.
fn seed(dir: &Path, fixture: &Path, before: &Path) -> Result {
    fs::create_dir_all(dir)?;
    let catalog = dir.join(CATALOG);
    let mut service = luxforge_core::EditorService::open(&catalog)
        .map_err(|error| format!("cannot seed the catalog: {error}"))?;
    service
        .import(fixture)
        .map_err(|error| format!("cannot develop the fixture into the seed: {error}"))?;
    drop(service);
    ensure(
        !luxforge_testbase::paths::wal(&catalog).exists(),
        "The seeded catalog kept its log after closing",
    )?;
    let mut bytes = fs::read(&catalog)?;
    bytes[USER_VERSION_OFFSET..USER_VERSION_OFFSET + 4]
        .copy_from_slice(&REFUSED_FORMAT.to_be_bytes());
    fs::write(&catalog, bytes)?;
    for (folder, file) in [
        ("catalog.artifacts", "later-version.json"),
        ("catalog.index", "index.sqlite"),
    ] {
        fs::create_dir_all(dir.join(folder))?;
        fs::write(
            dir.join(folder).join(file),
            format!("{folder} kept by format {REFUSED_FORMAT}"),
        )?;
    }
    copy_dir(dir, before)
}

fn copy_dir(from: &Path, to: &Path) -> Result {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Every file under `dir`, relative to it, with its bytes.
fn files(dir: &Path) -> Result<std::collections::BTreeMap<PathBuf, Vec<u8>>> {
    fn walk(
        root: &Path,
        dir: &Path,
        files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) -> Result {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(root, &path, files)?;
            } else {
                files.insert(path.strip_prefix(root)?.to_path_buf(), fs::read(&path)?);
            }
        }
        Ok(())
    }
    let mut files = std::collections::BTreeMap::new();
    walk(dir, dir, &mut files)?;
    Ok(files)
}

pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    if let Some(note) = scenario.note {
        run.note(note);
    }
    run.check(|run| {
        let seed_dir = std::path::absolute(run.out().join(SEED))?;
        if !run.replaying() {
            let fixture = sources.first().ok_or("The scenario opens its fixture")?;
            seed(&seed_dir, fixture, &run.out().join(BEFORE))?;
        }
        run.hash(&sources)?;
        let mut launch = Launch::app()
            .evidence_catalog(&seed_dir.join(CATALOG), "start")
            .open_all(&sources);
        if let Some(window) = scenario.window {
            launch = launch.window(window);
        }
        let evidence = run.launch(launch)?.dir;
        let checked = plan(&sources).check(&evidence)?;
        (scenario.verify)(run, std::slice::from_ref(&checked))?;
        run.sources_unchanged()
    })
}

/// A 32-bit big-endian header field of the database at `path`.
fn header_field(path: &Path, offset: usize) -> Result<u32> {
    let bytes = fs::read(path)?;
    let field: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or("The database is shorter than its header")?
        .try_into()?;
    Ok(u32::from_be_bytes(field))
}

/// Mean and variance of the luminance over a box of `image`, as fractions of its size.
fn luminance(image: &image::RgbaImage, [x0, y0, x1, y1]: [f64; 4]) -> (f64, f64) {
    let (width, height) = (f64::from(image.width()), f64::from(image.height()));
    let mut values = Vec::new();
    for y in (y0 * height) as u32..(y1 * height) as u32 {
        for x in (x0 * width) as u32..(x1 * width) as u32 {
            let [r, g, b, _] = image.get_pixel(x, y).0;
            values.push(0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b));
        }
    }
    let n = values.len().max(1) as f64;
    let mean = values.iter().sum::<f64>() / n;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    (mean, variance)
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let evidence = &launch.evidence;
    let seed_dir = std::path::absolute(
        run.recorded(&run.out().join(SEED))
            .unwrap_or_else(|| run.out().join(SEED)),
    )?;
    let before_dir = run
        .recorded(&run.out().join(BEFORE))
        .unwrap_or_else(|| run.out().join(BEFORE));
    // The sheet's state: both formats, the unchanged catalog, the releases page, the new catalog's
    // name and both choices, captured from the hidden window before either was taken.
    let frame = read_json(&evidence.join(CAPTURE_STATE))?;
    let state = &frame["state"];
    let sheet = &state["unsupported_catalog"];
    let refused = seed_dir.join(CATALOG);
    let new = seed_dir.join(NEW_CATALOG);
    ensure(
        sheet["title"] == TITLE,
        format!("The sheet's title: {sheet}"),
    )?;
    let formats = sheet["formats"].as_str().unwrap_or_default();
    ensure(
        formats.starts_with("It is catalog format 16. Luxforge ")
            && formats.ends_with(" opens catalog format 1."),
        format!("The sheet names both formats: {formats:?}"),
    )?;
    ensure(
        sheet["unchanged"]
            .as_str()
            .is_some_and(|text| text.contains("unchanged") && text.contains("can still open it")),
        format!("The sheet says the catalog is unchanged: {sheet}"),
    )?;
    ensure(
        sheet["link"] == "Open releases page"
            && sheet["catalog"] == json!(format!("{CATALOG} in {}", seed_dir.display())),
        format!("The sheet names the refused catalog and links the releases: {sheet}"),
    )?;
    ensure(
        sheet["new_catalog"]
            .as_str()
            .is_some_and(|text| text.contains(NEW_CATALOG)),
        format!("The sheet names the new catalog: {sheet}"),
    )?;
    ensure(
        sheet["start"] == json!({"label": "Start a new catalog", "enabled": true})
            && sheet["quit"] == json!({"label": "Quit", "enabled": true})
            && sheet["problem"].is_null(),
        format!("Both choices are offered: {sheet}"),
    )?;
    ensure(
        state["refusal"]["catalog_format"] == 16
            && state["refusal"]["expected_format"] == 1
            && state["new_catalog"] == json!(new)
            && state["phase"] == "offered"
            && state["hidden"] == true
            && state["choice"] == "start",
        format!("The captured state: {state}"),
    )?;
    ensure(
        frame["capture_provenance"] == "window-renderer-readback",
        "The sheet is a renderer readback",
    )?;
    // Its pixels: the size the state records, and the sheet drawn over the window's middle, where
    // its text and buttons vary far more than the plain background at the window's corner.
    let capture = image::open(evidence.join(CAPTURE))?.to_rgba8();
    ensure(
        frame["physical_size"] == json!([capture.width(), capture.height()]),
        "The capture is the size its state records",
    )?;
    let (sheet_mean, sheet_variance) = luminance(&capture, [0.3, 0.3, 0.7, 0.7]);
    let (corner_mean, corner_variance) = luminance(&capture, [0.0, 0.0, 0.1, 0.1]);
    ensure(
        sheet_variance > corner_variance + 50.0,
        format!(
            "The sheet is drawn: luminance variance {sheet_variance:.1} over the middle, \
             {corner_variance:.1} at the corner"
        ),
    )?;
    // The log: the sheet before any catalog opened, its capture, Start and the new catalog, then
    // the editor's own startup over it.
    let names: Vec<&str> = launch
        .events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();
    let order = [
        "startup",
        "unsupported_catalog",
        "unsupported_catalog_captured",
        "unsupported_catalog_start",
        "unsupported_catalog_started",
        "startup",
    ];
    let mut at = 0;
    for name in names.iter().copied() {
        if at < order.len() && name == order[at] {
            at += 1;
        }
    }
    ensure(
        at == order.len(),
        format!("The log's events are in order {order:?}: {names:?}"),
    )?;
    let started = launch
        .events
        .iter()
        .find(|event| event["event"] == "unsupported_catalog_started")
        .ok_or("No unsupported_catalog_started event")?;
    ensure(
        started["detail"]["new_catalog"] == json!(new)
            && started["detail"]["unremembered"].is_null(),
        format!("The new catalog started and was remembered: {started}"),
    )?;
    // The new catalog: beside the refused one, this format, marked, remembered by this run's own
    // preferences, and the fixture open in the editor over it.
    ensure(
        header_field(&new, USER_VERSION_OFFSET)? == 1
            && header_field(&new, APPLICATION_ID_OFFSET)? == APPLICATION_ID,
        "The new catalog is format 1 and marked as a Luxforge catalog",
    )?;
    let preferences = read_json(&evidence.join("host/config/preferences.json"))?;
    ensure(
        preferences["catalog"] == json!(new),
        format!("The new catalog is remembered: {preferences}"),
    )?;
    let opened = launch.at("opened")?;
    ensure(
        opened.state()["phase"] != "error",
        format!(
            "The fixture opened over the new catalog: {}",
            opened.state()
        ),
    )?;
    // The refused catalog, its artifacts and its index keep every byte, and nothing new is among
    // them.
    let before = files(&before_dir)?;
    let mut after = files(&seed_dir)?;
    after.retain(|path, _| {
        let name = path
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        !name.starts_with("catalog (format 1)")
    });
    ensure(
        after.keys().eq(before.keys()),
        format!(
            "The refused catalog's files are the ones seeded: {:?} against {:?}",
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>()
        ),
    )?;
    for (path, bytes) in &before {
        ensure(
            after.get(path) == Some(bytes),
            format!("{} changed", path.display()),
        )?;
    }
    ensure(
        header_field(&refused, USER_VERSION_OFFSET)? == REFUSED_FORMAT,
        "The refused catalog keeps its marker",
    )?;
    write_json(
        &run.out().join("unsupported-catalog-checks.json"),
        &json!({
            "sheet": sheet,
            "luminance": {
                "sheet": {"mean": sheet_mean, "variance": sheet_variance},
                "corner": {"mean": corner_mean, "variance": corner_variance},
            },
            "events": order,
            "new_catalog": new,
            "refused_files": before.keys().collect::<Vec<_>>(),
            "refused_bytes_unchanged": true,
        }),
    )
}
