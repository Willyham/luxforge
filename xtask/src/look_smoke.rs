//! The `look` smoke scenario: the RAW look (`docs/design/raw-looks.md`) on a real RAW photo in the
//! real editor. A new RAW photograph's Original holds the Standard look at amount 100, which
//! `recipe.describe` reports as neutral; the Look section is listed directly above Basic's with
//! Standard chosen and Amount at 100, and its band carries no edited dot.
//!
//! Then, through the section: Neutral, one entry, whose picture is darker and flatter than the
//! Standard one in the photograph's central region; Standard again, which returns the photograph to
//! the opened picture, and which auto-collapse history, on by default, records as a return to the
//! Original rather than an entry; an Amount drag held at 60, drafted and never committed, whose
//! drafted picture's mean luminance lies between Neutral's and Standard's, released as one entry;
//! the module reset back to Standard at 100; and a double-click on Amount, a committed jump and its
//! reset to 100, which collapses back to the reset's entry. Through the API, `edit.set-look
//! {look: neutral}`, which the section follows. Settings › General shows the starting look for new
//! RAW photos at Standard. Every frame is correlated with the photograph's state, the draft and the
//! step's own events.
//!
//! No RAW photograph is checked in (see `fixtures/README.md`), so this scenario is outside the
//! rendered tier of [`crate::smoke::SCENARIOS`] and takes its source from `--source`. It proves
//! what the look does to a real photograph and that the section and the API edit it alike, not the
//! look's numbers, which its frozen reference fixture holds.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, pixels, plan::only},
    *,
};
use luxforge_core::LOOK_EFFECT;
use luxforge_evidence::{self as script, ControlsStep, DoubleClickStep, SliderStep};

pub const SCENARIO: &str = "look";
const LOOK_MODULE: &str = "luxforge.look";
const BASIC_MODULE: &str = "luxforge.basic";
const SET_LOOK: &str = "set-look";
const LOOK: &str = "look";
const AMOUNT: &str = "amount";
const STANDARD: &str = "standard";
const NEUTRAL: &str = "neutral";
/// The preference the General tab's Starting look for new RAW photos row edits.
const RAW_LOOK_PREFERENCE: &str = "raw_look";

/// The steps the checks read by name. The plan and the checks share each name, so a misspelt one
/// does not build: no RAW is checked in, so no recorded run would catch it.
mod names {
    pub const OPENED: &str = "opened";
    pub const OPENED_SETTLED: &str = "opened-settled";
    pub const DESCRIBE_OPENED: &str = "describe-opened";
    pub const NEUTRAL: &str = "neutral";
    pub const NEUTRAL_SETTLED: &str = "neutral-settled";
    pub const STANDARD: &str = "standard";
    pub const STANDARD_SETTLED: &str = "standard-settled";
    pub const AMOUNT_FIRST: &str = "amount-first";
    pub const AMOUNT_HELD: &str = "amount-held";
    pub const AMOUNT_DRAG: &str = "amount-drag";
    pub const AMOUNT_RELEASE: &str = "amount-release";
    pub const AMOUNT_SETTLED: &str = "amount-settled";
    pub const DESCRIBE_AMOUNT: &str = "describe-amount";
    pub const RESET: &str = "reset";
    pub const DOUBLE_CLICK: &str = "amount-double-click";
    pub const API_NEUTRAL: &str = "api-neutral";
    pub const DESCRIBE_NEUTRAL: &str = "describe-neutral";
    pub const SETTINGS: &str = "settings-general";
    pub const SETTINGS_CLOSED: &str = "settings-closed";
}

/// The Amount drag's first tick, the tick its drafted frame shows and releases, and where the
/// double-click's first press lands. All inside 0..=200 on the step of 1, and off the default 100.
const FIRST_AMOUNT: f64 = 70.0;
const DRAG_AMOUNT: f64 = 60.0;
const JUMP_AMOUNT: f64 = 150.0;
/// The Amount's declared default, which its double-click resets it to and the Original holds.
const FULL_AMOUNT: f64 = 100.0;
/// Between a double-click's release and its second press: a person's ordinary double-click, well
/// inside the 300 ms iced gives the two presses.
const GAP_MS: u64 = 120;
/// The quiet a wait step leaves the editor, and the most a step waits for the GPU's compiles, as
/// the GPU preview scenarios wait.
const QUIET_MS: u64 = 1500;
const WARM_MS: u64 = 60_000;

