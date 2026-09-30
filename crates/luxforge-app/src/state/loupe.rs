//! The loupe's view model ([catalog design](../../../../docs/design/catalog.md#browsing-at-speed),
//! TASK-020): the active frame of Select's view fitted to the screen, the bar naming its moment,
//! its moment's numbered frames under it, the 100% focus check and compare, and the pure rules
//! behind them — where each key moves, what the look-ahead wants, which frames compare shows, where
//! the focus check's rectangle is, and where picking a burst frame moves on to (P7). Like every
//! view model it names no framework type.
//!
//! **Seam.** This file, `app/loupe.rs` (the update, with `app/loupe_frames.rs` and
//! `app/loupe_region.rs`), `app/message/loupe.rs` and `view/loupe.rs` are the loupe's own modules.
//! Select hands the loupe what it needs through [`subject`]: the view's revision and count, the
//! active item's position and the moment it sits in. The rows it names are Select's rows window,
//! which the loupe keeps near the active frame by scrolling the grid under it.
//!
//! **What is drawn is what is named.** The app mirrors what it holds into [`LoupeState::held`]
//! after every message: for each frame the loupe may draw, and each photograph in the strip, the
//! picture it holds for that frame's own item (its preview's key, origin and size), and the 100%
//! region with the item it was cut from. The model draws a frame's picture, or a strip frame's
//! thumbnail, only when its item is the item of the row at the frame's position, and the view looks
//! a handle up by that item and key alone, so a picture never appears under another frame's name.
use super::select::{SelectState, exposure_text, thousands};
use crate::layout::canvas_logical;
use luxforge_core::catalog_types::{
    BrowseSession, CaptureTime, Dimensions, MomentKind, PixelRect, PreviewItem, PreviewOrigin,
    RowItem, ViewRow, ViewSummary,
};
use std::ops::Range;

/// Frames the look-ahead decodes past the active one in the direction of travel, before the next
/// moment's first frame.
pub(crate) const LOOK_AHEAD_FRAMES: u32 = 3;
/// The most frames compare shows side by side.
pub(crate) const COMPARE_FRAMES: u32 = 4;

// -- Geometry, in points, matched to the burst board -------------------------------------------------

/// Above the info bar, under the title bar's rule.
pub(crate) const TOP_INSET: f32 = 11.0;
/// The info bar: the draft bar's floating surface (`theme::DRAFT_BAR_HEIGHT`).
pub(crate) const INFO_BAR_HEIGHT: f32 = 34.0;
/// Between the info bar and the photograph.
pub(crate) const BAR_GAP: f32 = 10.0;
/// Between the photograph and the frame strip.
pub(crate) const STRIP_GAP: f32 = 13.0;
/// The frame strip: one moment frame's height (`theme::MOMENT_FRAME_HEIGHT`).
pub(crate) const STRIP_HEIGHT: f32 = 84.0;
/// Between the strip and the key hints.
pub(crate) const HINTS_GAP: f32 = 10.0;
/// The key hints' line.
pub(crate) const HINTS_HEIGHT: f32 = 18.0;
/// Under the key hints, over the status bar's rule.
pub(crate) const BOTTOM_INSET: f32 = 7.0;
/// The least room either side of the photograph.
pub(crate) const SIDE_INSET: f32 = 24.0;
/// Between compare's cells.
pub(crate) const COMPARE_GAP: f32 = 8.0;
/// The strip's frame width and spacing, and a chevron's width (`theme::MOMENT_FRAME_WIDTH`,
/// `theme::MOMENT_STRIP_SPACING`, `theme::ICON_BUTTON_SIZE`).
pub(crate) const STRIP_FRAME_WIDTH: f32 = 124.0;
pub(crate) const STRIP_SPACING: f32 = 6.0;
pub(crate) const STRIP_CHEVRON: f32 = 26.0;
/// The box a strip frame's picture is fitted into (`theme::MOMENT_IMAGE_WIDTH`,
/// `theme::MOMENT_IMAGE_HEIGHT`): what a photograph's thumbnail is decoded for.
pub(crate) const STRIP_IMAGE: (f32, f32) = (116.0, 76.0);
/// The 100% inset's region, in points (`theme::FOCUS_INSET_WIDTH`, `theme::FOCUS_REGION_HEIGHT`):
/// at 100% it shows this many points' worth of pixels at the display's scale.
pub(crate) const FOCUS_REGION: (f32, f32) = (308.0, 209.0);
/// The whole inset, region and footer (`theme::FOCUS_FOOTER_HEIGHT`).
pub(crate) const FOCUS_INSET_HEIGHT: f32 = 209.0 + 24.0;
/// The inset's inset from the photograph's lower right corner.
pub(crate) const FOCUS_INSET_MARGIN: f32 = 16.0;

/// A rectangle of the window, in points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Area {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl Area {
    /// `[x, y, width, height]`, for evidence.
    pub(crate) fn to_array(self) -> [f32; 4] {
        [self.x, self.y, self.width, self.height]
    }
}

/// Where the loupe draws its photograph: Select's centre between the panels and the bars, less the
/// info bar above and the strip and key hints below.
pub(crate) fn frame_area(window: (f32, f32), sources_open: bool, info_open: bool) -> Area {
    let [left, top, right, bottom] = canvas_logical(window, sources_open, info_open);
    let above = TOP_INSET + INFO_BAR_HEIGHT + BAR_GAP;
    let below = STRIP_GAP + STRIP_HEIGHT + HINTS_GAP + HINTS_HEIGHT + BOTTOM_INSET;
    Area {
        x: left + SIDE_INSET,
        y: top + above,
        width: (right - left - 2.0 * SIDE_INSET).max(1.0),
        height: (bottom - top - above - below).max(1.0),
    }
}

/// A `width` × `height` picture fitted inside `area` and centred: scaled to fill it in one
/// direction, up or down, as the loupe fits a frame to the screen.
pub(crate) fn fitted(area: Area, width: u32, height: u32) -> Area {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let scale = (area.width / w).min(area.height / h);
    let (fw, fh) = ((w * scale).floor().max(1.0), (h * scale).floor().max(1.0));
    Area {
        x: area.x + ((area.width - fw) / 2.0).floor(),
        y: area.y + ((area.height - fh) / 2.0).floor(),
        width: fw,
        height: fh,
    }
}

/// `count` equal cells side by side across `area`, [`COMPARE_GAP`] apart.
pub(crate) fn compare_cells(area: Area, count: u32) -> Vec<Area> {
    let count = count.max(1);
    let gaps = COMPARE_GAP * (count - 1) as f32;
    let width = ((area.width - gaps) / count as f32).floor().max(1.0);
    (0..count)
        .map(|at| Area {
            x: area.x + at as f32 * (width + COMPARE_GAP),
            y: area.y,
            width,
            height: area.height,
        })
        .collect()
}

/// How many strip frames fit across Select's centre between its chevrons.
pub(crate) fn strip_capacity(window: (f32, f32), sources_open: bool, info_open: bool) -> u32 {
    let [left, _, right, _] = canvas_logical(window, sources_open, info_open);
    let room = right - left - 2.0 * SIDE_INSET - 2.0 * (STRIP_CHEVRON + STRIP_SPACING);
    ((room + STRIP_SPACING) / (STRIP_FRAME_WIDTH + STRIP_SPACING))
        .floor()
        .max(1.0) as u32
}

/// Which of the active frame's unit's frames the strip shows, as indices of the unit from 0: a
/// window of them that fits across Select's centre, keeping the active one (`index`) in view.
fn strip_window(state: &SelectState, unit: Unit, index: u32) -> Range<u32> {
    let loupe = &state.loupe;
    let capacity = strip_capacity(loupe.window, state.sources_panel, state.info_panel);
    window(unit.len, index, capacity)
}

/// The pixels a picture drawn over `area` at `scale` needs: its size, never more than the preview
/// itself (`width` × `height`), which a decode never enlarges.
pub(crate) fn display_pixels(area: Area, scale: f32) -> (u32, u32) {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    (
        (area.width * scale).ceil().max(1.0) as u32,
        (area.height * scale).ceil().max(1.0) as u32,
    )
}

