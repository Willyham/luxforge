//! The `theme` smoke scenario: UI themes end to end at 1440 × 900 over the orientation 1 fixture
//! ([design](../../docs/design/ui-themes.md#verification)). It opens the Settings sheet at
//! Appearance and imports the synthetic Omarchy set through the tab's own Import Omarchy theme…
//! task: Dusk (dark) and Linen (light) are imported, a theme named like the bundled Nord is listed
//! as already built in and one with no accent fails with its reason; importing Dusk's folder again
//! lists it as already imported. With the sheet closed it switches from Luxforge Dark to the
//! bundled Nord, Dusk and Linen, opens the sheet once more in the light theme, sets the canvas
//! background to Grey and back to Theme, exports the photograph, and then a second client of the
//! session switches the theme back to Luxforge Dark with `preferences.set`, after which the
//! photograph is exported again.
//!
//! Every frame records the theme drawn, its resolved tokens and the library; the import steps
//! record each theme's outcome and report. The runner checks the frames against the tokens the
//! core resolves (Luxforge Dark's and Nord's from the core itself, the imports' from what their
//! import answered), and the pixels against the frames: the panels' background in the status bar
//! and the state panel, the title bar's surface, a control (the Settings sheet's import buttons,
//! White balance's Neutral picker and As shot) and the canvas beside the photograph, which is the theme's surround, at
//! most 0.010 OKLCh chroma, while the canvas background is Theme and `#777777` under the light
//! theme while it is Grey. The photograph, located by the canvas colour each frame records rather
//! than by Luxforge Dark's, is the same pixels in every frame, and the two exports, under Linen and
//! under Luxforge Dark, have the same SHA-256.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, pixels, plan::only},
    *,
};
use luxforge_core::theme::{LUXFORGE_DARK_ID, Token, built_in_themes};
use luxforge_evidence::{self as script};

pub const SCENARIO: &str = "theme";
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
/// The synthetic Omarchy set, and one theme folder of it.
const SET: &str = "fixtures/themes/omarchy";
const DUSK_FOLDER: &str = "fixtures/themes/omarchy/dusk";
/// The bundled theme the scenario switches to.
const NORD: &str = "omarchy.nord";
/// The two exports, under Linen and under Luxforge Dark.
const EXPORT_LIGHT: &str = "theme-linen.jpg";
const EXPORT_DARK: &str = "theme-luxforge-dark.jpg";

/// The canvas background's fixed Grey.
const GREY: [u8; 3] = [0x77, 0x77, 0x77];
/// The surround's chroma bound, in OKLCh.
const CHROMA_BOUND: f64 = 0.010;
/// How far a flat fill may read from its token in any channel: nothing, since an opaque quad is
/// drawn in its own sRGB code.
const FILL_TOLERANCE: f64 = 0.0;
/// The fewest pixels of exactly the control token a control region must hold: a compact button is
/// over 40 × 20 points, so at any scale factor at least this many of its pixels are its fill.
const CONTROL_PIXELS: u64 = 400;
/// The share of a sampled region its dominant colour must hold for that colour to be the region's
/// surface rather than a widget on it.
const DOMINANT_SHARE: f64 = 0.5;

/// The tokens a frame records and the runner samples.
const TOKENS: [(&str, Token); 5] = [
    ("surround", Token::Surround),
    ("background", Token::Background),
    ("surface", Token::Surface),
    ("control", Token::Control),
    ("text", Token::Text),
];

/// Which theme each step's frame must draw: a built-in theme by its id, an import by its name.
const DRAWN: [(&str, &str); 15] = [
    ("opened", LUXFORGE_DARK_ID),
    ("appearance", LUXFORGE_DARK_ID),
    ("imported", LUXFORGE_DARK_ID),
    ("again", LUXFORGE_DARK_ID),
    ("closed", LUXFORGE_DARK_ID),
    ("nord", NORD),
    ("dusk", "Dusk"),
    ("linen", "Linen"),
    ("linen-sheet", "Linen"),
    ("linen-closed", "Linen"),
    ("grey", "Linen"),
    ("theme-canvas", "Linen"),
    ("export-linen", "Linen"),
    ("agent", LUXFORGE_DARK_ID),
    ("export-dark", LUXFORGE_DARK_ID),
];

