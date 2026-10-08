//! The `filmstrip` smoke scenario: moving through Develop's development set across JPEG and RAW
//! photographs, in the real editor at 1440 × 900.
//!
//! The run generates real JPEGs (`generate-catalog --images 120`) and copies three of them —
//! the darkest, the brightest and the one between, by the middle of the picture, so each frame's
//! picture can be told from its neighbours' — into a scratch folder, with a copy of the RAW file
//! `--source` names when one is given (the user's file is hashed before and after, and never
//! touched). It develops them into a new catalog with `pick.develop` and asks the core for the
//! catalog view the frames are checked against (`filmstrip-expected.json`). Without a RAW source
//! the RAW photograph's steps are not run, and the run records them pending, never passed.
//!
//! The editor opens that catalog with nothing open. Its frames, in [`plan`] order: `G` showing
//! Select; All photographs; the first photograph double-clicked, which opens Develop on it with the
//! view's photographs as the development set; then for each next photograph, the neighbours' large
//! previews decoded, `→` captured in the frame after the key, and the photograph's exact render;
//! `←` the same; a press on the filmstrip's first cell; and the filmstrip collapsed and expanded
//! with `Cmd+Option+F`.
//!
//! Each frame is checked against the core's view: the set is its photographs in order, the active
//! one the step moved to, the preview drawn that photograph's own (by asset and entry) and drawn in
//! the frame after the key, and the open photograph the one moved to. What the capture shows is
//! read back: the picture's middle against the photograph's own large preview, decoded
//! independently from the cache after the run (`filmstrip-after.json`), and nearer its own than
//! any other photograph's of the set.
use crate::{
    generate_catalog,
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step, Tolerance, plan::only},
    smoke::Scenario,
    *,
};
use luxforge_core::{ApiRequest, ClientId, EditorService, OwnerHandle};
use luxforge_evidence::{self as script, DevelopStep, SelectStep, SetStep};

pub const SCENARIO: &str = "filmstrip";
/// What `reproduce.md` says the run does before it launches.
pub const NOTE: &str = "The run first generates real JPEGs with `cargo xtask generate-catalog \
    --images 120 --seed 1`, copies three of them, and a copy of the RAW file `--source` names when \
    one is given, into a scratch folder, develops them into `generated/catalog.sqlite` with \
    `pick.develop`, asks the core for the catalog view the frames are checked against \
    (`filmstrip-expected.json`) and launches the editor over that catalog with `--catalog`; after \
    the run it reads each photograph's large preview from the cache (`filmstrip-after.json`). \
    Without a RAW source the RAW steps are recorded pending.";
pub const GENERATED: &str = "generated";
pub const EXPECTED: &str = "filmstrip-expected.json";
pub const AFTER: &str = "filmstrip-after.json";
const SEED: u64 = 1;
const IMAGES: u32 = 120;
/// How far a patch of the drawn picture may be from the same patch of its large preview, in 8-bit
/// codes of luminance: the decoders differ in rounding and the picture is resampled to the screen.
const DRAWN_TOLERANCE: f64 = 10.0;

/// Every frame, in order, for a set of `count` photographs.
pub fn plan(count: usize) -> Plan {
    let develop = |name: &str, step: DevelopStep| Step::new(name, script::Step::Develop(step));
    let mut steps = vec![
        Step::opened("opened"),
        Step::new("select", script::Step::key("g")),
        Step::new(
            "all",
            script::Step::Select(SelectStep::Source("All photographs".into())),
        )
        .status_starts("All photographs"),
        Step::new(
            "first",
            script::Step::Select(SelectStep::Click {
                position: 0,
                shift: false,
                command: false,
            }),
        ),
        develop("open", DevelopStep::Active),
    ];
    for at in 1..count {
        steps.push(develop(&format!("ready-{at}"), DevelopStep::Ready));
        steps.push(develop(
            &format!("next-{at}"),
            DevelopStep::Step(SetStep::Next),
        ));
        steps.push(develop(&format!("settled-{at}"), DevelopStep::Settle));
    }
    steps.extend([
        develop("ready-back", DevelopStep::Ready),
        develop("previous", DevelopStep::Step(SetStep::Previous)),
        develop("settled-back", DevelopStep::Settle),
        develop("cell", DevelopStep::Cell(0)),
        develop("settled-cell", DevelopStep::Settle),
        develop("collapsed", DevelopStep::Strip),
        develop("expanded", DevelopStep::Strip),
    ]);
    Plan::new(steps)
}

