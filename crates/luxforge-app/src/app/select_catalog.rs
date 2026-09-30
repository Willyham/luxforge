//! The catalog in the Select workspace ([catalog design](../../../../docs/design/catalog.md#the-catalog),
//! TASK-022): the Catalog sources' folders and collections, the filter bar over the catalog and the
//! Metadata browser, Save as smart collection…, and the Info panel's Organize band and batch form.
//! **Lane D (views and desktop)** owns this seam; its model is `state/select_catalog.rs` and its
//! regions `view/select_catalog.rs`.
//!
//! The desktop holds no catalog logic: every gesture sends the request an API client sends, and
//! every row and count shows what the owner answered.
//!
//! - **The folders and collections** are `folder.list` and `collection.list`, read whenever the
//!   Select shell reads the catalog's counts — when Select is shown, after a library change of this
//!   desktop's and after another client's change reached it — one read in flight, one waiting.
//! - **The view's query.** Every chip, column value and search changes its own part of the query
//!   and sends the whole of it to `browse.view` through the shell ([`model::changed`]); the search
//!   is sent as typed, one evaluation in flight and the newest text waiting for it, so typing never
//!   queues work on the owner. The Metadata browser's counts are the shell's own `browse.facets`
//!   over the source and filter, which counts each column without its own condition. With a filter
//!   set, the source is counted once with none for the bar's "9 of 55" (`browse.facets` of Kind,
//!   summed), again only after a library change.
//! - **Organizing.** Folder create, rename, nest, merge and delete, collection create, rename and
//!   delete, Save as smart collection… (exactly the query shown), and the selection's Move to…, Add
//!   to… and remove are each one library change as this desktop's actor, sent synchronously in the
//!   update of the press like every library gesture of the shell's, which follows what it recorded
//!   (the view, the events, the counts, these lists and the change's label). `Cmd+Z` undoes them.
//! - **The batch form** reads the selection's rows the desktop does not hold near the screen
//!   (`browse.rows`, at most [`model::MAX_SELECTION_ROWS`]), so its folder and collection chips
//!   count every selected photograph; a larger selection is organized all the same.
use crate::app::{
    Before, Editor,
    message::{Message, select::SelectMessage, select_catalog::CatalogMessage},
    tasks::{CallError, call, call_detailed, owner_task, request},
};
use crate::coalesce::Coalesce;
use crate::state::select::{LibraryGesture, SelectionModel};
use crate::state::select_catalog::{
    self as model, CatalogAction, CatalogChange, CatalogGesture, Naming, NamingTarget,
    SelectionRows, SourceTotal, YearGroup,
};
use iced::Task;
use luxforge_core::{
    ClientId, OwnerHandle,
    catalog_types::{
        CatalogFolders, CollectionKind, Collections, Facets, LibraryAnswer, MAX_VIEW_ROWS,
        ViewQuery, ViewRow, ViewRows, ViewSource,
    },
};
use serde_json::{Value, json};

