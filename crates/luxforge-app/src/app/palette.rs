//! The command palette: its query, its selection and running one entry, which is always an
//! existing message, so the palette reaches nothing the panels, the title bar and the Settings
//! sheet cannot.
use super::{
    Editor,
    message::{
        Message, action::ActionMessage, export::ExportMessage, history::HistoryMessage,
        palette::PaletteMessage, performance::PerformanceMessage, settings::SettingsMessage,
        theme::ThemeMessage, view::ViewMessage,
    },
};
use crate::state::palette::{PaletteAction, Panel, RevealTarget, Revealed};
use crate::state::tools::{self, RevealKey};
use crate::view;
use iced::{Subscription, Task, widget::operation};
use luxforge_core::{Control, ModuleLayout};
use std::time::Duration;

/// How long a revealed section or control stays marked.
const REVEAL_MARK: Duration = Duration::from_millis(1600);

impl Editor {
    /// One command palette message.
    pub(super) fn palette_update(&mut self, message: PaletteMessage) -> Task<Message> {
        match message {
            PaletteMessage::Open => {
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
                    Some(PaletteAction::Reveal(target)) => self.reveal(target),
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
                    Some(PaletteAction::ToggleInformation) => {
                        self.dispatch(Message::View(ViewMessage::ToggleInformation))
                    }
                    Some(PaletteAction::Fit) => self.dispatch(Message::View(ViewMessage::Fit)),
                    Some(PaletteAction::HundredPercent) => {
                        self.dispatch(Message::View(ViewMessage::HundredPercent))
                    }
                    Some(PaletteAction::CopySettings(kind)) => {
                        use super::message::copy_settings::CopySettingsMessage as C;
                        self.dispatch(Message::CopySettings(match kind {
                            0 | 1 => C::Copy {
                                choose: kind == 1,
                                source: None,
                            },
                            2 => C::Paste,
                            _ => C::Previous,
                        }))
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
                    Some(PaletteAction::Compare) => {
                        self.dispatch(Message::History(HistoryMessage::CompareToggle))
                    }
                    Some(PaletteAction::Export {
                        keep_metadata,
                        reference,
                    }) => self.dispatch(Message::Export(ExportMessage::Start {
                        keep_metadata,
                        reference,
                    })),
                    Some(PaletteAction::Settings(tab)) => {
                        self.dispatch(Message::Settings(SettingsMessage::Open(tab)))
                    }
                    Some(PaletteAction::Theme(id)) => {
                        self.dispatch(Message::Theme(ThemeMessage::Choose(id)))
                    }
                    None => Task::none(),
                };
            }
            PaletteMessage::RunIndex(index) => {
                self.palette.selected = index;
                return self.dispatch(Message::Palette(PaletteMessage::Run));
            }
            PaletteMessage::Unmark(sequence) => {
                if self
                    .palette
                    .revealed
                    .as_ref()
                    .is_some_and(|revealed| revealed.sequence == sequence)
                {
                    self.palette.revealed = None;
                }
            }
        }
        Task::none()
    }

    /// Open the target's section and collapse every other, open the groups and the tab that hold
    /// its control, show the tools panel if it is hidden, and mark the target for a moment. Every
    /// change is this client's view state, exactly what clicking the headers and the tab would set;
    /// the panel scrolls to the target in [`after_derive`] once it is drawn, and [`subscription`]
    /// ends the mark.
    fn reveal(&mut self, target: RevealTarget) -> Task<Message> {
        for module in &self.modules {
            self.controls
                .expanded
                .insert(module.id.clone(), module.id == target.module_id);
        }
        if let (Some((path, key)), Some(module)) = (
            &target.control,
            tools::module_of(&self.modules, &target.module_id),
        ) {
            // The groups holding the control, and a revealed group itself.
            let groups = match key {
                RevealKey::Group(_) => path.len(),
                RevealKey::Field(_) => path.len().saturating_sub(1),
            };
            for depth in 1..=groups {
                self.controls
                    .ui
                    .group_expanded
                    .insert(tools::group_key(&module.id, &path[..depth]), true);
            }
            // A tabbed module's top-level groups are its tabs, counted among its groups only.
            if module.layout == ModuleLayout::Tabs
                && let Some(top) = path.first()
                && matches!(module.controls.get(*top), Some(Control::Group(_)))
            {
                let tab = module.controls[..*top]
                    .iter()
                    .filter(|control| matches!(control, Control::Group(_)))
                    .count();
                self.controls.ui.selected_tab.insert(module.id.clone(), tab);
            }
        }
        self.palette.reveal_sequence += 1;
        self.palette.revealed = Some(Revealed {
            target,
            sequence: self.palette.reveal_sequence,
            scrolled: false,
        });
        if self.session.workspace.tools_panel {
            Task::none()
        } else {
            self.dispatch(Message::View(ViewMessage::TogglePanel(Panel::Tools)))
        }
    }
}

/// After the screen is derived: scroll the tools panel to a reveal's target once the panel draws
/// it. The scroll is measured on the laid-out panel, so it runs as a widget operation after this
/// update's view is built.
pub(super) fn after_derive(editor: &mut Editor) -> Task<Message> {
    if !editor.workspace.title.tools_panel_open {
        return Task::none();
    }
    match editor.palette.revealed.as_mut() {
        Some(revealed) if !revealed.scrolled => {
            revealed.scrolled = true;
            view::tools_panel::scroll_to_revealed()
        }
        _ => Task::none(),
    }
}

/// The mark's timeout, only while a reveal is marked. It is keyed by the reveal's sequence, so a
/// newer reveal starts its own full timeout rather than inheriting the older one's.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    let Some(revealed) = &editor.palette.revealed else {
        return Subscription::none();
    };
    iced::time::every(REVEAL_MARK)
        .with(revealed.sequence)
        .map(|(sequence, _)| Message::Palette(PaletteMessage::Unmark(sequence)))
}
