//! The `mask-linear` smoke scenario: phase A's whole slice on the real editor, in two launches.
//!
//! Launch 1 enters Mask mode, draws a linear gradient with one sweep, commits it, lifts Exposure
//! through it with a slider gesture, edits the same masked layer again from JSON — naming the mask
//! by the name the host gave it, which is the only way a script can name a mask `mask.create-linear`
//! assigned the identity of — turns the coverage overlay on and off again, and undoes. Launch 2
//! reopens the same catalog in a new process and walks the history.
//!
//! Every frame is checked against the photograph's own measured change, not only against the state
//! it recorded: the gradient runs down the picture, so the top of the frame sits at coverage 0 and
//! must stay where it was while the bottom is lifted.
use crate::{
    smoke::{self, columns, frame_identity},
    *,
};
use std::time::Duration;

pub const SCENARIO: &str = "mask-linear";
/// The golden four-quadrant fixture, unrotated: content and output coordinates coincide, so the
/// normalized positions the script sweeps through are the ones the capture is measured at.
const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
/// The reference window every panelled scenario is written against.
const WINDOW: [&str; 2] = workspace_smoke::WINDOW;

const BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";
/// Where the gradient's two ends sit, in normalized content coordinates: coverage 0 above `Y0`,
/// coverage 1 below `Y1`.
const Y0: f64 = 0.30;
const Y1: f64 = 0.70;
/// The exposure the slider gesture commits, then what the JSON client raises it to.
const DRAGGED: f64 = 2.0;
const RETYPED: f64 = 2.5;

/// Where the two measured patches sit inside the displayed photograph, as fractions of its own
/// drawn rectangle. The first is above the gradient's start and the second below its end, each
/// clear of the fixture's white centre stripe and its black dashes.
const UNCOVERED: [f64; 2] = [0.25, 0.12];
const COVERED: [f64; 2] = [0.25, 0.88];
/// Half the side of a measured patch, in capture pixels.
const PATCH_HALF: i64 = 6;
/// How far a patch's mean luminance must move before this scenario calls it lifted. A +2 EV lift of
/// a mid-blue quadrant moves it by tens of codes, so this is a margin, not a tuned threshold.
const LIFTED: f64 = 12.0;
/// How close two mean luminance readings must stay before this scenario calls a patch untouched.
/// Only JPEG-free renderer readback is compared, so this is tight on purpose.
const UNTOUCHED: f64 = 1.0;

/// Launch 1's own frames: the open plus one per script step.
const LAUNCH1_FRAMES: usize = 11;
/// Launch 2's: the reopen plus its two history steps.
const LAUNCH2_FRAMES: usize = 4;

fn launch1_script() -> Value {
    json!([
        // 1: Mask mode, through the same `workspace.set` the mode strip sends.
        {"workspace":{"mode":"mask"}},
        // 2: a new mask whose first component is a linear gradient: the gesture opens and drafts.
        {"mask":{"new":"linear"}},
        // 3: the drag itself, one sweep from the untouched side towards the affected one, with the
        // pointer still down: the frame is the picture mid-gesture.
        {"mask":{"sweep":{"from":[0.5,Y0],"to":[0.5,Y1]}}},
        // 4: the pointer lifted. The gradient stays; nothing is committed by a release.
        {"mask":{"release":true}},
        // 5: Apply: one history entry, one mask, no layer bound to it yet.
        {"mask":{"apply":true}},
        // 6: the masked Exposure gesture. The sections below the list are bound to the mask the
        // commit opened, so this is the panel's own drag on the masked layer.
        {"slider":{"action":BASIC,"parameter":EXPOSURE,"values":[0.8,1.4,DRAGGED],"release":true}},
        // 7: the same layer from JSON, naming the mask by the name the host gave it.
        {"api":{"method":"edit.set-basic","params":{"mask":{"name":"Mask 1"},"exposure":RETYPED}}},
        // 8: the coverage overlay on.
        {"workspace":{"mask_overlay":"tint"}},
        // 9: and off again.
        {"workspace":{"mask_overlay":"off"}},
        // 10: undo, back to the exposure the drag committed.
        {"api":{"method":"history.undo","params":{}}}
    ])
}

fn launch2_script() -> Value {
    json!([
        // 1: Mask mode again, so the reopened masks are shown as well as stored.
        {"workspace":{"mode":"mask"}},
        // 2: the entry the gradient was committed in, selected from the history.
        {"preview":{"sequence":1}},
        // 3: back to the current state.
        {"preview":"current"}
    ])
}

