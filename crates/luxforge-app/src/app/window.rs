//! The window's remembered frame ([design](../../../../docs/design/preferences.md#behaviour)): a
//! window opened at it is checked against the display it opened on, and the close stores where
//! the window is, in the system's points, through the desktop's one preference writer.
use super::{
    Editor,
    message::{Message, view::ViewMessage},
};
use crate::{
    state::preferences::PreferenceChange,
    window_frame::{self, Placement, WindowReport},
};
use iced::{Point, Size, Task, window};
use serde_json::json;

impl Editor {
    /// Iced reported where a window opened at its remembered frame is. A frame that does not lie
    /// within its display is centred on the main display at its size shrunk to fit; one no display
    /// holds is first moved to the main display and checked again there.
    pub(super) fn window_placed(
        &mut self,
        report: Option<WindowReport>,
        on_main: bool,
    ) -> Task<Message> {
        let scale = self.interface_scale();
        let Some((report, frame)) = report.and_then(|report| Some((report, report.frame(scale)?)))
        else {
            return Task::none();
        };
        match window_frame::placement(frame, report.display(scale), on_main) {
            Placement::Keep => Task::none(),
            Placement::ToMain => {
                self.event("window_moved_to_main_display", || json!({"frame": frame}));
                window::oldest()
                    .and_then(|id| window::move_to(id, Point::ORIGIN))
                    .chain(window_frame::report().map(|report| {
                        Message::View(ViewMessage::Placed {
                            report,
                            on_main: true,
                        })
                    }))
            }
            Placement::Fit(fit) => {
                self.event("window_fitted", || json!({"frame": frame, "fitted": fit}));
                window::oldest().and_then(move |id| {
                    Task::batch([
                        window::resize(id, Size::new(fit.width / scale, fit.height / scale)),
                        window::move_to(id, Point::new(fit.x, fit.y)),
                    ])
                })
            }
        }
    }

    /// Iced reported where the window is as it closes: store its frame, unless it fills the
    /// screen, which keeps the frame stored before, then close once the write has landed.
    pub(super) fn closing_frame(&mut self, report: Option<WindowReport>) -> Task<Message> {
        self.view_state.memory.waiting = false;
        let frame = report.and_then(|report| {
            report.stored_frame(self.interface_scale(), self.view_state.fullscreen)
        });
        let unchanged = self
            .preferences
            .applied()
            .is_some_and(|preferences| preferences.window == frame);
        let stored = match frame {
            Some(frame) if !unchanged => self.store_preferences(PreferenceChange {
                window: Some(Some(frame)),
                ..PreferenceChange::default()
            }),
            _ => Task::none(),
        };
        Task::batch([stored, self.close()])
    }
}
