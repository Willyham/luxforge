//! The transient crop draft: the single owner of everything a crop gesture changes before Apply.
//!
//! This module holds no framework types. Every geometric decision is delegated to
//! [`lightwell_core::CropStage`], so the desktop and the crop module share exactly one
//! implementation of the rotated box, coverage and fitting math. What lives here is the state
//! machine: which handle a gesture grabbed, what the rectangle looked like when it started, and
//! which reference an angle change refits.
//!
//! Box space is the rotated bounding box of the crop layer's input stage in whole pixels, with a
//! top-left origin, x right and y down. Every rectangle this module produces has passed
//! [`CropStage::fit_about_center`], so it is covered by the source, snapped to whole box pixels and
//! accepted by [`CropPayload::output_rect`].
use lightwell_core::{
    BoxRect, CropPayload, CropStage, Edge, LayerId, MAX_ANGLE, MIN_ANGLE, OutputRect, guide_angle,
    largest_with_ratio_inside,
};
use serde_json::{Value, json};

/// The smallest extent a gesture may leave on either axis, in box pixels.
pub(crate) const MIN_EXTENT: f64 = 1.0;

/// The special words the crop module's `aspect` enum uses for the ratios that are not `W:H`.
/// `free` is also the option a draft shows as chosen when its ratio is not locked.
pub(crate) const FREE: &str = "free";
const ORIGINAL: &str = "original";
const CUSTOM: &str = "custom";

/// Whether the rectangle's width to height ratio is pinned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Aspect {
    Free,
    /// Width divided by height; always finite and positive.
    Locked(f64),
}

impl Aspect {
    pub(crate) fn ratio(self) -> Option<f64> {
        match self {
            Self::Free => None,
            Self::Locked(ratio) => Some(ratio),
        }
    }
}

/// What one option of the module's `aspect` enum means. The list of presets is generated from the
/// descriptor, so the desktop hard-codes no ratio.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PresetKind {
    Free,
    /// The input stage's own ratio, which already reflects preceding quarter-turns.
    Original,
    /// A literal `W:H` option.
    Ratio(f64),
    /// `custom`, whose two extents come from the panel's own fields.
    Custom,
}

/// One ratio preset: the enum option exactly as the descriptor spells it and what it means.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AspectPreset {
    pub(crate) option: String,
    pub(crate) kind: PresetKind,
}

impl AspectPreset {
    /// The button label: the declared option, with the two special words capitalized.
    pub(crate) fn label(&self) -> String {
        match self.kind {
            PresetKind::Free => "Free".into(),
            PresetKind::Original => "Original".into(),
            PresetKind::Custom => "Custom".into(),
            PresetKind::Ratio(_) => self.option.clone(),
        }
    }
}

/// `W:H` as a ratio; only finite, positive extents are a ratio.
pub(crate) fn parse_ratio(option: &str) -> Option<f64> {
    let (width, height) = option.split_once(':')?;
    let width: f64 = width.trim().parse().ok()?;
    let height: f64 = height.trim().parse().ok()?;
    (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
        .then_some(width / height)
}

/// The presets the panel offers, derived from the declared `aspect` enum options. An option that is
/// neither a special word nor a `W:H` ratio is dropped rather than guessed at.
pub(crate) fn aspect_presets(options: &[String]) -> Vec<AspectPreset> {
    options
        .iter()
        .filter_map(|option| {
            let kind = match option.as_str() {
                FREE => PresetKind::Free,
                ORIGINAL => PresetKind::Original,
                CUSTOM => PresetKind::Custom,
                other => PresetKind::Ratio(parse_ratio(other)?),
            };
            Some(AspectPreset {
                option: option.clone(),
                kind,
            })
        })
        .collect()
}

/// The declared ratio a committed whole-pixel rectangle reads as, with the width over height a
/// draft locks to for it, or `None` when it reads as Free. The recipe stores the rectangle, not
/// the preset that produced it, so this is how the crop section and a draft opened on an existing
/// crop agree on which chip is chosen.
///
/// Presets are tried in declared order, so `original` (the input stage's ratio) wins over a `W:H`
/// option equal to it, and each in either orientation, since a swapped ratio keeps its preset.
/// Free and Custom name no ratio of their own and never match. The rectangle has a ratio `r` (as
/// its long side over its short one) when the long side is within `2r` pixels of the short side
/// times `r`: fitting a ratio snaps each extent inward by less than two pixels, and a ratio-locked
/// drag keeps it within one pixel plus the ratio, so every rectangle a preset or a locked gesture
/// produced reads as that preset again.
pub(crate) fn committed_aspect(
    presets: &[AspectPreset],
    stage: (u32, u32),
    output: (u32, u32),
) -> Option<(&AspectPreset, f64)> {
    let (width, height) = (f64::from(output.0.max(1)), f64::from(output.1.max(1)));
    let landscape = width >= height;
    let (long, short) = if landscape {
        (width, height)
    } else {
        (height, width)
    };
    presets.iter().find_map(|preset| {
        let ratio = match preset.kind {
            PresetKind::Original => f64::from(stage.0.max(1)) / f64::from(stage.1.max(1)),
            PresetKind::Ratio(ratio) => ratio,
            PresetKind::Free | PresetKind::Custom => return None,
        };
        let wide = ratio.max(1.0 / ratio);
        ((long - wide * short).abs() <= 2.0 * wide)
            .then_some((preset, if landscape { wide } else { 1.0 / wide }))
    })
}

/// Which corner of the rectangle a drag grabbed; the opposite corner stays fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub(crate) const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    /// The two edges this corner moves: the vertical one first.
    fn edges(self) -> (Edge, Edge) {
        match self {
            Self::TopLeft => (Edge::Left, Edge::Top),
            Self::TopRight => (Edge::Right, Edge::Top),
            Self::BottomLeft => (Edge::Left, Edge::Bottom),
            Self::BottomRight => (Edge::Right, Edge::Bottom),
        }
    }

    pub(crate) fn point(self, rect: &BoxRect) -> (f64, f64) {
        let (horizontal, vertical) = self.edges();
        (edge_line(horizontal, rect), edge_line(vertical, rect))
    }

    fn opposite(self) -> Self {
        match self {
            Self::TopLeft => Self::BottomRight,
            Self::TopRight => Self::BottomLeft,
            Self::BottomLeft => Self::TopRight,
            Self::BottomRight => Self::TopLeft,
        }
    }
}

/// What a pointer press grabbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handle {
    Corner(Corner),
    Side(Edge),
    /// Move the composition inside the source.
    Move,
    /// Draw a straightening guide; only [`CropDraft::end`] changes the angle.
    Guide,
}

/// The modifier keys a drag is evaluated with. Option (Alt) turns any handle into a uniform scale
/// about the fixed center.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modifiers {
    pub(crate) option: bool,
}

/// One gesture in flight. Every `drag` is evaluated against `start_rect`, never against the
/// previous pointer position, so a drag away and back returns the starting rectangle exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Gesture {
    handle: Handle,
    start_rect: BoxRect,
    start_point: (f64, f64),
    /// Where the pointer is now; only a guide gesture reads it.
    point: (f64, f64),
}