/// The frames the Settings sheet covers the workspace in.
const SHEET: [&str; 4] = ["appearance", "imported", "again", "linen-sheet"];

pub fn plan(_: &[PathBuf]) -> Plan {
    let step = |name: &str, script: script::Step| Step::new(name, script).commits(0);
    Plan::new(vec![
        Step::opened("opened"),
        step("appearance", script::Step::settings_tab("appearance")),
        step("imported", script::Step::theme_import_omarchy(SET)),
        step("again", script::Step::theme_import_omarchy(DUSK_FOLDER)),
        step("closed", script::Step::settings(false)),
        step("nord", script::Step::theme(NORD)),
        step("dusk", script::Step::theme_named("Dusk")),
        step("linen", script::Step::theme_named("Linen")),
        // The tab drawn in the light theme, for review.
        step("linen-sheet", script::Step::settings_tab("appearance")),
        step("linen-closed", script::Step::settings(false)),
        step(
            "grey",
            script::Step::preference([("canvas_background", json!("grey"))]),
        ),
        step(
            "theme-canvas",
            script::Step::preference([("canvas_background", json!("theme"))]),
        ),
        step("export-linen", script::Step::export(EXPORT_LIGHT, false)),
        // Another client of the session switches the theme back.
        step(
            "agent",
            script::Step::agent("preferences.set", json!({"theme": LUXFORGE_DARK_ID})),
        ),
        step("export-dark", script::Step::export(EXPORT_DARK, false)),
    ])
}

fn theme(frame: &Frame) -> &Value {
    &frame.state()["theme"]
}

/// `#rrggbb` as its channels.
fn rgb(text: &Value) -> Result<[u8; 3]> {
    let text = text
        .as_str()
        .and_then(|text| text.strip_prefix('#'))
        .filter(|hex| hex.len() == 6)
        .ok_or_else(|| format!("{text} is not a #rrggbb colour"))?;
    let channel = |at: usize| u8::from_str_radix(&text[at..at + 2], 16);
    Ok([channel(0)?, channel(2)?, channel(4)?])
}

/// The token the frame records.
fn token(frame: &Frame, name: &str) -> Result<[u8; 3]> {
    rgb(&theme(frame)["tokens"][name])
}

/// OKLCh chroma of an sRGB colour, through the independent reference's conversion.
fn chroma(colour: [u8; 3]) -> f64 {
    let lab = luxforge_reference::colour::to_oklab(colour.map(luxforge_reference::srgb::decode));
    lab.a.hypot(lab.b)
}

/// What the canvas around the photograph is drawn in: the theme's surround with the canvas
/// background at Theme, else its fixed grey.
fn canvas_colour(frame: &Frame) -> Result<[u8; 3]> {
    match frame.state()["preferences"]["display"]["canvas_background"].as_str() {
        Some("theme") => token(frame, "surround"),
        Some("dark") => Ok(pixels::CANVAS),
        Some("black") => Ok([0, 0, 0]),
        Some("grey") => Ok(GREY),
        other => Err(format!("The frame records no canvas background: {other:?}").into()),
    }
}

/// The frame's canvas region, `[left, top, right, bottom]` physical pixels.
fn canvas_rect(frame: &Frame) -> Result<[u32; 4]> {
    serde_json::from_value(frame["canvas_rect"].clone())
        .map_err(|_| "The frame records no canvas rectangle".into())
}

/// The frame's scale factor.
fn scale(frame: &Frame) -> Result<u32> {
    let scale = frame["scale"]
        .as_f64()
        .ok_or("The frame records no scale factor")?;
    Ok(scale.round().max(1.0) as u32)
}

