//! The `panorama-merge` smoke scenario: overlapping frames merged into panoramas in Select, one
//! through the preview sheet and one by ⌥-click, shown as collapsed tiles, removed and put back,
//! both developed and the one past the resident source limits exported, and both reopened with
//! their frames moved away, in the real editor at 1440 × 900 ([panorama
//! design](../../docs/design/panorama-merge.md), "Evidence and acceptance", item 5).
//!
//! The run first writes two rows of JPEG frames of one synthetic scene (`luxforge_reference::
//! panorama`), each frame the scene seen by a level camera turned about its centre: `Frames/Row`,
//! four 1536 × 1024 frames 30° apart with a 60° field (about 3 MP stitched), and `Frames/Wide`, ten
//! 6000 × 4000 frames 26° apart with a 40° field, which stitch past the resident limits (about
//! 150 MP, so its result is a paged source). It makes a new catalog with `Frames` indexed and
//! nothing merged (`panorama-merge-expected.json`).
//!
//! The `select` launch, in [`select_plan`] order: `G`; the Row's folder, its four frames selected
//! and the selection menu opened over them, both kinds planned; Merge panorama… opening the
//! preview sheet; Boundary Warp dragged to [`WARP`]; Frame outlines and Crop and coverage turned
//! off and on again; Merge, its pending row, and one collapsed panorama tile; `S` expanding its
//! originals, each "Frame k of 4"; an original clicked, then the result, its Info band reading the
//! projection and field of view; Remove merge (`⌫`), `Cmd+Z`, `Shift+Cmd+Z` and `Cmd+Z`; then the
//! Wide's folder, its ten frames selected, the menu opened and Merge panorama ⌥-clicked, merging
//! at once with the remembered settings, its pending row, and one collapsed tile of ten.
//!
//! The catalog that launch left is read (`panorama-merge-after-select.json`): where each result is
//! filed. The `develop` launch, in [`develop_plan`] order, opens the Row result from its catalog
//! folder in Develop: Fit, 100% and the bottom-right corner, Fit again, a linear mask and Exposure
//! through it, and `merge.inspect`; then the paged Wide result the same way, at Fit counting its
//! overview and at 100% from a window of level 0, and Develop's export of it, above 128 MP, on the
//! GPU. The catalog is read again (`panorama-merge-after-develop.json`), `Frames` renamed so no
//! frame can be read, and the `offline` launch, in [`offline_plan`] order, opens the Row result
//! with its masked edit and inspects it, then the Wide result with its own. The folder is named
//! back, the catalog read again (`panorama-merge-after.json`) and every frame's hash checked.
//!
//! Develop opens a photograph from a catalog folder, which the merges choose as they run, so the
//! results are developed in a launch of their own, from the folders the catalog names; each launch
//! also stays within the editor's bound on a scripted journey.
use crate::{
    generate_catalog,
    hdr_merge_smoke::{ask, with_owner},
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step, Tolerance},
    smoke::Scenario,
    *,
};
use luxforge_core::{BASIC_EFFECT, ClientId, EditorService, OwnerHandle};
use luxforge_evidence::{
    self as script, CatalogStep, DevelopStep, ExportFile, ExportStep, LibraryKey, MergeChoice,
    MergePreviewStep, SelectStep, ViewStep,
};
use luxforge_reference::{
    panorama::{Lens, Pose, Scene, apply, row},
    srgb,
};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

pub const SCENARIO: &str = "panorama-merge";
/// What `reproduce.md` says the run does before, between and after its launches.
pub const NOTE: &str = "The run first writes two rows of JPEG frames of one synthetic scene \
    (`luxforge_reference::panorama`, a level camera turned about its centre): \
    `generated/Frames/Row`, four 1536 × 1024 frames 30° apart with a 60° field, and \
    `generated/Frames/Wide`, ten 6000 × 4000 frames 26° apart with a 40° field, which stitch past \
    128 MP. It makes `generated/catalog.sqlite` with `Frames` indexed and nothing merged \
    (`panorama-merge-expected.json`). Each launch opens that catalog with `--catalog`. The `select` \
    launch merges both rows; the run then reads where the catalog filed them \
    (`panorama-merge-after-select.json`). The `develop` launch opens each from its catalog folder \
    and exports the Wide result into its evidence directory (`wide-panorama.jpg`); the run reads \
    the catalog again (`panorama-merge-after-develop.json`), renames `Frames` to `Frames offline`, \
    and the `offline` launch reopens both; the folder is named back and the catalog read again \
    (`panorama-merge-after.json`).";
/// Where the run writes its frames and catalog.
pub const GENERATED: &str = "generated";
pub const EXPECTED: &str = "panorama-merge-expected.json";
pub const AFTER_SELECT: &str = "panorama-merge-after-select.json";
pub const AFTER_DEVELOP: &str = "panorama-merge-after-develop.json";
pub const AFTER: &str = "panorama-merge-after.json";
const EXPORT_FILE: &str = "wide-panorama.jpg";
const FRAMES: &str = "Frames";
const OFFLINE: &str = "Frames offline";

/// One row of frames: a level camera turned `step_deg` between frames, each `width × height` with
/// a horizontal field of `hfov_deg`, over the scene of `seed`.
#[derive(Clone, Copy, Debug)]
struct Set {
    name: &'static str,
    prefix: &'static str,
    frames: usize,
    width: usize,
    height: usize,
    hfov_deg: f64,
    step_deg: f64,
    seed: u64,
}

/// Merged through the sheet and developed: a resident result of about 3 MP, cylindrical.
const ROW: Set = Set {
    name: "Row",
    prefix: "ROW",
    frames: 4,
    width: 1536,
    height: 1024,
    hfov_deg: 60.0,
    step_deg: 30.0,
    seed: 41,
};
/// Merged by ⌥-click and exported: 274° at an 8,242-pixel focal length, about 39,400 × 3,900
/// stitched, past the 128 MP a resident source may hold.
const WIDE: Set = Set {
    name: "Wide",
    prefix: "WIDE",
    frames: 10,
    width: 6000,
    height: 4000,
    hfov_deg: 40.0,
    step_deg: 26.0,
    seed: 43,
};
/// The scene's cell in radians: about 53 pixels in a Row frame and 82 in a Wide frame's analysis
/// proxy, which registration's coarser pyramid levels see at 13 to 27 pixels, as the core's own
/// end-to-end frames are.
const CELL: f64 = 0.04;
/// The frames' exposure of the scene, keeping its brightest boxes below the JPEG's white.
const GAIN: f64 = 0.5;
const QUALITY: u8 = 95;
/// The Boundary Warp the sheet's merge sets, which ⌥-click then merges with as the last settings.
const WARP: u8 = 60;
/// The largest source a resident derived source may hold: past it a result is paged.
const RESIDENT_PIXELS: u64 = 128_000_000;
/// How long the setup waits for a job (the folder's indexing; a test's previews), and how long the
/// launches that merge and develop the Wide row may take: the editor ends a scripted journey after
/// five minutes.
const SETUP_DEADLINE: Duration = Duration::from_secs(600);
const LAUNCH_DEADLINE: Duration = Duration::from_secs(420);
/// The linear mask's ends in content coordinates, coverage 0 above `y0` and 1 below `y1`, and the
/// Exposure through it.
const GRADIENT: [f64; 4] = [0.5, 0.3, 0.5, 0.7];
const EXPOSURE: f64 = 1.0;
const MASKED_LABEL: &str = "Mask 1 \u{b7} Exposure +1.00 EV";
/// Where the mask's effect is read, as fractions of the drawn photograph: clear of the gradient
/// above it and fully covered below it; and each patch's half side, a fraction of its height.
const UNCOVERED: [f64; 2] = [0.5, 0.12];
const COVERED: [f64; 2] = [0.5, 0.88];
const PATCH: f64 = 0.05;
/// One stop through the mask lifts the covered patch by at least this many 8-bit codes, and leaves
/// the uncovered one within the other.
const LIFT: f64 = 10.0;
const UNTOUCHED: f64 = 2.0;
/// How far the reopened result's covered patch may be from the edited one's: the same saved source
/// and edit at the same size by the same renderer.
const REOPEN_TOLERANCE: f64 = 3.0;
/// The words a merge's pending row shows for each stage of its job.
const STAGES: [&str; 4] = ["Reading ", "Matching", "Stitching ", "Saving"];

