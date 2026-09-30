//! Evidence steps on the catalog in Select (**lane D**): each gesture sent through the
//! message its control sends, as the model offers it — a source row's press, a menu's choice, a
//! Metadata browser value, the search field's text — and captured once nothing Select asked the
//! owner for is in flight, a library change's view evaluated again.
use super::Settle;
use crate::app::{
    Editor,
    message::{Message, select::SelectMessage, select_catalog::CatalogMessage},
};
use crate::state::select_catalog::{
    ActionChoice, CatalogAction, CatalogMenu, CatalogRow, FacetColumnModel, NamingTarget,
};
use iced::Task;
use luxforge_evidence::{CatalogStep, FacetColumn};

fn act(action: CatalogAction) -> Message {
    Message::Select(SelectMessage::Catalog(CatalogMessage::Act(action)))
}

/// The choice labelled `label` of a menu, when it does something.
fn choice(menu: Option<&[ActionChoice]>, label: &str) -> Result<CatalogAction, String> {
    let menu = menu.ok_or("the menu did not open")?;
    let found = menu
        .iter()
        .find(|choice| choice.label == label)
        .ok_or_else(|| {
            let listed: Vec<&str> = menu.iter().map(|choice| choice.label.as_str()).collect();
            format!("no choice is labelled {label:?}: {listed:?}")
        })?;
    found.action.clone().ok_or_else(|| {
        format!(
            "{label:?} is refused: {}",
            found.reason.clone().unwrap_or_default()
        )
    })
}

impl Editor {
    /// Run one catalog step.
    pub(super) fn catalog_step(&mut self, step: CatalogStep) -> Task<Message> {
        if !self.select_shown() {
            return self.fail_step("Select is not shown");
        }
        self.select.evidence_after = None;
        let pressed_source = matches!(step, CatalogStep::Source(_));
        let result = match step {
            CatalogStep::Source(name) => self
                .catalog_row(&name)
                .and_then(|row| row.press.ok_or_else(|| format!("{name:?} does nothing")))
                .map(|press| vec![act(press)]),
            CatalogStep::Search(text) => Ok(vec![act(CatalogAction::Search(text))]),
            CatalogStep::Metadata => Ok(vec![act(CatalogAction::Metadata)]),
            CatalogStep::Facet { column, value } => self.facet_press(column, &value),
            CatalogStep::Edited(item) => {
                let _ = self.update(act(CatalogAction::Menu(Some(CatalogMenu::Edited))));
                let menu = self
                    .workspace
                    .select
                    .catalog
                    .filter
                    .as_ref()
                    .and_then(|bar| bar.edited.menu.clone());
                choice(menu.as_deref(), &item).map(|action| vec![act(action)])
            }
            CatalogStep::Rename { folder, name } => self
                .folder_menu(&folder, None, "Rename\u{2026}")
                .map(|rename| {
                    vec![
                        act(rename),
                        act(CatalogAction::NameText(name)),
                        act(CatalogAction::Submit),
                    ]
                }),
            CatalogStep::Nest { folder, into } => self
                .folder_menu(&folder, Some("Move to\u{2026}"), &into)
                .map(|nest| vec![act(nest)]),
            CatalogStep::Merge { folder, into } => self
                .folder_menu(&folder, Some("Merge into\u{2026}"), &into)
                .map(|merge| vec![act(merge)]),
            CatalogStep::NewFolder(name) => {
                let _ = self.update(act(CatalogAction::Menu(Some(CatalogMenu::Add))));
                let menu = self.workspace.select.catalog.sources.add_menu.clone();
                choice(menu.as_deref(), "New folder").map(|new| {
                    vec![
                        act(new),
                        act(CatalogAction::NameText(name)),
                        act(CatalogAction::Submit),
                    ]
                })
            }
            CatalogStep::MoveTo(folder) => {
                let _ = self.update(act(CatalogAction::Menu(Some(CatalogMenu::MovePhotos))));
                let menu = self
                    .workspace
                    .select
                    .catalog
                    .info
                    .as_ref()
                    .and_then(|info| info.move_menu.clone());
                choice(menu.as_deref(), &folder).map(|action| vec![act(action)])
            }
            CatalogStep::AddTo(collection) => {
                let _ = self.update(act(CatalogAction::Menu(Some(CatalogMenu::AddTo))));
                let menu = self
                    .workspace
                    .select
                    .catalog
                    .info
                    .as_ref()
                    .and_then(|info| info.add_menu.clone());
                choice(menu.as_deref(), &collection).map(|action| vec![act(action)])
            }
            CatalogStep::SaveSmart(name) => {
                match self
                    .workspace
                    .select
                    .catalog
                    .filter
                    .as_ref()
                    .map(|bar| bar.save_refused.clone())
                {
                    Some(None) => Ok(vec![
                        act(CatalogAction::Name(NamingTarget::SmartCollection)),
                        act(CatalogAction::NameText(name)),
                        act(CatalogAction::Submit),
                    ]),
                    Some(Some(reason)) => {
                        Err(format!("Save as smart collection… is refused: {reason}"))
                    }
                    None => Err("the view is not over the catalog".to_owned()),
                }
            }
        };
        let messages = match result {
            Ok(messages) => messages,
            Err(reason) => {
                let _ = self.update(act(CatalogAction::Menu(None)));
                return self.fail_step(reason);
            }
        };
        let mut tasks: Vec<Task<Message>> = messages
            .into_iter()
            .map(|message| self.update(message))
            .collect();
        // The Catalog section is at the foot of the sources panel: scrolled to, as a person
        // scrolls to the row they press, so the frame shows it.
        if pressed_source {
            tasks.push(iced::widget::operation::snap_to_end(
                iced::widget::Id::from(crate::view::select::SOURCES_SCROLL),
            ));
        }
        // A gesture that asked the owner for nothing (the Metadata browser opened over counts it
        // holds) is captured at once; any other once what it asked for has answered.
        if self.select_quiet() {
            self.capture_next_frame();
        } else {
            self.await_step(Settle::Select);
        }
        Task::batch(tasks)
    }

