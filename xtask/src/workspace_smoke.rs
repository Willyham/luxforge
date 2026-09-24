//! The `workspace` and `unavailable` smoke scenarios.
//!
//! `workspace` drives the panels, canvas mode, thirds overlay, historical preview, a live conflict
//! and the command palette through one evidence script, exactly as `crop` and `crop-draft` drive
//! the crop workflow. `unavailable` needs two launches sharing one catalog, the second with the crop
//! module disabled, because a module can only be disabled at startup.
use crate::{
    scenario::{Checked, Fixture, Frame, Plan, Run, Step, plan::only},
    *,
};
use lightwell_core::CROP_EFFECT;

/// The window the design's layout constants are written against.
pub const WINDOW: [&str; 2] = ["1440", "900"];
/// `edit.transform rotate-right` on an orientation-1 fixture reorders its quadrants exactly as
/// EXIF orientation 6 does (a 90 degree clockwise turn: new top-left is old bottom-left, and so
/// on), so every rotated frame reuses the ordinary fixture check at that orientation, with width
/// and height swapped.
const ROTATED: u8 = 6;
const CROP_MODULE: &str = "lightwell.crop";
const BASIC_MODULE: &str = "lightwell.basic";
const TRANSFORM_MODULE: &str = "lightwell.transform";
const POINTER_MODE: &str = "pointer";

/// Every frame of `workspace`, in order: the open, then one per step. Comments in the acceptance
/// criteria name what each step proves; the plan says what it commits, and `verify` below checks
/// the workspace, the status and the photograph each step produces.
pub fn plan(_: &[PathBuf]) -> Plan {
    let workspace =
        |name: &str, request: Value| Step::new(name, json!({ "workspace": request })).commits(0);
    Plan::new(vec![
        Step::opened("opened"),
        // `edit.transform rotate-right` commits one entry; the panels are untouched.
        Step::new(
            "rotated",
            json!({"api":{"method":"edit.transform","params":{"transform":"rotate-right"}}}),
        )
        .commits(1)
        .label("Rotate right"),
        // Each workspace step, its own columns, the photograph still centred in them.
        workspace("state-hidden", json!({"state_panel":false})),
        workspace(
            "tools-hidden",
            json!({"state_panel":true,"tools_panel":false}),
        ),
        workspace("thirds", json!({"tools_panel":true,"thirds":true})),
        // A historical preview of entry 0, the Original, then back to current.
        Step::new("preview", json!({"preview":{"sequence":0}})).commits(0),
        Step::new("current", json!({"preview":"current"})).commits(0),
        // Transforms, collapsed by its own default, expanded under a collapsed Basic: its four
        // exact operations as one row of icon buttons, both view state alone.
        Step::new(
            "basic-collapsed",
            json!({"section":{"module":BASIC_MODULE,"expanded":false}}),
        )
        .commits(0)
        .collapsed(BASIC_MODULE),
        Step::new(
            "transform-expanded",
            json!({"section":{"module":TRANSFORM_MODULE,"expanded":true}}),
        )
        .commits(0)
        .expanded(TRANSFORM_MODULE)
        .collapsed(BASIC_MODULE),
        // Starting a crop draft, by the `draft.start` route, enters the crop mode.
        Step::new("draft", json!({"draft":{"start":true}})).commits(0),
        // A commit while the draft is open is the conflict, whoever made it.
        Step::new(
            "conflict",
            json!({"api":{"method":"edit.transform","params":{"transform":"rotate-left"}}}),
        )
        .commits(1)
        .label("Rotate left"),
        // The palette, opened and queried by the script.
        Step::new("palette", json!({"palette":{"query":"rotate"}})).commits(0),
        // Cancelling the draft returns the session to pointer.
        Step::new("cancelled", json!({"draft":{"cancel":true}})).commits(0),
    ])
}

fn workspace_state(frame: &Value) -> &Value {
    &frame["state"]["workspace"]
}

