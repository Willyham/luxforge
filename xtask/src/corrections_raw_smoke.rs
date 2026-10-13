//! The `raw-corrections` smoke scenario: the Corrections journeys on a supplied RAW
//! (`docs/design/corrections.md`, "Acceptance and verification"), background-only, with correlated
//! state, log and capture, as the `corrections` scenario walks them on the quadrant JPEG.
//!
//! Launch 1 develops the RAW, enters the mode and adds two automatic Heal spots and a Clone stroke
//! from a set source, asks `/` for a new source, undoes and redoes it, names a version, previews
//! and restores the first entry, undoes the restore, exports the current entry on the GPU and on
//! the reference renderer, and leaves. Launch 2 reopens the catalog, shows the photograph's
//! rendered preview in Select, removes it from the library and puts it back, and returns to
//! Develop with its corrections intact. Launch 3 reopens it again and checks the overlay through
//! the source's lens profile and a Perspective warp against the core's own `render.transform`,
//! then exports the warped entry on the GPU and on the reference renderer. The run then opens the
//! catalog in its own owner, with no GPU, and exports the same entry through `export.plan`,
//! `batch.export` and `export.write`, read with `job.read`. Launches 6 to 8 are the Locate journey
//! on a copy of the RAW, as the `corrections` scenario's.
//!
//! What the pixels prove: a Clone copies its source's developed pixels, so the committed Clone's
//! destination reads what the source read before it; the history preview before the Clone reads
//! the photograph as it was; each GPU export is within the spatial display limit of the reference
//! renderer's export of the same entry; the batch and single exports of the entry are the same
//! bytes; and every original keeps its hash.
use crate::{
    corrections_smoke::{
        CORRECTIONS_EFFECT, MODULE, SIZE, add, band, correction, field, geometry_launch, key,
        locate, overlay_through_geometry, press, rgb, with,
    },
    resolve_missing_smoke::{settle, with_owner},
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance},
    smoke::{Scenario, launch_planned_then},
    *,
};
use luxforge_core::{ApiRequest, ClientId, OwnerHandle};
use luxforge_evidence::{
    self as script, CatalogStep, CorrectionStep, ExportFile, ExportStep, PaletteStep,
    PressModifier, PreviewStep, SelectStep, SelectWorkspace,
};
use luxforge_reference::preview_error::{self, Class, Rgb8};

pub const SCENARIO: &str = "raw-corrections";
pub const NOTE: &str = "Six launches over a supplied RAW. The first develops it and walks automatic Heal spots, a set-source Clone stroke, `/`, undo, redo, a version, a history preview, Restore and its undo, and exports on the GPU and the reference renderer; the second reopens its catalog, shows the rendered preview in Select, removes and puts back the photograph and returns to Develop; the third reopens it and checks the overlay through the lens profile and a Perspective warp, then exports again. The run then exports the same entry in its own owner through export.plan, batch.export and export.write. The fourth to sixth (launch6 to launch8) are the Locate journey over a copy of the RAW the run makes in locate/original.";

/// Where things are, in normalized content coordinates.
const SPOTS: [[f64; 2]; 2] = [[0.30, 0.35], [0.45, 0.62]];
/// The `⌥`-click that sets the source, and the Clone stroke painted from it.
const SOURCE: [f64; 2] = [0.55, 0.80];
const STROKE: [[f64; 2]; 2] = [[0.62, 0.40], [0.72, 0.42]];
/// A patch on the Clone's path, inside its full coverage, and where its source is.
const CLONED: [f64; 2] = [0.64, 0.404];
const CLONED_FROM: [f64; 2] = [
    CLONED[0] + SOURCE[0] - STROKE[0][0],
    CLONED[1] + SOURCE[1] - STROKE[0][1],
];
/// How close the Clone's patch must read to its source's, per channel, in 8-bit codes of the
/// capture: the same developed pixels, read from two places of one Fit rendering.
const SAME: f64 = 12.0;

/// The geometries launch 3 checks the overlay through, each on top of the last.
const GEOMETRIES: [&str; 3] = ["plain", "lens", "perspective"];

const GPU_EXPORT: &str = "raw-corrections.jpg";
const REFERENCE_EXPORT: &str = "raw-corrections-reference.jpg";
const GPU_WARPED: &str = "raw-corrections-warped.jpg";
const REFERENCE_WARPED: &str = "raw-corrections-warped-reference.jpg";

