//! The `workspace` and `unavailable` smoke scenarios.
//!
//! `workspace` drives the panels, canvas mode, thirds overlay, historical preview, a live conflict
//! and the command palette through one evidence script, exactly as `crop` and `crop-draft` drive
//! the crop workflow. `unavailable` is not scriptable at all: it needs two separate launches
//! sharing one catalog, the second with the crop module disabled, so it is orchestrated directly
//! rather than through `smoke::run`.
use crate::{
    smoke::{Expect, columns, frame_identity, pixels, spawn_editor, wait},
    *,
};
use std::time::Duration;

/// The fixture both scenarios open: the same landscape, orientation-1 pattern the crop scenarios
/// use, at 480x320.
const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
/// The window the design's layout constants are written against.
pub const WINDOW: [&str; 2] = ["1440", "900"];
/// `edit.transform rotate-right` on an orientation-1 fixture reorders its quadrants exactly as
/// EXIF orientation 6 does (a 90 degree clockwise turn: new top-left is old bottom-left, and so
/// on), so every rotated frame reuses the ordinary fixture check at that orientation, with width
/// and height swapped.
const ROTATED: u8 = 6;
const CROP_MODULE: &str = "lightwell.crop";
const POINTER_MODE: &str = "pointer";

/// How many frames the `workspace` scenario captures: one open frame plus one per script step.
pub fn frames(scenario: &str) -> Option<usize> {
    match scenario {
        "workspace" => Some(11),
        _ => None,
    }
}

