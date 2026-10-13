//! Pointer-anchored trackpad zoom. Geometry is local; `view.set` remains the only authoritative
//! view mutation. The native adapter supplies numbers and owns no editor state.
use super::{Editor, message::Message, outcome::Outcome, tasks};
use crate::{
    layout,
    state::canvas::{PhotoView, ZoomView},
    view::{
        canvas::FIT_PADDING,
        canvas_view::{CanvasView, overhang, scrolled_origin},
        minimap_geometry::{self, NavStage, Viewport},
    },
};
use iced::{
    Point, Rectangle, Size, Task,
    widget::{operation, scrollable::AbsoluteOffset},
};
use luxforge_core::{ClientSession, ViewState, Zoom};
use luxforge_input::Pinch;
use serde_json::json;

/// Magnification is a logarithmic increment, so spreading and pinching by equal amounts undo
/// each other. Fit starts at its actual displayed scale, including its padding and display scale.
fn pinched_view(
    current: &ViewState,
    stage: (f64, f64),
    canvas: Rectangle,
    scale_factor: f32,
    pan: (f32, f32),
    input: Pinch,
) -> Option<ViewState> {
    if !input.delta.is_finite()
        || input.delta == 0.0
        || !input.x.is_finite()
        || !input.y.is_finite()
        || !scale_factor.is_finite()
        || scale_factor <= 0.0
        || !stage.0.is_finite()
        || !stage.1.is_finite()
        || stage.0 <= 0.0
        || stage.1 <= 0.0
    {
        return None;
    }
    let pointer = Point::new(input.x as f32, input.y as f32);
    if !canvas.contains(pointer) {
        return None;
    }
    let pointer = Point::new(pointer.x - canvas.x, pointer.y - canvas.y);
    let old = match current.zoom {
        Zoom::Fit => {
            let mut view = CanvasView::fit(
                stage,
                Size::new(
                    canvas.width - FIT_PADDING.left - FIT_PADDING.right,
                    canvas.height - FIT_PADDING.top - FIT_PADDING.bottom,
                ),
            )?;
            view.origin.x += FIT_PADDING.left;
            view.origin.y += FIT_PADDING.top;
            view
        }
        Zoom::Percent { value } => {
            let mut view = CanvasView::percent(value, scale_factor)?;
            view.origin.x = scrolled_origin(stage.0 as f32 * view.scale, canvas.width, pan.0);
            view.origin.y = scrolled_origin(stage.1 as f32 * view.scale, canvas.height, pan.1);
            view
        }
    };
    let old_percent = match current.zoom {
        Zoom::Percent { value } => value,
        Zoom::Fit => old.scale * scale_factor * 100.0,
    };
    let percent = (f64::from(old_percent) * input.delta.exp()).clamp(10.0, 1600.0) as f32;
    let new_scale = percent / 100.0 / scale_factor;
    let (x, y) = old.stage_point(pointer);
    let offset = |pixel: f64, at: f32, length: f64, available: f32| {
        (pixel as f32 * new_scale - at).clamp(0.0, overhang(length as f32 * new_scale, available))
    };
    let target = ViewState {
        zoom: Zoom::Percent { value: percent },
        pan_x: offset(x, pointer.x, stage.0, canvas.width),
        pan_y: offset(y, pointer.y, stage.1, canvas.height),
    };
    (target.zoom != current.zoom || (target.pan_x, target.pan_y) != pan).then_some(target)
}

/// How far inside the canvas's edge a revealed point is kept, in logical pixels.
const REVEAL_MARGIN: f32 = 32.0;