/// The most frequent colour in `region` and the share of its pixels it holds.
fn dominant(frame: &Frame, region: [u32; 4]) -> Result<([u8; 3], f64)> {
    let image = frame.image()?;
    let [left, top, right, bottom] = region;
    ensure(
        left < right && top < bottom && right <= image.width() && bottom <= image.height(),
        format!("The region {region:?} is not inside the capture"),
    )?;
    let mut counts = std::collections::BTreeMap::<[u8; 3], u64>::new();
    for y in top..bottom {
        for x in left..right {
            *counts.entry(image.get_pixel(x, y).0).or_default() += 1;
        }
    }
    let total = u64::from(right - left) * u64::from(bottom - top);
    let (colour, count) = counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .ok_or("An empty region")?;
    Ok((colour, count as f64 / total as f64))
}

/// The pixels in `region` that are exactly `colour`.
fn exactly(frame: &Frame, region: [u32; 4], colour: [u8; 3]) -> Result<u64> {
    let image = frame.image()?;
    let [left, top, right, bottom] = region;
    Ok((top..bottom)
        .flat_map(|y| (left..right).map(move |x| (x, y)))
        .filter(|(x, y)| image.get_pixel(*x, *y).0 == colour)
        .count() as u64)
}

/// The largest channel difference between two colours.
fn difference(a: [u8; 3], b: [u8; 3]) -> f64 {
    f64::from(
        a.iter()
            .zip(b)
            .map(|(a, b)| a.abs_diff(b))
            .max()
            .unwrap_or(0),
    )
}

/// The workspace's regions in a frame with the sheet closed: the title bar above the canvas, the
/// status bar under it, the state panel to its left and the tools panel to its right, each inset
/// from its edges.
struct Regions {
    title_bar: [u32; 4],
    status_bar: [u32; 4],
    state_panel: [u32; 4],
    tools_panel: [u32; 4],
}

fn regions(frame: &Frame) -> Result<Regions> {
    let [left, top, right, bottom] = canvas_rect(frame)?;
    let (width, height) = frame.image()?.dimensions();
    let inset = 4 * scale(frame)?;
    ensure(
        left > 2 * inset
            && top > 2 * inset
            && bottom + 2 * inset < height
            && right + 2 * inset < width,
        "The canvas leaves no room for the panels and the bars",
    )?;
    Ok(Regions {
        title_bar: [left + inset, inset, right - inset, top - inset],
        status_bar: [inset, bottom + inset, width - inset, height - inset],
        state_panel: [inset, top + inset, left - inset, bottom - inset],
        tools_panel: [right + inset, top + inset, width - inset, bottom - inset],
    })
}

/// The canvas beside the photograph, midway between the canvas's left edge and the photograph's
/// at its vertical middle: the largest channel difference from `expected` over a 5 × 5 patch, and
/// the colour at its centre.
fn beside_photo(frame: &Frame, photo: [u32; 4], expected: [u8; 3]) -> Result<(f64, [u8; 3])> {
    let [canvas_left, ..] = canvas_rect(frame)?;
    let [left, top, _, bottom] = photo;
    ensure(
        left >= canvas_left + 12,
        format!(
            "No canvas beside the photograph: it starts at {left}, the canvas at {canvas_left}"
        ),
    )?;
    let centre = [(canvas_left + left) / 2, (top + bottom) / 2];
    let image = frame.image()?;
    let mut worst = 0.0f64;
    for y in centre[1] - 2..=centre[1] + 2 {
        for x in centre[0] - 2..=centre[0] + 2 {
            worst = worst.max(difference(image.get_pixel(x, y).0, expected));
        }
    }
    Ok((worst, image.get_pixel(centre[0], centre[1]).0))
}

