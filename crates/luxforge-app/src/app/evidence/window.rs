//! The `window_size` evidence step: the window resized as dragging its frame does, through Iced's
//! own resize of the native window, and captured once the window reports the size asked for — so
//! the frame is laid out for it, with the canvas, the scrollable and anything placed in them
//! following the new bounds exactly as a person's resize makes them.
use super::Settle;
use crate::app::{Editor, message::Message};
use iced::{Size, Task, window};
use serde_json::json;
use std::time::{Duration, Instant};

/// How long a step waits for the window to report its new size.
const RESIZE_WAIT: Duration = Duration::from_secs(10);

/// A window reports its size in Iced's logical pixels, which round.
const SIZE_SLACK: f32 = 1.0;

/// A `window_size` step waiting for the window to report the size it asked for.
pub(crate) struct PendingSize {
    /// The size asked for, in Iced's logical pixels.
    size: Size,
    since: Instant,
}

impl Editor {
    /// Resize the window to `width` × `height` in the system's points: Iced's logical size times
    /// the interface scale, as `--window-size` gives it.
    pub(super) fn window_size_step(&mut self, width: f32, height: f32) -> Task<Message> {
        let scale = self.interface_scale();
        let size = Size::new(width / scale, height / scale);
        self.note_step(json!({"window_before": [self.view_state.window.0,
            self.view_state.window.1], "logical": [size.width, size.height]}));
        if let Some(evidence) = &mut self.evidence {
            evidence.window_size = Some(PendingSize {
                size,
                since: Instant::now(),
            });
        }
        self.await_step(Settle::Window);
        window::oldest().and_then(move |id| window::resize(id, size))
    }

    /// After every message: the waiting step settles once the window reports its size, or fails
    /// once it has waited too long.
    pub(super) fn settle_window_size(&mut self) {
        let Some((size, since)) = self
            .evidence
            .as_ref()
            .filter(|evidence| evidence.awaiting == Some(Settle::Window))
            .and_then(|evidence| evidence.window_size.as_ref())
            .map(|pending| (pending.size, pending.since))
        else {
            return;
        };
        let (width, height) = self.view_state.window;
        let reached =
            (width - size.width).abs() <= SIZE_SLACK && (height - size.height).abs() <= SIZE_SLACK;
        if !reached && since.elapsed() < RESIZE_WAIT {
            return;
        }
        if let Some(evidence) = &mut self.evidence {
            evidence.window_size = None;
        }
        if reached {
            self.note_step(json!({"window": [width, height]}));
            self.settle_step(Settle::Window, "window_resized");
        } else {
            let _ = self.fail_step(format!(
                "the window reports {width} × {height}, not the {} × {} asked for",
                size.width, size.height
            ));
        }
    }
}