// -- Navigation ---------------------------------------------------------------------------------------

/// Which way the loupe last moved, which the look-ahead follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Travel {
    Back,
    #[default]
    Forward,
}

/// One gesture that moves the active frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Goto {
    /// `←` `→`: the previous or next frame of the view.
    Frame(Travel),
    /// `↑` `↓`: the first frame of the previous or next moment; a single frame is a moment of its
    /// own here, so every frame of the view is reached.
    Moment(Travel),
    /// `1`–`9` or a strip frame: that frame of the active moment, from 0.
    Frame0(u32),
}

/// A run of the view the loupe steps between: a burst or bracket of the summary's moments, or a
/// single frame on its own. The view is tiled by them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unit {
    pub(crate) start: u32,
    pub(crate) len: u32,
    /// Its index in the summary's `groups.moments`; none for a single.
    pub(crate) moment: Option<u32>,
}

/// The unit `position` is in.
pub(crate) fn unit_of(summary: &ViewSummary, position: u32) -> Unit {
    let moments = &summary.groups.moments;
    let after = moments.partition_point(|moment| moment.start <= position);
    after
        .checked_sub(1)
        .map(|index| (index, &moments[index]))
        .filter(|(_, moment)| {
            moment.kind != MomentKind::Single
                && position < moment.start.saturating_add(moment.len)
                && moment.len > 0
        })
        .map_or(
            Unit {
                start: position,
                len: 1,
                moment: None,
            },
            |(index, moment)| Unit {
                start: moment.start,
                len: moment.len.min(summary.count.saturating_sub(moment.start)),
                moment: Some(index as u32),
            },
        )
}

/// The first frame of the unit after (or before) the one `position` is in, or none at the view's
/// edge.
pub(crate) fn next_unit(summary: &ViewSummary, position: u32, travel: Travel) -> Option<u32> {
    let unit = unit_of(summary, position);
    match travel {
        Travel::Forward => {
            let end = unit.start.saturating_add(unit.len);
            (end < summary.count).then_some(end)
        }
        Travel::Back => unit
            .start
            .checked_sub(1)
            .map(|before| unit_of(summary, before).start),
    }
}

/// Where `goto` takes the active frame from `position`: `None` at the view's edge, or when it would
/// not move.
pub(crate) fn target(summary: &ViewSummary, position: u32, goto: Goto) -> Option<u32> {
    let to = match goto {
        Goto::Frame(Travel::Forward) => position.checked_add(1)?,
        Goto::Frame(Travel::Back) => position.checked_sub(1)?,
        Goto::Moment(travel) => next_unit(summary, position, travel)?,
        Goto::Frame0(index) => {
            let unit = unit_of(summary, position);
            unit.moment?;
            (index < unit.len).then_some(unit.start + index)?
        }
    };
    (to < summary.count && to != position).then_some(to)
}

/// The frames the look-ahead wants decoded after the active one, nearest first: the next
/// [`LOOK_AHEAD_FRAMES`] frames in the direction of travel, then the first frame of the next moment
/// that way when they do not already reach it.
pub(crate) fn look_ahead(summary: &ViewSummary, active: u32, travel: Travel) -> Vec<u32> {
    let mut frames = Vec::with_capacity(LOOK_AHEAD_FRAMES as usize + 1);
    for step in 1..=LOOK_AHEAD_FRAMES {
        let at = match travel {
            Travel::Forward => active.checked_add(step).filter(|at| *at < summary.count),
            Travel::Back => active.checked_sub(step),
        };
        frames.extend(at);
    }
    if let Some(next) = next_unit(summary, active, travel)
        && next != active
        && !frames.contains(&next)
    {
        frames.push(next);
    }
    frames
}

/// The frames compare shows: up to [`COMPARE_FRAMES`] of the active frame's moment, a window that
/// keeps the active one in view. None for a single frame.
pub(crate) fn compare_frames(summary: &ViewSummary, active: u32) -> Option<Range<u32>> {
    let unit = unit_of(summary, active);
    unit.moment?;
    let window = window(unit.len, active - unit.start, COMPARE_FRAMES);
    Some(unit.start + window.start..unit.start + window.end)
}

/// Which of `total` frames to draw, at most `capacity` of them, keeping `active` in view and as near
/// the middle as the ends allow: the frame strip widget's own rule (its `visible_window`).
pub(crate) fn window(total: u32, active: u32, capacity: u32) -> Range<u32> {
    let count = total.min(capacity);
    let active = active.min(total.saturating_sub(1));
    let start = active
        .saturating_sub(count.saturating_sub(1) / 2)
        .min(total - count);
    start..start + count
}

/// P7: once the active frame of a burst has been picked, the first frame of the next moment.
/// Nothing for a clear, a bracket's frame (one is usually merged, not chosen) or a single, or at the
/// view's end.
pub(crate) fn after_pick(summary: &ViewSummary, active: u32, picked: bool) -> Option<u32> {
    if !picked {
        return None;
    }
    let unit = unit_of(summary, active);
    let moment = summary.groups.moments.get(unit.moment? as usize)?;
    if moment.kind != MomentKind::Burst {
        return None;
    }
    next_unit(summary, active, Travel::Forward)
}

// -- The 100% region ------------------------------------------------------------------------------

/// The inset's region in pixels at the display's `scale`: at 100% one of the photograph's pixels is
/// one of the display's.
pub(crate) fn region_size(scale: f32) -> (u32, u32) {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    (
        (FOCUS_REGION.0 * scale).ceil() as u32,
        (FOCUS_REGION.1 * scale).ceil() as u32,
    )
}

/// The size in points a region of `rect`'s pixels takes at 100% on a display of `scale`: one of its
/// pixels to one of the display's.
pub(crate) fn region_points(rect: PixelRect, scale: f32) -> (f32, f32) {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    (rect.width as f32 / scale, rect.height as f32 / scale)
}

/// The rectangle of `frame` under the pointer at `pointer` (fractions of the picture): `size`
/// pixels centred there, moved back inside the frame and no larger than it.
pub(crate) fn region_rect(frame: Dimensions, pointer: (f32, f32), size: (u32, u32)) -> PixelRect {
    let (width, height) = (
        size.0.min(frame.width).max(1),
        size.1.min(frame.height).max(1),
    );
    let place = |fraction: f32, side: u32, span: u32| {
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.5
        };
        let centre = (fraction * side as f32).round() as i64;
        (centre - i64::from(span) / 2).clamp(0, i64::from(side - span)) as u32
    };
    PixelRect {
        x: place(pointer.0, frame.width, width),
        y: place(pointer.1, frame.height, height),
        width,
        height,
    }
}

/// `rect` of `frame` as it falls on a picture drawn `picture` points large, relative to it.
pub(crate) fn box_on(rect: PixelRect, frame: Dimensions, picture: Area) -> Area {
    let sx = picture.width / frame.width.max(1) as f32;
    let sy = picture.height / frame.height.max(1) as f32;
    Area {
        x: rect.x as f32 * sx,
        y: rect.y as f32 * sy,
        width: (rect.width as f32 * sx).max(2.0),
        height: (rect.height as f32 * sy).max(2.0),
    }
}

/// A row's upright full-resolution size, from its header: what the focus check names as the frame
/// its rectangle is in until a region has answered with the frame it was cut from.
pub(crate) fn upright(row: &ViewRow) -> Option<Dimensions> {
    let dimensions = row.dimensions?;
    Some(if row.orientation.is_some_and(|turn| turn.transposes()) {
        Dimensions {
            width: dimensions.height,
            height: dimensions.width,
        }
    } else {
        dimensions
    })
    .filter(|size| size.width > 0 && size.height > 0)
}

/// The preview item a row's frame is read as: a file's, or a developed photograph's current entry.
pub(crate) fn preview_item(row: &ViewRow) -> PreviewItem {
    match &row.item {
        RowItem::File { file_id } => PreviewItem::File { file_id: *file_id },
        RowItem::Photo { asset_id } => PreviewItem::Photo {
            asset_id: asset_id.clone(),
            entry_id: None,
        },
    }
}

