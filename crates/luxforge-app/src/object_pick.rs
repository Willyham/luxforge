//! Object picking, shared by every tool that points at an object rather than painting it: Masks ›
//! Object and the Corrections band's Object kind (`docs/design/ai-tools-workspace.md`, "Object" and
//! "Object in the Masks panel").
//!
//! This is the gesture's plain data and the one place its requests are built, so a gesture and its
//! Copy as JSON request cannot drift apart. Nothing here reads a pixel or calls the owner; the
//! controller ([`crate::app::object_pick`]) sends what is built here and hands the answers back.
//!
//! **The gestures.** Over the photograph the cursor is a crosshair and the object under it is
//! washed white inside a white outline over a dark under-stroke, from `query.pick-object {point}`
//! with one pick in flight and the latest position asked next. A press that comes up within
//! [`crate::range_pick::CLICK_TRAVEL`] of where it went down is a **click**: with an object outlined it selects that
//! object (`task.select-object {asset_id, point}`), and with none it says
//! [`NOTHING_TO_SELECT`]. A longer one is a **loose drag**, drawn as a dashed stroke; on release
//! `query.pick-object {stroke, radius, previews: all}` decides **One** (selected as a click would),
//! **Several** (a numbered pin on each candidate, toggled by a click, used by Return or Use N,
//! dismissed by Escape) or **Nothing** (the stroke stays tinted under [`NOTHING_FOUND`]; what that
//! hands off to is the tool's).
//!
//! **Selections.** Each chosen object becomes one `task.select-object`, run one at a time from a
//! bounded queue of [`SELECTION_LIMIT`]; what its published artifact becomes is the tool's
//! ([`SelectFor`]).
//!
//! All coordinates are the content stage's normalized coordinates, which the picker and the
//! selection task take and a model mask stores.
use crate::{
    app::{draft::GestureId, tasks::CallError},
    brush::{ContentMap, Modifiers},
};
use iced::widget::image::Handle;
use luxforge_core::{
    AssetId, ComponentId, EntryId, MaskId, ModuleDescriptor,
    flags::FlagList,
    mask::{OBJECT_PROMPT_POINTS_MAX, ObjectPrompt},
};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};

/// `query.pick-object`.
pub(crate) const PICK: &str = "query.pick-object";
/// `task.analyse`.
pub(crate) const ANALYSE: &str = "task.analyse";
/// `task.select-object`.
pub(crate) const SELECT: &str = "task.select-object";
/// `module.runtime.load`.
pub(crate) const RUNTIME_LOAD: &str = luxforge_core::capabilities::runtime::LOAD;
/// The picker's query and the two tasks, without their prefixes.
const PICK_QUERY: &str = "pick-object";
const ANALYSE_TASK: &str = "analyse";
const SELECT_TASK: &str = "select-object";
/// The most objects one tool holds or waits to select at once: a recorded default of the design.
pub(crate) const SELECTION_LIMIT: usize = 8;
/// What the canvas chip and the tool say until the photograph is analysed.
pub(crate) const FINDING_OBJECTS: &str = "Finding objects\u{2026}";
/// What a click with nothing outlined says in the status bar.
pub(crate) const NOTHING_TO_SELECT: &str = "Nothing to select here";
/// What a loose drag that found nothing says.
pub(crate) const NOTHING_FOUND: &str = "Nothing found here";
/// The wash over a fully covered pixel of the hovered object: white, translucent enough that the
/// object stays readable under it. A wash in a hue would vanish over an object of that hue.
const WASH: [u8; 3] = [255, 255, 255];
const WASH_ALPHA: f32 = 0.35;
/// A selected object's tint: Remove's selection tint, so a picked object reads as a painted
/// selection does.
const TINT: [u8; 3] = [255, 69, 69];
const TINT_ALPHA: f32 = 0.32;
/// The outline, opaque white, and the under-stroke outside it, Corrections' translucent dark ink.
const OUTLINE: [u8; 4] = [255, 255, 255, 255];
const UNDER_STROKE: [u8; 4] = [0, 0, 0, 115];
/// The coverage code at and above which a preview pixel is inside the object.
const INSIDE: u8 = 128;

/// The module that offers the picker: one declaring `query.pick-object` whose analysis task is
/// offered under the flags as they are.
pub(crate) fn pick_offered(modules: &[ModuleDescriptor], flags: Option<&FlagList>) -> bool {
    modules.iter().filter(|m| m.is_available()).any(|module| {
        module.queries.iter().any(|query| query.id == PICK_QUERY)
            && module
                .task(ANALYSE_TASK)
                .is_some_and(|task| crate::state::settings::task_offered(flags, task))
    })
}

/// The module whose `task.select-object` is offered under the flags as they are, with the picker
/// beside it: the run can pick an object and keep it.
pub(crate) fn select_module<'a>(
    modules: &'a [ModuleDescriptor],
    flags: Option<&FlagList>,
) -> Option<&'a ModuleDescriptor> {
    if !pick_offered(modules, flags) {
        return None;
    }
    modules.iter().filter(|m| m.is_available()).find(|module| {
        module
            .task(SELECT_TASK)
            .is_some_and(|task| crate::state::settings::task_offered(flags, task))
    })
}

