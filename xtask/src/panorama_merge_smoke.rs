//! The `panorama-merge` smoke scenario: overlapping frames merged into panoramas, one shown in
//! Select and developed, removed and put back, one past the resident source limits exported, and
//! the first reopened with its frames moved away, in the real editor at 1440 × 900 ([panorama
//! design](../../docs/design/panorama-merge.md), "Evidence and acceptance", item 5).
//!
//! The run first writes two rows of JPEG frames of one synthetic scene (`luxforge_reference::
//! panorama`), each frame the scene seen by a level camera turned about its centre: `Frames/Row`,
//! four 1536 × 1024 frames 30° apart with a 60° field (about 3 MP stitched), and `Frames/Wide`, ten
//! 6000 × 4000 frames 26° apart with a 40° field, which stitch past the resident limits (about
//! 150 MP, so its result is a paged source). It makes a new catalog with `Frames` indexed.
//!
//! **What the editor cannot do yet runs through the core's API beforehand**, as the documented
//! requests the desktop will send ([`PENDING`] lists each step and the task it waits on):
//!
//! - the Row's merge as the panorama sheet sends it (`merge.plan`, `merge.preview`,
//!   `merge.preview-update` with Boundary Warp [`WARP`], then `merge.create` with the preview), and
//!   the Wide's as ⌥-click sends it (`merge.plan` and `merge.create` at once with the last
//!   settings), each job's progress read as it runs (`panorama-merge-expected.json`);
//! - after the first launch, the Wide result's JPEG export, through the banded reference
//!   (`panorama-merge-export.json`).
//!
//! The first launch, in [`first_plan`] order: `G`; the Row's folder, one collapsed panorama tile;
//! `S` expanding its originals; an original clicked, its Info band "Frame k of 4"; the result's
//! catalog folder, the result clicked and opened in Develop from its saved source; Fit, 100% and
//! the bottom-right corner, Fit again; a linear mask and Exposure through it; `merge.inspect`; back
//! to Select, the result clicked and Remove merge (`⌫`), `Cmd+Z`, `Shift+Cmd+Z` and `Cmd+Z`; then
//! the Wide's folder, one collapsed tile of ten.
//!
//! The catalog the first launch left is read (`panorama-merge-after-first.json`), the Wide result
//! exported, `Frames` renamed so no frame can be read, and the second launch, in [`second_plan`]
//! order, opens the Row result from its catalog folder in Develop with its masked edit and inspects
//! it. The folder is named back, the catalog read again (`panorama-merge-after.json`) and every
//! frame's hash checked. The run then fails naming the [`PENDING`] steps until there are none.
use crate::{
    generate_catalog,
    hdr_merge_smoke::{ask, with_owner},
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step, Tolerance},
    smoke::Scenario,
    *,
};
use luxforge_core::{BASIC_EFFECT, ClientId, EditorService, OwnerHandle};
use luxforge_evidence::{
    self as script, CatalogStep, DevelopStep, LibraryKey, SelectStep, ViewStep,
};
use luxforge_reference::{
    panorama::{Lens, Pose, Scene, apply, row},
    srgb,
};
use std::time::{Duration, Instant};

pub const SCENARIO: &str = "panorama-merge";
/// What `reproduce.md` says the run does before, between and after its launches.
pub const NOTE: &str = "The run first writes two rows of JPEG frames of one synthetic scene \
    (`luxforge_reference::panorama`, a level camera turned about its centre): \
    `generated/Frames/Row`, four 1536 × 1024 frames 30° apart with a 60° field, and \
    `generated/Frames/Wide`, ten 6000 × 4000 frames 26° apart with a 40° field, which stitch past \
    128 MP. It makes `generated/catalog.sqlite` with `Frames` indexed and, through the core's own \
    client, merges the Row as the panorama sheet does (`merge.plan`, `merge.preview`, \
    `merge.preview-update` with Boundary Warp 60, `merge.create` with the preview) and the Wide as \
    ⌥-click does (`merge.plan` and `merge.create` with Boundary Warp 60), reading each job's \
    progress (`panorama-merge-expected.json`). The first launch opens that catalog with \
    `--catalog`; the run then reads the catalog it left (`panorama-merge-after-first.json`), \
    exports the Wide result through the core (`panorama-merge-export.json`, the file under \
    `exports/`), renames `Frames` to `Frames offline`, and the second launch reopens the same \
    catalog; the folder is named back and the catalog read again (`panorama-merge-after.json`).";
/// Where the run writes its frames and catalog.
pub const GENERATED: &str = "generated";
pub const EXPECTED: &str = "panorama-merge-expected.json";
pub const AFTER_FIRST: &str = "panorama-merge-after-first.json";
pub const EXPORTED: &str = "panorama-merge-export.json";
pub const AFTER: &str = "panorama-merge-after.json";
const EXPORTS: &str = "exports";
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
/// How long the setup waits for a preview, a merge and the export. The xtask is a dev build.
const PREVIEW_DEADLINE: Duration = Duration::from_secs(600);
const MERGE_DEADLINE: Duration = Duration::from_secs(1800);
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

