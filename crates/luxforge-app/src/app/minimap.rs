//! The high-zoom minimap's navigation ([minimap design](../../../../docs/design/minimap.md)): what
//! the card shows, and what a press, a drag, a release, the wheel, Escape and focus loss on it do.
//!
//! - **One projection.** The card's rectangle and its navigation are the shared geometry's
//!   ([`crate::view::minimap_geometry`]) over the view the canvas draws now — the stage, the stored
//!   zoom, the canvas region, the display scale and the scrollable's local offset — so what the card
//!   shows and where a click sends the view cannot disagree.
//! - **View only.** A press centres the view or grabs the rectangle; a drag moves it by the grab;
//!   either is applied through [`Editor::reveal_pan`] — `view.set` on this client's session in the
//!   input's update, the scrollable moved to the same offset and the asynchronous pan coalescer
//!   told, so a late answer cannot undo it. Nothing here touches a recipe, history, a draft, the
//!   preview's content or the histogram.
//! - **Navigable only against its own stage.** Navigation acts only while the overview the card
//!   shows is of the stage the canvas draws: the output stage, or a crop draft's input stage turned
//!   into its box. A pending overview, another photograph or a changed stage withdraws the
//!   rectangle; a drag in progress then ends, keeping the view it last applied.
//! - **The drag ends** on release, on Escape — which it consumes, so an open draft under it is
//!   never cancelled by it — and on focus loss.
use super::{
    Before, Editor,
    message::Message,
    overview::{MinimapOverview, OverviewStage, StageKind},
};
use crate::{
    layout,
    view::{
        Surfaces,
        minimap::{self as card, Card, MinimapModel, Shows},
        minimap_geometry::{Grab, NavStage, Overview, Viewport},
    },
};
use iced::{
    Point, Rectangle, Size, Task,
    keyboard::{Event as KeyEvent, Key, key::Named},
    widget::{operation, scrollable::AbsoluteOffset},
};
use serde_json::{Value, json};

/// The minimap's navigation: the drag in progress, and what it did, for evidence.
#[derive(Default)]
pub(crate) struct Navigation {
    drag: Option<Drag>,
    counters: Counters,
}

/// A drag holding the viewport's rectangle: the grab, and the photograph and stage it grabbed.
#[derive(Clone, Debug, PartialEq)]
struct Drag {
    grab: Grab,
    stage: NavStage,
    asset: Option<luxforge_core::AssetId>,
}

/// What the minimap's input did. Counters only.
#[derive(Clone, Copy, Debug, Default)]
struct Counters {
    presses: u64,
    /// Presses in the letterbox, or while the card could not navigate.
    inert: u64,
    /// Pans a press or drag asked for.
    navigations: u64,
    releases: u64,
    escapes: u64,
    focus_losses: u64,
    /// Drags ended because the photograph or stage changed under them.
    stage_ends: u64,
    wheels: u64,
    /// Native pinches over the card, kept from the photograph.
    pinches: u64,
}

/// The minimap as the canvas has it now.
pub(crate) struct Minimap {
    pub(crate) viewport: Viewport,
    pub(crate) overview: Overview,
    pub(crate) card: Card,
    /// The canvas region, in the window's logical pixels.
    pub(crate) canvas: Rectangle,
    pub(crate) shows: MinimapOverview,
    /// The overview shown is of the stage the canvas draws, so navigation may act.
    pub(crate) navigable: bool,
    /// Compare's divider is shown: the overview is its fixed After side.
    pub(crate) after: bool,
}

impl Minimap {
    /// The card, in the window's logical pixels.
    pub(crate) fn card_window(&self) -> Rectangle {
        let bounds = self.card.bounds;
        Rectangle::new(
            Point::new(self.canvas.x + bounds.x, self.canvas.y + bounds.y),
            bounds.size(),
        )
    }

    /// The image area's top-left corner, in the window's logical pixels: what the points the card
    /// publishes are relative to.
    pub(crate) fn area_origin(&self) -> Point {
        let card = self.card_window();
        Point::new(card.x + card::PADDING, card.y + card::PADDING)
    }