/// Where a set's result is filed, as the sources panel nests its folder and its place there.
fn filed(places: &Value, set: &Set, whose: &str) -> Result<(String, u32)> {
    let place = &places[set.name];
    let path = place["path"]
        .as_str()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| format!("{whose} name no catalog folder for the {} result", set.name))?;
    let position = place["position"]
        .as_u64()
        .and_then(|position| u32::try_from(position).ok())
        .ok_or_else(|| format!("{whose} place no {} result", set.name))?;
    Ok((path.to_owned(), position))
}

fn click(name: &str, position: u32) -> Step {
    Step::new(
        name,
        script::Step::Select(SelectStep::Click {
            position,
            shift: false,
            command: false,
        }),
    )
}

/// A Select gesture.
fn in_select(name: &str, step: SelectStep) -> Step {
    Step::new(name, script::Step::Select(step))
}

fn key(name: &str, key: &str) -> Step {
    Step::new(name, script::Step::key(key))
}

fn sheet(name: &str, step: MergePreviewStep) -> Step {
    in_select(name, SelectStep::MergePreview(step))
}

/// A set's folder on disk browsed, its frames selected from the first to the last, and the
/// selection's menu opened over the last. Each step's name begins with `prefix`.
fn selected(prefix: &str, folder: String, set: &Set) -> Vec<Step> {
    let name = |step: &str| format!("{prefix}{step}");
    let last = set.frames as u32 - 1;
    vec![
        in_select(&name("folder"), SelectStep::Folder(folder)),
        click(&name("first"), 0),
        in_select(
            &name("selected"),
            SelectStep::Click {
                position: last,
                shift: true,
                command: false,
            },
        ),
        in_select(&name("menu"), SelectStep::MergeMenu(last)),
    ]
}

/// The result clicked in its catalog folder, pressed first when it is given, and opened in
/// Develop; at Fit, at 100% in its
/// bottom-right corner and at Fit again; then a linear mask and one stop of Exposure through it.
/// Each step's name begins with `prefix`.
fn developed(prefix: &str, path: Option<String>, position: u32) -> Vec<Step> {
    let name = |step: &str| format!("{prefix}{step}");
    let place = path.map(|path| {
        Step::new(
            name("place"),
            script::Step::Catalog(CatalogStep::Source(path)),
        )
    });
    place
        .into_iter()
        .chain([
        click(&name("result"), position),
        Step::new(name("develop"), script::Step::Develop(DevelopStep::Active)),
        Step::new(name("fit"), ViewStep::Fit).commits(0).fit(),
        Step::new(name("percent"), ViewStep::Percent(100.0))
            .commits(0)
            .percent(100.0),
        Step::new(name("corner"), script::Step::Pan { x: 1.0, y: 1.0 })
            .commits(0)
            .percent(100.0),
        Step::new(name("refit"), ViewStep::Fit).commits(0).fit(),
        Step::new(
            name("masked"),
            script::Step::call(
                "mask.create-linear",
                json!({"x0": GRADIENT[0], "y0": GRADIENT[1], "x1": GRADIENT[2], "y1": GRADIENT[3]}),
            ),
        )
        .commits(1)
        .masks(1),
        Step::new(
            name("lifted"),
            script::Step::call(
                "edit.set-basic",
                json!({"mask": {"name": "Mask 1"}, "exposure": EXPOSURE}),
            ),
        )
        .commits(1)
        .label(MASKED_LABEL)
        .payload(BASIC_EFFECT, json!({ "exposure": EXPOSURE }))
        .fit(),
        ])
        .collect()
}

/// A result opened in Develop as the `develop` launch left it: its masked edit, at Fit.
fn reopened(name: &str) -> Step {
    Step::new(name, script::Step::Develop(DevelopStep::Active))
        .payload(BASIC_EFFECT, json!({ "exposure": EXPOSURE }))
        .masks(1)
        .fit()
}

/// The `select` launch's frames: the Row merged through the preview sheet, its stack and its
/// removal and undo; then the Wide merged by ⌥-click.
pub fn select_plan(expected: &Value) -> Result<Plan> {
    let folder = |set: &Set| -> Result<String> {
        expected["sets"][set.name]["folder"]
            .as_str()
            .filter(|folder| !folder.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("The expected answers name no {} folder", set.name).into())
    };
    let mut steps = vec![Step::opened("opened"), key("select", "g")];
    steps.extend(selected("row_", folder(&ROW)?, &ROW));
    steps.extend([
        in_select(
            "previewing",
            SelectStep::MergeChoose {
                kind: MergeChoice::Panorama,
                last_settings: false,
            },
        ),
        sheet("warped", MergePreviewStep::BoundaryWarp(WARP)),
        sheet("plain", MergePreviewStep::FrameOutlines(false)),
        sheet("uncropped", MergePreviewStep::CropCoverage(false)),
        sheet("outlined", MergePreviewStep::FrameOutlines(true)),
        sheet("overlaid", MergePreviewStep::CropCoverage(true)),
        sheet("merging", MergePreviewStep::Merge),
        in_select("merged", SelectStep::Merged),
        key("expanded", "s"),
        click("original", 2),
        click("clicked", 0),
        Step::new("removed", script::Step::Catalog(CatalogStep::DeleteKey)),
        in_select("restored", SelectStep::Library(LibraryKey::Undo)),
        in_select("redone", SelectStep::Library(LibraryKey::Redo)),
        in_select("undone", SelectStep::Library(LibraryKey::Undo)),
    ]);
    steps.extend(selected("wide_", folder(&WIDE)?, &WIDE));
    steps.extend([
        in_select(
            "wide_merging",
            SelectStep::MergeChoose {
                kind: MergeChoice::Panorama,
                last_settings: true,
            },
        ),
        in_select("wide_merged", SelectStep::Merged),
    ]);
    Ok(Plan::new(steps))
}

/// The `develop` launch's frames, over the catalog the `select` launch left: the Row result in
/// Develop at Fit, at 100% in a corner and through a mask, and its merge inspected; then the Wide's
/// paged result the same way, and Develop's export of it.
pub fn develop_plan(after: &Value) -> Result<Plan> {
    let places = &after["places"];
    let whose = "The select launch's catalog";
    let (row_path, row_position) = filed(places, &ROW, whose)?;
    let (wide_path, wide_position) = filed(places, &WIDE, whose)?;
    let mut steps = vec![Step::opened("opened"), key("select", "g")];
    // Select shows the Row result's folder again; the Wide's is pressed only when it is another.
    let wide_path = (wide_path != row_path).then_some(wide_path);
    steps.extend(developed("", Some(row_path), row_position));
    steps.extend([
        Step::new("inspected", script::Step::call("merge.inspect", json!({}))),
        key("back", "g"),
    ]);
    steps.extend(developed("wide_", wide_path, wide_position));
    steps.push(
        Step::new(
            "wide_exported",
            ExportStep::File(ExportFile {
                name: EXPORT_FILE.into(),
                keep_metadata: false,
                reference: false,
                background: false,
            }),
        )
        .commits(0),
    );
    Ok(Plan::new(steps))
}

/// The `offline` launch's frames, over the catalog the `develop` launch left with the frames moved
/// away: the Row result's catalog folder, the result opened in Develop with its masked edit and its
/// merge inspected; then the Wide result opened with its own.
pub fn offline_plan(after: &Value) -> Result<Plan> {
    let places = &after["places"];
    let whose = "The develop launch's catalog";
    let (row_path, row_position) = filed(places, &ROW, whose)?;
    let (wide_path, wide_position) = filed(places, &WIDE, whose)?;
    let mut steps = vec![
        Step::opened("opened"),
        key("select", "g"),
        Step::new(
            "place",
            script::Step::Catalog(CatalogStep::Source(row_path.clone())),
        ),
        click("result", row_position),
        reopened("reopened"),
        Step::new("inspected", script::Step::call("merge.inspect", json!({}))),
        key("back", "g"),
    ];
    // Select shows the Row result's folder again; the Wide's is pressed only when it is another.
    if wide_path != row_path {
        steps.push(Step::new(
            "wide_place",
            script::Step::Catalog(CatalogStep::Source(wide_path)),
        ));
    }
    steps.extend([
        click("wide_result", wide_position),
        reopened("wide_reopened"),
    ]);
    Ok(Plan::new(steps))
}