/// What a step of item 5 waits on before the editor can take it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Awaits {
    /// GPU editing, previews and export of paged sources.
    Task005,
    /// Select's Merge panorama… item, its preview sheet, ⌥-click, the pending row and the
    /// panorama words.
    Task010,
}

impl Awaits {
    pub fn task(self) -> &'static str {
        match self {
            Self::Task005 => "TASK-005",
            Self::Task010 => "TASK-010",
        }
    }
}

/// A step of item 5 the editor cannot take on this base: where it goes in a launch's plan, the
/// gesture as a proposed script step, and what its frame must show. The setup or the runner drives
/// its documented requests meanwhile, where a frame would otherwise have nothing to show.
#[derive(Clone, Copy, Debug)]
pub struct Pending {
    /// The plan step it goes after, in the launch named first: `first/row` is after `row`.
    pub after: &'static str,
    pub awaits: Awaits,
    /// The proposed script steps, as JSON; a gesture with no script step yet proposes one.
    pub gesture: &'static str,
    pub expects: &'static str,
}

/// Every step of item 5 the journey cannot take yet. When the task a step awaits lands, add its
/// script steps to the plan at `after`, move the setup's requests it replaces into the launch, check
/// its frame in [`verify`], and delete its row. The run fails while any is left.
pub const PENDING: &[Pending] = &[
    Pending {
        after: "first/select",
        awaits: Awaits::Task010,
        gesture: r#"[{"select":{"folder":"<Frames/Row>"}},{"select":{"click":{"position":0}}},{"select":{"click":{"position":3,"shift":true}}},{"select":{"choose":{"menu":"selection","item":"Merge panorama…"}}}]"#,
        expects: "the Row's four frames selected and the selection menu's Merge panorama… opening the sheet \"Merge panorama · 4 photos\", captured once revision 1 is drawn; sent exactly merge.plan then merge.preview as the setup's Row `sent` records them; the setup's Row merge then moves here",
    },
    Pending {
        after: "first/previewing",
        awaits: Awaits::Task010,
        gesture: r#"{"select":{"merge_preview":{"boundary_warp":60}}}"#,
        expects: "merge.preview-update {preview_id, panorama: {boundary_warp: 60}} sent; a newer revision drawn, its crop losing no more and its output no smaller than revision 1's",
    },
    Pending {
        after: "first/warped",
        awaits: Awaits::Task010,
        gesture: r#"[{"select":{"merge_preview":{"crop_overlay":true}}},{"select":{"merge_preview":{"frame_outlines":true}}}]"#,
        expects: "the crop and coverage overlay and the frame outlines on, drawn from the revision's overlays: the crop rectangle, the dimmed coverage with \"Crop loses N% at the edges\", and frames numbered 1 to 4",
    },
    Pending {
        after: "first/overlaid",
        awaits: Awaits::Task010,
        gesture: r#"[{"select":{"merge_preview":"merge"}},{"select":"merged"}]"#,
        expects: "merge.create {kind: panorama, preview_id, panorama: {boundary_warp: 60}, folder: the plan's proposal} sent; the pending row \"Merging panorama · 4 photos\" with its stage words, bar and Cancel; then one collapsed tile, selected, with Select still shown; each Select frame's owner_merge_jobs lists merge.panorama jobs too",
    },
    Pending {
        after: "first/expanded",
        awaits: Awaits::Task010,
        gesture: "none: the expanded frame's cells",
        expects: "each original's cell footer reads \"Frame k of 4\" in the stack's order (today's cell words carry only HDR's EV offsets)",
    },
    Pending {
        after: "first/clicked",
        awaits: Awaits::Task010,
        gesture: r#"{"select":{"click":{"position":0}}}"#,
        expects: "the result's Info band \"Panorama · 4 photos\" with its resolved projection and field of view",
    },
    Pending {
        after: "first/undone",
        awaits: Awaits::Task010,
        gesture: r#"[{"select":{"folder":"<Frames/Wide>"}},{"select":{"click":{"position":0}}},{"select":{"click":{"position":9,"shift":true}}},{"select":{"choose":{"menu":"selection","item":"Merge panorama"}}},{"select":"merged"}]"#,
        expects: "⌥-click: merge.create {kind: panorama, plan_id, panorama: {boundary_warp: 60}} sent at once with no sheet, the last settings; the pending row \"Merging panorama · 10 photos\" showing \"Reading k of 10\" and \"Stitching k of n\"; then one collapsed tile of ten; the setup's Wide merge then moves here",
    },
    Pending {
        after: "first/wide",
        awaits: Awaits::Task005,
        gesture: "none: the Wide frame's tile",
        expects: "the paged result's tile preview rendered from its resident level",
    },
    Pending {
        after: "first/wide",
        awaits: Awaits::Task005,
        gesture: r#"[{"catalog":{"source":"<the Wide result's folder path>"}},{"select":{"click":{"position":"<its position>"}}},{"develop":"active"},{"zoom":"fit"}]"#,
        expects: "the paged result in Develop at Fit from its saved source, drawn from its overview level, the Info histogram saying it counts the overview",
    },
    Pending {
        after: "first/wide_fit",
        awaits: Awaits::Task005,
        gesture: r#"[{"zoom":100},{"pan":{"x":1.0,"y":1.0}},{"zoom":"fit"}]"#,
        expects: "100% in the bottom-right corner drawn from a level-0 window, no blank or uncovered draw",
    },
    Pending {
        after: "first/wide_corner",
        awaits: Awaits::Task005,
        gesture: r#"[{"api":{"method":"mask.create-linear","params":{"x0":0.5,"y0":0.3,"x1":0.5,"y1":0.7}}},{"api":{"method":"edit.set-basic","params":{"mask":{"name":"Mask 1"},"exposure":1.0}}}]"#,
        expects: "a linear mask and Exposure through it on the paged result: the covered side lifted, the other unchanged",
    },
    Pending {
        after: "first/wide_masked",
        awaits: Awaits::Task005,
        gesture: r#"{"export":{"file":{"name":"wide-panorama.jpg"}}}"#,
        expects: "Develop's export of the paged result written above 128 MP by the GPU's streamed level-0 windows (renderer gpu); it replaces the runner's reference export",
    },
    Pending {
        after: "second/inspected",
        awaits: Awaits::Task005,
        gesture: r#"[{"key":"g"},{"catalog":{"source":"<the Wide result's folder path>"}},{"select":{"click":{"position":"<its position>"}}},{"develop":"active"}]"#,
        expects: "with the frames moved away, the Wide result opens at Fit with its masked edit",
    },
];