/// The pixels inside `photo`, less its outermost row and column on each side, that differ between
/// two captures.
fn photo_changed(a: &Frame, b: &Frame, photo: [u32; 4]) -> Result<u64> {
    let [left, top, right, bottom] = photo;
    let (a, b) = (a.image()?, b.image()?);
    Ok((top + 1..bottom - 1)
        .flat_map(|y| (left + 1..right - 1).map(move |x| (x, y)))
        .filter(|(x, y)| a.get_pixel(*x, *y) != b.get_pixel(*x, *y))
        .count() as u64)
}

/// The Settings sheet in a 1440 × 900 window: 720 × 480 points, centred, its tab rail 160 points
/// wide. The rail's empty foot, drawn on the surface; and the band under its header where the
/// Appearance tab's title and import buttons are, in fractions of the window.
const RAIL_FOOT: [f64; 2] = [(360.0 + 80.0) / 1440.0, (690.0 - 40.0) / 900.0];
const SHEET_HEADER: [f64; 4] = [
    (360.0 + 161.0) / 1440.0,
    (210.0 + 44.0) / 900.0,
    1080.0 / 1440.0,
    (210.0 + 110.0) / 900.0,
];

fn fraction_region(frame: &Frame, region: [f64; 4]) -> Result<[u32; 4]> {
    let (width, height) = frame.image()?.dimensions();
    let [left, top, right, bottom] = region;
    Ok([
        (f64::from(width) * left) as u32,
        (f64::from(height) * top) as u32,
        (f64::from(width) * right) as u32,
        (f64::from(height) * bottom) as u32,
    ])
}

/// The ids, names and resolved tokens the scenario's themes must draw: Luxforge Dark's and Nord's
/// as the core resolves them, and each import's as its own `theme.import` answered.
struct Expected {
    tokens: std::collections::BTreeMap<String, Value>,
    /// An import's name to its id, which is new on every run.
    ids: std::collections::BTreeMap<String, String>,
}