/// One request through the runner's own client.
fn ask(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Result<Value> {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("filmstrip-smoke-{method}"),
                method: method.into(),
                params,
                token: None,
            },
        )
        .map_err(|error| format!("{method}: {error}"))?;
    match response.error {
        Some(error) => Err(format!("{method}: {}: {}", error.code, error.message).into()),
        None => Ok(response.result.unwrap_or(Value::Null)),
    }
}

/// Read a job until it ends.
fn settle(owner: &OwnerHandle, client: ClientId, job: &Value) -> Result<Value> {
    let started = std::time::Instant::now();
    loop {
        let read = ask(owner, client, "job.read", json!({ "job_id": job }))?;
        if !matches!(read["status"].as_str(), Some("queued" | "running")) {
            return Ok(read);
        }
        ensure(
            started.elapsed() < std::time::Duration::from_secs(180),
            format!("A job did not end: {read}"),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// `body` through an owner of the runner's own over `catalog`.
fn with_owner<T>(
    catalog: &Path,
    body: impl FnOnce(&OwnerHandle, ClientId) -> Result<T>,
) -> Result<T> {
    let (owner, join) = OwnerHandle::start(catalog)
        .map_err(|error| format!("the core cannot open the catalog: {error}"))?;
    let client = owner.register();
    let outcome = body(&owner, client);
    owner.stop();
    let _ = join.join();
    outcome
}

/// Mean luminance of the middle of the image at `path`, a quarter of its short side either way,
/// decoded independently of the editor.
pub(crate) fn file_luminance(path: &Path) -> Result<f64> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    crate::scenario::pixels::mean_luminance(
        &image,
        (f64::from(width) / 2.0, f64::from(height) / 2.0),
        i64::from(width.min(height) / 4),
    )
}

/// Mean luminance of the middle of the photograph the frame draws, a quarter of its short side
/// either way.
pub(crate) fn drawn_luminance(frame: &Frame) -> Result<f64> {
    let [left, top, right, bottom] = frame.photo()?;
    let (width, height) = (f64::from(right - left), f64::from(bottom - top));
    crate::scenario::pixels::mean_luminance(
        frame.image()?,
        (f64::from(left) + width / 2.0, f64::from(top) + height / 2.0),
        (width.min(height) / 4.0) as i64,
    )
}

/// The generated JPEGs, the darkest, the brightest and the one between by the middle of the
/// picture.
fn distinct(images: &Path) -> Result<Vec<PathBuf>> {
    let manifest = read_json(&images.join("manifest.json"))?;
    let mut measured: Vec<(f64, PathBuf)> = manifest["files"]
        .as_array()
        .ok_or("The image manifest lists no files")?
        .iter()
        .filter_map(|file| file["path"].as_str())
        .map(|path| {
            let path = path.split('/').fold(images.to_path_buf(), |p, c| p.join(c));
            file_luminance(&path).map(|luminance| (luminance, path))
        })
        .collect::<Result<_>>()?;
    measured.sort_by(|a, b| a.0.total_cmp(&b.0));
    ensure(measured.len() >= 3, "Too few generated images")?;
    let middle = measured.len() / 2;
    Ok(vec![
        measured[0].1.clone(),
        measured[middle].1.clone(),
        measured[measured.len() - 1].1.clone(),
    ])
}

/// Make the folder and the catalog, develop the photographs and ask the core for the catalog view.
pub(crate) fn make(generated: &Path, raw: Option<&Path>) -> Result<Value> {
    generate_catalog::run(
        &generated.join("source"),
        &generate_catalog::Options {
            seed: SEED,
            files: None,
            assets: None,
            images: Some(IMAGES),
        },
    )?;
    let folder = generated.join("Filmstrip");
    fs::create_dir_all(&folder)?;
    let mut files = Vec::new();
    for (at, jpeg) in distinct(&generated.join("source").join("images"))?
        .iter()
        .enumerate()
    {
        let name = jpeg.file_name().ok_or("a file")?.to_string_lossy();
        let target = folder.join(format!("{}_{name}", ["A", "B", "C"][at]));
        fs::copy(jpeg, &target)?;
        files.push(target);
    }
    let raw_copy = match raw {
        Some(raw) => {
            let target = folder.join(raw.file_name().ok_or("a RAW file")?);
            fs::copy(raw, &target)?;
            files.push(target.clone());
            Some(target)
        }
        None => None,
    };
    let catalog = generated.join(generate_catalog::CATALOG);
    drop(EditorService::open(&catalog).map_err(|error| format!("a new catalog: {error}"))?);
    with_owner(&catalog, |owner, client| {
        for (index, path) in files.iter().enumerate() {
            let started = ask(
                owner,
                client,
                "pick.develop",
                json!({
                    "targets": {"kind": "paths", "paths": [path]},
                    "into": [],
                    "confirm_removable": true,
                    "mutation": {"request_id": format!("setup-develop-{index}"), "actor": "setup"},
                }),
            )?;
            let settled = settle(owner, client, &started["job_id"])?;
            ensure(
                settled["status"] == "ready" && settled["result"]["failed"] == json!([]),
                format!("The setup could not develop {}: {settled}", path.display()),
            )?;
        }
        let view = ask(
            owner,
            client,
            "browse.view",
            // As Select views the catalog: newest first, ungrouped.
            json!({"source": {"kind": "all-photographs"},
                "sort": {"key": "capture-time", "descending": true}, "grouping": "none"}),
        )?;
        let count = view["count"].as_u64().ok_or("no count")?;
        let rows = ask(
            owner,
            client,
            "browse.rows",
            json!({"from": 0, "count": count}),
        )?;
        let rows: Vec<Value> = rows["rows"]
            .as_array()
            .ok_or("no rows")?
            .iter()
            .map(|row| {
                let raw = raw_copy
                    .as_ref()
                    .is_some_and(|copy| row["path"] == json!(copy.canonicalize().ok()));
                json!({"position": row["position"], "asset_id": row["asset_id"],
                    "file_name": row["file_name"], "kind": row["kind"], "raw": raw})
            })
            .collect();
        Ok(
            json!({"rows": rows, "raw": raw_copy.map(|copy| copy.file_name().map(|name| name.to_string_lossy().into_owned()))}),
        )
    })
}

/// Each photograph's large preview as the cache holds it after the run, rendered from its current
/// entry: its path, size, key and the renderer that drew it.
fn after(generated: &Path, expected: &Value) -> Result<Value> {
    let catalog = generated.join(generate_catalog::CATALOG);
    with_owner(&catalog, |owner, client| {
        let mut previews = serde_json::Map::new();
        for row in expected["rows"].as_array().into_iter().flatten() {
            let asset = &row["asset_id"];
            let params = json!({"item": {"kind": "photo", "asset_id": asset}, "tier": "large"});
            let started = std::time::Instant::now();
            let preview = loop {
                let answer = ask(owner, client, "preview.read", params.clone())?;
                if answer["state"] == "ready" {
                    break answer["preview"].clone();
                }
                settle(owner, client, &answer["job_id"])?;
                ensure(
                    started.elapsed() < std::time::Duration::from_secs(180),
                    format!("{asset}'s large preview was never rendered: {answer}"),
                )?;
            };
            // A rendered tier names the renderer that drew it: the desktop's GPU tile worker, or
            // the reference naming why (this reader's own owner draws with the reference alone).
            ensure(
                matches!(
                    preview["renderer"]["record"].as_str(),
                    Some("gpu" | "reference")
                ),
                format!("{asset}'s large preview names no renderer: {preview}"),
            )?;
            previews.insert(
                asset.as_str().unwrap_or_default().to_owned(),
                json!({"path": preview["path"], "width": preview["width"],
                    "height": preview["height"], "key": preview["key"],
                    "renderer": preview["renderer"]}),
            );
        }
        Ok(json!({"previews": previews}))
    })
}

/// Make the catalog and the core's answers (a replay reads the recorded ones), launch the editor
/// over it, read the previews it left and check the frames.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let generated = run.out().join(GENERATED);
    let expected_file = run.out().join(EXPECTED);
    let after_file = run.out().join(AFTER);
    if let Some(note) = scenario.note {
        run.note(note);
    }
    run.check(|run| {
        ensure(
            sources.len() <= 1,
            "The filmstrip scenario takes at most one RAW --source",
        )?;
        if !run.replaying() {
            fs::create_dir_all(&generated)?;
            write_json(
                &expected_file,
                &make(&generated, sources.first().map(PathBuf::as_path))?,
            )?;
        }
        let expected = read_json(&expected_file)?;
        let count = expected["rows"].as_array().map_or(0, Vec::len);
        ensure(
            count >= 3,
            format!("The catalog view holds {count} photographs"),
        )?;
        run.record(
            "raw",
            match expected["raw"].as_str() {
                Some(name) => json!({"status": "checked", "file": name}),
                None => json!({"status": "pending",
                    "reason": "no --source RAW file was given, so no RAW photograph was moved to"}),
            },
        );
        let plan = plan(count);
        let mut launch = Launch::app()
            .catalog(&generated.join(generate_catalog::CATALOG))
            .script("script.json", plan.script());
        if let Some(window) = scenario.window {
            launch = launch.window(window);
        }
        run.hash(&sources)?;
        let evidence = run.launch(launch)?.dir;
        if !run.replaying() {
            write_json(&after_file, &after(&generated, &expected)?)?;
        }
        let checked = plan.check(&evidence)?;
        (scenario.verify)(run, std::slice::from_ref(&checked))?;
        run.sources_unchanged()?;
        Ok(())
    })
}

fn develop(frame: &Frame) -> &Value {
    &frame.state()["develop"]
}

/// What each photograph's large preview reads at its middle.
fn own_luminances(run: &Run, expected: &Value, after: &Value) -> Result<Vec<f64>> {
    expected["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            let asset = row["asset_id"].as_str().unwrap_or_default();
            let path = after["previews"][asset]["path"]
                .as_str()
                .ok_or_else(|| format!("No large preview of {asset}"))?;
            let path = run
                .recorded(Path::new(path))
                .filter(|recorded| recorded.exists())
                .unwrap_or_else(|| PathBuf::from(path));
            file_luminance(&path)
        })
        .collect()
}