    fn to_window(&self, rect: Rectangle) -> Rectangle {
        let origin = self.area_origin();
        Rectangle::new(
            Point::new(origin.x + rect.x, origin.y + rect.y),
            rect.size(),
        )
    }

    /// The thumbnail, in the window's logical pixels.
    pub(crate) fn image_window(&self) -> Rectangle {
        self.to_window(self.overview.image())
    }

    /// The viewport's rectangle in the image area, while navigation may act on it.
    pub(crate) fn rectangle(&self) -> Option<Rectangle> {
        self.navigable
            .then(|| self.overview.rectangle(&self.viewport))
            .flatten()
    }

    /// [`Self::rectangle`], in the window's logical pixels.
    pub(crate) fn rectangle_window(&self) -> Option<Rectangle> {
        self.rectangle().map(|rect| self.to_window(rect))
    }

    fn shows(&self) -> Shows {
        match self.shows {
            MinimapOverview::Ready(_) if self.navigable => Shows::Ready,
            MinimapOverview::Updating(_) if self.navigable => Shows::Updating,
            MinimapOverview::Unavailable { .. } if self.navigable => Shows::Unavailable,
            _ => Shows::Pending,
        }
    }
}

/// Whether an overview of `stage` shows the stage the canvas draws, `drawn`: the output stage at its
/// size, or the crop draft's input stage that `draft` turns into the box drawn.
fn same_stage(
    stage: OverviewStage,
    drawn: NavStage,
    draft: Option<&crate::crop_draft::CropDraft>,
) -> bool {
    match stage.kind {
        StageKind::Output => {
            drawn.snapped && drawn.size == (f64::from(stage.width), f64::from(stage.height))
        }
        StageKind::CropInput => draft.is_some_and(|draft| {
            let (width, height) = draft.stage_size();
            !drawn.snapped
                && drawn.size == draft.box_size()
                && (width.round() as u32, height.round() as u32) == (stage.width, stage.height)
        }),
    }
}

fn rect_value(rect: Rectangle) -> Value {
    json!([rect.x, rect.y, rect.width, rect.height])
}

impl Editor {
    /// The canvas region, in the window's logical pixels.
    pub(crate) fn canvas_region(&self) -> Rectangle {
        let [left, top, right, bottom] = layout::canvas_logical(
            self.view_state.window,
            self.session.workspace.state_panel,
            self.session.workspace.tools_panel,
            self.filmstrip_shown(),
        );
        Rectangle::new(Point::new(left, top), Size::new(right - left, bottom - top))
    }

    /// The minimap the canvas shows with `surfaces`, if any: at a stored percentage of at least
    /// 200% over a drawn photograph with an overview to show, on a canvas that holds a card.
    pub(crate) fn minimap(&self, surfaces: &Surfaces<'_>) -> Option<Minimap> {
        let shows = self.minimap_overview();
        if matches!(shows, MinimapOverview::None) {
            return None;
        }
        let viewport = self.minimap_viewport_in(surfaces)?;
        let canvas = self.canvas_region();
        let card = card::card(
            canvas.size(),
            viewport.stage().size,
            self.workspace.canvas.modes.len(),
        )?;
        let overview = Overview::place(viewport.stage().size, card.area)?;
        let navigable = shows
            .navigable()
            .is_some_and(|stage| same_stage(stage, viewport.stage(), surfaces.draft));
        Some(Minimap {
            viewport,
            overview,
            card,
            canvas,
            shows,
            navigable,
            after: surfaces.comparison.is_some(),
        })
    }