fn expected(imported: &Frame) -> Result<Expected> {
    let mut tokens = std::collections::BTreeMap::new();
    let mut ids = std::collections::BTreeMap::new();
    for id in [LUXFORGE_DARK_ID, NORD] {
        let theme = built_in_themes()
            .iter()
            .find(|theme| theme.id() == id)
            .ok_or_else(|| format!("The core holds no {id}"))?;
        let resolved: serde_json::Map<String, Value> = TOKENS
            .iter()
            .map(|(name, token)| {
                (
                    (*name).to_owned(),
                    json!(theme.resolved.tokens[*token].to_string()),
                )
            })
            .collect();
        tokens.insert(id.to_owned(), Value::Object(resolved));
        ids.insert(id.to_owned(), id.to_owned());
    }
    let answers = imported["step"]["folder_import"]["themes"]
        .as_array()
        .ok_or("The import step records no folder import")?;
    for answer in answers
        .iter()
        .filter(|answer| answer["outcome"] == "imported")
    {
        let name = answer["theme"]["name"]
            .as_str()
            .ok_or("An imported theme has no name")?;
        let id = answer["theme"]["id"]
            .as_str()
            .ok_or("An imported theme has no id")?;
        let resolved: serde_json::Map<String, Value> = TOKENS
            .iter()
            .map(|(token, _)| {
                (
                    (*token).to_owned(),
                    answer["theme"]["resolved"][*token].clone(),
                )
            })
            .collect();
        tokens.insert(id.to_owned(), Value::Object(resolved));
        ids.insert(name.to_owned(), id.to_owned());
    }
    Ok(Expected { tokens, ids })
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let opened = launch.at("opened")?;
    let mut checks = Checks::new();
    checks.note(
        opened,
        "the unedited photograph under Luxforge Dark",
        pixels::identity_photo(opened)?,
    );
    let imports = folder_imports(launch, &mut checks)?;
    let expected = expected(launch.at("imported")?)?;
    let photo = opened.photo_on(canvas_colour(opened)?)?;
    let mut surrounds = Vec::new();

    for (step, drawn) in DRAWN {
        let frame = launch.at(step)?;
        let state = frame.state();
        let id = expected
            .ids
            .get(drawn)
            .ok_or_else(|| format!("No theme {drawn} was imported"))?;
        // A theme changes no photograph and no session: the workspace and the stack are the
        // open's throughout.
        ensure(
            state["workspace"] == opened.state()["workspace"]
                && state["stack"] == opened.state()["stack"],
            format!("Step {step:?} changed the session workspace or the stack"),
        )?;
        ensure(
            theme(frame)["id"] == json!(id)
                && theme(frame)["chosen"] == json!(id)
                && theme(frame)["problem"].is_null()
                && theme(frame)["reading"].is_null()
                && theme(frame)["pending"] == false,
            format!(
                "Step {step:?} does not draw {drawn} ({id}) settled: {}",
                theme(frame)
            ),
        )?;
        ensure(
            theme(frame)["tokens"] == expected.tokens[id],
            format!(
                "Step {step:?} records tokens {} for {drawn}, not the {} its theme resolves to",
                theme(frame)["tokens"],
                expected.tokens[id]
            ),
        )?;
        let surround = token(frame, "surround")?;
        checks.compare(
            frame,
            &format!("{step}: the surround's OKLCh chroma, at most {CHROMA_BOUND}"),
            chroma(surround),
            CHROMA_BOUND,
            Tolerance::AtMost(0.0),
        )?;
        surrounds.push(
            json!({"step": step, "theme": drawn, "surround": theme(frame)["tokens"]["surround"],
                              "chroma": chroma(surround)}),
        );

        if SHEET.contains(&step) {
            // The sheet and the scrim under it cover the photograph.
            sheet_pixels(frame, step, &mut checks)?;
            continue;
        }
        // The photograph, found by the canvas colour this frame records, is where and what it was
        // at the open.
        let canvas = canvas_colour(frame)?;
        let located = frame.photo_on(canvas)?;
        ensure(
            located == photo,
            format!("Step {step:?} moved the photograph: {located:?}, not {photo:?}"),
        )?;
        let changed = photo_changed(opened, frame, photo)?;
        checks.compare(
            frame,
            &format!("{step}: pixels of the photograph that differ from the open's"),
            changed as f64,
            0.0,
            Tolerance::Within(0.0),
        )?;
        workspace_pixels(frame, step, photo, canvas, &mut checks)?;
    }

    // Linen is light, and the native window follows its mode; the others are dark.
    for (step, mode) in [("nord", "dark"), ("dusk", "dark"), ("linen", "light")] {
        ensure(
            theme(launch.at(step)?)["mode"] == mode,
            format!("Step {step:?} is not drawn {mode}"),
        )?;
    }
    let grey = launch.at("grey")?;
    ensure(
        grey.state()["preferences"]["display"]["canvas_background"] == "grey"
            && launch.at("theme-canvas")?.state()["preferences"]["display"]["canvas_background"]
                == "theme",
        "The canvas background was not Grey and then Theme again",
    )?;

    // The second client's choice reached the window through the event sync.
    let agent = launch.at("agent")?;
    ensure(
        agent["step"]["status"] == "sent"
            && agent["step"]["actor"] == "evidence-agent"
            && agent.state()["preferences"]["stored"]["theme"] == LUXFORGE_DARK_ID,
        format!(
            "The second client's preferences.set did not reach the window: {} {}",
            agent["step"],
            agent.state()["preferences"]["stored"]
        ),
    )?;

    // One export under Linen and one under Luxforge Dark: the same bytes.
    let light = launch.evidence.join(EXPORT_LIGHT);
    let dark = launch.evidence.join(EXPORT_DARK);
    let (light_hash, dark_hash) = (hash(&light)?, hash(&dark)?);
    ensure(
        fs::metadata(&light)?.len() > 0,
        "The export under Linen is empty",
    )?;
    ensure(
        light_hash == dark_hash,
        format!(
            "The exports differ by theme: {light_hash} under Linen, {dark_hash} under Luxforge Dark"
        ),
    )?;
    checks.note(
        launch.at("export-dark")?,
        "the same export under Linen and under Luxforge Dark",
        json!({EXPORT_LIGHT: light_hash, EXPORT_DARK: dark_hash}),
    );
    checks.write(
        &launch.evidence,
        run.scenario(),
        json!({
            "imports": imports,
            "surrounds": surrounds,
            "photo": photo,
            "chroma_bound": CHROMA_BOUND,
            "fill_tolerance_per_channel": FILL_TOLERANCE,
            "dominant_share": DOMINANT_SHARE,
            "control_pixels": CONTROL_PIXELS,
            "scope": "Rendered fills sampled against the frame's recorded tokens: the status bar and \
                      the state panel's dominant colour (background), the title bar's (surface), \
                      the count of exact control pixels in the tools panel or the sheet's header \
                      band, and a 5 × 5 patch of the canvas beside the photograph (surround, or \
                      #777777 at Grey). The photograph is compared pixel for pixel, less its \
                      outermost row and column; text is not sampled.",
        }),
    )
}

