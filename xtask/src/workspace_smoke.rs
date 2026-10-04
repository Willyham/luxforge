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
use luxforge_evidence::{
    self as script, CompareStep, DraftStep, PaletteStep, PreviewStep, WorkspaceStep,
};

/// `edit.transform rotate-right` on an orientation-1 fixture reorders its quadrants exactly as
/// EXIF orientation 6 does (a 90 degree clockwise turn: new top-left is old bottom-left, and so
/// on), so every rotated frame reuses the ordinary fixture check at that orientation, with width
/// and height swapped.
const ROTATED: u8 = 6;
const CROP_MODULE: &str = "luxforge.crop";
const BASIC_MODULE: &str = "luxforge.basic";
const POINTER_MODE: &str = "pointer";

/// Every frame of `workspace`, in order: the open, then one per step. Comments in the acceptance
/// criteria name what each step proves; the plan says what it commits and what the workspace
/// holds — the four fields this scenario drives, and both clipping overlays off — and `verify`
/// below checks the status and the photograph each step produces.
pub fn plan(_: &[PathBuf]) -> Plan {
    let workspace = |name: &str, request: WorkspaceStep| Step::new(name, request).commits(0);
    Plan::new(vec![
        panels(Step::opened("opened"), true, true, POINTER_MODE, false),
        // Run the combined module's generated control through the palette: one entry,
        // exactly the existing `edit.transform rotate-right` request.
        panels(
            Step::new(
                "rotated",
                PaletteStep::Run("Crop, transform, straighten · Rotate 90° right".into()),
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
        // The combined crop section, expanded under a collapsed Basic: its four exact
        // transforms lead Ratio and Angle, both view state alone.
        Step::new(
            "basic-collapsed",
            script::Step::section(BASIC_MODULE, false),
        )
        .commits(0)
        .collapsed(BASIC_MODULE),
        Step::new("crop-expanded", script::Step::section(CROP_MODULE, true))
            .commits(0)
            .expanded(CROP_MODULE)
            .collapsed(BASIC_MODULE),
        // Starting a crop draft, by the `draft.start` route, enters the crop mode.
        panels(
            Step::new("draft", DraftStep::Start).commits(0),
            true,
            true,
            CROP_MODULE,
            true,
        ),
        // An agent's commit while the draft is open, through a client of its own and read back by
        // the event sync, is the conflict.
        Step::new(
            "conflict",
            script::Step::agent("edit.transform", json!({"transform":"rotate-left"})),
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
        Step::new("compare-ready", PaletteStep::Run("Fit".into())).commits(0),
        Step::new("compare-guides-off", WorkspaceStep::default().thirds(false)).commits(0),
        Step::new(
            "compare-adjusted",
            script::Step::call("edit.set-basic", json!({"exposure":-1.0})),
        )
        .commits(1),
        Step::new(
            "compare-cropped",
            script::Step::call(
                "edit.crop",
                json!({"x":0.1,"y":0.1,"width":0.8,"height":0.8}),
            ),
        )
        .commits(1),
        Step::new(
            "compare-after",
            script::Step::call("edit.transform", json!({"transform":"rotate-right"})),
        )
        .commits(1),
        Step::new("compare-slider", CompareStep::Tap).commits(0),
        Step::new("compare-quarter", CompareStep::Position(0.25)).commits(0),
        Step::new("compare-before", CompareStep::Position(1.0)).commits(0),
        Step::new("compare-after-only", CompareStep::Position(0.0)).commits(0),
        Step::new("compare-middle", CompareStep::Position(0.5)).commits(0),
        Step::new("compare-press", script::Step::Key { key: "\\".into() }).commits(0),
        Step::new("compare-held", script::Step::Wait { ms: 250 }).commits(0),
        Step::new("compare-released", CompareStep::Release).commits(0),
        Step::new("compare-zoomed", script::ViewStep::Percent(200.0)).commits(0),
        Step::new("compare-zoom-before", CompareStep::Position(1.0)).commits(0),
        Step::new("compare-zoom-after", CompareStep::Position(0.0)).commits(0),
        Step::new("compare-fit", script::ViewStep::Fit).commits(0),
        Step::new("compare-fit-middle", CompareStep::Position(0.5)).commits(0),
        Step::new(
            "compare-exited",
            script::Step::Key {
                key: script::KEY_ESCAPE.into(),
            },
        )
        .commits(0),
        Step::new("compare-toggled", CompareStep::Tap).commits(0),
        Step::new("compare-toggled-off", CompareStep::Tap).commits(0),
        Step::new("compare-full-press", script::Step::Key { key: "\\".into() }).commits(0),
        Step::new("compare-full-held", script::Step::Wait { ms: 250 }).commits(0),
        Step::new("compare-full-released", CompareStep::Release).commits(0),
        Step::new(
            "compare-cancel-press",
            script::Step::Key { key: "\\".into() },
        )
        .commits(0),
        Step::new("compare-cancelled", CompareStep::FocusLoss).commits(0),
        Step::new("compare-cancel-late", script::Step::Wait { ms: 300 }).commits(0),
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

    // Starting a crop draft, by the `draft.start` route, opens one; an agent's commit while it is
    // open marks it conflicted, and the palette and the cancel follow.
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

    let before = launch.at("compare-before")?;
    let after = launch.at("compare-after")?;
    for (name, position) in [
        ("compare-slider", 0.5),
        ("compare-quarter", 0.25),
        ("compare-before", 1.0),
        ("compare-after-only", 0.0),
        ("compare-middle", 0.5),
        ("compare-released", 0.5),
    ] {
        let frame = launch.at(name)?;
        ensure(
            frame.state()["comparison"]["position"] == json!(position),
            format!("{name}: divider position"),
        )?;
        ensure(
            frame.state()["comparison"]["after_entry"]
                == after.state()["stack"]["displayed"]["entry"],
            format!("{name}: the After entry changed"),
        )?;
        ensure(
            frame.photo_rect()? == after.photo_rect()?,
            format!("{name}: comparison lost the crop/orientation framing"),
        )?;
        for x in [0.15, 0.85] {
            let reference = if x < position { before } else { after };
            for (channel, (a, b)) in frame
                .rgb_at([x, 0.35], 2)?
                .into_iter()
                .zip(reference.rgb_at([x, 0.35], 2)?)
                .enumerate()
            {
                checks.compare(
                    frame,
                    &format!("{name} side at {x} channel {channel}"),
                    a,
                    b,
                    crate::scenario::Tolerance::Within(1.0),
                )?;
            }
        }
    }
    checks.compare(
        before,
        "Before visibly differs from adjusted After",
        before.luminance_at([0.15, 0.35], 2)?,
        after.luminance_at([0.15, 0.35], 2)?,
        crate::scenario::Tolerance::Apart(5.0),
    )?;
    for (name, position) in [
        ("compare-zoomed", 0.5),
        ("compare-zoom-before", 1.0),
        ("compare-zoom-after", 0.0),
    ] {
        let frame = launch.at(name)?;
        for x in [0.15, 0.85] {
            let reference = if x < position { before } else { after };
            for (channel, (a, b)) in frame
                .rgb_at([x, 0.35], 2)?
                .into_iter()
                .zip(reference.rgb_at([x, 0.35], 2)?)
                .enumerate()
            {
                checks.compare(
                    frame,
                    &format!("{name} preserves zoom alignment at {x} channel {channel}"),
                    a,
                    b,
                    crate::scenario::Tolerance::Within(1.0),
                )?;
            }
        }
    }
    let held = launch.at("compare-held")?;
    ensure(
        held.state()["compare_hold"] == true,
        "backslash did not replace the slider",
    )?;
    ensure(
        launch.at("compare-press")?.state()["compare_key_pending"] == true,
        "backslash press did not arm its hold deadline",
    )?;
    ensure(
        held.state()["compare_key_pending"] == false,
        "hold deadline remained active after recognition",
    )?;
    checks.compare(
        held,
        "Held Before fills the After side",
        held.luminance_at([0.85, 0.35], 2)?,
        before.luminance_at([0.85, 0.35], 2)?,
        crate::scenario::Tolerance::Within(1.0),
    )?;
    let middle = launch.at("compare-slider")?;
    let quarter = launch.at("compare-quarter")?;
    ensure(
        middle.state()["surface"]["texture_writes"] == quarter.state()["surface"]["texture_writes"],
        "divider motion uploaded an unchanged photograph",
    )?;
    let full_held = launch.at("compare-full-held")?;
    ensure(
        full_held.state()["compare"] == true && full_held.state()["comparison"].is_null(),
        "a long backslash press outside the slider did not hold Before",
    )?;
    checks.compare(
        full_held,
        "Held Before outside the slider",
        full_held.luminance_at([0.85, 0.35], 2)?,
        before.luminance_at([0.85, 0.35], 2)?,
        crate::scenario::Tolerance::Within(1.0),
    )?;
    for name in [
        "compare-exited",
        "compare-toggled-off",
        "compare-full-released",
        "compare-cancelled",
        "compare-cancel-late",
    ] {
        let frame = launch.at(name)?;
        ensure(
            frame.state()["comparison"].is_null() && frame.state()["compare"] == false,
            format!("{name}: compare did not exit"),
        )?;
        ensure(
            frame.state()["compare_key_pending"] == false,
            format!("{name}: hold deadline remained active"),
        )?;
        checks.compare(
            frame,
            "Exit restores adjusted After",
            frame.luminance_at([0.85, 0.35], 2)?,
            after.luminance_at([0.85, 0.35], 2)?,
            crate::scenario::Tolerance::Within(1.0),
        )?;
    }
    checks.write(&launch.evidence, "workspace", json!({"comparison":"aligned crop/orientation, divider endpoints, backslash tap/hold, Escape/focus loss and unchanged uploads"}))
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
