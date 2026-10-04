//! The `raw-panel` smoke scenario: the Basic section as the tools panel draws it for a real RAW
//! source, in place of a RAW section, which is never listed. Its White balance group is the RAW
//! development's on the global target: a Temperature drag in kelvin left open, whose drafted frame
//! approximates the white balance on the developed planes and is labelled so, then released,
//! which redevelops the mosaic and lands the exact frame, at Fit and again at 100%. At 100%, a
//! held-draft pause checks the exact visible-region refinement and captures full-detail
//! approximate pixels for white-balance accuracy against the release. Both drags keep the tint in
//! force (the first, from As shot, the camera's as-shot tint); and a double-click reset on
//! Temperature, Tint and Exposure after the committed jump the first press makes: Temperature and
//! Tint back to As shot, labelled `Reset White balance`, whose fields then show the camera's
//! as-shot equivalent, and Exposure back to 0 EV. Basic's band carries no edited dot on the
//! untouched photograph, one once a custom white balance is committed, and none again once the
//! resets leave As shot at 0 EV; `W` enters the RAW development's sensor pick, which Basic's
//! Neutral picker shows selected, and Escape leaves it; and a crop drafted on the RAW's whole
//! input stage, straightened by 7°, applied at Fit, read at 100% and replaced through the API's
//! `crop-fit`, each commit checked to be the picture on screen.
//!
//! No RAW photograph is checked in (see `fixtures/README.md`), so this scenario is outside the
//! rendered tier of [`crate::smoke::SCENARIOS`] and takes its source from `--source`: an owner or raw.pixls.us file
//! the editor supports. It proves what the panel shows, what its double-click does and that a RAW
//! crop is drawn and shown, not RAW decoding, which `raw-editor` covers.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_core::{CROP_EFFECT, POINTER_MODE};
use luxforge_evidence::{self as script, DoubleClickStep, DraftStep, SliderStep, ViewStep};

pub const SCENARIO: &str = "raw-panel";
/// The RAW development's module, whose section is never listed and whose sensor pick `W` enters,
/// and its effect, whose layer the checks read.
const RAW_MODULE: &str = "luxforge.raw";
const RAW_EFFECT: &str = "luxforge.raw";
const BASIC_MODULE: &str = "luxforge.basic";
/// The action Basic's Temperature and Tint send on a RAW photo's global target, and its fields.
const SET_RAW: &str = "set-raw";
const TEMPERATURE: &str = "temperature";
const TINT: &str = "tint";

/// The steps the checks read by name, apart from the drags, double-clicks and samples, whose
/// tables carry their own. The plan and the checks share each name, so a misspelt one does not
/// build: no RAW is checked in, so no recorded run would catch it.
mod names {
    pub const OPENED: &str = "opened";
    pub const SENSOR_PICK: &str = "sensor-pick";
    pub const PICK_LEFT: &str = "pick-left";
    pub const CROP_STARTED: &str = "crop-started";
    pub const CROP_STRAIGHTENED: &str = "crop-straightened";
    pub const CROP_APPLIED: &str = "crop-applied";
    pub const CROP_AT_100: &str = "crop-at-100";
    pub const CROP_FITTED: &str = "crop-fitted";
    pub const FITTED_SAMPLE: &str = "fitted-sample";
    pub const FITTED_AT_FIT: &str = "fitted-at-fit";
}

/// One scripted temperature drag and release at the same value. Fit uses a display proxy; at 100%
/// the moving frame is a half-detail region and `quiet` captures a full-detail approximate draft.
struct Drag {
    drag: &'static str,
    quiet: Option<&'static str>,
    release: &'static str,
    kelvin: f64,
    fit: bool,
}

/// At Fit, far enough from any camera's as-shot white balance to change the picture plainly; then
/// at 100%, far from that committed 3500 K. Both inside 2000..12000 K on the 10 K step.
const DRAGS: [Drag; 2] = [
    Drag {
        drag: "drag-at-fit",
        quiet: None,
        release: "release-at-fit",
        kelvin: 3500.0,
        fit: true,
    },
    Drag {
        drag: "drag-at-100",
        quiet: Some("quiet-at-100"),
        release: "release-at-100",
        kelvin: 2500.0,
        fit: false,
    },
];
/// Between a double-click's release and its second press: a person's ordinary double-click, well
/// inside the 300 ms iced gives the two presses.
const GAP_MS: u64 = 120;

/// One double-click the script makes: its step, the field, where its first press lands, the action
/// its reset runs, and what the field shows once that has run.
struct DoubleClick {
    step: &'static str,
    action: &'static str,
    parameter: &'static str,
    value: f64,
    /// The action the reset sends: the field's own, with its declared default, or the reset its
    /// control declares.
    reset: &'static str,
    /// The text the field shows after its reset: its declared default, or `None` for As shot,
    /// whose entry is labelled [`AS_SHOT_LABEL`] and whose temperature and tint are the camera's
    /// as-shot equivalent, computed from the frame's own RAW layer by the check.
    shows: Option<&'static str>,
}

/// As shot: the reset the RAW variants of Temperature and Tint declare, and its history label.
const AS_SHOT: &str = SET_RAW;
const AS_SHOT_LABEL: &str = "Reset White balance";

const DOUBLE_CLICKS: [DoubleClick; 3] = [
    // Temperature and Tint reset to the camera's own white balance, as Lightroom's Temp and Tint
    // do.
    DoubleClick {
        step: "temperature-reset",
        action: SET_RAW,
        parameter: TEMPERATURE,
        value: 5000.0,
        reset: AS_SHOT,
        shows: None,
    },
    DoubleClick {
        step: "tint-reset",
        action: SET_RAW,
        parameter: TINT,
        value: 12.0,
        reset: AS_SHOT,
        shows: None,
    },
    // Exposure is Basic's on every kind: its commit does not wait for a redevelopment.
    DoubleClick {
        step: "exposure-reset",
        action: "set-basic",
        parameter: "exposure",
        value: 0.4,
        reset: "set-basic",
        shows: Some("0.00"),
    },
];

/// The draft's straightening angle, and the angle the API's `crop-fit` then commits.
const CROP_ANGLE: f64 = 7.0;
const FIT_ANGLE: f64 = -12.0;
/// Where `render.sample` reads the committed crop at 100%, and the steps that ask: stage pixels
/// inside the corner of the crop the canvas shows at a zero pan, clear of the scroll bars and the
/// mode strip, for any supplied RAW (the smallest crop, the Z6's, is over 2000 px each way). The
/// API's crop is read again at the second.
const SAMPLES: [(&str, (u32, u32)); 2] = [
    ("crop-sample-near", (300, 200)),
    ("crop-sample-far", (1100, 700)),
];

/// The public point query of one stage pixel, whose answer the step records.
fn sample((x, y): (u32, u32)) -> script::Step {
    script::Step::call("render.sample", json!({"x":x,"y":y}))
}

/// The crop steps follow the double-clicks: a draft opened on the RAW's whole input stage, given
/// a 16:9 ratio and straightened, applied at Fit, inspected at 100% through two point samples,
/// replaced by a `crop-fit` through the API at 100% and read again, then Fit. Every frame of a
/// committed crop shows it with no notice over it.
fn crop_steps() -> Vec<Step> {
    let at_100 = |step: Step| step.percent(100.0).no_notices();
    vec![
        Step::new(names::CROP_STARTED, DraftStep::Start),
        Step::new("crop-ratio", DraftStep::Preset("16:9".into())),
        Step::new(names::CROP_STRAIGHTENED, DraftStep::Angle(CROP_ANGLE)),
        // Apply commits one entry.
        Step::new(names::CROP_APPLIED, DraftStep::Apply)
            .commits(1)
            .fit()
            .no_notices(),
        at_100(Step::new(names::CROP_AT_100, ViewStep::Percent(100.0))),
        at_100(Step::new(SAMPLES[0].0, sample(SAMPLES[0].1))),
        at_100(Step::new(SAMPLES[1].0, sample(SAMPLES[1].1))),
        // The API's `crop-fit` updates the applied crop's own layer in one entry.
        at_100(
            Step::new(
                names::CROP_FITTED,
                script::Step::call("edit.crop-fit", json!({"aspect":"3:2","angle":FIT_ANGLE})),
            )
            .commits(1)
            .same_layer(CROP_EFFECT, names::CROP_APPLIED),
        ),
        at_100(Step::new(names::FITTED_SAMPLE, sample(SAMPLES[1].1))),
        Step::new(names::FITTED_AT_FIT, ViewStep::Fit)
            .fit()
            .no_notices(),
    ]
}

/// Every frame, in order: the open, then one per step. The expectations here are what each step
/// commits, what its fields and label show and that the Basic section is on screen; `verify`
/// checks the rest.
pub fn plan(_: &[PathBuf]) -> Plan {
    let mut steps = vec![Step::opened(names::OPENED)];
    for drag in &DRAGS {
        if !drag.fit {
            // 100%, where a frame shows a stage pixel per display pixel and has no proxy.
            steps.push(Step::new("zoom-100", ViewStep::Percent(100.0)).percent(100.0));
        }
        // A Temperature drag left open. Its frame is the drafted value approximated on the planes
        // developed at the committed white balance.
        steps.push(Step::new(
            drag.drag,
            SliderStep::new(SET_RAW, TEMPERATURE, [drag.kelvin]),
        ));
        if let Some(quiet) = drag.quiet {
            // The shared 120 ms quiet policy must refine the held WB draft without a release.
            // One second accommodates the three photo-sized RAWs' exact whole-frame follow-up.
            steps.push(Step::new(quiet, script::Step::wait(1000)));
        }
        // Its release at the same value, which commits it and redevelops the mosaic.
        steps.push(
            Step::new(
                drag.release,
                SliderStep::new(SET_RAW, TEMPERATURE, [drag.kelvin]).release(),
            )
            .commits(1)
            .no_draft(),
        );
        if !drag.fit {
            // Back to Fit for the double-clicks.
            steps.push(Step::new("zoom-fit", ViewStep::Fit).fit());
        }
    }
    // A double-click on Temperature's, Tint's and Exposure's rails. The first press moves the
    // value, which commits on release; the second press resets the field: two entries.
    steps.extend(DOUBLE_CLICKS.iter().map(|click| {
        let step = Step::new(
            click.step,
            DoubleClickStep {
                action: click.action.into(),
                parameter: click.parameter.into(),
                value: click.value,
                gap_ms: GAP_MS,
            },
        )
        .commits(2);
        match click.shows {
            Some(default) => step.field(click.action, click.parameter, default),
            None => step.label(AS_SHOT_LABEL),
        }
    }));
    // Basic's letter enters the RAW development's sensor pick on this photograph's global
    // target; Escape leaves it for the pointer.
    steps.push(Step::new(names::SENSOR_PICK, script::Step::key("w")).mode(RAW_MODULE));
    steps.push(
        Step::new(names::PICK_LEFT, script::Step::key(script::KEY_ESCAPE)).mode(POINTER_MODE),
    );
    // A straightened crop drafted, applied and inspected; see `crop_steps`.
    steps.extend(crop_steps());
    // Basic's section, expanded in every frame; that its White balance fields are the RAW
    // development's is `verify`'s proof that the source opened as RAW.
    Plan::new(
        steps
            .into_iter()
            .map(|step| step.expanded(BASIC_MODULE))
            .collect(),
    )
}

fn raw_payload(frame: &Value) -> Result<&Value> {
    frame["state"]["stack"]["layers"]
        .as_array()
        .and_then(|layers| layers.iter().find(|layer| layer["effect"] == RAW_EFFECT))
        .map(|layer| &layer["payload"])
        .ok_or_else(|| "The stack has no RAW layer".into())
}

/// The sensor gains the frame's RAW layer develops at.
fn development_gains(frame: &Value) -> Result<[f32; 3]> {
    serde_json::from_value(raw_payload(frame)?["gains"].clone())
        .map_err(|error| format!("The RAW layer's gains are unreadable: {error}").into())
}

/// The most the released exact frame may differ from the tested draft view state, as a share of
/// the drag's own change from the frame before it (owner decision, 2026-09-27).
const MAX_WB_ACCURACY_SHARE: f64 = 0.1;