    /// The catalog folder, year or collection row showing `name`, as the sources panel lists it.
    fn catalog_row(&self, name: &str) -> Result<CatalogRow, String> {
        let sources = &self.workspace.select.catalog.sources;
        sources
            .folders
            .iter()
            .chain(&sources.collections)
            .find(|row| row.name == name)
            .cloned()
            .ok_or_else(|| format!("no catalog row shows {name:?}"))
    }

    /// Open `folder`'s menu and choose `label`, or, through `picker`, the folder listed as `label`.
    fn folder_menu(
        &mut self,
        folder: &str,
        picker: Option<&str>,
        label: &str,
    ) -> Result<CatalogAction, String> {
        let row = self.catalog_row(folder)?;
        let open = row
            .context
            .ok_or_else(|| format!("{folder:?} has no menu"))?;
        let _ = self.update(act(open));
        let menu = self.catalog_row(folder)?.menu;
        let Some(picker) = picker else {
            return choice(menu.as_deref(), label);
        };
        let opened = choice(menu.as_deref(), picker)?;
        let _ = self.update(act(opened));
        choice(self.catalog_row(folder)?.menu.as_deref(), label)
    }

    /// The Metadata browser's value labelled `value` in `column`, pressed as its row publishes it.
    fn facet_press(&self, column: FacetColumn, value: &str) -> Result<Vec<Message>, String> {
        let title = match column {
            FacetColumn::Date => "Date",
            FacetColumn::Place => "Place",
            FacetColumn::Camera => "Camera",
            FacetColumn::Lens => "Lens",
        };
        let columns = self
            .workspace
            .select
            .catalog
            .metadata
            .as_deref()
            .ok_or("the Metadata browser is not open")?;
        let column: &FacetColumnModel = columns
            .iter()
            .find(|column| column.title == title)
            .ok_or_else(|| format!("no {title} column"))?;
        let row = column
            .rows
            .iter()
            .find(|row| row.label == value)
            .ok_or_else(|| {
                let listed: Vec<&str> = column.rows.iter().map(|row| row.label.as_str()).collect();
                format!("the {title} column lists no {value:?}: {listed:?}")
            })?;
        let change = row
            .change
            .clone()
            .ok_or_else(|| format!("{value:?} names no condition"))?;
        Ok(vec![act(CatalogAction::Change(change))])
    }
}