/// Whether choosing Neutral lights the Look band's edited dot. The design and the task plan say the
/// dot is lit whenever the look is not the current Standard at amount 100, so Neutral lights it.
const NEUTRAL_LIGHTS_THE_DOT: bool = true;

/// How much darker, in mean luma codes, the Neutral picture's central region must be than the
/// Standard one's: the Standard look lifts mid-grey by more than a stop.
const DARKER_BY: f64 = 2.0;
/// How much smaller the Neutral picture's 10–90 percentile luma spread must be, in codes.
const FLATTER_BY: f64 = 1.0;
/// How far the Standard picture chosen again may be from the opened one: the mean absolute channel
/// difference over the photograph, in codes, and the share of its pixels more than two codes off.
const SAME_MEAN_CODES: f64 = 1.0;
const SAME_SHARE_OVER_2: f64 = 0.01;

/// The current Standard look at `amount`, as a new RAW photograph's Original stores it: the look
/// module's own answer to the Original hook, at the amount asked.
fn standard_payload(amount: f64) -> Value {
    let registry = luxforge_core::ModuleRegistry::builtin();
    let mut payload = registry
        .module(LOOK_MODULE)
        .expect("the look module is registered")
        .original(&luxforge_core::OriginalContext {
            source: luxforge_core::SourceTag::Raw,
            raw: None,
            header: &luxforge_core::catalog_types::HeaderMetadata::default(),
            preferences: luxforge_core::OriginalPreferences::default(),
        })
        .expect("the look module answers the Original hook")
        .expect("a RAW photograph starts from the Standard look")
        .payload;
    payload[AMOUNT] = json!(amount);
    payload
}

fn neutral_payload() -> Value {
    json!({ LOOK: NEUTRAL })
}

/// The answer `recipe.describe` gives, which the step records.
fn describe() -> script::Step {
    script::Step::call("recipe.describe", json!({}))
}

/// A step that waits until the GPU preview has compiled what it was handed and the editor has been
/// quiet, so its frame is the picture at rest.
fn warmed(name: &str) -> Step {
    Step::new(name, script::Step::gpu_warmed(QUIET_MS, WARM_MS)).commits(0)
}

fn choose(look: &str) -> script::Step {
    script::Step::Controls(ControlsStep::Discrete {
        action: SET_LOOK.into(),
        parameter: LOOK.into(),
        value: json!(look),
    })
}