fn revision(frame: &Value) -> Result<u64> {
    frame["state"]["stack"]["revision"]
        .as_u64()
        .ok_or_else(|| "Frame records no revision".into())
}

fn label(frame: &Value) -> Result<&str> {
    frame["state"]["stack"]["label"]
        .as_str()
        .ok_or_else(|| "Frame records no history label".into())
}

fn masks(frame: &Value) -> Result<&Vec<Value>> {
    frame["state"]["masks"]["masks"]
        .as_array()
        .ok_or_else(|| "Frame records no mask list".into())
}

/// The one mask the scenario draws, with its components, or an error naming what was listed.
fn only_mask(frame: &Value) -> Result<&Value> {
    let masks = masks(frame)?;
    ensure(
        masks.len() == 1,
        format!("Expected exactly one mask, found {}", json!(masks)),
    )?;
    Ok(&masks[0])
}

fn mask_id(frame: &Value) -> Result<&str> {
    only_mask(frame)?["id"]
        .as_str()
        .ok_or_else(|| "The listed mask has no identity".into())
}

/// Every component of the open mask, as the panel derived them.
fn components(frame: &Value) -> Result<&Vec<Value>> {
    frame["state"]["masks"]["components"]
        .as_array()
        .ok_or_else(|| "Frame records no component list".into())
}

/// The stack's one Basic layer, or `None` when the stack holds none.
fn basic_layer(frame: &Value) -> Option<&Value> {
    frame["state"]["stack"]["layers"]
        .as_array()?
        .iter()
        .find(|layer| layer["effect"] == json!(lightwell_core::BASIC_EFFECT))
}

fn exposure(frame: &Value) -> Option<f64> {
    basic_layer(frame)?["payload"][EXPOSURE].as_f64()
}

/// The photograph's own drawn rectangle inside the capture.
///
/// The two top quadrant colours are what the mask never touches — the gradient reaches coverage 0
/// above `Y0` — so the extent is found from those alone and the rest of the rectangle derived from
/// the fixture's own 3:2 aspect. A bounds search over all four colours would lose the bottom of the
/// picture the moment the masked layer lifted it.
fn photo_bounds(path: &Path, frame: &Value) -> Result<[u32; 4]> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [surface_left, surface_right] = columns(frame)?.unwrap_or([0, width]);
    ensure(
        surface_left < surface_right && surface_right <= width,
        "Invalid surface columns",
    )?;
    // `orientation-1` draws the quadrants in the fixture generator's own order.
    let top = crate::fixtures::ORDERS[0][..2]
        .iter()
        .map(|index| crate::fixtures::COLORS[*index])
        .collect::<Vec<_>>();
    let matches = |pixel: &[u8]| {
        top.iter()
            .any(|colour| pixel.iter().zip(*colour).all(|(a, b)| a.abs_diff(b) <= 8))
    };
    let (mut left, mut right, mut first) = (width, 0u32, None);
    for y in 0..height {
        let row: Vec<u32> = (surface_left..surface_right)
            .filter(|x| matches(&image.get_pixel(*x, y).0))
            .collect();
        if row.len() < 32 {
            continue;
        }
        first.get_or_insert(y);
        left = left.min(*row.first().expect("a matched column"));
        right = right.max(*row.last().expect("a matched column") + 1);
    }
    let top_edge = first.ok_or("No top-quadrant fixture pixels in the capture")?;
    ensure(
        right > left && right - left > 100,
        format!("Photograph too narrow to measure: {left}..{right}"),
    )?;
    // 480 × 320 at Fit: the drawn height follows from the measured width and the source's ratio.
    let drawn = f64::from(right - left) * 320.0 / 480.0;
    let bottom = top_edge + drawn.round() as u32;
    ensure(bottom <= height, "Photograph runs past the capture")?;
    Ok([left, top_edge, right, bottom])
}

