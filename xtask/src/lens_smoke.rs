//! Offline Lens/Perspective, global and masked Detail, and warped mask gestures through real messages.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, plan::only},
    *,
};
use luxforge_evidence::{
    self as script, ControlsStep, MaskStep, PaintStep, Reference, SliderStep, ViewStep,
    WorkspaceStep,
};

pub const FIXTURE: &str = "fixtures/geometry/z6-24-70-35mm-grid.jpg";
const LENS: &str = "luxforge.lens";
const PERSPECTIVE: &str = "luxforge.perspective";
const SELECT: &str = "select-lens-profile";
const LENS_EFFECT: &str = "luxforge.lens.distortion";
const PERSPECTIVE_EFFECT: &str = "luxforge.perspective";
const HELD_STROKE: [[f64; 2]; 2] = [[0.375, 0.375], [0.5, 0.375]];
const FRESH_STROKE: [[f64; 2]; 2] = [[0.5, 0.625], [0.625, 0.625]];
const CURVE_EFFECT: &str = luxforge_core::CURVE_EFFECT;
const DETAIL_EFFECT: &str = luxforge_core::DETAIL_EFFECT;
const GLOBAL_DETAIL: [f64; 3] = [60.0, 40.0, 40.0];
const QUERY_ERROR: &str = "validation: parameter text must be at most 64 characters";

fn quiet(name: &str, step: impl Into<script::Step>) -> Step {
    Step::new(name, step).commits(0)
}

pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened")
            .collapsed(LENS)
            .collapsed(PERSPECTIVE)
            .no_layer(LENS_EFFECT),
        Step::new("curve-global", crate::scenario::recipe::moderate_curve())
            .commits(1)
            .payload(CURVE_EFFECT, global_curve()),
        Step::new("detail-global", crate::scenario::recipe::moderate_detail())
            .commits(1)
            .payload(DETAIL_EFFECT, global_detail()),
        quiet(
            "detail-global-current",
            script::Step::Preview(script::PreviewStep::Current),
        )
        .fit()
        .same_layer(DETAIL_EFFECT, "detail-global"),
        quiet(
            "basic-collapsed",
            script::Step::section("luxforge.basic", false),
        ),
        quiet("lens-expanded", script::Step::section(LENS, true)).expanded(LENS),
        // The section's own answer for empty text: the detected lens as a card, and no list.
        quiet(
            "detected",
            ControlsStep::QueryChoiceSearch {
                action: SELECT.into(),
                text: String::new(),
            },
        )
        .no_layer(LENS_EFFECT),
        Step::new(
            "selected-fit",
            ControlsStep::QueryChoiceApply {
                action: SELECT.into(),
            },
        )
        .commits(1)
        .fit(),
        quiet("selected-100", ViewStep::Percent(100.0))
            .percent(100.0)
            .same_layer(LENS_EFFECT, "selected-fit"),
        quiet("fit", ViewStep::Fit).fit(),
        quiet(
            "change-open",
            ControlsStep::QueryChoiceChange {
                action: SELECT.into(),
                open: true,
            },
        ),
        quiet(
            "compatible",
            ControlsStep::QueryChoiceSearch {
                action: SELECT.into(),
                text: "NIKKOR Z 24-70".into(),
            },
        ),
        quiet(
            "no-compatible",
            ControlsStep::QueryChoiceSearch {
                action: SELECT.into(),
                text: "Summilux".into(),
            },
        ),
        quiet(
            "report-recorded",
            ControlsStep::QueryChoiceReport {
                action: SELECT.into(),
            },
        ),
        quiet(
            "change-closed",
            ControlsStep::QueryChoiceChange {
                action: SELECT.into(),
                open: false,
            },
        )
        .same_layer(LENS_EFFECT, "selected-fit"),
        quiet("lens-collapsed", script::Step::section(LENS, false)).collapsed(LENS),
        quiet(
            "perspective-expanded",
            script::Step::section(PERSPECTIVE, true),
        )
        .expanded(PERSPECTIVE),
        quiet(
            "horizontal-draft",
            SliderStep::new("set-perspective", "horizontal", [10.0, 25.0, 40.0]),
        )
        .draft("set-perspective", json!({"horizontal":40})),
        Step::new(
            "horizontal",
            SliderStep::new("set-perspective", "horizontal", [40.0]).release(),
        )
        .commits(1)
        .no_draft()
        .field("set-perspective", "horizontal", "40")
        .field("set-perspective", "vertical", "0"),
        Step::new(
            "vertical",
            SliderStep::new("set-perspective", "vertical", [-10.0, -25.0]).release(),
        )
        .commits(1)
        .no_draft()
        .field("set-perspective", "horizontal", "40")
        .field("set-perspective", "vertical", "-25")
        .same_layer(PERSPECTIVE_EFFECT, "horizontal"),
        quiet("mask-mode", WorkspaceStep::default().mode("mask")),
        quiet("radial-new", MaskStep::New("radial".into())),
        quiet(
            "radial-outline",
            MaskStep::Sweep {
                from: [0.5, 0.5],
                to: [0.7, 0.7],
            },
        ),
        Step::new("radial-committed", MaskStep::Apply)
            .commits(1)
            .masks(1),
        Step::new(
            "detail-masked",
            SliderStep::new("set-detail", "sharpening", [25.0]).release(),
        )
        .commits(1)
        .no_draft()
        .field("set-detail", "sharpening", "25")
        .same_layer(DETAIL_EFFECT, "detail-global"),
        quiet("pointer", WorkspaceStep::default().mode("pointer")),
        Step::new(
            "straightened",
            script::Step::call("edit.crop-fit", json!({"aspect":"original","angle":2.5})),
        )
        .commits(1)
        .same_layer(LENS_EFFECT, "selected-fit"),
        quiet("brush-mask-mode", WorkspaceStep::default().mode("mask")),
        quiet("brush-armed", MaskStep::Paint(PaintStep::NewBrush)),
        quiet("brush-held", MaskStep::stroke(HELD_STROKE, false)),
        Step::new(
            "brush-conflict",
            script::Step::agent(
                "edit.set-perspective",
                json!({"horizontal":-30,"vertical":15}),
            ),
        )
        .commits(1)
        .notice("Changed elsewhere")
        .field("set-perspective", "horizontal", "-30")
        .field("set-perspective", "vertical", "15")
        .same_layer(PERSPECTIVE_EFFECT, "vertical"),
        quiet("brush-apply-refused", MaskStep::Apply)
            .refused("Changed elsewhere: discard the mask gesture or reapply it")
            .notice("Changed elsewhere"),
        quiet("brush-reapplied", MaskStep::Reapply).no_notices(),
        Step::new("held-stroke-committed", MaskStep::Apply)
            .commits(1)
            .masks(1)
            .components(&["add radial", "add brush"]),
        // Re-arm through the panel's ordinary target message, so the current map has answered
        // before the next pointer begins another stroke.
        quiet(
            "brush-rearmed",
            MaskStep::Paint(PaintStep::Component(Reference::name("Brush 1"))),
        ),
        quiet("fresh-stroke-held", MaskStep::stroke(FRESH_STROKE, false)),
        Step::new("fresh-stroke-committed", MaskStep::Apply)
            .commits(1)
            .no_draft()
            .masks(1)
            .components(&["add radial", "add brush"])
            .same_layer(LENS_EFFECT, "selected-fit"),
        quiet("query-pointer", WorkspaceStep::default().mode("pointer"))
            .field("set-perspective", "horizontal", "-30")
            .field("set-perspective", "vertical", "15"),
        quiet("query-lens-expanded", script::Step::section(LENS, true)).expanded(LENS),
        quiet(
            "query-change-open",
            ControlsStep::QueryChoiceChange {
                action: SELECT.into(),
                open: true,
            },
        ),
        quiet(
            "query-error",
            ControlsStep::QueryChoiceSearch {
                action: SELECT.into(),
                text: "x".repeat(65),
            },
        )
        .refused(QUERY_ERROR),
        quiet(
            "query-retried",
            ControlsStep::QueryChoiceRetry {
                action: SELECT.into(),
            },
        )
        .refused(QUERY_ERROR),
        quiet(
            "query-recovered",
            ControlsStep::QueryChoiceSearch {
                action: SELECT.into(),
                text: String::new(),
            },
        )
        .same_layer(LENS_EFFECT, "selected-fit"),
        quiet(
            "combined-current",
            script::Step::Preview(script::PreviewStep::Current),
        )
        .fit()
        .payload(DETAIL_EFFECT, global_detail()),
        Step::new(
            "detail-neutral",
            script::Step::call(
                "edit.set-detail",
                json!({"sharpening":0.0,"luminance":0.0,"colour":0.0}),
            ),
        )
        .commits(1)
        .same_layer(DETAIL_EFFECT, "detail-global")
        .same_layer(LENS_EFFECT, "selected-fit")
        .same_layer(PERSPECTIVE_EFFECT, "horizontal"),
        quiet(
            "combined-neutral-current",
            script::Step::Preview(script::PreviewStep::Current),
        )
        .fit(),
        Step::new("detail-undo", script::Step::call("history.undo", json!({})))
            .commits(1)
            .payload(DETAIL_EFFECT, global_detail())
            .same_layer(DETAIL_EFFECT, "detail-global"),
        quiet(
            "combined-restored-current",
            script::Step::Preview(script::PreviewStep::Current),
        )
        .fit()
        .payload(DETAIL_EFFECT, global_detail())
        .same_layer(LENS_EFFECT, "selected-fit")
        .same_layer(PERSPECTIVE_EFFECT, "horizontal"),
    ])
}

fn global_curve() -> Value {
    json!({"luminance":crate::scenario::recipe::MODERATE_CURVE})
}

fn global_detail() -> Value {
    json!({"sharpening":GLOBAL_DETAIL[0],"luminance":GLOBAL_DETAIL[1],"colour":GLOBAL_DETAIL[2]})
}

/// Settlement is an identity claim, not merely the presence of an image file.
fn settled_current(frame: &Frame) -> Result {
    frame.visible_photo()?;
    let gpu = &frame.state()["surface"]["gpu"];
    ensure(
        gpu["drawn_photo_blank"] == false
            && gpu["drawn_stale_photo"] == false
            && gpu["blank_photo_draws"] == 0
            && gpu["stale_photo_draws"] == 0,
        "The combined current GPU capture is blank or stale",
    )?;
    let state = frame.state();
    let proxy = &state["proxy"];
    ensure(
        state["requested_generation"] == state["displayed_generation"]
            && state["stack"]["displayed"]["entry"] == state["stack"]["entry"]
            && state["surface"]["detail_updating"] == false
            && state["histogram"]["stale"] == false
            && state["approximate_white_balance"] == false
            && (proxy["presented"] == false
                || (proxy["presented"] == true
                    && proxy["settled_from_exact"] == true
                    && proxy["approximate"] == false
                    && proxy["approximate_reason"].is_null())),
        "The combined current capture has not settled exact pixels and analysis",
    )
}