fn expect_workspace(
    frame: &Value,
    state_panel: bool,
    tools_panel: bool,
    mode: &str,
    thirds: bool,
) -> Result {
    let workspace = workspace_state(frame);
    // The four fields this scenario drives, each read by name, plus the two clipping overlays it
    // never touches: the session's workspace also holds the per-client fields other features add,
    // and a scenario that does not touch them has nothing to say about them.
    ensure(
        workspace["state_panel"] == json!(state_panel)
            && workspace["tools_panel"] == json!(tools_panel)
            && workspace["mode"] == json!(mode)
            && workspace["thirds"] == json!(thirds)
            && workspace["clip_shadows"] == json!(false)
            && workspace["clip_highlights"] == json!(false),
        format!(
            "Workspace state is {workspace}, expected state_panel {state_panel}, tools_panel {tools_panel}, mode {mode}, thirds {thirds}, both clipping overlays off"
        ),
    )
}

/// The fitted photograph's own bounding box, then a horizontal brightness scan at its one-third
/// column against its neighbours: the thirds overlay is a 30%-white guide line, which raises
/// whatever it is drawn over, so a real line reads brighter than the plain photo beside it.
fn thirds_overlay_present(frame: &Frame) -> Result<Value> {
    let measured = frame.fixture(Fixture::fit(ROTATED))?;
    let bounds: [u32; 4] = serde_json::from_value(measured["image_bounds"].clone())?;
    let [left, top, right, bottom] = bounds;
    ensure(
        right > left + 30 && bottom > top + 30,
        "Image too small to sample thirds",
    )?;
    let image = frame.image()?;
    let third_x = left + (right - left) / 3;
    let (y0, y1) = (top + (bottom - top) / 4, top + 3 * (bottom - top) / 4);
    let brightness = |x: u32| -> f64 {
        let mut total = 0.0;
        let mut count = 0u32;
        for y in y0..y1 {
            let p = image.get_pixel(x, y).0;
            total += f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]);
            count += 1;
        }
        if count == 0 {
            0.0
        } else {
            total / f64::from(count)
        }
    };
    // The maximum over a small window around the computed column, against the mean of two
    // windows safely clear of it: tolerant of the line landing a pixel either way after rounding.
    let on = (third_x.saturating_sub(2)..=(third_x + 2).min(right - 1))
        .map(brightness)
        .fold(0.0, f64::max);
    let far = 14u32;
    let neighbour = |offset: i64| -> f64 {
        let x = (i64::from(third_x) + offset).clamp(i64::from(left), i64::from(right) - 1) as u32;
        brightness(x)
    };
    let neighbours = (neighbour(-i64::from(far)) + neighbour(i64::from(far))) / 2.0;
    let tolerance = 6.0;
    ensure(
        on > neighbours + tolerance,
        format!(
            "No lighter thirds guide at the one-third column: on={on:.1}, neighbours={neighbours:.1}, tolerance={tolerance}"
        ),
    )?;
    Ok(json!({
        "third_column_x": third_x,
        "on_line_brightness": on,
        "neighbour_brightness": neighbours,
        "tolerance": tolerance,
    }))
}

pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Vec::new();
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };

    // The fixture opens with both panels shown, pointer mode, no thirds.
    let opened = launch.at("opened")?;
    expect_workspace(opened, true, true, POINTER_MODE, false)?;
    record(
        opened,
        "the fixture at Fit, both panels open",
        opened.fixture(Fixture::fit(1))?,
    );

    // `edit.transform rotate-right` committed revision 1; the panels are untouched.
    let rotated = launch.at("rotated")?;
    expect_workspace(rotated, true, true, POINTER_MODE, false)?;
    ensure(
        rotated["state"]["stack"]["revision"] == json!(1),
        "rotate-right did not commit revision 1",
    )?;
    record(
        rotated,
        "rotated right, committed as revision 1",
        rotated.fixture(Fixture::fit(ROTATED))?,
    );

    // Each workspace step, its own columns, the photograph still centred in them.
    let hidden = launch.at("state-hidden")?;
    expect_workspace(hidden, false, true, POINTER_MODE, false)?;
    record(
        hidden,
        "the state panel collapsed",
        hidden.fixture(Fixture::fit(ROTATED))?,
    );
    let tools = launch.at("tools-hidden")?;
    expect_workspace(tools, true, false, POINTER_MODE, false)?;
    record(
        tools,
        "the state panel back, the tools panel collapsed",
        tools.fixture(Fixture::fit(ROTATED))?,
    );
    let thirds_frame = launch.at("thirds")?;
    expect_workspace(thirds_frame, true, true, POINTER_MODE, true)?;
    let thirds = thirds_overlay_present(thirds_frame)?;
    record(
        thirds_frame,
        "both panels open again, thirds overlay on",
        json!({"fit":thirds_frame.fixture(Fixture::fit(ROTATED))?, "thirds":thirds}),
    );

    // Previewing entry 0, the Original, unrotated at 480x320.
    let preview_frame = launch.at("preview")?;
    ensure(
        preview_frame["state"]["status"]
            .as_str()
            .is_some_and(|status| status.starts_with("Previewing entry 0")),
        format!(
            "The preview's status does not name the previewed entry: {}",
            preview_frame["state"]["status"]
        ),
    )?;
    expect_workspace(preview_frame, true, true, POINTER_MODE, true)?;
    let preview = preview_frame.fixture(Fixture::fit(1))?;
    record(
        preview_frame,
        "a historical preview of entry 0, the Original",
        json!({
            "pixels": preview,
            // The tools panel disables editing during a historical preview; the correlated state
            // carries no direct flag for it, so this is read off the status text it produces.
            "tools_disabled_inferred_from_status": true,
        }),
    );

    // Back to current, rotated again.
    let current = launch.at("current")?;
    ensure(
        current["state"]["status"]
            .as_str()
            .is_some_and(|status| status.starts_with("Current")),
        "Return to current did not restore the current marker",
    )?;
    record(
        current,
        "returned to the current, rotated state",
        current.fixture(Fixture::fit(ROTATED))?,
    );

    let expanded = launch.at("transform-expanded")?;
    record(
        expanded,
        "Transforms expanded under a collapsed Basic: its four actions as one icon-button row",
        json!({"expanded": expanded["state"]["expanded"]}),
    );

    // Starting a crop draft, by the `draft.start` route, enters the crop mode.
    let draft = launch.at("draft")?;
    ensure(
        draft["state"]["crop"]["drafting"] == json!(true),
        "draft.start did not open a draft",
    )?;
    expect_workspace(draft, true, true, CROP_MODULE, true)?;
    record(
        draft,
        "a crop draft open; the mode strip shows Crop",
        json!({"workspace": workspace_state(draft)}),
    );

    // A commit while the draft is open is the conflict, whoever made it.
    let conflict = launch.at("conflict")?;
    let notices = conflict.notices();
    ensure(
        notices.iter().any(|n| n == "Changed elsewhere"),
        format!("The conflict's notices do not include it: {notices:?}"),
    )?;
    ensure(
        conflict["state"]["crop"]["conflicted"] == json!(true),
        "The draft was not marked conflicted",
    )?;
    ensure(
        conflict["state"]["stack"]["revision"] == json!(2),
        "rotate-left during the draft did not commit revision 2",
    )?;
    record(
        conflict,
        "rotate-left during the draft: Changed elsewhere",
        json!({"notices": notices, "crop": conflict["state"]["crop"]}),
    );

    // The palette, opened and queried by the script.
    let palette = launch.at("palette")?;
    ensure(
        palette["state"]["palette"] == json!({"open":true,"query":"rotate"}),
        format!(
            "The palette state was not recorded as open with its query: {}",
            palette["state"]["palette"]
        ),
    )?;
    record(
        palette,
        "the command palette open, queried for \"rotate\"",
        palette["state"]["palette"].clone(),
    );

    // Cancelling the draft returns the session to pointer.
    let cancelled = launch.at("cancelled")?;
    ensure(
        cancelled["state"]["crop"]["drafting"] == json!(false),
        "draft.cancel did not end the draft",
    )?;
    expect_workspace(cancelled, true, true, POINTER_MODE, true)?;
    record(
        cancelled,
        "the draft cancelled; the mode strip returns to Pointer",
        json!({"workspace": workspace_state(cancelled)}),
    );

    write_json(
        &launch.evidence.join("workspace-checks.json"),
        &json!(checks),
    )?;
    Ok(())
}