fn catalog(message: CatalogMessage) -> Message {
    Message::Select(SelectMessage::Catalog(message))
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// What the catalog seam has in flight, beside what the model reads.
#[derive(Debug, Default)]
pub(crate) struct SelectCatalog {
    /// `folder.list` and `collection.list`: one read in flight, one waiting.
    pub(crate) lists: Coalesce<()>,
    /// The source and library change a total is being counted for.
    pub(crate) totalling: Option<(ViewSource, u64)>,
    /// A total that could not be counted, not asked again for the same source and change.
    pub(crate) total_failed: Option<(ViewSource, u64)>,
    /// The view revision and selection ranges whose rows are being read.
    pub(crate) reading: Option<(u64, Vec<(u32, u32)>)>,
    /// A selection whose rows could not be read, not asked again.
    pub(crate) rows_failed: Option<(u64, Vec<(u32, u32)>)>,
}

// -- The owner calls, each the body of one owner task (or, for a library change, the one
// synchronous call), so a test runs exactly what a task would against a real owner. --

/// `folder.list`, then `collection.list`.
pub(crate) fn lists_now(
    owner: &OwnerHandle,
    client: ClientId,
) -> Result<Box<(CatalogFolders, Collections)>, String> {
    let (folders, _) = call(owner, client, "folder.list", json!({}))?;
    let (collections, _) = call(owner, client, "collection.list", json!({}))?;
    Ok(Box::new((parse(folders)?, parse(collections)?)))
}

/// `browse.facets` of `source` with no filter, summed: how many photographs the source holds.
pub(crate) fn total_now(
    owner: &OwnerHandle,
    client: ClientId,
    source: &ViewSource,
) -> Result<u32, String> {
    let (facets, _) = call(owner, client, "browse.facets", model::total_params(source))?;
    let facets = parse::<Facets>(facets)?;
    Ok(facets
        .counts
        .values()
        .next()
        .map_or(0, |values| values.iter().map(|value| value.count).sum()))
}

/// `browse.rows` for each of `ranges` of the view at `revision`, at most [`MAX_VIEW_ROWS`] a
/// window.
pub(crate) fn selection_rows_now(
    owner: &OwnerHandle,
    client: ClientId,
    revision: u64,
    ranges: &[(u32, u32)],
) -> Result<Vec<ViewRow>, String> {
    let mut rows = Vec::new();
    for &(start, end) in ranges {
        let mut from = start;
        while from < end {
            let count = (end - from).min(MAX_VIEW_ROWS as u32);
            let (answer, _) = call(
                owner,
                client,
                "browse.rows",
                json!({"from": from, "count": count, "revision": revision}),
            )?;
            let answer = parse::<ViewRows>(answer)?;
            if answer.revision != revision || answer.rows.is_empty() {
                return Err("the view changed while its rows were read".to_owned());
            }
            from += answer.rows.len() as u32;
            rows.extend(answer.rows);
        }
    }
    Ok(rows)
}

/// One organizing library change of this desktop's, sent synchronously: the library change it
/// recorded — the answer itself, or a create's `change` — and the whole answer, or its refusal.
pub(crate) fn catalog_call(
    owner: &OwnerHandle,
    client: ClientId,
    gesture: CatalogGesture,
    params: Value,
) -> Result<(LibraryAnswer, Value), CallError> {
    let method = gesture.method();
    let answer = call_detailed(owner, client, method, params)?;
    let change = if gesture.creates() {
        answer["change"].clone()
    } else {
        answer.clone()
    };
    serde_json::from_value::<LibraryAnswer>(change)
        .map(|change| (change, answer))
        .map_err(|error| CallError {
            code: "internal".into(),
            message: format!("{method} answered unexpectedly: {error}"),
            data: None,
            job_id: None,
        })
}

impl Editor {
    /// One catalog message.
    pub(crate) fn catalog_update(&mut self, message: CatalogMessage) -> Task<Message> {
        match message {
            CatalogMessage::Act(action) => return self.catalog_act(action),
            CatalogMessage::Lists(result) => {
                self.select.catalog.lists.answered();
                let state = &mut self.select.state.catalog;
                match result {
                    Ok(answer) => {
                        let (folders, collections) = *answer;
                        state.folders = Some(folders);
                        state.collections = Some(collections);
                        state.lists_error = None;
                    }
                    Err(error) => {
                        self.status.text = format!("Folders and collections unavailable: {error}");
                        state.lists_error = Some(error);
                    }
                }
            }
            CatalogMessage::Totalled {
                source,
                sequence,
                result,
            } => {
                let key = (source, sequence);
                if self.select.catalog.totalling.as_ref() == Some(&key) {
                    self.select.catalog.totalling = None;
                }
                match result {
                    Ok(count) => {
                        let (source, sequence) = key;
                        self.select.state.catalog.total = Some(SourceTotal {
                            source,
                            sequence,
                            count,
                        });
                    }
                    Err(_) => self.select.catalog.total_failed = Some(key),
                }
            }
            CatalogMessage::SelectionRows {
                revision,
                ranges,
                result,
            } => {
                let key = (revision, ranges);
                if self.select.catalog.reading.as_ref() == Some(&key) {
                    self.select.catalog.reading = None;
                }
                let (revision, ranges) = key;
                match result {
                    Ok(rows) if Some(revision) == self.select.state.revision() => {
                        let held = self
                            .select
                            .state
                            .catalog
                            .selection_rows
                            .take()
                            .filter(|held| held.revision == revision);
                        let mut read = held.unwrap_or(SelectionRows {
                            revision,
                            ..SelectionRows::default()
                        });
                        read.ranges = ranges;
                        // What was read for this revision stays while it is selected; a row read
                        // again replaces its older copy.
                        for row in rows {
                            read.rows.insert(row.position, row);
                        }
                        if read.rows.len() > model::MAX_SELECTION_ROWS as usize {
                            let wanted = self.catalog_selection();
                            read.rows
                                .retain(|position, _| wanted.selected(*position, 1));
                        }
                        self.select.state.catalog.selection_rows = Some(read);
                    }
                    Ok(_) => {}
                    Err(_) => self.select.catalog.rows_failed = Some((revision, ranges)),
                }
            }
        }
        Task::none()
    }

    /// One press, choice, key or keystroke of the catalog's.
    fn catalog_act(&mut self, action: CatalogAction) -> Task<Message> {
        match action {
            CatalogAction::View(source) => {
                self.close_catalog_menus();
                let query = model::view_query(&self.select.state.catalog, source);
                return self.evaluate(query);
            }
            CatalogAction::ToggleYear(group) => {
                let open = self.year_open(group);
                self.select.state.catalog.years.insert(group, !open);
            }
            CatalogAction::ToggleFolder(folder) => {
                let open = &mut self.select.state.catalog.open_folders;
                if !open.remove(&folder) {
                    open.insert(folder);
                }
            }
            CatalogAction::ToggleGroup(group) => {
                let closed = &mut self.select.state.catalog.closed_groups;
                if !closed.remove(&group) {
                    closed.insert(group);
                }
            }
            CatalogAction::Menu(menu) => {
                self.select.state.menu = None;
                let state = &mut self.select.state.catalog;
                state.naming = None;
                state.menu = menu;
            }
            CatalogAction::Name(target) => return self.start_naming(target),
            CatalogAction::NameText(text) => {
                if let Some(naming) = &mut self.select.state.catalog.naming {
                    naming.text = text;
                }
            }
            CatalogAction::Submit => return self.submit_naming(),
            CatalogAction::MoveFolder { folder, parent } => {
                self.close_catalog_menus();
                let params = model::folder_move_params(&folder, parent.as_ref(), &request());
                return self.catalog_library(CatalogGesture::MoveFolder, params).0;
            }
            CatalogAction::MergeFolder { folder, into } => {
                self.close_catalog_menus();
                let params = model::folder_merge_params(&folder, &into, &request());
                let (task, done) = self.catalog_library(CatalogGesture::MergeFolder, params);
                // A merged folder is gone: its view becomes the folder it went into.
                let viewing = self.viewing_folder(&folder);
                if done && viewing {
                    let source = ViewSource::CatalogFolder {
                        folder_id: into,
                        subfolders: true,
                    };
                    let query = model::view_query(&self.select.state.catalog, source);
                    return Task::batch([task, self.evaluate(query)]);
                }
                return task;
            }
            CatalogAction::DeleteFolder(folder) => {
                self.close_catalog_menus();
                let params = model::folder_delete_params(&folder, &request());
                let viewing = self.viewing_folder(&folder);
                let (task, done) = self.catalog_library(CatalogGesture::DeleteFolder, params);
                return self.after_removal(task, done && viewing);
            }
            CatalogAction::DeleteCollection(collection) => {
                self.close_catalog_menus();
                let params = model::collection_delete_params(&collection, &request());
                let viewing = matches!(
                    self.select.state.query.as_ref().map(|query| &query.source),
                    Some(ViewSource::Collection { collection_id }) if collection_id == &collection
                );
                let (task, done) = self.catalog_library(CatalogGesture::DeleteCollection, params);
                return self.after_removal(task, done && viewing);
            }
            CatalogAction::MovePhotos(folder) => {
                self.close_catalog_menus();
                let params = model::asset_move_params(&folder, &request());
                return self.catalog_library(CatalogGesture::MovePhotos, params).0;
            }
            CatalogAction::AddPhotos(collection) => {
                self.close_catalog_menus();
                let params = model::members_params(&collection, &request());
                return self.catalog_library(CatalogGesture::AddPhotos, params).0;
            }
            CatalogAction::RemovePhotos(collection) => {
                self.close_catalog_menus();
                let params = model::members_params(&collection, &request());
                return self.catalog_library(CatalogGesture::RemovePhotos, params).0;
            }
            CatalogAction::Change(change) => {
                self.close_catalog_menus();
                if let CatalogChange::Text(text) = &change {
                    self.select.state.catalog.search = text.clone();
                }
                if let Some(query) = &self.select.state.query {
                    let query = model::changed(query, &change);
                    return self.evaluate(query);
                }
            }
            // The text as typed; the evaluation follows once none is in flight
            // ([`after_message`]).
            CatalogAction::Search(text) => self.select.state.catalog.search = text,
            CatalogAction::Metadata => {
                self.close_catalog_menus();
                let state = &mut self.select.state.catalog;
                state.metadata = !state.metadata;
            }
            CatalogAction::FocusSearch => {
                let field = if self.select.state.over_catalog() {
                    crate::view::select_catalog::SEARCH_FIELD
                } else {
                    crate::view::select::SEARCH_FIELD
                };
                return iced::widget::operation::focus(iced::widget::Id::from(field));
            }
        }
        Task::none()
    }

    /// Close the catalog's menu and a name being typed, and the shell's own menu.
    fn close_catalog_menus(&mut self) {
        self.select.state.menu = None;
        self.select.state.catalog.close();
    }

    /// Whether the year group's folders are shown, as the sources panel draws it now.
    fn year_open(&self, group: YearGroup) -> bool {
        self.workspace
            .select
            .catalog
            .sources
            .folders
            .iter()
            .find(|row| row.toggle == Some(CatalogAction::ToggleYear(group)))
            .and_then(|row| row.open)
            .unwrap_or(false)
    }

    fn viewing_folder(&self, folder: &luxforge_core::catalog_types::CatalogFolderId) -> bool {
        matches!(
            self.select.state.query.as_ref().map(|query| &query.source),
            Some(ViewSource::CatalogFolder { folder_id, .. }) if folder_id == folder
        )
    }

    /// After deleting the folder or collection on screen, All photographs is viewed instead.
    fn after_removal(&mut self, task: Task<Message>, viewed_gone: bool) -> Task<Message> {
        if !viewed_gone {
            return task;
        }
        let query = crate::state::select::source_query(ViewSource::AllPhotographs);
        Task::batch([task, self.evaluate(query)])
    }

    /// Start typing a name: a new folder's or collection's, a rename (from the current name) or a
    /// smart collection's; the field takes the focus.
    fn start_naming(&mut self, target: NamingTarget) -> Task<Message> {
        self.select.state.menu = None;
        let state = &mut self.select.state.catalog;
        state.menu = None;
        let text = match &target {
            NamingTarget::RenameFolder(id) => state
                .folders
                .as_ref()
                .and_then(|list| list.folders.iter().find(|folder| &folder.id == id))
                .map(|folder| folder.name.clone())
                .unwrap_or_default(),
            NamingTarget::RenameCollection(id) => state
                .collections
                .as_ref()
                .and_then(|list| list.collections.iter().find(|c| &c.id == id))
                .map(|collection| collection.name.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        if let NamingTarget::NewFolder {
            parent: Some(parent),
        } = &target
        {
            state.open_folders.insert(parent.clone());
        }
        state.naming = Some(Naming { target, text });
        iced::widget::operation::focus(iced::widget::Id::from(
            crate::view::select_catalog::NAMING_FIELD,
        ))
    }

    /// Return in a name's field: the folder or collection made or renamed, or the smart collection
    /// saved, as one library change. An empty name, or an unchanged one, changes nothing. A refused
    /// name stays in its field, the status bar saying why.
    fn submit_naming(&mut self) -> Task<Message> {
        let Some(naming) = self.select.state.catalog.naming.clone() else {
            return Task::none();
        };
        let name = naming.text.trim().to_owned();
        if name.is_empty() {
            self.select.state.catalog.naming = None;
            return Task::none();
        }
        let mutation = request();
        let (gesture, params) = match &naming.target {
            NamingTarget::NewFolder { parent } => (
                CatalogGesture::CreateFolder,
                model::folder_create_params(&name, parent.as_ref(), &mutation),
            ),
            NamingTarget::NewCollection { kind, parent } => (
                if *kind == CollectionKind::Group {
                    CatalogGesture::CreateGroup
                } else {
                    CatalogGesture::CreateCollection
                },
                model::collection_create_params(&name, *kind, parent.as_ref(), &mutation),
            ),
            NamingTarget::RenameFolder(folder) => (
                CatalogGesture::RenameFolder,
                model::folder_rename_params(folder, &name, &mutation),
            ),
            NamingTarget::RenameCollection(collection) => (
                CatalogGesture::RenameCollection,
                model::collection_rename_params(collection, &name, &mutation),
            ),
            NamingTarget::SmartCollection => {
                // Exactly the query shown: the view as the owner evaluated it.
                let Some(query) = self.shown_query() else {
                    self.status.text = "Waiting for the view".into();
                    return Task::none();
                };
                (
                    CatalogGesture::CreateSmart,
                    model::smart_create_params(&name, &query, &mutation),
                )
            }
        };
        let (task, done) = self.catalog_library(gesture, params);
        if done {
            self.select.state.catalog.naming = None;
        }
        task
    }

    /// The query of the view on screen, as the owner evaluated it.
    pub(crate) fn shown_query(&self) -> Option<ViewQuery> {
        let state = &self.select.state;
        state
            .summary
            .as_ref()
            .filter(|summary| {
                state
                    .query
                    .as_ref()
                    .is_none_or(|query| query.source == summary.query.source)
            })
            .map(|summary| summary.query.clone())
    }

    /// Send one organizing library change synchronously, in this update, as this desktop's actor,
    /// and follow what it recorded as the shell follows every library change. The request and the
    /// owner's answer are kept for evidence. Answers the task and whether the owner accepted it.
    pub(crate) fn catalog_library(
        &mut self,
        gesture: CatalogGesture,
        params: Value,
    ) -> (Task<Message>, bool) {
        let method = gesture.method();
        let result = catalog_call(&self.owner, self.client, gesture, params.clone());
        self.select.library = Some(json!({
            "method": method,
            "params": params,
            "answer": result.as_ref().ok().map(|(_, answer)| answer),
            "error": result.as_ref().err().map(|error| json!({
                "code": error.code,
                "message": error.message,
                "data": error.data,
            })),
        }));
        match result {
            Ok((answer, _)) => (
                self.library_answered(LibraryGesture::Catalog(gesture), &answer),
                true,
            ),
            Err(error) => {
                self.library_refused(LibraryGesture::Catalog(gesture), &error);
                // A stale view refuses its selection: read it again, so the next press acts on
                // what is shown.
                if error.code == "conflict" && gesture.of_selection() {
                    self.select.check.offer(());
                }
                (Task::none(), false)
            }
        }
    }

    /// Nothing the catalog asked the owner for is in flight or still wanted.
    pub(crate) fn catalog_quiet(&self) -> bool {
        let seam = &self.select.catalog;
        seam.lists.idle()
            && seam.totalling.is_none()
            && seam.reading.is_none()
            && self.total_wanted().is_none()
            && self.rows_wanted().is_none()
            && self.search_wanted().is_none()
    }

    /// The source to count with no filter, and the library change to count it at: over the
    /// catalog, with a filter set, when the total held is for another source or change.
    fn total_wanted(&self) -> Option<(ViewSource, u64)> {
        let state = &self.select.state;
        if !self.select_shown() || !state.over_catalog() {
            return None;
        }
        let summary = state.summary.as_ref()?;
        if summary.query.filter == Default::default() {
            return None;
        }
        let key = (summary.query.source.clone(), summary.library_sequence.0);
        let held = state
            .catalog
            .total
            .as_ref()
            .is_some_and(|total| total.source == key.0 && total.sequence == key.1);
        (!held && self.select.catalog.total_failed.as_ref() != Some(&key)).then_some(key)
    }

    /// The selection's rows the Info panel lacks, once the rows near the screen are read.
    fn rows_wanted(&self) -> Option<(u64, Vec<(u32, u32)>)> {
        let state = &self.select.state;
        if !self.select_shown() || state.rows.in_flight().is_some() {
            return None;
        }
        let revision = state.revision()?;
        let ranges = model::missing_rows(state, &self.catalog_selection());
        if ranges.is_empty() {
            return None;
        }
        let key = (revision, ranges);
        (self.select.catalog.rows_failed.as_ref() != Some(&key)).then_some(key)
    }

    /// The query the search text asks for, when the view's differs: over the catalog, once the
    /// text belongs to the source on screen.
    fn search_wanted(&self) -> Option<ViewQuery> {
        let state = &self.select.state;
        if !self.select_shown() || !state.over_catalog() {
            return None;
        }
        let query = state.query.as_ref()?;
        if state.catalog.searched.as_ref() != Some(&query.source) {
            return None;
        }
        let text = model::search_text(&state.catalog.search);
        (text != query.filter.text)
            .then(|| model::changed(query, &CatalogChange::Text(text.unwrap_or_default())))
    }

    /// The session's selection in the view on screen.
    fn catalog_selection(&self) -> SelectionModel {
        SelectionModel::of(&self.session.browse, self.select.state.revision())
    }

    /// What the catalog shows, for correlated evidence: the folders and collections listed, the
    /// filter bar, the Metadata browser's columns with each value's count, and the Info panel's
    /// organize band.
    pub(crate) fn catalog_summary(&self) -> Value {
        let model = &self.workspace.select.catalog;
        let rows = |rows: &[model::CatalogRow]| {
            rows.iter()
                .map(|row| {
                    json!({
                        "name": row.name,
                        "count": row.count,
                        "indent": row.indent,
                        "open": row.open,
                        "selected": row.selected,
                        "naming": row.naming,
                    })
                })
                .collect::<Vec<_>>()
        };
        let state = &self.select.state.catalog;
        json!({
            "shown": model.shown,
            "folders": rows(&model.sources.folders),
            "collections": rows(&model.sources.collections),
            "listed": {
                "folders": state.folders.as_ref().map(|list| list.folders.len()),
                "collections": state.collections.as_ref().map(|list| list.collections.len()),
            },
            "filter": model.filter.as_ref().map(|bar| json!({
                "search": bar.search,
                "metadata_open": bar.metadata_open,
                "edited": {"label": bar.edited.label, "set": bar.edited.set},
                "conditions": bar.conditions.iter().map(|chip| json!([format!("{:?}", chip.glyph).to_lowercase(), chip.label])).collect::<Vec<_>>(),
                "save": bar.save_refused,
                "count": bar.count,
            })),
            "total": state.total.as_ref().map(|total| json!({
                "source": total.source,
                "sequence": total.sequence,
                "count": total.count,
            })),
            "metadata": model.metadata.as_ref().map(|columns| columns.iter().map(|column| json!({
                "facet": column.facet.as_str(),
                "rows": column.rows.iter().map(|row| json!({
                    "label": row.label,
                    "count": row.count,
                    "indent": row.indent,
                    "selected": row.selected,
                    "enabled": row.change.is_some(),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>()),
            "info": model.info.as_ref().map(|info| json!({
                "count": info.count,
                "title": info.title,
                "folders": info.folders.iter().map(|chip| json!([chip.label, chip.partial])).collect::<Vec<_>>(),
                "collections": info.collections.iter().map(|chip| json!([chip.label, chip.partial])).collect::<Vec<_>>(),
                "note": info.organize_note,
                "edited": info.edited,
                "previews": info.previews,
            })),
            "menu": state.menu.as_ref().map(|menu| format!("{menu:?}")),
            "naming": state.naming.as_ref().map(|naming| &naming.text),
        })
    }
}

/// After every message while Select is shown: read the folders and collections when asked, follow
/// the search text once no evaluation is in flight, and count the source and read the selection's
/// rows when the bar and the Info panel want them.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    if !editor.select_shown() {
        return Task::none();
    }
    let mut tasks = Vec::new();
    if editor.select.catalog.lists.start().is_some() {
        let (owner, client) = (editor.owner.clone(), editor.client);
        tasks.push(owner_task(
            move || lists_now(&owner, client),
            |result| catalog(CatalogMessage::Lists(result)),
        ));
    }
    // A source chosen starts from its own query's search text.
    let state = &mut editor.select.state;
    if state.over_catalog()
        && let Some(query) = &state.query
        && state.catalog.searched.as_ref() != Some(&query.source)
    {
        state.catalog.search = query.filter.text.clone().unwrap_or_default();
        state.catalog.searched = Some(query.source.clone());
    }
    if !editor.select.state.loading
        && let Some(query) = editor.search_wanted()
    {
        tasks.push(editor.evaluate(query));
    }
    if editor.select.catalog.totalling.is_none()
        && let Some(key) = editor.total_wanted()
    {
        editor.select.catalog.totalling = Some(key.clone());
        let (owner, client) = (editor.owner.clone(), editor.client);
        tasks.push(owner_task(
            move || {
                let result = total_now(&owner, client, &key.0);
                (key, result)
            },
            |((source, sequence), result)| {
                catalog(CatalogMessage::Totalled {
                    source,
                    sequence,
                    result,
                })
            },
        ));
    }
    if editor.select.catalog.reading.is_none()
        && let Some(key) = editor.rows_wanted()
    {
        editor.select.catalog.reading = Some(key.clone());
        let (owner, client) = (editor.owner.clone(), editor.client);
        tasks.push(owner_task(
            move || {
                let result = selection_rows_now(&owner, client, key.0, &key.1);
                (key, result)
            },
            |((revision, ranges), result)| {
                catalog(CatalogMessage::SelectionRows {
                    revision,
                    ranges,
                    result,
                })
            },
        ));
    }
    Task::batch(tasks)
}

#[cfg(test)]
#[path = "select_catalog_tests.rs"]
mod tests;