/// Mean Rec. 709 luminance of one small patch of the displayed photograph.
fn patch_luminance(path: &Path, bounds: [u32; 4], at: [f64; 2]) -> Result<f64> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [left, top, right, bottom] = bounds;
    let px = f64::from(left) + at[0] * f64::from(right - left);
    let py = f64::from(top) + at[1] * f64::from(bottom - top);
    let mut total = 0.0;
    let mut count = 0u32;
    for dy in -PATCH_HALF..=PATCH_HALF {
        for dx in -PATCH_HALF..=PATCH_HALF {
            let x = (px as i64 + dx).clamp(0, i64::from(width) - 1) as u32;
            let y = (py as i64 + dy).clamp(0, i64::from(height) - 1) as u32;
            let p = image.get_pixel(x, y).0;
            total += 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
            count += 1;
        }
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(total / f64::from(count))
}

/// Both measured patches of one capture: above the gradient, and below it.
fn patches(path: &Path, frame: &Value) -> Result<[f64; 2]> {
    let bounds = photo_bounds(path, frame)?;
    Ok([
        patch_luminance(path, bounds, UNCOVERED)?,
        patch_luminance(path, bounds, COVERED)?,
    ])
}

fn lifted(what: &str, after: f64, before: f64) -> Result {
    ensure(
        after > before + LIFTED,
        format!("{what}: {after:.2} is not lifted above {before:.2} by {LIFTED}"),
    )
}

fn untouched(what: &str, after: f64, before: f64) -> Result {
    ensure(
        (after - before).abs() <= UNTOUCHED,
        format!("{what}: {after:.2} moved from {before:.2} by more than {UNTOUCHED}"),
    )
}

/// The whole scenario: two launches over one catalog, checked together.
pub fn run(root: &Path, out: &Path, bin: &Path, timeout: Duration) -> Result {
    ensure(!out.exists(), "Smoke output must be new")?;
    fs::create_dir_all(out)?;
    let fixture = root.join(FIXTURE);
    let launch1 = out.join("launch1");
    let launch2 = out.join("launch2");
    let mut result = json!({"scenario":SCENARIO,"status":"failed","launch_mode":launch::MODE,"platform":format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH)});
    let check = (|| -> Result {
        result["fixture_hash"] = json!(hash(&fixture)?);
        result["binary_sha256"] = json!(hash(bin)?);
        result["lockfile_sha256"] = json!(hash(&root.join("Cargo.lock"))?);

        let script1 = out.join("script1.json");
        write_json(&script1, &launch1_script())?;
        let args1: Vec<OsString> = vec![
            "--evidence-dir".into(),
            launch1.clone().into_os_string(),
            "--open".into(),
            fixture.clone().into_os_string(),
            "--evidence-script".into(),
            script1.into_os_string(),
            "--window-size".into(),
            WINDOW[0].into(),
            WINDOW[1].into(),
        ];
        let mut child1 = smoke::spawn_editor(root, bin, &args1, &out.join("launch1.log"))?;
        let status1 = smoke::wait(&mut child1, timeout)?;
        result["launch1_exit_code"] = json!(status1.code());
        ensure(status1.success(), format!("Launch 1 exit {status1}"))?;
        let (app1, _) = smoke::preamble(&launch1, LAUNCH1_FRAMES)?;
        let checks = verify_launch1(&launch1, &app1)?;
        result["launch1"] = checks.clone();

        // Launch 2: the same catalog and the same file, in a new process.
        let catalog = launch1.join("catalog.sqlite");
        ensure(catalog.is_file(), "Launch 1 wrote no catalog")?;
        let script2 = out.join("script2.json");
        write_json(&script2, &launch2_script())?;
        let args2: Vec<OsString> = vec![
            "--catalog".into(),
            catalog.into_os_string(),
            "--evidence-dir".into(),
            launch2.clone().into_os_string(),
            "--open".into(),
            fixture.clone().into_os_string(),
            "--evidence-script".into(),
            script2.into_os_string(),
            "--window-size".into(),
            WINDOW[0].into(),
            WINDOW[1].into(),
        ];
        let mut child2 = smoke::spawn_editor(root, bin, &args2, &out.join("launch2.log"))?;
        let status2 = smoke::wait(&mut child2, timeout)?;
        result["launch2_exit_code"] = json!(status2.code());
        ensure(status2.success(), format!("Launch 2 exit {status2}"))?;
        let (app2, _) = smoke::preamble(&launch2, LAUNCH2_FRAMES)?;
        result["launch2"] = verify_launch2(&launch2, &app2, &checks)?;
        ensure(
            json!(hash(&fixture)?) == result["fixture_hash"],
            "Source changed",
        )?;
        write_json(&out.join("mask-linear-checks.json"), &result)?;
        Ok(())
    })();
    match &check {
        Ok(()) => result["status"] = json!("passed"),
        Err(error) => result["error"] = json!(error.to_string()),
    };
    write_json(&out.join("result.json"), &result)?;
    fs::write(
        out.join("reproduce.md"),
        format!(
            "# Smoke run\n\nScenario: {SCENARIO}. Status: {}.\n\nLaunch mode: {}. Reproduce with `cargo xtask smoke --scenario {SCENARIO} --output NEW_DIR --binary PATH`; on macOS each launch runs hidden in a background-only bundle, so no window is ever placed on the desktop.\n\nTwo launches over one catalog: the first draws and edits through a mask, the second reopens it.\n\nActual renderer readback. Synthetic fixtures only.\n",
            result["status"],
            launch::MODE
        ),
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    check
}