/// Every frame, in order: the open, then one per step, each with what it commits and shows. The
/// Look and Basic sections are expanded in every frame but the Settings sheet's own.
pub fn plan(_: &[PathBuf]) -> Plan {
    let standard_at = |amount: f64| standard_payload(amount);
    let steps = vec![
        Step::opened(names::OPENED)
            .no_draft()
            .payload(LOOK_EFFECT, standard_at(FULL_AMOUNT))
            .field(SET_LOOK, LOOK, STANDARD)
            .field(SET_LOOK, AMOUNT, "100"),
        warmed(names::OPENED_SETTLED),
        Step::new(names::DESCRIBE_OPENED, describe()).commits(0),
        // The section's Neutral: one entry.
        Step::new(names::NEUTRAL, choose(NEUTRAL))
            .commits(1)
            .no_draft()
            .label("Look Neutral")
            .payload(LOOK_EFFECT, neutral_payload())
            .field(SET_LOOK, LOOK, NEUTRAL),
        warmed(names::NEUTRAL_SETTLED),
        // The section's Standard: the same action and field set as the Neutral entry, ending where
        // the Original began, so auto-collapse writes no entry and moves the head back to the
        // Original, which `verify` reads.
        Step::new(names::STANDARD, choose(STANDARD))
            .commits(1)
            .no_draft()
            .payload(LOOK_EFFECT, standard_at(FULL_AMOUNT))
            .field(SET_LOOK, LOOK, STANDARD)
            .field(SET_LOOK, AMOUNT, "100"),
        warmed(names::STANDARD_SETTLED),
        // An Amount drag left open: its first tick, held until the GPU has compiled what it asked
        // for, then the tick whose drafted frame is measured.
        Step::new(
            names::AMOUNT_FIRST,
            SliderStep::new(SET_LOOK, AMOUNT, [FIRST_AMOUNT]),
        )
        .commits(0)
        .draft(SET_LOOK, json!({ AMOUNT: FIRST_AMOUNT })),
        warmed(names::AMOUNT_HELD).draft(SET_LOOK, json!({ AMOUNT: FIRST_AMOUNT })),
        Step::new(
            names::AMOUNT_DRAG,
            SliderStep::new(SET_LOOK, AMOUNT, [DRAG_AMOUNT]),
        )
        .commits(0)
        .draft(SET_LOOK, json!({ AMOUNT: DRAG_AMOUNT }))
        .payload(LOOK_EFFECT, standard_at(FULL_AMOUNT)),
        // Released at the same value: one entry.
        Step::new(
            names::AMOUNT_RELEASE,
            SliderStep::new(SET_LOOK, AMOUNT, [DRAG_AMOUNT]).release(),
        )
        .commits(1)
        .no_draft()
        .label("Look amount 60")
        .payload(LOOK_EFFECT, standard_at(DRAG_AMOUNT))
        .field(SET_LOOK, AMOUNT, "60")
        .same_layer(LOOK_EFFECT, names::OPENED),
        warmed(names::AMOUNT_SETTLED).no_draft(),
        Step::new(names::DESCRIBE_AMOUNT, describe()).commits(0),
        // The module reset on the Look band: back to Standard at 100, the layer kept.
        Step::new(names::RESET, script::Step::reset(LOOK_MODULE, None))
            .commits(1)
            .label("Reset Look")
            .payload(LOOK_EFFECT, standard_at(FULL_AMOUNT))
            .field(SET_LOOK, AMOUNT, "100")
            .same_layer(LOOK_EFFECT, names::OPENED),
        // A double-click on Amount's rail: the first press commits a jump, the second resets the
        // field to its declared default, 100, where the run began, so auto-collapse leaves the
        // reset's entry current.
        Step::new(
            names::DOUBLE_CLICK,
            DoubleClickStep {
                action: SET_LOOK.into(),
                parameter: AMOUNT.into(),
                value: JUMP_AMOUNT,
                gap_ms: GAP_MS,
            },
        )
        .commits_collapsing_back(2)
        .no_draft()
        .label("Reset Look")
        .payload(LOOK_EFFECT, standard_at(FULL_AMOUNT))
        .field(SET_LOOK, AMOUNT, "100"),
        // The same choice through the API's command service: one entry, which the section follows.
        Step::new(
            names::API_NEUTRAL,
            script::Step::call("edit.set-look", json!({ LOOK: NEUTRAL })),
        )
        .commits(1)
        .no_draft()
        .label("Look Neutral")
        .payload(LOOK_EFFECT, neutral_payload())
        .field(SET_LOOK, LOOK, NEUTRAL)
        .same_layer(LOOK_EFFECT, names::OPENED),
        Step::new(names::DESCRIBE_NEUTRAL, describe()).commits(0),
    ];
    let mut steps: Vec<Step> = steps
        .into_iter()
        .map(|step| step.expanded(LOOK_MODULE).expanded(BASIC_MODULE))
        .collect();
    steps.push(Step::new(names::SETTINGS, script::Step::settings_tab("general")).commits(0));
    steps.push(
        Step::new(
            names::SETTINGS_CLOSED,
            script::Step::key(script::KEY_ESCAPE),
        )
        .commits(0)
        .expanded(LOOK_MODULE)
        .expanded(BASIC_MODULE),
    );
    Plan::new(steps)
}

/// The sections the frame's tools panel lists, in the order it draws them: the registered modules
/// in the order the editor lists them, which is the order the panel derives its sections in, less
/// those it draws no section for (every section it draws is in `expanded`).
fn panel_order(frame: &Frame) -> Result<Vec<String>> {
    let state = frame.state();
    let sections = state["expanded"]
        .as_object()
        .ok_or("The frame records no sections")?;
    Ok(state["modules"]
        .as_array()
        .ok_or("The frame lists no modules")?
        .iter()
        .filter_map(|module| module["id"].as_str())
        .filter(|id| sections.contains_key(*id))
        .map(str::to_owned)
        .collect())
}

