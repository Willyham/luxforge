//! Missing originals in the Select workspace ([catalog
//! design](../../../../docs/design/catalog.md#missing-originals)), and Locate original… in
//! Develop's Original not found notice. Its model is `state/select_missing.rs` and its region
//! `view/select_missing.rs`.
//!
//! The desktop holds no catalog logic: every gesture sends the request an API client sends, and
//! every row shows what the owner answered.
//!
//! - **The list.** While Select's source is Missing originals, `source.missing` (with `volume.list`
//!   for the volumes' labels) is read once for each evaluation of that source by the Select shell —
//!   choosing it, or the shell evaluating it again after another client's change reached the
//!   desktop — and again after this desktop's own Relink or Locate, whose answers wake nothing.
//! - **Searches.** Find in a folder… asks the native folder dialog and sends `source.find
//!   {search_root, source_folder}`, a `source-find` job on the library lane, one search at a time.
//!   The running search and the running Locate are read with `job.read` when each is named, and
//!   then whenever long-running work reads the activity board, which its watch wakes it to do as
//!   catalog jobs begin, report progress or end ([`Editor::missing_followed`]) — as the Select
//!   shell follows `index.refresh` — so no timer asks after them; while a search runs its `result`
//!   is the report so far, each row `checking` until it is settled. Stop search is `job.cancel`: a
//!   cancelled search remembers nothing, so its group goes back to its header and nothing has
//!   changed.
//! - **Relink N** sends `source.relink {pairs}` with exactly the pairs finished searches verified
//!   and the files chosen among several identical ones ([`model::relink_pairs`]), one library change
//!   the core's `library.undo` reverts.
//! - **Locate…** on a row (or the Info panel's Locate a different file…, or Develop's Locate
//!   original…) asks the native file dialog and sends `source.locate {asset_id, path}`, a
//!   `source-locate` job. When Develop asked, a located photograph is opened again from its new file
//!   through the Open path the desktop already has (a `pick.develop` of that file, which links it
//!   to the photograph whose original it now is).
use crate::app::{
    Before, Editor,
    message::{Message, select::SelectMessage, select_missing::MissingMessage},
    select::job_now,
    tasks::{call, owner_task, request},
};
use crate::state::select_missing::{
    self as model, LocateFrom, Locating, PhotoFacts, Search, SearchStatus,
};
use iced::Task;
use luxforge_core::{
    AssetId, ClientId, OwnerHandle,
    catalog_types::{FindReport, MissingOriginals, VolumeState, Volumes},
    jobs::JOB_CANCEL,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn missing(message: MissingMessage) -> Message {
    Message::Select(SelectMessage::Missing(message))
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// -- The owner calls, each the body of one owner task, so a test runs exactly what a task would
// against a real owner. --

/// `source.missing`, then `volume.list` for the labels of the volumes the groups are on.
pub(crate) fn missing_now(
    owner: &OwnerHandle,
    client: ClientId,
) -> Result<Box<(MissingOriginals, Vec<VolumeState>)>, String> {
    let (list, _) = call(owner, client, "source.missing", json!({}))?;
    let (volumes, _) = call(owner, client, "volume.list", json!({}))?;
    Ok(Box::new((parse(list)?, parse::<Volumes>(volumes)?.volumes)))
}

/// A method that starts a job, answering the job.
fn started(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<String, String> {
    let (started, _) = call(owner, client, method, params)?;
    started["job_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{method} answered no job: {started}"))
}

/// `source.find`, answering its job.
pub(crate) fn find_now(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<String, String> {
    started(owner, client, "source.find", params)
}

/// `source.locate`, answering its job.
pub(crate) fn locate_now(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<String, String> {
    started(owner, client, "source.locate", params)
}

/// `job.cancel` of a search, answered with the job as `job.read` answers it afterwards.
pub(crate) fn cancel_now(
    owner: &OwnerHandle,
    client: ClientId,
    job: &str,
) -> Result<Value, String> {
    call(owner, client, JOB_CANCEL, model::cancel_params(job)).map(|(record, _)| record)
}

/// `source.relink`, answered with the library change.
pub(crate) fn relink_now(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<Value, String> {
    call(owner, client, "source.relink", params).map(|(answer, _)| answer)
}

/// The selected photograph's facts: `asset.state` for where its original was, and `history.list`'s
/// newest row for how many edits it has.
pub(crate) fn facts_now(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &AssetId,
) -> Result<PhotoFacts, String> {
    let (state, _) = call(owner, client, "asset.state", json!({ "asset_id": asset }))?;
    let was = state["asset"]["locator"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| format!("asset.state named no original: {state}"))?;
    let (page, _) = call(
        owner,
        client,
        "history.list",
        model::newest_entry_params(asset),
    )?;
    let entries = page["entries"][0]["sequence"].as_u64().unwrap_or(0);
    Ok(PhotoFacts { was, entries })
}

impl Editor {
    /// Select shows Missing originals.
    pub(crate) fn missing_shown(&self) -> bool {
        model::shown(&self.select.state)
    }

    /// Nothing Missing originals asked the owner for is in flight or still wanted.
    pub(crate) fn missing_quiet(&self) -> bool {
        self.select
            .state
            .missing
            .quiet(self.missing_shown(), self.select.serial)
    }

    /// One Missing originals message.
    pub(crate) fn missing_update(&mut self, message: MissingMessage) -> Task<Message> {
        match message {
            MissingMessage::Listed { serial, result } => {
                let state = &mut self.select.state.missing;
                state.reading = false;
                match result {
                    Ok(answer) => {
                        let (list, volumes) = *answer;
                        state.listed(list, volumes);
                    }
                    Err(error) => {
                        if state.read_for == Some(serial) {
                            self.status.text = format!("Missing originals unavailable: {error}");
                        }
                        self.select.state.missing.error = Some(error);
                    }
                }
            }
            MissingMessage::Find(group) => {
                if self.view_state.picker_open || self.evidence.is_some() {
                    return Task::none();
                }
                if self.select.state.missing.live_search().is_some() {
                    self.status.text = "One search at a time: stop this one first".into();
                    return Task::none();
                }
                self.view_state.picker_open = true;
                let title = format!("Find the files from {}", file_name(&group));
                return Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_title(title)
                            .pick_folder()
                            .await
                            .map(|folder| folder.path().to_path_buf())
                    },
                    move |root| missing(MissingMessage::FindIn { group, root }),
                );
            }
            MissingMessage::FindIn { group, root } => {
                self.view_state.picker_open = false;
                if let Some(root) = root {
                    return self.start_find(group, root);
                }
            }
            MissingMessage::Finding { group, result } => {
                let Some(search) = self.select.state.missing.searches.get_mut(&group) else {
                    return Task::none();
                };
                if search.status != SearchStatus::Starting {
                    return Task::none();
                }
                match result {
                    // Read once as soon as it is named, which a job that ended before any wake
                    // reached the desktop needs.
                    Ok(job) => {
                        search.status = SearchStatus::Running { job };
                        return self.poll_jobs();
                    }
                    Err(error) => {
                        self.status.text = format!("Could not search: {error}");
                        search.status = SearchStatus::Failed(error);
                    }
                }
            }
            MissingMessage::Stop => return self.stop_search(),
            MissingMessage::Stopped(result) => {
                let job = self
                    .select
                    .state
                    .missing
                    .live_search()
                    .and_then(|(_, search)| search.status.job().map(str::to_owned));
                match (job, result) {
                    // The cancel answers the job as `job.read` does afterwards.
                    (Some(job), Ok(record)) => self.search_read(&job, Ok(record)),
                    (_, Err(error)) => {
                        self.status.text = format!("Could not stop the search: {error}");
                    }
                    (None, Ok(_)) => {}
                }
            }
            MissingMessage::Polled { search, locate } => {
                let state = &mut self.select.state.missing;
                state.polling = false;
                let again = std::mem::take(&mut state.poll_again);
                if let Some((job, result)) = search {
                    self.search_read(&job, result);
                }
                let located = match locate {
                    Some((job, result)) => self.locate_read(&job, result),
                    None => Task::none(),
                };
                // The board changed while that read was out: read what still runs once more.
                let polled = if again {
                    self.poll_jobs()
                } else {
                    Task::none()
                };
                return Task::batch([located, polled]);
            }
            MissingMessage::Filter(filter) => {
                let state = &mut self.select.state.missing;
                state.filter = filter;
                state.menu = None;
            }
            MissingMessage::Row(asset) => {
                let state = &mut self.select.state.missing;
                state.menu = None;
                state.selected = Some(asset);
            }
            MissingMessage::Facts { asset, result } => {
                let state = &mut self.select.state.missing;
                if state.facts_reading.as_ref() == Some(&asset) {
                    state.facts_reading = None;
                }
                if state.selected.as_ref() == Some(&asset) {
                    state.facts = Some((asset, result));
                }
            }
            MissingMessage::Menu(menu) => self.select.state.missing.menu = menu,
            MissingMessage::Choose { asset, path } => self.choose(asset, path),
            MissingMessage::Relink => return self.relink(),
            MissingMessage::Relinked { pairs, result } => {
                let state = &mut self.select.state.missing;
                state.relinking = false;
                match result {
                    Ok(answer) => {
                        if let Some(change) = answer["change"].as_u64() {
                            self.select.own_change = Some(change);
                        }
                        let state = &mut self.select.state.missing;
                        state.resolved(&pairs);
                        // The desktop's own change wakes nothing: the list is read again here.
                        state.read_for = None;
                        let count = pairs.len();
                        self.status.text = match answer["change"].as_u64() {
                            Some(change) => format!(
                                "Relinked {} \u{b7} library change {change}",
                                originals(count)
                            ),
                            None => format!("Relinked {}", originals(count)),
                        };
                    }
                    Err(error) => {
                        self.status.text = format!("Nothing was relinked: {error}");
                    }
                }
            }
            MissingMessage::Locate(asset) => {
                let Some((_, _, row)) = self.select.state.missing.row(&asset) else {
                    return Task::none();
                };
                let file_name = row.file_name.clone();
                return self.pick_original(asset, file_name, LocateFrom::Missing);
            }
            MissingMessage::LocateOriginal => {
                let Some(state) = &self.document.state else {
                    return Task::none();
                };
                let (asset, file_name) = (state.asset.id.clone(), file_name(&state.asset.locator));
                return self.pick_original(asset, file_name, LocateFrom::Develop);
            }
            MissingMessage::LocatePicked {
                asset,
                file_name,
                from,
                path,
            } => {
                self.view_state.picker_open = false;
                if let Some(path) = path {
                    return self.start_locate(asset, file_name, from, path);
                }
            }
            MissingMessage::Locating(result) => {
                let state = &mut self.select.state.missing;
                let Some(locating) = &mut state.locating else {
                    return Task::none();
                };
                match result {
                    Ok(job) => {
                        locating.job = Some(job);
                        return self.poll_jobs();
                    }
                    Err(error) => {
                        self.status.text =
                            format!("Could not locate {}: {error}", locating.file_name);
                        state.locating = None;
                    }
                }
            }
        }
        Task::none()
    }

    /// Search `root` for every missing photograph developed from `group`: `source.find`, one
    /// search at a time. A new search of a group replaces its earlier results.
    pub(crate) fn start_find(&mut self, group: PathBuf, root: PathBuf) -> Task<Message> {
        let state = &mut self.select.state.missing;
        if state.live_search().is_some() {
            self.status.text = "One search at a time: stop this one first".into();
            return Task::none();
        }
        let count = state
            .list
            .as_ref()
            .and_then(|list| {
                list.groups
                    .iter()
                    .find(|entry| entry.source_folder == group)
            })
            .map_or(0, |entry| entry.count);
        state.menu = None;
        state
            .searches
            .insert(group.clone(), Search::new(root.clone()));
        let home = self.select.state.home.as_deref();
        self.status.text = format!(
            "Searching {} for the {} from {}, by name and size, then fingerprint",
            crate::state::select::shown_path(&root, home),
            if count == 1 {
                "1 file".to_owned()
            } else {
                format!("{} files", crate::state::select::thousands(count))
            },
            file_name(&group),
        );
        let params = model::find_params(&root, &group);
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || find_now(&owner, client, params),
            move |result| missing(MissingMessage::Finding { group, result }),
        )
    }

    /// Stop search: `job.cancel` of the running search. Its end is read as any other.
    pub(crate) fn stop_search(&mut self) -> Task<Message> {
        let Some(search) = self
            .select
            .state
            .missing
            .searches
            .values_mut()
            .find(|search| matches!(search.status, SearchStatus::Running { .. }))
        else {
            return Task::none();
        };
        let SearchStatus::Running { job } = search.status.clone() else {
            return Task::none();
        };
        search.status = SearchStatus::Stopping { job: job.clone() };
        self.status.text = "Stopping the search\u{2026}".into();
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || cancel_now(&owner, client, &job),
            |result| missing(MissingMessage::Stopped(result)),
        )
    }

    /// Long-running work has read the activity board, which its watch wakes it to read whenever a
    /// catalog job begins, reports progress or ends: the running search and Locate are read then,
    /// so a search's rows fill in as it reports each settled one and its end is heard, and nothing
    /// polls them.
    pub(crate) fn missing_followed(&mut self) -> Task<Message> {
        self.poll_jobs()
    }

    /// Read the running search and Locate, one read at a time; asked again while one is out, they
    /// are read once more when it answers.
    fn poll_jobs(&mut self) -> Task<Message> {
        let state = &mut self.select.state.missing;
        let (search, locate) = state.running_jobs();
        if search.is_none() && locate.is_none() {
            return Task::none();
        }
        if state.polling {
            state.poll_again = true;
            return Task::none();
        }
        state.polling = true;
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let read = |job: String| {
                    let record = job_now(&owner, client, &job);
                    (job, record)
                };
                (search.map(read), locate.map(read))
            },
            |(search, locate)| missing(MissingMessage::Polled { search, locate }),
        )
    }

    /// The running search's job as the owner answered it: still running, its report so far; ended,
    /// its final report; stopped, nothing changed and its group goes back to its header; failed,
    /// why.
    fn search_read(&mut self, job: &str, result: Result<Value, String>) {
        let home = self.select.state.home.clone();
        let state = &mut self.select.state.missing;
        let Some((group, search)) = state
            .searches
            .iter_mut()
            .find(|(_, search)| search.status.job() == Some(job))
        else {
            return;
        };
        let group = group.clone();
        let root = crate::state::select::shown_path(&search.root, home.as_deref());
        let record = match result {
            Ok(record) => record,
            Err(error) => {
                self.status.text = format!("Could not read the search of {root}: {error}");
                search.status = SearchStatus::Failed(error);
                return;
            }
        };
        let report = || parse::<FindReport>(record["result"].clone());
        match record["status"].as_str() {
            Some("queued" | "running") => {
                if let Ok(report) = report() {
                    search.rows = report.rows;
                }
                search.progress = record["progress"]["message"].as_str().map(str::to_owned);
            }
            Some("ready") => match report() {
                Ok(report) => {
                    search.rows = report.rows;
                    search.progress = None;
                    search.status = SearchStatus::Ended;
                    let found = search
                        .rows
                        .iter()
                        .filter(|row| {
                            matches!(
                                row.result,
                                luxforge_core::catalog_types::FindResult::Found { .. }
                            )
                        })
                        .count();
                    self.status.text = format!(
                        "Searched {root}: {found} of {} from {} found with the same bytes",
                        search.rows.len(),
                        file_name(&group)
                    );
                }
                Err(error) => {
                    self.status.text = format!("The search of {root} answered no report: {error}");
                    search.status = SearchStatus::Failed(error);
                }
            },
            Some("cancelled") => {
                state.searches.remove(&group);
                if state
                    .menu
                    .as_ref()
                    .is_some_and(|menu| state.row(menu).is_none())
                {
                    state.menu = None;
                }
                if state
                    .selected
                    .as_ref()
                    .is_some_and(|selected| state.row(selected).is_none())
                {
                    state.selected = None;
                }
                self.status.text = format!("Stopped searching {root}: nothing changed");
            }
            other => {
                let reason = record["error"]["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("the search ended {}", other.unwrap_or("unknown")));
                self.status.text = format!("The search of {root} failed: {reason}");
                search.rows.clear();
                search.chosen.clear();
                search.progress = None;
                search.status = SearchStatus::Failed(reason);
            }
        }
    }

    /// Choose one of a photograph's identical files: Relink points it there.
    fn choose(&mut self, asset: AssetId, path: PathBuf) {
        let state = &mut self.select.state.missing;
        state.menu = None;
        let Some(search) = state.searches.values_mut().find(|search| {
            search.rows.iter().any(|row| {
                row.asset_id == asset
                    && matches!(&row.result, luxforge_core::catalog_types::FindResult::SeveralIdentical { paths } if paths.contains(&path))
            })
        }) else {
            return;
        };
        search.chosen.insert(asset, path.clone());
        let home = self.select.state.home.as_deref();
        self.status.text = format!(
            "Chose {} \u{b7} nothing changes until you relink",
            crate::state::select::shown_path(&path, home)
        );
    }

    /// Relink N: `source.relink` of every verified pair, one library change.
    pub(crate) fn relink(&mut self) -> Task<Message> {
        let state = &mut self.select.state.missing;
        if state.relinking {
            return Task::none();
        }
        let pairs = model::relink_pairs(state);
        if pairs.is_empty() {
            return Task::none();
        }
        state.relinking = true;
        state.menu = None;
        self.status.text = format!("Relinking {}\u{2026}", originals(pairs.len()));
        let params = model::relink_params(&pairs, &request());
        let assets: Vec<AssetId> = pairs.into_iter().map(|pair| pair.asset_id).collect();
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || relink_now(&owner, client, params),
            move |result| {
                missing(MissingMessage::Relinked {
                    pairs: assets,
                    result,
                })
            },
        )
    }

    /// Ask the native file dialog for the file a photograph's original is now.
    fn pick_original(
        &mut self,
        asset: AssetId,
        file_name: String,
        from: LocateFrom,
    ) -> Task<Message> {
        if self.view_state.picker_open || self.evidence.is_some() {
            return Task::none();
        }
        if let Some(locating) = &self.select.state.missing.locating {
            self.status.text = format!("Already locating {}", locating.file_name);
            return Task::none();
        }
        self.view_state.picker_open = true;
        let title = format!("Locate {file_name}");
        Task::perform(
            async move {
                rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .pick_file()
                    .await
                    .map(|file| file.path().to_path_buf())
            },
            move |path| {
                missing(MissingMessage::LocatePicked {
                    asset,
                    file_name,
                    from,
                    path,
                })
            },
        )
    }

    /// Point one photograph at `path`: `source.locate`, whose job verifies the file's bytes
    /// against the photograph's fingerprint before anything changes.
    pub(crate) fn start_locate(
        &mut self,
        asset: AssetId,
        file_name: String,
        from: LocateFrom,
        path: PathBuf,
    ) -> Task<Message> {
        let state = &mut self.select.state.missing;
        if let Some(locating) = &state.locating {
            self.status.text = format!("Already locating {}", locating.file_name);
            return Task::none();
        }
        state.menu = None;
        let home = self.select.state.home.as_deref();
        self.status.text = format!(
            "Checking that {} holds {file_name}'s bytes\u{2026}",
            crate::state::select::shown_path(&path, home)
        );
        let params = model::locate_params(&asset, &path, &request());
        self.select.state.missing.locating = Some(Locating {
            asset_id: asset,
            file_name,
            path,
            from,
            job: None,
        });
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || locate_now(&owner, client, params),
            |result| missing(MissingMessage::Locating(result)),
        )
    }

    /// The running Locate's job as the owner answered it. Located, the photograph leaves the list,
    /// which is read again, and one Develop asked for is opened again from its new file; refused or
    /// stopped, nothing changed and the status bar says why.
    fn locate_read(&mut self, job: &str, result: Result<Value, String>) -> Task<Message> {
        let state = &mut self.select.state.missing;
        let Some(locating) = state
            .locating
            .as_ref()
            .filter(|locating| locating.job.as_deref() == Some(job))
            .cloned()
        else {
            return Task::none();
        };
        let record = match result {
            Ok(record) => record,
            Err(error) => {
                state.locating = None;
                self.status.text = format!("Could not locate {}: {error}", locating.file_name);
                return Task::none();
            }
        };
        match record["status"].as_str() {
            Some("queued" | "running") => {
                if let Some(progress) = record["progress"]["message"].as_str() {
                    self.status.text = format!("Locating {} \u{b7} {progress}", locating.file_name);
                }
                Task::none()
            }
            Some("ready") => {
                state.locating = None;
                state.resolved(std::slice::from_ref(&locating.asset_id));
                if let Some(change) = record["result"]["change"].as_u64() {
                    self.select.own_change = Some(change);
                }
                let state = &mut self.select.state.missing;
                state.read_for = None;
                let home = self.select.state.home.as_deref();
                self.status.text = format!(
                    "Located {} at {}",
                    locating.file_name,
                    crate::state::select::shown_path(&locating.path, home)
                );
                match locating.from {
                    // Opened again from its new file, through the Open path.
                    LocateFrom::Develop
                        if self
                            .document
                            .state
                            .as_ref()
                            .is_some_and(|state| state.asset.id == locating.asset_id) =>
                    {
                        self.open(locating.path)
                    }
                    _ => Task::none(),
                }
            }
            other => {
                state.locating = None;
                let reason = record["error"]["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("the Locate ended {}", other.unwrap_or("unknown")));
                self.status.text = format!(
                    "Could not locate {}: {reason} \u{b7} nothing changed",
                    locating.file_name
                );
                Task::none()
            }
        }
    }

    /// Missing originals for correlated evidence: what it read and shows, each row's result as the
    /// owner answered it and what Relink would send. Paths are recorded under their search's root,
    /// and groups by their folder's name, so no absolute path is recorded.
    pub(crate) fn missing_summary(&self) -> Value {
        let state = &self.select.state.missing;
        let model = &self.workspace.select.missing;
        let name = |path: &Path| file_name(path);
        let under = |root: &Path, path: &Path| {
            path.strip_prefix(root)
                .map(|rest| rest.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| format!("(outside) {}", name(path)))
        };
        let groups: Vec<Value> = state
            .list
            .iter()
            .flat_map(|list| &list.groups)
            .map(|group| {
                let search = state.searches.get(&group.source_folder).map(|search| {
                    let rows: Vec<Value> = search
                        .rows
                        .iter()
                        .map(|row| {
                            let mut result = serde_json::to_value(&row.result).unwrap_or_default();
                            if let Some(object) = result.as_object_mut() {
                                if let Some(path) = object.get("path").and_then(Value::as_str) {
                                    let path = under(&search.root, Path::new(path));
                                    object.insert("path".into(), json!(path));
                                }
                                if let Some(paths) = object.get("paths").and_then(Value::as_array)
                                {
                                    let paths: Vec<String> = paths
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .map(|path| under(&search.root, Path::new(path)))
                                        .collect();
                                    object.insert("paths".into(), json!(paths));
                                }
                            }
                            json!({
                                "asset_id": row.asset_id,
                                "file_name": row.file_name,
                                "result": result,
                                "chosen": search.chosen.get(&row.asset_id).map(|path| under(&search.root, path)),
                            })
                        })
                        .collect();
                    json!({
                        "root": name(&search.root),
                        "status": match &search.status {
                            SearchStatus::Starting => "starting",
                            SearchStatus::Running { .. } => "running",
                            SearchStatus::Stopping { .. } => "stopping",
                            SearchStatus::Ended => "ended",
                            SearchStatus::Failed(_) => "failed",
                        },
                        "rows": rows,
                    })
                });
                json!({
                    "folder": name(&group.source_folder),
                    "count": group.count,
                    "reason": group.reason,
                    "catalog_folders": group.catalog_folders.iter().map(|folder| &folder.name).collect::<Vec<_>>(),
                    "search": search,
                })
            })
            .collect();
        let drawn: Vec<Value> = model
            .groups
            .iter()
            .map(|group| {
                json!({
                    "folder": name(&group.folder),
                    "detail": group.detail,
                    "status": group.status,
                    "find": group.find.as_ref().map(|find| json!({"label": find.label, "enabled": find.reason.is_none()})),
                    "rows": group.rows.iter().map(|row| json!({
                        "asset_id": row.asset_id,
                        "name": row.name,
                        "text": row.text,
                        "detail": row.detail,
                        "action": row.action.as_ref().map(|action| match action {
                            model::RowAction::Locate { .. } => "locate",
                            model::RowAction::Choose { .. } => "choose",
                        }),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        let pairs: Vec<Value> = model::relink_pairs(state)
            .iter()
            .map(|pair| json!({"asset_id": pair.asset_id, "file": name(&pair.path)}))
            .collect();
        let info = match &model.info {
            model::MissingInfo::Nothing => json!({"kind": "nothing"}),
            model::MissingInfo::One(info) => json!({
                "kind": "one",
                "asset_id": info.asset_id,
                "name": info.name,
                "labels": info.rows.iter().map(|(label, _)| label).collect::<Vec<_>>(),
                "check": info.rows.get(2).map(|(_, value)| value),
                "verified": info.verified,
            }),
        };
        json!({
            "shown": model.shown,
            "quiet": self.missing_quiet(),
            "count": state.list.as_ref().map(|list| list.count),
            "error": state.error,
            "groups": groups,
            "drawn": drawn,
            "filter": model.filters.get(model.filter).map(|(label, _)| label),
            "filters": model.filters,
            "heading": model.heading.as_ref().map(|(heading, _)| heading),
            "note": model.note,
            "bar": model.bar.as_ref().map(|bar| json!({
                "verified": bar.verified,
                "detail": bar.detail,
                "stop": bar.stop,
                "relink": bar.relink,
                "enabled": bar.pairs > 0,
            })),
            "pairs": pairs,
            "selected": state.selected,
            "info": info,
            "menu": state.menu,
            "locating": state.locating.as_ref().map(|locating| &locating.file_name),
            "title": model.title,
            "line": model.line,
        })
    }
}

/// `1 original`, `18 originals`.
fn originals(count: usize) -> String {
    if count == 1 {
        "1 original".to_owned()
    } else {
        format!(
            "{} originals",
            crate::state::select::thousands(count as u32)
        )
    }
}

/// After every message: while Select shows Missing originals, read the list once for each of the
/// shell's evaluations of it (and again after a relink or a Locate), and the selected row's facts;
/// and a waiting evidence step's own gestures.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let mut tasks = Vec::new();
    if editor.missing_shown() {
        let serial = editor.select.serial;
        let state = &mut editor.select.state.missing;
        if !state.reading && state.read_for != Some(serial) {
            state.reading = true;
            state.read_for = Some(serial);
            let (owner, client) = (editor.owner.clone(), editor.client);
            tasks.push(owner_task(
                move || missing_now(&owner, client),
                move |result| missing(MissingMessage::Listed { serial, result }),
            ));
        }
        let state = &mut editor.select.state.missing;
        if let Some(asset) = state.selected.clone()
            && state.facts.as_ref().is_none_or(|(held, _)| held != &asset)
            && state.facts_reading.as_ref() != Some(&asset)
        {
            state.facts_reading = Some(asset.clone());
            let (owner, client) = (editor.owner.clone(), editor.client);
            tasks.push(owner_task(
                move || {
                    let result = facts_now(&owner, client, &asset);
                    (asset, result)
                },
                |(asset, result)| missing(MissingMessage::Facts { asset, result }),
            ));
        }
    }
    if editor.evidence.is_some() {
        tasks.push(editor.missing_evidence_after());
    }
    Task::batch(tasks)
}