/// A setup a picking tool needs, by the id its module declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupId {
    /// Masks › Object: the object finder.
    MaskObject,
    /// Corrections › Object: the object finder and the generative fill.
    Object,
}

impl SetupId {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MaskObject => crate::state::fill_setup::MASK_OBJECT,
            Self::Object => crate::state::fill_setup::OBJECT,
        }
    }
}

/// The line of `setup` (one of `fill_setup`'s ids, such as [`fill_setup::MASK_OBJECT`]) a picking
/// tool needs, from the available module that declares it, as the setup flow derives it from the
/// module's last-read status: installed, checking (`state` is `None`), or what to press.
///
/// [`fill_setup::MASK_OBJECT`]: crate::state::fill_setup::MASK_OBJECT
pub(crate) fn setup_line(
    modules: &[ModuleDescriptor],
    capabilities: &crate::state::capabilities::CapabilityStore,
    setup: &str,
) -> Option<crate::state::fill_setup::SetupLine> {
    modules
        .iter()
        .filter(|module| module.is_available())
        .find_map(|module| {
            crate::state::fill_setup::group(module, capabilities.module(&module.id), setup)
        })
}

/// The hovered object as the canvas draws it: an image over its bounds in normalized content
/// coordinates, for the photograph and revision it was picked on.
#[derive(Clone, Debug)]
pub(crate) struct HoverOverlay {
    pub(crate) asset: AssetId,
    pub(crate) revision: u64,
    /// `[x0, y0, x1, y1]`.
    pub(crate) bounds: [f64; 4],
    pub(crate) handle: Handle,
    /// What a click on it keeps, for a tool that holds what it selects.
    pub(crate) held: Option<Held>,
}

/// A candidate's highlight, decoded off the update loop: its width, height and RGBA bytes.
pub(crate) type Highlight = (u32, u32, Vec<u8>);

/// A pick's coverage preview over its bounds, kept for the hit test of a selected object. At most
/// the picker's preview cap, 65,536 codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Coverage {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) codes: Vec<u8>,
}

impl Coverage {
    /// Whether the content position `at` lies inside the object, its coverage spread over
    /// `bounds`. `O(1)`.
    pub(crate) fn covers(&self, bounds: [f64; 4], at: [f64; 2]) -> bool {
        let [x0, y0, x1, y1] = bounds;
        if !(x1 > x0 && y1 > y0) || at[0] < x0 || at[0] > x1 || at[1] < y0 || at[1] > y1 {
            return false;
        }
        let column = ((at[0] - x0) / (x1 - x0) * self.width as f64) as usize;
        let row = ((at[1] - y0) / (y1 - y0) * self.height as f64) as usize;
        let column = column.min(self.width.saturating_sub(1));
        let row = row.min(self.height.saturating_sub(1));
        self.codes
            .get(row * self.width + column)
            .is_some_and(|code| *code >= INSIDE)
    }
}

/// What a tool that holds its selection keeps of a pick: its bounds, its selected tint and the
/// coverage a `⌥`-click is hit-tested against.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Held {
    pub(crate) bounds: [f64; 4],
    pub(crate) tint: Handle,
    pub(crate) coverage: Arc<Coverage>,
}

/// [`Held`] before its tint is wrapped in an image handle: what the blocking pool hands back.
#[derive(Clone, Debug)]
pub(crate) struct Tinted {
    pub(crate) tint: Highlight,
    pub(crate) coverage: Arc<Coverage>,
}

/// One point pick's answer, decoded off the update loop: the overlay of its chosen candidate, or
/// none when it chose nothing.
#[derive(Clone, Debug)]
pub(crate) struct Picked {
    pub(crate) decision: String,
    pub(crate) bounds: Option<[f64; 4]>,
    pub(crate) image: Option<Highlight>,
    /// The selected tint and coverage, when the tool holds what it selects.
    pub(crate) tinted: Option<Tinted>,
}

/// The answer of `query.pick-object` as the overlay it draws: the first chosen candidate's bounds
/// and its coverage, washed and outlined, and, for a tool that holds what it selects (`held`), the
/// same coverage tinted as a selection. `O(preview pixels)`.
pub(crate) fn picked(answer: &Value, held: bool) -> Result<Picked, String> {
    let decision = answer["decision"].as_str().unwrap_or("nothing").to_owned();
    let chosen = answer["chosen"][0].as_u64().map(|index| index as usize);
    let Some(candidate) = chosen.map(|index| &answer["candidates"][index]) else {
        return Ok(Picked {
            decision,
            bounds: None,
            image: None,
            tinted: None,
        });
    };
    let (bounds, image, tinted) = candidate_image(candidate, held)?;
    Ok(Picked {
        decision,
        bounds,
        image,
        tinted,
    })
}

/// A candidate's bounds, the highlight drawn from its preview and, when asked, its selected tint.
type CandidateImage = (Option<[f64; 4]>, Option<Highlight>, Option<Tinted>);