/// The first launch's frames: the Row's stack, Develop at Fit, 100% in a corner and with a mask,
/// removal and its undo, and the Wide's tile.
pub fn first_plan(expected: &Value) -> Result<Plan> {
    let row = &expected["sets"][ROW.name];
    let text = |value: &Value, what: &str| -> Result<String> {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("The expected answers name no {what}").into())
    };
    let row_folder = text(&row["folder"], "Row folder")?;
    let wide_folder = text(&expected["sets"][WIDE.name]["folder"], "Wide folder")?;
    let place = text(&row["place"]["path"], "catalog folder for the Row result")?;
    let position = row["place"]["position"]
        .as_u64()
        .ok_or("The expected answers place no Row result")? as u32;
    let select = |name: &str, step: SelectStep| Step::new(name, script::Step::Select(step));
    let key = |name: &str, key: &str| Step::new(name, script::Step::key(key));
    let click = |name: &str, position| {
        select(
            name,
            SelectStep::Click {
                position,
                shift: false,
                command: false,
            },
        )
    };
    Ok(Plan::new(vec![
        Step::opened("opened"),
        key("select", "g"),
        // TASK-010: the sheet's gestures and the merge go here (`PENDING`).
        select("row", SelectStep::Folder(row_folder.clone())),
        key("expanded", "s"),
        click("original", 2),
        click("clicked", 0),
        Step::new(
            "place",
            script::Step::Catalog(CatalogStep::Source(place.clone())),
        ),
        click("result", position),
        Step::new("develop", script::Step::Develop(DevelopStep::Active)),
        Step::new("fit", ViewStep::Fit).commits(0).fit(),
        Step::new("percent", ViewStep::Percent(100.0))
            .commits(0)
            .percent(100.0),
        Step::new("corner", script::Step::Pan { x: 1.0, y: 1.0 })
            .commits(0)
            .percent(100.0),
        Step::new("refit", ViewStep::Fit).commits(0).fit(),
        Step::new(
            "masked",
            script::Step::call(
                "mask.create-linear",
                json!({"x0": GRADIENT[0], "y0": GRADIENT[1], "x1": GRADIENT[2], "y1": GRADIENT[3]}),
            ),
        )
        .commits(1)
        .masks(1),
        Step::new(
            "lifted",
            script::Step::call(
                "edit.set-basic",
                json!({"mask": {"name": "Mask 1"}, "exposure": EXPOSURE}),
            ),
        )
        .commits(1)
        .label(MASKED_LABEL)
        .payload(BASIC_EFFECT, json!({ "exposure": EXPOSURE }))
        .fit(),
        Step::new("inspected", script::Step::call("merge.inspect", json!({}))),
        key("back", "g"),
        select("again", SelectStep::Folder(row_folder)),
        click("chosen", 0),
        Step::new("removed", script::Step::Catalog(CatalogStep::DeleteKey)),
        select("restored", SelectStep::Library(LibraryKey::Undo)),
        select("redone", SelectStep::Library(LibraryKey::Redo)),
        select("undone", SelectStep::Library(LibraryKey::Undo)),
        // TASK-010: the Wide's ⌥-click merge goes here (`PENDING`).
        select("wide", SelectStep::Folder(wide_folder)),
        // TASK-005: the Wide result in Develop and its export go here (`PENDING`).
    ]))
}