/// Fit tests the moving proxy. At 100%, the moving half-detail capture remains evidence of the
/// interaction, while white-balance accuracy tests the held full-detail capture before release.
/// The latter says nothing by itself about motion timing or visible softness.
fn check_white_balance_accuracy(
    fit: bool,
    moving_mean: f64,
    moving_ratio: f64,
    held: Option<(f64, f64)>,
    change_mean: f64,
) -> Result<&'static str> {
    if fit {
        ensure(
            moving_ratio <= MAX_WB_ACCURACY_SHARE,
            format!(
                "Fit moving white-balance accuracy failed: the released frame is {moving_mean:.3} codes from the moving draft on average, {:.2}% of the drag's own {change_mean:.3}; the limit is 10%",
                moving_ratio * 100.0,
            ),
        )?;
        ensure(
            moving_mean <= 1.0,
            format!(
                "Fit moving white-balance accuracy failed: the released frame is {moving_mean:.3} codes from the moving draft on average; the limit is 1 code"
            ),
        )?;
        Ok("moving")
    } else {
        let (held_mean, held_ratio) =
            held.ok_or("The 100% draft has no held full-detail capture")?;
        ensure(
            held_ratio <= MAX_WB_ACCURACY_SHARE,
            format!(
                "100% held full-detail white-balance accuracy failed: the released frame is {held_mean:.3} codes from the held draft on average, {:.2}% of the drag's own {change_mean:.3}; the limit is 10%. Moving half-detail difference: {moving_mean:.3} codes, {:.2}% of the same change",
                held_ratio * 100.0,
                moving_ratio * 100.0,
            ),
        )?;
        Ok("held_full_detail")
    }
}

/// The exception the owner recorded on 2026-09-30 for the white-balance accuracy gates: a Bayer
/// scene with enough sites at sensor white keeps the approved `W` draft, whose first-order
/// `diag(g'/g)` cannot follow the demosaic's input clamp there. See
/// `docs/decisions.md#raw-white-balance-drafts`.
const HIGHLIGHT_CLIP_EXCEPTION: &str = "highlight-clipped Bayer scene";
/// A site counts as clipped when its normalized, gained value reaches this share of sensor white.
const CLIP_FRACTION: f32 = 0.99;
/// The share of the default crop's sites, clipped at any of the drags' developments, from which a
/// Bayer scene qualifies. Chosen from the 2026-09-30 measurement of 33 sources (see
/// `docs/design/instant-preview.md#popular-cameras`): the five that miss a limit clip 1.25% to
/// 4.07%, and the next Bayer scene below 1% clips 0.61%. Two passing scenes (2.50%, 1.42%) also
/// qualify, which changes nothing for them while their figures stay within the limits.
const MIN_HIGHLIGHT_CLIP_SHARE: f64 = 0.01;

/// How much of the scene the drags' developments clip, from the source itself: the share of its
/// Bayer sites at [`CLIP_FRACTION`] of sensor white or above under the channel-wise largest of the
/// gains the scenario develops at (as shot and each committed temperature), since a site clipped
/// at any of them is one the draft's `diag(g'/g)` cannot follow. `share` is `None` for a
/// development without one clip ceiling: X-Trans, or a DNG corrected after the demosaic.
#[derive(Debug, Clone, PartialEq)]
struct Highlights {
    gains: [f32; 3],
    share: Option<f64>,
}

impl Highlights {
    fn of(source: &Path, developments: &[[f32; 3]]) -> Result<Self> {
        let gains = developments.iter().fold([0.0_f32; 3], |most, gains| {
            std::array::from_fn(|c| most[c].max(gains[c]))
        });
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let raw = luxforge_raw::RawSource::decode(std::fs::read(source)?, &cancel)
            .map_err(|error| format!("{}: {error}", source.display()))?;
        let share = raw
            .highlight_clip_share(gains, CLIP_FRACTION, &cancel)
            .map_err(|error| format!("{}: {error}", source.display()))?;
        Ok(Self { gains, share })
    }

    fn qualifies(&self) -> bool {
        self.share
            .is_some_and(|share| share >= MIN_HIGHLIGHT_CLIP_SHARE)
    }

    fn record(&self) -> Value {
        json!({
            "gains": self.gains,
            "clip_fraction_of_sensor_white": CLIP_FRACTION,
            "clipped_share": self.share,
            "uniform_clip_ceiling": self.share.is_some(),
            "minimum_share": MIN_HIGHLIGHT_CLIP_SHARE,
            "exception": self.qualifies().then_some(HIGHLIGHT_CLIP_EXCEPTION),
        })
    }
}

/// The white-balance accuracy verdict for one drag: the gate of [`check_white_balance_accuracy`],
/// except that a limit a highlight-clipped Bayer scene misses is recorded, not failed. A missing
/// held capture is never excused.
fn white_balance_accuracy_verdict(
    fit: bool,
    moving_mean: f64,
    moving_ratio: f64,
    held: Option<(f64, f64)>,
    change_mean: f64,
    highlights: &Highlights,
) -> Result<Value> {
    ensure(
        fit || held.is_some(),
        "The 100% draft has no held full-detail capture",
    )?;
    let view_state = if fit { "moving" } else { "held_full_detail" };
    match check_white_balance_accuracy(fit, moving_mean, moving_ratio, held, change_mean) {
        Ok(checked) => Ok(json!({"checked": checked, "held": true, "exception": null})),
        Err(failure) if highlights.qualifies() => Ok(json!({
            "checked": view_state,
            "held": false,
            "exception": HIGHLIGHT_CLIP_EXCEPTION,
            "clipped_share": highlights.share,
            "failed_limit": failure.to_string(),
        })),
        Err(failure) => Err(failure),
    }
}

/// How far the photograph in one capture is from another's: the mean absolute channel difference
/// in codes over the photograph as the second frame draws it on screen (its photo rectangle clipped
/// to the canvas and to the surface columns, between the frame's top and bottom tenths, so the
/// title and status bars stay outside), and the share of those pixels differing by more than two
/// codes in any channel. Only the photograph counts: the canvas beside a 3:2 or 4:3 photo at Fit
/// never changes and would dilute the mean. No overlay is on and no draft bar is shown for a
/// slider gesture.
fn surface_difference(first: &Frame, second: &Frame) -> Result<(f64, f64)> {
    let frame = second;
    let first = first.image()?;
    let second = second.image()?;
    ensure(
        first.dimensions() == second.dimensions(),
        "The two captures are different sizes",
    )?;
    let (width, height) = first.dimensions();
    let [left, right] = frame.columns()?.unwrap_or([0, width]);
    let [photo_left, photo_top, photo_right, photo_bottom] = frame.visible_photo()?;
    let (mut total, mut over, mut count) = (0_u64, 0_u64, 0_u64);
    for y in (height / 10).max(photo_top)..(height - height / 10).min(photo_bottom) {
        for x in left.max(photo_left)..right.min(photo_right).min(width) {
            let (a, b) = (first.get_pixel(x, y).0, second.get_pixel(x, y).0);
            let differences = [0, 1, 2].map(|channel| a[channel].abs_diff(b[channel]));
            total += differences
                .iter()
                .map(|value| u64::from(*value))
                .sum::<u64>();
            over += u64::from(differences.iter().any(|value| *value > 2));
            count += 1;
        }
    }
    ensure(count > 0, "The frame draws no photograph on its surface")?;
    Ok((
        total as f64 / (3 * count) as f64,
        over as f64 / count as f64,
    ))
}

/// The events one script step logged, from its own `script_step` to the next one's.
fn step_events(events: &[Value], step: usize) -> Vec<&Value> {
    let mut current = 0;
    events
        .iter()
        .filter(|event| {
            if event["event"] == "script_step" {
                current = event["detail"]["step"].as_u64().unwrap_or(0) as usize;
            }
            current == step
        })
        .collect()
}

/// The events the named step logged. Its number in the script is the one its frame records, which
/// the plan's check has held to the step's own place in the script.
fn step_log<'a>(launch: &'a Checked, step: &str) -> Result<Vec<&'a Value>> {
    let number = launch.at(step)?["step"]["step"]
        .as_u64()
        .ok_or_else(|| format!("Step {step:?} records no script step number"))?;
    Ok(step_events(&launch.events, number as usize))
}

/// The frames presented among `events`.
fn presented<'a>(events: &[&'a Value]) -> Vec<&'a Value> {
    events
        .iter()
        .copied()
        .filter(|event| event["event"] == "preview_displayed")
        .collect()
}

/// One step's events split at its last `script_step_settled`: what happened up to the outcome its
/// frame was captured for, and what the editor went on to do before the next step began. The next
/// step's `script_step` is logged only once this step's capture is written, which can take longer
/// than the shared 120 ms quiet interval, so a held gesture's quiet refinement can land in the
/// tail. A step that never settled has no tail.
fn split_at_settle<'a>(events: &[&'a Value]) -> (Vec<&'a Value>, Vec<&'a Value>) {
    let settled = events
        .iter()
        .rposition(|event| event["event"] == "script_step_settled")
        .map_or(events.len(), |index| index + 1);
    (events[..settled].to_vec(), events[settled..].to_vec())
}

/// The frame captured just before the named step's.
fn frame_before<'a>(launch: &'a Checked, step: &str) -> Result<&'a Frame> {
    launch
        .index(step)?
        .checked_sub(1)
        .map(|before| &launch.frames[before])
        .ok_or_else(|| format!("No frame comes before step {step:?}").into())
}