/// Launch 1, frame by frame. Returns the identities and measurements launch 2 is checked against.
fn verify_launch1(evidence: &Path, app: &Value) -> Result<Value> {
    ensure(
        app["had_input_errors"] == json!(false),
        format!("The run recorded an input error: {}", app["script"]),
    )?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?.clone();
    ensure(
        frames.len() == LAUNCH1_FRAMES,
        format!("Launch 1 wrote {} frames", frames.len()),
    )?;
    let paths: Vec<PathBuf> = frames
        .iter()
        .map(|frame| frame_identity(evidence, app, frame))
        .collect::<Result<Vec<_>>>()?;
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // Frame 0: the fixture as launched. No mask, no Basic layer, the pointer canvas mode.
    ensure(
        masks(&frames[0])?.is_empty() && basic_layer(&frames[0]).is_none(),
        "The fixture opened with a mask or a Basic layer already in the recipe",
    )?;
    let opened = patches(&paths[0], &frames[0])?;
    record(
        &frames[0],
        "the fixture as launched, with no mask in the recipe",
        json!({"patches":opened}),
    );

    // Frame 1: Mask mode. A canvas mode commits nothing.
    ensure(
        frames[1]["state"]["workspace"]["mode"] == json!("mask"),
        format!(
            "The workspace step did not enter Mask mode: {}",
            frames[1]["state"]["workspace"]
        ),
    )?;
    ensure(
        revision(&frames[1])? == revision(&frames[0])?,
        "Entering Mask mode committed something",
    )?;
    record(
        &frames[1],
        "Mask mode with an empty Masks panel",
        json!({"caption":frames[1]["state"]["masks"]["caption"]}),
    );

    // Frame 2: the gesture is open and drafting, and nothing is committed.
    let opened_draft = &frames[2]["state"]["mask_draft"];
    ensure(
        opened_draft["kind"] == json!("linear") && opened_draft["mask"] == Value::Null,
        format!("Frame 2 holds no open create gesture: {opened_draft}"),
    )?;
    ensure(
        revision(&frames[2])? == revision(&frames[0])? && masks(&frames[2])?.is_empty(),
        "Opening the gesture committed something",
    )?;
    record(
        &frames[2],
        "an open linear-gradient gesture on the canvas, drafted and uncommitted",
        json!({"draft":opened_draft}),
    );

    // Frame 3: the sweep put the gradient exactly where the script drew it, still uncommitted.
    let swept = &frames[3]["state"]["mask_draft"];
    ensure(
        swept["shape"] == json!({"x0":0.5,"y0":Y0,"x1":0.5,"y1":Y1})
            && swept["dragging"] == json!(true),
        format!("Frame 3's gesture holds {swept}"),
    )?;
    ensure(
        revision(&frames[3])? == revision(&frames[0])? && masks(&frames[3])?.is_empty(),
        "The sweep committed something",
    )?;
    record(
        &frames[3],
        "the gradient swept from the untouched side to the affected one, still a draft",
        json!({"draft":swept}),
    );

    // Frame 4: the pointer lifted. The gradient the sweep drew is still exactly where it was and
    // the gesture is still open, because a release commits nothing on its own.
    let released = &frames[4]["state"]["mask_draft"];
    ensure(
        released["shape"] == json!({"x0":0.5,"y0":Y0,"x1":0.5,"y1":Y1})
            && released["dragging"] == json!(false),
        format!("Frame 4's gesture holds {released}"),
    )?;
    ensure(
        revision(&frames[4])? == revision(&frames[0])? && masks(&frames[4])?.is_empty(),
        "The release committed something",
    )?;
    record(
        &frames[4],
        "the drawn gradient with the pointer lifted, still uncommitted",
        json!({"draft":released}),
    );

    // Frame 5: Apply. One entry, one mask, one linear component, no layer bound to it — so the
    // photograph is byte-unchanged: a mask on its own is a selection, not an edit.
    ensure(
        revision(&frames[5])? == revision(&frames[0])? + 1,
        format!("Apply moved the revision to {}", revision(&frames[5])?),
    )?;
    ensure(
        frames[5]["state"]["mask_draft"] == Value::Null,
        "The gesture is still open after Apply",
    )?;
    let mask = mask_id(&frames[5])?.to_owned();
    let mask_name = only_mask(&frames[5])?["name"]
        .as_str()
        .ok_or("The listed mask has no name")?
        .to_owned();
    let component = {
        let listed = components(&frames[5])?;
        ensure(
            listed.len() == 1 && listed[0]["kind"] == json!("linear"),
            format!("The committed mask holds {}", json!(listed)),
        )?;
        listed[0]["id"]
            .as_str()
            .ok_or("The component has no identity")?
            .to_owned()
    };
    ensure(
        only_mask(&frames[5])?["layers"] == json!([]),
        "A freshly drawn mask already has a layer bound to it",
    )?;
    let applied = patches(&paths[5], &frames[5])?;
    untouched(
        "a mask with no layer, above the gradient",
        applied[0],
        opened[0],
    )?;
    untouched(
        "a mask with no layer, below the gradient",
        applied[1],
        opened[1],
    )?;
    record(
        &frames[5],
        "the committed mask in the panel and the photograph unchanged by it",
        json!({"mask":mask,"name":mask_name,"component":component,"label":label(&frames[5])?,"patches":applied}),
    );

    // Frame 6: the masked Exposure gesture, released. The picture is lifted below the gradient and
    // untouched above it, and the layer the panel committed names the mask.
    ensure(
        revision(&frames[6])? == revision(&frames[5])? + 1,
        format!(
            "The gesture moved the revision to {}",
            revision(&frames[6])?
        ),
    )?;
    let layer = basic_layer(&frames[6]).ok_or("The masked gesture committed no Basic layer")?;
    ensure(
        layer["mask"] == json!(mask),
        format!("The committed Basic layer names {}", layer["mask"]),
    )?;
    ensure(
        exposure(&frames[6]) == Some(DRAGGED),
        format!("The committed layer holds {:?}", exposure(&frames[6])),
    )?;
    let layer_id = layer["id"]
        .as_str()
        .ok_or("The masked layer has no identity")?
        .to_owned();
    ensure(
        only_mask(&frames[6])?["layers"]
            .as_array()
            .is_some_and(|layers| layers.len() == 1),
        format!(
            "The mask lists {} bound layers",
            only_mask(&frames[6])?["layers"]
        ),
    )?;
    let dragged = patches(&paths[6], &frames[6])?;
    untouched(
        "above the gradient after the masked drag",
        dragged[0],
        opened[0],
    )?;
    lifted(
        "below the gradient after the masked drag",
        dragged[1],
        opened[1],
    )?;
    record(
        &frames[6],
        "one part of the photograph lifted through the mask and the other left alone",
        json!({"layer":layer_id,"exposure":DRAGGED,"patches":dragged,"label":label(&frames[6])?}),
    );

    // Frame 7: the same layer again from JSON, naming the mask by name. The step's own record must
    // show the identity the name resolved to, which is what makes the request reproducible.
    let step = app["script"]
        .as_array()
        .and_then(|steps| steps.get(6))
        .ok_or("Launch 1 recorded no seventh step")?;
    ensure(
        step["resolved"]["mask"] == json!(mask),
        format!(
            "The JSON step resolved {} rather than the mask",
            step["resolved"]
        ),
    )?;
    ensure(
        step["status"] != json!("failed"),
        format!("The JSON step failed: {step}"),
    )?;
    ensure(
        revision(&frames[7])? == revision(&frames[6])? + 1,
        format!(
            "The JSON edit moved the revision to {}",
            revision(&frames[7])?
        ),
    )?;
    ensure(
        basic_layer(&frames[7]).and_then(|layer| layer["id"].as_str()) == Some(layer_id.as_str()),
        "The JSON edit replaced the masked layer rather than updating it in place",
    )?;
    ensure(
        exposure(&frames[7]) == Some(RETYPED),
        format!("The JSON edit stored {:?}", exposure(&frames[7])),
    )?;
    let retyped = patches(&paths[7], &frames[7])?;
    untouched(
        "above the gradient after the JSON edit",
        retyped[0],
        opened[0],
    )?;
    lifted(
        "below the gradient after the JSON edit",
        retyped[1],
        dragged[1],
    )?;
    record(
        &frames[7],
        "the same masked layer raised again from JSON, by the mask's name",
        json!({"resolved":step["resolved"],"exposure":RETYPED,"patches":retyped}),
    );

    // Frame 8: the coverage overlay. It is drawn over the covered part of the picture only, so the
    // uncovered patch is exactly where it was and the covered one is not.
    ensure(
        frames[8]["state"]["workspace"]["mask_overlay"] == json!("tint"),
        format!(
            "The overlay step left the workspace at {}",
            frames[8]["state"]["workspace"]["mask_overlay"]
        ),
    )?;
    ensure(
        revision(&frames[8])? == revision(&frames[7])?,
        "Switching the overlay on committed something",
    )?;
    let tinted = patches(&paths[8], &frames[8])?;
    untouched(
        "above the gradient with the overlay on",
        tinted[0],
        retyped[0],
    )?;
    ensure(
        (tinted[1] - retyped[1]).abs() > UNTOUCHED,
        format!(
            "The overlay drew nothing over the covered part: {:.2} against {:.2}",
            tinted[1], retyped[1]
        ),
    )?;
    record(
        &frames[8],
        "the coverage overlay tinting exactly the covered part of the photograph",
        json!({"patches":tinted,"colour":frames[8]["state"]["workspace"]["mask_overlay_colour"]}),
    );

    // Frame 9: the overlay off again, and the photograph back to what it was under it.
    ensure(
        frames[9]["state"]["workspace"]["mask_overlay"] == json!("off"),
        "The overlay did not switch off",
    )?;
    let cleared = patches(&paths[9], &frames[9])?;
    untouched(
        "above the gradient with the overlay off",
        cleared[0],
        retyped[0],
    )?;
    untouched(
        "below the gradient with the overlay off",
        cleared[1],
        retyped[1],
    )?;
    record(
        &frames[9],
        "the overlay switched off, leaving the edited photograph",
        json!({"patches":cleared}),
    );

    // Frame 10: undo. One entry back: the masked layer holds what the drag committed again, the
    // mask and its component keep their identities, and the picture follows.
    ensure(
        revision(&frames[10])? == revision(&frames[9])? + 1,
        "Undo did not advance the revision",
    )?;
    ensure(
        exposure(&frames[10]) == Some(DRAGGED),
        format!("Undo left the layer at {:?}", exposure(&frames[10])),
    )?;
    ensure(
        mask_id(&frames[10])? == mask && components(&frames[10])?[0]["id"] == json!(component),
        "Undo changed the mask or component identity",
    )?;
    let undone = patches(&paths[10], &frames[10])?;
    untouched("above the gradient after undo", undone[0], opened[0])?;
    untouched("below the gradient after undo", undone[1], dragged[1])?;
    record(
        &frames[10],
        "the undone state: the drag's exposure again, through the same mask",
        json!({"patches":undone,"label":label(&frames[10])?}),
    );

    Ok(json!({
        "mask": mask,
        "mask_name": mask_name,
        "component": component,
        "layer": layer_id,
        "revision": revision(&frames[10])?,
        "label": label(&frames[10])?,
        "exposure": DRAGGED,
        "patches": {"opened": opened, "dragged": dragged, "undone": undone},
        "lifted_margin": LIFTED,
        "untouched_tolerance": UNTOUCHED,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of two patches of the displayed photograph, read back from the renderer; not a colorimetric claim",
    }))
}