/// The whole crop editor state between opening a draft and Apply or Cancel.
#[derive(Clone, Debug)]
pub(crate) struct CropDraft {
    /// The crop layer's input stage seen through the current draft angle.
    pub(crate) stage: CropStage,
    /// The crop rectangle in whole box pixels; always covered by the rotated source.
    pub(crate) rect: BoxRect,
    /// The last frame gesture's result. Every angle change refits this, so sweeping the angle away
    /// and back returns the reference rectangle with no cumulative trim.
    reference: (CropStage, BoxRect),
    pub(crate) aspect: Aspect,
    /// The declared `aspect` option the panel currently shows as chosen.
    pub(crate) preset: String,
    /// The revision this draft was opened against; Apply expects it.
    pub(crate) base_revision: u64,
    /// The existing crop layer being edited, or `None` when Apply will append one.
    pub(crate) layer: Option<LayerId>,
    /// How many layers precede the crop layer: the preview truncation that shows its input stage.
    pub(crate) layer_index: usize,
    /// Something else changed the asset; Apply is refused until Discard or Reapply.
    pub(crate) conflicted: bool,
    gesture: Option<Gesture>,
}

impl CropDraft {
    /// Edit an existing crop layer: the rectangle starts at the payload's own output rectangle, so
    /// reopening a draft shows exactly what was committed, and the ratio it reads as among the
    /// declared presets ([`committed_aspect`]) is chosen and locked without refitting it. A
    /// neutral payload is no crop at all and starts Free, as a draft on a stack without one does.
    pub(crate) fn from_layer(
        input: CropStage,
        payload: CropPayload,
        layer: LayerId,
        layer_index: usize,
        base_revision: u64,
        presets: &[AspectPreset],
    ) -> Self {
        let stage = CropStage {
            angle: payload.angle.clamp(MIN_ANGLE, MAX_ANGLE),
            ..input
        };
        // A valid payload reopens as exactly the rectangle it committed, rounding included; only an
        // unfittable one is fitted, and fitting never enlarges it.
        let (box_width, box_height) = stage.bounding_box();
        let rect = match payload.output_rect(&stage) {
            Ok(output) => BoxRect {
                x: output.x as f64,
                y: output.y as f64,
                width: f64::from(output.width),
                height: f64::from(output.height),
            },
            Err(_) => stage.fit_about_center(BoxRect {
                x: payload.x * box_width,
                y: payload.y * box_height,
                width: payload.width * box_width,
                height: payload.height * box_height,
            }),
        };
        let mut draft = Self::seeded(stage, rect, Some(layer), layer_index, base_revision);
        if !payload.is_neutral()
            && let Ok(output) = draft.output()
            && let Some((preset, ratio)) = committed_aspect(
                presets,
                (input.width, input.height),
                (output.width, output.height),
            )
        {
            draft.preset = preset.option.clone();
            draft.aspect = Aspect::Locked(ratio);
        }
        draft
    }

    /// Start a draft on a stack without a crop layer: no straightening and the whole stage.
    pub(crate) fn neutral(input: CropStage, base_revision: u64, layer_count: usize) -> Self {
        let stage = CropStage {
            angle: 0.0,
            ..input
        };
        let (box_width, box_height) = stage.bounding_box();
        let rect = stage.fit_about_center(BoxRect {
            x: 0.0,
            y: 0.0,
            width: box_width,
            height: box_height,
        });
        Self::seeded(stage, rect, None, layer_count, base_revision)
    }

    fn seeded(
        stage: CropStage,
        rect: BoxRect,
        layer: Option<LayerId>,
        layer_index: usize,
        base_revision: u64,
    ) -> Self {
        Self {
            stage,
            rect,
            reference: (stage, rect),
            aspect: Aspect::Free,
            preset: FREE.into(),
            base_revision,
            layer,
            layer_index,
            conflicted: false,
            gesture: None,
        }
    }

    pub(crate) fn payload(&self) -> CropPayload {
        self.rect.normalized(&self.stage)
    }

    /// The whole-pixel stage this draft would commit, or the validation error naming the corner
    /// that leaves the source. A drafted rectangle is always covered, so this is `Ok`.
    pub(crate) fn output(&self) -> Result<OutputRect, lightwell_core::Error> {
        self.payload().output_rect(&self.stage)
    }

    pub(crate) fn mark_conflicted(&mut self) {
        self.conflicted = true;
        self.gesture = None;
    }

    /// The line a guide gesture has drawn so far, in box pixels.
    pub(crate) fn guide_line(&self) -> Option<((f64, f64), (f64, f64))> {
        self.gesture
            .filter(|gesture| gesture.handle == Handle::Guide)
            .map(|gesture| (gesture.start_point, gesture.point))
    }

    pub(crate) fn dragging(&self) -> bool {
        self.gesture.is_some()
    }

    /// Which handle a press at this box point grabbed. `tolerance` is the hit radius in box pixels;
    /// corners win over sides, and anything else moves the composition.
    pub(crate) fn hit(&self, point: (f64, f64), tolerance: f64) -> Handle {
        let tolerance = tolerance.max(0.0);
        for corner in Corner::ALL {
            let (x, y) = corner.point(&self.rect);
            if (point.0 - x).abs() <= tolerance && (point.1 - y).abs() <= tolerance {
                return Handle::Corner(corner);
            }
        }
        let inside_x = point.0 >= self.rect.x - tolerance
            && point.0 <= self.rect.x + self.rect.width + tolerance;
        let inside_y = point.1 >= self.rect.y - tolerance
            && point.1 <= self.rect.y + self.rect.height + tolerance;
        for edge in [Edge::Left, Edge::Right] {
            if (point.0 - edge_line(edge, &self.rect)).abs() <= tolerance && inside_y {
                return Handle::Side(edge);
            }
        }
        for edge in [Edge::Top, Edge::Bottom] {
            if (point.1 - edge_line(edge, &self.rect)).abs() <= tolerance && inside_x {
                return Handle::Side(edge);
            }
        }
        Handle::Move
    }

    /// Start a gesture, snapshotting the rectangle every later `drag` is measured against.
    pub(crate) fn begin(&mut self, handle: Handle, point: (f64, f64)) {
        self.gesture = Some(Gesture {
            handle,
            start_rect: self.rect,
            start_point: point,
            point,
        });
    }

    /// Re-evaluate the gesture at a new pointer position. Nothing is committed and no API is called.
    pub(crate) fn drag(&mut self, point: (f64, f64), modifiers: Modifiers) {
        let Some(mut gesture) = self.gesture else {
            return;
        };
        if !point.0.is_finite() || !point.1.is_finite() {
            return;
        }
        gesture.point = point;
        self.gesture = Some(gesture);
        if gesture.handle == Handle::Guide {
            return;
        }
        let delta = (
            point.0 - gesture.start_point.0,
            point.1 - gesture.start_point.1,
        );
        let start = gesture.start_rect;
        let rect = if modifiers.option {
            self.option_scaled(start, gesture.handle, delta)
        } else {
            match gesture.handle {
                Handle::Move => self.moved(start, delta),
                Handle::Side(edge) => match self.aspect.ratio() {
                    None => self.free_side(start, edge, delta),
                    Some(_) => self.locked_side(start, edge, delta),
                },
                Handle::Corner(corner) => match self.aspect.ratio() {
                    None => self.free_corner(start, corner, delta),
                    Some(_) => self.locked_corner(start, corner, delta),
                },
                Handle::Guide => start,
            }
        };
        self.rect = self.stage.fit_about_center(rect);
    }