/// One candidate's bounds and, when its preview was sent, the highlight drawn from it and (`held`)
/// its selected tint and coverage.
fn candidate_image(candidate: &Value, held: bool) -> Result<CandidateImage, String> {
    let bounds = candidate["bounds"].as_array().and_then(|bounds| {
        let values: Vec<f64> = bounds.iter().filter_map(Value::as_f64).collect();
        <[f64; 4]>::try_from(values).ok()
    });
    let preview = &candidate["preview"];
    let (Some(width), Some(height), Some(codes)) = (
        preview["width"].as_u64(),
        preview["height"].as_u64(),
        preview["codes"].as_str(),
    ) else {
        return Ok((bounds, None, None));
    };
    let codes = crate::app::remove::decode_base64(codes)?;
    if codes.len() as u64 != width * height {
        return Err(format!(
            "a {width} x {height} preview carried {} codes",
            codes.len()
        ));
    }
    let (width, height) = (width as usize, height as usize);
    let image = (
        width as u32,
        height as u32,
        highlight(&codes, width, height),
    );
    let tinted = held.then(|| Tinted {
        tint: (width as u32, height as u32, tint(&codes, width, height)),
        coverage: Arc::new(Coverage {
            width,
            height,
            codes,
        }),
    });
    Ok((bounds, Some(image), tinted))
}

/// A coverage preview of `width × height` codes as the highlight's RGBA: each pixel washed by its
/// coverage, an inside pixel with an outside 4-neighbour (or on the preview's edge) drawn as the
/// outline, and an outside pixel beside the outline as the under-stroke.
pub(crate) fn highlight(codes: &[u8], width: usize, height: usize) -> Vec<u8> {
    painted(codes, width, height, WASH, WASH_ALPHA)
}

/// A coverage preview as a selected object's RGBA: [`highlight`]'s outline and under-stroke around
/// Remove's selection tint in place of the white wash.
pub(crate) fn tint(codes: &[u8], width: usize, height: usize) -> Vec<u8> {
    painted(codes, width, height, TINT, TINT_ALPHA)
}

/// A coverage preview washed in `wash` at `alpha` of its coverage, outlined white over a dark
/// under-stroke.
fn painted(codes: &[u8], width: usize, height: usize, wash: [u8; 3], alpha: f32) -> Vec<u8> {
    let inside = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < width
            && (y as usize) < height
            && codes[y as usize * width + x as usize] >= INSIDE
    };
    let edge = |x: isize, y: isize| {
        inside(x, y)
            && [(-1, 0), (1, 0), (0, -1), (0, 1)]
                .iter()
                .any(|(dx, dy)| !inside(x + dx, y + dy))
    };
    let mut rgba = Vec::with_capacity(codes.len() * 4);
    for y in 0..height as isize {
        for x in 0..width as isize {
            let pixel = if edge(x, y) {
                OUTLINE
            } else if !inside(x, y)
                && [(-1, 0), (1, 0), (0, -1), (0, 1)]
                    .iter()
                    .any(|(dx, dy)| edge(x + dx, y + dy))
            {
                UNDER_STROKE
            } else {
                let code = codes[y as usize * width + x as usize];
                let alpha = (f32::from(code) * alpha).round() as u8;
                [wash[0], wash[1], wash[2], alpha]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}

/// Whether a refusal names the analysis the pick needs.
pub(crate) fn needs_analysis(error: &CallError) -> bool {
    error.code == "not-ready"
        && error
            .data
            .as_ref()
            .and_then(|data| data["requirements"].as_array())
            .is_some_and(|list| list.iter().any(|r| r["kind"] == "analysis"))
}

// ---- the requests ------------------------------------------------------------------------------

/// The `query.pick-object` a hover at `point` sends: what an API client sends for it.
pub(crate) fn point_query(asset: &AssetId, point: [f64; 2]) -> Value {
    json!({"asset_id": asset, "point": [point]})
}

/// The `query.pick-object` a loose stroke sends on release: every candidate's preview, so pins
/// can be drawn for each.
pub(crate) fn stroke_query(asset: &AssetId, stroke: &[[f64; 2]], radius: f64) -> Value {
    json!({"asset_id": asset, "stroke": stroke, "radius": radius, "previews": "all"})
}

/// The `task.select-object` one prompt sends, under `mutation`.
pub(crate) fn select_request(asset: &AssetId, prompt: &ObjectPrompt, mutation: Value) -> Value {
    let mut request = serde_json::Map::new();
    request.insert("asset_id".into(), json!(asset));
    request.extend(prompt.request_fields());
    request.insert("mutation".into(), mutation);
    Value::Object(request)
}

/// The `query.pick-object` that asks again for the object a stored prompt picked: a point's, or
/// a stroke's with every preview, as the gesture that picked it asked.
pub(crate) fn prompt_query(asset: &AssetId, prompt: &ObjectPrompt) -> Value {
    match prompt {
        ObjectPrompt::Point { point, .. } => point_query(asset, *point),
        ObjectPrompt::Stroke { stroke, radius, .. } => stroke_query(asset, stroke, *radius),
    }
}

/// The prompt a click at `point` selects with.
pub(crate) fn click_prompt(point: [f64; 2]) -> ObjectPrompt {
    ObjectPrompt::Point {
        point,
        candidate: None,
    }
}

/// The prompt a loose stroke selects with: the decision's one object without a candidate, or one
/// of several by its position.
pub(crate) fn stroke_prompt(
    stroke: &[[f64; 2]],
    radius: f64,
    candidate: Option<u32>,
) -> ObjectPrompt {
    ObjectPrompt::Stroke {
        stroke: stroke.to_vec(),
        radius,
        candidate,
    }
}

// ---- the press -----------------------------------------------------------------------------------

/// What one press on the photograph was, once it came up.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Gesture {
    /// It came up within [`crate::range_pick::CLICK_TRAVEL`] of where it went down: a click there.
    Click([f64; 2]),
    /// It travelled further: the loose stroke it drew, thinned and bounded for the picker.
    Loose(Vec<[f64; 2]>),
}

/// One press on the photograph, from the button going down to it coming up.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Press {
    /// Where it went down, in canvas logical points, which the click threshold is measured in.
    pub(crate) view: (f32, f32),
    /// Every content position it passed through, the first where it went down.
    pub(crate) points: Vec<[f64; 2]>,
    /// It has travelled [`crate::range_pick::CLICK_TRAVEL`] or more, so it is a loose drag for the rest of its life.
    pub(crate) dragging: bool,
    /// The modifiers held as it went down: `⇧` adds and `⌥` takes away, where the tool says so.
    pub(crate) modifiers: Modifiers,
}