/// The pan that brings a stage point into view at a percentage zoom, or `None` when it already is
/// (or when the stage fits the canvas on that axis, where there is nothing to scroll). A point
/// outside the view, or within [`REVEAL_MARGIN`] of its edge, is centred on that axis, clamped to
/// the scrollable range, so a point near the photograph's border sits as near the middle as the
/// photograph allows.
pub(crate) fn revealed_pan(
    stage: (f64, f64),
    canvas: (f32, f32),
    scale: f32,
    point: (f64, f64),
    pan: (f32, f32),
) -> Option<(f32, f32)> {
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let axis = |stage: f64, available: f32, point: f64, pan: f32| {
        let content = stage as f32 * scale;
        let range = overhang(content, available);
        if range <= 0.0 {
            return pan;
        }
        let at = point as f32 * scale;
        let margin = REVEAL_MARGIN.min(available / 4.0);
        let shown = at - pan;
        if shown >= margin && shown <= available - margin {
            return pan;
        }
        (at - available / 2.0).clamp(0.0, range)
    };
    let target = (
        axis(stage.0, canvas.0, point.0, pan.0),
        axis(stage.1, canvas.1, point.1, pan.1),
    );
    (target != pan).then_some(target)
}

impl Editor {
    /// The main view the minimap shows, when it shows one: a Develop photograph at a stored
    /// percentage of at least 200%, over the stage the canvas draws now — a crop draft's box or the
    /// photograph's output stage — in the canvas region the layout leaves, at the display scale
    /// and the scrollable's local offset. `None` at Fit, below the threshold, in Select or a
    /// gallery page, with no photograph drawn, or for degenerate geometry. A pan it answers is
    /// applied as any other, through [`Editor::reveal_pan`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn minimap_viewport(&self) -> Option<Viewport> {
        self.minimap_viewport_in(&self.surfaces())
    }

    /// [`Self::minimap_viewport`] over the stage `surfaces` draws.
    pub(crate) fn minimap_viewport_in(
        &self,
        surfaces: &crate::view::Surfaces<'_>,
    ) -> Option<Viewport> {
        if self.document.state.is_none() || self.gallery_page().is_some() || self.select_shown() {
            return None;
        }
        let Zoom::Percent { value } = self.session.preview.view.zoom else {
            return None;
        };
        let percent = minimap_geometry::eligible(ZoomView::Percent(value))?;
        let stage = NavStage::drawn(&self.workspace.canvas, surfaces)?;
        let [left, top, right, bottom] = layout::canvas_logical(
            self.view_state.window,
            self.session.workspace.state_panel,
            self.session.workspace.tools_panel,
            self.filmstrip_shown(),
        );
        Viewport::new(
            stage,
            percent,
            self.view_state.scale_factor,
            Rectangle::new(Point::new(left, top), Size::new(right - left, bottom - top)),
            self.view_state.local_pan,
        )
    }

    /// A `view.set` this client's session answered outside the desktop's own input — a script's
    /// request on the same session — is this client's view all the same: the answer is adopted and
    /// the scrollable follows its pan, as [`Self::reveal_pan`] does. An answer older than the
    /// session held is not adopted and moves nothing.
    pub(crate) fn follow_view(&mut self, session: ClientSession) -> Task<Message> {
        let revision = session.revision;
        let pan = (session.preview.view.pan_x, session.preview.view.pan_y);
        self.adopt(session);
        if self.session.revision != revision {
            return Task::none();
        }
        self.view_state.local_pan = pan;
        self.view_state.pan.drop_pending();
        if self.view_state.pan.in_flight() {
            self.view_state.pan.offer(pan);
        }
        self.note_view_motion();
        self.event("view_followed", || json!({"pan": [pan.0, pan.1]}));
        operation::scroll_to(
            super::crop::SURFACE_ID,
            AbsoluteOffset {
                x: Some(pan.0),
                y: Some(pan.1),
            },
        )
    }

    /// Scroll the photograph to `pan` through `view.set`, as a pinch does: the session is told in
    /// this update and the scrollable follows.
    pub(super) fn reveal_pan(&mut self, pan: (f32, f32)) -> Task<Message> {
        let params = json!({"pan_x": pan.0, "pan_y": pan.1});
        match tasks::call(&self.owner, self.client, "view.set", params.clone()).and_then(
            |(value, _)| serde_json::from_value::<ClientSession>(value).map_err(|e| e.to_string()),
        ) {
            Ok(session) => {
                self.adopt(session);
                self.view_state.local_pan = pan;
                self.view_state.pan.drop_pending();
                if self.view_state.pan.in_flight() {
                    self.view_state.pan.offer(pan);
                }
                self.note_view_motion();
                self.event("view_revealed", || json!({"params": params}));
                self.outcome(Outcome::SessionAnswered);
                operation::scroll_to(
                    super::crop::SURFACE_ID,
                    AbsoluteOffset {
                        x: Some(pan.0),
                        y: Some(pan.1),
                    },
                )
            }
            Err(reason) => {
                self.status.text = reason;
                Task::none()
            }
        }
    }

    pub(super) fn pinch(&mut self, input: Pinch) -> Task<Message> {
        if self.document.state.is_none()
            || self.gallery_page().is_some()
            || self.palette.open
            || self.view_state.picker_open
            || matches!(self.workspace.canvas.photo, PhotoView::Empty(_))
        {
            return Task::none();
        }
        // A pinch over the minimap is the minimap's: it never zooms the photograph under it.
        if self.minimap_holds(Point::new(input.x as f32, input.y as f32)) {
            self.event(
                "view_pinch_held",
                || json!({"by":"minimap","pointer":[input.x,input.y]}),
            );
            return Task::none();
        }
        let stage = self
            .crop_gesture()
            .map(|crop| crop.frame.box_size())
            .or(self
                .workspace
                .canvas
                .dimensions
                .map(|(width, height)| (f64::from(width), f64::from(height))));
        let Some(stage) = stage else {
            return Task::none();
        };
        let [left, top, right, bottom] = layout::canvas_logical(
            self.view_state.window,
            self.session.workspace.state_panel,
            self.session.workspace.tools_panel,
            self.filmstrip_shown(),
        );
        let canvas = Rectangle::new(Point::new(left, top), Size::new(right - left, bottom - top));
        let Some(target) = pinched_view(
            &self.session.preview.view,
            stage,
            canvas,
            self.view_state.scale_factor,
            self.view_state.local_pan,
            input,
        ) else {
            return Task::none();
        };
        // `view.set` does bounded session work only. Like draft.set, do it in this update rather
        // than dropping input while an unrelated catalog request or a display frame is in flight.
        let params = json!({"zoom": target.zoom, "pan_x": target.pan_x, "pan_y": target.pan_y});
        match tasks::call(&self.owner, self.client, "view.set", params.clone()).and_then(
            |(value, _)| serde_json::from_value::<ClientSession>(value).map_err(|e| e.to_string()),
        ) {
            Ok(session) => {
                self.adopt(session);
                self.view_state.local_pan = (target.pan_x, target.pan_y);
                // An older asynchronous scroll request can still answer after this change. Keep
                // the newest offset waiting behind it so the owner ends at the pinched position.
                self.view_state.pan.drop_pending();
                if self.view_state.pan.in_flight() {
                    self.view_state.pan.offer(self.view_state.local_pan);
                }
                self.view_state.zoom_editing = false;
                self.note_view_motion();
                self.event("view_pinched", || json!({"delta": input.delta, "pointer": [input.x, input.y], "params": params}));
                self.outcome(Outcome::SessionAnswered);
                operation::scroll_to(
                    super::crop::SURFACE_ID,
                    AbsoluteOffset {
                        x: Some(target.pan_x),
                        y: Some(target.pan_y),
                    },
                )
            }
            Err(reason) => {
                self.status.text = reason;
                Task::none()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_outside_the_view_is_centred_and_one_inside_it_is_left_alone() {
        // A 4000x3000 stage at 50% in a 1000x700 canvas: 2000x1500 of content.
        let stage = (4000.0, 3000.0);
        let canvas = (1000.0, 700.0);
        // In view: no pan.
        assert_eq!(
            revealed_pan(stage, canvas, 0.5, (1000.0, 800.0), (400.0, 300.0)),
            None
        );
        // Out of view to the right and below: centred on both axes.
        assert_eq!(
            revealed_pan(stage, canvas, 0.5, (3000.0, 2400.0), (0.0, 0.0)),
            Some((1000.0, 800.0))
        );
        // Near the far border the pan clamps to the scrollable range.
        assert_eq!(
            revealed_pan(stage, canvas, 0.5, (3990.0, 2990.0), (0.0, 0.0)),
            Some((1000.0, 800.0))
        );
        // Near the origin it clamps to zero; a point inside the margin counts as outside.
        assert_eq!(
            revealed_pan(stage, canvas, 0.5, (10.0, 10.0), (100.0, 100.0)),
            Some((0.0, 0.0))
        );
        assert_eq!(
            revealed_pan(stage, canvas, 0.5, (1050.0, 800.0), (500.0, 300.0)),
            Some((25.0, 300.0)),
            "an x just inside the left margin moves; y is in view and keeps its pan"
        );
        // An axis the content does not overflow has nothing to scroll.
        assert_eq!(
            revealed_pan((400.0, 3000.0), canvas, 0.5, (390.0, 2900.0), (0.0, 0.0)),
            Some((0.0, 800.0))
        );
    }

    fn show_picture(editor: &mut Editor, dimensions: (u32, u32)) {
        let (_, raster) = crate::app::testing::analysed(editor, 1, &[[80, 90, 100, 255]], 1, 1);
        editor.presentation.presenter.show_photo(&raster);
        editor.presentation.dimensions = Some(dimensions);
        editor.rederive();
    }

    fn canvas() -> Rectangle {
        Rectangle::new(Point::new(200.0, 50.0), Size::new(1000.0, 700.0))
    }

    fn input(delta: f64) -> Pinch {
        Pinch {
            delta,
            x: 570.0,
            y: 344.0,
        }
    }

    fn percent(value: f32) -> ViewState {
        ViewState {
            zoom: Zoom::Percent { value },
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }

    #[test]
    fn pinch_preserves_an_off_centre_source_point_on_a_retina_display() {
        let stage = (6000.0, 4000.0);
        let old = percent(100.0);
        let pan = (420.0, 310.0);
        let next = pinched_view(&old, stage, canvas(), 2.0, pan, input(1.2_f64.ln())).unwrap();
        assert_eq!(next.zoom, Zoom::Percent { value: 120.0 });
        let Zoom::Percent { value } = next.zoom else {
            unreachable!()
        };
        // 370/294 logical pixels from the canvas origin: old and new physical scales agree.
        assert!(((370.0 + pan.0) / 0.5 - (370.0 + next.pan_x) / (value / 200.0)).abs() < 0.001);
        assert!(((294.0 + pan.1) / 0.5 - (294.0 + next.pan_y) / (value / 200.0)).abs() < 0.001);
        let back = pinched_view(
            &next,
            stage,
            canvas(),
            2.0,
            (next.pan_x, next.pan_y),
            input(-1.2_f64.ln()),
        )
        .unwrap();
        assert_eq!(back.zoom, old.zoom);
        assert!((back.pan_x - pan.0).abs() < 0.001 && (back.pan_y - pan.1).abs() < 0.001);
    }

    #[test]
    fn pinch_leaves_fit_from_its_actual_scale_and_padding() {
        let stage = (6000.0, 4000.0);
        let available = Size::new(
            canvas().width - FIT_PADDING.left - FIT_PADDING.right,
            canvas().height - FIT_PADDING.top - FIT_PADDING.bottom,
        );
        let fit = CanvasView::fit(stage, available).unwrap();
        let next = pinched_view(
            &ViewState::default(),
            stage,
            canvas(),
            2.0,
            (0.0, 0.0),
            input(4.0_f64.ln()),
        )
        .unwrap();
        let Zoom::Percent { value } = next.zoom else {
            unreachable!()
        };
        assert!((value - fit.scale * 200.0 * 4.0).abs() < 0.001);
        let old_pixel = (370.0 - FIT_PADDING.left - fit.origin.x) / fit.scale;
        assert!((old_pixel - (370.0 + next.pan_x) / (value / 200.0)).abs() < 0.001);
    }

    #[test]
    fn pinch_clamps_zoom_and_pan_and_centres_an_axis_that_fits() {
        let upper = pinched_view(
            &percent(1500.0),
            (6000.0, 4000.0),
            canvas(),
            2.0,
            (100000.0, 100000.0),
            input(10.0),
        )
        .unwrap();
        assert_eq!(upper.zoom, Zoom::Percent { value: 1600.0 });
        assert!((0.0..=47000.0).contains(&upper.pan_x));
        assert!((0.0..=31300.0).contains(&upper.pan_y));
        let lower = pinched_view(
            &percent(100.0),
            (6000.0, 4000.0),
            canvas(),
            2.0,
            (400.0, 300.0),
            input(-100.0),
        )
        .unwrap();
        assert_eq!(lower.zoom, Zoom::Percent { value: 10.0 });
        assert_eq!((lower.pan_x, lower.pan_y), (0.0, 0.0));
        let narrow = pinched_view(
            &percent(100.0),
            (1000.0, 4000.0),
            canvas(),
            2.0,
            (0.0, 300.0),
            input(1.2_f64.ln()),
        )
        .unwrap();
        assert_eq!(narrow.pan_x, 0.0);
        // A cursor outside the canvas, a stationary gesture and non-finite input do nothing.
        for delta in [0.0, f64::NAN, f64::INFINITY] {
            assert!(
                pinched_view(
                    &percent(100.0),
                    (6000.0, 4000.0),
                    canvas(),
                    2.0,
                    (0.0, 0.0),
                    input(delta)
                )
                .is_none()
            );
        }
        assert!(
            pinched_view(
                &percent(100.0),
                (6000.0, 4000.0),
                canvas(),
                2.0,
                (0.0, 0.0),
                Pinch {
                    x: 50.0,
                    ..input(0.1)
                }
            )
            .is_none()
        );
    }

    /// AppKit reports a pinch in the system's points; at an interface scale of 125% the layout is
    /// in logical pixels 1.25 points wide, so a pinch near the canvas's far corner, which in points
    /// lies beyond the canvas's logical rectangle, still zooms about that corner.
    #[test]
    fn a_pinch_at_an_interface_scale_reads_the_pointer_in_the_systems_points() {
        use crate::app::testing::{boot, finish, real_photo};
        let (empty, catalog) = boot();
        finish(empty, catalog.clone());
        let (mut editor, _, _) = real_photo(&catalog);
        show_picture(&mut editor, (480, 320));
        editor.view_state.window = (1440.0, 900.0);
        editor.view_state.set_system_scale_factor(2.0);
        editor.view_state.set_interface_size(125);
        assert_eq!(editor.view_state.window, (1152.0, 720.0));
        let [_, _, right, bottom] = layout::canvas_logical(
            editor.view_state.window,
            editor.session.workspace.state_panel,
            editor.session.workspace.tools_panel,
            editor.filmstrip_shown(),
        );
        let (x, y) = (right - 10.0, bottom - 10.0);
        assert!(
            x * 1.25 > right,
            "in points the corner lies beyond the canvas"
        );
        let _ = editor.update(Message::View(
            super::super::message::view::ViewMessage::Pinch(Pinch {
                delta: 0.1,
                x: f64::from(x * 1.25),
                y: f64::from(y * 1.25),
            }),
        ));
        assert!(
            matches!(editor.session.preview.view.zoom, Zoom::Percent { .. }),
            "the pinch zoomed"
        );
    }

    #[test]
    fn pinch_uses_the_same_session_command_as_an_api_client_and_commits_nothing() {
        use crate::app::testing::{boot, finish, real_photo};
        let (empty, catalog) = boot();
        finish(empty, catalog.clone());
        let (mut editor, asset, agent) = real_photo(&catalog);
        show_picture(&mut editor, (480, 320));
        let before = tasks::call(
            &editor.owner,
            agent,
            "asset.state",
            json!({"asset_id":asset}),
        )
        .unwrap()
        .0;
        let preview_generation = editor.session.preview.generation;
        // Another edit's request in flight does not throw away trackpad increments.
        editor.busy = true;
        let _ = editor.update(Message::View(
            super::super::message::view::ViewMessage::Pinch(input(0.1)),
        ));
        assert!(editor.busy);
        let Zoom::Percent { value: first } = editor.session.preview.view.zoom else {
            panic!("pinch leaves Fit")
        };
        let _ = editor.update(Message::View(
            super::super::message::view::ViewMessage::Pinch(input(0.1)),
        ));
        let Zoom::Percent { value: second } = editor.session.preview.view.zoom else {
            unreachable!()
        };
        assert!((f64::from(second / first) - 0.1_f64.exp()).abs() < 0.00001);
        let target = editor.session.preview.view.clone();
        let (api, _) = tasks::call(
            &editor.owner,
            agent,
            "view.set",
            json!({
            "zoom":target.zoom,"pan_x":target.pan_x,"pan_y":target.pan_y}),
        )
        .unwrap();
        let api: ClientSession = serde_json::from_value(api).unwrap();
        assert_eq!(target, api.preview.view);
        let (own, _) =
            tasks::call(&editor.owner, editor.client, "session.state", json!({})).unwrap();
        let own: ClientSession = serde_json::from_value(own).unwrap();
        assert_eq!(target, own.preview.view);
        assert_eq!(editor.session.preview.generation, preview_generation);
        assert_eq!(
            before,
            tasks::call(
                &editor.owner,
                agent,
                "asset.state",
                json!({"asset_id":asset})
            )
            .unwrap()
            .0
        );
        finish(editor, catalog);
    }

    /// A photograph of `dimensions` on screen: its frame presented as the content shown.
    fn drawn_picture(editor: &mut Editor, dimensions: (u32, u32)) {
        let (_, raster) = crate::app::testing::analysed(editor, 1, &[[80, 90, 100, 255]], 1, 1);
        editor.presentation.presenter.show_reduced(&raster, 7);
        editor.presentation.presented_content = 7;
        editor.presentation.dimensions = Some(dimensions);
        editor.rederive();
    }

    /// Set this client's zoom through `view.set`, as the title bar's field and an agent do.
    fn zoom(editor: &mut Editor, zoom: serde_json::Value) {
        let (value, _) = tasks::call(
            &editor.owner,
            editor.client,
            "view.set",
            json!({ "zoom": zoom }),
        )
        .unwrap();
        editor.adopt(serde_json::from_value(value).unwrap());
    }

    fn assert_near(actual: [f64; 4], expected: [f64; 4]) {
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(a, e)| (a - e).abs() < 0.001),
            "{actual:?} is not {expected:?}"
        );
    }

    /// The minimap's view is the editor's own: the stored percentage decides, and the canvas the
    /// layout leaves beside both open panels, the 2× display and the scrollable's local offset
    /// place it. Expected values are worked from the layout constants by hand.
    #[test]
    fn the_minimap_views_the_canvas_the_layout_leaves_at_the_stored_zoom() {
        use crate::app::testing::{boot, finish, real_photo};
        let (empty, catalog) = boot();
        assert!(empty.minimap_viewport().is_none(), "no photograph");
        finish(empty, catalog.clone());
        let (mut editor, _, _) = real_photo(&catalog);
        drawn_picture(&mut editor, (6000, 4000));
        editor.view_state.window = (1440.0, 900.0);
        editor.view_state.set_system_scale_factor(2.0);
        assert!(editor.session.workspace.state_panel && editor.session.workspace.tools_panel);
        assert!(!editor.filmstrip_shown());
        assert!(editor.minimap_viewport().is_none(), "Fit");
        zoom(&mut editor, json!({"mode": "percent", "value": 199.9}));
        assert!(editor.minimap_viewport().is_none(), "199.9%");
        zoom(&mut editor, json!({"mode": "percent", "value": 200.0}));
        assert!(editor.minimap_viewport().is_some(), "200%");
        zoom(&mut editor, json!({"mode": "percent", "value": 400.0}));
        editor.view_state.local_pan = (1000.0, 500.0);
        let view = editor.minimap_viewport().expect("400%");
        assert_eq!(view.stage(), NavStage::photo((6000, 4000)));
        assert_eq!(view.scale(), 2.0);
        assert_eq!(view.pan(), (1000.0, 500.0));
        // The canvas is 241..1139 by 44..874: 898 × 830 logical. Two logical pixels per stage
        // pixel, moved by 1000 × 500, shows columns 500..949 and rows 250..665.
        assert_eq!(view.pan_range(), (12000.0 - 898.0, 8000.0 - 830.0));
        assert_near(view.visible().unwrap(), [500.0, 250.0, 949.0, 665.0]);
        // Hiding the tools panel widens the canvas to 241..1440: 1199 logical.
        editor.session.workspace.tools_panel = false;
        assert_near(
            editor.minimap_viewport().unwrap().visible().unwrap(),
            [500.0, 250.0, 1099.5, 665.0],
        );
        finish(editor, catalog);
    }

    /// At an interface size of 125% the layout is 1152 × 720 logical pixels of 2.5 physical
    /// pixels each, and the rectangle follows the photograph the surface snaps to that grid.
    #[test]
    fn the_minimap_view_follows_the_interface_scale() {
        use crate::app::testing::{boot, finish, real_photo};
        let (empty, catalog) = boot();
        finish(empty, catalog.clone());
        let (mut editor, _, _) = real_photo(&catalog);
        drawn_picture(&mut editor, (6000, 4000));
        editor.view_state.window = (1440.0, 900.0);
        editor.view_state.set_system_scale_factor(2.0);
        editor.view_state.set_interface_size(125);
        zoom(&mut editor, json!({"mode": "percent", "value": 200.0}));
        editor.view_state.local_pan = (1000.3, 400.6);
        let view = editor.minimap_viewport().expect("200%");
        assert_eq!(view.scale(), 0.8);
        // The canvas is 241..851 by 44..694 logical: 602.5..2127.5 by 110..1735 physical,
        // clipped at 603..2128. The photograph, moved by 1000 and 401 logical pixels, is drawn
        // from −1897.5 and −892.5 physical, snapped half to even to −1898 and −892, at two
        // physical pixels per stage pixel.
        assert_near(view.visible().unwrap(), [1250.5, 501.0, 2013.0, 1313.5]);
        finish(editor, catalog);
    }

    /// Navigating in the minimap is a pan through the same `view.set` an API client sends: the
    /// same client's equivalent request leaves the identical session and visible region, another
    /// client's view is untouched either way, and no edit, history entry or preview content
    /// changes.
    #[test]
    fn minimap_navigation_is_the_same_view_set_as_an_api_client_and_edits_nothing() {
        use crate::app::testing::{boot, finish, real_photo};
        use crate::view::minimap_geometry::Overview;
        let (empty, catalog) = boot();
        finish(empty, catalog.clone());
        let (mut editor, asset, agent) = real_photo(&catalog);
        drawn_picture(&mut editor, (6000, 4000));
        editor.view_state.window = (1440.0, 900.0);
        editor.view_state.set_system_scale_factor(2.0);
        zoom(&mut editor, json!({"mode": "percent", "value": 400.0}));
        let state = |editor: &Editor| {
            tasks::call(
                &editor.owner,
                agent,
                "asset.state",
                json!({"asset_id": asset}),
            )
            .unwrap()
            .0
        };
        let before = state(&editor);
        let generation = editor.session.preview.generation;
        let agent_view = |editor: &Editor| {
            let (value, _) = tasks::call(&editor.owner, agent, "session.state", json!({})).unwrap();
            serde_json::from_value::<ClientSession>(value)
                .unwrap()
                .preview
                .view
        };
        assert_eq!(agent_view(&editor), ViewState::default());

        // A click at the card's centre is stage (3000, 2000): 6000 × 4000 logical less half the
        // 898 × 830 canvas.
        let view = editor.minimap_viewport().unwrap();
        let overview = Overview::place(view.stage().size, Size::new(180.0, 120.0)).unwrap();
        let pan = overview
            .press(&view, Point::new(90.0, 60.0))
            .unwrap()
            .pan
            .unwrap();
        assert_eq!(pan, (5551.0, 3585.0));
        let _ = editor.reveal_pan(pan);
        let gui = editor.session.preview.view.clone();
        assert_eq!(
            gui,
            ViewState {
                zoom: Zoom::Percent { value: 400.0 },
                pan_x: 5551.0,
                pan_y: 3585.0,
            }
        );
        assert_eq!(editor.view_state.local_pan, pan);
        let shown = editor.minimap_viewport().unwrap().visible().unwrap();
        assert_near(shown, [2775.5, 1792.5, 3224.5, 2207.5]);

        // The same client asking for the same pan through the API: the identical session.
        let (value, _) = tasks::call(
            &editor.owner,
            editor.client,
            "view.set",
            json!({"pan_x": 5551.0, "pan_y": 3585.0}),
        )
        .unwrap();
        let api: ClientSession = serde_json::from_value(value).unwrap();
        assert_eq!(api.preview.view, gui);
        assert_eq!(api.preview, editor.session.preview);
        editor.adopt(api);
        assert_eq!(editor.minimap_viewport().unwrap().visible(), Some(shown));

        // Clients are isolated in both directions.
        assert_eq!(agent_view(&editor), ViewState::default());
        tasks::call(
            &editor.owner,
            agent,
            "view.set",
            json!({"pan_x": 1.0, "pan_y": 2.0}),
        )
        .unwrap();
        let (own, _) =
            tasks::call(&editor.owner, editor.client, "session.state", json!({})).unwrap();
        assert_eq!(
            serde_json::from_value::<ClientSession>(own)
                .unwrap()
                .preview
                .view,
            gui
        );

        // Nothing was edited, recorded or rendered anew.
        assert_eq!(editor.session.preview.generation, generation);
        assert_eq!(state(&editor), before);
        finish(editor, catalog);
    }

    #[test]
    fn a_pinch_keeps_its_latest_pan_waiting_behind_an_older_scroll_request() {
        use crate::app::testing::{boot, finish, real_photo};
        let (mut empty, catalog) = boot();
        let before = empty.session.clone();
        let _ = empty.pinch(input(0.1));
        assert_eq!(empty.session, before);
        finish(empty, catalog.clone());
        let (mut editor, _, _) = real_photo(&catalog);
        show_picture(&mut editor, (6000, 4000));
        editor.view_state.pan.offer((1.0, 2.0));
        editor.view_state.pan.start();
        editor.view_state.pan.offer((3.0, 4.0));
        let _ = editor.pinch(input(0.1));
        assert!(editor.view_state.pan.in_flight());
        assert_eq!(
            editor.view_state.pan.pending(),
            Some(&editor.view_state.local_pan)
        );
        finish(editor, catalog);
    }
}