/// Compare the complete drawn photo in two Fit captures; chrome outside it is irrelevant.
fn photo_difference(a: &Frame, b: &Frame) -> Result<f64> {
    let ar = a.photo()?;
    let br = b.photo()?;
    let size = |r: [u32; 4]| (r[2] - r[0], r[3] - r[1]);
    ensure(size(ar) == size(br), "The combined Fit photo sizes differ")?;
    let (width, height) = size(ar);
    let (ai, bi) = (a.image()?, b.image()?);
    let mut sum = 0u64;
    for y in 0..height {
        for x in 0..width {
            let ap = ai.get_pixel(ar[0] + x, ar[1] + y).0;
            let bp = bi.get_pixel(br[0] + x, br[1] + y).0;
            sum += ap
                .into_iter()
                .zip(bp)
                .map(|(a, b)| u64::from(a.abs_diff(b)))
                .sum::<u64>();
        }
    }
    Ok(sum as f64 / (f64::from(width) * f64::from(height) * 3.0))
}

/// The Lens section's states as captured: no list before a search, the detected lens offered as a
/// card whose Apply carries the JPEG acknowledgement, the applied card after it, a search that
/// lists only compatible profiles, and a report link recorded rather than opened.
fn lens_section_states(launch: &Checked) -> Result<Value> {
    let choice = |name: &str| -> Result<Value> {
        Ok(launch.at(name)?["state"]["control_ui"]["query_choices"][SELECT].clone())
    };
    let detected = choice("detected")?;
    let suggestion = &detected["suggestion"];
    ensure(
        detected["rows"].as_array().is_some_and(Vec::is_empty)
            && detected["current"].is_null()
            && detected["report"].is_null()
            && detected["changing"] == false
            && suggestion["title"] == "NIKKOR Z 24-70mm f/4 S"
            && suggestion["eligible"] == true
            && suggestion["parameters"] == json!({"assume-uncorrected": true})
            && suggestion["note"]
                .as_str()
                .is_some_and(|note| note.contains("assumes it did not")),
        format!("The Lens section did not offer its detected lens without a list: {detected}"),
    )?;
    let profile = launch
        .at("selected-fit")?
        .payload(LENS_EFFECT)
        .ok_or("Apply produced no lens layer")?;
    ensure(
        profile["profile"]["key"] == suggestion["key"]
            && profile["profile"]["optics"]["acknowledged"] == "assume-uncorrected",
        "Apply did not freeze the detected profile with its acknowledgement",
    )?;
    let opened = choice("change-open")?;
    ensure(
        opened["changing"] == true
            && opened["current"]["key"] == suggestion["key"]
            && opened["suggestion"].is_null()
            && opened["rows"].as_array().is_some_and(Vec::is_empty),
        format!("The applied card or Change did not show as answered: {opened}"),
    )?;
    let compatible = choice("compatible")?;
    let rows = compatible["rows"]
        .as_array()
        .ok_or("The compatible search has no rows")?;
    let lens_level = [
        "incompatible-mount",
        "calibration-sensor-smaller",
        "aspect-mismatch",
        "focal-out-of-range",
    ];
    ensure(
        rows.iter().any(|row| row["key"] == suggestion["key"])
            && rows.iter().all(|row| {
                row["eligible"] == true
                    && row["parameters"] == json!({"assume-uncorrected": true})
                    && row["reasons"].as_array().is_none_or(|reasons| {
                        reasons
                            .iter()
                            .all(|reason| !lens_level.iter().any(|level| reason == level))
                    })
            }),
        format!("The search listed an incompatible or unselectable profile: {compatible}"),
    )?;
    let empty = choice("no-compatible")?;
    let report = empty["report"]["url"]
        .as_str()
        .ok_or("A search without a compatible profile offered no report")?;
    ensure(
        empty["rows"].as_array().is_some_and(Vec::is_empty)
            && empty["notice"]["level"] == "warning"
            && empty["notice"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("Summilux"))
            && report.starts_with("https://github.com/Willyham/luxforge/issues/new?title="),
        format!("The empty search did not warn and offer the report: {empty}"),
    )?;
    let recorded = choice("report-recorded")?;
    ensure(
        recorded["opened"] == report,
        "The report link did not record the page it would open",
    )?;
    let closed = choice("change-closed")?;
    ensure(
        closed["changing"] == false
            && closed["text"] == ""
            && closed["rows"].as_array().is_some_and(Vec::is_empty)
            && closed["notice"].is_null()
            && closed["report"].is_null(),
        format!("Closing Change did not return to the applied card: {closed}"),
    )?;
    Ok(
        json!({"suggestion":suggestion,"compatible_rows":rows.len(),"report_url_length":report.len(),
        "frozen":profile["profile"]["key"]}),
    )
}

