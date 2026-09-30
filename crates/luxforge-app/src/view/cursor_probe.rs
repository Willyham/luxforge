//! Evidence-only native cursor input. The wrapper forwards a queued Mouse::CursorMoved through
//! the actual widget tree on its next redraw and keeps that cursor available while drawing.
//! The mask canvas reports its real geometry construction, paired with the input by one epoch.
//! There is one pending input and at most 240 traces, no polling, source pixels or ordinary-session
//! state. A paced path coalesces overdue positions before the widget sees them.
use crate::app::message::{Message, pointer::PointerMessage};
use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
        widget::{Operation, Tree},
    },
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub(crate) struct Probe(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    serial: u64,
    input: Option<Input>,
    trace: Option<Value>,
    path: VecDeque<(Instant, Point)>,
    completed: Vec<Value>,
    coalesced: usize,
}

struct Input {
    epoch: u64,
    point: Point,
    requested: Instant,
    dispatched: Option<Instant>,
    update_ms: f64,
    readout_position: Option<(u32, u32)>,
    message_count: usize,
    pointer_at_ms: Option<f64>,
    pointer_ms: Option<f64>,
    loop_ms: Option<[f64; 2]>,
    loop_pending: bool,
}

thread_local! {
    /// Installed only while an evidence wrapper draws. Normal canvas draws retain no trace.
    static DRAWING: RefCell<Option<Probe>> = const { RefCell::new(None) };
}