/// The two import steps: each theme's outcome as the tab lists it and the step records it with
/// its report, and the library after them.
fn folder_imports(launch: &Checked, checks: &mut Checks) -> Result<Value> {
    let outcomes = |frame: &Frame| -> Vec<(String, String)> {
        theme(frame)["folder_import"]["themes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|theme| {
                (
                    theme["folder"].as_str().unwrap_or_default().to_owned(),
                    theme["outcome"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    };
    let expect = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(folder, outcome)| ((*folder).to_owned(), (*outcome).to_owned()))
            .collect()
    };
    let imported = launch.at("imported")?;
    ensure(
        outcomes(imported)
            == expect(&[
                ("dusk", "imported"),
                ("no-accent", "failed"),
                ("nord", "built-in"),
                ("omarchy-linen-theme", "imported"),
            ]),
        format!(
            "The set's themes became {}",
            theme(imported)["folder_import"]
        ),
    )?;
    let answers = imported["step"]["folder_import"]["themes"]
        .as_array()
        .ok_or("The import step records no answers")?;
    ensure(
        answers.len() == 4
            && answers
                .iter()
                .filter(|answer| answer["outcome"] == "imported")
                .all(|answer| answer["report"]["omarchy"].is_object())
            && answers[1]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("has no accent")),
        format!("The import step does not record each theme's report or reason: {answers:?}"),
    )?;
    let library = &theme(imported)["library"]["themes"];
    ensure(
        library.as_array().map(Vec::len) == Some(9),
        format!("The library after the set is not the seven built in and two imports: {library}"),
    )?;
    let again = launch.at("again")?;
    ensure(
        outcomes(again) == expect(&[("dusk", "already-imported")]),
        format!(
            "Importing Dusk again became {}",
            theme(again)["folder_import"]
        ),
    )?;
    let rows = imported.state()["settings"]["appearance"]["rows"]
        .as_array()
        .ok_or("The tab drew no rows")?;
    ensure(
        rows.iter()
            .filter(|row| {
                row["origin"] == "Omarchy \u{b7} omarchy-linen-theme" && row["mode"] == "Light"
            })
            .count()
            == 1,
        "The tab does not list Linen as a light Omarchy import",
    )?;
    let summary = json!({
        "set": theme(imported)["folder_import"],
        "again": theme(again)["folder_import"],
    });
    checks.note(
        imported,
        "the synthetic set imported through Import Omarchy theme…",
        summary.clone(),
    );
    Ok(summary)
}