    /// What the card draws with `surfaces`.
    pub(crate) fn minimap_model(&self, surfaces: &Surfaces<'_>) -> Option<MinimapModel> {
        let minimap = self.minimap(surfaces)?;
        let image = minimap.overview.image();
        let shown = minimap
            .shows
            .shown()
            .filter(|_| minimap.navigable)
            .map(|shown| shown.image);
        // A crop draft's input stage is turned into its box as the crop surface turns it, its
        // empty corners left as the ground.
        let turn = surfaces
            .draft
            .filter(|_| !minimap.viewport.stage().snapped)
            .map(|draft| {
                let scale = image.width / minimap.viewport.stage().size.0 as f32;
                let (width, height) = draft.stage_size();
                let size = Size::new(width as f32 * scale, height as f32 * scale);
                luxforge_ui::Turn {
                    rect: Rectangle::new(
                        Point::new(
                            (image.width - size.width) / 2.0,
                            (image.height - size.height) / 2.0,
                        ),
                        size,
                    ),
                    angle: draft.angle().to_radians() as f32,
                    bright: Rectangle::new(Point::ORIGIN, image.size()),
                    dim: 1.0,
                }
            });
        Some(MinimapModel {
            card: minimap.card,
            image,
            overview: shown,
            turn,
            rectangle: minimap.rectangle(),
            shows: minimap.shows(),
            after: minimap.after,
            dragging: self.minimap_nav.drag.is_some(),
        })
    }

    /// A press on the card at `point` of its image area: centre the view there, or grab the
    /// rectangle where it was pressed. Inert in the letterbox and while the card cannot navigate.
    pub(super) fn minimap_press(&mut self, point: Point) -> Task<Message> {
        self.minimap_nav.counters.presses += 1;
        let Some(minimap) = self.minimap(&self.surfaces()).filter(|m| m.navigable) else {
            self.minimap_nav.counters.inert += 1;
            self.event(
                "minimap_pressed",
                || json!({"point":[point.x,point.y],"inert":"not navigable"}),
            );
            return Task::none();
        };
        let Some(press) = minimap.overview.press(&minimap.viewport, point) else {
            self.minimap_nav.counters.inert += 1;
            self.event(
                "minimap_pressed",
                || json!({"point":[point.x,point.y],"inert":"letterbox"}),
            );
            return Task::none();
        };
        self.minimap_nav.drag = Some(Drag {
            grab: press.grab,
            stage: minimap.viewport.stage(),
            asset: self
                .document
                .state
                .as_ref()
                .map(|state| state.asset.id.clone()),
        });
        self.event(
            "minimap_pressed",
            || json!({"point":[point.x,point.y],"grabbed":press.pan.is_none()}),
        );
        match press.pan {
            Some(pan) => self.minimap_pan(pan, "press"),
            None => Task::none(),
        }
    }

    /// The pointer moved to `point` of the image area with a drag held: the rectangle follows it by
    /// its grab, clamped to the stage's edges.
    pub(super) fn minimap_drag(&mut self, point: Point) -> Task<Message> {
        let Some(drag) = self.minimap_nav.drag.clone() else {
            return Task::none();
        };
        let Some(minimap) = self
            .minimap(&self.surfaces())
            .filter(|minimap| minimap.navigable && minimap.viewport.stage() == drag.stage)
        else {
            self.minimap_end("stage");
            return Task::none();
        };
        match drag.grab.pan(&minimap.overview, &minimap.viewport, point) {
            Some(pan) => self.minimap_pan(pan, "drag"),
            None => Task::none(),
        }
    }

    fn minimap_pan(&mut self, pan: (f32, f32), why: &'static str) -> Task<Message> {
        self.minimap_nav.counters.navigations += 1;
        self.event(
            "minimap_navigated",
            || json!({"why":why,"pan":[pan.0,pan.1]}),
        );
        self.reveal_pan(pan)
    }

    /// End the drag in progress, keeping the view it last applied, for `why`: a release, Escape,
    /// focus loss, or a changed photograph or stage.
    pub(super) fn minimap_end(&mut self, why: &'static str) {
        let counters = &mut self.minimap_nav.counters;
        match why {
            "release" => counters.releases += 1,
            "escape" => counters.escapes += 1,
            "focus" => counters.focus_losses += 1,
            _ => counters.stage_ends += 1,
        }
        if self.minimap_nav.drag.take().is_some() {
            self.event("minimap_drag_ended", || json!({"why":why}));
        }
    }