// The frames.

/// `set`'s frames recorded into `dir`, left to right, each an sRGB JPEG with no metadata.
fn write_set(dir: &Path, set: &Set) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dir)?;
    let scene = Scene::new(set.seed, CELL);
    let lens = Lens::with_hfov(set.width, set.height, set.hfov_deg);
    row(set.frames, set.step_deg, 0.0)
        .iter()
        .enumerate()
        .map(|(at, pose)| {
            let path = dir.join(format!("{}_{:04}.JPG", set.prefix, at + 1));
            let picture = image::RgbImage::from_raw(
                set.width as u32,
                set.height as u32,
                record(&scene, &lens, pose),
            )
            .ok_or("a frame's pixels do not fill it")?;
            image::codecs::jpeg::JpegEncoder::new_with_quality(fs::File::create(&path)?, QUALITY)
                .encode_image(&picture)?;
            Ok(path)
        })
        .collect()
}

/// The scene as `lens` turned to `pose` records it, in 8-bit sRGB, row-major: each pixel samples
/// the scene along the direction its centre shows, times [`GAIN`]. Bands of rows are recorded on
/// every core at once; the result does not depend on how.
fn record(scene: &Scene, lens: &Lens, pose: &Pose) -> Vec<u8> {
    let to_world = pose.camera_to_world();
    let width = lens.width;
    let mut rgb = vec![0u8; width * lens.height * 3];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let rows = lens.height.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (band, pixels) in rgb.chunks_mut(rows * width * 3).enumerate() {
            let to_world = &to_world;
            scope.spawn(move || {
                for (at, pixel) in pixels.chunks_exact_mut(3).enumerate() {
                    let index = band * rows * width + at;
                    let (x, y) = ((index % width) as f64, (index / width) as f64);
                    let value = scene.radiance(apply(to_world, lens.ray((x, y))));
                    for (code, linear) in pixel.iter_mut().zip(value) {
                        *code = srgb::code(linear * GAIN);
                    }
                }
            });
        }
    });
    rgb
}

/// The stitched size a row of `set`'s frames makes at full scale, cylindrical and uncropped
/// sideways: its turn at the focal length across, and its height where two frames meet, half a
/// step from each centre, down. Within a few per cent of the stitch's own crop.
fn stitched_estimate(set: &Set) -> (f64, f64) {
    let lens = Lens::with_hfov(set.width, set.height, set.hfov_deg);
    let turn = (set.hfov_deg + set.step_deg * (set.frames as f64 - 1.0)).to_radians();
    let seam = (set.step_deg / 2.0).to_radians().cos();
    (lens.focal_px * turn, set.height as f64 * seam)
}

// The setup's requests and the catalog's answers.