/// A frame with the sheet closed: the title bar is the surface, the status bar and the state
/// panel the background, the tools panel holds the control token (White balance's Neutral picker
/// and As shot buttons), and the canvas beside the photograph is the canvas colour.
fn workspace_pixels(
    frame: &Frame,
    step: &str,
    photo: [u32; 4],
    canvas: [u8; 3],
    checks: &mut Checks,
) -> Result {
    ensure(
        frame.state()["settings"]["open"].is_null(),
        format!("The sheet is open at step {step:?}"),
    )?;
    let regions = regions(frame)?;
    for (name, region, token_name) in [
        ("the title bar", regions.title_bar, "surface"),
        ("the status bar", regions.status_bar, "background"),
        ("the state panel", regions.state_panel, "background"),
    ] {
        let expected = token(frame, token_name)?;
        let (colour, share) = dominant(frame, region)?;
        checks.compare(
            frame,
            &format!("{step}: {name}'s dominant colour {colour:?}, largest channel difference from the {token_name} token"),
            difference(colour, expected),
            0.0,
            Tolerance::Within(FILL_TOLERANCE),
        )?;
        checks.compare(
            frame,
            &format!("{step}: the share of {name} its dominant colour holds"),
            share,
            DOMINANT_SHARE,
            Tolerance::Above(0.0),
        )?;
    }
    let control = token(frame, "control")?;
    checks.compare(
        frame,
        &format!("{step}: pixels of exactly the control token in the tools panel"),
        exactly(frame, regions.tools_panel, control)? as f64,
        CONTROL_PIXELS as f64,
        Tolerance::Above(0.0),
    )?;
    let (worst, at) = beside_photo(frame, photo, canvas)?;
    checks.compare(
        frame,
        &format!("{step}: the canvas beside the photograph {at:?}, largest channel difference from {canvas:?}"),
        worst,
        0.0,
        Tolerance::Within(FILL_TOLERANCE),
    )?;
    Ok(())
}

/// A frame with the sheet open: its rail's foot is the surface, and its header band holds the
/// import buttons' control fill.
fn sheet_pixels(frame: &Frame, step: &str, checks: &mut Checks) -> Result {
    ensure(
        frame.state()["settings"]["open"] == "appearance",
        format!("The sheet is not open at Appearance at step {step:?}"),
    )?;
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let foot = image
        .get_pixel(
            (f64::from(width) * RAIL_FOOT[0]) as u32,
            (f64::from(height) * RAIL_FOOT[1]) as u32,
        )
        .0;
    checks.compare(
        frame,
        &format!(
            "{step}: the sheet's rail {foot:?}, largest channel difference from the surface token"
        ),
        difference(foot, token(frame, "surface")?),
        0.0,
        Tolerance::Within(FILL_TOLERANCE),
    )?;
    let header = fraction_region(frame, SHEET_HEADER)?;
    checks.compare(
        frame,
        &format!("{step}: pixels of exactly the control token in the sheet's header band"),
        exactly(frame, header, token(frame, "control")?)? as f64,
        CONTROL_PIXELS as f64,
        Tolerance::Above(0.0),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_names_a_drawn_theme_for_every_frame() {
        let plan = plan(&[]);
        plan.validate().unwrap();
        let names: Vec<_> = plan.steps().iter().map(Step::name).collect();
        let drawn: Vec<_> = DRAWN.iter().map(|(step, _)| *step).collect();
        assert_eq!(names, drawn);
        assert!(SHEET.iter().all(|step| names.contains(step)));
    }

    #[test]
    fn the_chroma_reads_luxforge_darks_canvas_as_near_neutral() {
        assert!(chroma(pixels::CANVAS) < 0.005);
        assert!(chroma([0x77, 0x77, 0x77]) < 1e-6);
        // A navy surround such as Retro 82's own is well past the bound.
        assert!(chroma([0x03, 0x12, 0x22]) > CHROMA_BOUND);
        assert_eq!(rgb(&json!("#19191b")).unwrap(), pixels::CANVAS);
        assert!(rgb(&json!("#19191")).is_err());
    }
}