    /// Finish the gesture. A frame gesture becomes the new reference for later angle changes; a
    /// guide commits its angle instead and leaves the reference alone.
    pub(crate) fn end(&mut self) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        if gesture.handle == Handle::Guide {
            let angle = guide_angle(self.stage.angle, gesture.start_point, gesture.point);
            self.set_angle(angle);
            return;
        }
        self.remember();
    }

    /// Adopt the current rectangle and angle as the reference for later angle changes.
    fn remember(&mut self) {
        self.reference = (self.stage, self.rect);
    }

    /// Set the straightening angle in degrees. The rectangle is always the reference refitted at
    /// the new angle, never the previous rectangle, so no trim accumulates.
    pub(crate) fn set_angle(&mut self, degrees: f64) {
        if !degrees.is_finite() {
            return;
        }
        self.stage.angle = degrees.clamp(MIN_ANGLE, MAX_ANGLE);
        self.rect = self.stage.refit(&self.reference.0, self.reference.1);
    }

    pub(crate) fn nudge_angle(&mut self, degrees: f64) {
        self.set_angle(self.stage.angle + degrees);
    }

    /// Choose a declared ratio preset: the center is kept and the largest rectangle of that ratio
    /// inside the current one becomes the new frame reference.
    pub(crate) fn set_preset(&mut self, preset: &AspectPreset, custom: Option<(f64, f64)>) {
        let aspect = match preset.kind {
            PresetKind::Free => Aspect::Free,
            PresetKind::Original => Aspect::Locked(
                f64::from(self.stage.width.max(1)) / f64::from(self.stage.height.max(1)),
            ),
            PresetKind::Ratio(ratio) => Aspect::Locked(ratio),
            PresetKind::Custom => match custom {
                Some((width, height))
                    if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 =>
                {
                    Aspect::Locked(width / height)
                }
                _ => return,
            },
        };
        self.preset = preset.option.clone();
        self.apply_aspect(aspect);
    }

    /// Invert the locked ratio, keeping the center and fitting inside the current rectangle.
    pub(crate) fn swap(&mut self) {
        if let Aspect::Locked(ratio) = self.aspect
            && ratio.is_finite()
            && ratio > 0.0
        {
            self.apply_aspect(Aspect::Locked(1.0 / ratio));
        }
    }

    /// Lock the rectangle's current ratio, or release the lock.
    pub(crate) fn lock_toggle(&mut self) {
        match self.aspect {
            Aspect::Locked(_) => {
                self.aspect = Aspect::Free;
                self.preset = FREE.into();
            }
            Aspect::Free => {
                let ratio = self.rect.width / self.rect.height;
                if ratio.is_finite() && ratio > 0.0 {
                    self.apply_aspect(Aspect::Locked(ratio));
                    self.preset = CUSTOM.into();
                }
            }
        }
    }

    fn apply_aspect(&mut self, aspect: Aspect) {
        self.aspect = aspect;
        if let Aspect::Locked(ratio) = aspect {
            let wanted = largest_with_ratio_inside(self.rect, ratio);
            self.rect = self.stage.fit_about_center(clamped_extents(wanted));
        }
        self.gesture = None;
        self.remember();
    }

    /// Point the draft at a new input stage and revision after something else committed: the angle
    /// and the pixel extents are kept and the center keeps addressing the same input point, so a
    /// stage of a different size keeps the composition as far as it fits.
    pub(crate) fn rebase(
        &mut self,
        input: CropStage,
        base_revision: u64,
        layer: Option<LayerId>,
        layer_index: usize,
    ) {
        let stage = CropStage {
            angle: self.stage.angle,
            ..input
        };
        let center = self.rect.center();
        let (u, v) = self.stage.to_input(center.0, center.1);
        let rect = stage.fit_about_center(BoxRect::from_center(
            stage.to_box(u, v),
            self.rect.width,
            self.rect.height,
        ));
        self.stage = stage;
        self.rect = rect;
        self.reference = (stage, rect);
        self.base_revision = base_revision;
        self.layer = layer;
        self.layer_index = layer_index;
        self.conflicted = false;
        self.gesture = None;
    }

    /// Correlated evidence: what the draft holds when a frame is captured or an event is logged.
    pub(crate) fn summary(&self) -> Value {
        let payload = self.payload();
        let output = self
            .output()
            .ok()
            .map(|rect| json!([rect.width, rect.height]));
        json!({
            "layer": self.layer.as_ref().map(|id| id.as_str().to_owned()),
            "layer_index": self.layer_index,
            "base_revision": self.base_revision,
            "input_stage": [self.stage.width, self.stage.height],
            "angle": self.stage.angle,
            "rect": [self.rect.x, self.rect.y, self.rect.width, self.rect.height],
            "payload": {"angle": payload.angle, "x": payload.x, "y": payload.y, "width": payload.width, "height": payload.height},
            "output": output,
            "preset": self.preset,
            "ratio": self.aspect.ratio(),
            "conflicted": self.conflicted,
        })
    }

    // ---- gesture kinds -------------------------------------------------------------------------

    /// Free mode, one side: the opposite edge is fixed and only the dragged extent changes, bounded
    /// by the source and by the one-pixel minimum. The extent is rounded first so the fixed edge
    /// never drifts when the result is snapped.
    fn free_side(&self, start: BoxRect, edge: Edge, delta: (f64, f64)) -> BoxRect {
        let wanted = extent_of(start, edge) + signed(edge) * along(edge, delta);
        with_extent(
            start,
            edge,
            whole_extent(wanted, self.stage.max_extent(start, edge)),
        )
    }

    /// Free mode, one corner: the x extent is clamped first and the y extent against the rectangle
    /// that already has it, which is the order the fitting contract prescribes.
    fn free_corner(&self, start: BoxRect, corner: Corner, delta: (f64, f64)) -> BoxRect {
        let (horizontal, vertical) = corner.edges();
        let wide = self.free_side(start, horizontal, delta);
        self.free_side(wide, vertical, delta)
    }

    /// Locked ratio, one side: the opposite edge stays fixed, the dragged extent sets one scale
    /// factor and the perpendicular extent follows it about the fixed edge's midpoint. The scale
    /// multiplies the rectangle's own extents, so what it preserves is the width to height on
    /// screen; whole-pixel snapping holds that within a pixel of the chosen ratio, and choosing the
    /// preset again re-imposes the ratio exactly.
    fn locked_side(&self, start: BoxRect, edge: Edge, delta: (f64, f64)) -> BoxRect {
        let extent = extent_of(start, edge);
        let across = if horizontal(edge) {
            start.height
        } else {
            start.width
        };
        if extent <= 0.0 || across <= 0.0 || !extent.is_finite() || !across.is_finite() {
            return start;
        }
        // The primary axis carries the whole dragged extent away from the fixed edge; the
        // perpendicular axis carries half of the other extent on each side of its midpoint.
        let direction = signed(edge);
        let (primary, half) = if horizontal(edge) {
            ((direction * extent, 0.0), (0.0, across / 2.0))
        } else {
            ((0.0, direction * extent), (across / 2.0, 0.0))
        };
        let anchor = fixed_midpoint(edge, &start);
        let wanted = extent + direction * along(edge, delta);
        // Two anchored rectangles, one per side of the fixed edge's midpoint, together cover all
        // four corners. The two extra rays they add lie between covered corners, so by convexity
        // they never bind first and the bound stays exact.
        let limit = self
            .stage
            .max_scale_about_anchor(anchor, (primary.0 + half.0, primary.1 + half.1))
            .min(
                self.stage
                    .max_scale_about_anchor(anchor, (primary.0 - half.0, primary.1 - half.1)),
            );
        let scale = clamp_scale(wanted / extent, limit, extent, across);
        span([
            offset(anchor, primary, half, scale, 1.0),
            offset(anchor, primary, half, scale, -1.0),
            offset(anchor, (0.0, 0.0), half, scale, 1.0),
            offset(anchor, (0.0, 0.0), half, scale, -1.0),
        ])
    }

    /// Locked ratio, one corner: the opposite corner stays fixed and the size comes from the
    /// pointer's projection onto the ratio diagonal.
    fn locked_corner(&self, start: BoxRect, corner: Corner, delta: (f64, f64)) -> BoxRect {
        let anchor = corner.opposite().point(&start);
        let grabbed = corner.point(&start);
        let diagonal = (grabbed.0 - anchor.0, grabbed.1 - anchor.1);
        let square = diagonal.0 * diagonal.0 + diagonal.1 * diagonal.1;
        if square <= 0.0 || !square.is_finite() {
            return start;
        }
        let target = (
            grabbed.0 + delta.0 - anchor.0,
            grabbed.1 + delta.1 - anchor.1,
        );
        let wanted = (target.0 * diagonal.0 + target.1 * diagonal.1) / square;
        let limit = self.stage.max_scale_about_anchor(anchor, diagonal);
        let scale = clamp_scale(wanted, limit, start.width, start.height);
        span([
            anchor,
            (anchor.0 + scale * diagonal.0, anchor.1 + scale * diagonal.1),
        ])
    }

    /// Option held: one scale factor about the fixed center, preserving the current width to height
    /// even in Free mode. The center never moves.
    fn option_scaled(&self, start: BoxRect, handle: Handle, delta: (f64, f64)) -> BoxRect {
        let center = start.center();
        let reach = match handle {
            Handle::Corner(corner) => {
                let (x, y) = corner.point(&start);
                (x - center.0, y - center.1)
            }
            Handle::Side(edge) => {
                let (x, y) = edge_midpoint(edge, &start);
                (x - center.0, y - center.1)
            }
            // A move is a move: Option scales the eight handles, not the composition.
            Handle::Move | Handle::Guide => return self.moved(start, delta),
        };
        let square = reach.0 * reach.0 + reach.1 * reach.1;
        if square <= 0.0 || !square.is_finite() {
            return start;
        }
        let target = (reach.0 + delta.0, reach.1 + delta.1);
        let wanted = (target.0 * reach.0 + target.1 * reach.1) / square;
        let limit = self
            .stage
            .max_scale_about(center, start.width / 2.0, start.height / 2.0);
        let scale = clamp_scale(wanted, limit, start.width, start.height);
        BoxRect::from_center(center, start.width * scale, start.height * scale)
    }

    /// Move the composition: the horizontal delta is clamped first and the vertical delta against
    /// the already shifted rectangle, so a move into a boundary slides along it. The travel is
    /// truncated to whole box pixels, which can only stay inside the clamp.
    fn moved(&self, start: BoxRect, delta: (f64, f64)) -> BoxRect {
        let (dx, dy) = self
            .stage
            .clamp_translation(start, delta.0.round(), delta.1.round());
        start.translated(dx.trunc(), dy.trunc())
    }
}