/// Read a job until it ends ready, keeping each progress message it showed on the way.
fn follow(owner: &OwnerHandle, client: ClientId, job: &Value, deadline: Duration) -> Result<Value> {
    let started = Instant::now();
    let mut progress: Vec<String> = Vec::new();
    loop {
        let read = ask(owner, client, "job.read", json!({ "job_id": job }))?;
        if let Some(message) = read["progress"]["message"].as_str()
            && progress.last().map(String::as_str) != Some(message)
        {
            progress.push(message.to_owned());
        }
        if !matches!(read["status"].as_str(), Some("queued" | "running")) {
            ensure(
                read["status"] == "ready",
                format!("A job did not finish: {read}"),
            )?;
            return Ok(json!({"record": read, "progress": progress,
                "seconds": started.elapsed().as_secs_f64()}));
        }
        ensure(
            started.elapsed() < deadline,
            format!("A job did not end: {read}"),
        )?;
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The folder choice a merge into the plan's proposal names: its existing folder, or the folders
/// it suggests, which the merge makes.
fn proposal(plan: &Value) -> Value {
    if plan["folder"].is_object() {
        json!({"kind": "existing", "folder_id": plan["folder"]["folder_id"]})
    } else {
        plan["suggested_folder"].clone()
    }
}

fn mutation(request: &str) -> Value {
    json!({"request_id": request, "actor": "setup"})
}

/// `asset`'s row in a view of `source`, read whole, or null.
fn row_in(owner: &OwnerHandle, client: ClientId, source: Value, asset: &Value) -> Result<Value> {
    let view = ask(owner, client, "browse.view", json!({ "source": source }))?;
    let count = view["count"].as_u64().unwrap_or(0);
    if count == 0 {
        return Ok(Value::Null);
    }
    let rows = ask(
        owner,
        client,
        "browse.rows",
        json!({"from": 0, "count": count}),
    )?;
    Ok(rows["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["asset_id"] == *asset)
        .cloned()
        .unwrap_or(Value::Null))
}

/// Where `result` is filed: its catalog folder, that folder and each above it to the top level
/// (`{id, name}`, the folder first), its path as the sources panel nests it, and its place in that
/// folder's view.
fn place(owner: &OwnerHandle, client: ClientId, result: &Value) -> Result<Value> {
    let photograph = row_in(owner, client, json!({"kind": "all-photographs"}), result)?;
    let folders = ask(owner, client, "folder.list", json!({}))?;
    let listed = folders["folders"].as_array().cloned().unwrap_or_default();
    let folder_id = photograph["folder_id"].clone();
    let mut chain = Vec::new();
    let mut at = folder_id.clone();
    while !at.is_null() && chain.len() <= listed.len() {
        let Some(held) = listed.iter().find(|held| held["id"] == at) else {
            break;
        };
        chain.push(json!({"id": held["id"], "name": held["name"]}));
        at = held["parent_id"].clone();
    }
    let path: Vec<&str> = chain
        .iter()
        .rev()
        .filter_map(|folder| folder["name"].as_str())
        .collect();
    let position = if folder_id.is_null() {
        Value::Null
    } else {
        row_in(
            owner,
            client,
            json!({"kind": "catalog-folder", "folder_id": folder_id}),
            result,
        )?["position"]
            .clone()
    };
    Ok(
        json!({"folder_id": folder_id, "chain": chain, "path": path.join(" \u{203a} "),
        "position": position}),
    )
}

/// Write both sets and make the catalog with their folder indexed and nothing merged. Answers the
/// expected record and every frame, Row then Wide.
fn make(generated: &Path) -> Result<(Value, Vec<PathBuf>)> {
    let frames = generated.join(FRAMES);
    let canonical = |paths: Vec<PathBuf>| -> Result<Vec<PathBuf>> {
        Ok(paths
            .iter()
            .map(|path| path.canonicalize())
            .collect::<std::io::Result<_>>()?)
    };
    let row = canonical(write_set(&frames.join(ROW.name), &ROW)?)?;
    let wide = canonical(write_set(&frames.join(WIDE.name), &WIDE)?)?;
    let catalog = generated.join(generate_catalog::CATALOG);
    drop(EditorService::open(&catalog).map_err(|error| format!("a new catalog: {error}"))?);
    let frames = frames.canonicalize()?;
    with_owner(&catalog, |owner, client| {
        let added = ask(
            owner,
            client,
            "index.add-folder",
            json!({"path": frames, "mutation": mutation("setup-frames")}),
        )?;
        if !added["job_id"].is_null() {
            follow(owner, client, &added["job_id"], SETUP_DEADLINE)?;
        }
        Ok(())
    })?;
    let set = |set: &Set, paths: &[PathBuf]| {
        json!({"folder": frames.join(set.name), "frames": paths,
            "estimate": stitched_estimate(set)})
    };
    Ok((
        json!({"sets": {ROW.name: set(&ROW, &row), WIDE.name: set(&WIDE, &wide)}, "warp": WARP}),
        row.into_iter().chain(wide).collect(),
    ))
}

/// What the catalog says of both results and the run's changes, over an owner of the runner's
/// own: each merge inspected, the Row result's history, where each is filed, and the library
/// journal.
fn after(catalog: &Path, results: &[(&str, Value)]) -> Result<Value> {
    with_owner(catalog, |owner, client| {
        let mut merges = serde_json::Map::new();
        let mut places = serde_json::Map::new();
        for (name, result) in results {
            merges.insert(
                (*name).to_owned(),
                ask(
                    owner,
                    client,
                    "merge.inspect",
                    json!({ "asset_id": result }),
                )?,
            );
            places.insert((*name).to_owned(), place(owner, client, result)?);
        }
        let history = ask(
            owner,
            client,
            "history.list",
            json!({ "asset_id": results[0].1 }),
        )?;
        let journal = ask(owner, client, "library.journal", json!({"limit": 500}))?;
        let changes: Vec<Value> = journal["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|change| {
                json!({"method": change["method"], "label": change["label"],
                    "actor": change["actor"], "request_id": change["request_id"]})
            })
            .collect();
        Ok(
            json!({"merges": merges, "places": places, "history": history["entries"],
            "changes": changes}),
        )
    })
}

/// Each result the `select` launch's merges answered, by set.
fn results(launch: &Checked) -> Result<[(&'static str, Value); 2]> {
    let result = |step: &str| -> Result<Value> {
        let frame = launch.at(step)?;
        let result = merge_answer(frame)["result_asset_id"].clone();
        ensure(
            result.is_string(),
            format!(
                "{step}: the merge answered no result: {}",
                frame["step"]["merge_record"]
            ),
        )?;
        Ok(result)
    };
    Ok([
        (ROW.name, result("merged")?),
        (WIDE.name, result("wide_merged")?),
    ])
}

/// Make the frames and the catalog (a replay reads the recorded answers), launch the editor to
/// merge them, read where the catalog filed the results, launch it to develop them, take the
/// frames offline, launch it again, and check every frame.
pub fn run(mut run: Run, scenario: &'static Scenario, _: Vec<PathBuf>) -> Result {
    let generated = run.out().join(GENERATED);
    let catalog = generated.join(generate_catalog::CATALOG);
    let expected_file = run.out().join(EXPECTED);
    let select_file = run.out().join(AFTER_SELECT);
    let develop_file = run.out().join(AFTER_DEVELOP);
    let after_file = run.out().join(AFTER);
    if let Some(note) = scenario.note {
        run.note(note);
    }
    run.check(|run| {
        let frames = if run.replaying() {
            Vec::new()
        } else {
            fs::create_dir_all(&generated)?;
            let (expected, frames) = make(&generated)?;
            write_json(&expected_file, &expected)?;
            frames
        };
        let expected = read_json(&recorded(run, &expected_file))?;
        if !run.replaying() {
            run.hash(&frames)?;
        }
        let window = |launch: Launch| match scenario.window {
            Some(window) => launch.window(window),
            None => launch,
        };
        let selecting = select_plan(&expected)?;
        let launched = run.launch(window(
            Launch::named("select")
                .catalog(&catalog)
                .script("select-script.json", selecting.script())
                .deadline(LAUNCH_DEADLINE),
        ))?;
        let select_checked = selecting.check(&launched.dir)?;
        let results = results(&select_checked)?;
        if !run.replaying() {
            write_json(&select_file, &after(&catalog, &results)?)?;
        }
        let developing = develop_plan(&read_json(&recorded(run, &select_file))?)?;
        let launched = run.launch(window(
            Launch::named("develop")
                .catalog(&catalog)
                .script("develop-script.json", developing.script())
                .deadline(LAUNCH_DEADLINE),
        ))?;
        let develop_checked = developing.check(&launched.dir)?;
        if !run.replaying() {
            write_json(&develop_file, &after(&catalog, &results)?)?;
        }
        let reopening = offline_plan(&read_json(&recorded(run, &develop_file))?)?;
        let online = generated.join(FRAMES);
        let offline = generated.join(OFFLINE);
        if !run.replaying() {
            fs::rename(&online, &offline)?;
        }
        let launched = run.launch(window(
            Launch::named("offline")
                .catalog(&catalog)
                .script("offline-script.json", reopening.script()),
        ));
        if !run.replaying() {
            fs::rename(&offline, &online)?;
        }
        let offline_checked = reopening.check(&launched?.dir)?;
        if !run.replaying() {
            write_json(&after_file, &after(&catalog, &results)?)?;
        }
        (scenario.verify)(run, &[select_checked, develop_checked, offline_checked])?;
        if !run.replaying() {
            run.sources_unchanged()?;
        }
        Ok(())
    })
}

/// What the recorded run holds at `file` in a replay, else `file`.
fn recorded(run: &Run, file: &Path) -> PathBuf {
    run.recorded(file).unwrap_or_else(|| file.to_owned())
}

// The checks.

fn select(frame: &Frame) -> &Value {
    &frame.state()["select"]
}

fn develop(frame: &Frame) -> &Value {
    &frame.state()["develop"]
}

/// The frame's stacked rows, as their cells draw them.
fn stacks(frame: &Frame) -> Vec<Value> {
    select(frame)["stacks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The library request a frame's Select step sent, without its envelope's identity.
fn library(frame: &Frame) -> &Value {
    &select(frame)["library"]
}

/// A panorama stack's badge words, long and short, and whether its originals are shown.
fn badge(frames: usize, expanded: bool) -> Value {
    json!([
        format!("Panorama \u{b7} {frames} photos"),
        format!("Pano \u{b7} {frames}"),
        expanded
    ])
}

/// The one row of the view: the set's result leading its collapsed panorama stack, never shown
/// picked.
fn collapsed_leader(frame: &Frame, name: &str, set: &Set, result: &Value) -> Result {
    let rows = stacks(frame);
    ensure(
        rows.len() == 1 && select(frame)["count"] == 1,
        format!(
            "{name}: the view holds {} items and these stacked rows: {rows:?}",
            select(frame)["count"]
        ),
    )?;
    let row = &rows[0];
    ensure(
        row["item"]["asset_id"] == *result
            && row["role"] == "leader"
            && row["kind"] == "panorama"
            && row["size"] == set.frames
            && row["collapsed"] == set.frames
            && row["badge"] == badge(set.frames, false)
            && row["picked"] == false,
        format!("{name}: the result's cell is {row}"),
    )
}

/// Mean luminance of a patch of the drawn photograph at `at`, fractions of its rectangle.
fn patch(frame: &Frame, at: [f64; 2]) -> Result<f64> {
    let [left, top, right, bottom] = frame.photo()?;
    let (width, height) = (f64::from(right - left), f64::from(bottom - top));
    crate::scenario::pixels::mean_luminance(
        frame.image()?,
        (
            f64::from(left) + width * at[0],
            f64::from(top) + height * at[1],
        ),
        (height * PATCH).max(2.0) as i64,
    )
}

/// Whether the result is filed where the plan proposed: in the plan's existing folder, or in the
/// folders its suggestion names.
fn in_proposed_folder(plan: &Value, place: &Value) -> bool {
    let chain = place["chain"].as_array().cloned().unwrap_or_default();
    let proposed = proposal(plan);
    match proposed["kind"].as_str() {
        Some("existing") => chain
            .iter()
            .any(|folder| folder["id"] == proposed["folder_id"]),
        Some(_) => chain
            .iter()
            .any(|folder| folder["name"] == proposed["name"]),
        None => false,
    }
}

/// A result opened in Develop from its saved source and its frames named `prefix…` by
/// [`developed`]: drawn at Fit, at 100% in its corner and at Fit again, and one stop through its
/// mask lifting the covered side only. Answers the lifted frame's covered patch.
fn check_developed(
    checks: &mut Checks,
    launch: &Checked,
    prefix: &str,
    answer: &Value,
) -> Result<f64> {
    let at = |step: &str| launch.at(&format!("{prefix}{step}"));
    let opened = at("develop")?;
    ensure(
        develop(opened)["document"]["asset_id"] == answer["result_asset_id"]
            && develop(opened)["document"]["source"] == "derived"
            && develop(opened)["document"]["title"] == answer["file_name"]
            && select(opened)["shown"] == "develop",
        format!(
            "{prefix}develop: Develop shows {}",
            develop(opened)["document"]
        ),
    )?;
    for step in ["fit", "corner", "refit"] {
        let frame = at(step)?;
        frame.photo()?;
        checks.note(
            frame,
            &format!("{prefix}{step}: the result drawn"),
            json!({"view": frame.state()["surface"]["view"],
                "drawing_path": frame.state()["surface"]["gpu"]["drawing_path"],
                "histogram": {"overview": frame.state()["histogram"]["overview"],
                    "tooltips": frame.state()["histogram"]["tooltips"]}}),
        );
    }
    let (refit, lifted) = (at("refit")?, at("lifted")?);
    let covered = patch(lifted, COVERED)?;
    checks.compare(
        lifted,
        &format!("{prefix}lifted: one stop through the mask lifts its covered side"),
        covered,
        patch(refit, COVERED)?,
        Tolerance::Above(LIFT),
    )?;
    checks.compare(
        lifted,
        &format!("{prefix}lifted: the mask leaves its uncovered side"),
        patch(lifted, UNCOVERED)?,
        patch(refit, UNCOVERED)?,
        Tolerance::Within(UNTOUCHED),
    )?;
    Ok(covered)
}

/// A result reopened in Develop with its frames moved away: from its saved source, its covered
/// patch where the `develop` launch's edit left it.
fn check_reopened(
    checks: &mut Checks,
    frame: &Frame,
    name: &str,
    answer: &Value,
    lifted: f64,
) -> Result {
    ensure(
        develop(frame)["document"]["asset_id"] == answer["result_asset_id"]
            && develop(frame)["document"]["title"] == answer["file_name"],
        format!("{name}: Develop shows {}", develop(frame)["document"]),
    )?;
    checks.compare(
        frame,
        &format!("{name}: the covered side, frames offline, against the edited one"),
        patch(frame, COVERED)?,
        lifted,
        Tolerance::Within(REOPEN_TOLERANCE),
    )
}

/// The result a merge step's frame recorded: the job's `ready` record's result.
fn merge_answer(frame: &Frame) -> &Value {
    &frame["step"]["merge_record"]["result"]
}

/// The merge jobs the owner's activity board listed when the frame was settled, and their kinds.
fn merge_jobs(frame: &Frame) -> BTreeSet<(String, String)> {
    frame["step"]["owner_merge_jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|job| {
            Some((
                job["job_id"].as_str()?.to_owned(),
                job["kind"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// The requests in `sent`, by method, in order.
fn methods(sent: &Value) -> Vec<&str> {
    sent.as_array()
        .into_iter()
        .flatten()
        .filter_map(|request| request["method"].as_str())
        .collect()
}

/// What the sheet showed in the frame.
fn shown_sheet(frame: &Frame) -> &Value {
    &select(frame)["merge"]["sheet"]
}

/// The preview revision the frame's step recorded last: the preview job's `ready` result.
fn revision(frame: &Frame) -> &Value {
    &frame["step"]["preview_record"]["result"]
}

/// The selection menu over a set's frames, planned: Merge panorama… enabled beside Merge HDR…
/// where the frames are few enough for HDR, the panorama plan the menu asked for naming the frames
/// selected, and its answer suitable. Answers the plan and its request.
fn check_menu<'a>(frame: &'a Frame, name: &str, set: &Set) -> Result<(&'a Value, &'a Value)> {
    let merge = &select(frame)["merge"];
    let groups = merge["menu"].as_array().cloned().unwrap_or_default();
    let heading = |heading: &str| groups.iter().find(|group| group["heading"] == heading);
    ensure(
        heading("Panorama").is_some_and(|group| {
            group["entries"]
                == json!([{"label": "Merge panorama\u{2026}", "enabled": true, "reason": null}])
        }) && heading("HDR").is_some() == (set.frames <= 9),
        format!("{name}: the menu shows {}", merge["menu"]),
    )?;
    let request = merge["plan"]
        .as_array()
        .and_then(|sent| sent.iter().find(|params| params["kind"] == "panorama"))
        .ok_or_else(|| format!("{name}: the menu planned {}", merge["plan"]))?;
    let plan = merge["plan_answers"]
        .as_array()
        .and_then(|answers| answers.iter().find(|answer| answer["kind"] == "panorama"))
        .map(|answer| &answer["plan"])
        .ok_or_else(|| {
            format!(
                "{name}: the menu's plans answered {}",
                merge["plan_answers"]
            )
        })?;
    ensure(
        request["targets"]["kind"] == "files"
            && request["targets"]["file_ids"].as_array().map(Vec::len) == Some(set.frames)
            && plan["kind"] == "panorama"
            && plan["suitable"] == true
            && plan["inputs"].as_array().map(Vec::len) == Some(set.frames),
        format!("{name}: the menu asked {request} and was answered {plan}"),
    )?;
    Ok((plan, request))
}

/// A merge's pending row while its job runs: the row's words with the job's stage, and the
/// requests its start sent, the plan then the merge into the plan's proposal. Answers the job
/// and the `merge.create` parameters.
fn check_merging(frame: &Frame, name: &str, set: &Set, plan: &Value) -> Result<(String, Value)> {
    let merge = &select(frame)["merge"];
    let pending = &merge["pending"];
    let job = pending["job_id"]
        .as_str()
        .ok_or_else(|| format!("{name}: no pending merge: {merge}"))?
        .to_owned();
    let stage = pending["message"].as_str().unwrap_or_default();
    ensure(
        pending["kind"] == "panorama"
            && pending["frames"] == set.frames
            && STAGES.iter().any(|words| stage.starts_with(words))
            && pending["failed"].is_null()
            && select(frame)["headers"]["pending"]
                == json!([{"title": format!("Merging panorama \u{b7} {} photos", set.frames),
                    "detail": stage, "action": "Cancel", "failed": false}])
            && merge["sheet"].is_null()
            && merge["option"] == false,
        format!(
            "{name}: the pending row is {} for {pending}",
            select(frame)["headers"]["pending"]
        ),
    )?;
    let sent = &frame["step"]["merge_sent"];
    let create = sent
        .as_array()
        .and_then(|sent| {
            sent.iter()
                .find(|request| request["method"] == "merge.create")
        })
        .map(|request| request["params"].clone())
        .ok_or_else(|| format!("{name}: no merge.create was sent: {sent}"))?;
    ensure(
        methods(sent) == ["merge.plan", "merge.create"]
            && create["kind"] == "panorama"
            && create["mutation"]["actor"] == "desktop"
            && create["panorama"]["boundary_warp"] == WARP
            && create["folder"] == proposal(plan),
        format!("{name}: sent {sent}, the plan proposes {}", proposal(plan)),
    )?;
    Ok((job, create))
}

/// A merge's end: its job's answer, one collapsed panorama tile of its frames, selected, with
/// Select still shown, its Info band reading the projection and field of view the merge answered,
/// and no merge on the owner's board but this desktop's. Answers the result.
fn check_merged(
    frame: &Frame,
    name: &str,
    set: &Set,
    job: &str,
    jobs: &BTreeSet<(String, String)>,
) -> Result<Value> {
    let answer = merge_answer(frame);
    let result = answer["result_asset_id"].clone();
    ensure(
        frame["step"]["merge_record"]["job_id"] == job
            && answer["kind"] == "panorama"
            && answer["merged"] == true
            && answer["reused"] == false
            && answer["inputs"] == set.frames
            && result.is_string()
            && answer["file_name"]
                .as_str()
                .is_some_and(|file| file.ends_with(" Panorama")),
        format!("{name}: the job answered {}", frame["step"]["merge_record"]),
    )?;
    collapsed_leader(frame, name, set, &result)?;
    let projection = answer["projection"].as_str().unwrap_or_default();
    let field = |at: usize| {
        answer["field_of_view_deg"][at]
            .as_f64()
            .map(|degrees| degrees.round() as i64)
    };
    let (Some(across), Some(down)) = (field(0), field(1)) else {
        return Err(format!("{name}: the merge answered no field of view: {answer}").into());
    };
    let mut letters = projection.chars();
    let words = format!(
        "{}{} \u{b7} {across}\u{b0} \u{d7} {down}\u{b0}",
        letters
            .next()
            .map(|first| first.to_ascii_uppercase())
            .unwrap_or_default(),
        letters.as_str()
    );
    let info = &select(frame)["info"];
    ensure(
        info["kind"] == "one"
            && info["stack"]["title"] == format!("Panorama \u{b7} {} photos", set.frames)
            && info["stack"]["detail"] == words.as_str()
            && select(frame)["selection"]["active"] == 0
            && select(frame)["shown"] == "select",
        format!("{name}: the Info panel shows {info}, the merge answered {words}"),
    )?;
    // The board keeps finished work as recent only past its threshold and only the newest
    // sixteen, so a quick merge may be gone already; no other merge may be there.
    ensure(
        merge_jobs(frame).is_subset(jobs),
        format!("{name}: the board lists merges {:?}", merge_jobs(frame)),
    )?;
    Ok(result)
}

/// The Row through the preview sheet: the menu, the sheet's first revision, Boundary Warp, the
/// overlay switches, Merge and its pending row, the merge's end, the originals' words, the Info
/// bands, and Remove merge with its undo and redo. Answers the job and the menu's plan.
fn check_row(checks: &mut Checks, launch: &Checked) -> Result<(String, Value)> {
    let folder = launch.at("row_folder")?;
    ensure(
        select(folder)["count"] == ROW.frames && stacks(folder).is_empty(),
        format!(
            "row_folder: the view holds {} items, stacked {:?}",
            select(folder)["count"],
            stacks(folder)
        ),
    )?;
    let menu = launch.at("row_menu")?;
    let (plan, request) = check_menu(menu, "row_menu", &ROW)?;

    // Merge panorama…: the sheet asks the menu's plan again, then the preview, and draws its
    // first revision with Boundary Warp at 0 and both overlays on.
    let previewing = launch.at("previewing")?;
    let sheet = shown_sheet(previewing);
    let opened = revision(previewing);
    let overlays = &opened["overlays"]["panorama"];
    let settings = &sheet["panorama"];
    ensure(
        sheet["title"] == format!("Merge panorama \u{b7} {} photos", ROW.frames)
            && sheet["kind"] == "panorama"
            && sheet["preview_id"].is_string()
            && sheet["picture"].is_string()
            && sheet["picture_shown"] == sheet["picture"]
            && sheet["current"] == true
            && sheet["refusal"].is_null()
            && sheet["enabled"] == true
            && sheet["status"].as_str().is_some_and(|status| {
                status.starts_with("Preview from ") && status.contains("Crop loses ")
            })
            && methods(&sheet["sent"]) == ["merge.plan", "merge.preview"]
            && sheet["sent"][0]["params"] == *request
            && settings["boundary_warp"] == 0
            && settings["outlines"] == true
            && settings["crop"] == true
            && sheet["frames"].as_array().map(Vec::len) == Some(ROW.frames)
            && select(previewing)["merge"]["pending"].is_null(),
        format!("previewing: the sheet is {sheet}"),
    )?;
    ensure(
        opened["kind"] == "panorama"
            && opened["preview_id"] == sheet["preview_id"]
            && opened["revision"] == sheet["revision"]
            && opened["path"] == sheet["picture"]
            && opened["refusal"].is_null()
            && opened["diagnostics"]["panorama"]["projection"].is_string()
            && overlays["crop"].as_array().map(Vec::len) == Some(4)
            && overlays["covered"]
                .as_array()
                .is_some_and(|covered| !covered.is_empty())
            && overlays["frames"].as_array().map(Vec::len) == Some(ROW.frames),
        format!("previewing: the preview opened as {opened}"),
    )?;
    let preview_id = sheet["preview_id"].clone();

    // Boundary Warp let go at 60: the whole options sent to the same preview, a newer revision
    // drawn, its crop losing no more and its output no smaller.
    let warped = launch.at("warped")?;
    let changed = shown_sheet(warped);
    let update = changed["sent"]
        .as_array()
        .and_then(|sent| sent.last())
        .cloned()
        .unwrap_or_default();
    let drawn = revision(warped);
    let area = |revision: &Value| {
        revision["output"]["width"].as_u64().unwrap_or(0)
            * revision["output"]["height"].as_u64().unwrap_or(0)
    };
    let loss = |revision: &Value| revision["diagnostics"]["panorama"]["crop_loss"].as_f64();
    ensure(
        changed["panorama"]["boundary_warp"] == WARP
            && changed["preview_id"] == preview_id
            && changed["current"] == true
            && changed["picture_shown"] == changed["picture"]
            && changed["revision"].as_u64() > sheet["revision"].as_u64()
            && update["method"] == "merge.preview-update"
            && update["params"]["preview_id"] == preview_id
            && update["params"]["panorama"]["boundary_warp"] == WARP,
        format!("warped: the sheet is {changed}"),
    )?;
    ensure(
        drawn["preview_id"] == preview_id
            && drawn["revision"] == changed["revision"]
            && drawn["options"]["panorama"]["boundary_warp"] == WARP
            && drawn["refusal"].is_null()
            && loss(drawn).zip(loss(opened)).is_some_and(|(w, o)| w <= o)
            && area(drawn) >= area(opened),
        format!("warped: Boundary Warp {WARP} drew {drawn}, after {opened}"),
    )?;

    // The overlay switches: drawn from the revision's data, sending nothing.
    let sent = methods(&changed["sent"]).len();
    for (name, outlines, crop) in [
        ("plain", false, true),
        ("uncropped", false, false),
        ("outlined", true, false),
        ("overlaid", true, true),
    ] {
        let shown = shown_sheet(launch.at(name)?);
        ensure(
            shown["panorama"]["outlines"] == outlines
                && shown["panorama"]["crop"] == crop
                && shown["revision"] == changed["revision"]
                && methods(&shown["sent"]).len() == sent,
            format!("{name}: the sheet is {shown}"),
        )?;
    }
    checks.note(
        launch.at("overlaid")?,
        "previewing: the sheet draws the preview, redraws it for Boundary Warp, and its overlays",
        json!({"opened": opened, "warped": drawn}),
    );

    // Merge: the preview continued into the pending row, then one collapsed tile.
    let merging = launch.at("merging")?;
    let (job, create) = check_merging(merging, "merging", &ROW, plan)?;
    ensure(
        create["preview_id"] == preview_id && create.get("plan_id").is_none(),
        format!("merging: Merge sent {create}"),
    )?;
    let jobs = BTreeSet::from([(job.clone(), "merge.panorama".to_owned())]);
    let result = check_merged(launch.at("merged")?, "merged", &ROW, &job, &jobs)?;

    // Expanded: the result, then its four originals in the stack's order, "Frame k of 4".
    let expanded = launch.at("expanded")?;
    let rows = stacks(expanded);
    let words: Vec<Value> = rows
        .iter()
        .skip(1)
        .map(|row| row["exposure"].clone())
        .collect();
    let frames: Vec<Value> = (1..=ROW.frames)
        .map(|k| json!(format!("Frame {k} of {}", ROW.frames)))
        .collect();
    ensure(
        rows.len() == ROW.frames + 1
            && rows[0]["role"] == "leader"
            && rows[0]["item"]["asset_id"] == result
            && rows[0]["expansion"] == "expanded"
            && rows[0]["badge"] == badge(ROW.frames, true)
            && rows
                .iter()
                .skip(1)
                .all(|row| row["role"] == "original" && row["kind"] == "panorama")
            && words == frames
            && select(expanded)["merge"]["stack"]["expansion"] == "expanded",
        format!("expanded: the stack shows {rows:?}"),
    )?;
    // An original's Info band names its place in the stack's order, as its cell does.
    let band = &select(launch.at("original")?)["info"]["stack"];
    ensure(
        band["leader"] == false && band["title"] == frames[1],
        format!("original: the Info band is {band}"),
    )?;
    let band = &select(launch.at("clicked")?)["info"]["stack"];
    let merged_band = &select(launch.at("merged")?)["info"]["stack"];
    ensure(
        band["leader"] == true
            && band["title"] == merged_band["title"]
            && band["detail"] == merged_band["detail"],
        format!("clicked: the Info band is {band}"),
    )?;

    // Remove merge, then its undo, redo and undo again: the originals and the result in turn.
    let removed = launch.at("removed")?;
    ensure(
        library(removed)["method"] == "merge.remove"
            && library(removed)["error"].is_null()
            && stacks(removed).is_empty()
            && select(removed)["count"] == ROW.frames,
        format!(
            "removed: ⌫ sent {}, the view holds {} items",
            library(removed),
            select(removed)["count"]
        ),
    )?;
    for (name, shown) in [("restored", true), ("redone", false), ("undone", true)] {
        let frame = launch.at(name)?;
        let method = if shown {
            "library.undo"
        } else {
            "library.redo"
        };
        ensure(
            library(frame)["method"] == method && library(frame)["error"].is_null(),
            format!("{name}: sent {}", library(frame)),
        )?;
        let leader = stacks(frame)
            .into_iter()
            .find(|row| row["role"] == "leader");
        ensure(
            leader.is_some_and(|row| row["item"]["asset_id"] == result) == shown,
            format!("{name}: the stacked rows are {:?}", stacks(frame)),
        )?;
    }
    Ok((job, plan.clone()))
}

/// The Wide by ⌥-click: the menu, the merge at once with the remembered settings and no sheet,
/// its pending row and its end, the tile's preview drawn. Answers the menu's plan.
fn check_wide(checks: &mut Checks, launch: &Checked, row_job: &str) -> Result<Value> {
    let folder = launch.at("wide_folder")?;
    ensure(
        select(folder)["count"] == WIDE.frames && stacks(folder).is_empty(),
        format!(
            "wide_folder: the view holds {} items, stacked {:?}",
            select(folder)["count"],
            stacks(folder)
        ),
    )?;
    let (plan, _) = check_menu(launch.at("wide_menu")?, "wide_menu", &WIDE)?;
    let merging = launch.at("wide_merging")?;
    let (job, create) = check_merging(merging, "wide_merging", &WIDE, plan)?;
    ensure(
        create["plan_id"].is_string() && create.get("preview_id").is_none(),
        format!("wide_merging: ⌥-click sent {create}"),
    )?;
    let jobs = BTreeSet::from([
        (row_job.to_owned(), "merge.panorama".to_owned()),
        (job.clone(), "merge.panorama".to_owned()),
    ]);
    let merged = launch.at("wide_merged")?;
    check_merged(merged, "wide_merged", &WIDE, &job, &jobs)?;
    let previews = &select(merged)["previews"];
    ensure(
        previews["visible"].as_u64() > Some(0)
            && previews["visible_held"] == previews["visible"]
            && previews["failed"] == 0
            && previews["refused"] == 0,
        format!("wide_merged: the tile's previews are {previews}"),
    )?;
    checks.note(
        merging,
        "wide_merging: ⌥-click merges at once with Boundary Warp 60, the sheet's last settings",
        json!({"create": create, "pending": select(merging)["merge"]["pending"],
            "estimate": stitched_estimate(&WIDE), "answer": merge_answer(merged)}),
    );
    Ok(plan.clone())
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [selecting, developing, offline] = launches else {
        return Err(format!("panorama-merge has three launches, not {}", launches.len()).into());
    };
    let after_select = read_json(&recorded(run, &run.out().join(AFTER_SELECT)))?;
    let after_develop = read_json(&recorded(run, &run.out().join(AFTER_DEVELOP)))?;
    let after = read_json(&recorded(run, &run.out().join(AFTER)))?;
    let mut checks = Checks::new();

    // Select: the Row through the sheet, the Wide by ⌥-click.
    let (row_job, row_plan) = check_row(&mut checks, selecting)?;
    let wide_plan = check_wide(&mut checks, selecting, &row_job)?;
    let row_answer = merge_answer(selecting.at("merged")?);
    let row_result = row_answer["result_asset_id"].clone();
    let wide_answer = merge_answer(selecting.at("wide_merged")?);
    let wide_result = wide_answer["result_asset_id"].clone();

    // The catalog the select launch left: both merges active, each result where its plan
    // proposed, and nothing merged again by the undo and redo.
    let merges = &after_select["merges"];
    for (set, plan) in [(ROW, &row_plan), (WIDE, &wide_plan)] {
        let place = &after_select["places"][set.name];
        ensure(
            merges[set.name]["state"] == "active"
                && merges[set.name]["source"]["status"] == "available"
                && in_proposed_folder(plan, place),
            format!(
                "The select launch left the {} merge as {}, filed in {place}, the plan proposed {}",
                set.name,
                merges[set.name],
                proposal(plan)
            ),
        )?;
    }
    let count = |after: &Value, method: &str| {
        after["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|change| change["method"] == method)
            .count()
    };
    ensure(
        count(&after_select, "merge.create") == 2,
        format!("The journal records {}", after_select["changes"]),
    )?;

    // Develop: the Row result from its saved source, at Fit, at 100% in its far corner, and
    // edited through a linear mask; a resident source, whose histogram counts the photograph.
    let row_lifted = check_developed(&mut checks, developing, "", row_answer)?;
    let histogram = &developing.at("fit")?.state()["histogram"];
    ensure(
        histogram["overview"].is_null(),
        format!("fit: the Row's histogram is {histogram}"),
    )?;
    let inspected = &developing.at("inspected")?["step"]["result"];
    ensure(
        inspected["result_asset_id"] == row_result
            && inspected["kind"] == "panorama"
            && inspected["state"] == "active"
            && inspected["source"]["status"] == "available"
            && inspected["parameters"]["panorama"]["boundary_warp"] == WARP
            && inspected["projection"].is_string()
            && inspected["inputs"].as_array().is_some_and(|inputs| {
                inputs.len() == ROW.frames
                    && inputs.iter().all(|input| input["file"] == "available")
            }),
        format!("inspected: merge.inspect answered {inspected}"),
    )?;

    // The paged result in Develop: at Fit its histogram counts the overview; at 100% in its
    // corner the surface holds a window of level 0; then the mask and its export.
    let wide_lifted = check_developed(&mut checks, developing, "wide_", wide_answer)?;
    let histogram = &developing.at("wide_fit")?.state()["histogram"];
    ensure(
        histogram["overview"]
            .as_array()
            .is_some_and(|size| size.len() == 2),
        format!("wide_fit: the histogram is {histogram}"),
    )?;
    let events = crate::scenario::events(&developing.evidence.join("events.jsonl"))?;
    let windows: Vec<&Value> = events
        .iter()
        .filter(|event| event["event"] == "gpu_window")
        .map(|event| &event["detail"])
        .collect();
    ensure(
        windows.iter().any(|window| window["held"] == true),
        format!("wide_corner: no window of level 0 was held: {windows:?}"),
    )?;
    let exported = developing.at("wide_exported")?;
    let export = &exported["step"]["export"];
    let (width, height) = image::image_dimensions(developing.evidence.join(EXPORT_FILE))?;
    ensure(
        export["record"]["status"] == "ready"
            && export["record"]["result"]["renderer"]["record"] == "gpu"
            && u64::from(width) * u64::from(height) > RESIDENT_PIXELS,
        format!("wide_exported: a {width} × {height} file from {export}"),
    )?;
    checks.note(
        exported,
        "wide_exported: Develop's export of the paged result",
        json!({"width": width, "height": height, "export": export,
            "windows": windows.len()}),
    );

    // The catalog the develop launch left: both merges still active, nothing merged again, and
    // the Row result's edits in its history.
    ensure(
        [ROW.name, WIDE.name]
            .iter()
            .all(|name| after_develop["merges"][name]["state"] == "active")
            && count(&after_develop, "merge.create") == 2,
        format!("The develop launch left {after_develop}"),
    )?;
    ensure(
        after_develop["history"].as_array().map_or(0, Vec::len) >= 3,
        format!(
            "The Row result's history holds {}",
            after_develop["history"]
        ),
    )?;

    // Reopened with the frames moved away: each result from its saved source with its edit.
    check_reopened(
        &mut checks,
        offline.at("reopened")?,
        "reopened",
        row_answer,
        row_lifted,
    )?;
    check_reopened(
        &mut checks,
        offline.at("wide_reopened")?,
        "wide_reopened",
        wide_answer,
        wide_lifted,
    )?;
    let missing = &offline.at("inspected")?["step"]["result"];
    ensure(
        missing["result_asset_id"] == row_result
            && missing["state"] == "active"
            && missing["source"]["status"] == "available"
            && missing["inputs"]
                .as_array()
                .is_some_and(|inputs| inputs.iter().all(|input| input["file"] == "missing")),
        format!("inspected: with the frames offline merge.inspect answered {missing}"),
    )?;
    // Named back: the same two merges, nothing merged again.
    ensure(
        [ROW.name, WIDE.name]
            .iter()
            .all(|name| after["merges"][name]["state"] == "active")
            && after["changes"].as_array().map(Vec::len)
                == after_develop["changes"].as_array().map(Vec::len),
        format!("After the offline launch the catalog holds {after}"),
    )?;
    checks.write(
        &selecting.evidence,
        SCENARIO,
        json!({"row": row_result, "wide": wide_result,
            "tolerance": {"lift": LIFT, "untouched": UNTOUCHED, "reopen": REOPEN_TOLERANCE}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_test_macros::slow;

    fn places() -> Value {
        json!({
            ROW.name: {"path": "2026 \u{203a} 13 Oct", "position": 0},
            WIDE.name: {"path": "2026 \u{203a} 13 Oct", "position": 1},
        })
    }

    fn expected() -> Value {
        json!({"sets": {
            ROW.name: {"folder": "/tmp/Frames/Row"},
            WIDE.name: {"folder": "/tmp/Frames/Wide"},
        }})
    }

    #[test]
    fn the_panorama_merge_plans_are_well_formed() {
        let selecting = select_plan(&expected()).unwrap();
        selecting.validate().unwrap();
        assert!(selecting.scripted());
        assert_eq!(selecting.len(), 27);
        let after = json!({ "places": places() });
        let developing = develop_plan(&after).unwrap();
        developing.validate().unwrap();
        assert_eq!(developing.len(), 22);
        assert!(developing.index("wide_place").is_none());
        let reopening = offline_plan(&after).unwrap();
        reopening.validate().unwrap();
        assert_eq!(reopening.len(), 9);
        // A Wide result filed in another folder has that folder pressed first.
        let mut apart = places();
        apart[WIDE.name]["path"] = json!("2026 \u{203a} Elsewhere");
        let apart = json!({ "places": apart });
        for plan in [develop_plan(&apart).unwrap(), offline_plan(&apart).unwrap()] {
            plan.validate().unwrap();
            assert!(plan.index("wide_place").is_some());
        }
        // Registered, opening nothing a checkout lacks, so `verify --tier rendered` runs it.
        let row = crate::smoke::find(SCENARIO).unwrap();
        assert!(row.rendered() && !row.takes_source());
    }

    /// Both merges are the desktop's own gestures: the Row's frames selected, the menu over them,
    /// Merge panorama…, the sheet's Boundary Warp, overlays and Merge; the Wide's ⌥-clicked.
    #[test]
    fn the_select_launch_merges_through_the_menu_and_the_sheet() {
        let script = select_plan(&expected()).unwrap().script();
        let steps = script.as_array().unwrap();
        let at = |step: Value| {
            steps
                .iter()
                .position(|held| *held == step)
                .unwrap_or_else(|| panic!("no step {step} in {script}"))
        };
        let menu = at(json!({"select": {"merge_menu": 3}}));
        assert_eq!(
            steps[menu - 2..menu],
            [
                json!({"select": {"click": {"position": 0}}}),
                json!({"select": {"click": {"position": 3, "shift": true}}}),
            ]
        );
        let sheet = [
            json!({"select": {"merge_choose": {"kind": "panorama"}}}),
            json!({"select": {"merge_preview": {"boundary_warp": WARP}}}),
            json!({"select": {"merge_preview": {"frame_outlines": false}}}),
            json!({"select": {"merge_preview": {"crop_coverage": false}}}),
            json!({"select": {"merge_preview": {"frame_outlines": true}}}),
            json!({"select": {"merge_preview": {"crop_coverage": true}}}),
            json!({"select": {"merge_preview": "merge"}}),
            json!({"select": "merged"}),
        ];
        assert_eq!(steps[menu + 1..menu + 1 + sheet.len()], sheet);
        let wide = at(json!({"select": {"merge_menu": 9}}));
        assert!(wide > menu);
        assert_eq!(
            steps[wide + 1..],
            [
                json!({"select": {"merge_choose": {"kind": "panorama", "last_settings": true}}}),
                json!({"select": "merged"}),
            ]
        );
    }

    #[test]
    fn the_wide_row_stitches_past_the_resident_limit_and_the_row_within_it() {
        let (width, height) = stitched_estimate(&WIDE);
        // A tenth to spare for the crop's own losses.
        assert!(
            width * height > 1.1 * RESIDENT_PIXELS as f64,
            "{width} × {height}"
        );
        assert!(width < 65_000.0);
        let (width, height) = stitched_estimate(&ROW);
        assert!(width * height < 16_000_000.0, "{width} × {height}");
        // Neighbours overlap by at least a third of a frame.
        for set in [ROW, WIDE] {
            assert!(set.step_deg <= set.hfov_deg * 2.0 / 3.0);
        }
    }

    #[test]
    fn frames_are_recorded_the_same_on_any_number_of_bands() {
        let scene = Scene::new(1, CELL);
        let lens = Lens::with_hfov(16, 12, 60.0);
        let pose = Pose::new(10.0, 0.0, 0.0);
        let recorded = record(&scene, &lens, &pose);
        let to_world = pose.camera_to_world();
        for (index, pixel) in recorded.chunks_exact(3).enumerate() {
            let (x, y) = ((index % 16) as f64, (index / 16) as f64);
            let value = scene.radiance(apply(&to_world, lens.ray((x, y))));
            assert_eq!(pixel, value.map(|v| srgb::code(v * GAIN)));
        }
    }

    /// Each set registers through the core's own frame path and registration as a panorama of its
    /// frames, at the size its analysis proxies see (the Wide row at a quarter, its proxies' box
    /// factor), and the Wide's output scales past the resident limit at full size.
    #[test]
    #[slow]
    fn the_generated_rows_preview_as_panoramas() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = directory.path().join("catalog.sqlite");
        drop(EditorService::open(&catalog).unwrap());
        let quarter = Set {
            width: WIDE.width / 4,
            height: WIDE.height / 4,
            ..WIDE
        };
        let mut previews = Vec::new();
        for set in [ROW, quarter] {
            let frames: Vec<PathBuf> = write_set(&directory.path().join(set.name), &set)
                .unwrap()
                .iter()
                .map(|path| path.canonicalize().unwrap())
                .collect();
            let preview = with_owner(&catalog, |owner, client| {
                let opened = ask(
                    owner,
                    client,
                    "merge.preview",
                    json!({"kind": "panorama", "targets": {"kind": "paths", "paths": frames}}),
                )?;
                follow(owner, client, &opened["job_id"], SETUP_DEADLINE)
            })
            .unwrap();
            let revision = preview["record"]["result"].clone();
            assert!(revision["refusal"].is_null(), "{}: {revision}", set.name);
            assert_eq!(
                revision["overlays"]["panorama"]["frames"]
                    .as_array()
                    .map(Vec::len),
                Some(set.frames),
                "{}: {revision}",
                set.name
            );
            previews.push(revision);
        }
        let output = &previews[1]["output"];
        let pixels = output["width"].as_f64().unwrap() * output["height"].as_f64().unwrap();
        assert!(pixels * 16.0 > RESIDENT_PIXELS as f64, "{output}");
    }
}
