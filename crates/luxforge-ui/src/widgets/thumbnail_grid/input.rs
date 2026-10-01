//! The grid's response to input — presses, double clicks, the wheel, the scrollbar, the moment
//! action and the viewport size — as a function of the widget, its input state, one event and a
//! text measure, so the tests drive it without a window or a renderer.

use super::layout::Target;
use super::paint::{self, Hover, Measure};
use super::{GridBlock, GridContext, GridPress, PressModifiers, ThumbnailGrid};
use iced::advanced::mouse;
use iced::{Event, Point, Rectangle, Size, keyboard};

/// What the widget remembers between events.
#[derive(Debug, Default)]
pub(super) struct Input {
    pub(super) modifiers: keyboard::Modifiers,
    /// The last press on a cell and which cell it was.
    pub(super) last_click: Option<(mouse::Click, u32)>,
    /// While the scroller is dragged: where in it the pointer took it.
    pub(super) drag: Option<f32>,
    pub(super) hover: Hover,
    /// The caller's scroll offset when the widget last published one, and the offset published.
    pub(super) pending: Option<(f32, f32)>,
    /// The size last published through `on_viewport`.
    pub(super) published: Option<Size>,
}

/// What an event asks of the shell besides its messages.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Response {
    pub(super) capture: bool,
    pub(super) redraw: bool,
}

