//! The `grid_scroll` evidence step (**catalog lane D**; the grid is `app/select.rs`): the Select grid
//! scrolled continuously for its timing, one offset per display frame.
//!
//! On each frame of the window's own frame clock, which exists only while the step scrolls, the
//! driver sends the offset the grid's scrollable publishes (`SelectMessage::Scrolled`) moved down
//! by the step's speed, and records `grid_scroll_frame`: the frame's time, the offset, and the
//! cells on screen at that offset with a decoded preview to draw (`cached`), the placeholder while
//! one loads (`pending`) or nothing ever (`unreadable`), and the bytes of decoded previews the grid
//! holds then against its budget. The grid records `select_scrolled` in the
//! update that adopts the offset, the one whose redraw draws it. The step is captured once Select
//! has nothing in flight after its last frame, or after the frame that reached the end of the grid.
use super::{Evidence, Settle};
use crate::app::{
    Editor,
    message::{Message, evidence::EvidenceMessage, select::SelectMessage},
};
use iced::{Subscription, Task};
use luxforge_evidence::GridScrollStep;
use serde_json::json;
use std::time::Instant;

/// A running `grid_scroll` step.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GridScrolling {
    px_per_frame: f32,
    /// Frames still to scroll.
    remaining: u32,
    /// Frames scrolled.
    sent: u32,
}

impl Editor {
    /// Start scrolling the grid on the next display frame.
    pub(super) fn grid_scroll_step(&mut self, step: GridScrollStep) -> Task<Message> {
        if !self.select_shown() || self.loupe_open() || self.select.viewport.height <= 0.0 {
            return self.fail_step("the Select grid is not shown");
        }
        if let Some(evidence) = &mut self.evidence {
            evidence.grid_scroll = Some(GridScrolling {
                px_per_frame: step.px_per_frame,
                remaining: step.frames,
                sent: 0,
            });
        }
        Task::none()
    }

    /// One display frame of the running `grid_scroll` step: the grid moved down by its speed, as
    /// its scrollable publishes an offset, and what that frame draws recorded.
    pub(super) fn grid_scroll_frame(&mut self, at: Instant) -> Task<Message> {
        let Some(scrolling) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.grid_scroll.as_mut())
        else {
            return Task::none();
        };
        let index = scrolling.sent;
        scrolling.sent += 1;
        scrolling.remaining -= 1;
        let last = scrolling.remaining == 0;
        let from = self.select.scroll;
        let to = self
            .select
            .layout
            .clamp_scroll(from + scrolling.px_per_frame, self.select.viewport.height);
        let moved = to != from;
        let task = if moved {
            self.dispatch(Message::Select(SelectMessage::Scrolled(to)))
        } else {
            Task::none()
        };
        let frame_ms = at.saturating_duration_since(self.log.started).as_secs_f64() * 1000.0;
        let (cells, cached, pending, unreadable) = self.grid_cells_drawn();
        self.event("grid_scroll_frame", || {
            let previews = self.select.previews.summary();
            json!({
                "index": index,
                "frame_ms": frame_ms,
                "offset": self.select.scroll,
                "moved": moved,
                "cells": cells,
                "cached": cached,
                "pending": pending,
                "unreadable": unreadable,
                "decoded_bytes": previews["bytes"],
                "decoded_budget": previews["budget"],
            })
        });
        if last || !moved {
            if let Some(evidence) = &mut self.evidence {
                evidence.grid_scroll = None;
            }
            self.await_step(Settle::Select);
        }
        task
    }

    /// The grid's cells on screen at its offset now: how many there are, and how many draw a
    /// decoded preview, the placeholder while one loads, or nothing ever.
    fn grid_cells_drawn(&self) -> (usize, usize, usize, usize) {
        let select = &self.select;
        let rows = &select.state.rows;
        let images = select.previews.grid(rows);
        let (mut cells, mut cached, mut unreadable) = (0, 0, 0);
        for index in select
            .layout
            .visible_cells(select.scroll, select.viewport.height, 0.0)
        {
            let cell = select.layout.cell(index);
            let shown = rows.shown(cell.item, cell.span);
            cells += 1;
            if images.image(shown).is_some() {
                cached += 1;
            } else if images.unreadable(shown) {
                unreadable += 1;
            }
        }
        (cells, cached, cells - cached - unreadable, unreadable)
    }
}

/// The window's frame clock while a `grid_scroll` step scrolls, and nothing otherwise.
pub(super) fn subscription(evidence: &Evidence) -> Option<Subscription<Message>> {
    evidence.grid_scroll.as_ref()?;
    Some(iced::window::frames().map(|at| Message::Evidence(EvidenceMessage::GridScrollFrame(at))))
}

#[cfg(test)]
mod tests {
    use crate::app::message::{Message, evidence::EvidenceMessage, select::SelectMessage};
    use crate::app::select_owner_tests::{evaluate, finish, read_rows, selecting};
    use crate::app::testing::{attach_log, attach_script, events, logged};
    use crate::state::select::SourcePress;
    use iced::Size;
    use std::time::Instant;

    /// Each display frame of a `grid_scroll` step moves the grid down by the step's speed through
    /// the offset its scrollable publishes, which the grid records in the update that adopts it;
    /// the step ends at its last frame, or at the frame that reached the end.
    #[test]
    fn a_grid_scroll_moves_the_grid_once_per_frame_and_records_what_each_draws() {
        let (mut editor, catalog) = selecting();
        let Some(SourcePress::View(source)) = editor.workspace.select.sources.months[0].rows[0]
            .press
            .clone()
        else {
            panic!("an event row views its event");
        };
        let _ = editor.update(Message::Select(SelectMessage::Source(source)));
        let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
            600.0, 160.0,
        ))));
        evaluate(&mut editor);
        read_rows(&mut editor);
        let _ = attach_script(
            &mut editor,
            r#"[{"grid_scroll":{"px_per_frame":40.0,"frames":3}}]"#,
        );
        let log = attach_log(&mut editor);
        let _ = editor.next_step();
        assert!(editor.evidence.as_ref().unwrap().grid_scroll.is_some());
        for _ in 0..3 {
            let _ = editor.update(Message::Evidence(EvidenceMessage::GridScrollFrame(
                Instant::now(),
            )));
        }
        assert!(editor.evidence.as_ref().unwrap().grid_scroll.is_none());
        let scroll = editor.select.scroll;
        let records = logged(&mut editor, &log);
        let frames = events(&records, "grid_scroll_frame");
        let scrolled = events(&records, "select_scrolled");
        assert!(!frames.is_empty() && frames.len() <= 3, "{frames:?}");
        assert_eq!(frames[0]["offset"], 40.0, "{frames:?}");
        assert!(frames.iter().all(|frame| {
            let budget = frame["decoded_budget"].as_u64();
            frame["decoded_bytes"]
                .as_u64()
                .zip(budget)
                .is_some_and(|(bytes, budget)| bytes <= budget)
        }));
        assert!(frames.iter().all(|frame| {
            frame["cells"].as_u64()
                == Some(
                    frame["cached"].as_u64().unwrap()
                        + frame["pending"].as_u64().unwrap()
                        + frame["unreadable"].as_u64().unwrap(),
                )
        }));
        assert_eq!(
            scrolled.len(),
            frames.iter().filter(|frame| frame["moved"] == true).count(),
            "the grid recorded each offset it adopted"
        );
        assert_eq!(scrolled.last().unwrap()["scroll"], scroll);
        finish(editor, catalog);
    }
}