/// The second launch's frames, over the catalog the first left with the frames moved away: the
/// Row result's catalog folder, the result opened in Develop with its masked edit, and its merge
/// inspected.
pub fn second_plan(after: &Value) -> Result<Plan> {
    let row = &after["places"][ROW.name];
    let folder = row["path"]
        .as_str()
        .filter(|path| !path.is_empty())
        .ok_or("The first launch's catalog names no folder for the Row result")?;
    let position = row["position"]
        .as_u64()
        .ok_or("The first launch's catalog places no Row result")?;
    Ok(Plan::new(vec![
        Step::opened("opened"),
        Step::new("select", script::Step::key("g")),
        Step::new(
            "place",
            script::Step::Catalog(CatalogStep::Source(folder.to_owned())),
        ),
        Step::new(
            "result",
            script::Step::Select(SelectStep::Click {
                position: position as u32,
                shift: false,
                command: false,
            }),
        ),
        Step::new("reopened", script::Step::Develop(DevelopStep::Active))
            .payload(BASIC_EFFECT, json!({ "exposure": EXPOSURE }))
            .masks(1)
            .fit(),
        Step::new("inspected", script::Step::call("merge.inspect", json!({}))),
        // TASK-005: the Wide result reopened at Fit goes here (`PENDING`).
    ]))
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

// The setup's requests.

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

/// `params` with `folder` set to the plan's proposal, when it makes one.
fn into_proposal(mut params: Value, plan: &Value) -> Value {
    let folder = proposal(plan);
    if !folder.is_null() {
        params["folder"] = folder;
    }
    params
}

fn mutation(request: &str) -> Value {
    json!({"request_id": request, "actor": "setup"})
}

/// One set merged as the panorama sheet merges it: the plan, the preview, Boundary Warp changed,
/// then Merge with the preview. Answers every request as sent, the plan, both revisions and the
/// merge's job with the progress it showed.
fn merge_through_preview(
    owner: &OwnerHandle,
    client: ClientId,
    frames: &[PathBuf],
) -> Result<Value> {
    let targets = json!({"kind": "paths", "paths": frames});
    let mut sent = Vec::new();
    let mut send = |method: &str, params: Value| -> Result<Value> {
        sent.push(json!({"method": method, "params": params}));
        ask(owner, client, method, params)
    };
    let plan = send(
        "merge.plan",
        json!({"kind": "panorama", "targets": targets}),
    )?;
    ensure(
        plan["suitable"] == true,
        format!("The core refused the Row: {plan}"),
    )?;
    let opened = send(
        "merge.preview",
        json!({"kind": "panorama", "targets": targets, "plan_id": plan["plan_id"]}),
    )?;
    let first = follow(owner, client, &opened["job_id"], PREVIEW_DEADLINE)?;
    let preview_id = first["record"]["result"]["preview_id"].clone();
    let options = json!({ "boundary_warp": WARP });
    let updated = send(
        "merge.preview-update",
        json!({"preview_id": preview_id, "panorama": options}),
    )?;
    let second = follow(owner, client, &updated["job_id"], PREVIEW_DEADLINE)?;
    let created = send(
        "merge.create",
        into_proposal(
            json!({"kind": "panorama", "targets": targets, "preview_id": preview_id,
                "panorama": options, "mutation": mutation("setup-row")}),
            &plan,
        ),
    )?;
    let job = follow(owner, client, &created["job_id"], MERGE_DEADLINE)?;
    Ok(json!({"plan": plan, "sent": sent,
        "previews": [first["record"]["result"], second["record"]["result"]],
        "job": job, "result": job["record"]["result"]}))
}

/// One set merged as ⌥-click merges it: the plan with the last settings, then the merge at once.
fn merge_at_once(owner: &OwnerHandle, client: ClientId, frames: &[PathBuf]) -> Result<Value> {
    let targets = json!({"kind": "paths", "paths": frames});
    let options = json!({ "boundary_warp": WARP });
    let mut sent = Vec::new();
    let mut send = |method: &str, params: Value| -> Result<Value> {
        sent.push(json!({"method": method, "params": params}));
        ask(owner, client, method, params)
    };
    let plan = send(
        "merge.plan",
        json!({"kind": "panorama", "targets": targets, "panorama": options}),
    )?;
    ensure(
        plan["suitable"] == true,
        format!("The core refused the Wide row: {plan}"),
    )?;
    let created = send(
        "merge.create",
        into_proposal(
            json!({"kind": "panorama", "targets": targets, "plan_id": plan["plan_id"],
                "panorama": options, "mutation": mutation("setup-wide")}),
            &plan,
        ),
    )?;
    let job = follow(owner, client, &created["job_id"], MERGE_DEADLINE)?;
    Ok(json!({"plan": plan, "sent": sent, "job": job, "result": job["record"]["result"]}))
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

/// Write both sets, make the catalog with their folder indexed, and merge them as the desktop's
/// sheet and ⌥-click would. Answers the expected record and every frame, Row then Wide.
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
    let sets = with_owner(&catalog, |owner, client| {
        let added = ask(
            owner,
            client,
            "index.add-folder",
            json!({"path": frames, "mutation": mutation("setup-frames")}),
        )?;
        if !added["job_id"].is_null() {
            follow(owner, client, &added["job_id"], PREVIEW_DEADLINE)?;
        }
        let mut merged_row = merge_through_preview(owner, client, &row)?;
        let mut merged_wide = merge_at_once(owner, client, &wide)?;
        for (merged, set, paths) in [
            (&mut merged_row, ROW, &row),
            (&mut merged_wide, WIDE, &wide),
        ] {
            let filed = place(owner, client, &merged["result"]["result_asset_id"])?;
            merged["place"] = filed;
            merged["folder"] = json!(frames.join(set.name));
            merged["frames"] = json!(paths);
            merged["estimate"] = json!(stitched_estimate(&set));
        }
        Ok(json!({ ROW.name: merged_row, WIDE.name: merged_wide }))
    })?;
    Ok((
        json!({"sets": sets, "warp": WARP}),
        row.into_iter().chain(wide).collect(),
    ))
}

/// The Wide result exported through the core into `destination`: its source prepared, then
/// `export.jpeg`, which renders a paged source through the banded reference; with the file's own
/// header size.
fn export(catalog: &Path, result: &Value, destination: &Path) -> Result<Value> {
    with_owner(catalog, |owner, client| {
        let prepared = ask(owner, client, "source.prepare", json!({"asset_id": result}))?;
        if !prepared["job_id"].is_null() {
            follow(owner, client, &prepared["job_id"], PREVIEW_DEADLINE)?;
        }
        let answer = ask(
            owner,
            client,
            "export.jpeg",
            json!({"asset_id": result, "destination": destination,
                "mutation": mutation("setup-export")}),
        )?;
        let job = follow(owner, client, &answer["job_id"], MERGE_DEADLINE)?;
        let (width, height) = image::image_dimensions(destination)?;
        Ok(json!({"answer": answer, "job": job,
            "file": {"width": width, "height": height, "bytes": fs::metadata(destination)?.len()}}))
    })
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

/// Make the frames, the catalog and the merges (a replay reads the recorded answers), launch the
/// editor over them, read the catalog it left, export the Wide result, take the frames offline,
/// launch it again, and check every frame.
pub fn run(mut run: Run, scenario: &'static Scenario, _: Vec<PathBuf>) -> Result {
    let generated = run.out().join(GENERATED);
    let catalog = generated.join(generate_catalog::CATALOG);
    let expected_file = run.out().join(EXPECTED);
    let first_file = run.out().join(AFTER_FIRST);
    let export_file = run.out().join(EXPORTED);
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
        let first = first_plan(&expected)?;
        let launched = run.launch(window(
            Launch::named("first")
                .catalog(&catalog)
                .script("first-script.json", first.script()),
        ))?;
        let first_checked = first.check(&launched.dir)?;
        let results = [ROW.name, WIDE.name].map(|name| {
            (
                name,
                expected["sets"][name]["result"]["result_asset_id"].clone(),
            )
        });
        if !run.replaying() {
            write_json(&first_file, &after(&catalog, &results)?)?;
            let exports = run.out().join(EXPORTS);
            fs::create_dir_all(&exports)?;
            let exported = export(&catalog, &results[1].1, &exports.join(EXPORT_FILE))?;
            write_json(&export_file, &exported)?;
        }
        let after_first = read_json(&recorded(run, &first_file))?;
        let second = second_plan(&after_first)?;
        let online = generated.join(FRAMES);
        let offline = generated.join(OFFLINE);
        if !run.replaying() {
            fs::rename(&online, &offline)?;
        }
        let launched = run.launch(window(
            Launch::named("second")
                .catalog(&catalog)
                .script("second-script.json", second.script()),
        ));
        if !run.replaying() {
            fs::rename(&offline, &online)?;
        }
        let second_checked = second.check(&launched?.dir)?;
        if !run.replaying() {
            write_json(&after_file, &after(&catalog, &results)?)?;
        }
        (scenario.verify)(run, &[first_checked, second_checked])?;
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

/// The setup's merges: each plan, the Row's preview revisions, each request as the desktop will
/// send it, each job's progress and each result where the plan proposed.
fn check_setup(expected: &Value, checks: &mut Checks, first: &Checked) -> Result {
    let row = &expected["sets"][ROW.name];
    let wide = &expected["sets"][WIDE.name];
    for (set, merged) in [(ROW, row), (WIDE, wide)] {
        let plan = &merged["plan"];
        ensure(
            plan["kind"] == "panorama"
                && plan["suitable"] == true
                && plan["inputs"].as_array().map(Vec::len) == Some(set.frames),
            format!("The core plans the {} as {plan}", set.name),
        )?;
        let result = &merged["result"];
        ensure(
            result["kind"] == "panorama"
                && result["merged"] == true
                && result["reused"] == false
                && result["inputs"] == set.frames
                && result["result_asset_id"].is_string()
                && result["projection"].is_string()
                && result["field_of_view_deg"].is_array()
                && result["file_name"]
                    .as_str()
                    .is_some_and(|name| name.ends_with(" Panorama")),
            format!("The {} merge answered {result}", set.name),
        )?;
        ensure(
            in_proposed_folder(plan, &merged["place"]),
            format!(
                "The {} result is filed in {}, the plan proposed {}",
                set.name,
                merged["place"],
                proposal(plan)
            ),
        )?;
        let progress: Vec<&str> = merged["job"]["progress"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        ensure(
            progress.iter().any(|stage| stage.starts_with("Reading "))
                && progress.iter().any(|stage| stage.starts_with("Stitching ")),
            format!("The {} merge showed the progress {progress:?}", set.name),
        )?;
    }

    // The sheet's requests: plan, preview, Boundary Warp, Merge with the preview.
    let methods: Vec<&Value> = row["sent"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|sent| &sent["method"])
        .collect();
    ensure(
        methods
            == [
                &json!("merge.plan"),
                &json!("merge.preview"),
                &json!("merge.preview-update"),
                &json!("merge.create"),
            ],
        format!("The Row's merge sent {methods:?}"),
    )?;
    let [opened, warped] = [&row["previews"][0], &row["previews"][1]];
    let area = |revision: &Value| {
        revision["output"]["width"].as_u64().unwrap_or(0)
            * revision["output"]["height"].as_u64().unwrap_or(0)
    };
    let overlays = &opened["overlays"]["panorama"];
    ensure(
        opened["kind"] == "panorama"
            && opened["revision"] == 1
            && opened["path"].is_string()
            && opened["refusal"].is_null()
            && opened["diagnostics"]["panorama"]["projection"].is_string()
            && overlays["crop"].as_array().map(Vec::len) == Some(4)
            && overlays["covered"]
                .as_array()
                .is_some_and(|covered| !covered.is_empty())
            && overlays["frames"].as_array().map(Vec::len) == Some(ROW.frames),
        format!("The Row's preview opened as {opened}"),
    )?;
    let loss = |revision: &Value| revision["diagnostics"]["panorama"]["crop_loss"].as_f64();
    ensure(
        warped["preview_id"] == opened["preview_id"]
            && warped["revision"] == 2
            && warped["options"]["panorama"]["boundary_warp"] == WARP
            && warped["refusal"].is_null()
            && loss(warped).zip(loss(opened)).is_some_and(|(w, o)| w <= o)
            && area(warped) >= area(opened),
        format!("Boundary Warp {WARP} drew {warped}"),
    )?;
    let create = &row["sent"][3]["params"];
    ensure(
        create["preview_id"] == opened["preview_id"]
            && create.get("plan_id").is_none()
            && create["panorama"] == json!({ "boundary_warp": WARP }),
        format!("The Row's Merge sent {create}"),
    )?;
    // ⌥-click's requests: the plan and the merge at once, with the last settings.
    let create = &wide["sent"][1]["params"];
    ensure(
        wide["sent"].as_array().map(Vec::len) == Some(2)
            && create["plan_id"] == wide["plan"]["plan_id"]
            && create.get("preview_id").is_none()
            && create["panorama"] == json!({ "boundary_warp": WARP }),
        format!("The Wide's merge sent {}", wide["sent"]),
    )?;
    let [width, height] = [0, 1].map(|at| wide["estimate"][at].as_f64().unwrap_or(0.0));
    checks.note(
        first.at("opened")?,
        "setup: the merges the desktop's sheet and ⌥-click will send, through the core",
        json!({"row": {"previews": row["previews"], "progress": row["job"]["progress"],
                "seconds": row["job"]["seconds"]},
            "wide": {"progress": wide["job"]["progress"], "seconds": wide["job"]["seconds"],
                "estimate": [width, height]}}),
    );
    Ok(())
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [first, second] = launches else {
        return Err(format!("panorama-merge has two launches, not {}", launches.len()).into());
    };
    let expected = read_json(&recorded(run, &run.out().join(EXPECTED)))?;
    let after_first = read_json(&recorded(run, &run.out().join(AFTER_FIRST)))?;
    let exported = read_json(&recorded(run, &run.out().join(EXPORTED)))?;
    let after = read_json(&recorded(run, &run.out().join(AFTER)))?;
    let mut checks = Checks::new();
    check_setup(&expected, &mut checks, first)?;
    let row_answer = &expected["sets"][ROW.name]["result"];
    let row_result = row_answer["result_asset_id"].clone();
    let wide_result = expected["sets"][WIDE.name]["result"]["result_asset_id"].clone();

    // The Row's folder: one collapsed panorama tile.
    collapsed_leader(first.at("row")?, "row", &ROW, &row_result)?;

    // Expanded: the result, then its four originals.
    let expanded = first.at("expanded")?;
    let rows = stacks(expanded);
    ensure(
        rows.len() == ROW.frames + 1
            && rows[0]["role"] == "leader"
            && rows[0]["item"]["asset_id"] == row_result
            && rows[0]["expansion"] == "expanded"
            && rows[0]["badge"] == badge(ROW.frames, true)
            && rows
                .iter()
                .skip(1)
                .all(|row| row["role"] == "original" && row["kind"] == "panorama")
            && select(expanded)["merge"]["stack"]["expansion"] == "expanded",
        format!("expanded: the stack shows {rows:?}"),
    )?;
    // An original's Info band names its place in the stack's order.
    let band = &select(first.at("original")?)["info"]["stack"];
    let title = band["title"].as_str().unwrap_or_default();
    ensure(
        band["leader"] == false
            && (1..=ROW.frames).any(|k| title == format!("Frame {k} of {}", ROW.frames)),
        format!("original: the Info band is {band}"),
    )?;
    let band = &select(first.at("clicked")?)["info"]["stack"];
    ensure(
        band["leader"] == true && band["title"] == format!("Panorama \u{b7} {} photos", ROW.frames),
        format!("clicked: the Info band is {band}"),
    )?;

    // Develop: the result from its saved source, at Fit, at 100% in its far corner, and edited
    // through a linear mask.
    let opened = first.at("develop")?;
    ensure(
        develop(opened)["document"]["asset_id"] == row_result
            && develop(opened)["document"]["source"] == "derived"
            && develop(opened)["document"]["title"] == row_answer["file_name"]
            && select(opened)["shown"] == "develop",
        format!("develop: Develop shows {}", develop(opened)["document"]),
    )?;
    for name in ["fit", "corner", "refit"] {
        let frame = first.at(name)?;
        frame.photo()?;
        checks.note(
            frame,
            "the result drawn",
            json!({"view": frame.state()["surface"]["view"],
                "gpu": frame.state()["surface"]["gpu"]["drawing_path"]}),
        );
    }
    let (refit, lifted) = (first.at("refit")?, first.at("lifted")?);
    checks.compare(
        lifted,
        "lifted: one stop through the mask lifts its covered side",
        patch(lifted, COVERED)?,
        patch(refit, COVERED)?,
        Tolerance::Above(LIFT),
    )?;
    checks.compare(
        lifted,
        "lifted: the mask leaves its uncovered side",
        patch(lifted, UNCOVERED)?,
        patch(refit, UNCOVERED)?,
        Tolerance::Within(UNTOUCHED),
    )?;
    let inspected = &first.at("inspected")?["step"]["result"];
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

    // Remove merge, then its undo, redo and undo again: the originals and the result in turn.
    let removed = first.at("removed")?;
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
        let frame = first.at(name)?;
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
            leader.is_some_and(|row| row["item"]["asset_id"] == row_result) == shown,
            format!("{name}: the stacked rows are {:?}", stacks(frame)),
        )?;
    }

    // The Wide's folder: one collapsed tile of ten.
    collapsed_leader(first.at("wide")?, "wide", &WIDE, &wide_result)?;
    checks.note(
        first.at("wide")?,
        "wide: the paged result's tile",
        json!({"stacks": stacks(first.at("wide")?)}),
    );

    // The catalog the first launch left: both merges active and nothing merged again.
    let merges = &after_first["merges"];
    for name in [ROW.name, WIDE.name] {
        ensure(
            merges[name]["state"] == "active" && merges[name]["source"]["status"] == "available",
            format!("The first launch left the {name} merge as {}", merges[name]),
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
        count(&after_first, "merge.create") == 2,
        format!("The journal records {}", after_first["changes"]),
    )?;
    ensure(
        after_first["history"].as_array().map_or(0, Vec::len) >= 3,
        format!("The Row result's history holds {}", after_first["history"]),
    )?;

    // The Wide result exported past the resident limits, through the reference until the GPU
    // streams paged sources.
    let file = &exported["file"];
    let pixels = file["width"].as_u64().unwrap_or(0) * file["height"].as_u64().unwrap_or(0);
    let renderer = &exported["job"]["record"]["result"]["renderer"];
    ensure(
        pixels > RESIDENT_PIXELS
            && file["width"] == exported["answer"]["width"]
            && file["height"] == exported["answer"]["height"]
            && (renderer.is_null() || renderer["record"] == "reference"),
        format!("The Wide export wrote {file} by {renderer}"),
    )?;

    // Reopened with the frames moved away: the Row result from its saved source with its edit.
    let reopened = second.at("reopened")?;
    ensure(
        develop(reopened)["document"]["asset_id"] == row_result
            && develop(reopened)["document"]["title"] == row_answer["file_name"],
        format!("reopened: Develop shows {}", develop(reopened)["document"]),
    )?;
    checks.compare(
        reopened,
        "reopened: the covered side, frames offline, against the edited one",
        patch(reopened, COVERED)?,
        patch(lifted, COVERED)?,
        Tolerance::Within(REOPEN_TOLERANCE),
    )?;
    let offline = &second.at("inspected")?["step"]["result"];
    ensure(
        offline["result_asset_id"] == row_result
            && offline["state"] == "active"
            && offline["source"]["status"] == "available"
            && offline["inputs"]
                .as_array()
                .is_some_and(|inputs| inputs.iter().all(|input| input["file"] == "missing")),
        format!("inspected: with the frames offline merge.inspect answered {offline}"),
    )?;
    // Named back: the same two merges, nothing merged again.
    ensure(
        [ROW.name, WIDE.name]
            .iter()
            .all(|name| after["merges"][name]["state"] == "active")
            && after["changes"].as_array().map(Vec::len)
                == after_first["changes"].as_array().map(Vec::len),
        format!("After the second launch the catalog holds {after}"),
    )?;
    checks.write(
        &first.evidence,
        SCENARIO,
        json!({"row": row_result, "wide": wide_result, "export": exported["file"],
            "tolerance": {"lift": LIFT, "untouched": UNTOUCHED, "reopen": REOPEN_TOLERANCE},
            "pending": pending()}),
    )?;
    match PENDING {
        [] => Ok(()),
        _ => Err(format!(
            "panorama-merge passed every step this build can take; {} steps of item 5 wait: {}",
            PENDING.len(),
            [Awaits::Task010, Awaits::Task005]
                .map(|awaits| {
                    let steps: Vec<&str> = PENDING
                        .iter()
                        .filter(|pending| pending.awaits == awaits)
                        .map(|pending| pending.after)
                        .collect();
                    format!("{} after {}", awaits.task(), steps.join(", "))
                })
                .join("; ")
        )
        .into()),
    }
}

/// The steps still waiting, as the checks file records them.
fn pending() -> Value {
    PENDING
        .iter()
        .map(|pending| {
            json!({"after": pending.after, "awaits": pending.awaits.task(),
                "gesture": pending.gesture, "expects": pending.expects})
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_test_macros::slow;

    fn expected() -> Value {
        json!({"sets": {
            ROW.name: {"folder": "/tmp/Frames/Row",
                "place": {"path": "2026 \u{203a} 13 Oct", "position": 0}},
            WIDE.name: {"folder": "/tmp/Frames/Wide"},
        }})
    }

    #[test]
    fn the_panorama_merge_plans_are_well_formed() {
        let first = first_plan(&expected()).unwrap();
        first.validate().unwrap();
        assert!(first.scripted());
        assert_eq!(first.len(), 24);
        let second = second_plan(
            &json!({"places": {ROW.name: {"path": "2026 \u{203a} 13 Oct", "position": 1}}}),
        )
        .unwrap();
        second.validate().unwrap();
        assert_eq!(second.len(), 6);
        // Registered, opening nothing a checkout lacks, so `verify --tier rendered` runs it.
        let row = crate::smoke::find(SCENARIO).unwrap();
        assert!(row.rendered() && !row.takes_source());
    }

    #[test]
    fn every_pending_step_names_its_place_task_and_gesture() {
        let first = first_plan(&expected()).unwrap();
        let second = second_plan(
            &json!({"places": {ROW.name: {"path": "2026 \u{203a} 13 Oct", "position": 1}}}),
        )
        .unwrap();
        // Steps a pending row adds are named by the row before them.
        let added = [
            "previewing",
            "warped",
            "overlaid",
            "wide_fit",
            "wide_corner",
            "wide_masked",
        ];
        for pending in PENDING {
            let (launch, step) = pending.after.split_once('/').unwrap();
            let plan = match launch {
                "first" => &first,
                "second" => &second,
                other => panic!("no launch {other}"),
            };
            assert!(
                plan.index(step).is_some() || added.contains(&step),
                "{} names no step",
                pending.after
            );
            assert!(!pending.expects.is_empty());
            if pending.gesture.starts_with(['[', '{']) {
                serde_json::from_str::<Value>(pending.gesture).unwrap();
            }
        }
        assert!(
            PENDING
                .iter()
                .any(|pending| pending.awaits == Awaits::Task010)
        );
        assert!(
            PENDING
                .iter()
                .any(|pending| pending.awaits == Awaits::Task005)
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
                follow(owner, client, &opened["job_id"], PREVIEW_DEADLINE)
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