impl<M> ThumbnailGrid<'_, M> {
    /// Responds to `event` with the widget at `bounds`, publishing its messages through `publish`.
    pub(super) fn respond(
        &self,
        input: &mut Input,
        measure: &mut impl Measure,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        publish: &mut impl FnMut(M),
    ) -> Response {
        let mut response = Response::default();
        if let Some(on_viewport) = &self.on_viewport {
            let size = bounds.size();
            if size != self.viewport && input.published != Some(size) {
                input.published = Some(size);
                publish(on_viewport(size));
            }
        }
        let scroll = self.current_scroll(input, bounds.height);
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                input.modifiers = *modifiers;
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if !cursor.is_over(bounds) || self.on_scroll.is_none() {
                    return response;
                }
                let moved = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => y * self.layout.row_step(),
                    mouse::ScrollDelta::Pixels { y, .. } => *y,
                };
                let next = self.layout.clamp_scroll(scroll - moved, bounds.height);
                if next != scroll {
                    self.publish_scroll(input, publish, next);
                    response.capture = true;
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(button))
                if *button == mouse::Button::Right
                    || (cfg!(target_os = "macos")
                        && *button == mouse::Button::Left
                        && input.modifiers.control()) =>
            {
                // A secondary press on a cell: a right-click, or a Control-click on macOS.
                let (Some(position), Some(on_context)) =
                    (cursor.position_in(bounds), &self.on_context)
                else {
                    return response;
                };
                let content = Point::new(position.x, position.y + scroll);
                if let Some(Target::Cell(cell)) = self.layout.hit_detail(content) {
                    let grid = self.layout.cell(cell);
                    publish(on_context(GridContext {
                        cell,
                        item: grid.item,
                        span: grid.span,
                        at: position,
                    }));
                    response.capture = true;
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(position) = cursor.position_in(bounds) else {
                    return response;
                };
                if let Some(bar) = paint::scrollbar(self.layout.height(), scroll, bounds.size())
                    && bar.zone.contains(position)
                {
                    if position.y >= bar.scroller.y
                        && position.y < bar.scroller.y + bar.scroller.height
                    {
                        input.drag = Some(position.y - bar.scroller.y);
                        response.redraw = true;
                    } else {
                        // A press in the track pages toward it.
                        let page = if position.y < bar.scroller.y {
                            -bounds.height
                        } else {
                            bounds.height
                        };
                        let next = self.layout.clamp_scroll(scroll + page, bounds.height);
                        if next != scroll {
                            self.publish_scroll(input, publish, next);
                        }
                    }
                    response.capture = true;
                    return response;
                }
                let content = Point::new(position.x, position.y + scroll);
                match self.layout.hit_detail(content) {
                    Some(Target::Cell(cell)) => {
                        let previous = input
                            .last_click
                            .filter(|(_, last)| *last == cell)
                            .map(|(click, _)| click);
                        let click = mouse::Click::new(position, mouse::Button::Left, previous);
                        input.last_click = Some((click, cell));
                        if let Some(on_press) = &self.on_press {
                            let grid = self.layout.cell(cell);
                            publish(on_press(GridPress {
                                cell,
                                item: grid.item,
                                span: grid.span,
                                modifiers: PressModifiers {
                                    shift: input.modifiers.shift(),
                                    command: input.modifiers.command(),
                                },
                                double: click.kind() == mouse::click::Kind::Double,
                            }));
                            response.capture = true;
                        }
                    }
                    Some(Target::Header(_)) => {
                        if let Some(index) = self.action_at(position, scroll, measure)
                            && let Some(on_action) = &self.on_moment_action
                        {
                            publish(on_action(self.layout.frames()[index as usize].moment));
                            response.capture = true;
                        }
                    }
                    None => {}
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let Some(grab) = input.drag {
                    if let (Some(position), Some(bar)) = (
                        cursor.position(),
                        paint::scrollbar(self.layout.height(), scroll, bounds.size()),
                    ) {
                        let next = bar.scroll_at(position.y - bounds.y - grab);
                        if next != scroll {
                            self.publish_scroll(input, publish, next);
                        }
                    }
                    response.capture = true;
                    return response;
                }
                let hover = cursor.position_in(bounds).map_or(Hover::None, |position| {
                    self.hover(position, bounds.size(), scroll, measure)
                });
                if hover != input.hover {
                    input.hover = hover;
                    response.redraw = true;
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                if input.hover != Hover::None {
                    input.hover = Hover::None;
                    response.redraw = true;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if input.drag.take().is_some() {
                    response.capture = true;
                    response.redraw = true;
                }
            }
            _ => {}
        }
        response
    }

    /// The scroll offset to continue from: the one last published while the caller has not yet
    /// passed a new one, otherwise the caller's, clamped. Iced hands a widget every event of a frame
    /// before the caller rebuilds it, so a trackpad's several deltas in one frame add up.
    pub(super) fn current_scroll(&self, input: &Input, viewport: f32) -> f32 {
        match input.pending {
            Some((base, target)) if base == self.scroll => target,
            _ => self.layout.clamp_scroll(self.scroll, viewport),
        }
    }

    fn publish_scroll(&self, input: &mut Input, publish: &mut impl FnMut(M), scroll: f32) {
        if let Some(on_scroll) = &self.on_scroll {
            input.pending = Some((self.scroll, scroll));
            publish(on_scroll(scroll));
        }
    }

    /// What `position` (widget coordinates) is over that draws differently: the scrollbar, or a
    /// moment's action button.
    pub(super) fn hover(
        &self,
        position: Point,
        size: Size,
        scroll: f32,
        measure: &mut impl Measure,
    ) -> Hover {
        if let Some(bar) = paint::scrollbar(self.layout.height(), scroll, size)
            && bar.zone.contains(position)
        {
            return Hover::Scrollbar;
        }
        match self.action_at(position, scroll, measure) {
            Some(frame) => Hover::Action(frame),
            None => Hover::None,
        }
    }

    /// The frame whose action button is under `position`, as the header's plan places it.
    fn action_at(&self, position: Point, scroll: f32, measure: &mut impl Measure) -> Option<u32> {
        let content = Point::new(position.x, position.y + scroll);
        let Some(Target::Header(index)) = self.layout.hit_detail(content) else {
            return None;
        };
        let frame = &self.layout.frames()[index as usize];
        let GridBlock::Moment { header, .. } = self.layout.block(frame.block) else {
            return None;
        };
        let metrics = self.layout.metrics();
        let strip = paint::header_strip(frame, metrics, scroll);
        let plan = paint::plan_header(header, strip, metrics, measure);
        plan.action
            .is_some_and(|(_, rect)| rect.contains(position))
            .then_some(index)
    }
}

#[cfg(test)]
mod tests;