/// Launch 2: the same catalog in a new process. Identities, bindings and history navigation.
fn verify_launch2(evidence: &Path, app: &Value, launch1: &Value) -> Result<Value> {
    ensure(
        app["had_input_errors"] == json!(false),
        format!("The reopen recorded an input error: {}", app["script"]),
    )?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?.clone();
    ensure(
        frames.len() == LAUNCH2_FRAMES,
        format!("Launch 2 wrote {} frames", frames.len()),
    )?;
    let paths: Vec<PathBuf> = frames
        .iter()
        .map(|frame| frame_identity(evidence, app, frame))
        .collect::<Result<Vec<_>>>()?;

    // Frame 0: the reopen itself. Revision, label, mask, component, bound layer and the stored
    // exposure all come back with the identities launch 1 wrote.
    let reopened = &frames[0];
    ensure(
        json!(revision(reopened)?) == launch1["revision"],
        format!(
            "Launch 2 reopened at revision {} rather than {}",
            revision(reopened)?,
            launch1["revision"]
        ),
    )?;
    ensure(
        json!(label(reopened)?) == launch1["label"],
        format!("Launch 2's history label is {:?}", label(reopened)?),
    )?;
    ensure(
        json!(mask_id(reopened)?) == launch1["mask"]
            && only_mask(reopened)?["name"] == launch1["mask_name"],
        format!("Launch 2 reopened the mask as {}", only_mask(reopened)?),
    )?;
    let layer = basic_layer(reopened).ok_or("Launch 2 reopened without the masked layer")?;
    ensure(
        layer["id"] == launch1["layer"] && layer["mask"] == launch1["mask"],
        format!("Launch 2's masked layer is {layer}"),
    )?;
    ensure(
        json!(exposure(reopened)) == launch1["exposure"],
        format!("Launch 2 reopened at exposure {:?}", exposure(reopened)),
    )?;

    // Frame 1: Mask mode, where the components are derived. The component keeps its identity too,
    // and the panel names the layer bound to the mask.
    let listed = components(&frames[1])?;
    ensure(
        listed.len() == 1 && listed[0]["id"] == launch1["component"],
        format!("Launch 2 reopened the components as {}", json!(listed)),
    )?;
    ensure(
        only_mask(&frames[1])?["layers"]
            .as_array()
            .is_some_and(|layers| layers.len() == 1),
        format!(
            "The reopened mask lists {} bound layers",
            only_mask(&frames[1])?["layers"]
        ),
    )?;
    let reopened_patches = patches(&paths[1], &frames[1])?;
    let undone: [f64; 2] = serde_json::from_value(launch1["patches"]["undone"].clone())?;
    untouched(
        "above the gradient after the reopen",
        reopened_patches[0],
        undone[0],
    )?;
    untouched(
        "below the gradient after the reopen",
        reopened_patches[1],
        undone[1],
    )?;

    // Frame 2: the entry the gradient was committed in, selected from the reopened history. The
    // mask exists there, no layer is bound to it yet, and the photograph is the unedited one.
    let historical = &frames[2];
    ensure(
        historical["state"]["selection"] != Value::Null,
        format!(
            "Launch 2 did not select a history entry: {}",
            historical["state"]["selection"]
        ),
    )?;
    let displayed = &historical["state"]["stack"]["displayed"]["layers"];
    ensure(
        displayed
            .as_array()
            .is_some_and(|layers| layers.iter().all(|layer| layer["mask"] == Value::Null)),
        format!("The mask-create entry already renders a masked layer: {displayed}"),
    )?;
    let historical_patches = patches(&paths[2], historical)?;
    let opened: [f64; 2] = serde_json::from_value(launch1["patches"]["opened"].clone())?;
    untouched(
        "above the gradient at the mask-create entry",
        historical_patches[0],
        opened[0],
    )?;
    untouched(
        "below the gradient at the mask-create entry",
        historical_patches[1],
        opened[1],
    )?;

    // Frame 3: back to the current state, which is the reopened one again.
    let current = &frames[3];
    ensure(
        json!(revision(current)?) == launch1["revision"]
            && json!(exposure(current)) == launch1["exposure"],
        format!(
            "Returning to current left revision {} exposure {:?}",
            revision(current)?,
            exposure(current)
        ),
    )?;
    let current_patches = patches(&paths[3], current)?;
    untouched(
        "above the gradient back at current",
        current_patches[0],
        undone[0],
    )?;
    untouched(
        "below the gradient back at current",
        current_patches[1],
        undone[1],
    )?;

    Ok(json!({
        "mask": launch1["mask"],
        "component": launch1["component"],
        "layer": launch1["layer"],
        "revision": revision(reopened)?,
        "patches": {
            "reopened": reopened_patches,
            "mask_create_entry": historical_patches,
            "back_at_current": current_patches,
        },
    }))
}
