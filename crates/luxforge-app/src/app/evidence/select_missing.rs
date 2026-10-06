//! Evidence steps on Missing originals: each gesture sent through the
//! message its control sends, the native folder and file dialogs bypassed with the step's path,
//! and captured once nothing Missing originals asked the owner for is in flight. A `find` step with
//! `stop` presses Stop search as soon as its search has started, and is captured once the job has
//! ended.
use super::Settle;
use crate::app::{
    Editor,
    message::{Message, select::SelectMessage, select_missing::MissingMessage},
};
use crate::state::select_missing::{LocateFrom, MissingFilter, RowAction, SearchStatus};
use iced::Task;
use luxforge_core::catalog_types::FindResult;
use luxforge_evidence::{MissingFilterStep, MissingStep};
use serde_json::json;
use std::path::PathBuf;

fn missing(message: MissingMessage) -> Message {
    Message::Select(SelectMessage::Missing(message))
}

impl Editor {
    /// Run one Missing originals step.
    pub(super) fn missing_step(&mut self, step: MissingStep) -> Task<Message> {
        if !self.missing_shown() {
            return self.fail_step("Missing originals is not shown");
        }
        match step {
            MissingStep::Find {
                group,
                folder,
                stop,
            } => {
                let Some(model) = self.workspace.select.missing.groups.iter().find(|model| {
                    model
                        .folder
                        .file_name()
                        .is_some_and(|name| name == group.as_str())
                }) else {
                    return self.fail_step(format!("no group is developed from a folder {group}"));
                };
                match &model.find {
                    Some(find) if find.reason.is_none() => {}
                    Some(find) => {
                        let reason = find.reason.clone().unwrap_or_default();
                        return self.fail_step(format!("Find in a folder… is refused: {reason}"));
                    }
                    None => return self.fail_step("the group offers no Find in a folder…"),
                }
                let group = model.folder.clone();
                let task = self.update(missing(MissingMessage::FindIn {
                    group,
                    root: Some(PathBuf::from(folder)),
                }));
                if self.select.state.missing.live_search().is_none() {
                    let reason = self.status.text.clone();
                    return self.fail_step(format!("the search did not start: {reason}"));
                }
                self.await_step(if stop {
                    Settle::MissingStop
                } else {
                    Settle::Select
                });
                task
            }
            MissingStep::Filter(filter) => {
                let filter = match filter {
                    MissingFilterStep::All => MissingFilter::All,
                    MissingFilterStep::Found => MissingFilter::Found,
                    MissingFilterStep::NeedsYou => MissingFilter::NeedsYou,
                    MissingFilterStep::NotFound => MissingFilter::NotFound,
                };
                let task = self.update(missing(MissingMessage::Filter(filter)));
                self.await_missing(task)
            }
            MissingStep::Row(file) => {
                let Some(asset) = self.missing_row(&file) else {
                    return self.fail_step(format!("no row shows {file}"));
                };
                let task = self.update(missing(MissingMessage::Row(asset)));
                self.await_missing(task)
            }
            MissingStep::Choose { file, index } => {
                let Some(asset) = self.missing_row(&file) else {
                    return self.fail_step(format!("no row shows {file}"));
                };
                let _ = self.update(missing(MissingMessage::Menu(Some(asset.clone()))));
                let choice = self
                    .workspace
                    .select
                    .missing
                    .groups
                    .iter()
                    .flat_map(|group| &group.rows)
                    .find(|row| row.asset_id == asset)
                    .and_then(|row| match &row.action {
                        Some(RowAction::Choose {
                            open: true,
                            choices,
                        }) => choices.get(index).map(|(_, path, _)| path.clone()),
                        _ => None,
                    });
                let Some(path) = choice else {
                    return self.fail_step(format!("{file}'s Choose… menu has no file {index}"));
                };
                let task = self.update(missing(MissingMessage::Choose { asset, path }));
                self.await_missing(task)
            }
            MissingStep::Relink => {
                let pairs = self
                    .workspace
                    .select
                    .missing
                    .bar
                    .as_ref()
                    .map_or(0, |bar| bar.pairs);
                if pairs == 0 {
                    return self.fail_step("Relink has nothing verified to relink");
                }
                self.note_step(json!({ "relink_pairs": pairs }));
                let task = self.update(missing(MissingMessage::Relink));
                self.await_missing(task)
            }
            MissingStep::Locate { file, path } => {
                let Some(asset) = self.missing_row(&file) else {
                    return self.fail_step(format!("no row shows {file}"));
                };
                let offered = self
                    .workspace
                    .select
                    .missing
                    .groups
                    .iter()
                    .flat_map(|group| &group.rows)
                    .any(|row| {
                        row.asset_id == asset
                            && matches!(row.action, Some(RowAction::Locate { enabled: true }))
                    });
                if !offered {
                    return self.fail_step(format!("{file}'s row offers no Locate…"));
                }
                let task = self.update(missing(MissingMessage::LocatePicked {
                    asset,
                    file_name: file,
                    from: LocateFrom::Missing,
                    path: Some(PathBuf::from(path)),
                }));
                self.await_missing(task)
            }
        }
    }

    /// The photograph whose row shows `file`, whatever the filter.
    fn missing_row(&self, file: &str) -> Option<luxforge_core::AssetId> {
        self.select
            .state
            .missing
            .searches
            .values()
            .flat_map(|search| &search.rows)
            .find(|row| row.file_name == file && !matches!(row.result, FindResult::Checking))
            .map(|row| row.asset_id.clone())
    }

    /// Capture once Missing originals is quiet: now, when the gesture asked the owner for nothing,
    /// or once what it asked for has answered.
    fn await_missing(&mut self, task: Task<Message>) -> Task<Message> {
        if self.select_quiet() {
            self.capture_next_frame();
        } else {
            self.await_step(Settle::Select);
        }
        task
    }

    /// After every message of an evidence run: press Stop search once the search a `find` step
    /// with `stop` started is running, then wait for its job to end.
    pub(crate) fn missing_evidence_after(&mut self) -> Task<Message> {
        if self
            .evidence
            .as_ref()
            .is_none_or(|evidence| evidence.awaiting != Some(Settle::MissingStop))
        {
            return Task::none();
        }
        match self
            .select
            .state
            .missing
            .live_search()
            .map(|(_, search)| search.status.clone())
        {
            Some(SearchStatus::Starting) => Task::none(),
            Some(SearchStatus::Running { job }) => {
                self.note_step(json!({ "stopped_job": job }));
                self.await_step(Settle::Select);
                self.stop_search()
            }
            _ => self.fail_step("the search ended before Stop search could be pressed"),
        }
    }
}