/// What each frame shows beyond its plan, once the plan has held: every frame ready with Basic's
/// section and no RAW section, the drags' drafted and committed frames, each double-click's events
/// and As shot fields, Basic's dot, the sensor pick `W` enters and the crop on screen.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    for (step, frame) in launch.names().iter().zip(&launch.frames) {
        let state = frame.state();
        ensure(
            state["phase"] == "ready",
            format!("RAW panel step {step:?} is not ready: {}", state["phase"]),
        )?;
        // One Basic section: the RAW development draws none, and Basic's Temperature and Tint are
        // its fields, which exist only because the photograph is RAW.
        ensure(
            state["expanded"].get(RAW_MODULE).is_none(),
            format!(
                "RAW panel step {step:?} lists a RAW section: {}",
                state["expanded"]
            ),
        )?;
        for parameter in [TEMPERATURE, TINT] {
            let field = format!("{SET_RAW}.{parameter}");
            ensure(
                state["controls"][&field].is_string(),
                format!("RAW panel step {step:?} shows no {field}"),
            )?;
        }
    }
    let opened = launch.at(names::OPENED)?;
    checks.note(
        opened,
        "Basic's White balance group, the RAW development's own",
        white_balance_group(opened)?,
    );
    // Whether the scene is a highlight-clipped Bayer one, from the source at the gains the drags
    // develop at, recorded before any gate can fail.
    let developments = std::iter::once(names::OPENED)
        .chain(DRAGS.iter().map(|drag| drag.release))
        .map(|step| development_gains(launch.at(step)?))
        .collect::<Result<Vec<_>>>()?;
    let source = run
        .sources()
        .first()
        .ok_or("The raw-panel run records no source")?
        .clone();
    let highlights = Highlights::of(&source, &developments)?;
    run.record("highlight_clip", highlights.record());
    checks.note(
        opened,
        "how much of the scene the drags' developments clip at sensor white",
        highlights.record(),
    );
    for drag in &DRAGS {
        checks.note(
            launch.at(drag.release)?,
            "a temperature drag's approximate draft and its exact release",
            white_balance_drag(launch, drag, &highlights)?,
        );
    }

    // Each double-click is two history entries, which the plan counts: the first press's committed
    // jump, then the reset, sent against the revision that commit produced and never refused as
    // stale. The plan also holds the field's default, or As shot's label, once the reset has run.
    for click in &DOUBLE_CLICKS {
        let (before, after) = (frame_before(launch, click.step)?, launch.at(click.step)?);
        let field = format!("{}.{}", click.action, click.parameter);
        let logged = step_log(launch, click.step)?;
        let named = |name: &str| -> Vec<&&Value> {
            logged
                .iter()
                .filter(|event| event["event"] == name)
                .collect()
        };
        ensure(
            named("command_failed").is_empty(),
            format!(
                "{field}: a request was refused during the double-click: {:?}",
                named("command_failed")
            ),
        )?;
        let sent = named("field_reset_sent");
        let preset = if click.shows.is_none() {
            json!({"white-balance": "as-shot"})
        } else {
            json!({ click.parameter: 0.0 })
        };
        ensure(
            sent.len() == 1
                && sent[0]["detail"]["action"] == click.reset
                && sent[0]["detail"]["preset"] == preset
                && sent[0]["detail"]["field"]
                    == json!({"action": click.action, "parameter": click.parameter})
                && sent[0]["detail"]["revision"] == json!(before.revision()? + 1),
            format!(
                "{field}: the reset was not {} sent once, after the jump's commit: {sent:?}",
                click.reset
            ),
        )?;
        ensure(
            named("slider_draft_commit").len() == 1,
            format!("{field}: the first press did not commit its jump once"),
        )?;
        let mut shown = json!({"field": after["state"]["controls"][&field]});
        if click.shows.is_none() {
            // As shot: the development is the camera's own white balance, and both white-balance
            // fields show its equivalent.
            ensure(
                raw_payload(after)?["wb_mode"] == "as-shot",
                format!("{field}: the reset left {}", raw_payload(after)?),
            )?;
            shown = shows_as_shot_equivalent(after, &field)?;
        }
        checks.note(
            after,
            "a double-click's jump and reset",
            json!({
                "field": field,
                "reset": click.reset,
                "label": after["state"]["stack"]["label"],
                "revision_before": before.revision()?,
                "revision_after": after.revision()?,
                "reset_sent_at_revision": sent[0]["detail"]["revision"],
                "queued": !named("field_reset_queued").is_empty(),
                "shown": shown,
            }),
        );
    }
    // The RAW development the white-balance resets leave: the camera's own white balance, which
    // is the Original's development.
    let last = DOUBLE_CLICKS
        .iter()
        .rfind(|click| click.action == SET_RAW)
        .ok_or("No double-click resets a RAW white-balance field")?;
    let as_shot = launch.at(last.step)?;
    let raw = raw_payload(as_shot)?;
    ensure(
        raw["wb_mode"] == "as-shot",
        format!("The RAW layer after the white-balance resets is {raw}"),
    )?;
    let reset = launch.at(DOUBLE_CLICKS[DOUBLE_CLICKS.len() - 1].step)?;
    // Basic's dot reads every layer its controls edit: none on the untouched photograph, one once
    // a drag has committed a custom white balance to the RAW development, none again once As shot
    // is back, and none once Exposure is back at 0 EV too.
    for (frame, dotted, when) in [
        (launch.at(names::OPENED)?, false, "untouched"),
        (
            launch.at(DRAGS[0].release)?,
            true,
            "after a committed custom temperature",
        ),
        (as_shot, false, "back at As shot"),
        (reset, false, "back at As shot and 0 EV"),
    ] {
        ensure(
            frame["state"]["active"][BASIC_MODULE] == json!(dotted),
            format!(
                "Basic's dot is {} {when}, not {dotted}",
                frame["state"]["active"][BASIC_MODULE]
            ),
        )?;
        checks.note(
            frame,
            "Basic's dot",
            json!({"when": when, "basic_dot": dotted}),
        );
    }
    sensor_pick(launch)?;
    checks.note(
        launch.at(names::FITTED_AT_FIT)?,
        "the RAW crop drafted, applied and inspected",
        raw_crop(launch)?,
    );
    checks.write(&launch.evidence, SCENARIO, json!({}))
}

/// Basic's White balance group on this RAW photograph's global target: the same four controls, in
/// the same order and under the same labels as on a JPEG, each the RAW development's own —
/// Temperature in kelvin and Tint over `set-raw`, the Neutral picker entering its sensor pick and
/// As shot sending its As shot.
fn white_balance_group(frame: &Value) -> Result<Value> {
    let controls = frame["state"]["section_controls"][BASIC_MODULE]
        .as_array()
        .ok_or("The frame records no Basic section controls")?;
    let start = controls
        .iter()
        .position(|control| control["kind"] == "group" && control["label"] == "White balance")
        .ok_or("The Basic section draws no White balance group")?;
    let group: Vec<&Value> = controls[start + 1..]
        .iter()
        .take_while(|control| control["kind"] != "group")
        .collect();
    let expected = [
        json!({"kind": "number", "label": "Temperature", "action": SET_RAW,
               "parameter": TEMPERATURE, "unit": "K"}),
        json!({"kind": "number", "label": "Tint", "action": SET_RAW, "parameter": TINT,
               "unit": null}),
        json!({"kind": "picker", "label": "Neutral picker", "mode": RAW_MODULE}),
        json!({"kind": "action", "label": "As shot", "action": SET_RAW}),
    ];
    ensure(
        group.len() == expected.len() && group.iter().zip(&expected).all(|(a, b)| *a == b),
        format!("The White balance group on a RAW photograph draws {group:?}"),
    )?;
    Ok(json!({"white_balance_group": group}))
}

/// `W` enters the RAW development's sensor pick, whose canvas mode the plan holds, and Basic's
/// Neutral picker, which that pick provides on this photograph's global target, reads selected
/// under its one letter. Escape returns to the pointer and deselects it.
fn sensor_pick(launch: &Checked) -> Result {
    let entered = &launch.at(names::SENSOR_PICK)?["state"];
    let picker = &entered["pickers"][RAW_MODULE];
    ensure(
        picker["selected"] == json!(true)
            && picker["label"] == "Neutral picker"
            && picker["shortcut"] == "W",
        format!(
            "W did not select the sensor pick: pickers {}",
            entered["pickers"]
        ),
    )?;
    ensure(
        launch.at(names::PICK_LEFT)?["state"]["pickers"][RAW_MODULE]["selected"] == json!(false),
        "Escape did not deselect the sensor pick",
    )
}

/// Under As shot, a frame's temperature and tint fields show the camera's as-shot equivalent: the
/// core's answer for the frame's own RAW layer, to the precision each field declares, and a
/// temperature and tint whose gains are the as-shot gains. Returns what was compared.
fn shows_as_shot_equivalent(frame: &Value, field: &str) -> Result<Value> {
    let payload: luxforge_core::RawPayload = serde_json::from_value(raw_payload(frame)?.clone())?;
    let [kelvin, tint] =
        luxforge_core::temperature_tint_from_gains(payload.as_shot_gains, payload.cam_xyz)
            .map_err(|error| format!("{field}: the as-shot gains have no equivalent: {error}"))?;
    let back = luxforge_core::gains_from_temperature_tint(kelvin, tint, payload.cam_xyz)?;
    // At the ±100 tint limit the core also answers a white up to half a tint unit beyond it, held
    // to the limit, so that answer selects gains within that half unit rather than exactly.
    ensure(
        tint.abs() >= 100.0
            || back
                .iter()
                .zip(payload.as_shot_gains)
                .all(|(gain, shot)| (gain - shot).abs() <= 1.0e-6 * shot),
        format!("{field}: {kelvin} K, {tint} does not reproduce the as-shot gains"),
    )?;
    let controls = &frame["state"]["controls"];
    let registry = luxforge_core::ModuleRegistry::builtin();
    for (action, parameter, expected) in [(SET_RAW, TEMPERATURE, kelvin), (SET_RAW, TINT, tint)] {
        let key = format!("{action}.{parameter}");
        let shown: f64 = controls[&key]
            .as_str()
            .ok_or_else(|| format!("{key} is not shown"))?
            .parse()?;
        // The text is the value rounded to the decimals the parameter declares: half the last one.
        let precision = registry
            .action(action)
            .and_then(|(_, declared)| declared.parameter(parameter))
            .and_then(|declared| declared.precision)
            .ok_or_else(|| format!("{key} declares no precision"))?;
        let tolerance = 0.5 * 10f64.powi(-i32::from(precision)) + 1e-9;
        ensure(
            (shown - expected).abs() <= tolerance,
            format!("{field}: {key} shows {shown}, not the as-shot {expected}"),
        )?;
    }
    Ok(json!({
        "kelvin": controls[format!("{SET_RAW}.{TEMPERATURE}")],
        "tint": controls[format!("{SET_RAW}.{TINT}")],
        "as_shot_equivalent": [kelvin, tint],
        "as_shot_gains": payload.as_shot_gains,
    }))
}

/// The step's events that would mean a commit's picture never reached the canvas: a refused
/// request, a failed render, a picture withdrawn for a failure, or a draft that could not open.
fn expect_no_failure(launch: &Checked, step: &str, what: &str) -> Result {
    let failures: Vec<&Value> = step_log(launch, step)?
        .into_iter()
        .filter(|event| {
            [
                "command_failed",
                "render_failed",
                "preview_withdrawn",
                "crop_draft_failed",
            ]
            .contains(&event["event"].as_str().unwrap_or_default())
        })
        .collect();
    ensure(
        failures.is_empty(),
        format!("{what}: a failure was logged: {failures:?}"),
    )
}

/// The one crop layer of a frame's current stack: its payload. That a later commit updates this
/// same layer is the plan's.
fn crop_layer(frame: &Value) -> Result<luxforge_core::CropPayload> {
    let layers: Vec<&Value> = frame["state"]["stack"]["layers"]
        .as_array()
        .ok_or("The frame records no stack")?
        .iter()
        .filter(|layer| layer["effect"] == CROP_EFFECT)
        .collect();
    ensure(
        layers.len() == 1,
        format!("Expected one crop layer, found {}", layers.len()),
    )?;
    Ok(serde_json::from_value(layers[0]["payload"].clone())?)
}

/// A committed crop frame shows that crop: the entry on the surface is the current one, its
/// dimensions are the output the payload declares on the RAW's upright stage, and no failure
/// stands in for it. This is the check a picture left over from before the commit fails.
fn shows_crop(frame: &Value, source: [u32; 2], angle: f64, what: &str) -> Result<[u32; 2]> {
    let state = &frame["state"];
    let payload = crop_layer(frame)?;
    ensure(
        payload.angle == angle,
        format!("{what}: the crop layer's angle is {}", payload.angle),
    )?;
    let stage = luxforge_core::CropStage {
        width: source[0],
        height: source[1],
        angle,
    };
    let output = payload.output_rect(&stage)?;
    let output = [output.width, output.height];
    let displayed = &state["stack"]["displayed"];
    ensure(
        displayed["entry"] == state["stack"]["entry"],
        format!(
            "{what}: the surface shows entry {} while history's current entry is {}",
            displayed["entry"], state["stack"]["entry"]
        ),
    )?;
    ensure(
        displayed["dimensions"] == json!(output),
        format!(
            "{what}: the surface shows {} for a {output:?} crop",
            displayed["dimensions"]
        ),
    )?;
    ensure(
        state["render_error"].is_null(),
        format!("{what}: {}", state["render_error"]),
    )?;
    Ok(output)
}

/// The area Fit lays the photograph out in, `[left, top, right, bottom]` physical pixels: the canvas
/// less the Fit padding, recorded by the frame from the layout's own constants.
fn fit_area(frame: &Value) -> Result<[u32; 4]> {
    let rect: [u32; 4] = serde_json::from_value(frame["fit_rect"].clone())
        .map_err(|_| "The frame records no Fit rectangle")?;
    ensure(
        rect[0] < rect[2] && rect[1] < rect[3],
        format!("The frame's Fit rectangle {rect:?} is empty"),
    )?;
    Ok(rect)
}

/// The canvas background, read inside the canvas's own corner, which no photograph reaches.
fn canvas_background(image: &image::RgbImage, frame: &Value) -> Result<([u8; 3], [u32; 4])> {
    let rect: [u32; 4] = serde_json::from_value(frame["canvas_rect"].clone())
        .map_err(|_| "The frame records no canvas rectangle")?;
    Ok((image.get_pixel(rect[0] + 4, rect[1] + 4).0, rect))
}