fn export(name: &str, reference: bool) -> script::Step {
    ExportStep::File(ExportFile {
        name: name.into(),
        keep_metadata: false,
        reference,
        background: false,
    })
    .into()
}

/// Launch 1: the journeys on the developed RAW.
pub fn launch1(_: &[PathBuf]) -> Plan {
    let mut painted = press(STROKE[0]);
    painted.to = STROKE[1..].to_vec();
    Plan::new(vec![
        Step::opened("opened")
            .no_layer(CORRECTIONS_EFFECT)
            .no_draft(),
        Step::new("asset", script::Step::api("asset.state")).commits(0),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
        Step::new("size", field("radius", SIZE)).commits(0),
        Step::new("spot-1", add(SPOTS[0]))
            .commits(1)
            .label("Add Heal 1")
            .status_starts("Added Heal 1 \u{b7} source picked"),
        Step::new("spot-2", add(SPOTS[1]))
            .commits(1)
            .label("Add Heal 2"),
        Step::new("source-set", with(SOURCE, PressModifier::Alt))
            .commits(0)
            .status_starts("Source set"),
        Step::new(
            "clone-kind",
            correction(CorrectionStep::Kind("clone".into())),
        )
        .commits(0),
        Step::new("cloned", correction(CorrectionStep::Press(painted)))
            .commits(1)
            .label("Add Clone 3"),
        Step::new("new-source", key("/"))
            .commits(1)
            .label("New source Clone 3"),
        Step::new("undo", script::Step::api("history.undo"))
            .commits(1)
            .label("Add Clone 3"),
        Step::new("redo", script::Step::api("history.redo"))
            .commits(1)
            .label("New source Clone 3"),
        Step::new(
            "version",
            script::Step::call("version.create", json!({"name": "Corrected"})),
        ),
        Step::new("versions", script::Step::api("version.list")),
        // The first entry previewed and restored, then the restore undone.
        Step::new("history-first", PreviewStep::Sequence(1)).commits(0),
        Step::new(
            "restored",
            script::Step::Palette(PaletteStep::Run("Restore".into())),
        )
        .commits(1)
        .label("Restore entry 1"),
        Step::new("restore-undone", script::Step::api("history.undo"))
            .commits(1)
            .label("New source Clone 3"),
        Step::new("planned", script::Step::api("export.plan")).commits(0),
        Step::new("exported", export(GPU_EXPORT, false)).commits(0),
        Step::new("exported-reference", export(REFERENCE_EXPORT, true)).commits(0),
        Step::new("leave", key("q")).commits(0).mode("pointer"),
    ])
}

/// Launch 2: the reopened catalog, the rendered preview in Select, Remove and Put back, and the
/// corrections back in Develop.
pub fn launch2(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("reopened").label("New source Clone 3"),
        Step::new("select", SelectStep::Switch(SelectWorkspace::Select)).commits(0),
        Step::new("all", SelectStep::Source("All photographs".into())).commits(0),
        Step::new(
            "selected",
            SelectStep::Click {
                position: 0,
                shift: false,
                command: false,
            },
        )
        .commits(0),
        Step::new("remove-asked", script::Step::Catalog(CatalogStep::Remove)).commits(0),
        Step::new("removed", script::Step::Catalog(CatalogStep::Confirm))
            .commits(0)
            .status_starts("Removed "),
        Step::new("removed-view", SelectStep::Source("Removed".into())).commits(0),
        Step::new(
            "removed-selected",
            SelectStep::Click {
                position: 0,
                shift: false,
                command: false,
            },
        )
        .commits(0),
        Step::new("put-back", script::Step::Catalog(CatalogStep::PutBack))
            .commits(0)
            .status_starts("Put back "),
        Step::new("develop", SelectStep::Switch(SelectWorkspace::Develop)).commits(0),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
    ])
}

/// Launch 3: the overlay through the lens profile and a Perspective warp, then the warped entry
/// exported on the GPU and on the reference renderer.
pub fn launch3(_: &[PathBuf]) -> Plan {
    geometry_launch(
        "New source Clone 3",
        &GEOMETRIES,
        // Heal 1, the bottom of the three: a spot, whose outline is a circle the map warps.
        2,
        vec![
            Step::new("warped", export(GPU_WARPED, false)).commits(0),
            Step::new("warped-reference", export(REFERENCE_WARPED, true)).commits(0),
        ],
    )
}