pub const UNAVAILABLE_NOTE: &str = "Two launches: the first commits a crop layer with every built-in module registered; the second reuses its catalog with `--disable-module lightwell.crop` and reopens the same fixture, which the catalog dedupes to the same asset, so the stack's crop layer is reported unavailable instead of silently rendered without it.";

/// The first launch of `unavailable`: a 16:9 crop committed with every module registered.
pub fn unavailable_first(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_layer(CROP_EFFECT),
        Step::new(
            "cropped",
            json!({"api":{"method":"edit.crop-fit","params":{"aspect":"16:9"}}}),
        )
        .commits(1)
        .label("Crop 16:9"),
    ])
}

/// The second: the same catalog with the crop module disabled and the same fixture reopened, which
/// the catalog dedupes to the same asset. Rendering its stack reports the unavailable effect rather
/// than silently omitting it, so the open ends in the `incompatible` error, and that refusal is the
/// run's one input error.
pub fn unavailable_second(_: &[PathBuf]) -> Plan {
    Plan::new(vec![Step::opened("reopened").refused("incompatible")])
}

/// The two launches together: the crop committed in the first is reported unavailable in the
/// second, and no photograph is drawn without it.
pub fn verify_unavailable(run: &mut Run, launches: &[Checked]) -> Result {
    let [first, second] = launches else {
        return Err(format!("Expected two launches, found {}", launches.len()).into());
    };
    let committed = &first.at("cropped")?["state"]["stack"];
    ensure(
        committed["layers"]
            .as_array()
            .is_some_and(|layers| layers.iter().any(|l| l["effect"] == CROP_EFFECT)),
        "Launch 1 did not commit a crop layer",
    )?;
    let frame2 = second.at("reopened")?;
    ensure(
        frame2["state"]["render_error"]["code"] == json!("incompatible"),
        format!(
            "Launch 2's render error is {}, expected incompatible",
            frame2["state"]["render_error"]
        ),
    )?;
    let notices = frame2.notices();
    ensure(
        notices.iter().any(|n| n == "Preview is stale"),
        format!("Launch 2's notices do not name the stale preview: {notices:?}"),
    )?;
    // The histogram has nothing to plot, and says why inside the plot's own area rather than in a
    // row under it that would move the tools panel.
    let histogram = &frame2["state"]["histogram"];
    ensure(
        histogram["status"] == json!("unavailable")
            && histogram["notice"]
                .as_str()
                .is_some_and(|notice| notice.starts_with("Unavailable")),
        format!(
            "Launch 2's histogram is {} with the notice {}",
            histogram["status"], histogram["notice"]
        ),
    )?;
    let crop_module = frame2["state"]["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|m| m["id"] == json!(CROP_MODULE))
        .ok_or("The crop module is not listed at all")?;
    ensure(
        crop_module["available"] == json!(false),
        "The crop module is not reported unavailable",
    )?;
    // No photo drawn: the canvas region carries none of the fixture's own colours.
    let image = frame2.image()?;
    let [left, right] = frame2.columns()?.unwrap_or([0, image.width()]);
    let has_fixture_colour = (0..image.height()).step_by(4).any(|y| {
        (left..right).step_by(4).any(|x| {
            let p = image.get_pixel(x, y).0;
            fixtures::COLORS
                .iter()
                .any(|c| p.iter().zip(c).all(|(a, b)| a.abs_diff(*b) <= 8))
        })
    });
    ensure(
        !has_fixture_colour,
        "Launch 2 drew the photo despite the unavailable provider",
    )?;
    write_json(
        &run.out().join("unavailable-checks.json"),
        &json!({
            "launch1_committed_crop_layer": true,
            "launch2_render_error": frame2["state"]["render_error"],
            "launch2_notices": notices,
            "launch2_histogram_notice": frame2["state"]["histogram"]["notice"],
            "launch2_crop_module": crop_module,
            "launch2_photo_drawn": has_fixture_colour,
        }),
    )?;
    Ok(())
}
