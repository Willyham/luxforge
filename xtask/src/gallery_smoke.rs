//! Ten renderer captures of the full 72-state widget gallery in the real desktop.
use crate::{smoke::frame_identity, *};

pub const WINDOW: [&str; 2] = ["1440", "1000"];
pub const PAGES: usize = 10;
pub const STATES: usize = 72;

pub fn frames(scenario: &str) -> Option<usize> {
    (scenario == "gallery").then_some(PAGES + 2)
}

pub fn script(scenario: &str) -> Option<Value> {
    (scenario == "gallery").then(|| {
        Value::Array(
            (0..PAGES)
                .map(|page| json!({"gallery":{"page":page}}))
                .chain(std::iter::once(json!({"gallery":{"page":null}})))
                .collect(),
        )
    })
}

/// A small image-content check independent of the gallery's own state metadata. A valid renderer
/// readback must contain multiple visual elements in the central board area; an all-background
/// capture or a page with only a title is not accepted as rendered widget evidence.
fn board_content(path: &Path) -> Result<Value> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    ensure(
        width >= 1440 && height >= 1000,
        "Gallery capture is too small to review",
    )?;
    // The board deliberately anchors its cards at the left edge; the named-icons page is a
    // narrow column there. Exclude the title/navigation band and the outer padding, not most of the
    // actual content as a centred crop would.
    let (left, right, top, bottom) = (width / 100, width * 99 / 100, height / 10, height * 3 / 4);
    let mut colours = std::collections::BTreeSet::new();
    let mut changed = 0u32;
    let background = image.get_pixel(width / 2, height - 20).0;
    for y in (top..bottom).step_by(4) {
        for x in (left..right).step_by(4) {
            let colour = image.get_pixel(x, y).0;
            if colour
                .iter()
                .zip(background)
                .any(|(a, b)| a.abs_diff(b) > 12)
            {
                changed += 1;
            }
            colours.insert(colour);
        }
    }
    ensure(
        colours.len() >= 12 && changed >= 200,
        format!(
            "Gallery board in {} is blank or unreadable: {} colours, {changed} differing samples",
            path.display(),
            colours.len()
        ),
    )?;
    Ok(
        json!({"physical_size":[width,height],"sampled_bounds":[left,top,right,bottom],
        "distinct_colours":colours.len(),"non_background_samples":changed}),
    )
}

pub fn verify(evidence: &Path, app: &Value, events: &[Value]) -> Result {
    let frames = app["frames"].as_array().ok_or("Missing gallery frames")?;
    ensure(
        frames.len() == PAGES + 2 && app["had_input_errors"] == false,
        "Gallery run is incomplete or reported an input error",
    )?;
    let script = script("gallery").expect("static gallery script");
    let steps = script.as_array().unwrap();
    let logged: Vec<&Value> = events
        .iter()
        .filter(|event| event["event"] == "script_step")
        .collect();
    ensure(
        logged.len() == PAGES + 1,
        "A gallery script event is missing",
    )?;
    let initial = frame_identity(evidence, app, &frames[0])?;
    let original = controls_smoke::identity_photo(&initial, &frames[0])?;
    let mut checks = vec![json!({"frame":frames[0]["file"],"original_photo":original})];
    let mut states = 0usize;
    for page in 0..PAGES {
        let frame = &frames[page + 1];
        let path = frame_identity(evidence, app, frame)?;
        let info = &frame["state"]["gallery"];
        ensure(
            info["page"] == page
                && info["count"] == PAGES
                && frame["state"]["workspace"]["component_gallery"] == page,
            format!("Gallery capture {page} describes another page: {info}"),
        )?;
        let count = info["state_count"]
            .as_u64()
            .ok_or("Missing gallery state count")? as usize;
        ensure(
            count > 0
                && info["title"]
                    .as_str()
                    .is_some_and(|title| !title.is_empty()),
            format!("Gallery page {page} lacks named states"),
        )?;
        states += count;
        ensure(
            frame["step"]["step"] == page + 1
                && frame["step"]["request"] == steps[page]
                && logged[page]["detail"]["step"] == page + 1
                && logged[page]["detail"]["request"] == steps[page],
            format!("Gallery page {page} is not correlated to its script/log"),
        )?;
        let image = board_content(&path)?;
        checks.push(json!({"frame":frame["file"],"page":info,"board":image}));
    }
    ensure(
        states == STATES,
        format!("Gallery pages contain {states} states, expected all {STATES}"),
    )?;
    let returned = frames.last().unwrap();
    let path = frame_identity(evidence, app, returned)?;
    let returned_photo = controls_smoke::identity_photo(&path, returned)?;
    ensure(
        returned["state"]["gallery"].is_null()
            && returned["state"]["workspace"] == frames[0]["state"]["workspace"]
            && returned["state"]["stack"] == frames[0]["state"]["stack"]
            && returned["state"]["displayed_generation"]
                == frames[0]["state"]["displayed_generation"]
            && returned["step"]["request"] == steps[PAGES]
            && logged[PAGES]["detail"]["request"] == steps[PAGES],
        "Returning from gallery changed the editor or failed request/session correlation",
    )?;
    checks.push(
        json!({"frame":returned["file"],"returned_photo":returned_photo,
        "editor_preserved":true}),
    );
    write_json(&evidence.join("gallery-checks.json"), &json!(checks))?;
    Ok(())
}
