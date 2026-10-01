//! Pointer-anchored trackpad zoom. Geometry is local; `view.set` remains the only authoritative
//! view mutation. The native adapter supplies numbers and owns no editor state.
use super::{Editor, message::Message, outcome::Outcome, tasks};
use crate::{
    layout,
    state::canvas::PhotoView,
    view::{canvas::FIT_PADDING, canvas_view::CanvasView},
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
            let origin = |content: f32, available: f32, offset: f32| {
                (available - content).max(0.0) / 2.0
                    - offset.clamp(0.0, (content - available).max(0.0))
            };
            view.origin.x = origin(stage.0 as f32 * view.scale, canvas.width, pan.0);
            view.origin.y = origin(stage.1 as f32 * view.scale, canvas.height, pan.1);
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
        (pixel as f32 * new_scale - at).clamp(0.0, (length as f32 * new_scale - available).max(0.0))
    };
    let target = ViewState {
        zoom: Zoom::Percent { value: percent },
        pan_x: offset(x, pointer.x, stage.0, canvas.width),
        pan_y: offset(y, pointer.y, stage.1, canvas.height),
    };
    (target.zoom != current.zoom || (target.pan_x, target.pan_y) != pan).then_some(target)
}

impl Editor {
    pub(super) fn pinch(&mut self, input: Pinch) -> Task<Message> {
        if self.document.state.is_none()
            || self.gallery_page().is_some()
            || self.palette.open
            || self.view_state.picker_open
            || matches!(self.workspace.canvas.photo, PhotoView::Empty(_))
        {
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