impl Probe {
    pub(crate) fn sweep(&self, points: &[Point], interval_ms: u64) -> u64 {
        self.clear();
        let mut state = self.0.lock().expect("cursor probe lock");
        let at = Instant::now();
        state.path = points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                (
                    at + Duration::from_millis(index as u64 * interval_ms),
                    *point,
                )
            })
            .collect();
        state.serial.saturating_add(1)
    }
    pub(crate) fn arm(&self, point: Point) -> u64 {
        self.clear();
        let mut state = self.0.lock().expect("cursor probe lock");
        state.serial = state.serial.saturating_add(1);
        let epoch = state.serial;
        state.input = Some(Input {
            epoch,
            point,
            requested: Instant::now(),
            dispatched: None,
            update_ms: 0.0,
            readout_position: None,
            message_count: 0,
            pointer_at_ms: None,
            pointer_ms: None,
            loop_ms: None,
            loop_pending: false,
        });
        state.trace = None;
        epoch
    }

    pub(crate) fn clear(&self) {
        let mut state = self.0.lock().expect("cursor probe lock");
        state.input = None;
        state.trace = None;
        state.path.clear();
        state.completed.clear();
        state.coalesced = 0;
    }

    fn pending(&self) -> Option<(u64, Point)> {
        let mut state = self.0.lock().expect("cursor probe lock");
        let ready = state
            .input
            .as_ref()
            .is_none_or(|input| input.loop_ms.is_some() && state.trace.is_some());
        if ready
            && state
                .path
                .front()
                .is_some_and(|(at, _)| *at <= Instant::now())
        {
            if let Some(trace) = trace(&state) {
                state.completed.push(trace);
            }
            let mut next = state.path.pop_front().expect("due position");
            while state
                .path
                .front()
                .is_some_and(|(at, _)| *at <= Instant::now())
            {
                next = state.path.pop_front().expect("due position");
                state.coalesced += 1;
            }
            state.serial = state.serial.saturating_add(1);
            let epoch = state.serial;
            state.input = Some(Input {
                epoch,
                point: next.1,
                requested: next.0,
                dispatched: None,
                update_ms: 0.0,
                readout_position: None,
                message_count: 0,
                pointer_at_ms: None,
                pointer_ms: None,
                loop_ms: None,
                loop_pending: false,
            });
            state.trace = None;
        }
        state
            .input
            .as_ref()
            .filter(|input| input.dispatched.is_none())
            .map(|input| (input.epoch, input.point))
    }

    fn next_redraw(&self) -> Option<Instant> {
        self.0
            .lock()
            .expect("cursor probe lock")
            .path
            .front()
            .map(|(at, _)| *at)
    }

    fn cursor(&self, fallback: mouse::Cursor) -> mouse::Cursor {
        self.0
            .lock()
            .expect("cursor probe lock")
            .input
            .as_ref()
            .filter(|input| input.dispatched.is_some())
            .map_or(fallback, |input| mouse::Cursor::Available(input.point))
    }

    pub(crate) fn waiting(&self) -> bool {
        let state = self.0.lock().expect("cursor probe lock");
        !state.path.is_empty()
            || state
                .input
                .as_ref()
                .is_some_and(|input| state.trace.is_none() || input.loop_ms.is_none())
    }

    pub(crate) fn trace(&self) -> Option<Value> {
        let state = self.0.lock().expect("cursor probe lock");
        let mut result = trace(&state)?;
        if !state.completed.is_empty() {
            let mut samples = state.completed.clone();
            samples.push(result.clone());
            result["samples"] = json!(samples);
        }
        result["coalesced"] = json!(state.coalesced);
        Some(result)
    }

    pub(crate) fn pointer_started(&self, position: Option<(u32, u32)>) -> Option<Instant> {
        let state = self.0.lock().expect("cursor probe lock");
        state
            .input
            .as_ref()
            .filter(|input| {
                input.dispatched.is_some()
                    && input.readout_position.is_some()
                    && input.readout_position == position
                    && input.pointer_at_ms.is_none()
            })
            .map(|_| Instant::now())
    }

    pub(crate) fn pointer_updated(&self, position: Option<(u32, u32)>, elapsed_ms: f64) {
        let mut state = self.0.lock().expect("cursor probe lock");
        if let Some(input) = &mut state.input
            && input.readout_position == position
            && position.is_some()
            && input.pointer_at_ms.is_none()
        {
            input.pointer_at_ms = Some(input.requested.elapsed().as_secs_f64() * 1000.0);
            input.pointer_ms = Some(elapsed_ms);
            input.loop_pending = true;
        }
    }

    /// Only the first editor loop that handled this input's matching Moved message is recorded.
    /// An unrelated result, capture tick or later redraw cannot overwrite that loop's timing.
    pub(crate) fn editor_loop_observed(&self, update_ms: f64, rederive_ms: f64) {
        let mut state = self.0.lock().expect("cursor probe lock");
        if let Some(input) = &mut state.input
            && input.loop_pending
        {
            input.loop_ms = Some([update_ms, rederive_ms]);
            input.loop_pending = false;
        }
    }

    fn routed(
        &self,
        epoch: u64,
        at: Instant,
        elapsed_ms: f64,
        message_count: usize,
        readout_position: Option<(u32, u32)>,
    ) {
        let mut state = self.0.lock().expect("cursor probe lock");
        if let Some(input) = &mut state.input
            && input.epoch == epoch
        {
            input.dispatched = Some(at);
            input.update_ms = elapsed_ms;
            input.message_count = message_count;
            input.readout_position = readout_position;
        }
    }

    fn drawn(&self, centre: Point, bounds: Rectangle, geometry_ms: f64) {
        let mut state = self.0.lock().expect("cursor probe lock");
        if state.trace.is_some() {
            return;
        }
        let Some(input) = &state.input else {
            return;
        };
        let Some(dispatched) = input.dispatched else {
            return;
        };
        // Percent scrollables translate the window cursor and bounds together. Their difference
        // is the cursor's actual local centre; the trace records both for capture correlation.
        state.trace = Some(json!({
            "epoch":input.epoch,
            "window_position":[input.point.x,input.point.y],
            "canvas_position":[centre.x,centre.y],
            "canvas_bounds":{"x":bounds.x,"y":bounds.y,"width":bounds.width,"height":bounds.height},
            "readout_position":input.readout_position.map(|(x,y)| [x,y]),
            "widget_messages":input.message_count,
            "dispatch_delay_ms":dispatched.duration_since(input.requested).as_secs_f64()*1000.0,
            "widget_update_ms":input.update_ms,
            "input_to_cursor_geometry_ms":input.requested.elapsed().as_secs_f64()*1000.0,
            "cursor_geometry_ms":geometry_ms,
            "scope":"Mouse CursorMoved routed through the real laid-out widget tree and brush cursor geometry built in MaskCanvas::draw; CPU geometry submission, not GPU completion or display scanout",
        }));
    }
}

fn trace(state: &State) -> Option<Value> {
    let mut result = state.trace.clone()?;
    let input = state.input.as_ref()?;
    result["input_to_pointer_update_ms"] = json!(input.pointer_at_ms);
    result["pointer_update_ms"] = json!(input.pointer_ms);
    result["editor_update_ms"] = json!(input.loop_ms.map(|timing| timing[0]));
    result["editor_rederive_ms"] = json!(input.loop_ms.map(|timing| timing[1]));
    Some(result)
}