/// How much of the input stage a crop draft draws outside its crop rectangle: the crop canvas's
/// `DIM_OPACITY`, which the photo surface applies as the stage's alpha over the canvas, so a dimmed
/// pixel is the canvas plus this share of the stage's difference from it.
const DRAFT_DIM_OPACITY: f64 = 0.35;
/// How far, in physical pixels, a sample must be from a draft's crop rectangle for the opacity it
/// is drawn at to be known: the rectangle's own edge, its stroke and its handles are skipped.
const DRAFT_BRIGHT_MARGIN: f64 = 4.0;

/// One crop draft at Fit, as its frame records it: where a stage point lands on screen through the
/// draft's rotation and Fit view, and the crop rectangle the stage is drawn at full opacity in.
struct DraftView {
    stage: luxforge_core::CropStage,
    zoom: f64,
    origin: (f64, f64),
    /// The crop rectangle on screen, `[left, top, right, bottom]` physical pixels.
    bright: [f64; 4],
}

impl DraftView {
    /// The rotated box centred in `fit`, the area the layout fits into, with the crop rectangle
    /// `rect` (`[x, y, width, height]` in box pixels) drawn bright.
    fn new(stage: luxforge_core::CropStage, fit: [u32; 4], rect: [f64; 4]) -> Self {
        let (box_width, box_height) = stage.bounding_box();
        let [fit_left, fit_top, fit_right, fit_bottom] = fit.map(f64::from);
        let available = (fit_right - fit_left, fit_bottom - fit_top);
        let zoom = (available.0 / box_width).min(available.1 / box_height);
        let origin = (
            fit_left + (available.0 - box_width * zoom) / 2.0,
            fit_top + (available.1 - box_height * zoom) / 2.0,
        );
        let [x, y, width, height] = rect;
        Self {
            stage,
            zoom,
            origin,
            bright: [
                origin.0 + x * zoom,
                origin.1 + y * zoom,
                origin.0 + (x + width) * zoom,
                origin.1 + (y + height) * zoom,
            ],
        }
    }

    /// The draft a frame records, on the `source` input stage.
    fn of(frame: &Value, source: [u32; 2]) -> Result<Self> {
        let draft = &frame["state"]["crop"];
        let angle = draft["angle"]
            .as_f64()
            .ok_or("The draft records no angle")?;
        let rect: [f64; 4] = serde_json::from_value(draft["rect"].clone())
            .map_err(|_| format!("The draft records no crop rectangle: {draft}"))?;
        let stage = luxforge_core::CropStage {
            width: source[0],
            height: source[1],
            angle,
        };
        Ok(Self::new(stage, fit_area(frame)?, rect))
    }

    /// Where stage point `(u, v)` is drawn, in physical pixels.
    fn screen(&self, u: f64, v: f64) -> (f64, f64) {
        let (x, y) = self.stage.to_box(u, v);
        (self.origin.0 + x * self.zoom, self.origin.1 + y * self.zoom)
    }

    /// The opacity the stage is drawn at here: full inside the crop rectangle, dimmed outside it,
    /// and unknown within [`DRAFT_BRIGHT_MARGIN`] of its edge.
    fn opacity(&self, (x, y): (f64, f64)) -> Option<f64> {
        let [left, top, right, bottom] = self.bright;
        let margin = DRAFT_BRIGHT_MARGIN;
        if x >= left + margin && x <= right - margin && y >= top + margin && y <= bottom - margin {
            Some(1.0)
        } else if x < left - margin || x > right + margin || y < top - margin || y > bottom + margin
        {
            Some(DRAFT_DIM_OPACITY)
        } else {
            None
        }
    }
}

/// How far a sample's scene must be from the canvas, in 8-bit codes on its furthest channel as the
/// straightened draft would draw it, for the canvas there to be a gap rather than dark content.
const SCENE_APART: f64 = 8.0;
/// The largest share of those samples that may show the canvas.
const GAP_SHARE: f64 = 0.005;
/// The fewest samples a straightened draft must be judged on, a quarter of the grid, so a photo
/// dark enough to leave little to judge fails rather than passing unexamined.
const JUDGED_AT_LEAST: u32 = 1200;

/// What a scan of a straightened draft against its unstraightened one found.
#[derive(Debug, PartialEq)]
struct Wholeness {
    /// Grid samples on screen in both drafts, clear of the bars drawn over the canvas.
    sampled: u32,
    /// Those at a known opacity in both whose scene the unstraightened draft shows at least
    /// [`SCENE_APART`] from the canvas throughout a 5 × 5 neighbourhood.
    judged: u32,
    /// Judged samples the straightened draft shows as the canvas, within a code on every channel.
    gaps: u32,
    /// Samples showing the canvas colour anywhere, judged or not: dark content included.
    canvas_coloured: u32,
}

/// Scan a straightened draft against the unstraightened draft of the same input stage, drawn with
/// nothing turned. An 80 × 60 grid of stage points over the stage's interior is mapped into both.
/// The unstraightened draft says what the scene is at each: its distance from the canvas colour,
/// the least over a 5 × 5 neighbourhood so a registration pixel, a thin guide line or noise cannot
/// lift it, taken back through the opacity it is drawn at there and forward through the opacity
/// the straightened draft draws it at. Where that is at least [`SCENE_APART`], the straightened
/// draft cannot show the canvas colour by drawing the scene; showing it there is a gap. Dark scene
/// content is the canvas's colour in both drafts and is never judged, however much of it there is.
fn draft_gaps(
    straightened: (&image::RgbImage, &DraftView),
    reference: (&image::RgbImage, &DraftView),
    background: [u8; 3],
    rows: (f64, f64),
    source: [u32; 2],
) -> Wholeness {
    let (image, view) = straightened;
    let (reference, reference_view) = reference;
    let on = |image: &image::RgbImage, (x, y): (f64, f64)| {
        x >= 0.0
            && y >= rows.0
            && y <= rows.1
            && x < f64::from(image.width())
            && y < f64::from(image.height())
    };
    let apart = |pixel: [u8; 3]| {
        pixel
            .iter()
            .zip(background)
            .map(|(a, b)| a.abs_diff(b))
            .max()
            .unwrap_or(0)
    };
    let mut found = Wholeness {
        sampled: 0,
        judged: 0,
        gaps: 0,
        canvas_coloured: 0,
    };
    for j in 0..60 {
        for i in 0..80 {
            let u = f64::from(source[0]) * (0.03 + 0.94 * (f64::from(i) + 0.5) / 80.0);
            let v = f64::from(source[1]) * (0.03 + 0.94 * (f64::from(j) + 0.5) / 60.0);
            let (at, then) = (view.screen(u, v), reference_view.screen(u, v));
            if !on(image, at) || !on(reference, then) {
                continue;
            }
            found.sampled += 1;
            let canvas = apart(image.get_pixel(at.0 as u32, at.1 as u32).0) <= 1;
            found.canvas_coloured += u32::from(canvas);
            let (Some(drawn), Some(drawn_then)) = (view.opacity(at), reference_view.opacity(then))
            else {
                continue;
            };
            let (cx, cy) = (then.0 as i64, then.1 as i64);
            let nearest = (-2..=2)
                .flat_map(|dy| (-2..=2).map(move |dx| (cx + dx, cy + dy)))
                .map(|(x, y)| {
                    let x = x.clamp(0, i64::from(reference.width()) - 1) as u32;
                    let y = y.clamp(0, i64::from(reference.height()) - 1) as u32;
                    apart(reference.get_pixel(x, y).0)
                })
                .min()
                .unwrap_or(0);
            if f64::from(nearest) / drawn_then * drawn >= SCENE_APART {
                found.judged += 1;
                found.gaps += u32::from(canvas);
            }
        }
    }
    found
}

/// A straightened draft draws its whole input stage as one rotated picture: at the stage points
/// where the unstraightened draft of the same stage shows a scene clearly apart from the canvas,
/// almost no sample of the straightened draft shows the canvas ([`draft_gaps`]). Drawn as the
/// toolkit's own fragments of an image wider than one atlas layer, each turned about its own
/// centre, it showed the background through 12% of the grid on the X100VI at 7°, and 66% at 44°.
/// Counting every canvas-coloured sample instead failed the 5D Mark IV's chart on a dark ground,
/// whose scene is the canvas colour, within a code, at 1.4% of the grid.
fn draft_is_whole(frame: &Frame, reference: &Frame, source: [u32; 2]) -> Result<Value> {
    let view = DraftView::of(frame, source)?;
    let reference_view = DraftView::of(reference, source)?;
    ensure(
        reference_view.stage.angle == 0.0,
        format!(
            "The unstraightened draft is at {}°",
            reference_view.stage.angle
        ),
    )?;
    let image = frame.image()?;
    let (background, [_, top, _, bottom]) = canvas_background(image, frame)?;
    let scale = frame["scale"]
        .as_f64()
        .ok_or("The frame records no scale")?;
    // The draft bar and the mode strip are drawn over the canvas; rows under them are skipped.
    let rows = (
        f64::from(top) + 70.0 * scale,
        f64::from(bottom) - 70.0 * scale,
    );
    let found = draft_gaps(
        (image, &view),
        (reference.image()?, &reference_view),
        background,
        rows,
        source,
    );
    whole(&found)?;
    Ok(json!({
        "angle": view.stage.angle,
        "samples": found.sampled,
        "judged_samples": found.judged,
        "gap_samples": found.gaps,
        "canvas_coloured_samples": found.canvas_coloured,
        "background_rgb": background,
        "scene_apart_codes": SCENE_APART,
        "threshold_share": GAP_SHARE,
        "judged_at_least": JUDGED_AT_LEAST,
    }))
}

/// The verdict on a scan: enough judged samples, and under [`GAP_SHARE`] of them gaps.
fn whole(found: &Wholeness) -> Result {
    let Wholeness {
        sampled,
        judged,
        gaps,
        ..
    } = *found;
    ensure(
        sampled >= 2000 && judged >= JUDGED_AT_LEAST,
        format!(
            "Only {judged} of {sampled} samples of the straightened draft have a scene clearly apart from the canvas: too few to judge whether it is drawn as one picture"
        ),
    )?;
    let share = f64::from(gaps) / f64::from(judged);
    ensure(
        share < GAP_SHARE,
        format!(
            "The straightened draft shows the canvas at {gaps} of {judged} samples whose scene is clearly apart from it ({:.1}%): it is not drawn as one picture",
            share * 100.0
        ),
    )
}

/// A committed crop at Fit: the photograph on the canvas — where the editor records drawing it, its
/// edges where its pixels end against the canvas background — is the crop's output fitted into the
/// photo area and centred in it, not the picture from before the commit.
fn fit_placement(frame: &Frame, output: [u32; 2]) -> Result<Value> {
    let image = frame.image()?;
    let (background, _) = canvas_background(image, frame)?;
    // The area Fit lays the photograph out in: the canvas less the Fit padding, as the frame
    // records it, which is taller at the top than at the bottom where the mode strip floats.
    let [fit_left, fit_top, fit_right, fit_bottom] = fit_area(frame)?;
    let available = (
        f64::from(fit_right - fit_left),
        f64::from(fit_bottom - fit_top),
    );
    let fit = (available.0 / f64::from(output[0])).min(available.1 / f64::from(output[1]));
    let expected = (f64::from(output[0]) * fit, f64::from(output[1]) * fit);
    let [x0, y0, x1, y1] =
        frame.photo_edges(|pixel| pixel.iter().zip(background).any(|(a, b)| a.abs_diff(b) > 1))?;
    let measured = (f64::from(x1 - x0), f64::from(y1 - y0));
    let centre = (
        (f64::from(x0) + f64::from(x1)) / 2.0,
        (f64::from(y0) + f64::from(y1)) / 2.0,
    );
    let wanted_centre = (
        f64::from(fit_left + fit_right) / 2.0,
        f64::from(fit_top + fit_bottom) / 2.0,
    );
    let tolerance = 4.0;
    ensure(
        (measured.0 - expected.0).abs() <= tolerance
            && (measured.1 - expected.1).abs() <= tolerance
            && (centre.0 - wanted_centre.0).abs() <= tolerance
            && (centre.1 - wanted_centre.1).abs() <= tolerance,
        format!(
            "The photograph measures {measured:?} at {centre:?}; the {output:?} crop fits as {expected:?} at {wanted_centre:?}"
        ),
    )?;
    Ok(json!({
        "output": output,
        "measured": [measured.0, measured.1],
        "expected": [expected.0, expected.1],
        "centre": [centre.0, centre.1],
        "tolerance_px": tolerance,
    }))
}