/// Launch 6: a copy of the RAW developed, with one Clone painted from a set source.
fn locate_clone() -> Plan {
    let mut painted = press(STROKE[0]);
    painted.to = STROKE[1..].to_vec();
    Plan::new(vec![
        Step::opened("opened")
            .no_layer(CORRECTIONS_EFFECT)
            .no_draft(),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
        Step::new("size", field("radius", SIZE)).commits(0),
        Step::new("source-set", with(SOURCE, PressModifier::Alt))
            .commits(0)
            .status_starts("Source set"),
        Step::new(
            "clone-kind",
            correction(CorrectionStep::Kind("clone".into())),
        )
        .commits(0),
        Step::new("cloned", correction(CorrectionStep::Press(painted)))
            .commits(1)
            .label("Add Clone 1"),
    ])
}

/// One owner request; a source that must be prepared first is prepared through the job the
/// refusal names, then asked again.
fn prepared(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Result<Value> {
    for attempt in 0..4 {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: format!("raw-corrections-{method}-{attempt}"),
                    method: method.into(),
                    params: params.clone(),
                    token: None,
                },
            )
            .map_err(|error| format!("{method}: {error}"))?;
        match response.error {
            None => return Ok(response.result.unwrap_or(Value::Null)),
            Some(error) if error.code == "preparation-required" => {
                let job = error
                    .job_id
                    .ok_or_else(|| format!("{method}: a preparation named no job"))?;
                let settled = settle(owner, client, &json!(job))?;
                ensure(
                    settled["status"] == "ready",
                    format!("{method}: the preparation ended {settled}"),
                )?;
            }
            Some(error) => {
                return Err(format!("{method}: {}: {}", error.code, error.message).into());
            }
        }
    }
    Err(format!("{method} kept asking for preparation").into())
}

/// The catalog's current entry exported in the run's own owner, which has no GPU: `export.plan`,
/// then `batch.export` and `export.write` of the same entry, each read with `job.read` until it
/// ends. Writes `headless/` with the files and `headless/result.json`.
fn headless_exports(run: &Run, asset: &str) -> Result {
    let dir = run.out().join("headless");
    if run.replaying() {
        return Ok(());
    }
    let batch = dir.join("batch");
    fs::create_dir_all(&batch)?;
    let single = dir.join("single.jpg");
    let catalog = run.out().join("launch1").join("catalog.sqlite");
    let result = with_owner(&catalog, |owner, client| {
        let plan = prepared(
            owner,
            client,
            "export.plan",
            json!({"asset_id": asset, "settings": crate::export_smoke::export_settings(&batch)}),
        )?;
        let started = prepared(
            owner,
            client,
            "batch.export",
            json!({"targets": {"kind": "assets", "asset_ids": [asset]}, "settings": crate::export_smoke::export_settings(&batch),
                "mutation": {"request_id": "raw-corrections-batch", "actor": "smoke"}}),
        )?;
        let batched = settle(owner, client, &started["job_id"])?;
        let started = prepared(
            owner,
            client,
            "export.write",
            json!({"asset_id": asset, "destination": single, "settings": crate::export_smoke::export_settings(&dir),
                "mutation": {"request_id": "raw-corrections-single", "actor": "smoke"}}),
        )?;
        let exported = settle(owner, client, &started["job_id"])?;
        Ok(json!({"plan": plan, "batch": batched, "single": exported}))
    })?;
    write_json(&dir.join("result.json"), &result)
}

/// The run's own launches, the headless exports of the warped entry, then the Locate journey.
pub fn run(run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let source = sources
        .first()
        .cloned()
        .ok_or("The raw-corrections scenario opens one RAW")?;
    launch_planned_then(
        run,
        scenario,
        sources,
        |_, sources| {
            Ok(scenario
                .launches
                .iter()
                .map(|spec| (spec.plan)(sources))
                .collect())
        },
        |run, checked| {
            let asset = checked
                .first()
                .ok_or("No first launch")?
                .at("asset")?["step"]["result"]["asset"]["id"]
                .as_str()
                .ok_or("asset.state answered no asset id")?
                .to_owned();
            headless_exports(run, &asset)?;
            locate(run, scenario, &source, checked, locate_clone())
        },
    )
}