/// Called after the real brush canvas builds its geometry. No trace exists in an ordinary session.
pub(crate) fn mask_cursor_drawn(centre: Point, bounds: Rectangle, geometry_ms: f64) {
    DRAWING.with(|active| {
        if let Some(probe) = active.borrow().as_ref() {
            probe.drawn(centre, bounds, geometry_ms);
        }
    });
}

pub(crate) fn geometry_started() -> Option<Instant> {
    DRAWING.with(|active| active.borrow().is_some().then(Instant::now))
}

pub(crate) fn wrap<'a>(content: Element<'a, Message>, probe: Probe) -> Element<'a, Message> {
    Element::new(NativeCursor { content, probe })
}

struct NativeCursor<'a> {
    content: Element<'a, Message>,
    probe: Probe,
}

impl Widget<Message, Theme, Renderer> for NativeCursor<'_> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if matches!(
            event,
            Event::Window(iced::window::Event::RedrawRequested(_))
        ) && let Some((epoch, point)) = self.probe.pending()
        {
            let at = Instant::now();
            let mut messages = Vec::new();
            let mut routed = Shell::new(&mut messages);
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                &Event::Mouse(mouse::Event::CursorMoved { position: point }),
                layout,
                mouse::Cursor::Available(point),
                renderer,
                clipboard,
                &mut routed,
                viewport,
            );
            let elapsed = at.elapsed().as_secs_f64() * 1000.0;
            let count = Cell::new(0);
            let readout = Cell::new(None);
            shell.merge(routed, |message| {
                count.set(count.get() + 1);
                if let Message::Pointer(PointerMessage::Moved(position)) = &message {
                    readout.set(*position);
                }
                message
            });
            self.probe
                .routed(epoch, at, elapsed, count.get(), readout.get());
            shell.request_redraw();
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            self.probe.cursor(cursor),
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if let Some(at) = self.probe.next_redraw() {
            shell.request_redraw_at(iced::window::RedrawRequest::At(at));
        }
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        DRAWING.with(|active| *active.borrow_mut() = Some(self.probe.clone()));
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            self.probe.cursor(cursor),
            viewport,
        );
        DRAWING.with(|active| *active.borrow_mut() = None);
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            self.probe.cursor(cursor),
            viewport,
            renderer,
        )
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_trace_records_only_its_matching_editor_loop_and_replacing_input_clears_it() {
        let probe = Probe::default();
        let epoch = probe.arm(Point::new(10.0, 20.0));
        probe.routed(epoch, Instant::now(), 0.01, 1, Some((3, 4)));
        probe.drawn(
            Point::new(5.0, 7.0),
            Rectangle::new(Point::new(5.0, 13.0), Size::new(50.0, 50.0)),
            0.02,
        );
        assert!(
            probe.waiting(),
            "drawing alone has not observed the editor route"
        );
        assert!(probe.pointer_started(Some((8, 9))).is_none());
        probe.editor_loop_observed(99.0, 98.0);
        assert!(probe.trace().unwrap()["editor_update_ms"].is_null());
        probe.pointer_updated(Some((3, 4)), 0.03);
        probe.editor_loop_observed(0.04, 0.02);
        probe.editor_loop_observed(100.0, 100.0);
        let trace = probe.trace().unwrap();
        assert_eq!(trace["epoch"], json!(epoch));
        assert_eq!(trace["readout_position"], json!([3, 4]));
        assert_eq!(trace["editor_update_ms"], json!(0.04));
        assert_eq!(trace["editor_rederive_ms"], json!(0.02));
        assert!(!probe.waiting());
        let newest = probe.arm(Point::new(30.0, 40.0));
        assert!(newest > epoch && probe.trace().is_none());
        assert_eq!(probe.pending(), Some((newest, Point::new(30.0, 40.0))));
        probe.clear();
        assert!(!probe.waiting() && probe.pending().is_none() && probe.trace().is_none());
    }

    #[test]
    fn overdue_hover_inputs_keep_only_the_newest_position_and_report_every_coalesced_one() {
        let probe = Probe::default();
        let points = vec![Point::new(10.0, 20.0); 240];
        probe.sweep(&points, 1);
        {
            let mut state = probe.0.lock().unwrap();
            for (at, _) in &mut state.path {
                *at = Instant::now() - Duration::from_secs(1);
            }
            state.path.back_mut().unwrap().1 = Point::new(30.0, 40.0);
        }
        assert_eq!(probe.pending(), Some((1, Point::new(30.0, 40.0))));
        let state = probe.0.lock().unwrap();
        assert!(state.path.is_empty());
        assert_eq!(state.coalesced, 239);
        assert!(state.completed.is_empty());
    }
}