// -- State ----------------------------------------------------------------------------------------

/// A decoded picture the app holds for a frame: what it is, which the loupe says, and the key the
/// view looks its handle up by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picture {
    /// The frame it is of: the item it was read for.
    pub(crate) item: PreviewItem,
    pub(crate) key: String,
    pub(crate) origin: PreviewOrigin,
    /// The preview's own size, which the bar states.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Not the tier the loupe asked for, but the best preview cached meanwhile (a grid tier or a
    /// thumbnail, or a photograph's camera preview): drawn while the tier is read, and said so.
    pub(crate) stand_in: bool,
    /// Rendered through the proxy path with an approximation the preview carries.
    pub(crate) approximate: bool,
}

/// What the app holds for one frame the loupe may draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) item: PreviewItem,
    pub(crate) picture: Option<Picture>,
    /// Why nothing will be drawn, when the owner refused the frame's preview.
    pub(crate) unavailable: Option<String>,
}

/// The 100% region the app holds: what it was cut from, and its number, which the view looks its
/// handle up by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub(crate) serial: u64,
    pub(crate) item: PreviewItem,
    pub(crate) rect: PixelRect,
    pub(crate) frame: Dimensions,
    pub(crate) origin: PreviewOrigin,
}

/// Everything the app mirrors into the state after each message, for the model to read.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Holding {
    /// The frames on screen: the active one and compare's.
    pub(crate) frames: Vec<Held>,
    /// The strip's photographs' thumbnails; a file's strip frame is the Select grid's preview.
    pub(crate) thumbnails: Vec<Held>,
    pub(crate) region: Option<Region>,
    /// A region is being cut, or waits to be.
    pub(crate) region_pending: bool,
    /// Why the last region failed, for the active frame.
    pub(crate) region_error: Option<String>,
    /// The frame the last region answered for its item: the upright full-resolution size the next
    /// rectangle is named in.
    pub(crate) frame_of: Option<(PreviewItem, Dimensions)>,
    /// The look-ahead's frames, and how many of them are decoded.
    pub(crate) ahead: u32,
    pub(crate) ahead_ready: u32,
}

/// The loupe's own state in the desktop: this desktop's view state, like the grid's collapsed
/// bursts; no other client sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LoupeState {
    pub(crate) open: bool,
    /// `C`: the moment's frames side by side.
    pub(crate) compare: bool,
    /// `Z`: the 100% focus check under the pointer.
    pub(crate) focus: bool,
    pub(crate) travel: Travel,
    /// The pointer over the picture, as fractions of it; none while it is elsewhere, when the focus
    /// check looks at the middle.
    pub(crate) pointer: Option<(f32, f32)>,
    /// The window's logical size and scale factor, which the loupe lays out in.
    pub(crate) window: (f32, f32),
    pub(crate) scale: f32,
    /// What the app holds to draw.
    pub(crate) held: Holding,
}

// -- The model ------------------------------------------------------------------------------------

/// What the loupe shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LoupeModel {
    pub(crate) open: bool,
    /// The frame to show and where it sits; none without a view or an active item.
    pub(crate) subject: Option<LoupeSubject>,
    /// The active frame, once its row is read.
    pub(crate) frame: Option<FrameModel>,
    pub(crate) info: InfoText,
    pub(crate) strip: Option<StripModel>,
    /// Compare's cells, while it is on.
    pub(crate) compare: Vec<FrameModel>,
    pub(crate) focus: Option<FocusModel>,
    pub(crate) hints: Vec<(String, String)>,
    /// The status bar's line while the loupe is open.
    pub(crate) status: String,
    /// Where the photograph may be drawn.
    pub(crate) area: Area,
}

/// The loupe's frame and where it sits: the active item's `position` in the view at `revision`
/// (`count` items), and the burst or bracket it belongs to, if any. Positions are the view's,
/// as `browse.rows` and `browse.select` name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LoupeSubject {
    pub(crate) revision: u64,
    pub(crate) position: u32,
    pub(crate) count: u32,
    pub(crate) moment: Option<MomentSpan>,
}

/// A moment of the view: its index in the summary's `groups.moments` and its frames
/// `start..start + len`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MomentSpan {
    pub(crate) index: u32,
    pub(crate) start: u32,
    pub(crate) len: u32,
}

/// One frame drawn: which row it is, the picture held for that row's own item, and where it goes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FrameModel {
    pub(crate) position: u32,
    pub(crate) item: PreviewItem,
    pub(crate) name: String,
    /// Its number in its moment, from 1 (compare's caption).
    pub(crate) number: u32,
    pub(crate) active: bool,
    /// The picture to draw, whose item is `item`; none while it is read.
    pub(crate) picture: Option<Picture>,
    /// Where the picture is drawn, in the window's points.
    pub(crate) rect: Area,
    /// What the frame says instead of a picture.
    pub(crate) note: Option<String>,
}

/// The info bar's four parts, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct InfoText {
    pub(crate) moment: String,
    pub(crate) frame: String,
    pub(crate) exposure: Option<String>,
    pub(crate) source: String,
}

/// The moment's frames under the photograph: a window of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StripModel {
    /// The moment's index of `frames[0]`, from 0.
    pub(crate) first: u32,
    pub(crate) frames: Vec<StripFrame>,
    /// The moment's index of the active frame.
    pub(crate) active: u32,
    /// The view's position of the moment's first frame.
    pub(crate) start: u32,
    pub(crate) previous: bool,
    pub(crate) next: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StripFrame {
    pub(crate) position: u32,
    /// Its item: a file, whose grid preview the strip borrows from the Select grid, or a
    /// photograph, whose grid tier the loupe reads; none for an unread row.
    pub(crate) item: Option<PreviewItem>,
    /// A photograph's thumbnail to draw, whose item is `item`; none for a file or while it is read.
    pub(crate) thumbnail: Option<Picture>,
    pub(crate) picked: bool,
}

/// The 100% focus check: the region box on the picture and the inset.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FocusModel {
    /// The rectangle asked for, in the frame's upright full-resolution pixels, and that frame.
    pub(crate) rect: PixelRect,
    pub(crate) frame: Dimensions,
    /// The box it makes on the picture, relative to the picture.
    pub(crate) region_box: Area,
    /// Where the inset is drawn, in the window's points: inside the picture's lower right corner.
    pub(crate) inset: Area,
    /// The region to draw: the active frame's own, cut from the frame named.
    pub(crate) region: Option<Region>,
    /// The size in points the region's pixels take at 100% on this display: its pixel size over
    /// the display's scale factor. The inset draws it at that size, never scaled.
    pub(crate) region_points: (f32, f32),
    /// The region is a Luxforge development, which the inset says.
    pub(crate) developed: bool,
    pub(crate) pending: bool,
    pub(crate) error: Option<String>,
}

/// The loupe's subject: the session's active item in the view the summary describes, when the
/// session's selection belongs to that view's revision. The moment is found by a binary search of
/// the summary's moments, which are ordered and disjoint.
pub(crate) fn subject(
    summary: Option<&ViewSummary>,
    browse: &BrowseSession,
) -> Option<LoupeSubject> {
    let summary = summary?;
    if browse.revision != summary.revision {
        return None;
    }
    let position = browse.selection.active?;
    if position >= summary.count {
        return None;
    }
    let unit = unit_of(summary, position);
    Some(LoupeSubject {
        revision: summary.revision,
        position,
        count: summary.count,
        moment: unit.moment.map(|index| MomentSpan {
            index,
            start: unit.start,
            len: unit.len,
        }),
    })
}

/// The row at `position` of the view at `revision`, when the rows window holds it.
fn row_at(state: &SelectState, revision: u64, position: u32) -> Option<&ViewRow> {
    (state.rows.revision() == revision)
        .then(|| state.rows.row(position))
        .flatten()
}

/// What the app holds for `item` among `held` (its frames, or its strip thumbnails), when it holds
/// anything.
fn held_for<'a>(held: &'a [Held], item: &PreviewItem) -> Option<&'a Held> {
    held.iter().find(|frame| &frame.item == item)
}