/// The coordinate of one edge: an x for the vertical edges, a y for the horizontal ones.
fn edge_line(edge: Edge, rect: &BoxRect) -> f64 {
    match edge {
        Edge::Left => rect.x,
        Edge::Right => rect.x + rect.width,
        Edge::Top => rect.y,
        Edge::Bottom => rect.y + rect.height,
    }
}

pub(crate) fn edge_midpoint(edge: Edge, rect: &BoxRect) -> (f64, f64) {
    match edge {
        Edge::Left | Edge::Right => (edge_line(edge, rect), rect.y + rect.height / 2.0),
        Edge::Top | Edge::Bottom => (rect.x + rect.width / 2.0, edge_line(edge, rect)),
    }
}

/// The midpoint of the edge a drag on `edge` keeps fixed.
fn fixed_midpoint(edge: Edge, rect: &BoxRect) -> (f64, f64) {
    edge_midpoint(opposite(edge), rect)
}

fn opposite(edge: Edge) -> Edge {
    match edge {
        Edge::Left => Edge::Right,
        Edge::Right => Edge::Left,
        Edge::Top => Edge::Bottom,
        Edge::Bottom => Edge::Top,
    }
}

fn horizontal(edge: Edge) -> bool {
    matches!(edge, Edge::Left | Edge::Right)
}

/// Which way the extent grows when the pointer moves in the positive direction of the drag axis.
fn signed(edge: Edge) -> f64 {
    match edge {
        Edge::Right | Edge::Bottom => 1.0,
        Edge::Left | Edge::Top => -1.0,
    }
}

fn along(edge: Edge, delta: (f64, f64)) -> f64 {
    if horizontal(edge) { delta.0 } else { delta.1 }
}

fn extent_of(rect: BoxRect, edge: Edge) -> f64 {
    if horizontal(edge) {
        rect.width
    } else {
        rect.height
    }
}

/// The rectangle with one edge moved to leave `extent`, the opposite edge fixed.
fn with_extent(rect: BoxRect, edge: Edge, extent: f64) -> BoxRect {
    match edge {
        Edge::Left => BoxRect {
            x: rect.x + rect.width - extent,
            width: extent,
            ..rect
        },
        Edge::Right => BoxRect {
            width: extent,
            ..rect
        },
        Edge::Top => BoxRect {
            y: rect.y + rect.height - extent,
            height: extent,
            ..rect
        },
        Edge::Bottom => BoxRect {
            height: extent,
            ..rect
        },
    }
}

/// A whole-pixel extent within the minimum and the source limit. Rounding is preferred for feel but
/// never allowed to exceed the limit, so the result is always covered.
fn whole_extent(wanted: f64, limit: f64) -> f64 {
    let limit = limit.max(MIN_EXTENT);
    let bounded = wanted.clamp(MIN_EXTENT, limit);
    let rounded = bounded.round().max(MIN_EXTENT);
    if rounded <= limit {
        rounded
    } else {
        bounded.floor().max(MIN_EXTENT)
    }
}

