//! The `workspace` and `unavailable` smoke scenarios.
//!
//! `workspace` drives the panels, canvas mode, thirds overlay, historical preview, a live conflict
//! and the command palette through one evidence script, exactly as `crop` and `crop-draft` drive
//! the crop workflow. `unavailable` needs two launches sharing one catalog, the second with the crop
//! module disabled, because a module can only be disabled at startup.
use crate::{
    scenario::{Checked, Checks, Fixture, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_core::CROP_EFFECT;
use luxforge_evidence::{self as script, DraftStep, PaletteStep, PreviewStep, WorkspaceStep};

/// `edit.transform rotate-right` on an orientation-1 fixture reorders its quadrants exactly as
/// EXIF orientation 6 does (a 90 degree clockwise turn: new top-left is old bottom-left, and so
/// on), so every rotated frame reuses the ordinary fixture check at that orientation, with width
/// and height swapped.
const ROTATED: u8 = 6;
const CROP_MODULE: &str = "luxforge.crop";
const BASIC_MODULE: &str = "luxforge.basic";
const TRANSFORM_MODULE: &str = "luxforge.transform";
const POINTER_MODE: &str = "pointer";

/// Every frame of `workspace`, in order: the open, then one per step. Comments in the acceptance
/// criteria name what each step proves; the plan says what it commits and what the workspace
/// holds — the four fields this scenario drives, and both clipping overlays off — and `verify`
/// below checks the status and the photograph each step produces.
pub fn plan(_: &[PathBuf]) -> Plan {
    let workspace = |name: &str, request: WorkspaceStep| Step::new(name, request).commits(0);
    Plan::new(vec![
        panels(Step::opened("opened"), true, true, POINTER_MODE, false),
        // `edit.transform rotate-right` commits one entry; the panels are untouched.
        panels(
            Step::new(
                "rotated",
                script::Step::call("edit.transform", json!({"transform":"rotate-right"})),
            )
            .commits(1)
            .label("Rotate right"),
            true,
            true,
            POINTER_MODE,
            false,
        ),
        // Each workspace step, its own columns, the photograph still centred in them.
        panels(
            workspace("state-hidden", WorkspaceStep::default().state_panel(false)),
            false,
            true,
            POINTER_MODE,
            false,
        ),
        panels(
            workspace(
                "tools-hidden",
                WorkspaceStep::default()
                    .state_panel(true)
                    .tools_panel(false),
            ),
            true,
            false,
            POINTER_MODE,
            false,
        ),
        panels(
            workspace(
                "thirds",
                WorkspaceStep::default().tools_panel(true).thirds(true),
            ),
            true,
            true,
            POINTER_MODE,
            true,
        ),
        // A historical preview of entry 0, the Original, named in the status bar, then back to
        // current.
        panels(
            Step::new("preview", PreviewStep::Sequence(0))
                .commits(0)
                .status_starts("Previewing entry 0"),
            true,
            true,
            POINTER_MODE,
            true,
        ),
        Step::new("current", PreviewStep::Current).commits(0),
        // Transforms, collapsed by its own default, expanded under a collapsed Basic: its four
        // exact operations as one row of icon buttons, both view state alone.
        Step::new(
            "basic-collapsed",
            script::Step::section(BASIC_MODULE, false),
        )
        .commits(0)
        .collapsed(BASIC_MODULE),
        Step::new(
            "transform-expanded",
            script::Step::section(TRANSFORM_MODULE, true),
        )
        .commits(0)
        .expanded(TRANSFORM_MODULE)
        .collapsed(BASIC_MODULE),
        // Starting a crop draft, by the `draft.start` route, enters the crop mode.
        panels(
            Step::new("draft", DraftStep::Start).commits(0),
            true,
            true,
            CROP_MODULE,
            true,
        ),
        // A commit while the draft is open is the conflict, whoever made it.
        Step::new(
            "conflict",
            script::Step::call("edit.transform", json!({"transform":"rotate-left"})),
        )
        .commits(1)
        .label("Rotate left")
        .notice("Changed elsewhere"),
        // The palette, opened and queried by the script.
        Step::new("palette", PaletteStep::Query("rotate".into())).commits(0),
        // Cancelling the draft returns the session to pointer.
        panels(
            Step::new("cancelled", DraftStep::Cancel).commits(0),
            true,
            true,
            POINTER_MODE,
            true,
        ),
    ])
}

/// The workspace fields this scenario drives, each read by name, plus the two clipping overlays it
/// never touches: the session's workspace also holds the per-client fields other features add, and
/// a scenario that does not touch them has nothing to say about them.
fn panels(step: Step, state_panel: bool, tools_panel: bool, mode: &str, thirds: bool) -> Step {
    step.workspace("state_panel", json!(state_panel))
        .workspace("tools_panel", json!(tools_panel))
        .mode(mode)
        .workspace("thirds", json!(thirds))
        .workspace("clip_shadows", json!(false))
        .workspace("clip_highlights", json!(false))
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
    let mut checks = Checks::new();
    // The fixture at its orientation and size in each frame that shows it at Fit: the open and
    // the preview of the Original upright, every current frame after the rotate turned.
    for (step, orientation) in [
        ("opened", 1),
        ("rotated", ROTATED),
        ("state-hidden", ROTATED),
        ("tools-hidden", ROTATED),
        ("thirds", ROTATED),
        ("preview", 1),
        ("current", ROTATED),
    ] {
        let frame = launch.at(step)?;
        checks.note(
            frame,
            "the fixture at Fit, centred in the photo surface",
            frame.fixture(Fixture::fit(orientation))?,
        );
    }
    let thirds = launch.at("thirds")?;
    checks.note(
        thirds,
        "the thirds overlay on",
        thirds_overlay_present(thirds)?,
    );
    ensure(
        launch.at("rotated")?.revision()? == 1,
        "rotate-right did not commit revision 1",
    )?;

    // Back to current, rotated again, and saying so.
    let preview = launch.at("preview")?;
    let current = launch.at("current")?;
    let label = current.label()?;
    ensure(
        current.status()?.starts_with("Returned to entry ")
            && current.status()?.ends_with(&format!(" \u{b7} {label}")),
        format!(
            "Return to current did not say it returned to the current entry: {}",
            current.status()?
        ),
    )?;
    // The status bar says what happened in words: no frame's status names the entry, snapshot or
    // source identity the correlated state carries.
    for frame in [preview, current] {
        let status = frame.status()?;
        let displayed = &frame.state()["stack"]["displayed"];
        for identity in [
            &frame.state()["stack"]["entry"],
            &displayed["entry"],
            &displayed["snapshot"],
        ] {
            let identity = identity.as_str().unwrap_or("");
            ensure(
                identity.is_empty() || !status.contains(&identity[..identity.len().min(12)]),
                format!("The status {status:?} names the identity {identity}"),
            )?;
        }
        ensure(
            !status.contains("snapshot") && !status.contains("source"),
            format!("The status {status:?} names a snapshot or a source"),
        )?;
    }

    // Starting a crop draft, by the `draft.start` route, opens one; a commit while it is open
    // marks it conflicted, and the palette and the cancel follow.
    ensure(
        launch.at("draft")?.state()["crop"]["drafting"] == json!(true),
        "draft.start did not open a draft",
    )?;
    let conflict = launch.at("conflict")?;
    ensure(
        conflict.state()["crop"]["conflicted"] == json!(true) && conflict.revision()? == 2,
        "rotate-left during the draft did not commit revision 2 and mark the draft conflicted",
    )?;
    let palette = launch.at("palette")?;
    ensure(
        palette.state()["palette"] == json!({"open":true,"query":"rotate"}),
        format!(
            "The palette state was not recorded as open with its query: {}",
            palette.state()["palette"]
        ),
    )?;
    ensure(
        launch.at("cancelled")?.state()["crop"]["drafting"] == json!(false),
        "draft.cancel did not end the draft",
    )?;

    checks.write(&launch.evidence, "workspace", json!({}))
}

pub const UNAVAILABLE_NOTE: &str = "Two launches: the first commits a crop layer with every built-in module registered; the second reuses its catalog with `--disable-module luxforge.crop` and reopens the same fixture, which the catalog dedupes to the same asset, so the stack's crop layer is reported unavailable instead of silently rendered without it.";

/// The first launch of `unavailable`: a 16:9 crop committed with every module registered.
pub fn unavailable_first(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_layer(CROP_EFFECT),
        Step::new(
            "cropped",
            script::Step::call("edit.crop-fit", json!({"aspect":"16:9"})),
        )
        .commits(1)
        .label("Crop 16:9"),
    ])
}