/// The evidence script for `workspace`. Comments in the acceptance criteria name what each step
/// proves; `verify` below checks exactly those things against the frame each step produces.
pub fn script(scenario: &str) -> Option<Value> {
    match scenario {
        "workspace" => Some(json!([
            {"api":{"method":"edit.transform","params":{"transform":"rotate-right"}}},
            {"workspace":{"state_panel":false}},
            {"workspace":{"state_panel":true,"tools_panel":false}},
            {"workspace":{"tools_panel":true,"thirds":true}},
            {"preview":{"sequence":0}},
            {"preview":"current"},
            {"draft":{"start":true}},
            {"api":{"method":"edit.transform","params":{"transform":"rotate-left"}}},
            {"palette":{"query":"rotate"}},
            {"draft":{"cancel":true}}
        ])),
        _ => None,
    }
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
fn thirds_overlay_present(path: &Path, frame: &Value) -> Result<Value> {
    let measured = pixels(
        path,
        &Expect {
            columns: columns(frame)?,
            ..Expect::fit(ROTATED)
        },
    )?;
    let bounds: [u32; 4] = serde_json::from_value(measured["image_bounds"].clone())?;
    let [left, top, right, bottom] = bounds;
    ensure(
        right > left + 30 && bottom > top + 30,
        "Image too small to sample thirds",
    )?;
    let image = image::open(path)?.to_rgb8();
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

pub fn verify(evidence: &Path, app: &Value, _events: &[Value]) -> Result {
    let frames = app["frames"].as_array().ok_or("Missing frames")?.clone();
    ensure(
        app["had_input_errors"] == json!(false),
        "The run recorded an input error",
    )?;
    let paths: Vec<PathBuf> = frames
        .iter()
        .map(|frame| frame_identity(evidence, app, frame))
        .collect::<Result<Vec<_>>>()?;
    let mut checks = Vec::new();
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };

    // Frame 0: the fixture opens with both panels shown, pointer mode, no thirds.
    expect_workspace(&frames[0], true, true, POINTER_MODE, false)?;
    let opened = pixels(
        &paths[0],
        &Expect {
            columns: columns(&frames[0])?,
            ..Expect::fit(1)
        },
    )?;
    record(&frames[0], "the fixture at Fit, both panels open", opened);

    // Frame 1: `edit.transform rotate-right` committed revision 1; the panels are untouched.
    expect_workspace(&frames[1], true, true, POINTER_MODE, false)?;
    ensure(
        frames[1]["state"]["stack"]["revision"] == json!(1),
        "rotate-right did not commit revision 1",
    )?;
    let rotated = pixels(
        &paths[1],
        &Expect {
            columns: columns(&frames[1])?,
            ..Expect::fit(ROTATED)
        },
    )?;
    record(
        &frames[1],
        "rotated right, committed as revision 1",
        rotated,
    );

    // Frames 2-4: each workspace step, its own columns, the photograph still centred in them.
    expect_workspace(&frames[2], false, true, POINTER_MODE, false)?;
    record(
        &frames[2],
        "the state panel collapsed",
        pixels(
            &paths[2],
            &Expect {
                columns: columns(&frames[2])?,
                ..Expect::fit(ROTATED)
            },
        )?,
    );
    expect_workspace(&frames[3], true, false, POINTER_MODE, false)?;
    record(
        &frames[3],
        "the state panel back, the tools panel collapsed",
        pixels(
            &paths[3],
            &Expect {
                columns: columns(&frames[3])?,
                ..Expect::fit(ROTATED)
            },
        )?,
    );
    expect_workspace(&frames[4], true, true, POINTER_MODE, true)?;
    let thirds = thirds_overlay_present(&paths[4], &frames[4])?;
    record(
        &frames[4],
        "both panels open again, thirds overlay on",
        json!({"fit":pixels(&paths[4], &Expect { columns: columns(&frames[4])?, ..Expect::fit(ROTATED) })?, "thirds":thirds}),
    );

    // Frame 5: previewing entry 0, the Original, unrotated at 480x320.
    ensure(
        frames[5]["state"]["status"]
            .as_str()
            .is_some_and(|status| status.starts_with("Previewing entry 0")),
        format!(
            "Frame 5's status does not name the previewed entry: {}",
            frames[5]["state"]["status"]
        ),
    )?;
    expect_workspace(&frames[5], true, true, POINTER_MODE, true)?;
    let preview = pixels(
        &paths[5],
        &Expect {
            columns: columns(&frames[5])?,
            ..Expect::fit(1)
        },
    )?;
    record(
        &frames[5],
        "a historical preview of entry 0, the Original",
        json!({
            "pixels": preview,
            // The tools panel disables editing during a historical preview; the correlated state
            // carries no direct flag for it, so this is read off the status text it produces.
            "tools_disabled_inferred_from_status": true,
        }),
    );

    // Frame 6: back to current, rotated again.
    ensure(
        frames[6]["state"]["status"]
            .as_str()
            .is_some_and(|status| status.starts_with("Current")),
        "Return to current did not restore the current marker",
    )?;
    record(
        &frames[6],
        "returned to the current, rotated state",
        pixels(
            &paths[6],
            &Expect {
                columns: columns(&frames[6])?,
                ..Expect::fit(ROTATED)
            },
        )?,
    );

    // Frame 7: starting a crop draft, by the `draft.start` route, enters the crop mode.
    ensure(
        frames[7]["state"]["crop"]["drafting"] == json!(true),
        "draft.start did not open a draft",
    )?;
    expect_workspace(&frames[7], true, true, CROP_MODULE, true)?;
    record(
        &frames[7],
        "a crop draft open; the mode strip shows Crop",
        json!({"workspace": workspace_state(&frames[7])}),
    );

    // Frame 8: a commit while the draft is open is the conflict, whoever made it.
    let notices: Vec<String> = frames[8]["state"]["notices"]
        .as_array()
        .ok_or("Missing notices")?
        .iter()
        .filter_map(|n| n.as_str().map(str::to_owned))
        .collect();
    ensure(
        notices.iter().any(|n| n == "Changed elsewhere"),
        format!("Frame 8's notices do not include the conflict: {notices:?}"),
    )?;
    ensure(
        frames[8]["state"]["crop"]["conflicted"] == json!(true),
        "The draft was not marked conflicted",
    )?;
    ensure(
        frames[8]["state"]["stack"]["revision"] == json!(2),
        "rotate-left during the draft did not commit revision 2",
    )?;
    record(
        &frames[8],
        "rotate-left during the draft: Changed elsewhere",
        json!({"notices": notices, "crop": frames[8]["state"]["crop"]}),
    );

    // Frame 9: the palette, opened and queried by the script.
    ensure(
        frames[9]["state"]["palette"] == json!({"open":true,"query":"rotate"}),
        format!(
            "The palette state was not recorded as open with its query: {}",
            frames[9]["state"]["palette"]
        ),
    )?;
    record(
        &frames[9],
        "the command palette open, queried for \"rotate\"",
        frames[9]["state"]["palette"].clone(),
    );

    // Frame 10: cancelling the draft returns the session to pointer.
    ensure(
        frames[10]["state"]["crop"]["drafting"] == json!(false),
        "draft.cancel did not end the draft",
    )?;
    expect_workspace(&frames[10], true, true, POINTER_MODE, true)?;
    record(
        &frames[10],
        "the draft cancelled; the mode strip returns to Pointer",
        json!({"workspace": workspace_state(&frames[10])}),
    );

    write_json(&evidence.join("workspace-checks.json"), &json!(checks))?;
    Ok(())
}

/// The `unavailable` scenario: two launches sharing one catalog. The first commits a crop layer
/// with the crop module registered; the second reopens the same fixture (the catalog dedupes by
/// file identity, so this is the same asset with the same committed stack) with the crop module
/// disabled, so rendering it reports the unavailable effect instead of silently omitting it.
pub fn run_unavailable(root: &Path, out: &Path, bin: &Path, timeout: Duration) -> Result {
    ensure(!out.exists(), "Smoke output must be new")?;
    fs::create_dir_all(out)?;
    let fixture = root.join(FIXTURE);
    let launch1 = out.join("launch1");
    let launch2 = out.join("launch2");

    let mut result = json!({"scenario":"unavailable","status":"failed","launch_mode":launch::MODE,"platform":format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH)});
    let check = (|| -> Result {
        result["fixture_hash"] = json!(hash(&fixture)?);
        result["binary_sha256"] = json!(hash(bin)?);
        result["lockfile_sha256"] = json!(hash(&root.join("Cargo.lock"))?);

        // Launch 1: crop enabled, commit a 16:9 fit.
        let script1 = out.join("script1.json");
        write_json(
            &script1,
            &json!([{"api":{"method":"edit.crop-fit","params":{"aspect":"16:9"}}}]),
        )?;
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
        let mut child1 = spawn_editor(root, bin, &args1, &out.join("launch1.log"))?;
        let status1 = wait(&mut child1, timeout)?;
        result["launch1_exit_code"] = json!(status1.code());
        ensure(status1.success(), format!("Launch 1 exit {status1}"))?;
        let app1 = read_json(&launch1.join("result.json"))?;
        ensure(
            app1["status"] == "captured",
            "Launch 1 did not finish captured",
        )?;
        let committed = &app1["frames"]
            .as_array()
            .ok_or("Launch 1 wrote no frames")?[1]["state"]["stack"];
        ensure(
            committed["layers"].as_array().is_some_and(|layers| {
                layers
                    .iter()
                    .any(|l| l["effect"] == lightwell_core::CROP_EFFECT)
            }),
            "Launch 1 did not commit a crop layer",
        )?;

        // Launch 2: the same catalog, the crop module disabled, the same fixture reopened.
        let catalog = launch1.join("catalog.sqlite");
        ensure(catalog.is_file(), "Launch 1 wrote no catalog")?;
        let args2: Vec<OsString> = vec![
            "--catalog".into(),
            catalog.clone().into_os_string(),
            "--disable-module".into(),
            CROP_MODULE.into(),
            "--evidence-dir".into(),
            launch2.clone().into_os_string(),
            "--open".into(),
            fixture.clone().into_os_string(),
            "--window-size".into(),
            WINDOW[0].into(),
            WINDOW[1].into(),
        ];
        let mut child2 = spawn_editor(root, bin, &args2, &out.join("launch2.log"))?;
        let status2 = wait(&mut child2, timeout)?;
        result["launch2_exit_code"] = json!(status2.code());
        ensure(status2.success(), format!("Launch 2 exit {status2}"))?;
        let app2 = read_json(&launch2.join("result.json"))?;
        ensure(
            app2["status"] == "captured",
            "Launch 2 did not finish captured",
        )?;
        let frame2 = app2["frames"]
            .as_array()
            .and_then(|f| f.last())
            .ok_or("Launch 2 wrote no frames")?
            .clone();
        let path2 = frame_identity(&launch2, &app2, &frame2)?;

        ensure(
            frame2["state"]["render_error"]["code"] == json!("incompatible"),
            format!(
                "Launch 2's render error is {}, expected incompatible",
                frame2["state"]["render_error"]
            ),
        )?;
        let notices: Vec<String> = frame2["state"]["notices"]
            .as_array()
            .ok_or("Missing notices")?
            .iter()
            .filter_map(|n| n.as_str().map(str::to_owned))
            .collect();
        ensure(
            notices.iter().any(|n| n == "Preview is stale"),
            format!("Launch 2's notices do not name the stale preview: {notices:?}"),
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
        let image = image::open(&path2)?.to_rgb8();
        let [left, right] = columns(&frame2)?.unwrap_or([0, image.width()]);
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
        // The source is read-only throughout: its hash is unchanged from before either launch.
        ensure(
            json!(hash(&fixture)?) == result["fixture_hash"],
            "Source changed",
        )?;
        write_json(
            &out.join("unavailable-checks.json"),
            &json!({
                "launch1_committed_crop_layer": true,
                "launch2_render_error": frame2["state"]["render_error"],
                "launch2_notices": notices,
                "launch2_crop_module": crop_module,
                "launch2_photo_drawn": has_fixture_colour,
            }),
        )?;
        Ok(())
    })();
    match &check {
        Ok(()) => result["status"] = json!("passed"),
        Err(e) => result["error"] = json!(e.to_string()),
    };
    write_json(&out.join("result.json"), &result)?;
    fs::write(
        out.join("reproduce.md"),
        format!(
            "# Smoke run\n\nScenario: unavailable. Status: {}.\n\nTwo launches: the first commits a crop layer with every built-in module registered; the second reuses its catalog with `--disable-module lightwell.crop` and reopens the same fixture, which the catalog dedupes to the same asset, so the stack's crop layer is reported unavailable instead of silently rendered without it.\n\nReproduce with `cargo xtask smoke --scenario unavailable --output NEW_DIR --binary PATH`.\n",
            result["status"],
        ),
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    check
}
