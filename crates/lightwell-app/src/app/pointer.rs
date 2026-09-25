//! The pointer over the photograph: the hover readout, one `render.sample` in flight at a time, and
//! canvas picks, located through the core and answered by the mode on screen.
use super::{Editor, message::Message};
use super::{gesture::Starting, tasks::sample_task};
use crate::state::tools;
use iced::Task;
use lightwell_core::ModuleDescriptor;

/// The reset a group declares, found by its position in the module's controls.
/// The active canvas mode's declared pick, with its names owned so the update function can act on
/// them while it mutates the editor. It is [`tools::CanvasPick`] with the borrows resolved and
/// nothing else: no module is named here and no coordinate name is assumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PickTarget {
    Point {
        action: String,
        x: String,
        y: String,
    },
    Sample {
        query: String,
        x: String,
        y: String,
        action: String,
    },
    /// The host's own pair: a `mask.*` read answers the pixel the masked operation receives and a
    /// `mask.*` command receives it, addressed to the mask and component the panel has open.
    HostSample {
        query: String,
        x: String,
        y: String,
        action: String,
    },
}

impl PickTarget {
    pub(super) fn of(modules: &[ModuleDescriptor], mode: &str) -> Option<Self> {
        match tools::canvas_pick(modules, mode)? {
            tools::CanvasPick::Point { action, x, y } => Some(Self::Point {
                action: action.to_owned(),
                x: x.to_owned(),
                y: y.to_owned(),
            }),
            tools::CanvasPick::Sample {
                query,
                x,
                y,
                action,
            } => Some(Self::Sample {
                query: query.to_owned(),
                x: x.to_owned(),
                y: y.to_owned(),
                action: action.to_owned(),
            }),
            tools::CanvasPick::HostSample {
                query,
                x,
                y,
                action,
            } => Some(Self::HostSample {
                query: query.to_owned(),
                x: x.to_owned(),
                y: y.to_owned(),
                action: action.to_owned(),
            }),
        }
    }
}

impl Editor {
    /// Ask for the pixel under the pointer, throttled to one request in flight with only the newest
    /// position waiting. `render.sample` is a point query: it evaluates one coordinate of the
    /// compiled recipe and rasterizes nothing.
    pub(super) fn sample(&mut self, x: u32, y: u32) -> Task<Message> {
        if self.sample_in_flight {
            self.pending_sample = Some((x, y));
            return Task::none();
        }
        let Some(state) = &self.state else {
            return Task::none();
        };
        let Some(entry) = self.displayed_entry() else {
            return Task::none();
        };
        self.sample_in_flight = true;
        sample_task(
            self.owner.clone(),
            self.client,
            state.asset.id.clone(),
            entry,
            None,
            x,
            y,
        )
    }

    /// Why a click on the photograph cannot be picked right now, in the words the status bar uses.
    ///
    /// A sample-apply pick commits, so it obeys the same one-draft rule every other commit does: a
    /// draft is finished deliberately, never displaced by a click. A point pick commits nothing,
    /// but it answers to the same rule so that one sentence describes every canvas pick.
    pub(super) fn pick_refusal(&self) -> Option<String> {
        if let Some(reason) = self.gesture_refusal(Starting::Pick) {
            return Some(reason);
        }
        if !self.session.preview.can_edit() {
            return Some("Return to the current state before picking from the photograph".into());
        }
        if self.state.is_none() {
            return Some("No photograph is open".into());
        }
        self.busy.then(|| "Waiting for the last request".to_owned())
    }
}