/// A frame's model: its row, the picture held for its own item, fitted into `area`.
fn frame_model(
    loupe: &LoupeState,
    row: &ViewRow,
    number: u32,
    active: bool,
    area: Area,
) -> FrameModel {
    let item = preview_item(row);
    let held = held_for(&loupe.held.frames, &item);
    let picture = held
        .and_then(|held| held.picture.clone())
        .filter(|picture| picture.item == item);
    let (width, height) = picture
        .as_ref()
        .map(|picture| (picture.width, picture.height))
        .or_else(|| upright(row).map(|size| (size.width, size.height)))
        .unwrap_or((3, 2));
    let note = match (&picture, held.and_then(|held| held.unavailable.as_deref())) {
        (Some(_), _) => None,
        (None, Some(reason)) => Some(format!("Preview unavailable: {reason}")),
        (None, None) => Some("Reading the preview\u{2026}".to_owned()),
    };
    FrameModel {
        position: row.position,
        item,
        name: row.file_name.clone(),
        number,
        active,
        picture,
        rect: fitted(area, width, height),
        note,
    }
}

/// What the bar says the picture is: its origin and size, and whether it stands in for the tier
/// being read or is an approximation.
pub(crate) fn source_text(picture: Option<&Picture>, note: Option<&str>) -> String {
    let Some(picture) = picture else {
        return note.unwrap_or("Reading the preview\u{2026}").to_owned();
    };
    let mut text = format!(
        "{} \u{b7} {} \u{d7} {}",
        picture.origin.label(),
        picture.width,
        picture.height
    );
    if picture.approximate {
        text.push_str(" \u{b7} approximate");
    }
    if picture.stand_in {
        text.push_str(" \u{b7} reading full size\u{2026}");
    }
    text
}

/// A capture time's instant in milliseconds, from the text a row carries
/// (`YYYY-MM-DDTHH:MM:SS[.fff][±HH:MM]`).
fn capture_ms(text: &str) -> Option<i64> {
    let head = text.get(..19)?;
    let rest = text.get(19..)?;
    let datetime = head.replacen('T', " ", 1);
    let (subsec, offset) = match rest.strip_prefix('.') {
        Some(rest) => {
            let end = rest.find(['+', '-']).unwrap_or(rest.len());
            (Some(&rest[..end]), Some(&rest[end..]))
        }
        None => (None, Some(rest)),
    };
    let offset = offset.filter(|offset| !offset.is_empty());
    CaptureTime::from_exif(&datetime, subsec, offset).map(|time| time.instant_ms())
}

/// The frame's time after its moment's first frame: `+0.52 s`.
fn offset_text(first: &ViewRow, row: &ViewRow) -> Option<String> {
    let from = capture_ms(first.capture.as_deref()?)?;
    let at = capture_ms(row.capture.as_deref()?)?;
    let seconds = (at - from) as f64 / 1000.0;
    Some(if seconds < 0.0 {
        format!("\u{2212}{:.2} s", -seconds)
    } else {
        format!("+{seconds:.2} s")
    })
}

/// The loupe's key hints, as the board lists them; jump and compare only in a moment.
fn hints(moment_len: Option<u32>) -> Vec<(String, String)> {
    let hint = |key: &str, action: &str| (key.to_owned(), action.to_owned());
    let mut hints = vec![hint("\u{2190} \u{2192}", "frames")];
    if let Some(len) = moment_len {
        hints.push(hint(&format!("1\u{2013}{}", len.min(9)), "jump"));
    }
    hints.push(hint("P", "pick"));
    hints.push(hint("Z", "100%"));
    hints.push(hint("\u{2191} \u{2193}", "moments"));
    if moment_len.is_some() {
        hints.push(hint("C", "side by side"));
    }
    hints.push(hint("Tab", "panels"));
    hints.push(hint("Esc", "grid"));
    hints
}

/// The frame the focus check names its rectangle in for `row`: the frame the last region of this
/// item was cut from, else the header's upright size, else the picture's.
pub(crate) fn focus_frame(
    loupe: &LoupeState,
    row: &ViewRow,
    picture: Option<&Picture>,
) -> Option<Dimensions> {
    let item = preview_item(row);
    loupe
        .held
        .frame_of
        .as_ref()
        .filter(|(of, _)| *of == item)
        .map(|(_, frame)| *frame)
        .or_else(|| upright(row))
        .or_else(|| {
            picture.map(|picture| Dimensions {
                width: picture.width,
                height: picture.height,
            })
        })
}

/// The rectangle the focus check asks for now — the active frame's item, the rectangle under the
/// pointer and the upright frame it is named in — or none while the check is off, compare is on,
/// or the frame's size is not known yet.
pub(crate) fn focus_request(
    state: &SelectState,
    browse: &BrowseSession,
) -> Option<(PreviewItem, PixelRect, Dimensions)> {
    let loupe = &state.loupe;
    if !loupe.open || !loupe.focus || loupe.compare {
        return None;
    }
    let subject = subject(state.summary.as_ref(), browse)?;
    let row = row_at(state, subject.revision, subject.position)?;
    let item = preview_item(row);
    let picture = held_for(&loupe.held.frames, &item)
        .and_then(|held| held.picture.as_ref())
        .filter(|picture| picture.item == item);
    let frame = focus_frame(loupe, row, picture)?;
    let rect = region_rect(
        frame,
        loupe.pointer.unwrap_or((0.5, 0.5)),
        region_size(loupe.scale),
    );
    Some((item, rect, frame))
}