    /// A wheel over the card: the scrollable pans by it as it would under the pointer, and its
    /// offset reaches the session through the ordinary pan.
    pub(super) fn minimap_wheel(&mut self, x: f32, y: f32) -> Task<Message> {
        if self.minimap(&self.surfaces()).is_none() {
            return Task::none();
        }
        self.minimap_nav.counters.wheels += 1;
        operation::scroll_by(super::crop::SURFACE_ID, AbsoluteOffset { x, y })
    }

    /// A key or window event while a drag is held: Escape ends it and is consumed, so it never
    /// reaches an open draft; focus loss ends it and goes on to the key table.
    pub(super) fn minimap_key(&mut self, event: &iced::Event) -> Option<Task<Message>> {
        self.minimap_nav.drag.as_ref()?;
        match event {
            iced::Event::Keyboard(KeyEvent::KeyPressed {
                key: Key::Named(Named::Escape),
                ..
            }) => {
                self.minimap_end("escape");
                Some(Task::none())
            }
            iced::Event::Window(iced::window::Event::Unfocused) => {
                self.minimap_end("focus");
                None
            }
            _ => None,
        }
    }

    /// Whether `point`, in the window's logical pixels, is on the card: a native pinch there is the
    /// card's and never zooms the photograph.
    pub(super) fn minimap_holds(&mut self, point: Point) -> bool {
        let held = self
            .minimap(&self.surfaces())
            .is_some_and(|minimap| minimap.card_window().contains(point));
        if held {
            self.minimap_nav.counters.pinches += 1;
        }
        held
    }

    /// Evidence: whether the card is shown, where, what it shows, the viewport it projects and what
    /// its input did.
    pub(crate) fn minimap_summary(&self) -> Value {
        let counters = self.minimap_nav.counters;
        let counters = json!({"presses":counters.presses,"inert":counters.inert,
            "navigations":counters.navigations,"releases":counters.releases,
            "escapes":counters.escapes,"focus_losses":counters.focus_losses,
            "stage_ends":counters.stage_ends,"wheels":counters.wheels,"pinches":counters.pinches});
        let dragging = self.minimap_nav.drag.is_some();
        let surfaces = self.surfaces();
        let Some(minimap) = self.minimap(&surfaces) else {
            return json!({"shown":false,"dragging":dragging,"counters":counters});
        };
        let viewport = &minimap.viewport;
        let stage = viewport.stage();
        let shown = minimap.shows.shown().map(|shown| {
            json!({"version":shown.image.version,"size":[shown.image.size.0,shown.image.size.1],
                "stage":[shown.stage.width,shown.stage.height],
                "crop_input":shown.stage.kind == StageKind::CropInput})
        });
        json!({
            "shown": true,
            "state": minimap.shows().as_str(),
            "navigable": minimap.navigable,
            "after": minimap.after,
            "dragging": dragging,
            "card": rect_value(minimap.card_window()),
            "image": rect_value(minimap.image_window()),
            "rectangle": minimap.rectangle_window().map(rect_value),
            "visible": viewport.visible(),
            "normalized": viewport.normalized(),
            "stage": {"size":[stage.size.0,stage.size.1],"snapped":stage.snapped},
            "scale": viewport.scale(),
            "scale_factor": self.view_state.scale_factor,
            "canvas": rect_value(minimap.canvas),
            "pan": [viewport.pan().0, viewport.pan().1],
            "pan_range": [viewport.pan_range().0, viewport.pan_range().1],
            "overview": shown,
            "counters": counters,
        })
    }
}

/// After every message: a drag whose photograph or stage is no longer the one navigable ends.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    if let Some(drag) = editor.minimap_nav.drag.clone() {
        let asset = editor
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone());
        let stage = editor
            .minimap(&editor.surfaces())
            .filter(|minimap| minimap.navigable)
            .map(|minimap| minimap.viewport.stage());
        if stage != Some(drag.stage) || asset != drag.asset {
            editor.minimap_end("stage");
        }
    }
    Task::none()
}

#[cfg(test)]
#[path = "minimap_tests.rs"]
mod tests;