/// The picture a frame draws is photograph `at`'s own: its middle within the tolerance of its large
/// preview's, and nearer it than any other photograph's that differs from it by more than twice the
/// tolerance.
fn own_picture(checks: &mut Checks, frame: &Frame, name: &str, at: usize, own: &[f64]) -> Result {
    let drawn = drawn_luminance(frame)?;
    checks.compare(
        frame,
        &format!("{name}: the picture's middle against photograph {at}'s large preview"),
        drawn,
        own[at],
        Tolerance::Within(DRAWN_TOLERANCE),
    )?;
    for (other, luminance) in own.iter().enumerate() {
        if other != at && (luminance - own[at]).abs() > 2.0 * DRAWN_TOLERANCE {
            checks.compare(
                frame,
                &format!("{name}: nearer photograph {at}'s preview than photograph {other}'s"),
                (luminance - drawn).abs(),
                (own[at] - drawn).abs(),
                Tolerance::Above(0.0),
            )?;
        }
    }
    Ok(())
}

/// A frame after a move to photograph `at`: the set's active photograph is it, and its cached
/// preview, `cached` says, was drawn in the frame after the key, as the surface's own record of the
/// frame it first drew it in says. The capture shows that preview — of its asset, drawn — or, once
/// a small original has opened before the capture, the photograph's render.
fn moved(
    checks: &mut Checks,
    frame: &Frame,
    name: &str,
    at: usize,
    assets: &[Value],
    own: &[f64],
    cached: bool,
) -> Result {
    let state = develop(frame);
    ensure(
        state["set"]["active"] == at,
        format!(
            "{name}: the set's active photograph is {}",
            state["set"]["active"]
        ),
    )?;
    let (preview, timing) = (&state["preview"], &state["timing"]);
    if cached {
        ensure(
            timing["asset_id"] == assets[at]
                && !timing["preview_version"].is_null()
                && timing["presented_after"] == 1,
            format!(
                "{name}: the move's preview was not drawn in the frame after the key: {timing}"
            ),
        )?;
    }
    if preview.is_null() {
        ensure(
            state["document"]["asset_id"] == assets[at],
            format!("{name}: no preview, and {} open", state["document"]),
        )?;
    } else {
        ensure(
            preview["asset_id"] == assets[at]
                && preview["drawn"] == true
                // Before the open answers, nothing is open; after, the photograph at the preview's
                // own entry, until its render replaces the preview.
                && (state["document"].is_null()
                    || (state["document"]["asset_id"] == assets[at]
                        && state["document"]["entry_id"] == preview["entry_id"])),
            format!("{name}: the preview drawn is {preview}, timed {timing}"),
        )?;
        ensure(
            frame.state()["status_bar"]["render"]
                .as_str()
                .is_some_and(|render| render.contains("preview")),
            format!(
                "{name}: the render slot reads {}",
                frame.state()["status_bar"]["render"]
            ),
        )?;
    }
    own_picture(checks, frame, name, at, own)?;
    checks.note(
        frame,
        &format!("{name}: photograph {at}, its own picture"),
        json!({"preview": preview, "timing": state["timing"], "document": state["document"]}),
    );
    Ok(())
}