/// The Look section is listed directly above Basic's, and its Amount is the descriptor's slider.
fn look_above_basic(frame: &Frame, step: &str) -> Result<Vec<String>> {
    let order = panel_order(frame)?;
    let look = order.iter().position(|id| id == LOOK_MODULE);
    let basic = order.iter().position(|id| id == BASIC_MODULE);
    ensure(
        look.is_some() && basic == look.map(|at| at + 1),
        format!("{step}: the Look section is not directly above Basic's: {order:?}"),
    )?;
    let controls = &frame.state()["section_controls"][LOOK_MODULE];
    ensure(
        controls
            == &json!([{"kind": "number", "label": "Amount", "action": SET_LOOK,
                        "parameter": AMOUNT, "unit": null}]),
        format!("{step}: the Look section's sliders are {controls}"),
    )?;
    Ok(order)
}

/// Whether the Look band carries its edited dot in this frame.
fn look_dot(frame: &Frame) -> Result<bool> {
    frame.state()["active"][LOOK_MODULE]
        .as_bool()
        .ok_or_else(|| "The frame records no Look band".into())
}

/// The look row `recipe.describe` answered at the named step.
fn described_look<'a>(launch: &'a Checked, step: &str) -> Result<&'a Value> {
    let answer = &launch.at(step)?["step"]["result"];
    answer["layers"]
        .as_array()
        .and_then(|layers| layers.iter().find(|layer| layer["effect"] == LOOK_EFFECT))
        .ok_or_else(|| format!("{step}: recipe.describe answered no look row: {answer}").into())
}

/// The step's events that would mean a commit's picture never reached the canvas: a refused
/// request, a failed render or a picture withdrawn for a failure.
fn failures<'a>(launch: &'a Checked, step: &str) -> Result<Vec<&'a Value>> {
    Ok(crate::gpu_preview_smoke::step_events(launch, step)?
        .iter()
        .filter(|event| {
            ["command_failed", "render_failed", "preview_withdrawn"]
                .contains(&event["event"].as_str().unwrap_or_default())
        })
        .collect())
}

/// The luma of every pixel in the central half of the photograph as the frame draws it on screen,
/// each side a quarter in from the visible photograph's edge.
fn central_luma(frame: &Frame) -> Result<Vec<f64>> {
    let [left, top, right, bottom] = frame.visible_photo()?;
    let (width, height) = (right - left, bottom - top);
    let image = frame.image()?;
    let mut luma = Vec::new();
    for y in top + height / 4..bottom - height / 4 {
        for x in left + width / 4..right - width / 4 {
            luma.push(pixels::luminance(image.get_pixel(x, y).0));
        }
    }
    ensure(
        !luma.is_empty(),
        format!("{} draws no photograph", frame["file"]),
    )?;
    Ok(luma)
}

/// The central region's mean luma, its 10th and 90th percentile (nearest rank) and their spread,
/// in 8-bit codes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tone {
    mean: f64,
    p10: f64,
    p90: f64,
}

impl Tone {
    fn of(luma: &mut [f64]) -> Self {
        luma.sort_by(f64::total_cmp);
        let rank = |p: f64| {
            let index = ((p * luma.len() as f64).ceil() as usize).clamp(1, luma.len()) - 1;
            luma[index]
        };
        Self {
            mean: luma.iter().sum::<f64>() / luma.len() as f64,
            p10: rank(0.10),
            p90: rank(0.90),
        }
    }

    fn spread(&self) -> f64 {
        self.p90 - self.p10
    }

    fn record(&self) -> Value {
        json!({"mean": self.mean, "p10": self.p10, "p90": self.p90, "spread": self.spread()})
    }
}

fn tone(frame: &Frame) -> Result<Tone> {
    Ok(Tone::of(&mut central_luma(frame)?))
}