/// The loupe's model from Select's state and the session's browse state.
pub(crate) fn derive(state: &SelectState, browse: &BrowseSession) -> LoupeModel {
    let loupe = &state.loupe;
    let subject = subject(state.summary.as_ref(), browse);
    let mut model = LoupeModel {
        open: loupe.open,
        subject,
        ..LoupeModel::default()
    };
    if !loupe.open {
        return model;
    }
    let (Some(subject), Some(summary)) = (subject, state.summary.as_ref()) else {
        model.info.moment = "Nothing to show".into();
        model.info.source = "Choose a frame in the grid".into();
        return model;
    };
    let area = frame_area(loupe.window, state.sources_panel, state.info_panel);
    model.area = area;
    let unit = unit_of(summary, subject.position);
    let moment = unit
        .moment
        .and_then(|index| summary.groups.moments.get(index as usize));
    model.hints = hints(moment.map(|_| unit.len));
    let Some(row) = row_at(state, subject.revision, subject.position) else {
        model.info.moment = "Reading\u{2026}".into();
        model.info.source = "Reading the frame\u{2026}".into();
        return model;
    };
    let index = subject.position - unit.start;
    let frame = frame_model(loupe, row, index + 1, true, area);
    // The bar: the moment and the frame's place in it, its exposure, and what the picture is.
    model.info = InfoText {
        moment: match moment {
            Some(moment) => format!(
                "Moment {} of {} \u{b7} {}",
                unit.moment.unwrap_or(0) + 1,
                thousands(summary.groups.moments.len() as u32),
                if moment.kind == MomentKind::Bracket {
                    "bracket"
                } else {
                    "burst"
                }
            ),
            None => row.file_name.clone(),
        },
        frame: match moment {
            Some(_) => {
                let place = format!("Frame {} of {}", index + 1, unit.len);
                match row_at(state, subject.revision, unit.start)
                    .and_then(|first| offset_text(first, row))
                {
                    Some(offset) => format!("{place} \u{b7} {offset}"),
                    None => place,
                }
            }
            None => format!(
                "{} of {} in view",
                thousands(subject.position + 1),
                thousands(summary.count)
            ),
        },
        exposure: exposure_text(&row.exposure),
        source: source_text(frame.picture.as_ref(), frame.note.as_deref()),
    };
    // The strip: the moment's frames, or the single frame alone, each a photograph's thumbnail
    // only when it is the one held for its own item.
    let shown = strip_window(state, unit, index);
    model.strip = Some(StripModel {
        first: shown.start,
        frames: shown
            .clone()
            .map(|at| {
                let position = unit.start + at;
                let row = row_at(state, subject.revision, position);
                let item = row.map(preview_item);
                let thumbnail = item
                    .as_ref()
                    .filter(|item| matches!(item, PreviewItem::Photo { .. }))
                    .and_then(|item| {
                        held_for(&loupe.held.thumbnails, item)?
                            .picture
                            .clone()
                            .filter(|picture| &picture.item == item)
                    });
                StripFrame {
                    position,
                    item,
                    thumbnail,
                    picked: row.is_some_and(|row| row.picked),
                }
            })
            .collect(),
        active: index,
        start: unit.start,
        previous: unit.start > 0,
        next: unit.start + unit.len < summary.count,
    });
    // Compare: the moment's frames at one zoom, each its own row's picture.
    if loupe.compare
        && let Some(frames) = compare_frames(summary, subject.position)
    {
        let cells = compare_cells(area, frames.len() as u32);
        model.compare = frames
            .zip(cells)
            .filter_map(|(position, cell)| {
                let row = row_at(state, subject.revision, position)?;
                Some(frame_model(
                    loupe,
                    row,
                    position - unit.start + 1,
                    position == subject.position,
                    cell,
                ))
            })
            .collect();
    }
    // The focus check: the rectangle under the pointer, its box on the picture and the region cut
    // from the active frame's own item.
    if let Some((item, rect, frame_size)) = focus_request(state, browse)
        && item == frame.item
    {
        let region = loupe
            .held
            .region
            .clone()
            .filter(|region| region.item == frame.item);
        let picture = frame.rect;
        model.focus = Some(FocusModel {
            rect,
            frame: frame_size,
            region_box: box_on(rect, frame_size, picture),
            inset: Area {
                x: picture.x + picture.width - FOCUS_INSET_MARGIN - FOCUS_REGION.0,
                y: picture.y + picture.height - FOCUS_INSET_MARGIN - FOCUS_INSET_HEIGHT,
                width: FOCUS_REGION.0,
                height: FOCUS_INSET_HEIGHT,
            },
            developed: region
                .as_ref()
                .is_some_and(|region| region.origin == PreviewOrigin::Developed),
            region_points: region.as_ref().map_or(FOCUS_REGION, |region| {
                region_points(region.rect, loupe.scale)
            }),
            region,
            pending: loupe.held.region_pending,
            error: loupe.held.region_error.clone(),
        });
    }
    model.status = format!(
        "Loupe \u{b7} {} \u{b7} {}",
        match frame.picture.as_ref().map(|picture| picture.origin) {
            Some(PreviewOrigin::Rendered) => "rendered previews",
            Some(PreviewOrigin::Developed) => "Luxforge development",
            _ => "camera previews",
        },
        if loupe.held.ahead == 0 || loupe.held.ahead_ready >= loupe.held.ahead {
            "next frames ready".to_owned()
        } else {
            format!(
                "reading next frames ({} of {})",
                loupe.held.ahead_ready, loupe.held.ahead
            )
        }
    );
    model.frame = Some(frame);
    model
}

/// One picture the loupe wants decoded: its row's item at its position, the pixels its area needs,
/// whether it is on screen, which the cache never evicts, and whether it is the item's strip
/// thumbnail (its grid tier) rather than its frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WantedFrame {
    pub(crate) position: u32,
    pub(crate) item: PreviewItem,
    pub(crate) pixels: (u32, u32),
    pub(crate) shown: bool,
    pub(crate) thumbnail: bool,
}