/// Each channel of `read` within `tolerance` of `expected`, recorded.
fn same(
    checks: &mut Checks,
    frame: &Frame,
    what: &str,
    read: [f64; 3],
    expected: [f64; 3],
    tolerance: f64,
) -> Result {
    for (channel, (read, expected)) in ["red", "green", "blue"]
        .iter()
        .zip(read.iter().zip(expected))
    {
        checks.compare(
            frame,
            &format!("{what}, {channel}"),
            *read,
            expected,
            Tolerance::Within(tolerance),
        )?;
    }
    Ok(())
}

/// A JPEG decoded to 8-bit RGB.
fn decoded(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let image = image::open(path)
        .map_err(|error| format!("{} does not decode: {error}", path.display()))?
        .to_rgb8();
    Ok((image.width(), image.height(), image.into_raw()))
}

/// `gpu` within the spatial display limit of `reference`, both decoded here, over every pixel.
fn within_the_reference(
    checks: &mut Checks,
    frame: &Frame,
    what: &str,
    gpu: &Path,
    reference: &Path,
) -> Result {
    let (a, b) = (decoded(gpu)?, decoded(reference)?);
    ensure(
        (a.0, a.1) == (b.0, b.1),
        format!(
            "{what}: the exports differ in size, {}x{} and {}x{}",
            a.0, a.1, b.0, b.1
        ),
    )?;
    let statistics = preview_error::compare(
        Rgb8::new(a.0, a.1, &a.2)?,
        Rgb8::new(b.0, b.1, &b.2)?,
        [0, 0, a.0, a.1],
    )?;
    let figures = json!({
        "pixels": statistics.pixels, "mean": statistics.mean,
        "worst_block": statistics.worst_block, "p99": statistics.p99,
        "mean_delta_l": statistics.mean_delta_l, "max": statistics.max,
        "size": [a.0, a.1],
    });
    ensure(
        preview_error::verdict(&statistics, Class::Spatial).passed(),
        format!("{what}: past the spatial display limit of the reference export: {figures}"),
    )?;
    let limits = Class::Spatial.limits();
    checks.note(
        frame,
        &format!("{what}: within the spatial display limit of the reference export"),
        json!({
            "files": [gpu.file_name().map(|name| name.to_string_lossy()), reference.file_name().map(|name| name.to_string_lossy())],
            "statistics": figures,
            "limits": {"mean": limits.mean, "worst_block": limits.worst_block, "p99": limits.p99,
                "mean_delta_l_abs": limits.mean_delta_l},
        }),
    );
    Ok(())
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [launch1, launch2, launch3, launch6, launch7, launch8] = launches else {
        return Err(format!("Expected six launches, found {}", launches.len()).into());
    };
    let mut checks = Checks::new();
    let opened = launch1.at("opened")?;
    // A Clone copies its source's developed pixels.
    let cloned = launch1.at("cloned")?;
    same(
        &mut checks,
        cloned,
        "the committed Clone reads its source as it read before",
        rgb(cloned, CLONED)?,
        rgb(opened, CLONED_FROM)?,
        SAME,
    )?;
    ensure(
        band(cloned)["count"] == json!(3),
        format!("The band after the Clone: {}", band(cloned)),
    )?;
    // The history preview before the Clone shows the photograph as it was there.
    let first = launch1.at("history-first")?;
    same(
        &mut checks,
        first,
        "the first entry previewed, where the Clone goes",
        rgb(first, CLONED)?,
        rgb(opened, CLONED)?,
        SAME,
    )?;
    let restored = launch1.at("restored")?;
    same(
        &mut checks,
        restored,
        "the first entry restored, where the Clone was",
        rgb(restored, CLONED)?,
        rgb(opened, CLONED)?,
        SAME,
    )?;
    let undone = launch1.at("restore-undone")?;
    ensure(
        band(undone)["count"] == json!(3),
        format!("The corrections after the restore's undo: {}", band(undone)),
    )?;
    // The version names the redone entry.
    let versions = &launch1.at("versions")?["step"]["result"];
    ensure(
        versions.to_string().contains("Corrected"),
        format!("version.list does not name the version: {versions}"),
    )?;
    // Nothing is stale on an offline Clone/Heal stack: where export.plan reports staleness (the
    // frozen-patch task adds it), it reports none.
    let planned = &launch1.at("planned")?["step"]["result"];
    ensure(
        planned["entry_id"].is_string()
            && (planned.get("stale").is_none() || planned["stale"] == json!([])),
        format!("export.plan: {planned}"),
    )?;
    let out = run.out().to_path_buf();
    within_the_reference(
        &mut checks,
        launch1.at("exported-reference")?,
        "the GPU's export of the corrected RAW",
        &out.join("launch1").join(GPU_EXPORT),
        &out.join("launch1").join(REFERENCE_EXPORT),
    )?;
    // Launch 2: reopened, removed, put back, and back in Develop.
    let back = launch2.at("mode")?;
    ensure(
        band(back)["count"] == json!(3),
        format!("The reopened photograph's corrections: {}", band(back)),
    )?;
    same(
        &mut checks,
        back,
        "the Clone after reopen, Remove and Put back",
        rgb(back, CLONED)?,
        rgb(undone, CLONED)?,
        SAME,
    )?;
    // Launch 3: the overlay through the lens profile and the warp, and the warped exports.
    overlay_through_geometry(&mut checks, launch3, &GEOMETRIES, 3)?;
    within_the_reference(
        &mut checks,
        launch3.at("warped-reference")?,
        "the GPU's export of the corrected RAW through its lens profile and a Perspective warp",
        &out.join("launch3").join(GPU_WARPED),
        &out.join("launch3").join(REFERENCE_WARPED),
    )?;
    // The run's own owner: export.plan, batch.export and export.write of the warped entry.
    let headless = read_json(&out.join("headless").join("result.json"))?;
    let plan = &headless["plan"];
    ensure(
        plan["entry_id"].is_string() && (plan.get("stale").is_none() || plan["stale"] == json!([])),
        format!("export.plan in the run's owner: {plan}"),
    )?;
    for job in ["batch", "single"] {
        ensure(
            headless[job]["status"] == "ready",
            format!("The {job} export ended {}", headless[job]),
        )?;
    }
    let written = headless["batch"]["result"]["written"][0]["path"]
        .as_str()
        .ok_or_else(|| format!("batch.export wrote nothing: {}", headless["batch"]))?;
    let single = out.join("headless").join("single.jpg");
    ensure(
        fs::read(written)? == fs::read(&single)?,
        "The batch export is not the single export's bytes",
    )?;
    let warped = launch3.at("warped")?;
    within_the_reference(
        &mut checks,
        warped,
        "the desktop GPU's export against the run's own batch export",
        &out.join("launch3").join(GPU_WARPED),
        Path::new(written),
    )?;
    checks.note(
        warped,
        "the run's own owner exported the warped entry",
        json!({"batch": headless["batch"]["result"], "single": headless["single"]["result"],
            "reference_pixels_identical": decoded(Path::new(written))? == decoded(&out.join("launch3").join(REFERENCE_WARPED))?}),
    );
    // Locate: the copy moved, Select located it, and the Clone is intact and drawn.
    let made = launch6.at("cloned")?;
    let before = launch6.at("opened")?;
    same(
        &mut checks,
        made,
        "the Clone before the original moves",
        rgb(made, CLONED)?,
        rgb(before, CLONED_FROM)?,
        SAME,
    )?;
    ensure(
        launch7.at("missing")?.state()["missing"]["count"] == json!(1)
            && launch7.at("located")?.state()["missing"]["count"] == json!(0),
        "Locate did not resolve the one missing original",
    )?;
    let paths = read_json(&out.join("locate").join("paths.json"))?;
    let moved = PathBuf::from(paths["moved"].as_str().ok_or("No moved path")?);
    let source = run.sources().first().ok_or("The run opens one RAW")?;
    ensure(
        hash(&moved)? == hash(source)?,
        "The moved original is not the RAW's bytes",
    )?;
    let reopened = launch8.at("opened")?;
    same(
        &mut checks,
        reopened,
        "the Clone rendered after Locate",
        rgb(reopened, CLONED)?,
        rgb(made, CLONED)?,
        SAME,
    )?;
    ensure(
        band(launch8.at("mode")?)["count"] == json!(1),
        format!(
            "The corrections after Locate: {}",
            band(launch8.at("mode")?)
        ),
    )?;
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "same_codes": SAME,
            "scope": "Mean 8-bit RGB of small patches of the captured photograph at Fit: a Clone's destination against its source read before it, history and restore against the photograph as opened, reopen and Locate against the frame before. Exports decoded here: each GPU export against the reference renderer's export of the same entry by the spatial display limit (CIEDE2000, luxforge_reference::preview_error) over every pixel, and the batch and single exports of the run's own owner byte for byte. Overlay placement as the corrections scenario reads it. The original's hash is checked unchanged by the run",
        }),
    )
}