/// A point sample of the committed crop at 100%: the codes `render.sample` answered for one stage
/// pixel are the codes the canvas shows at that pixel, one stage pixel per physical pixel from the
/// corner of the rectangle the editor records drawing the photograph in. The sample is the owner's
/// own point evaluation of the current stack, so this ties the picture on screen to the committed
/// recipe.
fn sample_on_screen(frame: &Frame, point: (u32, u32), output: [u32; 2]) -> Result<Value> {
    let state = &frame["state"];
    ensure(
        state["surface"]["raster"] == json!(output) && state["proxy"]["presented"] == json!(false),
        format!(
            "The 100% view is not the exact {output:?} crop: raster {}",
            state["surface"]["raster"]
        ),
    )?;
    let answer = &frame["step"]["result"];
    let codes: [u8; 4] = serde_json::from_value(answer["rgba"].clone())
        .map_err(|_| format!("render.sample at {point:?} carries no codes: {answer}"))?;
    let image = frame.image()?;
    let [left, top, right, bottom] = frame.photo_rect()?;
    ensure(
        [right - left, bottom - top] == output.map(i64::from),
        format!(
            "The 100% view draws the {output:?} crop in {:?}",
            [left, top, right, bottom]
        ),
    )?;
    let screen = (
        u32::try_from(left + i64::from(point.0))?,
        u32::try_from(top + i64::from(point.1))?,
    );
    let shown = image.get_pixel(screen.0, screen.1).0;
    ensure(
        shown
            .iter()
            .zip(codes)
            .all(|(shown, code)| shown.abs_diff(code) <= 1),
        format!(
            "Stage pixel {point:?} reads {codes:?} but the canvas shows {shown:?} at {screen:?}"
        ),
    )?;
    Ok(json!({
        "point": [point.0, point.1],
        "screen": [screen.0, screen.1],
        "sampled": codes,
        "shown": shown,
        "tolerance_codes": 1,
    }))
}

/// The straightened crop drafted, applied and inspected on the RAW itself: the draft draws its
/// whole input stage, Apply commits one entry whose picture is the one on screen at Fit and at
/// 100%, where `render.sample`'s codes are the canvas's own, and a `crop-fit` through the API
/// at 100% updates the same layer and is shown the same way. The entries each commit makes, and
/// that the `crop-fit` keeps the applied crop's layer, are the plan's.
fn raw_crop(launch: &Checked) -> Result<Value> {
    let source: [u32; 2] =
        serde_json::from_value(launch.at(names::OPENED)?["state"]["source_dimensions"].clone())
            .map_err(|_| "The open frame records no source dimensions")?;

    let started = launch.at(names::CROP_STARTED)?;
    expect_no_failure(launch, names::CROP_STARTED, "The draft's start")?;
    let draft = &started["state"]["crop"];
    ensure(
        draft["drafting"] == json!(true)
            && draft["input_stage"] == json!(source)
            && draft["input_stage_loaded"] == json!(true),
        format!("The draft did not open on the {source:?} stage: {draft}"),
    )?;
    let straightened = launch.at(names::CROP_STRAIGHTENED)?;
    ensure(
        straightened["state"]["crop"]["angle"] == json!(CROP_ANGLE),
        "The draft was not straightened",
    )?;
    let whole = draft_is_whole(straightened, started, source)?;

    let applied = launch.at(names::CROP_APPLIED)?;
    expect_no_failure(launch, names::CROP_APPLIED, "Apply")?;
    ensure(
        step_log(launch, names::CROP_APPLIED)?
            .iter()
            .filter(|event| event["event"] == "crop_draft_applied")
            .count()
            == 1
            && applied["state"]["crop"]["drafting"] == json!(false),
        "Apply did not apply the draft once and end it",
    )?;
    let output = shows_crop(applied, source, CROP_ANGLE, "The applied crop at Fit")?;
    ensure(
        applied["state"]["proxy"]["presented"] == json!(true),
        "The applied crop at Fit is not the display proxy",
    )?;
    let placement = fit_placement(applied, output)?;

    let exact = launch.at(names::CROP_AT_100)?;
    shows_crop(exact, source, CROP_ANGLE, "The applied crop at 100%")?;
    let mut samples = Vec::new();
    for (step, point) in SAMPLES {
        let frame = launch.at(step)?;
        shows_crop(frame, source, CROP_ANGLE, "A sample of the applied crop")?;
        samples.push(sample_on_screen(frame, point, output)?);
    }

    let fitted = launch.at(names::CROP_FITTED)?;
    expect_no_failure(launch, names::CROP_FITTED, "The API's crop-fit")?;
    let refitted = shows_crop(fitted, source, FIT_ANGLE, "The API's crop at 100%")?;
    ensure(
        fitted["state"]["surface"]["raster"] == json!(refitted),
        format!(
            "The API's crop at 100% is not drawn exactly: raster {}",
            fitted["state"]["surface"]["raster"]
        ),
    )?;
    let frame = launch.at(names::FITTED_SAMPLE)?;
    shows_crop(frame, source, FIT_ANGLE, "A sample of the API's crop")?;
    samples.push(sample_on_screen(frame, SAMPLES[1].1, refitted)?);
    let back = launch.at(names::FITTED_AT_FIT)?;
    shows_crop(back, source, FIT_ANGLE, "The API's crop back at Fit")?;
    let back_placement = fit_placement(back, refitted)?;
    Ok(json!({
        "crop": {
            "source": source,
            "straightened_draft": whole,
            "applied": {"output": output, "fit": placement},
            "samples": samples,
            "api_crop_fit": {"angle": FIT_ANGLE, "output": refitted, "fit": back_placement},
        }
    }))
}