/// What the loupe wants decoded now, most wanted first: the active frame, compare's frames, the
/// look-ahead in the direction of travel, then the strip's photographs' thumbnails, which are on
/// screen (the strip's files are the Select grid's). A frame whose row is not read yet is left out.
pub(crate) fn wanted(state: &SelectState, browse: &BrowseSession) -> Vec<WantedFrame> {
    let loupe = &state.loupe;
    if !loupe.open {
        return Vec::new();
    }
    let (Some(subject), Some(summary)) = (
        subject(state.summary.as_ref(), browse),
        state.summary.as_ref(),
    ) else {
        return Vec::new();
    };
    let area = frame_area(loupe.window, state.sources_panel, state.info_panel);
    let full = display_pixels(area, loupe.scale);
    let mut frames: Vec<WantedFrame> = Vec::new();
    let mut push = |position: u32, pixels: (u32, u32), shown: bool| {
        if let Some(row) = row_at(state, subject.revision, position) {
            let item = preview_item(row);
            if !frames.iter().any(|frame| frame.item == item) {
                frames.push(WantedFrame {
                    position,
                    item,
                    pixels,
                    shown,
                    thumbnail: false,
                });
            }
        }
    };
    let compare = loupe
        .compare
        .then(|| compare_frames(summary, subject.position))
        .flatten();
    match compare {
        Some(range) => {
            let cell = compare_cells(area, range.len() as u32)[0];
            let pixels = display_pixels(cell, loupe.scale);
            push(subject.position, pixels, true);
            for position in range {
                push(position, pixels, true);
            }
        }
        None => push(subject.position, full, true),
    }
    for position in look_ahead(summary, subject.position, loupe.travel) {
        push(position, full, false);
    }
    let unit = unit_of(summary, subject.position);
    let strip = display_pixels(
        Area {
            width: STRIP_IMAGE.0,
            height: STRIP_IMAGE.1,
            ..Area::default()
        },
        loupe.scale,
    );
    for at in strip_window(state, unit, subject.position - unit.start) {
        let position = unit.start + at;
        if let Some(row) = row_at(state, subject.revision, position)
            && let RowItem::Photo { .. } = row.item
        {
            frames.push(WantedFrame {
                position,
                item: preview_item(row),
                pixels: strip,
                shown: true,
                thumbnail: true,
            });
        }
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::select::RowCache;
    use luxforge_core::catalog_types::{
        Exposure, FileAvailability, FileId, GroupLayout, Moment, MomentRef, PreviewState, RowItem,
        ViewQuery, ViewSelection, ViewSource,
    };
    use luxforge_core::{SourceTag, catalog_types::LibraryChangeSeq};

    /// A view of `count` items at `revision` whose moments are `(start, len, kind)`.
    fn summary(revision: u64, count: u32, moments: &[(u32, u32, MomentKind)]) -> ViewSummary {
        ViewSummary {
            revision,
            query: ViewQuery::of(ViewSource::AllPhotographs),
            count,
            picked: 0,
            in_catalog: 0,
            unavailable: 0,
            groups: GroupLayout {
                moments: moments
                    .iter()
                    .map(|&(start, len, kind)| Moment {
                        kind,
                        evidence: None,
                        steps_ev: Vec::new(),
                        span_ms: 0,
                        start,
                        len,
                        picked: 0,
                    })
                    .collect(),
                ..GroupLayout::default()
            },
            library_sequence: LibraryChangeSeq(0),
            index_revision: 0,
        }
    }

    fn bursts(revision: u64, count: u32, moments: &[(u32, u32)]) -> ViewSummary {
        let moments: Vec<_> = moments
            .iter()
            .map(|&(start, len)| (start, len, MomentKind::Burst))
            .collect();
        summary(revision, count, &moments)
    }

    fn browse(revision: u64, active: Option<u32>) -> BrowseSession {
        BrowseSession {
            revision,
            count: 12,
            selection: ViewSelection {
                active,
                ..ViewSelection::default()
            },
            ..BrowseSession::default()
        }
    }

    /// A file row at `position`, file `100 + position`, captured `ms` milliseconds into a second.
    fn row(position: u32, ms: u32) -> ViewRow {
        ViewRow {
            position,
            item: RowItem::File {
                file_id: FileId(100 + i64::from(position)),
            },
            path: format!("/card/DSC_{position:04}.JPG").into(),
            file_name: format!("DSC_{position:04}.JPG"),
            kind: SourceTag::Jpeg,
            dimensions: Some(Dimensions {
                width: 6000,
                height: 4000,
            }),
            orientation: None,
            capture: Some(format!(
                "2026-09-12T10:15:0{}.{:03}+02:00",
                ms / 1000,
                ms % 1000
            )),
            place: None,
            camera: Some("NIKON Z 8".into()),
            lens: None,
            exposure: Exposure {
                time_s: Some(1.0 / 2000.0),
                f_number: Some(5.6),
                iso: Some(100),
                focal_mm: Some(28.0),
                ..Exposure::default()
            },
            moment: None::<MomentRef>,
            picked: false,
            developed_as: None,
            edited: false,
            availability: FileAvailability::Available,
            preview: PreviewState::Ready,
        }
    }

    /// Select's state over `view`, every row read, the loupe open at 1440 × 900 at scale 2.
    fn state(view: ViewSummary) -> SelectState {
        let mut rows = RowCache::default();
        rows.reset(view.revision, view.count);
        let block: Vec<ViewRow> = (0..view.count).map(|at| row(at, at * 260)).collect();
        rows.answered(view.revision, 0, block, 0..view.count);
        SelectState {
            summary: Some(view),
            rows,
            loupe: LoupeState {
                open: true,
                window: (1440.0, 900.0),
                scale: 2.0,
                ..LoupeState::default()
            },
            ..SelectState::default()
        }
    }

    fn file(position: u32) -> PreviewItem {
        PreviewItem::File {
            file_id: FileId(100 + i64::from(position)),
        }
    }

    fn picture(position: u32, key: &str) -> Picture {
        Picture {
            item: file(position),
            key: key.into(),
            origin: PreviewOrigin::Embedded,
            width: 2560,
            height: 1707,
            stand_in: false,
            approximate: false,
        }
    }

    /// The inset draws a region at 100%: its pixels over the display's scale, so a region the
    /// inset's size fills it on a 2x display and a smaller one (cut at the frame's edge) takes fewer
    /// points instead of being enlarged.
    #[test]
    fn loupe_region_is_drawn_at_one_pixel_to_one() {
        let rect = |width, height| PixelRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let (width, height) = region_size(2.0);
        assert_eq!(region_points(rect(width, height), 2.0), (308.0, 209.0));
        assert_eq!(region_points(rect(120, 80), 2.0), (60.0, 40.0));
        assert_eq!(region_points(rect(120, 80), 1.0), (120.0, 80.0));
        assert_eq!(
            region_points(rect(120, 80), 0.0),
            (120.0, 80.0),
            "no scale yet"
        );
    }

    /// The bar names what the picture is, and says so when a developed photograph's rendered tier
    /// is an approximation of its entry, as the core labels it.
    #[test]
    fn loupe_bar_says_an_approximate_rendered_tier() {
        let mut rendered = picture(0, "photo:a:e:large:r1");
        rendered.origin = PreviewOrigin::Rendered;
        assert_eq!(
            source_text(Some(&rendered), None),
            "Preview \u{b7} 2560 \u{d7} 1707"
        );
        rendered.approximate = true;
        assert_eq!(
            source_text(Some(&rendered), None),
            "Preview \u{b7} 2560 \u{d7} 1707 \u{b7} approximate"
        );
    }

    /// The subject is the active item of the view on screen, in its moment when it has one, and
    /// nothing without a view, an active item, or when the session's selection is another view's.
    #[test]
    fn loupe_subject_is_the_active_item_in_its_moment() {
        let view = bursts(3, 12, &[(2, 3), (7, 4)]);
        let at = |active| subject(Some(&view), &browse(3, Some(active)));
        assert_eq!(at(0).unwrap().moment, None, "a single before any moment");
        let burst = at(3).unwrap();
        assert_eq!(
            burst.moment,
            Some(MomentSpan {
                index: 0,
                start: 2,
                len: 3
            })
        );
        assert_eq!(at(5).unwrap().moment, None, "a single between moments");
        assert_eq!(at(10).unwrap().moment.unwrap().index, 1);
        assert_eq!(at(11).unwrap().count, 12);
        assert_eq!(subject(Some(&view), &browse(3, None)), None);
        assert_eq!(subject(Some(&view), &browse(2, Some(3))), None);
        assert_eq!(subject(Some(&view), &browse(3, Some(12))), None);
        assert_eq!(subject(None, &browse(3, Some(3))), None);
        let model = derive(&state(view), &browse(3, Some(3)));
        assert!(model.open);
        assert_eq!(model.subject, Some(burst));
    }

    /// `←` `→` step through the view's frames across moments, `↑` `↓` go to the first frame of the
    /// previous or next moment with each single frame a stop of its own, and `1`–`9` jump within
    /// the active moment only. Nothing moves past the view's edges.
    #[test]
    fn loupe_navigation_crosses_frames_and_moments() {
        // Singles 0–1, a burst 2–4, a single 5–6, a bracket 7–10, a single 11.
        let view = summary(
            1,
            12,
            &[(2, 3, MomentKind::Burst), (7, 4, MomentKind::Bracket)],
        );
        let go = |from, goto| target(&view, from, goto);
        let (forward, back) = (Travel::Forward, Travel::Back);
        assert_eq!(go(4, Goto::Frame(forward)), Some(5), "out of a burst");
        assert_eq!(go(2, Goto::Frame(back)), Some(1), "back into the singles");
        assert_eq!(go(0, Goto::Frame(back)), None, "the view's first frame");
        assert_eq!(go(11, Goto::Frame(forward)), None, "the view's last frame");
        assert_eq!(go(0, Goto::Moment(forward)), Some(1), "a single is a stop");
        assert_eq!(go(1, Goto::Moment(forward)), Some(2));
        assert_eq!(
            go(3, Goto::Moment(forward)),
            Some(5),
            "past the burst's frames"
        );
        assert_eq!(go(6, Goto::Moment(forward)), Some(7));
        assert_eq!(go(9, Goto::Moment(forward)), Some(11));
        assert_eq!(go(11, Goto::Moment(forward)), None);
        assert_eq!(go(9, Goto::Moment(back)), Some(6), "the single before it");
        assert_eq!(go(5, Goto::Moment(back)), Some(2), "a moment's first frame");
        assert_eq!(go(3, Goto::Moment(back)), Some(1));
        assert_eq!(go(0, Goto::Moment(back)), None);
        assert_eq!(go(8, Goto::Frame0(0)), Some(7), "1 in a bracket");
        assert_eq!(go(7, Goto::Frame0(3)), Some(10), "4 in a bracket");
        assert_eq!(go(7, Goto::Frame0(4)), None, "past the moment's frames");
        assert_eq!(go(7, Goto::Frame0(0)), None, "the frame already active");
        assert_eq!(
            go(5, Goto::Frame0(0)),
            None,
            "a single has no frames to jump to"
        );
        assert_eq!(unit_of(&view, 5).moment, None);
        assert_eq!(unit_of(&view, 10).len, 4);
    }

    /// The look-ahead follows the direction of travel, nearest first, and adds the next moment's
    /// first frame when the frames do not reach it; it never names a frame outside the view.
    #[test]
    fn loupe_look_ahead_follows_the_direction_of_travel() {
        let view = bursts(1, 20, &[(2, 6), (10, 3)]);
        assert_eq!(look_ahead(&view, 2, Travel::Forward), vec![3, 4, 5, 8]);
        assert_eq!(look_ahead(&view, 6, Travel::Forward), vec![7, 8, 9]);
        assert_eq!(
            look_ahead(&view, 11, Travel::Back),
            vec![10, 9, 8],
            "the frame before the moment is the single frame before it, already ahead"
        );
        assert_eq!(
            look_ahead(&view, 8, Travel::Back),
            vec![7, 6, 5, 2],
            "back into a moment: its first frame"
        );
        assert_eq!(look_ahead(&view, 3, Travel::Back), vec![2, 1, 0]);
        assert_eq!(look_ahead(&view, 18, Travel::Forward), vec![19]);
        assert_eq!(look_ahead(&view, 0, Travel::Back), Vec::<u32>::new());
    }

    /// Compare shows up to four frames of the active frame's moment, keeping the active one in its
    /// window; a single frame has nothing to compare.
    #[test]
    fn loupe_compare_shows_up_to_four_frames_of_the_moment() {
        let view = bursts(1, 20, &[(2, 3), (6, 10)]);
        assert_eq!(
            compare_frames(&view, 3),
            Some(2..5),
            "a short burst is whole"
        );
        assert_eq!(compare_frames(&view, 6), Some(6..10));
        assert_eq!(
            compare_frames(&view, 11),
            Some(10..14),
            "the active frame in view"
        );
        assert_eq!(
            compare_frames(&view, 15),
            Some(12..16),
            "held at the moment's end"
        );
        assert_eq!(compare_frames(&view, 5), None, "a single");
        for active in 6..16 {
            let frames = compare_frames(&view, active).unwrap();
            assert_eq!(frames.len(), 4);
            assert!(frames.contains(&active));
        }
        // The model draws each cell's own picture, the active one marked.
        let mut state = state(view);
        state.loupe.compare = true;
        state.loupe.held.frames = (6..10)
            .map(|at| Held {
                item: file(at),
                picture: Some(picture(at, &format!("key-{at}"))),
                unavailable: None,
            })
            .collect();
        let model = derive(&state, &browse(1, Some(7)));
        assert_eq!(model.compare.len(), 4);
        for cell in &model.compare {
            assert_eq!(cell.picture.as_ref().unwrap().item, cell.item);
            assert_eq!(cell.active, cell.position == 7);
        }
        let widths: Vec<f32> = model.compare.iter().map(|cell| cell.rect.width).collect();
        assert!(widths.windows(2).all(|pair| pair[0] == pair[1]), "one zoom");
        assert!(model.focus.is_none(), "no focus check in compare");
    }

    /// P7: picking a burst frame moves on to the next moment's first frame; a clear, a bracket's
    /// frame, a single and the view's last moment stay where they are.
    #[test]
    fn loupe_picking_a_burst_frame_moves_to_the_next_moment() {
        let view = summary(
            1,
            12,
            &[(2, 3, MomentKind::Burst), (7, 4, MomentKind::Bracket)],
        );
        assert_eq!(after_pick(&view, 3, true), Some(5), "a burst frame");
        assert_eq!(after_pick(&view, 3, false), None, "a clear");
        assert_eq!(after_pick(&view, 8, true), None, "a bracket frame");
        assert_eq!(after_pick(&view, 5, true), None, "a single");
        let last = bursts(1, 5, &[(2, 3)]);
        assert_eq!(after_pick(&last, 4, true), None, "the view's last moment");
    }

    /// The bar names the moment and the frame's place and time in it, its exposure and what the
    /// picture is; a stand-in and an approximation say so, and nothing is claimed while it reads.
    #[test]
    fn loupe_bar_names_the_moment_and_what_the_picture_is() {
        let view = bursts(1, 12, &[(2, 6), (9, 2)]);
        let mut state = state(view);
        let model = derive(&state, &browse(1, Some(4)));
        assert_eq!(model.info.moment, "Moment 1 of 2 \u{b7} burst");
        assert_eq!(model.info.frame, "Frame 3 of 6 \u{b7} +0.52 s");
        assert_eq!(
            model.info.exposure.as_deref(),
            Some("1/2000 s \u{b7} f/5.6 \u{b7} ISO 100 \u{b7} 28 mm")
        );
        assert_eq!(model.info.source, "Reading the preview\u{2026}");
        assert!(model.frame.as_ref().unwrap().picture.is_none());
        state.loupe.held.frames = vec![Held {
            item: file(4),
            picture: Some(picture(4, "k")),
            unavailable: None,
        }];
        let model = derive(&state, &browse(1, Some(4)));
        assert_eq!(model.info.source, "Camera preview \u{b7} 2560 \u{d7} 1707");
        state.loupe.held.frames[0].picture = Some(Picture {
            stand_in: true,
            approximate: true,
            origin: PreviewOrigin::Rendered,
            width: 512,
            height: 341,
            ..picture(4, "k")
        });
        let model = derive(&state, &browse(1, Some(4)));
        assert_eq!(
            model.info.source,
            "Preview \u{b7} 512 \u{d7} 341 \u{b7} approximate \u{b7} reading full size\u{2026}"
        );
        // A single names its file and its place in the view.
        let model = derive(&state, &browse(1, Some(0)));
        assert_eq!(model.info.moment, "DSC_0000.JPG");
        assert_eq!(model.info.frame, "1 of 12 in view");
        assert_eq!(model.strip.as_ref().unwrap().frames.len(), 1);
        assert!(!model.hints.iter().any(|(key, _)| key == "C"));
        // The strip holds the moment's frames, the active one marked.
        let model = derive(&state, &browse(1, Some(4)));
        let strip = model.strip.unwrap();
        assert_eq!((strip.first, strip.active, strip.frames.len()), (0, 2, 6));
        assert_eq!(strip.frames[2].position, 4);
        assert!(model.hints.iter().any(|(key, _)| key == "1\u{2013}6"));
    }

    /// Identity: a picture held for another frame's item is never drawn under the active frame's
    /// name, whichever order the app mirrored them in, and a refused preview says why instead.
    #[test]
    fn loupe_never_draws_a_frame_under_another_frames_name() {
        let view = bursts(1, 12, &[(2, 6)]);
        let mut state = state(view);
        // The app holds frame 3's and frame 5's pictures; frame 4 is active.
        state.loupe.held.frames = vec![
            Held {
                item: file(3),
                picture: Some(picture(3, "three")),
                unavailable: None,
            },
            Held {
                item: file(5),
                picture: Some(picture(5, "five")),
                unavailable: None,
            },
        ];
        let model = derive(&state, &browse(1, Some(4)));
        let frame = model.frame.unwrap();
        assert_eq!(frame.item, file(4));
        assert!(frame.picture.is_none(), "no other frame's picture");
        assert_eq!(model.info.frame, "Frame 3 of 6 \u{b7} +0.52 s");
        // A held entry for the active item carrying another item's picture is refused too.
        state.loupe.held.frames = vec![Held {
            item: file(4),
            picture: Some(picture(5, "five")),
            unavailable: None,
        }];
        let frame = derive(&state, &browse(1, Some(4))).frame.unwrap();
        assert!(frame.picture.is_none());
        state.loupe.held.frames = vec![Held {
            item: file(4),
            picture: None,
            unavailable: Some("unsupported-input".into()),
        }];
        let model = derive(&state, &browse(1, Some(4)));
        assert_eq!(model.info.source, "Preview unavailable: unsupported-input");
        // A region of another frame is never shown in the inset.
        state.loupe.focus = true;
        state.loupe.held.region = Some(Region {
            serial: 1,
            item: file(5),
            rect: PixelRect {
                x: 0,
                y: 0,
                width: 616,
                height: 418,
            },
            frame: Dimensions {
                width: 6000,
                height: 4000,
            },
            origin: PreviewOrigin::Embedded,
        });
        let focus = derive(&state, &browse(1, Some(4))).focus.unwrap();
        assert!(focus.region.is_none());
    }

    /// The focus check's rectangle is the inset's size in the display's pixels, centred on the
    /// pointer and held inside the frame; its box on the picture is that rectangle scaled, and a
    /// development's region is labelled as one.
    #[test]
    fn loupe_focus_check_is_the_region_under_the_pointer() {
        let frame = Dimensions {
            width: 6000,
            height: 4000,
        };
        assert_eq!(region_size(2.0), (616, 418));
        assert_eq!(region_size(1.0), (308, 209));
        assert_eq!(
            region_rect(frame, (0.5, 0.5), (616, 418)),
            PixelRect {
                x: 2692,
                y: 1791,
                width: 616,
                height: 418
            }
        );
        let corner = region_rect(frame, (0.0, 1.0), (616, 418));
        assert_eq!((corner.x, corner.y), (0, 4000 - 418), "held inside");
        let small = region_rect(
            Dimensions {
                width: 400,
                height: 300,
            },
            (0.9, 0.1),
            (616, 418),
        );
        assert_eq!(
            small,
            PixelRect {
                x: 0,
                y: 0,
                width: 400,
                height: 300
            }
        );
        let region_box = box_on(
            region_rect(frame, (0.5, 0.5), (616, 418)),
            frame,
            Area {
                x: 0.0,
                y: 0.0,
                width: 960.0,
                height: 640.0,
            },
        );
        assert!((region_box.width - 98.56).abs() < 0.01);
        assert!((region_box.x + region_box.width / 2.0 - 480.0).abs() < 0.5);
        // The model's box follows the pointer; the header's frame is named until a region answers.
        let view = bursts(1, 12, &[(2, 6)]);
        let mut state = state(view);
        state.loupe.focus = true;
        state.loupe.pointer = Some((0.25, 0.75));
        let focus = derive(&state, &browse(1, Some(4))).focus.unwrap();
        assert_eq!(focus.frame, frame);
        assert_eq!(focus.rect, region_rect(frame, (0.25, 0.75), (616, 418)));
        assert!(!focus.developed && focus.region.is_none());
        let answered = Dimensions {
            width: 6048,
            height: 4024,
        };
        state.loupe.held.frame_of = Some((file(4), answered));
        state.loupe.held.region = Some(Region {
            serial: 3,
            item: file(4),
            rect: region_rect(answered, (0.25, 0.75), (616, 418)),
            frame: answered,
            origin: PreviewOrigin::Developed,
        });
        let focus = derive(&state, &browse(1, Some(4))).focus.unwrap();
        assert_eq!(focus.frame, answered, "the answer's frame, once known");
        assert!(focus.developed, "a development says so");
        assert_eq!(focus.region.unwrap().serial, 3);
    }

    /// The loupe wants the active frame on screen, then the look-ahead at the same size; compare's
    /// frames are on screen at a cell's size. A frame whose row is not read is left out.
    #[test]
    fn loupe_wants_the_active_frame_then_the_look_ahead() {
        let view = bursts(1, 12, &[(2, 6)]);
        let mut state = state(view);
        let wanted = wanted(&state, &browse(1, Some(3)));
        let positions: Vec<(u32, bool)> = wanted.iter().map(|w| (w.position, w.shown)).collect();
        assert_eq!(
            positions,
            vec![(3, true), (4, false), (5, false), (6, false), (8, false)]
        );
        assert!(wanted.iter().all(|frame| frame.pixels == wanted[0].pixels));
        state.loupe.travel = Travel::Back;
        let back: Vec<u32> = super::wanted(&state, &browse(1, Some(3)))
            .iter()
            .map(|w| w.position)
            .collect();
        assert_eq!(back, vec![3, 2, 1, 0]);
        state.loupe.compare = true;
        let compare = super::wanted(&state, &browse(1, Some(3)));
        assert_eq!(
            compare.iter().filter(|frame| frame.shown).count(),
            4,
            "the active frame and three more of its moment"
        );
        assert!(compare[0].pixels.0 < wanted[0].pixels.0);
        state.loupe.open = false;
        assert!(super::wanted(&state, &browse(1, Some(3))).is_empty());
    }

    /// Photograph `position` of a view of developed photographs.
    fn photograph(position: u32) -> PreviewItem {
        PreviewItem::Photo {
            asset_id: luxforge_core::AssetId::parse(format!("asset-photo-{position:04}")).unwrap(),
            entry_id: None,
        }
    }

    /// Select's state over `view`, a view of developed photographs, every row read.
    fn photographs(view: ViewSummary) -> SelectState {
        let mut state = state(view.clone());
        let mut rows = RowCache::default();
        rows.reset(view.revision, view.count);
        let block: Vec<ViewRow> = (0..view.count)
            .map(|at| ViewRow {
                item: RowItem::Photo {
                    asset_id: luxforge_core::AssetId::parse(format!("asset-photo-{at:04}"))
                        .unwrap(),
                },
                ..row(at, at * 260)
            })
            .collect();
        rows.answered(view.revision, 0, block, 0..view.count);
        state.rows = rows;
        state
    }

    /// In a view of photographs the loupe wants each strip frame's grid tier, on screen and at the
    /// strip's picture box, after the frames and the look-ahead; in a view of files it wants none,
    /// the strip borrowing the grid's. A strip frame draws a thumbnail only when the one held is
    /// its own photograph's.
    #[test]
    fn loupe_strip_wants_its_photographs_grid_tiers_and_draws_only_their_own() {
        let view = bursts(1, 12, &[(2, 6)]);
        let files = wanted(&state(view.clone()), &browse(1, Some(3)));
        assert!(files.iter().all(|frame| !frame.thumbnail), "{files:?}");
        let mut state = photographs(view);
        let wanted = wanted(&state, &browse(1, Some(3)));
        let frames: Vec<(u32, bool)> = wanted
            .iter()
            .filter(|frame| !frame.thumbnail)
            .map(|frame| (frame.position, frame.shown))
            .collect();
        assert_eq!(
            frames,
            vec![(3, true), (4, false), (5, false), (6, false), (8, false)],
            "the frames as in a view of files"
        );
        let thumbnails: Vec<&WantedFrame> = wanted.iter().filter(|frame| frame.thumbnail).collect();
        assert_eq!(
            thumbnails
                .iter()
                .map(|frame| frame.position)
                .collect::<Vec<_>>(),
            (2..8).collect::<Vec<_>>(),
            "the burst's six frames, the active one among them"
        );
        assert!(thumbnails.iter().all(|frame| frame.shown
            && frame.pixels == (232, 152)
            && frame.item == photograph(frame.position)));
        assert!(
            wanted.iter().position(|frame| frame.thumbnail).unwrap() == frames.len(),
            "after the frames and the look-ahead"
        );
        // Nothing held yet: the strip names its photographs and draws nothing.
        let strip = derive(&state, &browse(1, Some(3))).strip.unwrap();
        assert_eq!(strip.frames.len(), 6);
        assert!(strip.frames.iter().all(
            |frame| frame.thumbnail.is_none() && frame.item == Some(photograph(frame.position))
        ));
        // Frame 4's own thumbnail is drawn; one held under frame 5 but of frame 6 is not.
        let thumbnail = |of: u32| Picture {
            item: photograph(of),
            key: format!("photo:{of}:e:grid:r1"),
            origin: PreviewOrigin::Rendered,
            width: 512,
            height: 341,
            stand_in: false,
            approximate: false,
        };
        state.loupe.held.thumbnails = vec![
            Held {
                item: photograph(4),
                picture: Some(thumbnail(4)),
                unavailable: None,
            },
            Held {
                item: photograph(5),
                picture: Some(thumbnail(6)),
                unavailable: None,
            },
        ];
        // A frame's picture of frame 3 is not a thumbnail.
        state.loupe.held.frames = vec![Held {
            item: photograph(3),
            picture: Some(thumbnail(3)),
            unavailable: None,
        }];
        let strip = derive(&state, &browse(1, Some(3))).strip.unwrap();
        let drawn: Vec<(u32, Option<PreviewItem>)> = strip
            .frames
            .iter()
            .map(|frame| {
                (
                    frame.position,
                    frame.thumbnail.as_ref().map(|picture| picture.item.clone()),
                )
            })
            .collect();
        assert_eq!(
            drawn,
            vec![
                (2, None),
                (3, None),
                (4, Some(photograph(4))),
                (5, None),
                (6, None),
                (7, None)
            ]
        );
    }

    /// The photograph's area is Select's centre less the bar above and the strip and hints below;
    /// a frame is fitted into it and centred.
    #[test]
    fn loupe_fits_the_frame_to_the_screen() {
        let area = frame_area((1440.0, 900.0), false, false);
        assert_eq!(area.y, 44.0 + TOP_INSET + INFO_BAR_HEIGHT + BAR_GAP);
        // The board's 641 pt, less its title bar's point more and status bar's point less.
        assert_eq!(area.height, 643.0, "{area:?}");
        let picture = fitted(area, 6000, 4000);
        assert!((picture.height - area.height).abs() < 1.0, "height-bound");
        assert!((picture.x - (area.x + (area.width - picture.width) / 2.0)).abs() <= 1.0);
        let panels = frame_area((1440.0, 900.0), true, true);
        assert!(panels.width < area.width && panels.x > area.x);
        assert_eq!(window(6, 2, 8), 0..6);
        assert_eq!(window(1000, 499, 5), 497..502);
        assert!(strip_capacity((1440.0, 900.0), false, false) >= 6);
    }
}