impl Press {
    pub(crate) fn pressed(view: (f32, f32), at: [f64; 2], modifiers: Modifiers) -> Self {
        Self {
            view,
            points: vec![at],
            dragging: false,
            modifiers,
        }
    }

    pub(crate) fn moved(&mut self, view: (f32, f32), at: [f64; 2]) {
        self.dragging |= crate::range_pick::travelled(self.view, view);
        if self.points.last() != Some(&at) {
            self.points.push(at);
        }
    }

    /// What the press was when it came up at `at`.
    pub(crate) fn released(mut self, view: (f32, f32), at: [f64; 2], radius: f64) -> Gesture {
        self.moved(view, at);
        if self.dragging {
            Gesture::Loose(thinned(&self.points, radius))
        } else {
            Gesture::Click(self.points[0])
        }
    }
}

/// A stroke's positions as the picker takes them: each at least a quarter of the radius from the
/// one kept before it, the last always kept, and no more than a stored prompt holds.
pub(crate) fn thinned(points: &[[f64; 2]], radius: f64) -> Vec<[f64; 2]> {
    let spacing = (radius / 4.0).max(1e-4);
    let mut kept: Vec<[f64; 2]> = Vec::with_capacity(points.len().min(OBJECT_PROMPT_POINTS_MAX));
    for point in points {
        let far = kept
            .last()
            .is_none_or(|last| (point[0] - last[0]).hypot(point[1] - last[1]) >= spacing);
        if far {
            kept.push(*point);
        }
    }
    if let (Some(last), Some(end)) = (kept.last().copied(), points.last())
        && last != *end
    {
        kept.push(*end);
    }
    if kept.len() > OBJECT_PROMPT_POINTS_MAX {
        let step = kept.len().div_ceil(OBJECT_PROMPT_POINTS_MAX);
        let end = *kept.last().expect("a stroke has a position");
        kept = kept.iter().step_by(step).copied().collect();
        if kept.last() != Some(&end) {
            if kept.len() == OBJECT_PROMPT_POINTS_MAX {
                kept.pop();
            }
            kept.push(end);
        }
    }
    kept
}

// ---- a loose stroke's answer ---------------------------------------------------------------------

/// The picker's decision about a loose stroke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    One,
    Several,
    Nothing,
}

/// One candidate a loose stroke found.
#[derive(Clone, Debug)]
pub(crate) struct FoundCandidate {
    /// Its position in the answer, which `task.select-object`'s `candidate` names.
    pub(crate) position: u32,
    pub(crate) bounds: [f64; 4],
    /// Its highlight, decoded off the update loop.
    pub(crate) image: Option<Highlight>,
    /// Its selected tint and coverage, when the tool holds what it selects.
    pub(crate) tinted: Option<Tinted>,
}

/// A loose stroke's answer, decoded off the update loop.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    pub(crate) decision: Decision,
    /// The positions the picker chose, in its order.
    pub(crate) chosen: Vec<u32>,
    /// The chosen candidates, each with its bounds and highlight.
    pub(crate) candidates: Vec<FoundCandidate>,
}