/// A retry is an explicit new request for the same failed input. Error and recovery frames must
/// stay on the same immutable entry, so neither list interaction can masquerade as an edit.
fn query_retry_states(states: [&Value; 4]) -> Result<Value> {
    let [before, failed, retried, recovered] = states;
    let choice = |state: &Value| state["control_ui"]["query_choices"][SELECT].clone();
    let before_ui = choice(before);
    let failed_ui = choice(failed);
    let retried_ui = choice(retried);
    let recovered_ui = choice(recovered);
    for state in [failed, retried, recovered] {
        ensure(
            state["stack"]["entry"] == before["stack"]["entry"]
                && state["stack"]["revision"] == before["stack"]["revision"],
            "Lens query Retry or recovery changed history",
        )?;
    }
    for ui in [&failed_ui, &retried_ui] {
        ensure(
            ui["text"] == "x".repeat(65)
                && ui["error"] == QUERY_ERROR
                && ui["loading"] == false
                && ui["rows"].as_array().is_some_and(Vec::is_empty)
                && ui["request"]["action"] == SELECT
                && ui["request"]["text"] == ui["text"]
                && ui["request"]["page"].is_u64()
                && ui["request"]["shared"].is_object()
                && ui["request"]["asset"].is_string()
                && ui["request"]["entry"] == before["stack"]["entry"],
            "Lens validation error was not captured with an available Retry button",
        )?;
    }
    let sequence = |ui: &Value| {
        ui["request"]["sequence"]
            .as_u64()
            .ok_or("Lens query has no request sequence")
    };
    ensure(
        sequence(&failed_ui)? > sequence(&before_ui)?
            && sequence(&retried_ui)? == sequence(&failed_ui)? + 1
            && sequence(&recovered_ui)? == sequence(&retried_ui)? + 1,
        "Retry or clearing the search did not create a fresh query request",
    )?;
    let without_sequence = |ui: &Value| {
        let mut identity = ui["request"].clone();
        if let Some(identity) = identity.as_object_mut() {
            identity.remove("sequence");
        }
        identity
    };
    ensure(
        without_sequence(&failed_ui) == without_sequence(&retried_ui),
        "Retry changed the failed query's input or asset/entry identity",
    )?;
    ensure(
        recovered_ui["text"] == ""
            && recovered_ui["request"]["text"] == ""
            && recovered_ui["request"]["asset"] == retried_ui["request"]["asset"]
            && recovered_ui["request"]["entry"] == retried_ui["request"]["entry"]
            && recovered_ui["error"].is_null()
            && recovered_ui["loading"] == false
            && recovered_ui["rows"].as_array().is_some_and(Vec::is_empty)
            && recovered_ui["current"]["key"].is_string(),
        "Clearing the Lens search did not recover the section's own answer",
    )?;
    Ok(
        json!({"failed_request":failed_ui["request"],"retry_request":retried_ui["request"],
        "recovered_request":recovered_ui["request"],"error":QUERY_ERROR,
        "recovered_current":recovered_ui["current"],"entry":before["stack"]["entry"]}),
    )
}

/// The mask gesture's pointer map changes with the externally committed geometry, while the
/// interrupted stroke remains in its original content coordinates. These are captured states,
/// independent of the rendering checks, so a stale map cannot pass merely by producing a PNG.
fn reapply_states(states: [&Value; 7]) -> Result<Value> {
    let [
        held,
        conflict,
        refused,
        rebased,
        published,
        fresh,
        committed,
    ] = states;
    let old_map = &held["mask_draft"]["mapping"];
    let current_hash = &conflict["geometry"]["mapping_sha256"];
    ensure(
        old_map["mapping_sha256"].is_string()
            && current_hash.is_string()
            && old_map["mapping_sha256"] != *current_hash,
        "The external Perspective commit did not replace the brush's geometry",
    )?;
    let fields = &held["draft"]["fields"];
    ensure(
        fields["points"].is_array() && held["mask_draft"]["stroke"]["painting"] == true,
        "The first stroke was not held open with captured content points",
    )?;
    for (name, state) in [
        ("conflicted", conflict),
        ("refused", refused),
        ("reapplied", rebased),
    ] {
        ensure(
            state["draft"]["fields"] == *fields,
            format!("The {name} stroke changed its captured content fields"),
        )?;
        ensure(
            state["draft"]["draft_id"] == held["draft"]["draft_id"],
            format!("The {name} stroke replaced its core draft"),
        )?;
    }
    for state in [conflict, refused] {
        // A brush's canvas bar offers Done and keeps that action enabled even while its
        // retained draft is conflicted. The plan checks the attempted Apply's actual refusal;
        // this state proof checks that the conflict interrupted painting and retained its map.
        ensure(
            state["mask_draft"]["conflicted"] == true
                && state["draft"]["conflicted"] == true
                && state["draft_bar"]["conflicted"] == true
                && state["mask_draft"]["stroke"]["painting"] == false
                && state["mask_draft"]["mapping"] == *old_map,
            "A conflict did not interrupt painting and preserve the original map",
        )?;
    }
    ensure(
        conflict["stack"]["revision"] == refused["stack"]["revision"]
            && conflict["stack"]["entry"] == refused["stack"]["entry"],
        "The refused Apply published an entry",
    )?;
    let new_map = &rebased["mask_draft"]["mapping"];
    let stamp = &new_map["identity"]["draft"];
    ensure(
        rebased["mask_draft"]["conflicted"] == false
            && rebased["draft"]["base_revision"] == rebased["stack"]["revision"]
            && rebased["mask_draft"]["stroke"]["painting"] == false
            && new_map["mapping_sha256"] == *current_hash
            && new_map["identity"]["entry_id"] == conflict["stack"]["entry"]
            && stamp["draft_id"] == rebased["draft"]["draft_id"]
            && stamp["draft_revision"].as_u64().is_some_and(|revision| {
                rebased["draft"]["draft_revision"]
                    .as_u64()
                    .is_some_and(|current| revision <= current)
            })
            && new_map["identity"]["source_fingerprint"].is_string()
            && new_map["identity"]["source_fingerprint"]
                == old_map["identity"]["source_fingerprint"],
        "Reapply did not interrupt the kept stroke and acquire the current entry's map",
    )?;
    let fresh_map = &fresh["mask_draft"]["mapping"];
    ensure(
        published["draft"].is_null()
            && fresh["mask_draft"]["stroke"]["painting"] == true
            && fresh_map["mapping_sha256"] == *current_hash
            && fresh_map["identity"]["entry_id"] == published["stack"]["entry"]
            && fresh_map["identity"]["source_fingerprint"]
                == new_map["identity"]["source_fingerprint"]
            && fresh["draft"]["fields"]["points"] != fields["points"],
        "The new stroke did not start under the refreshed committed geometry",
    )?;
    ensure(
        committed["draft"].is_null()
            && committed["mask_draft"].is_null()
            && committed["geometry"]["mapping_sha256"] == *current_hash,
        "The final stroke did not publish on the current geometry",
    )?;
    Ok(
        json!({"old_map":old_map,"reapplied_map":new_map,"fresh_stroke_map":fresh_map,
        "kept_fields":fields,"fresh_fields":fresh["draft"]["fields"],"final_entry":committed["stack"]["entry"]}),
    )
}

