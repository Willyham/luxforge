//! The command palette: its query, its selection and running one entry, which is always an
//! existing message, so the palette reaches nothing the panels and the title bar cannot.
use super::{
    Editor,
    message::{
        Message, action::ActionMessage, export::ExportMessage, history::HistoryMessage,
        palette::PaletteMessage, performance::PerformanceMessage, view::ViewMessage,
    },
};
use crate::state::palette::PaletteAction;
use crate::view;
use iced::{Task, widget::operation};

impl Editor {
    /// One command palette message.
    pub(super) fn palette_update(&mut self, message: PaletteMessage) -> Task<Message> {
        match message {
            PaletteMessage::Open => {
                if let Some(reason) = crate::state::masks::interaction_refusal(self.mask_shape()) {
                    self.status.text = reason;
                    return Task::none();
                }
                self.palette.open = true;
                self.palette.query.clear();
                self.palette.selected = 0;
                return operation::focus(view::palette::QUERY_ID);
            }
            PaletteMessage::Close => self.palette.open = false,
            PaletteMessage::Query(query) => {
                self.palette.query = query;
                self.palette.selected = 0;
            }
            PaletteMessage::Move(delta) => {
                let last = self.workspace.palette.entries.len().saturating_sub(1);
                let moved = self.palette.selected as i64 + i64::from(delta);
                self.palette.selected = moved.clamp(0, last as i64) as usize;
            }
            PaletteMessage::Run => {
                let chosen = self
                    .workspace
                    .palette
                    .entries
                    .get(self.palette.selected)
                    .map(|entry| entry.action.clone());
                self.palette.open = false;
                return match chosen {
                    Some(PaletteAction::Run { action, preset }) => {
                        self.dispatch(Message::Action(ActionMessage::Run { action, preset }))
                    }
                    Some(PaletteAction::Mode(mode)) => {
                        self.dispatch(Message::View(ViewMessage::SetMode(mode)))
                    }
                    Some(PaletteAction::TogglePanel(panel)) => {
                        self.dispatch(Message::View(ViewMessage::TogglePanel(panel)))
                    }
                    Some(PaletteAction::TogglePerformance) => {
                        self.dispatch(Message::Performance(PerformanceMessage::Toggle))
                    }
                    Some(PaletteAction::ToggleThirds) => {
                        self.dispatch(Message::View(ViewMessage::ToggleThirds))
                    }
                    Some(PaletteAction::Fit) => self.dispatch(Message::View(ViewMessage::Fit)),
                    Some(PaletteAction::HundredPercent) => {
                        self.dispatch(Message::View(ViewMessage::HundredPercent))
                    }
                    Some(PaletteAction::Undo) => {
                        self.dispatch(Message::History(HistoryMessage::Undo))
                    }
                    Some(PaletteAction::Redo) => {
                        self.dispatch(Message::History(HistoryMessage::Redo))
                    }
                    Some(PaletteAction::ReturnCurrent) => {
                        self.dispatch(Message::History(HistoryMessage::ReturnCurrent))
                    }
                    Some(PaletteAction::Restore) => {
                        self.dispatch(Message::History(HistoryMessage::Restore))
                    }
                    Some(PaletteAction::Export { keep_metadata }) => {
                        self.dispatch(Message::Export(ExportMessage::Start { keep_metadata }))
                    }
                    None => Task::none(),
                };
            }
            PaletteMessage::RunIndex(index) => {
                self.palette.selected = index;
                return self.dispatch(Message::Palette(PaletteMessage::Run));
            }
        }
        Task::none()
    }
}