/// The photograph `at` open, its render on screen and no preview left.
fn open(
    checks: &mut Checks,
    frame: &Frame,
    name: &str,
    at: usize,
    assets: &[Value],
    own: &[f64],
) -> Result {
    let state = develop(frame);
    ensure(
        state["document"]["asset_id"] == assets[at]
            && state["preview"].is_null()
            && state["set"]["active"] == at,
        format!(
            "{name}: {} open, preview {}",
            state["document"], state["preview"]
        ),
    )?;
    own_picture(checks, frame, name, at, own)
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let expected = read_json(&run.out().join(EXPECTED))?;
    let after = read_json(
        &run.recorded(&run.out().join(AFTER))
            .unwrap_or_else(|| run.out().join(AFTER)),
    )?;
    let mut checks = Checks::new();
    let rows = expected["rows"].as_array().ok_or("no rows")?;
    let assets: Vec<Value> = rows.iter().map(|row| row["asset_id"].clone()).collect();
    let own = own_luminances(run, &expected, &after)?;
    let count = assets.len();

    // D over All photographs: Develop on the first, with the view's photographs as the set.
    let opened = launch.at("open")?;
    let state = develop(opened);
    ensure(
        opened.state()["select"]["shown"] == "develop"
            && state["set"]["assets"] == json!(assets)
            && state["set"]["reading"] == false
            && state["filmstrip_shown"] == true
            && state["strip"]["total"] == count,
        format!("open: the set is {}, the view {assets:?}", state["set"]),
    )?;
    open(&mut checks, opened, "open", 0, &assets, &own)?;
    for at in 1..count {
        let ready = launch.at(&format!("ready-{at}"))?;
        ensure(
            develop(ready)["ahead_ready"] == true,
            format!("ready-{at}: {}", develop(ready)["frames"]),
        )?;
        let next = launch.at(&format!("next-{at}"))?;
        moved(
            &mut checks,
            next,
            &format!("next-{at}"),
            at,
            &assets,
            &own,
            true,
        )?;
        open(
            &mut checks,
            launch.at(&format!("settled-{at}"))?,
            &format!("settled-{at}"),
            at,
            &assets,
            &own,
        )?;
    }
    let back = count - 2;
    moved(
        &mut checks,
        launch.at("previous")?,
        "previous",
        back,
        &assets,
        &own,
        true,
    )?;
    open(
        &mut checks,
        launch.at("settled-back")?,
        "settled-back",
        back,
        &assets,
        &own,
    )?;
    // The first photograph is decoded ahead only while it is within two of the one left.
    let cell = launch.at("cell")?;
    let cached = develop(cell)["timing"]["asset_id"] == assets[0]
        && !develop(cell)["timing"]["preview_version"].is_null();
    moved(&mut checks, cell, "cell", 0, &assets, &own, cached)?;
    open(
        &mut checks,
        launch.at("settled-cell")?,
        "settled-cell",
        0,
        &assets,
        &own,
    )?;
    let collapsed = launch.at("collapsed")?;
    ensure(
        develop(collapsed)["filmstrip_shown"] == false && develop(collapsed)["strip"].is_null(),
        format!("collapsed: {}", develop(collapsed)),
    )?;
    let expanded = launch.at("expanded")?;
    ensure(
        develop(expanded)["filmstrip_shown"] == true && develop(expanded)["strip"]["active"] == 0,
        format!("expanded: {}", develop(expanded)),
    )?;
    // The RAW photograph, when one was given, was moved to and drawn as its own.
    if let Some(name) = expected["raw"].as_str() {
        let at = rows
            .iter()
            .position(|row| row["raw"] == true)
            .ok_or_else(|| format!("The view holds no RAW photograph {name}"))?;
        checks.note(
            opened,
            &format!("the RAW photograph {name} is photograph {at} of the set"),
            json!({"position": at}),
        );
    }
    checks.write(
        &launch.evidence,
        "filmstrip",
        json!({"rows": rows, "raw": expected["raw"], "tolerance": DRAWN_TOLERANCE}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filmstrip_plan_is_well_formed() {
        for (count, steps) in [(3, 18), (4, 21)] {
            let plan = plan(count);
            plan.validate().unwrap();
            assert_eq!(plan.len(), steps);
            assert!(plan.scripted());
        }
    }
}