fn expected_stroke(fields: &Value, path: &[[f64; 2]]) -> Result<luxforge_core::mask::Stroke> {
    let number = |name: &str| {
        fields[name]
            .as_f64()
            .ok_or_else(|| format!("The stroke has no {name}"))
    };
    ensure(
        fields["limit_to_colour"] == false,
        "The regression stroke unexpectedly samples a colour limit",
    )?;
    let stroke = luxforge_core::mask::Stroke::capture(
        path,
        number("size")?,
        number("feather")?,
        number("flow")?,
        fields["erase"]
            .as_bool()
            .ok_or("The stroke has no erase setting")?,
    )?;
    ensure(
        fields["points"] == serde_json::to_value(stroke.points().collect::<Vec<_>>())?,
        "Captured stroke points differ from the scripted content coordinates",
    )?;
    Ok(stroke)
}

/// Resolve the catalog's actual content-addressed objects after the editor exits. UI rows expose
/// their references, but only the stored stroke proves which coordinates were durably retained.
fn stored_strokes(launch: &Checked, kept_fields: &Value, fresh_fields: &Value) -> Result<Value> {
    let final_frame = launch.at("combined-restored-current")?;
    let service = luxforge_core::EditorService::open(&launch.evidence.join("catalog.sqlite"))?;
    // Two identities say whether a second photograph exists.
    let assets = service.asset_ids(2)?;
    ensure(
        assets.len() == 1,
        "The regression catalog does not contain exactly one source",
    )?;
    let state = service.state(&assets[0])?;
    ensure(
        state.current_entry.id.as_str() == final_frame.entry()?
            && state.revision == final_frame.revision()?,
        "The reopened catalog does not match the final captured entry",
    )?;
    let recipe = &state.current_entry.snapshot.recipe;
    let curve: Vec<_> = recipe
        .layers
        .iter()
        .filter(|layer| layer.effect_id == CURVE_EFFECT)
        .collect();
    ensure(
        curve.len() == 1 && curve[0].mask.is_none() && curve[0].payload == global_curve(),
        "The durable combined recipe lost its real global Tone curve payload",
    )?;
    let detail: Vec<_> = recipe
        .layers
        .iter()
        .filter(|layer| layer.effect_id == DETAIL_EFFECT)
        .collect();
    ensure(
        detail.len() == 2
            && detail[0].mask.is_none()
            && detail[0].payload == global_detail()
            && detail[1].mask.is_some()
            && detail[1].payload == json!({"sharpening":25.0}),
        "The durable combined recipe lost its global or masked Detail payload",
    )?;
    let mask = recipe.masks.first().ok_or("The final recipe has no mask")?;
    let brush = mask
        .components
        .get(1)
        .ok_or("The final recipe has no appended brush")?;
    let references = luxforge_core::path::references(&brush.payload, &brush.name)?;
    ensure(
        references.len() == 2 && references[0] != references[1],
        "The final brush did not retain two distinct strokes",
    )?;
    let expected = [
        expected_stroke(kept_fields, &HELD_STROKE)?,
        expected_stroke(fresh_fields, &FRESH_STROKE)?,
    ];
    let mut stored = Vec::new();
    for (index, (id, expected)) in references.iter().zip(expected).enumerate() {
        let stroke = recipe
            .strokes
            .get::<luxforge_core::mask::Stroke>(id)
            .ok_or("A durable stroke reference is unresolved")?;
        ensure(
            *stroke == expected,
            format!(
                "Durable stroke {index} differs from the captured content coordinates or brush settings"
            ),
        )?;
        ensure(
            final_frame.component(1)?["strokes"][index]["stroke"] == id.as_str(),
            "The panel's stroke reference differs from the durable recipe",
        )?;
        stored.push(json!({"id":id,"points":stroke.points().collect::<Vec<_>>(),"size":stroke.size(),"feather":stroke.feather(),"flow":stroke.flow()}));
    }
    Ok(
        json!({"entry":state.current_entry.id,"mask":mask.id,"component":brush.id,"strokes":stored,"detail_layers":detail}),
    )
}

pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let lens = lens_section_states(launch)?;
    let mut checks = Checks::new();
    checks.note(
        launch.at("report-recorded")?,
        "Lens section states: detected card, applied card, compatible search, report",
        lens,
    );
    for name in [
        "selected-fit",
        "selected-100",
        "horizontal",
        "vertical",
        "radial-outline",
        "straightened",
        "brush-held",
        "brush-reapplied",
        "fresh-stroke-held",
        "fresh-stroke-committed",
        "query-error",
        "query-retried",
        "query-recovered",
    ] {
        let frame = launch.at(name)?;
        ensure(
            frame.payload(DETAIL_EFFECT) == Some(&global_detail())
                && frame.layer_id(DETAIL_EFFECT)
                    == launch.at("detail-global")?.layer_id(DETAIL_EFFECT),
            format!("{name} lost or replaced global Detail during combined geometry/mask editing"),
        )?;
        ensure(
            frame.payload(CURVE_EFFECT) == Some(&global_curve())
                && frame.layer_id(CURVE_EFFECT)
                    == launch.at("curve-global")?.layer_id(CURVE_EFFECT),
            format!("{name} lost or replaced the real Tone curve during combined editing"),
        )?;
        let surface = &frame["state"]["surface"];
        ensure(
            frame["state"]["requested_generation"] == frame["state"]["displayed_generation"],
            format!("{name} shows a stale preview generation"),
        )?;
        ensure(
            frame["state"]["geometry"]["mapping_sha256"]
                .as_str()
                .is_some(),
            format!("{name} has no mapping hash"),
        )?;
        ensure(
            frame.image()?.width() > 0,
            format!("{name} has no native renderer capture"),
        )?;
        checks.note(frame,name,json!({"entry":frame.entry()?,"surface":surface,"lens":frame.payload(LENS_EFFECT),"perspective":frame.payload(PERSPECTIVE_EFFECT),"detail":frame.payload(DETAIL_EFFECT),"curve":frame.payload(CURVE_EFFECT)}));
    }
    ensure(
        launch.at("horizontal")?.payload(PERSPECTIVE_EFFECT) == Some(&json!({"horizontal":40})),
        "Horizontal drag did not commit its final value",
    )?;
    ensure(
        launch.at("vertical")?.payload(PERSPECTIVE_EFFECT)
            == Some(&json!({"horizontal":40,"vertical":-25})),
        "Perspective axes did not merge",
    )?;
    ensure(
        launch.at("brush-conflict")?.payload(PERSPECTIVE_EFFECT)
            == Some(&json!({"horizontal":-30,"vertical":15})),
        "The Agent did not publish the replacement Perspective geometry",
    )?;
    let names = [
        "brush-held",
        "brush-conflict",
        "brush-apply-refused",
        "brush-reapplied",
        "held-stroke-committed",
        "fresh-stroke-held",
        "fresh-stroke-committed",
    ];
    let states = names.map(|name| launch.at(name).map(|frame| frame.state()));
    let [
        held,
        conflict,
        refused,
        rebased,
        published,
        fresh,
        committed,
    ] = states;
    let reapply = reapply_states([
        held?, conflict?, refused?, rebased?, published?, fresh?, committed?,
    ])?;
    let reapply_step = launch.index("brush-reapplied")?;
    ensure(
        launch.events.iter().any(|event| {
            event["event"] == "script_step_settled"
                && event["detail"]["step"] == reapply_step
                && event["detail"]["waited_for"] == "mask_map"
                && event["detail"]["by"] == "mask_map"
        }),
        "Reapply was captured before its refreshed pointer map answered",
    )?;
    let stored = stored_strokes(launch, &reapply["kept_fields"], &reapply["fresh_fields"])?;
    checks.note(
        launch.at("fresh-stroke-committed")?,
        "mask conflict and refreshed pointer map",
        json!({"captured":reapply,"durable":stored}),
    );
    let retry = query_retry_states([
        launch.at("query-lens-expanded")?.state(),
        launch.at("query-error")?.state(),
        launch.at("query-retried")?.state(),
        launch.at("query-recovered")?.state(),
    ])?;
    for name in ["query-error", "query-retried", "query-recovered"] {
        let step = launch.index(name)?;
        ensure(
            launch.events.iter().any(|event| {
                event["event"] == "script_step_settled"
                    && event["detail"]["step"] == step
                    && event["detail"]["waited_for"] == "query_choice"
                    && event["detail"]["by"] == "query_choice_answered"
            }),
            format!("{name} was captured before its real query answer"),
        )?;
    }
    checks.note(
        launch.at("query-retried")?,
        "Lens query error and Retry",
        retry,
    );
    for name in [
        "detail-global-current",
        "combined-current",
        "combined-neutral-current",
        "combined-restored-current",
    ] {
        let frame = launch.at(name)?;
        settled_current(frame)?;
        ensure(
            frame.payload(CURVE_EFFECT) == Some(&global_curve())
                && frame.layer_id(CURVE_EFFECT)
                    == launch.at("curve-global")?.layer_id(CURVE_EFFECT),
            format!("{name} lost or replaced the combined Tone curve"),
        )?;
    }
    let current = launch.at("combined-current")?;
    let neutral = launch.at("combined-neutral-current")?;
    let restored = launch.at("combined-restored-current")?;
    let retained = current["state"]["stack"]["layers"]
        .as_array()
        .ok_or("No combined layers")?;
    let masked = retained
        .iter()
        .filter(|layer| layer["effect"] == DETAIL_EFFECT && !layer["mask"].is_null())
        .collect::<Vec<_>>();
    ensure(
        masked.len() == 1
            && masked[0]["payload"] == json!({"sharpening":25.0})
            && masked[0]["mask"] == current.only_mask()?["id"],
        "Combined mask editing did not retain a Detail layer bound to the warped mask",
    )?;
    ensure(
        current["state"]["stack"]["layers"] == restored["state"]["stack"]["layers"],
        "Undo did not restore the complete combined stack",
    )?;
    let changed = photo_difference(current, neutral)?;
    ensure(
        changed > 0.0,
        "Neutralizing global Detail did not change combined geometry/mask pixels",
    )?;
    checks.note(neutral,"Detail changes the combined Tone curve/Lens/Perspective/mask/crop photo",json!({"mean_absolute_rgb_codes":changed,"curve":current.payload(CURVE_EFFECT),"global_detail":current.payload(DETAIL_EFFECT),"masked_detail":masked[0],"settled_exact":true}));
    checks.compare(
        restored,
        "Undo restores byte-identical combined native photo pixels",
        photo_difference(current, restored)?,
        0.0,
        Tolerance::Within(0.0),
    )?;
    checks.write(&launch.evidence,"lens-perspective",json!({"scope":"Generated grid and descriptor-backed desktop gestures with correlated native captures; a real global Tone curve and global and masked Detail survive Lens/Perspective, crop and mask editing, explicit current captures settle exact and Undo restores byte-identical native photo pixels (a repeat/undo proof, not an independent numerical oracle); mask Reapply preserves stored content coordinates and refreshes the pointer map after another client's Perspective commit; the Lens section answers with its detected lens as a card and no list, Apply freezes it with the JPEG acknowledgement, Change searches only compatible profiles and an empty search records its report link without opening a browser; explicit Lens query Retry issues a fresh request with the same rejected input and clearing the search recovers the applied card without changing history; photographic lens qualification is separate"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lens_perspective_script_uses_real_query_choice_messages_and_one_commit_per_release() {
        let plan = plan(&[]);
        plan.validate().unwrap();
        let script = script::parse(&plan.script().to_string()).unwrap();
        assert_eq!(script.len(), plan.len() - 1);
        assert_eq!(plan.len(), 48);
        assert_eq!(script[0], crate::scenario::recipe::moderate_curve());
        assert_eq!(script[1], crate::scenario::recipe::moderate_detail());
        assert!(script.iter().any(|step| matches!(
            step,
            script::Step::Controls(ControlsStep::QueryChoiceApply { .. })
        )));
        assert_eq!(
            script[plan.index("brush-mask-mode").unwrap() - 1],
            script::Step::Workspace(WorkspaceStep::default().mode("mask"))
        );
        assert!(
            matches!(&script[plan.index("brush-conflict").unwrap() - 1],script::Step::Agent {method,..} if method == "edit.set-perspective")
        );
        assert_eq!(
            script[plan.index("brush-reapplied").unwrap() - 1],
            script::Step::Mask(MaskStep::Reapply)
        );
        assert!(matches!(
            &script[plan.index("fresh-stroke-held").unwrap() - 1],
            script::Step::Mask(MaskStep::Stroke { release: false, .. })
        ));
        assert!(matches!(
            &script[plan.index("query-error").unwrap() - 1],
            script::Step::Controls(ControlsStep::QueryChoiceSearch { action, text })
                if action == SELECT && text.chars().count() == 65
        ));
        assert_eq!(
            script[plan.index("query-retried").unwrap() - 1],
            script::Step::Controls(ControlsStep::QueryChoiceRetry {
                action: SELECT.into()
            })
        );
        for (name, horizontal, vertical) in [
            ("horizontal", "40", "0"),
            ("vertical", "40", "-25"),
            ("brush-conflict", "-30", "15"),
            ("query-pointer", "-30", "15"),
        ] {
            assert_eq!(
                plan.steps()[plan.index(name).unwrap()].expect().fields,
                [
                    (
                        "set-perspective".into(),
                        "horizontal".into(),
                        horizontal.into()
                    ),
                    ("set-perspective".into(), "vertical".into(), vertical.into()),
                ],
                "{name} checks the fields shown beside the stored Perspective layer"
            );
        }
    }

    #[test]
    fn reapply_verifier_rejects_stale_maps_changed_content_and_old_entry_identity() {
        let fields = json!({"points":HELD_STROKE});
        let old = json!({"mapping_sha256":"old","identity":{"entry_id":"original","source_fingerprint":"source"}});
        let new = json!({"mapping_sha256":"new","identity":{"entry_id":"geometry","source_fingerprint":"source","draft":{"draft_id":"first","draft_revision":3}}});
        let held = json!({"mask_draft":{"mapping":old,"stroke":{"painting":true}},
            "draft":{"draft_id":"first","fields":fields},"geometry":{"mapping_sha256":"old"},
            "stack":{"entry":"original","revision":5}});
        let mut conflict = held.clone();
        conflict["stack"] = json!({"entry":"geometry","revision":6});
        conflict["geometry"]["mapping_sha256"] = json!("new");
        conflict["mask_draft"]["conflicted"] = json!(true);
        conflict["mask_draft"]["stroke"]["painting"] = json!(false);
        conflict["draft"]["conflicted"] = json!(true);
        conflict["draft_bar"] = json!({"can_apply":true,"done":true,"conflicted":true});
        let refused = conflict.clone();
        let mut rebased = conflict.clone();
        rebased["mask_draft"]["mapping"] = new.clone();
        rebased["mask_draft"]["conflicted"] = json!(false);
        rebased["mask_draft"]["stroke"]["painting"] = json!(false);
        rebased["draft"]["base_revision"] = json!(6);
        rebased["draft"]["draft_revision"] = json!(4);
        let mut published = rebased.clone();
        published["draft"] = Value::Null;
        published["mask_draft"] = Value::Null;
        published["stack"] = json!({"entry":"kept-stroke","revision":7});
        let mut fresh = rebased.clone();
        fresh["draft"]["fields"]["points"] = json!(FRESH_STROKE);
        fresh["draft"]["draft_id"] = json!("second");
        fresh["mask_draft"]["stroke"]["painting"] = json!(true);
        fresh["mask_draft"]["mapping"]["identity"]["entry_id"] = json!("kept-stroke");
        let mut committed = published.clone();
        committed["stack"] = json!({"entry":"fresh-stroke","revision":8});
        let mut states = [
            held, conflict, refused, rebased, published, fresh, committed,
        ];
        assert!(reapply_states(states.each_ref()).is_ok());
        states[1]["draft_bar"]["conflicted"] = json!(false);
        assert!(reapply_states(states.each_ref()).is_err());
        states[1]["draft_bar"]["conflicted"] = json!(true);
        states[2]["mask_draft"]["stroke"]["painting"] = json!(true);
        assert!(reapply_states(states.each_ref()).is_err());
        states[2]["mask_draft"]["stroke"]["painting"] = json!(false);
        states[3]["mask_draft"]["mapping"]["mapping_sha256"] = json!("old");
        assert!(reapply_states(states.each_ref()).is_err());
        states[3]["mask_draft"]["mapping"]["mapping_sha256"] = json!("new");
        states[3]["mask_draft"]["mapping"]["identity"]["draft"]["draft_id"] = json!("other");
        assert!(reapply_states(states.each_ref()).is_err());
        states[3]["mask_draft"]["mapping"]["identity"]["draft"]["draft_id"] = json!("first");
        states[2]["draft"]["fields"]["points"] = json!(FRESH_STROKE);
        assert!(reapply_states(states.each_ref()).is_err());
        states[2]["draft"]["fields"]["points"] = json!(HELD_STROKE);
        states[5]["mask_draft"]["mapping"]["identity"]["entry_id"] = json!("original");
        assert!(reapply_states(states.each_ref()).is_err());
    }

    #[test]
    fn retry_verifier_requires_fresh_same_input_requests_and_history_preserving_recovery() {
        let before = json!({"stack":{"entry":"same-entry","revision":8},
            "control_ui":{"query_choices":{SELECT:{"request":{"sequence":4}}}}});
        let identity = json!({"sequence":5,"action":SELECT,"text":"x".repeat(65),"page":0,
            "shared":{"assume-uncorrected":true},"asset":"same-source","entry":"same-entry"});
        let mut failed = before.clone();
        failed["control_ui"]["query_choices"][SELECT] = json!({"request":identity,
            "text":"x".repeat(65),"loading":false,"error":QUERY_ERROR,"rows":[]});
        let mut retried = failed.clone();
        retried["control_ui"]["query_choices"][SELECT]["request"]["sequence"] = json!(6);
        let mut recovered = retried.clone();
        let ui = &mut recovered["control_ui"]["query_choices"][SELECT];
        ui["text"] = json!("");
        ui["request"]["text"] = json!("");
        ui["request"]["sequence"] = json!(7);
        ui["error"] = Value::Null;
        ui["rows"] = json!([]);
        ui["current"] = json!({"key":"lf1-applied","title":"Applied"});
        let mut states = [before, failed, retried, recovered];
        assert!(query_retry_states(states.each_ref()).is_ok());
        states[2]["control_ui"]["query_choices"][SELECT]["request"]["sequence"] = json!(5);
        assert!(query_retry_states(states.each_ref()).is_err());
        states[2]["control_ui"]["query_choices"][SELECT]["request"]["sequence"] = json!(6);
        states[2]["control_ui"]["query_choices"][SELECT]["request"]["shared"] = json!({});
        assert!(query_retry_states(states.each_ref()).is_err());
        states[2]["control_ui"]["query_choices"][SELECT]["request"]["shared"] =
            json!({"assume-uncorrected":true});
        states[3]["stack"]["revision"] = json!(9);
        assert!(query_retry_states(states.each_ref()).is_err());
        states[3]["stack"]["revision"] = json!(8);
        // A recovered answer that lists rows for empty text, or lost its card, is refused.
        states[3]["control_ui"]["query_choices"][SELECT]["rows"] =
            json!([{"key":"listed","title":"Listed","eligible":true}]);
        assert!(query_retry_states(states.each_ref()).is_err());
        states[3]["control_ui"]["query_choices"][SELECT]["rows"] = json!([]);
        states[3]["control_ui"]["query_choices"][SELECT]["current"] = Value::Null;
        assert!(query_retry_states(states.each_ref()).is_err());
    }

    #[test]
    fn durable_stroke_expectation_keeps_content_coordinates_and_quantized_settings() {
        let stroke =
            luxforge_core::mask::Stroke::capture(&HELD_STROKE, 0.1, 50.0, 100.0, false).unwrap();
        let mut fields = json!({"points":stroke.points().collect::<Vec<_>>(),"size":0.1,
            "feather":50,"flow":100,"erase":false,"limit_to_colour":false});
        assert_eq!(expected_stroke(&fields, &HELD_STROKE).unwrap(), stroke);
        fields["points"] = json!(FRESH_STROKE);
        assert!(expected_stroke(&fields, &HELD_STROKE).is_err());
    }
}