/// The second: the same catalog with the crop module disabled and the same fixture reopened, which
/// the catalog dedupes to the same asset. Rendering its stack reports the unavailable effect rather
/// than silently omitting it, so the open ends in the `incompatible` error, and that refusal is the
/// run's one input error; the canvas says the preview is stale.
pub fn unavailable_second(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("reopened")
            .refused("incompatible")
            .notice("Preview is stale"),
    ])
}

/// The two launches together: the crop committed in the first is reported unavailable in the
/// second, and no photograph is drawn without it.
pub fn verify_unavailable(run: &mut Run, launches: &[Checked]) -> Result {
    let [first, second] = launches else {
        return Err(format!("Expected two launches, found {}", launches.len()).into());
    };
    ensure(
        first.at("cropped")?.layer(CROP_EFFECT).is_some(),
        "Launch 1 did not commit a crop layer",
    )?;
    let frame2 = second.at("reopened")?;
    ensure(
        frame2.state()["render_error"]["code"] == json!("incompatible")
            && frame2.state()["render_error"]["data"]["effect_id"] == json!(CROP_EFFECT),
        format!(
            "Launch 2's render error is {}, expected incompatible naming {CROP_EFFECT} in its data",
            frame2.state()["render_error"]
        ),
    )?;
    // The histogram has nothing to plot, and says why inside the plot's own area rather than in a
    // row under it that would move the tools panel.
    let histogram = &frame2.state()["histogram"];
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
    let crop_module = frame2.module(CROP_MODULE)?;
    ensure(
        crop_module["available"] == json!(false),
        "The crop module is not reported unavailable",
    )?;
    // No photo drawn: the editor records no photograph rectangle, and the canvas region carries
    // none of the fixture's own colours.
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
        frame2.photo_rect().is_err() && !has_fixture_colour,
        "Launch 2 drew the photo despite the unavailable provider",
    )?;
    let mut checks = Checks::new();
    checks.note(
        frame2,
        "the crop reported unavailable: a render error naming it, and no photograph drawn",
        json!({
            "render_error": frame2.state()["render_error"],
            "histogram_notice": histogram["notice"],
            "crop_module": crop_module,
        }),
    );
    checks.write(run.out(), "unavailable", json!({}))
}