/// How far one capture's photograph is from another's: the mean absolute channel difference over
/// the photograph as both draw it, and the share of its pixels more than two codes off.
fn difference(first: &Frame, second: &Frame) -> Result<(f64, f64)> {
    let rect = first.visible_photo()?;
    ensure(
        second.visible_photo()? == rect,
        format!(
            "{} and {} show different rectangles",
            first["file"], second["file"]
        ),
    )?;
    let (a, b) = (first.image()?, second.image()?);
    let [left, top, right, bottom] = rect;
    let (mut total, mut over, mut count) = (0_u64, 0_u64, 0_u64);
    for y in top..bottom {
        for x in left..right {
            let (p, q) = (a.get_pixel(x, y).0, b.get_pixel(x, y).0);
            let differences = [0, 1, 2].map(|c| p[c].abs_diff(q[c]));
            total += differences.iter().map(|d| u64::from(*d)).sum::<u64>();
            over += u64::from(differences.iter().any(|d| *d > 2));
            count += 1;
        }
    }
    ensure(count > 0, "No photograph to compare")?;
    Ok((
        total as f64 / (3 * count) as f64,
        over as f64 / count as f64,
    ))
}

/// What each frame shows beyond its plan, once the plan has held.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    // Every frame: ready, the photograph on screen the current entry's or its draft's, the Look
    // section directly above Basic's, nothing refused or failed, and the Look band's dot what
    // the core's description of the current stack says.
    for (step, frame) in launch.names().iter().zip(&launch.frames) {
        let state = frame.state();
        ensure(
            state["phase"] == "ready",
            format!("Look step {step:?} is not ready: {}", state["phase"]),
        )?;
        look_above_basic(frame, step)?;
        if step != names::OPENED {
            let failed = failures(launch, step)?;
            ensure(
                failed.is_empty(),
                format!("Look step {step:?} logged a failure: {failed:?}"),
            )?;
        }
        if frame.draft().is_null() {
            let displayed = &state["stack"]["displayed"];
            ensure(
                displayed["entry"] == state["stack"]["entry"],
                format!(
                    "Look step {step:?} shows entry {} while history's current entry is {}",
                    displayed["entry"], state["stack"]["entry"]
                ),
            )?;
        } else {
            frame.displays_draft()?;
        }
    }
    let opened = launch.at(names::OPENED)?;
    checks.note(
        opened,
        "the Look section directly above Basic's",
        json!({"panel": panel_order(opened)?}),
    );

    // The Original holds the Standard look, which the core reports as neutral: no edited dot.
    let described = described_look(launch, names::DESCRIBE_OPENED)?;
    ensure(
        described["values"][LOOK] == STANDARD
            && described["values"][AMOUNT] == json!(FULL_AMOUNT)
            && described["neutral"] == json!(true),
        format!("The Original's look row is {described}"),
    )?;
    let original = launch.at(names::DESCRIBE_OPENED)?;
    ensure(
        original["step"]["result"]["entry_id"] == original.state()["stack"]["entry"],
        "recipe.describe described another entry than the one on screen",
    )?;
    checks.note(
        original,
        "recipe.describe of the Original",
        json!({"look": described, "label": opened.label()?}),
    );

    // Standard again returns to the Original: auto-collapse writes no entry for the run of two
    // choices that ends where it began.
    let again = launch.at(names::STANDARD)?;
    ensure(
        again.entry()? == opened.entry()? && again.label()? == opened.label()?,
        format!(
            "Standard chosen again left entry {} ({:?}), not the Original {} ({:?})",
            again.entry()?,
            again.label()?,
            opened.entry()?,
            opened.label()?
        ),
    )?;

    // The pictures: Neutral darker and flatter than Standard; Standard again the opened picture;
    // the drafted Amount between them.
    let (standard_frame, neutral_frame, again_frame, drafted, released) = (
        launch.at(names::OPENED_SETTLED)?,
        launch.at(names::NEUTRAL_SETTLED)?,
        launch.at(names::STANDARD_SETTLED)?,
        launch.at(names::AMOUNT_DRAG)?,
        launch.at(names::AMOUNT_SETTLED)?,
    );
    let (standard, neutral, drafted_tone, released_tone) = (
        tone(standard_frame)?,
        tone(neutral_frame)?,
        tone(drafted)?,
        tone(released)?,
    );
    checks.compare(
        neutral_frame,
        "the Standard picture's central mean luma above the Neutral one's",
        standard.mean,
        neutral.mean,
        Tolerance::Above(DARKER_BY),
    )?;
    checks.compare(
        neutral_frame,
        "the Standard picture's central 10-90 percentile luma spread above the Neutral one's",
        standard.spread(),
        neutral.spread(),
        Tolerance::Above(FLATTER_BY),
    )?;
    let (mean, share) = difference(standard_frame, again_frame)?;
    checks.compare(
        again_frame,
        "Standard chosen again against the opened picture: mean absolute codes",
        mean,
        0.0,
        Tolerance::Within(SAME_MEAN_CODES),
    )?;
    checks.compare(
        again_frame,
        "Standard chosen again against the opened picture: share of pixels over 2 codes",
        share,
        0.0,
        Tolerance::Within(SAME_SHARE_OVER_2),
    )?;
    checks.compare(
        drafted,
        "the Standard picture's central mean luma above the drafted Amount 60's",
        standard.mean,
        drafted_tone.mean,
        Tolerance::Above(0.0),
    )?;
    checks.compare(
        drafted,
        "the drafted Amount 60's central mean luma above the Neutral picture's",
        drafted_tone.mean,
        neutral.mean,
        Tolerance::Above(0.0),
    )?;
    checks.compare(
        released,
        "the committed Amount 60's central mean luma between: above the Neutral picture's",
        released_tone.mean,
        neutral.mean,
        Tolerance::Above(0.0),
    )?;
    checks.compare(
        released,
        "the committed Amount 60's central mean luma between: below the Standard picture's",
        standard.mean,
        released_tone.mean,
        Tolerance::Above(0.0),
    )?;
    let figures = json!({
        "standard": standard.record(),
        "neutral": neutral.record(),
        "drafted_amount_60": drafted_tone.record(),
        "committed_amount_60": released_tone.record(),
        "standard_again_against_opened": {"mean_codes": mean, "share_over_2": share},
    });
    run.record("central_luma", figures.clone());
    checks.note(
        neutral_frame,
        "the central region's luma: Standard, Neutral and Amount 60",
        figures,
    );

    // The drag: drafted on the GPU from its own revision, never committed until released, which
    // commits once.
    let first_reasons: Vec<&Value> =
        crate::gpu_preview_smoke::step_events(launch, names::AMOUNT_FIRST)?
            .iter()
            .filter(|event| {
                event["event"] == "gpu_preview_tick" && event["detail"]["path"] == "cpu"
            })
            .map(|event| &event["detail"]["reason"])
            .collect();
    let drawn = crate::gpu_preview_smoke::gpu_drawn(drafted)?;
    let (gpu_ticks, cpu_ticks, jobs) = crate::gpu_preview_smoke::ticks(
        crate::gpu_preview_smoke::step_events(launch, names::AMOUNT_DRAG)?,
    );
    ensure(
        gpu_ticks >= 1 && cpu_ticks == 0 && jobs == 0,
        format!(
            "The Amount drag's tick was {gpu_ticks} on the GPU and {cpu_ticks} on the CPU, with \
             {jobs} preview jobs"
        ),
    )?;
    let commits = |step: &str| -> Result<usize> {
        Ok(crate::gpu_preview_smoke::named(
            crate::gpu_preview_smoke::step_events(launch, step)?,
            "slider_draft_commit",
        )
        .len())
    };
    for step in [names::AMOUNT_FIRST, names::AMOUNT_HELD, names::AMOUNT_DRAG] {
        ensure(
            commits(step)? == 0,
            format!("{step}: the open drag committed"),
        )?;
    }
    ensure(
        commits(names::AMOUNT_RELEASE)? == 1,
        "The Amount release did not commit once",
    )?;
    let at_rest = crate::gpu_preview_smoke::at_rest_after(
        crate::gpu_preview_smoke::span_events(
            launch,
            names::AMOUNT_RELEASE,
            names::AMOUNT_SETTLED,
        )?,
        released,
        names::AMOUNT_RELEASE,
    )?;
    checks.note(
        drafted,
        "the Amount drag drafted on the GPU and released once",
        json!({"first_tick_cpu_reasons": first_reasons, "drafted": drawn,
               "drafted_ticks": {"gpu": gpu_ticks, "cpu": cpu_ticks, "jobs": jobs},
               "released_at_rest": at_rest}),
    );
    let amount = described_look(launch, names::DESCRIBE_AMOUNT)?;
    ensure(
        amount["values"][AMOUNT] == json!(DRAG_AMOUNT) && amount["neutral"] == json!(false),
        format!("The Amount 60 look row is {amount}"),
    )?;

    // The double-click: one jump committed, then the reset sent once.
    let click = crate::gpu_preview_smoke::step_events(launch, names::DOUBLE_CLICK)?;
    let sent = crate::gpu_preview_smoke::named(click, "field_reset_sent");
    ensure(
        sent.len() == 1
            && sent[0]["detail"]["action"] == SET_LOOK
            && sent[0]["detail"]["preset"] == json!({ AMOUNT: FULL_AMOUNT })
            && crate::gpu_preview_smoke::named(click, "slider_draft_commit").len() == 1,
        format!("The Amount double-click did not jump once and reset once: {sent:?}"),
    )?;

    // The API's Neutral: the section follows it, and the core describes it.
    let api = described_look(launch, names::DESCRIBE_NEUTRAL)?;
    ensure(
        api["values"] == json!({ LOOK: NEUTRAL }),
        format!("The API's Neutral look row is {api}"),
    )?;
    checks.note(
        launch.at(names::API_NEUTRAL)?,
        "edit.set-look {look: neutral} through the API, which the section follows",
        json!({"field": launch.at(names::API_NEUTRAL)?.field(SET_LOOK, LOOK)?, "row": api}),
    );

    // Settings › General: the starting look for new RAW photos is Standard.
    let general = launch.at(names::SETTINGS)?;
    let row = general.state()["settings"]["general"]["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == RAW_LOOK_PREFERENCE))
        .ok_or("Settings › General draws no starting look row")?;
    ensure(
        general.state()["settings"]["open"] == "general"
            && row["control"] == json!({"choice": STANDARD}),
        format!("Settings › General's starting look row is {row}"),
    )?;
    checks.note(general, "Settings › General's starting look", row.clone());

    // The Look band's dot, last, so every other record is written whatever it finds: dark on the
    // Original's Standard look, after Standard is chosen again and after the reset; lit at
    // Amount 60, and for Neutral, through the section or the API.
    let dots = [
        (names::OPENED, false),
        (names::NEUTRAL, NEUTRAL_LIGHTS_THE_DOT),
        (names::STANDARD, false),
        (names::AMOUNT_RELEASE, true),
        (names::RESET, false),
        (names::DOUBLE_CLICK, false),
        (names::API_NEUTRAL, NEUTRAL_LIGHTS_THE_DOT),
    ];
    let mut wrong = Vec::new();
    for (step, lit) in dots {
        let frame = launch.at(step)?;
        let shown = look_dot(frame)?;
        checks.note(
            frame,
            "the Look band's edited dot",
            json!({"step": step, "lit": shown, "expected": lit}),
        );
        if shown != lit {
            wrong.push(format!("{step}: {shown}, expected {lit}"));
        }
    }
    let neutral_row = described_look(launch, names::DESCRIBE_NEUTRAL)?["neutral"].clone();
    let written = checks.write(
        &launch.evidence,
        SCENARIO,
        json!({"scope": "a supplied RAW photograph's central half at Fit, 8-bit display codes",
               "neutral_look_reported_neutral": neutral_row}),
    );
    ensure(
        wrong.is_empty(),
        format!(
            "The Look band's dot is wrong: {}; recipe.describe reports the Neutral look \
             neutral={neutral_row}",
            wrong.join("; ")
        ),
    )?;
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the plan scripts at the named step.
    fn scripted(plan: &Plan, step: &str) -> Value {
        let at = plan
            .index(step)
            .unwrap_or_else(|| panic!("{step:?} is not planned"));
        plan.steps()[at]
            .script()
            .unwrap_or_else(|| panic!("{step:?} scripts nothing"))
    }

    /// One frame for the open and one per script step, each named for what it scripts, and the
    /// table's row runs this plan, outside `rendered`. No RAW run can be replayed, so this ties the
    /// names the checks read to the steps they mean.
    #[test]
    fn the_plan_is_the_open_and_one_frame_per_step_each_named_for_what_it_scripts() {
        let raw = [PathBuf::from("/raw/photo.nef")];
        let plan = plan(&raw);
        assert!(plan.validate().is_ok(), "{:?}", plan.validate());
        assert_eq!(
            plan.script().as_array().map(|steps| steps.len() + 1),
            Some(plan.len())
        );
        assert_eq!(plan.steps()[0].name(), names::OPENED);
        assert_eq!(scripted(&plan, names::NEUTRAL), choose(NEUTRAL).to_value());
        assert_eq!(
            scripted(&plan, names::STANDARD),
            choose(STANDARD).to_value()
        );
        let slider = |values: f64, release: bool| {
            let step = SliderStep::new(SET_LOOK, AMOUNT, [values]);
            script::Step::Slider(if release { step.release() } else { step }).to_value()
        };
        assert_eq!(
            scripted(&plan, names::AMOUNT_FIRST),
            slider(FIRST_AMOUNT, false)
        );
        assert_eq!(
            scripted(&plan, names::AMOUNT_DRAG),
            slider(DRAG_AMOUNT, false)
        );
        assert_eq!(
            scripted(&plan, names::AMOUNT_RELEASE),
            slider(DRAG_AMOUNT, true)
        );
        assert_eq!(
            plan.index(names::AMOUNT_HELD),
            plan.index(names::AMOUNT_FIRST).map(|at| at + 1)
        );
        assert_eq!(
            scripted(&plan, names::RESET),
            script::Step::reset(LOOK_MODULE, None).to_value()
        );
        assert_eq!(
            scripted(&plan, names::API_NEUTRAL),
            script::Step::call("edit.set-look", json!({"look": "neutral"})).to_value()
        );
        for step in [
            names::DESCRIBE_OPENED,
            names::DESCRIBE_AMOUNT,
            names::DESCRIBE_NEUTRAL,
        ] {
            assert_eq!(scripted(&plan, step), describe().to_value(), "{step}");
        }
        let row = crate::smoke::find(SCENARIO).unwrap();
        assert_eq!((row.launches[0].plan)(&raw).script(), plan.script());
        assert!(!row.rendered() && row.takes_source() && !row.listed());
    }

    /// The scripted values are declared Amounts on the parameter's step, off its default, which
    /// is the amount a new RAW photograph's Standard look holds.
    #[test]
    fn the_amounts_are_declared_and_the_default_is_the_originals() {
        let registry = luxforge_core::ModuleRegistry::builtin();
        let (_, action) = registry.action(SET_LOOK).expect("set-look is declared");
        let parameter = action.parameter(AMOUNT).expect("amount is declared");
        let luxforge_core::ParameterKind::Number { min, max } = parameter.kind else {
            panic!("amount is a number");
        };
        let default = parameter.default.as_ref().and_then(Value::as_f64).unwrap();
        assert_eq!(default, FULL_AMOUNT);
        assert_eq!(standard_payload(FULL_AMOUNT)[AMOUNT], json!(default));
        let step = parameter.step.unwrap_or(1.0);
        for amount in [FIRST_AMOUNT, DRAG_AMOUNT, JUMP_AMOUNT] {
            assert!((min..=max).contains(&amount) && amount != default);
            assert_eq!((amount / step).round() * step, amount);
        }
        const _: () = assert!(GAP_MS <= 250);
    }

    /// Percentiles are nearest-rank, as everywhere in the workspace.
    #[test]
    fn the_tone_figures_are_nearest_rank() {
        let mut luma: Vec<f64> = (1..=10).rev().map(f64::from).collect();
        let tone = Tone::of(&mut luma);
        assert_eq!((tone.p10, tone.p90, tone.mean), (1.0, 9.0, 5.5));
        assert_eq!(tone.spread(), 8.0);
    }
}
