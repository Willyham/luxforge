//! Evidence steps on the high-zoom minimap ([`MinimapStep`]): pointer gestures routed as real mouse
//! events through the window's laid-out widgets onto the card — so the card's own capture decides
//! what anything under it sees — and the same-client `view.set` equivalent to a click.
//!
//! A step waits until the card is shown with an overview navigation can act on, resolves its point
//! against the card as it then stands, records the window points and what the routed events
//! published, and settles once the editor has handled those messages ([`Settle::Minimap`]): a wheel
//! once its pan has reached the session ([`Settle::Pan`]), an API pan once its answer has
//! ([`Settle::Host`]).
use super::Settle;
use crate::app::{
    Editor,
    message::{Message, view::ViewMessage},
    tasks::host_task,
};
use iced::{
    Event, Point, Rectangle, Task,
    mouse::{Button, Event as Mouse, ScrollDelta},
};
use luxforge_evidence::{MinimapPoint, MinimapStep};
use serde_json::json;
use std::time::{Duration, Instant};

/// How long a step waits for the card to be able to navigate.
const NAVIGABLE_WAIT: Duration = Duration::from_secs(10);

/// A minimap step waiting for the card to be able to navigate.
pub(crate) struct PendingMinimap {
    step: MinimapStep,
    since: Instant,
}

fn at(rect: Rectangle, [x, y]: [f32; 2]) -> Point {
    Point::new(rect.x + rect.width * x, rect.y + rect.height * y)
}

impl Editor {
    pub(super) fn minimap_step(&mut self, step: MinimapStep) -> Task<Message> {
        if let Some(evidence) = &mut self.evidence {
            evidence.minimap = Some(PendingMinimap {
                step,
                since: Instant::now(),
            });
            evidence.awaiting = Some(Settle::Minimap);
        }
        self.minimap_step_when_navigable()
    }

    /// Send the waiting step once the card can navigate, or fail it once it has waited too long.
    pub(super) fn minimap_step_when_navigable(&mut self) -> Task<Message> {
        let Some(since) = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.minimap.as_ref())
            .map(|pending| pending.since)
        else {
            return Task::none();
        };
        let Some(minimap) = self
            .minimap(&self.surfaces())
            .filter(|minimap| minimap.navigable)
        else {
            if since.elapsed() >= NAVIGABLE_WAIT {
                if let Some(evidence) = &mut self.evidence {
                    evidence.minimap = None;
                }
                let state = self.minimap_summary();
                self.note_step(json!({"minimap_before": state}));
                return self.fail_step("the minimap was not shown with a navigable overview");
            }
            return Task::none();
        };
        let Some(pending) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.minimap.take())
        else {
            return Task::none();
        };
        let resolve = |point: MinimapPoint| -> Option<Point> {
            Some(match point {
                MinimapPoint::Image(fraction) => at(minimap.image_window(), fraction),
                MinimapPoint::Rectangle(fraction) => at(minimap.rectangle_window()?, fraction),
                MinimapPoint::Card(fraction) => at(minimap.card_window(), fraction),
            })
        };
        let centre = at(minimap.card_window(), [0.5, 0.5]);
        let moved = |point: Point| (Event::Mouse(Mouse::CursorMoved { position: point }), point);
        let pressed = |point: Point| (Event::Mouse(Mouse::ButtonPressed(Button::Left)), point);
        let released = |point: Point| (Event::Mouse(Mouse::ButtonReleased(Button::Left)), point);
        let origin = minimap.area_origin();
        let before = self.minimap_summary();
        let point = match &pending.step {
            MinimapStep::Press(point)
            | MinimapStep::Move(point)
            | MinimapStep::Release(point)
            | MinimapStep::Click(point)
            | MinimapStep::Api(point) => match resolve(*point) {
                Some(point) => Some(point),
                None => return self.fail_step("the minimap shows no viewport rectangle"),
            },
            MinimapStep::Wheel(_) | MinimapStep::Pinch(_) => None,
        };
        self.note_step(json!({
            "minimap_before": before,
            "window_point": point.map(|point| [point.x, point.y]),
            "area_point": point.map(|point| [point.x - origin.x, point.y - origin.y]),
            "card_centre": [centre.x, centre.y],
        }));
        let events = match pending.step {
            MinimapStep::Press(_) => {
                let point = point.expect("a resolved point");
                vec![moved(point), pressed(point)]
            }
            MinimapStep::Move(_) => vec![moved(point.expect("a resolved point"))],
            MinimapStep::Release(_) => {
                let point = point.expect("a resolved point");
                vec![moved(point), released(point)]
            }
            MinimapStep::Click(_) => {
                let point = point.expect("a resolved point");
                vec![moved(point), pressed(point), released(point)]
            }
            MinimapStep::Wheel([x, y]) => vec![
                moved(centre),
                (
                    Event::Mouse(Mouse::WheelScrolled {
                        delta: ScrollDelta::Pixels { x, y },
                    }),
                    centre,
                ),
            ],
            MinimapStep::Pinch(delta) => {
                // Through the native input's own message, which takes AppKit's points.
                let scale = f64::from(self.view_state.interface_scale());
                self.capture_next_frame();
                return self.update(Message::View(ViewMessage::Pinch(luxforge_input::Pinch {
                    delta,
                    x: f64::from(centre.x) * scale,
                    y: f64::from(centre.y) * scale,
                })));
            }
            MinimapStep::Api(_) => {
                // The pan an API client computes from `view.set`'s pan units: logical pixels from
                // the zoomed stage's top-left corner to the viewport's, centring the viewport on
                // the stage point as far as the stage's edges allow.
                let point = point.expect("a resolved point");
                let image = minimap.image_window();
                let viewport = &minimap.viewport;
                let (width, height) = viewport.stage().size;
                let scale = f64::from(viewport.scale());
                let canvas = minimap.canvas;
                let axis = |fraction: f64, extent: f64, view: f32| {
                    let content = extent * scale;
                    let range = (content - f64::from(view)).max(0.0);
                    (fraction * extent * scale - f64::from(view) / 2.0).clamp(0.0, range)
                };
                let pan = (
                    axis(
                        f64::from((point.x - image.x) / image.width),
                        width,
                        canvas.width,
                    ),
                    axis(
                        f64::from((point.y - image.y) / image.height),
                        height,
                        canvas.height,
                    ),
                );
                let params = json!({"pan_x": pan.0, "pan_y": pan.1});
                self.note_step(json!({"api_params": params}));
                self.await_step(Settle::Host);
                return host_task(
                    self.owner.clone(),
                    self.client,
                    "view.set".into(),
                    params,
                );
            }
        };
        if let Some(evidence) = &self.evidence {
            evidence.sync.cursor.route(events);
        }
        if matches!(pending.step, MinimapStep::Wheel(_)) {
            self.await_step(Settle::Pan);
        }
        Task::none()
    }

    /// The routed events were dispatched: record what they published, and capture once the editor
    /// has handled it.
    pub(super) fn settle_minimap(&mut self) {
        let Some(messages) = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.sync.cursor.route_messages())
        else {
            return;
        };
        self.note_step(json!({"routed_messages": messages}));
        self.settle_step(Settle::Minimap, "minimap_routed");
    }
}