/// The answer of `query.pick-object` for a stroke, with each candidate's selected tint and coverage
/// for a tool that holds what it selects (`held`). `O(preview pixels)`.
pub(crate) fn found(answer: &Value, held: bool) -> Result<Found, String> {
    let decision = match answer["decision"].as_str() {
        Some("one") => Decision::One,
        Some("several") => Decision::Several,
        _ => Decision::Nothing,
    };
    let chosen: Vec<u32> = answer["chosen"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(Value::as_u64)
                .filter_map(|position| u32::try_from(position).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut candidates = Vec::with_capacity(chosen.len());
    for &position in &chosen {
        let candidate = &answer["candidates"][position as usize];
        let (bounds, image, tinted) = candidate_image(candidate, held)?;
        let Some(bounds) = bounds else {
            return Err(format!("candidate {position} has no bounds"));
        };
        candidates.push(FoundCandidate {
            position,
            bounds,
            image,
            tinted,
        });
    }
    let decision = if candidates.is_empty() {
        Decision::Nothing
    } else {
        decision
    };
    Ok(Found {
        decision,
        chosen,
        candidates,
    })
}

/// One candidate's numbered pin while a loose stroke found several.
#[derive(Clone, Debug)]
pub(crate) struct Pin {
    pub(crate) position: u32,
    pub(crate) bounds: [f64; 4],
    pub(crate) chosen: bool,
    pub(crate) handle: Option<Handle>,
    /// What a tool that holds its selection keeps of the candidate once it is used.
    pub(crate) held: Option<Held>,
}

impl Pin {
    /// Where the pin is drawn: the centre of its candidate's bounds.
    pub(crate) fn centre(&self) -> [f64; 2] {
        let [x0, y0, x1, y1] = self.bounds;
        [(x0 + x1) / 2.0, (y0 + y1) / 2.0]
    }
}

/// The pins a loose stroke left: one per candidate it found, none chosen yet.
#[derive(Clone, Debug)]
pub(crate) struct Pins {
    pub(crate) stroke: Vec<[f64; 2]>,
    pub(crate) radius: f64,
    pub(crate) pins: Vec<Pin>,
    /// The stroke was drawn with `⇧`: what it selects adds to the selection.
    pub(crate) adds: bool,
}

impl Pins {
    /// A pin pressed: chosen, or not chosen again.
    pub(crate) fn toggle(&mut self, index: usize) {
        if let Some(pin) = self.pins.get_mut(index) {
            pin.chosen = !pin.chosen;
        }
    }

    pub(crate) fn chosen(&self) -> usize {
        self.pins.iter().filter(|pin| pin.chosen).count()
    }

    /// The prompt each chosen pin selects with, in pin order.
    pub(crate) fn prompts(&self) -> Vec<ObjectPrompt> {
        self.used().into_iter().map(|(prompt, _)| prompt).collect()
    }

    /// Each chosen pin with the prompt it selects with, in pin order.
    pub(crate) fn used(&self) -> Vec<(ObjectPrompt, &Pin)> {
        self.pins
            .iter()
            .filter(|pin| pin.chosen)
            .map(|pin| {
                let prompt = stroke_prompt(&self.stroke, self.radius, Some(pin.position));
                (prompt, pin)
            })
            .collect()
    }
}

// ---- the session ---------------------------------------------------------------------------------

/// The tool that is picking, which decides what a selection becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickUse {
    /// Masks › Object: each selection is one `mask.create-model` or `mask.add-model`.
    Mask,
    /// Corrections › Object: each selection is one object the fill removes, held by the band until
    /// Generate names it in `object-artifacts`.
    Object,
}

impl PickUse {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Mask => "mask",
            Self::Object => "object",
        }
    }

    /// The tool holds what it selects, so its picks are decoded with a selected tint and the
    /// coverage a `⌥`-click is hit-tested against.
    pub(crate) fn holds(self) -> bool {
        matches!(self, Self::Object)
    }
}

/// One tool's picking session: armed on one photograph until the tool leaves it. Per-client view
/// state; nothing here is sent on its own.
#[derive(Clone, Debug)]
pub(crate) struct ObjectPick {
    pub(crate) purpose: PickUse,
    /// The photograph it was armed on: another photograph ends it.
    pub(crate) asset: AssetId,
    /// A loose stroke's radius, in content heights.
    pub(crate) radius: f64,
    /// The content map a press is placed through, as `render.transform` answered it, the entry it
    /// was asked for, and the request in flight.
    pub(crate) map: Option<ContentMap>,
    pub(crate) entry: Option<EntryId>,
    pub(crate) request: Option<GestureId>,
    /// The analysis in flight, by photograph and revision.
    pub(crate) analysing: Option<(AssetId, u64)>,
    /// The photograph and revision last analysed, whatever the analysis answered: not analysed
    /// again until it moves.
    pub(crate) analysed: Option<(AssetId, u64)>,
    /// Analyses run, and why the last one failed.
    pub(crate) analyses: u64,
    pub(crate) analysis_error: Option<String>,
    /// The models asked to load on entering.
    pub(crate) loads: Vec<String>,
    /// The hover position wanted next, and the one whose pick is in flight.
    pub(crate) want: Option<[f64; 2]>,
    pub(crate) asking: Option<[f64; 2]>,
    /// The hover position last sent and the revision it was sent for: not sent again until the
    /// pointer or the photograph moves, or an analysis lands.
    pub(crate) asked_for: Option<([f64; 2], u64)>,
    /// What the canvas draws of the object under the pointer.
    pub(crate) overlay: Option<HoverOverlay>,
    /// Point picks answered.
    pub(crate) answered: u64,
    /// A press on the photograph that has not come up.
    pub(crate) press: Option<Press>,
    /// A loose stroke whose pick waits for its turn (`false`) or is in flight (`true`).
    pub(crate) finding: Option<(Vec<[f64; 2]>, bool)>,
    /// The stroke was drawn with `⇧`: what it finds adds to the selection.
    pub(crate) adds: bool,
    /// The pins of a stroke that found several objects.
    pub(crate) pins: Option<Pins>,
    /// A stroke that found nothing, still tinted.
    pub(crate) nothing: Option<Vec<[f64; 2]>>,
    /// The last pick this session sent: what an API client sends for the same gesture.
    pub(crate) last_request: Option<Value>,
}