/// One temperature drag and its release.
///
/// Left open, the drafted frame is labelled approximate — in state, status and every event — and
/// changes the photograph plainly. Fit displays the whole proxy; 100% motion displays a half-
/// detail region. A held 100% pause must refine an exact visible region with the same approximate
/// white balance, and its captured full-detail frame supplies the WB accuracy comparison. No
/// approximate report is adopted: the last exact plot remains marked updating.
///
/// Released, the commit redevelops the mosaic and adopts the exact report. At Fit its proxy is the
/// first new photo; at 100% its exact region precedes the whole frame, all under one committed
/// generation. White-balance accuracy compares the moving Fit capture or the held full-detail
/// 100% capture with the release, within a tenth of the drag's own image change; Fit is also within
/// a code. Moving 100% differences remain reported for independent visual assessment. The plan
/// checks one history entry.
fn white_balance_drag(launch: &Checked, drag: &Drag, highlights: &Highlights) -> Result<Value> {
    let kelvin = drag.kelvin;
    let (before, drafted, released) = (
        frame_before(launch, drag.drag)?,
        launch.at(drag.drag)?,
        launch.at(drag.release)?,
    );
    let state = &drafted["state"];
    let generation = state["surface"]["generation"].clone();
    ensure(
        state["proxy"]["presented"] == json!(drag.fit),
        format!(
            "The drafted frame at {} is {}the proxy",
            if drag.fit { "Fit" } else { "100%" },
            if drag.fit { "not " } else { "" }
        ),
    )?;
    ensure(
        state["approximate_white_balance"] == json!(true),
        format!(
            "The open temperature drag's frame is not labelled approximate: {}",
            state["approximate_white_balance"]
        ),
    )?;
    ensure(
        state["draft"]["action"] == SET_RAW
            && state["draft"]["fields"][TEMPERATURE] == json!(kelvin)
            && !state["displayed_draft_revision"].is_null(),
        format!(
            "The open drag's frame is not its drafted value: {}",
            state["draft"]
        ),
    )?;
    // The status bar calls both the whole Fit proxy and the half-detail viewport provisional.
    // `proxy.presented` distinguishes the whole proxy from a 100% region.
    let render = state["status_bar"]["render"].as_str().unwrap_or_default();
    ensure(
        render.starts_with("Approximate render \u{b7} ")
            && (render.ends_with(" ms") || render.ends_with(" s"))
            && state["status_bar"]["render_approximate"] == true
            && state["status_bar"]["render_proxy"] == true,
        format!(
            "The status bar does not say the drafted frame is approximate: {render:?}, proxy {}",
            state["status_bar"]["render_proxy"]
        ),
    )?;
    let histogram = &state["histogram"];
    ensure(
        histogram["status"] == "updating"
            && histogram["identity"]["generation"] != generation
            && histogram["identity"]["draft_revision"].is_null(),
        format!("The histogram was adopted from the approximate frame: {histogram}"),
    )?;
    let drag_events = step_log(launch, drag.drag)?;
    // The moving frames are those up to the drag's settle, which its capture shows. At 100% the
    // held draft's quiet refinement, a later generation, may begin before the next step is logged:
    // the quiet check below reads that tail.
    let (moving_events, held_tail) = split_at_settle(&drag_events);
    let displayed = presented(&moving_events);
    let all_displayed = presented(&drag_events);
    ensure(
        !displayed.is_empty()
            && all_displayed
                .iter()
                .all(|event| event["detail"]["approximate_white_balance"] == true),
        format!(
            "The drafted generation's frames are not all labelled approximate: {all_displayed:?}"
        ),
    )?;
    ensure(
        displayed
            .iter()
            .all(|event| event["detail"]["generation"] == generation)
            && all_displayed.iter().all(|event| {
                let detail = &event["detail"];
                detail["draft_revision"] == state["displayed_draft_revision"]
                    && detail["entry_id"] == state["stack"]["displayed"]["entry"]
                    && detail["snapshot_id"] == state["stack"]["displayed"]["snapshot"]
            }),
        "A drafted frame has the wrong generation, revision, entry or snapshot",
    )?;
    if drag.fit {
        ensure(
            displayed.iter().all(|event| {
                event["detail"]["path"] == "surface" && event["detail"]["proxy"] == true
            }),
            "The moving Fit draft was not the whole display proxy",
        )?;
    } else {
        let gpu = &state["surface"]["gpu"];
        let full = state["preview_dimensions"]
            .as_array()
            .ok_or("The 100% draft has no full-stage dimensions")?;
        let region_matches = |event: &&Value| {
            let detail = &event["detail"];
            let half = detail["region_stage"].as_array();
            let rect = detail["region"].as_array();
            detail["path"] == "region"
                && detail["quality"] == "interactive"
                && detail["proxy_approximate"] == true
                && detail["viewport_declined"].is_null()
                && detail["dimensions"] == state["preview_dimensions"]
                && half.is_some_and(|stage| {
                    stage.len() == 2
                        && (0..2).all(|axis| {
                            full[axis]
                                .as_u64()
                                .zip(stage[axis].as_u64())
                                .is_some_and(|(full, half)| half == full.div_ceil(2))
                        })
                })
                && rect.is_some_and(|rect| {
                    rect.len() == 4
                        && rect[0]
                            .as_u64()
                            .zip(rect[2].as_u64())
                            .is_some_and(|(a, b)| a < b)
                        && rect[1]
                            .as_u64()
                            .zip(rect[3].as_u64())
                            .is_some_and(|(a, b)| a < b)
                        && rect[2].as_u64() <= full[0].as_u64()
                        && rect[3].as_u64() <= full[1].as_u64()
                })
        };
        ensure(
            displayed.iter().all(region_matches)
                && state["surface"]["version"] == before["state"]["surface"]["version"]
                && state["surface"]["detail_updating"] == true
                && gpu["drawn_full_version"].is_null()
                && gpu["drawn_region_generation"] == generation
                && gpu["drawn_region_quality"] == "interactive"
                && gpu["drawn_region_version"].is_u64()
                && gpu["drawn_content"].is_u64()
                && gpu["drawn_regions"].as_array().is_some_and(|regions| {
                    regions.iter().any(|region| {
                        region["content"] == gpu["drawn_content"]
                            && region["generation"] == generation
                            && region["quality"] == "interactive"
                            && region["version"] == gpu["drawn_region_version"]
                    })
                }),
            "The moving 100% WB draft was not a matching drawn half-detail viewport region",
        )?;
    }
    ensure(
        !drag_events.iter().any(|event| {
            event["event"] == "analysis_adopted" && event["detail"]["generation"] == generation
        }),
        "A report was adopted for the approximate generation",
    )?;
    let unpreviewed = drag_events
        .iter()
        .filter(|event| event["event"] == "slider_draft_unpreviewed")
        .count();
    ensure(
        unpreviewed == 0,
        format!("{unpreviewed} drafted values had no preview"),
    )?;
    let (drag_mean, drag_over) = surface_difference(before, drafted)?;
    ensure(
        drag_mean > 1.0 && drag_over > 0.1,
        format!(
            "The drafted frame barely differs from the one before the drag: {drag_mean:.3} codes on average, {:.1}% of pixels over 2",
            drag_over * 100.0
        ),
    )?;

    let quality_draft = if let Some(name) = drag.quiet {
        let quiet = launch.at(name)?;
        let paused = &quiet["state"];
        let quiet_generation = &paused["surface"]["generation"];
        // The held draft's quiet interval runs from the drag's last input, so its refinement may
        // begin in the drag step's tail, after the moving frame was captured.
        let quiet_events: Vec<&Value> = held_tail
            .iter()
            .copied()
            .chain(step_log(launch, name)?)
            .collect();
        let presented = presented(&quiet_events);
        ensure(
            quiet_events
                .iter()
                .any(|event| event["event"] == "preview_quiet_refine")
                && presented.iter().any(|event| {
                    event["detail"]["path"] == "region" && event["detail"]["quality"] == "exact"
                })
                && presented.iter().all(|event| {
                    let detail = &event["detail"];
                    detail["generation"] == *quiet_generation
                        && detail["draft_revision"] == state["displayed_draft_revision"]
                        && detail["entry_id"] == state["stack"]["displayed"]["entry"]
                        && detail["snapshot_id"] == state["stack"]["displayed"]["snapshot"]
                        && detail["dimensions"] == state["preview_dimensions"]
                        && detail["approximate_white_balance"] == true
                        && (detail["path"] == "surface"
                            || detail["path"] == "region"
                                && detail["quality"] == "exact"
                                && detail["region_stage"] == state["preview_dimensions"]
                                && detail["proxy_approximate"] == false)
                })
                && !quiet_events
                    .iter()
                    .any(|event| event["event"] == "analysis_adopted"),
            "The held 100% WB draft did not refine an exact matching region without adopting an approximate report",
        )?;
        let gpu = &paused["surface"]["gpu"];
        let drew_exact_region = gpu["drawn_region_generation"] == *quiet_generation
            && gpu["drawn_region_quality"] == "exact"
            && gpu["drawn_region_version"].is_u64();
        let drew_full_detail = gpu["drawn_full_version"] == paused["surface"]["version"]
            && gpu["drawn_full_version"].is_u64()
            && paused["surface"]["raster"] == paused["preview_dimensions"];
        ensure(
            paused["draft"]["draft_id"] == state["draft"]["draft_id"]
                && paused["draft"]["fields"][TEMPERATURE] == json!(kelvin)
                && paused["displayed_draft_revision"] == state["displayed_draft_revision"]
                && paused["stack"]["revision"] == state["stack"]["revision"]
                && paused["stack"]["displayed"]["entry"] == state["stack"]["displayed"]["entry"]
                && paused["approximate_white_balance"] == true
                && paused["proxy"]["presented"] == false
                && paused["histogram"]["status"] == "updating"
                && paused["histogram"]["stale"] == true
                && paused["histogram"]["identity"]["draft_revision"].is_null()
                && paused["histogram"]["identity"]["generation"] != *quiet_generation
                && paused["status_bar"]["render_approximate"] == true
                && gpu["drawn_content"].is_u64()
                && (drew_exact_region || drew_full_detail),
            "The held WB capture is not full-detail approximate pixels of the same draft with its old exact histogram",
        )?;
        quiet
    } else {
        drafted
    };

    let state = &released["state"];
    let generation = state["surface"]["generation"].clone();
    ensure(
        state["approximate_white_balance"] == json!(false)
            && raw_payload(released)?["temperature_kelvin"] == json!(kelvin)
            && raw_payload(released)?["wb_mode"] == "custom",
        format!(
            "The release did not land the exact committed frame: approximate {}, RAW {}",
            state["approximate_white_balance"],
            raw_payload(released)?
        ),
    )?;
    let histogram = &state["histogram"];
    ensure(
        histogram["status"] == "ready"
            && histogram["identity"]["generation"] == generation
            && histogram["identity"]["draft_revision"].is_null(),
        format!("The committed frame's own report is not plotted: {histogram}"),
    )?;
    let release_events = step_log(launch, drag.release)?;
    let commit = release_events
        .iter()
        .position(|event| event["event"] == "slider_draft_commit")
        .ok_or("The release did not commit")?;
    let after: Vec<&&Value> = release_events[commit..]
        .iter()
        .filter(|event| event["event"] == "preview_displayed")
        .collect();
    let first = after
        .first()
        .ok_or("Nothing was presented after the commit")?;
    ensure(
        after.iter().all(|event| {
            let detail = &event["detail"];
            detail["generation"] == generation
                && detail["draft_revision"].is_null()
                && detail["entry_id"] == state["stack"]["displayed"]["entry"]
                && detail["snapshot_id"] == state["stack"]["displayed"]["snapshot"]
                && detail["dimensions"] == state["preview_dimensions"]
                && detail["approximate_white_balance"] == false
        }),
        format!("A stale, drafted or approximate frame was presented after the commit: {after:?}"),
    )?;
    if drag.fit {
        ensure(
            after.len() == 1
                && first["detail"]["path"] == "surface"
                && first["detail"]["proxy"] == true,
            "Fit release did not present its one committed display proxy",
        )?;
    } else {
        let whole = after.last().ok_or("No committed whole frame")?;
        ensure(
            after.len() == 2
                && first["detail"]["path"] == "region"
                && first["detail"]["quality"] == "exact"
                && first["detail"]["region_stage"] == state["preview_dimensions"]
                && first["detail"]["proxy_approximate"] == false
                && whole["detail"]["path"] == "surface"
                && whole["detail"]["proxy"] == false
                && state["proxy"]["presented"] == false
                && state["surface"]["detail_updating"] == false,
            "100% release did not present an exact region followed by the same committed whole frame",
        )?;
    }
    ensure(
        !release_events
            .iter()
            .any(|event| event["event"] == "render_failed"),
        "A render failed between the release and the committed frame",
    )?;
    // Exactly one new *full-slot* version reaches the surface. At 100% an exact region uses its
    // separate region slot before that whole frame; counting only the full version preserves the
    // original stale-whole-frame fence without rejecting the required region refinement.
    let versions = (
        quality_draft["state"]["surface"]["version"].as_u64(),
        state["surface"]["version"].as_u64(),
    );
    ensure(
        matches!(versions, (Some(drafted), Some(released)) if released == drafted + 1),
        format!(
            "The surface was handed more than the committed frame after the drag: {versions:?}"
        ),
    )?;
    ensure(
        state["surface"]["gpu"]["drawn_full_version"] == state["surface"]["version"]
            && (drag.fit
                || state["surface"]["gpu"]["drawn_content"].is_u64()
                    && state["surface"]["gpu"]["drawn_region_version"].is_null()),
        "The exact committed whole frame was not the photographed surface's encoded draw",
    )?;
    let analyses: Vec<&&Value> = release_events
        .iter()
        .filter(|event| event["event"] == "analysis_adopted")
        .collect();
    ensure(
        !analyses.is_empty()
            && analyses.iter().all(|event| {
                event["detail"]["generation"] == generation
                    && event["detail"]["entry_id"] == state["stack"]["displayed"]["entry"]
                    && event["detail"]["draft_revision"].is_null()
            }),
        "A stale report was adopted, or the committed frame's own report was not adopted",
    )?;
    let (moving_mean, moving_over) = surface_difference(drafted, released)?;
    let held_difference = drag
        .quiet
        .map(|_| surface_difference(quality_draft, released))
        .transpose()?;
    let (change_mean, _) = surface_difference(before, released)?;
    ensure(
        change_mean > 0.0,
        "The temperature drag caused no photographed change",
    )?;
    let moving_ratio = moving_mean / change_mean;
    let held_accuracy = held_difference.map(|(mean, _)| (mean, mean / change_mean));
    let held_ratio = held_accuracy.map(|(_, ratio)| ratio);
    let accuracy = white_balance_accuracy_verdict(
        drag.fit,
        moving_mean,
        moving_ratio,
        held_accuracy,
        change_mean,
        highlights,
    )?;
    let accuracy_ratio = if drag.fit {
        moving_ratio
    } else {
        held_ratio.expect("the 100% accuracy check requires a held capture")
    };
    let tint = keeps_the_tint_in_force(before, drafted, released, drag)?;
    Ok(json!({
        "step": drag.drag,
        "kelvin": kelvin,
        "tint": tint,
        "view": if drag.fit { "fit" } else { "100%" },
        "drafted_frame": drafted["file"],
        "drafted_render": drafted["state"]["status_bar"]["render"],
        "drafted_histogram": drafted["state"]["histogram"]["status"],
        "drafted_against_before": {"mean_codes": drag_mean, "share_over_2": drag_over},
        "quiet_full_detail_frame": drag.quiet.map(|_| quality_draft["file"].clone()),
        "moving_against_released": {"mean_codes": moving_mean, "share_over_2": moving_over},
        "released_frame": released["file"],
        "released_render": state["status_bar"]["render"],
        "released_histogram": histogram["status"],
        "released_against_held_full_detail_draft": held_difference.map(|(mean, over)| json!({"mean_codes": mean, "share_over_2": over})),
        "released_against_before": {"mean_codes": change_mean},
        "moving_share_of_change": moving_ratio,
        "held_full_detail_share_of_change": held_ratio,
        "accuracy_checked_view_state": accuracy["checked"],
        "accuracy": accuracy,
        "accuracy_share_of_change": accuracy_ratio,
        "surface_versions": [versions.0, versions.1],
    }))
}