/// A uniform scale factor within the source limit and the one-pixel minimum on both axes.
fn clamp_scale(wanted: f64, limit: f64, width: f64, height: f64) -> f64 {
    if !wanted.is_finite() {
        return 1.0;
    }
    let smallest =
        (MIN_EXTENT / width.max(f64::MIN_POSITIVE)).max(MIN_EXTENT / height.max(f64::MIN_POSITIVE));
    let largest = if limit.is_finite() {
        limit.max(smallest)
    } else {
        f64::MAX
    };
    wanted.clamp(smallest, largest)
}

fn offset(
    anchor: (f64, f64),
    primary: (f64, f64),
    half: (f64, f64),
    scale: f64,
    side: f64,
) -> (f64, f64) {
    (
        anchor.0 + scale * (primary.0 + side * half.0),
        anchor.1 + scale * (primary.1 + side * half.1),
    )
}

/// The axis-aligned rectangle spanned by a set of points.
fn span<const N: usize>(points: [(f64, f64); N]) -> BoxRect {
    let mut min = points[0];
    let mut max = points[0];
    for (x, y) in points {
        min = (min.0.min(x), min.1.min(y));
        max = (max.0.max(x), max.1.max(y));
    }
    BoxRect {
        x: min.0,
        y: min.1,
        width: (max.0 - min.0).max(MIN_EXTENT),
        height: (max.1 - min.1).max(MIN_EXTENT),
    }
}