impl ObjectPick {
    pub(crate) fn new(purpose: PickUse, asset: AssetId, radius: f64) -> Self {
        Self {
            purpose,
            asset,
            radius,
            map: None,
            entry: None,
            request: None,
            analysing: None,
            analysed: None,
            analyses: 0,
            analysis_error: None,
            loads: Vec::new(),
            want: None,
            asking: None,
            asked_for: None,
            overlay: None,
            answered: 0,
            press: None,
            finding: None,
            adds: false,
            pins: None,
            nothing: None,
            last_request: None,
        }
    }

    /// A pick is in flight: a hover's or a stroke's. One at a time.
    pub(crate) fn picking(&self) -> bool {
        self.asking.is_some() || self.finding.as_ref().is_some_and(|(_, sent)| *sent)
    }

    /// What the tool says while the photograph is not analysed yet.
    pub(crate) fn finding_objects(&self) -> bool {
        self.analysing.is_some()
    }

    /// Correlated evidence: the session as a captured frame records it.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "purpose": self.purpose.as_str(),
            "asset_id": self.asset,
            "mapped": self.map.is_some(),
            "analysing": self.analysing.is_some(),
            "analyses": self.analyses,
            "analysis_error": self.analysis_error,
            "loads": self.loads,
            "asking": self.asking,
            "answered": self.answered,
            "overlay": self.overlay.as_ref().map(|overlay| json!({"bounds": overlay.bounds})),
            "pressed": self.press.as_ref().map(|press| json!({"dragging": press.dragging, "points": press.points.len()})),
            "finding": self.finding.as_ref().map(|(stroke, sent)| json!({"points": stroke.len(), "sent": sent})),
            "pins": self.pins.as_ref().map(|pins| pins.pins.iter().enumerate().map(|(index, pin)| json!({
                "number": index + 1,
                "candidate": pin.position,
                "bounds": pin.bounds,
                "chosen": pin.chosen,
            })).collect::<Vec<_>>()),
            "nothing": self.nothing.as_ref().map(Vec::len),
            "last_request": self.last_request.as_ref().map(|params| json!({"method": PICK, "params": params})),
        })
    }
}

/// What a published selection becomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SelectFor {
    /// A pick of the armed session, for the tool picking.
    Pick(PickUse),
    /// A model component's Recompute: its stored prompt selected again, then `mask.set-model`.
    Recompute {
        mask: MaskId,
        component: ComponentId,
    },
    /// One object of Corrections › Object's selection, by the ticket the band numbered it with.
    Object { ticket: u64 },
}

impl SelectFor {
    /// The picking tool this selection is for, which leaving drops it with.
    pub(crate) fn pick_use(&self) -> Option<PickUse> {
        match self {
            Self::Pick(purpose) => Some(*purpose),
            Self::Object { .. } => Some(PickUse::Object),
            Self::Recompute { .. } => None,
        }
    }

    fn summary(&self) -> Value {
        match self {
            Self::Pick(purpose) => json!({"pick": purpose.as_str()}),
            Self::Recompute { mask, component } => {
                json!({"recompute": {"mask": mask, "component": component}})
            }
            Self::Object { ticket } => json!({"object": ticket}),
        }
    }
}

/// The selections this desktop asked for: one running, the rest waiting their turn, and the one
/// answered whose commit waits for the desktop's request slot.
#[derive(Clone, Debug, Default)]
pub(crate) struct ObjectSelects {
    /// The selection running, and the request it sent.
    pub(crate) running: Option<(SelectFor, Value)>,
    /// Selections waiting, oldest first, at most [`SELECTION_LIMIT`].
    pub(crate) queued: VecDeque<(SelectFor, AssetId, ObjectPrompt)>,
    /// A selection answered whose commit has not gone yet.
    pub(crate) answered: Option<(SelectFor, Value)>,
    /// The last `task.select-object` this desktop sent.
    pub(crate) last_request: Option<Value>,
    /// Selections sent and answered, for the evidence log and tests.
    pub(crate) sent: u64,
    pub(crate) finished: u64,
}

/// No selections: what a view built without an editor reads.
#[cfg(test)]
pub(crate) static NO_SELECTS: ObjectSelects = ObjectSelects {
    running: None,
    queued: VecDeque::new(),
    answered: None,
    last_request: None,
    sent: 0,
    finished: 0,
};

impl ObjectSelects {
    /// Nothing is running, waiting or waiting to commit.
    pub(crate) fn idle(&self) -> bool {
        self.running.is_none() && self.queued.is_empty() && self.answered.is_none()
    }