/// A temperature drag keeps the tint in force, as Lightroom's Temp does: the committed payload's
/// tint is the core's answer for the development before the drag — for the first drag, which starts
/// from the untouched photograph, the camera's as-shot equivalent — and the Tint field reads the
/// same before the drag, while it is open and once it is released.
fn keeps_the_tint_in_force(
    before: &Value,
    drafted: &Value,
    released: &Value,
    drag: &Drag,
) -> Result<Value> {
    let prior: luxforge_core::RawPayload = serde_json::from_value(raw_payload(before)?.clone())?;
    if drag.drag == DRAGS[0].drag {
        ensure(
            prior.wb_mode == luxforge_core::WhiteBalanceMode::AsShot,
            "The first temperature drag does not start from As shot",
        )?;
    }
    let [_, in_force] = prior.white_balance_controls();
    let committed = raw_payload(released)?["tint"]
        .as_f64()
        .ok_or("The committed RAW layer has no tint")?;
    ensure(
        (committed - in_force).abs() <= 1e-9,
        format!("The temperature drag committed tint {committed}, not the {in_force} in force"),
    )?;
    let field = format!("{SET_RAW}.{TINT}");
    let shown = [before, drafted, released].map(|frame| frame["state"]["controls"][&field].clone());
    ensure(
        shown
            .iter()
            .all(|text| *text == shown[0] && text.is_string()),
        format!("The Tint field moved during a Temperature drag: {shown:?}"),
    )?;
    Ok(json!({
        "from": if prior.wb_mode == luxforge_core::WhiteBalanceMode::AsShot { "as-shot" } else { "custom" },
        "in_force": in_force,
        "committed": committed,
        "field": shown[0],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measurement, not a gate: the highlight clip share of recorded runs, one `RUN_DIR SOURCE`
    /// pair per line of the file `RAW_PANEL_CLIP_RUNS` names, from each run's opened and released
    /// frames' RAW gains. `RAW_PANEL_CLIP_RUNS=list cargo test -p xtask clip_share_of_recorded
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads recorded runs and their RAW sources"]
    fn clip_share_of_recorded_runs() {
        let list = std::fs::read_to_string(std::env::var("RAW_PANEL_CLIP_RUNS").unwrap()).unwrap();
        for line in list.lines().filter(|line| !line.trim().is_empty()) {
            let (run, source) = line.split_once(' ').unwrap();
            let state = |index: usize| -> Value {
                let path = Path::new(run).join(format!("app/state-{index}.json"));
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
            };
            // The opened frame and the two releases: frames 1, 3 and 7 of the plan.
            let gains: Vec<[f32; 3]> = [1, 3, 7]
                .map(|index| development_gains(&state(index)).unwrap())
                .into();
            let highlights = Highlights::of(Path::new(source), &gains).unwrap();
            println!("{run} {}", highlights.record());
        }
    }

    /// The draft dimming this check undoes is the app's own: xtask does not link the app, so the
    /// copy is held to `DIM_OPACITY` in the crop canvas source.
    #[test]
    fn draft_dim_opacity_is_the_crop_canvas_constant() {
        let source = include_str!("../../crates/luxforge-app/src/view/crop_canvas.rs");
        let value = source
            .lines()
            .find_map(|line| line.trim().strip_prefix("const DIM_OPACITY: f32 = "))
            .and_then(|rest| rest.strip_suffix(';'))
            .expect("the crop canvas declares DIM_OPACITY");
        assert_eq!(value.parse::<f64>().unwrap(), DRAFT_DIM_OPACITY);
    }

    /// The recorded Z6 and Canon 90D failures on a loaded host: the 100% drag settled on its
    /// half-detail draft, its capture took longer than the 120 ms quiet interval, and the quiet
    /// refinement began (and on the Z6 presented its exact region) before the wait step was logged.
    /// The drag's moving frames end at its settle; the quiet check reads the tail with the wait.
    #[test]
    fn a_quiet_refinement_before_the_next_step_belongs_to_the_held_draft() {
        let event = |name: &str, detail: Value| json!({"event": name, "detail": detail});
        let events = [
            event("script_step", json!({"step": 4})),
            event("slider_draft_preview", json!({"generation": 6})),
            event(
                "preview_displayed",
                json!({"generation": 6, "path": "region", "quality": "interactive"}),
            ),
            event(
                "script_step_settled",
                json!({"step": 4, "waited_for": "slider_draft", "by": "presented_draft"}),
            ),
            event("preview_quiet_refine", json!({})),
            event(
                "preview_displayed",
                json!({"generation": 7, "path": "region", "quality": "exact"}),
            ),
            event("script_step", json!({"step": 5})),
            event(
                "preview_displayed",
                json!({"generation": 7, "path": "surface", "proxy": false}),
            ),
        ];
        let drag = step_events(&events, 4);
        let (moving, tail) = split_at_settle(&drag);
        assert_eq!(
            presented(&moving)
                .iter()
                .map(|event| &event["detail"]["generation"])
                .collect::<Vec<_>>(),
            [&json!(6)],
            "only the frame the drag's capture shows is a moving frame"
        );
        assert_eq!(tail[0]["event"], "preview_quiet_refine");
        assert_eq!(presented(&tail)[0]["detail"]["quality"], "exact");
        assert_eq!(step_events(&events, 5).len(), 2);
        // A step that never settled keeps every event as its own.
        let (all, none) = split_at_settle(&drag[..3]);
        assert_eq!((all.len(), none.len()), (3, 0));
    }

    /// A scene qualifies as highlight-clipped Bayer from its clip share alone: at or above the
    /// minimum, and only with one clip ceiling. The lowest failing source measured, the A7 IV
    /// (6932), clipped 1.25%; the highest below it, the A6700 (6735), 0.61%.
    #[test]
    fn a_highlight_clipped_bayer_scene_is_decided_by_its_clip_share() {
        let scene = |share| Highlights {
            gains: [2.0, 1.0, 2.5],
            share,
        };
        assert!(scene(Some(0.012_54)).qualifies());
        assert!(scene(Some(MIN_HIGHLIGHT_CLIP_SHARE)).qualifies());
        assert!(!scene(Some(0.006_11)).qualifies());
        assert!(!scene(Some(0.0)).qualifies());
        assert!(
            !scene(None).qualifies(),
            "X-Trans or a corrected DNG never qualifies"
        );
        let record = scene(Some(0.04)).record();
        assert_eq!(record["exception"], HIGHLIGHT_CLIP_EXCEPTION);
        assert_eq!(record["uniform_clip_ceiling"], true);
        assert!(scene(None).record()["exception"].is_null());
    }

    /// A qualifying scene's missed limits are recorded with the exception, its share and the limit
    /// it failed; a non-qualifying scene fails exactly as the gate does; figures within the limits
    /// carry no exception either way; a missing held capture is never excused.
    #[test]
    fn a_highlight_clipped_scene_records_its_missed_accuracy_limits_and_others_fail() {
        let clipped = Highlights {
            gains: [2.4, 1.0, 2.6],
            share: Some(0.0407),
        };
        let unclipped = Highlights {
            gains: [2.0, 1.0, 1.8],
            share: Some(0.0),
        };
        // The OM-1's Fit drag and the S5II's held 100% drag, as measured on 2026-09-30.
        let om1 = |highlights| {
            white_balance_accuracy_verdict(true, 1.133, 0.0618, None, 18.335, highlights)
        };
        let s5ii = |highlights| {
            white_balance_accuracy_verdict(
                false,
                12.0,
                0.5134,
                Some((11.142, 0.4758)),
                23.42,
                highlights,
            )
        };
        for verdict in [om1(&clipped).unwrap(), s5ii(&clipped).unwrap()] {
            assert_eq!(verdict["exception"], HIGHLIGHT_CLIP_EXCEPTION);
            assert_eq!(verdict["held"], false);
            assert_eq!(verdict["clipped_share"], 0.0407);
            assert!(
                verdict["failed_limit"]
                    .as_str()
                    .unwrap()
                    .contains("the limit is")
            );
        }
        assert_eq!(om1(&clipped).unwrap()["checked"], "moving");
        assert_eq!(s5ii(&clipped).unwrap()["checked"], "held_full_detail");
        assert!(
            om1(&unclipped)
                .unwrap_err()
                .to_string()
                .contains("the limit is 1 code")
        );
        assert!(
            s5ii(&unclipped)
                .unwrap_err()
                .to_string()
                .contains("held full-detail")
        );
        let within = white_balance_accuracy_verdict(true, 0.5, 0.05, None, 10.0, &clipped).unwrap();
        assert_eq!(within["held"], true);
        assert!(within["exception"].is_null());
        assert!(white_balance_accuracy_verdict(false, 0.5, 0.05, None, 10.0, &clipped).is_err());
    }

    #[test]
    fn hundred_percent_accuracy_uses_held_full_detail_after_refinement() {
        // The Air 2S review's 17.33% moving difference is still evidence, while its 5.06%
        // held difference passes the accepted white-balance comparison.
        assert_eq!(
            check_white_balance_accuracy(false, 4.2, 0.1733, Some((1.227, 0.0506)), 24.242)
                .unwrap(),
            "held_full_detail"
        );
        let failure = check_white_balance_accuracy(false, 0.5, 0.02, Some((2.5, 0.1001)), 24.242)
            .unwrap_err();
        assert!(failure.to_string().contains("held full-detail"));
        assert!(check_white_balance_accuracy(false, 0.5, 0.02, None, 24.242).is_err());
    }

    #[test]
    fn fit_accuracy_requires_moving_share_and_one_code_mean() {
        assert_eq!(
            check_white_balance_accuracy(true, 1.0, 0.1, None, 10.0).unwrap(),
            "moving"
        );
        assert!(check_white_balance_accuracy(true, 0.9, 0.1001, None, 8.99).is_err());
        assert!(check_white_balance_accuracy(true, 1.001, 0.08, None, 12.5).is_err());
    }

    /// What the plan scripts at the named step.
    fn scripted(plan: &Plan, step: &str) -> Value {
        let at = plan
            .index(step)
            .unwrap_or_else(|| panic!("{step:?} is not planned"));
        plan.steps()[at]
            .script()
            .unwrap_or_else(|| panic!("{step:?} scripts nothing"))
    }

    /// Each drag stops on a declared temperature and releases that value; the 100% drag first
    /// pauses while still held so the verifier can check full-detail white-balance accuracy.
    #[test]
    fn each_drag_is_a_declared_temperature_on_its_step_where_the_checks_look() {
        let registry = luxforge_core::ModuleRegistry::builtin();
        let (_, action) = registry.action(SET_RAW).expect("a declared action");
        let parameter = action.parameter(TEMPERATURE).expect("a declared field");
        assert_eq!(parameter.unit.as_deref(), Some("K"));
        let luxforge_core::ParameterKind::Number { min, max } = parameter.kind else {
            panic!("temperature is a number");
        };
        let step = parameter.step.unwrap_or(1.0);
        let plan = plan(&[]);
        for drag in &DRAGS {
            assert!((min..=max).contains(&drag.kelvin));
            assert_eq!((drag.kelvin / step).round() * step, drag.kelvin);
            let at = plan.index(drag.drag).expect("a planned drag");
            assert_eq!(
                plan.index(drag.release),
                Some(at + 1 + usize::from(drag.quiet.is_some())),
                "{}",
                drag.release
            );
            if let Some(quiet) = drag.quiet {
                assert_eq!(plan.index(quiet), Some(at + 1));
                assert_eq!(scripted(&plan, quiet), json!({"wait":{"ms":1000}}));
            }
            let open = &scripted(&plan, drag.drag)["slider"];
            let release = &scripted(&plan, drag.release)["slider"];
            assert_eq!(open["values"], json!([drag.kelvin]));
            assert_eq!(open["release"], json!(false));
            assert_eq!(release["values"], json!([drag.kelvin]));
            assert_eq!(release["release"], json!(true));
            if !drag.fit {
                assert_eq!(
                    plan.steps()[at - 1].script(),
                    Some(script::Step::View(ViewStep::Percent(100.0)).to_value())
                );
                assert_eq!(
                    plan.steps()[at + 3].script(),
                    Some(script::Step::View(ViewStep::Fit).to_value())
                );
            }
        }
    }

    /// One frame for the open and one per script step, each step named for what it scripts and
    /// every frame held to Basic's section expanded; and the table's row runs this plan, outside
    /// `rendered`. No RAW run can be replayed, so this is what ties the names the checks read to
    /// the steps they mean.
    #[test]
    fn the_plan_is_the_open_and_one_frame_per_step_each_named_for_what_it_scripts() {
        let raw = [PathBuf::from("/raw/photo.nef")];
        let plan = plan(&raw);
        assert!(plan.validate().is_ok(), "{:?}", plan.validate());
        let script = plan.script();
        assert_eq!(
            script.as_array().map(|steps| steps.len() + 1),
            Some(plan.len())
        );
        let (open, steps) = plan.steps().split_first().expect("a planned open");
        assert_eq!(open.name(), names::OPENED);
        assert!(open.script().is_none() && steps.iter().all(|step| step.script().is_some()));
        assert!(plan.steps().iter().all(|step| {
            step.expect()
                .expanded
                .contains(&(BASIC_MODULE.to_owned(), true))
        }));
        assert_eq!(
            scripted(&plan, names::SENSOR_PICK),
            script::Step::key("w").to_value()
        );
        assert_eq!(
            scripted(&plan, names::PICK_LEFT),
            script::Step::key(script::KEY_ESCAPE).to_value()
        );
        for click in &DOUBLE_CLICKS {
            assert_eq!(
                scripted(&plan, click.step),
                script::Step::DoubleClick(DoubleClickStep {
                    action: click.action.into(),
                    parameter: click.parameter.into(),
                    value: click.value,
                    gap_ms: GAP_MS
                })
                .to_value()
            );
        }
        for (step, request) in [
            (names::CROP_STARTED, script::Step::Draft(DraftStep::Start)),
            (
                names::CROP_STRAIGHTENED,
                script::Step::Draft(DraftStep::Angle(CROP_ANGLE)),
            ),
            (names::CROP_APPLIED, script::Step::Draft(DraftStep::Apply)),
            (
                names::CROP_AT_100,
                script::Step::View(ViewStep::Percent(100.0)),
            ),
            (SAMPLES[0].0, sample(SAMPLES[0].1)),
            (SAMPLES[1].0, sample(SAMPLES[1].1)),
            (
                names::CROP_FITTED,
                script::Step::call("edit.crop-fit", json!({"aspect":"3:2","angle":FIT_ANGLE})),
            ),
            (names::FITTED_SAMPLE, sample(SAMPLES[1].1)),
            (names::FITTED_AT_FIT, script::Step::View(ViewStep::Fit)),
        ] {
            assert_eq!(scripted(&plan, step), request.to_value(), "{step}");
        }
        let row = crate::smoke::find(SCENARIO).unwrap();
        assert_eq!((row.launches[0].plan)(&raw).script(), script);
        assert!(!row.rendered());
    }

    /// The reset a number control declares for its own field — a control of the module, or a
    /// variant another module's control carries — found the way `module.list` lists it.
    fn declared_reset(
        controls: &[luxforge_core::Control],
        action: &str,
        parameter: &str,
    ) -> Option<Option<luxforge_core::ResetAction>> {
        controls.iter().find_map(|control| {
            match control {
                luxforge_core::Control::Group(luxforge_core::GroupControl { controls, .. }) => {
                    declared_reset(controls, action, parameter)
                }
                luxforge_core::Control::Number(luxforge_core::NumberControl {
                    action: declared,
                    parameter: named,
                    reset,
                    ..
                }) if declared == action && named == parameter => Some(reset.clone()),
                _ => None,
            }
            .or_else(|| {
                let variants: Vec<luxforge_core::Control> = control
                    .variants()
                    .iter()
                    .filter_map(|variant| variant.control.as_deref().cloned())
                    .collect();
                declared_reset(&variants, action, parameter)
            })
        })
    }

    /// Every double-click names a declared slider field whose one value is a whole request and
    /// lands its first press inside the declared range and off the default. It expects the reset
    /// the control declares — As shot for the RAW variants of Temperature and Tint — or, for a
    /// control that declares none, its own action and the declared default back, formatted as the
    /// field shows it.
    #[test]
    fn every_double_click_is_a_declared_drafting_field_and_its_declared_reset() {
        let registry = luxforge_core::ModuleRegistry::builtin();
        let every: Vec<luxforge_core::Control> = registry
            .descriptors()
            .into_iter()
            .flat_map(|module| module.controls.clone())
            .collect();
        for click in &DOUBLE_CLICKS {
            let (_, action) = registry.action(click.action).expect("a declared action");
            let parameter = action.parameter(click.parameter).expect("a declared field");
            assert!(
                action.patch || action.parameters.len() == 1,
                "{}",
                click.action
            );
            let luxforge_core::ParameterKind::Number { min, max } = parameter.kind else {
                panic!("{} is a number", click.parameter);
            };
            assert!((min..=max).contains(&click.value));
            let default = parameter.default.as_ref().and_then(Value::as_f64).unwrap();
            assert_ne!(default, click.value, "the first press moves the value");
            let reset = declared_reset(&every, click.action, click.parameter)
                .expect("a number control of the field");
            match reset {
                Some(reset) => {
                    assert_eq!(reset.action, click.reset, "{}", click.action);
                    assert_eq!(
                        Value::Object(reset.preset),
                        json!({"white-balance": "as-shot"})
                    );
                    assert_eq!(click.shows, None);
                }
                None => {
                    assert_eq!(click.reset, click.action);
                    let decimals = usize::from(parameter.precision.unwrap_or(0));
                    assert_eq!(Some(format!("{default:.decimals$}").as_str()), click.shows);
                }
            }
        }
    }

    // The gap stays inside the window iced gives a double-click's two presses.
    const _: () = assert!(GAP_MS <= 250);

    const STAGE: [u32; 2] = [1200, 800];
    const FIT: [u32; 4] = [20, 20, 980, 680];
    const BACKGROUND: [u8; 3] = crate::scenario::pixels::CANVAS;

    fn stage(angle: f64) -> luxforge_core::CropStage {
        luxforge_core::CropStage {
            width: STAGE[0],
            height: STAGE[1],
            angle,
        }
    }

    /// The unstraightened draft on the whole stage, and a 7° draft with an inner crop rectangle,
    /// so both opacities are drawn.
    fn views() -> (DraftView, DraftView) {
        let turned = stage(7.0);
        let (width, height) = turned.bounding_box();
        (
            DraftView::new(stage(0.0), FIT, [0.0, 0.0, 1200.0, 800.0]),
            DraftView::new(turned, FIT, [150.0, 150.0, width - 300.0, height - 300.0]),
        )
    }

    /// A chart on a ground exactly the canvas's colour at the right, and dark specks within a code
    /// of it scattered over a lit gradient elsewhere: canvas-coloured scene content, lots of it.
    fn dark_scene(u: f64, v: f64) -> [u8; 3] {
        let [width, height] = STAGE.map(f64::from);
        if u > 0.55 * width && v > 0.35 * height {
            let (column, row) = ((u / 60.0) as u32, (v / 60.0) as u32);
            return if (column + row) % 3 == 0 {
                [200, 60, 90]
            } else {
                BACKGROUND
            };
        }
        let (column, row) = ((u / 8.0) as u64, (v / 8.0) as u64);
        if (column * 7919 + row * 104_729) % 29 == 0 {
            return [26, 25, 27];
        }
        [
            70 + (130.0 * u / width) as u8,
            90 + (90.0 * v / height) as u8,
            150,
        ]
    }

    /// A draft drawn as the photo surface draws it: the stage, turned, at full opacity inside the
    /// crop rectangle and at the dim opacity over the canvas outside it, and the canvas elsewhere.
    fn render(view: &DraftView, scene: impl Fn(f64, f64) -> [u8; 3]) -> image::RgbImage {
        image::RgbImage::from_fn(1000, 700, |x, y| {
            let (sx, sy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let (u, v) = view.stage.to_input(
                (sx - view.origin.0) / view.zoom,
                (sy - view.origin.1) / view.zoom,
            );
            if !(0.0..f64::from(STAGE[0])).contains(&u) || !(0.0..f64::from(STAGE[1])).contains(&v)
            {
                return image::Rgb(BACKGROUND);
            }
            let [left, top, right, bottom] = view.bright;
            let opacity = if (left..right).contains(&sx) && (top..bottom).contains(&sy) {
                1.0
            } else {
                DRAFT_DIM_OPACITY
            };
            let pixel = scene(u, v);
            image::Rgb(std::array::from_fn(|channel| {
                let (drawn, under) = (f64::from(pixel[channel]), f64::from(BACKGROUND[channel]));
                (under + opacity * (drawn - under)).round() as u8
            }))
        })
    }

    fn scan(straightened: &image::RgbImage, reference: &image::RgbImage) -> Wholeness {
        let (unturned, turned) = views();
        draft_gaps(
            (straightened, &turned),
            (reference, &unturned),
            BACKGROUND,
            (0.0, 700.0),
            STAGE,
        )
    }

    /// Paint the canvas over a region of a draft, where the picture does not cover it.
    fn expose(image: &mut image::RgbImage, inside: impl Fn(f64, f64) -> bool) {
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            if inside(f64::from(x), f64::from(y)) {
                *pixel = image::Rgb(BACKGROUND);
            }
        }
    }

    /// Dark content the canvas's colour, more of it than the 1% of the grid a canvas-colour count
    /// allowed, is never taken for a gap: the unstraightened draft shows the same dark scene there.
    #[test]
    fn a_straightened_draft_with_canvas_coloured_content_is_whole() {
        let (unturned, turned) = views();
        let found = scan(&render(&turned, dark_scene), &render(&unturned, dark_scene));
        assert!(
            f64::from(found.canvas_coloured) > 0.1 * f64::from(found.sampled),
            "{found:?}"
        );
        assert_eq!(found.gaps, 0, "{found:?}");
        whole(&found).unwrap();
    }

    /// A wedge of canvas between two fragments, and a hole of a few grid steps, each fail.
    #[test]
    fn a_straightened_draft_with_a_canvas_wedge_or_hole_is_not_whole() {
        let (unturned, turned) = views();
        let reference = render(&unturned, dark_scene);
        let straightened = render(&turned, dark_scene);

        let mut wedge = straightened.clone();
        // From a point at the top of the Fit area to 60 px wide at its bottom.
        expose(&mut wedge, |x, y| {
            (x - 500.0).abs() * 660.0 <= 30.0 * (y - 20.0)
        });
        let found = scan(&wedge, &reference);
        let failure = whole(&found).unwrap_err().to_string();
        assert!(failure.contains("not drawn as one picture"), "{failure}");

        let mut hole = straightened;
        // 64 px square over the lit gradient, a quarter of the way into the stage.
        let (x, y) = turned.screen(300.0, 200.0);
        expose(&mut hole, |px, py| {
            (px - x).abs() <= 32.0 && (py - y).abs() <= 32.0
        });
        let found = scan(&hole, &reference);
        assert!(found.gaps >= 25, "{found:?}");
        let failure = whole(&found).unwrap_err().to_string();
        assert!(failure.contains("not drawn as one picture"), "{failure}");
    }

    /// A scene all the canvas's colour leaves nothing to judge, which fails rather than passing.
    #[test]
    fn a_straightened_draft_too_dark_to_judge_fails() {
        let (unturned, turned) = views();
        let dark = |_: f64, _: f64| BACKGROUND;
        let found = scan(&render(&turned, dark), &render(&unturned, dark));
        assert_eq!(found.judged, 0);
        let failure = whole(&found).unwrap_err().to_string();
        assert!(failure.contains("too few to judge"), "{failure}");
    }

    /// The recorded runs under `STRAIGHTENED_RECORDED` (each `DIR/raw-panel*`, a `verify`
    /// component's or a `smoke --output` directory), their straightened draft judged against their
    /// unstraightened one and printed, whatever an earlier check of the run found; any failure is
    /// listed.
    #[test]
    #[ignore = "needs recorded raw-panel runs"]
    fn straightened_drafts_recorded() {
        let root = PathBuf::from(std::env::var("STRAIGHTENED_RECORDED").expect("a directory"));
        let plan = plan(&[]);
        let frame = |app: &Path, step: &str| -> Frame {
            let number = plan.index(step).expect("a planned step") + 1;
            let record = std::fs::read_to_string(app.join(format!("state-{number}.json")))
                .expect("a frame record");
            Frame::unchecked(
                serde_json::from_str(&record).expect("a frame record"),
                &app.join(format!("frame-{number}.png")),
            )
        };
        let mut runs = std::fs::read_dir(&root)
            .expect("a directory")
            .map(|entry| entry.unwrap().path())
            .filter(|run| {
                run.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(SCENARIO)
            })
            // A `verify` component's run is under `run/`; a `smoke --output` directory is the run.
            .flat_map(|run| [run.join("run/app"), run.join("app")])
            .filter(|app| app.join("state-1.json").is_file())
            .collect::<Vec<_>>();
        runs.sort();
        assert!(!runs.is_empty(), "no run under {}", root.display());
        let mut failures = Vec::new();
        for app in runs {
            let started = frame(&app, names::CROP_STARTED);
            let source: [u32; 2] =
                serde_json::from_value(started["state"]["crop"]["input_stage"].clone()).unwrap();
            let verdict = draft_is_whole(&frame(&app, names::CROP_STRAIGHTENED), &started, source);
            println!("{}: {verdict:?}", app.display());
            if let Err(error) = verdict {
                failures.push(format!("{}: {error}", app.display()));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