/// A rectangle with both extents at least the one-pixel minimum, keeping its center.
fn clamped_extents(rect: BoxRect) -> BoxRect {
    BoxRect::from_center(
        rect.center(),
        rect.width.max(MIN_EXTENT),
        rect.height.max(MIN_EXTENT),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGES: [(u32, u32); 2] = [(480, 320), (320, 480)];
    const ANGLES: [f64; 2] = [0.0, 7.0];
    const PRESETS: [&str; 7] = ["free", "original", "1:1", "3:2", "4:3", "16:9", "custom"];

    fn presets() -> Vec<AspectPreset> {
        aspect_presets(&PRESETS.map(String::from))
    }

    fn preset(option: &str) -> AspectPreset {
        presets()
            .into_iter()
            .find(|preset| preset.option == option)
            .expect("a declared preset")
    }

    /// A draft on a stage of that size at that angle, started from the neutral payload and then
    /// straightened, so its reference is the whole stage.
    fn draft(width: u32, height: u32, angle: f64) -> CropDraft {
        let mut draft = CropDraft::neutral(
            CropStage {
                width,
                height,
                angle: 0.0,
            },
            3,
            2,
        );
        draft.set_angle(angle);
        draft
    }

    /// A rectangle matches a ratio when its extents agree to within the pixel that inward snapping
    /// can take off each of them.
    fn matches_ratio(rect: &BoxRect, ratio: f64, what: &str) {
        let slack = 1.0 + ratio.max(1.0);
        assert!(
            (rect.width - rect.height * ratio).abs() <= slack,
            "{what}: {rect:?} is not {ratio} within {slack} px"
        );
    }

    /// Every invariant a drafted rectangle carries, checked after every gesture in these tests.
    fn check(draft: &CropDraft, what: &str) {
        let rect = draft.rect;
        assert!(
            draft.stage.covers(&rect),
            "{what}: {rect:?} is not covered on {:?}",
            draft.stage
        );
        for value in [rect.x, rect.y, rect.width, rect.height] {
            assert_eq!(value, value.round(), "{what}: {rect:?} is not whole pixels");
        }
        assert!(
            rect.width >= MIN_EXTENT && rect.height >= MIN_EXTENT,
            "{what}: {rect:?} is smaller than one pixel"
        );
        draft
            .output()
            .unwrap_or_else(|error| panic!("{what}: {rect:?} is not a valid payload: {error}"));
    }

    fn gesture(
        draft: &mut CropDraft,
        handle: Handle,
        from: (f64, f64),
        to: (f64, f64),
        modifiers: Modifiers,
    ) {
        draft.begin(handle, from);
        draft.drag(to, modifiers);
        draft.end();
    }

    /// Pull one corner to an absolute box point with a whole gesture.
    fn pull(draft: &mut CropDraft, corner: Corner, to: (f64, f64)) {
        let from = corner.point(&draft.rect);
        gesture(
            draft,
            Handle::Corner(corner),
            from,
            to,
            Modifiers::default(),
        );
    }

    /// Pull one corner to a fraction of the rectangle, leaving room to drag either way afterwards.
    fn seed(draft: &mut CropDraft, corner: Corner, fraction: (f64, f64)) {
        let to = (
            draft.rect.x + draft.rect.width * fraction.0,
            draft.rect.y + draft.rect.height * fraction.1,
        );
        pull(draft, corner, to);
    }

    fn handles() -> Vec<Handle> {
        let mut handles: Vec<Handle> = Corner::ALL.into_iter().map(Handle::Corner).collect();
        handles.extend(
            [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom]
                .into_iter()
                .map(Handle::Side),
        );
        handles.push(Handle::Move);
        handles
    }

    /// Where a press on this handle lands: a corner, an edge midpoint or the rectangle's center.
    fn grip(handle: Handle, rect: &BoxRect) -> (f64, f64) {
        match handle {
            Handle::Corner(corner) => corner.point(rect),
            Handle::Side(edge) => edge_midpoint(edge, rect),
            Handle::Move | Handle::Guide => rect.center(),
        }
    }

    #[test]
    fn presets_come_from_the_declared_enum_options() {
        let presets = presets();
        assert_eq!(presets.len(), PRESETS.len());
        assert_eq!(presets[0].kind, PresetKind::Free);
        assert_eq!(presets[1].kind, PresetKind::Original);
        assert_eq!(presets[2].kind, PresetKind::Ratio(1.0));
        assert_eq!(presets[3].kind, PresetKind::Ratio(1.5));
        assert_eq!(presets[4].kind, PresetKind::Ratio(4.0 / 3.0));
        assert_eq!(presets[5].kind, PresetKind::Ratio(16.0 / 9.0));
        assert_eq!(presets[6].kind, PresetKind::Custom);
        assert_eq!(presets[0].label(), "Free");
        assert_eq!(presets[3].label(), "3:2");
        // An option the desktop cannot read is dropped, never guessed at.
        assert!(aspect_presets(&["wide".to_string(), "0:3".into(), "1:0".into()]).is_empty());
        assert_eq!(parse_ratio(" 3 : 2 "), Some(1.5));
        assert_eq!(parse_ratio("3"), None);
    }

    #[test]
    fn a_neutral_draft_starts_at_the_whole_stage_and_an_existing_layer_at_its_payload() {
        for (width, height) in STAGES {
            let input = CropStage {
                width,
                height,
                angle: 0.0,
            };
            let neutral = CropDraft::neutral(input, 9, 4);
            assert_eq!(neutral.stage.angle, 0.0);
            assert_eq!(neutral.layer_index, 4);
            assert_eq!(neutral.base_revision, 9);
            assert!(neutral.layer.is_none());
            assert_eq!(neutral.aspect, Aspect::Free);
            assert_eq!(
                neutral.rect,
                BoxRect {
                    x: 0.0,
                    y: 0.0,
                    width: f64::from(width),
                    height: f64::from(height)
                }
            );
            check(&neutral, "neutral");
            assert_eq!(neutral.payload(), CropPayload::NEUTRAL);

            // Reopening an existing layer shows exactly the rectangle it committed.
            let payload = CropPayload {
                angle: 7.0,
                x: 0.2,
                y: 0.25,
                width: 0.4,
                height: 0.3,
            };
            let stage = CropStage {
                angle: 7.0,
                ..input
            };
            let expected = payload.output_rect(&stage).expect("a covered payload");
            let layer = LayerId::new();
            let draft = CropDraft::from_layer(input, payload, layer.clone(), 1, 12, &presets());
            assert_eq!(draft.layer, Some(layer));
            assert_eq!(draft.layer_index, 1);
            assert_eq!(draft.stage.angle, 7.0);
            check(&draft, "from_layer");
            assert_eq!(draft.output().expect("a valid draft"), expected);
        }
    }

    /// Reopen the payload a draft would commit, as a later draft on that crop layer would.
    fn reopened(draft: &CropDraft) -> CropDraft {
        let input = CropStage {
            angle: 0.0,
            ..draft.stage
        };
        CropDraft::from_layer(input, draft.payload(), LayerId::new(), 1, 12, &presets())
    }

    /// Every ratio a draft can choose, in either orientation and after a ratio-locked drag, reads
    /// as that same preset when its committed payload is reopened: the chip stays chosen and the
    /// lock stays closed on the same ratio, and the rectangle is exactly the committed one.
    #[test]
    fn a_reopened_crop_seeds_the_ratio_it_was_committed_with() {
        for (width, height) in STAGES {
            for angle in ANGLES {
                for option in ["original", "1:1", "3:2", "4:3", "16:9"] {
                    for swapped in [false, true] {
                        for dragged in [false, true] {
                            let mut draft = draft(width, height, angle);
                            seed(&mut draft, Corner::TopLeft, (0.2, 0.15));
                            draft.set_preset(&preset(option), None);
                            if swapped {
                                draft.swap();
                            }
                            if dragged {
                                let from = Corner::BottomRight.point(&draft.rect);
                                gesture(
                                    &mut draft,
                                    Handle::Corner(Corner::BottomRight),
                                    from,
                                    (from.0 - 37.0, from.1 - 23.0),
                                    Modifiers::default(),
                                );
                            }
                            let what = format!(
                                "{option} on {width}x{height} at {angle}°, swapped {swapped}, dragged {dragged}"
                            );
                            check(&draft, &what);
                            let reopened = reopened(&draft);
                            assert_eq!(reopened.rect, draft.rect, "{what}: the rectangle moved");
                            // Both stages are 3:2, so a 3:2 crop reads as Original, which the
                            // descriptor declares first, and locks the same ratio.
                            let expected = if option == "3:2" { "original" } else { option };
                            assert_eq!(reopened.preset, expected, "{what}");
                            let ratio = draft.aspect.ratio().expect("a locked ratio");
                            let seeded = reopened
                                .aspect
                                .ratio()
                                .unwrap_or_else(|| panic!("{what}: not locked"));
                            assert!(
                                (seeded - ratio).abs() <= 1e-9,
                                "{what}: locked {seeded}, chosen {ratio}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// A rectangle that no declared ratio produces reads as Free, and so does the neutral payload,
    /// whose whole-stage rectangle has the Original ratio but is no crop at all. Original is
    /// declared before the `W:H` options, so a crop at the stage's own ratio reads as Original
    /// even when a `W:H` option names the same ratio.
    #[test]
    fn a_free_or_neutral_crop_reads_as_free_and_original_wins_over_an_equal_ratio() {
        let presets = presets();
        let free = |stage: (u32, u32), output: (u32, u32)| {
            committed_aspect(&presets, stage, output).map(|(preset, _)| preset.option.clone())
        };
        // 258 × 169 is 1.527: four and a half pixels from 3:2, beyond the three a 3:2 fit allows.
        assert_eq!(free((480, 320), (258, 169)), None);
        assert_eq!(free((480, 320), (300, 200)), Some("original".to_owned()));
        assert_eq!(free((480, 320), (200, 300)), Some("original".to_owned()));
        assert_eq!(free((480, 360), (300, 200)), Some("3:2".to_owned()));
        assert_eq!(free((480, 360), (161, 90)), Some("16:9".to_owned()));
        assert_eq!(free((480, 360), (90, 161)), Some("16:9".to_owned()));
        assert_eq!(
            committed_aspect(&presets, (480, 360), (90, 160)).map(|(_, ratio)| ratio),
            Some(9.0 / 16.0),
            "a portrait crop locks the portrait ratio"
        );
        // Free and Custom are never the answer, whatever the rectangle.
        assert!(
            (1..40)
                .flat_map(|w| (1..40).map(move |h| (w * 7, h * 5)))
                .filter_map(|output| committed_aspect(&presets, (480, 360), output))
                .all(|(preset, _)| matches!(
                    preset.kind,
                    PresetKind::Original | PresetKind::Ratio(_)
                ))
        );
        for (width, height) in STAGES {
            let input = CropStage {
                width,
                height,
                angle: 0.0,
            };
            let neutral =
                CropDraft::from_layer(input, CropPayload::NEUTRAL, LayerId::new(), 0, 3, &presets);
            assert_eq!(neutral.preset, "free");
            assert_eq!(neutral.aspect, Aspect::Free);
        }
    }

    #[test]
    fn every_handle_keeps_the_rectangle_covered_and_whole_in_free_and_locked_modes() {
        for (width, height) in STAGES {
            for angle in ANGLES {
                for locked in [None, Some("3:2"), Some("16:9")] {
                    for option in [false, true] {
                        for handle in handles() {
                            let mut draft = draft(width, height, angle);
                            // Start from an off-centre rectangle so a drag has room both ways.
                            seed(&mut draft, Corner::TopLeft, (0.3, 0.25));
                            if let Some(option) = locked {
                                draft.set_preset(&preset(option), None);
                            }
                            check(&draft, "seed");
                            let modifiers = Modifiers { option };
                            for step in [
                                (-400.0, -400.0),
                                (400.0, 400.0),
                                (-37.0, 91.0),
                                (13.0, -113.0),
                            ] {
                                let from = grip(handle, &draft.rect);
                                let to = (from.0 + step.0, from.1 + step.1);
                                gesture(&mut draft, handle, from, to, modifiers);
                                check(
                                    &draft,
                                    &format!(
                                        "{width}x{height} at {angle} {locked:?} option={option} {handle:?} {step:?}"
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_drag_away_and_back_returns_the_starting_rectangle_exactly() {
        for (width, height) in STAGES {
            for angle in ANGLES {
                for locked in [None, Some("1:1"), Some("4:3")] {
                    for option in [false, true] {
                        for handle in handles() {
                            let mut draft = draft(width, height, angle);
                            seed(&mut draft, Corner::BottomRight, (0.7, 0.65));
                            if let Some(option) = locked {
                                draft.set_preset(&preset(option), None);
                            }
                            let start = draft.rect;
                            let from = grip(handle, &start);
                            let modifiers = Modifiers { option };
                            draft.begin(handle, from);
                            for step in [(-60.0, -45.0), (80.0, 17.0), (0.0, 0.0)] {
                                draft.drag((from.0 + step.0, from.1 + step.1), modifiers);
                            }
                            draft.end();
                            assert_eq!(
                                draft.rect, start,
                                "{width}x{height} at {angle} {locked:?} option={option} {handle:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_angle_sweep_away_and_back_returns_the_reference_rectangle_exactly() {
        for (width, height) in STAGES {
            let mut draft = draft(width, height, 0.0);
            pull(&mut draft, Corner::TopLeft, (60.0, 40.0));
            let reference = draft.rect;
            for angle in [3.5, -12.0, 45.0, -45.0, 22.5, 0.0] {
                draft.set_angle(angle);
                check(&draft, &format!("{width}x{height} at {angle}"));
            }
            assert_eq!(draft.stage.angle, 0.0);
            assert_eq!(
                draft.rect, reference,
                "{width}x{height}: an angle sweep accumulated trim"
            );
            // A clamped angle is still the reference refitted, and out-of-range values are clamped.
            draft.set_angle(90.0);
            assert_eq!(draft.stage.angle, MAX_ANGLE);
            draft.set_angle(-90.0);
            assert_eq!(draft.stage.angle, MIN_ANGLE);
            draft.set_angle(f64::NAN);
            assert_eq!(draft.stage.angle, MIN_ANGLE, "NaN never reaches the stage");
            draft.set_angle(0.0);
            assert_eq!(draft.rect, reference);
        }
    }

    #[test]
    fn option_scaling_keeps_the_center_and_the_ratio() {
        for (width, height) in STAGES {
            for angle in ANGLES {
                for handle in handles()
                    .into_iter()
                    .filter(|handle| *handle != Handle::Move)
                {
                    let mut draft = draft(width, height, angle);
                    seed(&mut draft, Corner::TopLeft, (0.2, 0.3));
                    let start = draft.rect;
                    let ratio = start.width / start.height;
                    let from = grip(handle, &start);
                    // Shrink towards the center, which no source boundary can bind.
                    let toward = (
                        (start.center().0 - from.0) * 0.4,
                        (start.center().1 - from.1) * 0.4,
                    );
                    gesture(
                        &mut draft,
                        handle,
                        from,
                        (from.0 + toward.0, from.1 + toward.1),
                        Modifiers { option: true },
                    );
                    let what = format!("{width}x{height} at {angle} {handle:?}");
                    assert!(
                        draft.rect.width < start.width && draft.rect.height < start.height,
                        "{what}: Option did not scale both axes ({:?} from {start:?})",
                        draft.rect
                    );
                    // Snapping to whole pixels can move each edge by under a pixel, so the centre
                    // moves by under half of one and the ratio changes by that much of an extent.
                    let center = draft.rect.center();
                    assert!(
                        (center.0 - start.center().0).abs() <= 0.5
                            && (center.1 - start.center().1).abs() <= 0.5,
                        "{what}: the centre moved to {center:?} from {:?}",
                        start.center()
                    );
                    matches_ratio(&draft.rect, ratio, &what);
                    check(&draft, &what);
                }
            }
        }
    }

    #[test]
    fn a_free_side_moves_one_edge_and_a_free_corner_two() {
        let mut draft = draft(480, 320, 0.0);
        gesture(
            &mut draft,
            Handle::Side(Edge::Right),
            (480.0, 160.0),
            (360.0, 160.0),
            Modifiers::default(),
        );
        assert_eq!(
            draft.rect,
            BoxRect {
                x: 0.0,
                y: 0.0,
                width: 360.0,
                height: 320.0
            }
        );
        gesture(
            &mut draft,
            Handle::Side(Edge::Top),
            (180.0, 0.0),
            (180.0, 50.0),
            Modifiers::default(),
        );
        assert_eq!(
            draft.rect,
            BoxRect {
                x: 0.0,
                y: 50.0,
                width: 360.0,
                height: 270.0
            }
        );
        gesture(
            &mut draft,
            Handle::Corner(Corner::TopLeft),
            (0.0, 50.0),
            (40.0, 90.0),
            Modifiers::default(),
        );
        assert_eq!(
            draft.rect,
            BoxRect {
                x: 40.0,
                y: 90.0,
                width: 320.0,
                height: 230.0
            }
        );
        // A side cannot leave the source and cannot collapse below one pixel.
        gesture(
            &mut draft,
            Handle::Side(Edge::Right),
            (360.0, 205.0),
            (9000.0, 205.0),
            Modifiers::default(),
        );
        assert_eq!(draft.rect.x + draft.rect.width, 480.0);
        gesture(
            &mut draft,
            Handle::Side(Edge::Right),
            (480.0, 205.0),
            (-9000.0, 205.0),
            Modifiers::default(),
        );
        assert_eq!(draft.rect.width, MIN_EXTENT);
        check(&draft, "collapsed");
    }

    #[test]
    fn moving_slides_along_the_boundary_and_never_resizes() {
        for angle in ANGLES {
            let mut draft = draft(480, 320, angle);
            seed(&mut draft, Corner::TopLeft, (0.25, 0.25));
            let start = draft.rect;
            let from = start.center();
            for step in [(-9000.0, -9000.0), (9000.0, 9000.0), (37.0, -9000.0)] {
                gesture(
                    &mut draft,
                    Handle::Move,
                    from,
                    (from.0 + step.0, from.1 + step.1),
                    Modifiers::default(),
                );
                assert_eq!(
                    (draft.rect.width, draft.rect.height),
                    (start.width, start.height),
                    "at {angle} moving by {step:?} resized the rectangle"
                );
                check(&draft, &format!("move {step:?} at {angle}"));
            }
        }
    }

    #[test]
    fn ratio_presets_swap_and_lock_keep_the_centre() {
        for (width, height) in STAGES {
            for angle in ANGLES {
                let mut draft = draft(width, height, angle);
                let before = draft.rect.center();
                for option in ["1:1", "3:2", "16:9", "original"] {
                    draft.set_preset(&preset(option), None);
                    let what = format!("{width}x{height} at {angle} {option}");
                    check(&draft, &what);
                    assert_eq!(draft.preset, option);
                    let ratio = draft.aspect.ratio().expect("a locked ratio");
                    matches_ratio(&draft.rect, ratio, &what);
                    let center = draft.rect.center();
                    assert!(
                        (center.0 - before.0).abs() <= 1.0 && (center.1 - before.1).abs() <= 1.0,
                        "{what}: the centre moved to {center:?} from {before:?}"
                    );
                    draft.swap();
                    check(&draft, &format!("{what} swapped"));
                    matches_ratio(&draft.rect, 1.0 / ratio, &format!("{what} swapped"));
                    let center = draft.rect.center();
                    assert!(
                        (center.0 - before.0).abs() <= 1.0 && (center.1 - before.1).abs() <= 1.0,
                        "{what} swapped: the centre moved to {center:?}"
                    );
                }
                // Custom needs its two extents; without them the preset is refused, not guessed.
                let locked = draft.aspect;
                draft.set_preset(&preset("custom"), None);
                assert_eq!(draft.aspect, locked);
                draft.set_preset(&preset("custom"), Some((5.0, 4.0)));
                assert_eq!(draft.aspect, Aspect::Locked(1.25));
                check(&draft, "custom");
                // Lock releases and re-takes the rectangle's own ratio.
                draft.lock_toggle();
                assert_eq!(draft.aspect, Aspect::Free);
                assert_eq!(draft.preset, "free");
                let ratio = draft.rect.width / draft.rect.height;
                draft.lock_toggle();
                assert_eq!(draft.aspect, Aspect::Locked(ratio));
                draft.set_preset(&preset("free"), None);
                assert_eq!(draft.aspect, Aspect::Free);
            }
        }
    }

    #[test]
    fn a_ratio_change_becomes_the_reference_an_angle_change_refits() {
        let mut draft = draft(480, 320, 0.0);
        draft.set_preset(&preset("1:1"), None);
        let reference = draft.rect;
        draft.set_angle(11.0);
        check(&draft, "straightened");
        draft.set_angle(0.0);
        assert_eq!(draft.rect, reference);
    }

    #[test]
    fn a_guide_straightens_by_the_angle_that_levels_the_dragged_line() {
        let mut draft = draft(480, 320, 0.0);
        let reference = draft.rect;
        draft.begin(Handle::Guide, (100.0, 100.0));
        draft.drag((200.0, 110.0), Modifiers::default());
        assert_eq!(draft.rect, reference, "a guide drag never moves the frame");
        assert_eq!(
            draft.guide_line(),
            Some(((100.0, 100.0), (200.0, 110.0))),
            "the canvas draws the line the guide has drawn"
        );
        draft.end();
        let expected = guide_angle(0.0, (100.0, 100.0), (200.0, 110.0));
        assert_eq!(draft.stage.angle, expected);
        assert!(
            expected < 0.0,
            "levelling a line that slopes downwards turns the image counter-clockwise"
        );
        check(&draft, "guided");
        // The guide never becomes the reference: returning to zero returns the reference rectangle.
        draft.set_angle(0.0);
        assert_eq!(draft.rect, reference);
        assert!(draft.guide_line().is_none());
    }

    #[test]
    fn hit_testing_prefers_corners_then_sides_and_otherwise_moves() {
        let mut draft = draft(480, 320, 0.0);
        pull(&mut draft, Corner::TopLeft, (100.0, 80.0));
        let rect = draft.rect;
        assert_eq!(
            draft.hit((100.0, 80.0), 8.0),
            Handle::Corner(Corner::TopLeft)
        );
        assert_eq!(
            draft.hit((480.0, 320.0), 8.0),
            Handle::Corner(Corner::BottomRight)
        );
        assert_eq!(
            draft.hit((rect.x, rect.y + rect.height / 2.0), 8.0),
            Handle::Side(Edge::Left)
        );
        assert_eq!(
            draft.hit((rect.x + rect.width / 2.0, rect.y + rect.height), 8.0),
            Handle::Side(Edge::Bottom)
        );
        assert_eq!(draft.hit(rect.center(), 8.0), Handle::Move);
        assert_eq!(draft.hit((5.0, 5.0), 8.0), Handle::Move, "outside moves");
    }

    #[test]
    fn rebasing_keeps_the_angle_and_the_composition_on_a_new_stage() {
        let mut draft = draft(480, 320, 7.0);
        pull(&mut draft, Corner::TopLeft, (140.0, 90.0));
        draft.mark_conflicted();
        assert!(draft.conflicted);
        let before = draft.rect;
        let layer = LayerId::new();
        // The same stage keeps the rectangle exactly, including its own reference.
        draft.rebase(
            CropStage {
                width: 480,
                height: 320,
                angle: 0.0,
            },
            21,
            Some(layer.clone()),
            2,
        );
        assert!(!draft.conflicted);
        assert_eq!(draft.base_revision, 21);
        assert_eq!(draft.layer, Some(layer));
        assert_eq!(draft.layer_index, 2);
        assert_eq!(draft.stage.angle, 7.0);
        assert_eq!(draft.rect, before);
        check(&draft, "rebased onto the same stage");

        // A smaller stage keeps the angle and the composition as far as it fits.
        let extents = (draft.rect.width, draft.rect.height);
        draft.rebase(
            CropStage {
                width: 240,
                height: 160,
                angle: 0.0,
            },
            22,
            None,
            0,
        );
        assert_eq!(draft.stage.angle, 7.0);
        assert_eq!((draft.stage.width, draft.stage.height), (240, 160));
        assert!(draft.rect.width <= extents.0 && draft.rect.height <= extents.1);
        check(&draft, "rebased onto a smaller stage");

        // A larger stage keeps the extents and the same input point at the centre.
        let centre = draft
            .stage
            .to_input(draft.rect.center().0, draft.rect.center().1);
        draft.rebase(
            CropStage {
                width: 960,
                height: 640,
                angle: 0.0,
            },
            23,
            None,
            0,
        );
        let mapped = draft
            .stage
            .to_input(draft.rect.center().0, draft.rect.center().1);
        assert!(
            (mapped.0 - centre.0).abs() <= 1.0 && (mapped.1 - centre.1).abs() <= 1.0,
            "the centre moved from {centre:?} to {mapped:?}"
        );
        check(&draft, "rebased onto a larger stage");
    }

    #[test]
    fn the_summary_reports_the_state_a_capture_is_correlated_with() {
        let mut draft = draft(480, 320, 0.0);
        draft.set_preset(&preset("3:2"), None);
        let summary = draft.summary();
        assert_eq!(summary["input_stage"], json!([480, 320]));
        assert_eq!(summary["angle"], json!(0.0));
        assert_eq!(summary["preset"], json!("3:2"));
        assert_eq!(summary["ratio"], json!(1.5));
        assert_eq!(summary["conflicted"], json!(false));
        assert_eq!(summary["layer"], Value::Null);
        assert_eq!(summary["layer_index"], json!(2));
        assert_eq!(summary["base_revision"], json!(3));
        let output = draft.output().expect("a valid draft");
        assert_eq!(summary["output"], json!([output.width, output.height]));
        assert_eq!(summary["payload"]["width"], json!(draft.payload().width));
    }

    #[test]
    fn a_gesture_without_a_press_changes_nothing() {
        let mut draft = draft(480, 320, 0.0);
        let before = draft.rect;
        draft.drag((10.0, 10.0), Modifiers::default());
        draft.end();
        assert_eq!(draft.rect, before);
        assert!(!draft.dragging());
        // A non-finite pointer position is ignored rather than poisoning the rectangle.
        draft.begin(Handle::Move, before.center());
        draft.drag((f64::NAN, 0.0), Modifiers::default());
        draft.end();
        assert_eq!(draft.rect, before);
    }
}