    /// Room for one more selection.
    pub(crate) fn room(&self) -> bool {
        self.queued.len() + usize::from(self.running.is_some()) < SELECTION_LIMIT
    }

    /// Drop every waiting selection `for_` names: its tool left.
    pub(crate) fn drop_for(&mut self, purpose: PickUse) -> usize {
        let before = self.queued.len();
        self.queued
            .retain(|(waiting, _, _)| waiting.pick_use() != Some(purpose));
        if self
            .answered
            .as_ref()
            .is_some_and(|(waiting, _)| waiting.pick_use() == Some(purpose))
        {
            self.answered = None;
        }
        before - self.queued.len()
    }

    /// Drop one object's waiting selection: it left the selection before its turn.
    pub(crate) fn drop_object(&mut self, ticket: u64) {
        let object = SelectFor::Object { ticket };
        self.queued.retain(|(waiting, _, _)| waiting != &object);
        if self
            .answered
            .as_ref()
            .is_some_and(|(waiting, _)| waiting == &object)
        {
            self.answered = None;
        }
    }

    pub(crate) fn summary(&self) -> Value {
        json!({
            "running": self.running.as_ref().map(|(purpose, _)| purpose.summary()),
            "queued": self.queued.iter().map(|(purpose, _, _)| purpose.summary()).collect::<Vec<_>>(),
            "answered": self.answered.as_ref().map(|(purpose, _)| purpose.summary()),
            "sent": self.sent,
            "finished": self.finished,
            "last_request": self.last_request.as_ref().map(|params| json!({"method": SELECT, "params": params})),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A press that comes up within 3 pt of where it went down is a click where it went down; one
    /// that travels 3 pt or more is a loose stroke through every position it passed, and stays one
    /// even if it comes back.
    #[test]
    fn object_pick_a_press_is_a_click_until_it_travels_three_points() {
        let mut press = Press::pressed((10.0, 10.0), [0.5, 0.5], Modifiers::default());
        press.moved((11.0, 11.0), [0.501, 0.501]);
        assert_eq!(
            press.released((12.0, 10.5), [0.502, 0.5], 0.02),
            Gesture::Click([0.5, 0.5])
        );
        let mut press = Press::pressed((10.0, 10.0), [0.5, 0.5], Modifiers::default());
        press.moved((40.0, 10.0), [0.6, 0.5]);
        let Gesture::Loose(stroke) = press.released((10.0, 10.0), [0.5, 0.5], 0.02) else {
            panic!("a press that travelled is a loose stroke")
        };
        assert_eq!(stroke, vec![[0.5, 0.5], [0.6, 0.5], [0.5, 0.5]]);
    }

    /// A stroke is thinned to positions a quarter of its radius apart, keeps its end, and never
    /// holds more positions than a stored prompt may.
    #[test]
    fn object_pick_a_loose_stroke_is_thinned_and_bounded() {
        let dense: Vec<[f64; 2]> = (0..=100)
            .map(|i| [0.4 + f64::from(i) * 0.0001, 0.5])
            .collect();
        let thin = thinned(&dense, 0.02);
        assert_eq!(thin.first(), Some(&[0.4, 0.5]));
        assert_eq!(thin.last(), dense.last());
        assert!(thin.len() < 4, "{thin:?}");
        let long: Vec<[f64; 2]> = (0..5000).map(|i| [f64::from(i) * 0.01, 0.5]).collect();
        let bounded = thinned(&long, 0.0004);
        assert!(
            bounded.len() <= OBJECT_PROMPT_POINTS_MAX,
            "{}",
            bounded.len()
        );
        assert_eq!(bounded.last(), long.last());
    }

    /// Each gesture's request is the one an API client sends: a hover's point, a stroke's
    /// positions with every preview, and a selection naming the prompt and the candidate.
    #[test]
    fn object_pick_requests_are_the_api_clients() {
        let asset = AssetId::new();
        assert_eq!(
            point_query(&asset, [0.25, 0.5]),
            json!({"asset_id": asset, "point": [[0.25, 0.5]]})
        );
        let stroke = [[0.1, 0.2], [0.3, 0.4]];
        assert_eq!(
            stroke_query(&asset, &stroke, 0.03),
            json!({"asset_id": asset, "stroke": stroke, "radius": 0.03, "previews": "all"})
        );
        let mutation = json!({"request_id": "r", "actor": "a"});
        assert_eq!(
            select_request(&asset, &click_prompt([0.25, 0.5]), mutation.clone()),
            json!({"asset_id": asset, "point": [[0.25, 0.5]], "mutation": mutation})
        );
        assert_eq!(
            select_request(
                &asset,
                &stroke_prompt(&stroke, 0.03, Some(2)),
                mutation.clone()
            ),
            json!({"asset_id": asset, "stroke": stroke, "radius": 0.03, "candidate": 2, "mutation": mutation})
        );
        // A stored prompt asks the picker again as the gesture that made it did.
        assert_eq!(
            prompt_query(&asset, &stroke_prompt(&stroke, 0.03, Some(2))),
            stroke_query(&asset, &stroke, 0.03)
        );
    }

    /// A stroke's answer becomes one pin per candidate the picker chose, each selecting with the
    /// stroke and its own position; an answer that chose nothing is Nothing.
    #[test]
    fn object_pick_several_candidates_become_pins_that_select_by_position() {
        let preview = json!({"width": 2, "height": 1, "codes": "/wA="});
        let answer = json!({
            "decision": "several",
            "chosen": [0, 2],
            "candidates": [
                {"candidate": 0, "bounds": [0.0, 0.0, 0.5, 0.5], "preview": preview},
                {"candidate": 1, "bounds": [0.5, 0.0, 1.0, 0.5]},
                {"candidate": 2, "bounds": [0.0, 0.5, 0.5, 1.0], "preview": preview},
            ],
        });
        let found = found(&answer, true).unwrap();
        assert_eq!(found.decision, Decision::Several);
        assert_eq!(found.chosen, vec![0, 2]);
        assert_eq!(found.candidates.len(), 2);
        assert!(found.candidates[1].image.is_some());
        let coverage = found.candidates[1]
            .tinted
            .as_ref()
            .unwrap()
            .coverage
            .clone();
        // The left half of the bounds is inside the object, the right half is not.
        assert!(coverage.covers(found.candidates[1].bounds, [0.1, 0.7]));
        assert!(!coverage.covers(found.candidates[1].bounds, [0.4, 0.7]));
        assert!(!coverage.covers(found.candidates[1].bounds, [0.6, 0.7]));
        let mut pins = Pins {
            stroke: vec![[0.2, 0.2], [0.3, 0.8]],
            radius: 0.03,
            pins: found
                .candidates
                .iter()
                .map(|candidate| Pin {
                    position: candidate.position,
                    bounds: candidate.bounds,
                    chosen: false,
                    handle: None,
                    held: None,
                })
                .collect(),
            adds: false,
        };
        assert_eq!(pins.pins[1].centre(), [0.25, 0.75]);
        pins.toggle(1);
        assert_eq!(pins.chosen(), 1);
        assert_eq!(
            pins.prompts(),
            vec![stroke_prompt(&[[0.2, 0.2], [0.3, 0.8]], 0.03, Some(2))]
        );
        pins.toggle(1);
        assert!(pins.prompts().is_empty());
        let nothing = found_of(json!({"decision": "nothing", "chosen": [], "candidates": []}));
        assert_eq!(nothing.decision, Decision::Nothing);
    }

    fn found_of(answer: Value) -> Found {
        found(&answer, false).unwrap()
    }

    #[test]
    fn object_pick_a_hovered_object_is_outlined_whatever_its_colour() {
        // A 4 x 4 square of full coverage in a 6 x 6 preview.
        let mut codes = vec![0u8; 36];
        for y in 1..5 {
            for x in 1..5 {
                codes[y * 6 + x] = 255;
            }
        }
        let at =
            |rgba: &[u8], x: usize, y: usize| rgba[(y * 6 + x) * 4..(y * 6 + x) * 4 + 4].to_vec();
        let rgba = highlight(&codes, 6, 6);
        // The edge of the object is opaque white, whatever the photograph's colour under it.
        assert_eq!(at(&rgba, 1, 1), [255, 255, 255, 255]);
        assert_eq!(at(&rgba, 4, 2), [255, 255, 255, 255]);
        // Inside, a white wash that takes no hue from the object.
        assert_eq!(at(&rgba, 2, 2), [255, 255, 255, 89]);
        // Outside the outline a translucent dark under-stroke, and nothing beyond it.
        assert_eq!(at(&rgba, 0, 2), [0, 0, 0, 115]);
        assert_eq!(at(&rgba, 0, 0)[3], 0);
        // A selected object keeps the outline around Remove's selection tint.
        let tinted = tint(&codes, 6, 6);
        assert_eq!(at(&tinted, 1, 1), [255, 255, 255, 255]);
        assert_eq!(at(&tinted, 2, 2), [255, 69, 69, 82]);
        assert_eq!(at(&tinted, 0, 2), [0, 0, 0, 115]);
    }

    /// The selection queue holds at most the selection limit, and a tool leaving drops what waits
    /// for it and nothing else.
    #[test]
    fn object_pick_selects_wait_in_a_bounded_queue_dropped_with_their_tool() {
        let asset = AssetId::new();
        let mut selects = ObjectSelects::default();
        assert!(selects.idle());
        for _ in 0..SELECTION_LIMIT {
            assert!(selects.room());
            selects.queued.push_back((
                SelectFor::Pick(PickUse::Mask),
                asset.clone(),
                click_prompt([0.5, 0.5]),
            ));
        }
        assert!(!selects.room());
        let (mask, component) = (MaskId::new(), ComponentId::new());
        selects.queued.pop_back();
        selects.queued.push_back((
            SelectFor::Recompute { mask, component },
            asset.clone(),
            click_prompt([0.5, 0.5]),
        ));
        assert_eq!(selects.drop_for(PickUse::Mask), SELECTION_LIMIT - 1);
        assert_eq!(
            selects.queued.len(),
            1,
            "a Recompute is not the tool's to drop"
        );
    }
}
