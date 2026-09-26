//! Ten renderer captures of the full 78-state widget gallery in the real desktop.
use crate::{
    scenario::{Checked, Frame, Plan, Run, Step, pixels, plan::only},
    *,
};
use luxforge_evidence::{self as script};

pub const WINDOW: [&str; 2] = ["1440", "1000"];
pub const PAGES: usize = 10;
pub const STATES: usize = 78;

/// The step that shows gallery page `page`.
fn page_step(page: usize) -> String {
    format!("page-{page}")
}

/// The open, one frame per gallery page, and the return to the editor. Showing the gallery is view
/// state: nothing is committed, and the return leaves the editor as it opened.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(
        std::iter::once(Step::opened("opened"))
            .chain((0..PAGES).map(|page| {
                Step::new(page_step(page), script::Step::gallery(Some(page))).commits(0)
            }))
            .chain(std::iter::once(
                Step::new("returned", script::Step::gallery(None))
                    .commits(0)
                    .label("Original"),
            ))
            .collect(),
    )
}

/// A small image-content check independent of the gallery's own state metadata. A valid renderer
/// readback must contain multiple visual elements in the central board area; an all-background
/// capture or a page with only a title is not accepted as rendered widget evidence.
fn board_content(frame: &Frame) -> Result<Value> {
    let image = frame.image()?;
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
            frame.path()?.display(),
            colours.len()
        ),
    )?;
    Ok(
        json!({"physical_size":[width,height],"sampled_bounds":[left,top,right,bottom],
        "distinct_colours":colours.len(),"non_background_samples":changed}),
    )
}

pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let initial = launch.at("opened")?;
    let original = pixels::identity_photo(initial)?;
    let mut checks = vec![json!({"frame":initial["file"],"original_photo":original})];
    let mut states = 0usize;
    for page in 0..PAGES {
        let frame = launch.at(&page_step(page))?;
        let info = &frame["state"]["gallery"];
        ensure(
            info["page"] == page && info["count"] == PAGES,
            format!("Gallery capture {page} describes another page: {info}"),
        )?;
        // The page is the desktop's own view state, so the session's workspace is untouched.
        ensure(
            frame["state"]["workspace"] == initial["state"]["workspace"],
            format!("Gallery capture {page} changed the session workspace"),
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
        let image = board_content(frame)?;
        checks.push(json!({"frame":frame["file"],"page":info,"board":image}));
    }
    ensure(
        states == STATES,
        format!("Gallery pages contain {states} states, expected all {STATES}"),
    )?;
    let returned = launch.at("returned")?;
    let returned_photo = pixels::identity_photo(returned)?;
    ensure(
        returned["state"]["gallery"].is_null()
            && returned["state"]["workspace"] == initial["state"]["workspace"]
            && returned["state"]["stack"] == initial["state"]["stack"]
            && returned["state"]["displayed_generation"]
                == initial["state"]["displayed_generation"],
        "Returning from gallery changed the editor",
    )?;
    checks.push(
        json!({"frame":returned["file"],"returned_photo":returned_photo,
        "editor_preserved":true}),
    );
    write_json(&launch.evidence.join("gallery-checks.json"), &json!(checks))?;
    Ok(())
}
