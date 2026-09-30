//! The Select workspace ([catalog design](../../../../docs/design/catalog.md#workspaces)): the
//! workspace switch, the sources, the grouped virtualized grid over the owner's view, keyboard
//! navigation and selection, and the Info panel. **Lane D (views and desktop)** owns this seam.
//!
//! The desktop holds no catalog logic: every gesture sends the request its API equivalent sends,
//! the owner holds the view and the selection, and this seam keeps only what it last read, what is
//! in flight, and the grid's own layout, scroll and viewport.
//!
//! - `event.list`, `browse.view` (with the session read after it), `browse.facets` and
//!   `browse.rows` are owner tasks, off the gesture path. Each answer names what it answers — the
//!   evaluation's number, the view revision a window of rows belongs to — and an overtaken one is
//!   dropped. Rows are read in aligned blocks near the screen, one request in flight, into a bounded
//!   cache ([`crate::state::select::RowCache`]), so a 10,000-file view reads only what is near what
//!   is shown.
//! - `browse.select` is session-only and cheap on the owner, so a click or an arrow key calls it
//!   synchronously in its own update ([performance rule 12](../../../../docs/engineering/performance-rules.md#rules)):
//!   the grid draws the session's selection and active item in the frame after the key, as the
//!   owner reports them.
//! - The owner's wake for another client's change asks for `session.state` while Select is shown;
//!   a view the session reports stale — a library change or an index revision since it was
//!   evaluated — is evaluated again, the scroll kept near the active item.
//! - A library change — `P` and the Info panel's Pick (`pick.set` of the selection), a bracket's
//!   Pick all (`pick.set` of its files), and `Cmd+Z` and `Shift+Cmd+Z` (`library.undo` and
//!   `library.redo`) — is sent synchronously in the update of its key or press, as this desktop's
//!   actor: the journal records the gestures in the order they were made, and a pick of the
//!   selection names the selection on screen, which the next arrow key's synchronous
//!   `browse.select` would otherwise overtake. The owner's work is one catalog transaction. What
//!   the change leaves is read as another client's change is: the view evaluated again, keeping
//!   the scroll and the active item, the events and the catalog's counts read again, and the
//!   change's label from the journal said in the status bar, as owner tasks.
//! - The sources panel reads the cards and volumes (`card.list`, `volume.list`) and the catalog's
//!   counts (`catalog.info`) each time Select is shown, the counts again after a library change,
//!   and a volume's or folder's subfolders (`disk.folders`) when it is opened On disk. A card or a
//!   folder is read by the index lane (`index.refresh`) before it is viewed.
//!
//! Which workspace is shown, the panels, the collapsed bursts, the size slider and the selection's
//! anchor are this desktop's own view state, like the developer gallery page: no other client sees
//! them.
use crate::app::select_previews::{GridWindow, SelectPreviewMessage, SelectPreviews};
use crate::app::{
    Before, Editor,
    gesture::Starting,
    message::{
        Message,
        select::{SelectMessage, Step},
    },
    outcome::Outcome,
    tasks::{CallError, call, call_detailed, owner_task, request},
};
use crate::coalesce::Coalesce;
use crate::state::select::{
    self as model, Block, GridContent, LibraryGesture, ReadSource, RowsRequest, SelectGesture,
    SelectPanel, SelectState, SelectionModel, Shown,
};
use iced::{Size, Task};
use luxforge_core::{
    ClientId, ClientSession, OwnerHandle,
    catalog_types::{
        Cards, CatalogCounts, CatalogInfo, DiskFolders, EventList, Facets, LibraryAnswer,
        LibraryChange, LibraryJournal, RowItem, Targets, ViewQuery, ViewRows, ViewSource,
        ViewSummary, Volumes,
    },
};
use luxforge_ui::{
    GridBlock, GridDirection, GridHeading, GridLayout, GridMetrics, GridPress, MomentHeader,
    MomentKind,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, ops::Range, path::PathBuf};

/// The Select workspace's own state in the editor: its view-model state, the grid's layout, scroll
/// and viewport, and what is in flight.
#[derive(Debug)]
pub(crate) struct Select {
    /// What the model reads.
    pub(crate) state: SelectState,
    /// The grid's geometry, laid out again only when the view's group layout, a collapsed burst,
    /// the width or the cell size changes.
    pub(crate) layout: GridLayout,
    /// The grid's scroll offset, as the widget last published it or a key revealed an item.
    pub(crate) scroll: f32,
    /// The grid's size as the widget last reported it; zero until it has.
    pub(crate) viewport: Size,
    /// Where a Shift extension starts: the item of the last click or arrow key.
    pub(crate) anchor: Option<u32>,
    /// The newest evaluation's number; an answer for any other is dropped.
    pub(crate) serial: u64,
    /// `event.list`: one in flight, the newest search text waiting.
    pub(crate) events: Coalesce<String>,
    /// The events have been asked for since Select was first shown.
    pub(crate) events_read: bool,
    /// The staleness check after a wake: one in flight, one waiting.
    pub(crate) check: Coalesce<()>,
    /// The owner woke the desktop while Develop was shown, so showing Select checks once.
    pub(crate) check_on_show: bool,
    /// The evaluation whose facets have answered, successfully or not.
    pub(crate) facets_answered: u64,
    /// An evidence step that waits for the view to be evaluated again: the revision it must pass.
    pub(crate) evidence_after: Option<u64>,
    /// Why the evaluation in flight was asked for, which the status bar says when it lands.
    pub(crate) reread: Reread,
    /// The card or folder being read before it is viewed: `index.refresh` lists it and reads its
    /// headers.
    pub(crate) reading: Option<Reading>,
    /// A `job.read` of the reading source's job is in flight.
    pub(crate) read_in_flight: bool,
    /// `card.list` and `volume.list`: one read in flight, one waiting.
    pub(crate) disks: Coalesce<()>,
    /// `catalog.info`: one read in flight, one waiting.
    pub(crate) counts: Coalesce<()>,
    /// The volumes and folders On disk whose `disk.folders` is in flight.
    pub(crate) listing: BTreeSet<PathBuf>,
    /// This desktop's newest library change and the gesture that made it: its label is read from
    /// the journal for the status bar.
    pub(crate) change: Option<(u64, LibraryGesture)>,
    /// The change whose label is being read.
    pub(crate) label: Option<u64>,
    /// The last library request this desktop sent and what the owner answered, for evidence.
    pub(crate) library: Option<Value>,
    /// The grid's decoded previews: each cell's handle, made once and held under the byte budget.
    pub(crate) previews: SelectPreviews,
}

/// A card or a folder on disk whose listing and headers the index lane is reading.
#[derive(Clone, Debug)]
pub(crate) struct Reading {
    pub(crate) source: ReadSource,
    /// The `index.refresh` job, once the owner has answered with it.
    pub(crate) job: Option<String>,
}

/// Why a view is being evaluated, which the status bar says once it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Reread {
    /// A source, filter, sort or grouping was chosen: the status bar names the view.
    #[default]
    Asked,
    /// Another client's change made it stale.
    Elsewhere,
    /// This desktop's own library change made it stale; the status bar says that change.
    Own,
}

/// How often the reading source's job is read while it runs. The core pushes no client anything
/// about a job, so a client waiting for one reads it, as export does; the timer exists only while
/// a card or folder is being read.
pub(crate) const READ_POLL: std::time::Duration = std::time::Duration::from_millis(100);

impl Default for Select {
    fn default() -> Self {
        let state = SelectState {
            home: std::env::var_os("HOME").map(PathBuf::from),
            ..SelectState::default()
        };
        let layout = GridLayout::new(Vec::new(), metrics(&state), 0.0);
        Self {
            state,
            layout,
            scroll: 0.0,
            viewport: Size::ZERO,
            anchor: None,
            serial: 0,
            events: Coalesce::default(),
            events_read: false,
            check: Coalesce::default(),
            check_on_show: false,
            facets_answered: 0,
            evidence_after: None,
            reread: Reread::Asked,
            reading: None,
            read_in_flight: false,
            disks: Coalesce::default(),
            counts: Coalesce::default(),
            listing: BTreeSet::new(),
            change: None,
            label: None,
            library: None,
            previews: SelectPreviews::default(),
        }
    }
}

/// The grid's metrics for the preset the view is drawn with, at the size slider's width.
fn metrics(state: &SelectState) -> GridMetrics {
    if state.over_catalog() {
        GridMetrics::catalog(state.cell_width())
    } else {
        GridMetrics::select(state.cell_width())
    }
}

/// The grid widget's blocks for the view model's.
pub(crate) fn grid_blocks(content: &GridContent) -> Vec<GridBlock> {
    content
        .blocks
        .iter()
        .map(|block| match block {
            Block::Day { title, detail } => GridBlock::Day(GridHeading {
                title: title.clone(),
                detail: detail.clone(),
            }),
            Block::Camera { title, detail } => GridBlock::Camera(GridHeading {
                title: title.clone(),
                detail: detail.clone(),
            }),
            Block::Moment {
                bracket,
                title,
                detail,
                evidence,
                picked,
                action,
                frames,
                collapsed,
                ..
            } => GridBlock::Moment {
                header: MomentHeader {
                    kind: if *bracket {
                        MomentKind::Bracket
                    } else {
                        MomentKind::Burst
                    },
                    title: title.clone(),
                    detail: detail.clone(),
                    evidence: evidence.clone(),
                    picked: picked.clone(),
                    action: action.clone(),
                },
                frames: *frames,
                collapsed: *collapsed,
            },
            Block::Singles(count) => GridBlock::Singles(*count),
        })
        .collect()
}

fn direction(step: Step) -> GridDirection {
    match step {
        Step::Left => GridDirection::Left,
        Step::Right => GridDirection::Right,
        Step::Up => GridDirection::Up,
        Step::Down => GridDirection::Down,
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

// -- The owner calls, each the body of one owner task (or, for `browse.select`, the one synchronous
// call), so a test runs exactly what a task would against a real owner. --

/// `event.list` for the sources panel's search text.
pub(crate) fn events_now(
    owner: &OwnerHandle,
    client: ClientId,
    search: &str,
) -> Result<EventList, String> {
    let (list, _) = call(owner, client, "event.list", model::events_params(search))?;
    parse(list)
}

/// `browse.view` with the whole query, then `session.state` for the selection the owner carried
/// over to the new evaluation.
pub(crate) fn evaluate_now(
    owner: &OwnerHandle,
    client: ClientId,
    query: &ViewQuery,
) -> Result<Box<(ViewSummary, ClientSession)>, String> {
    let (summary, _) = call(owner, client, "browse.view", model::view_params(query))?;
    let summary = parse::<ViewSummary>(summary)?;
    Ok(Box::new((summary, session_now(owner, client)?)))
}

/// `browse.facets` for the chips' menus.
pub(crate) fn facets_now(
    owner: &OwnerHandle,
    client: ClientId,
    query: &ViewQuery,
) -> Result<Facets, String> {
    let (counts, _) = call(owner, client, "browse.facets", model::facets_params(query))?;
    parse(counts)
}

/// `browse.rows` for one block.
pub(crate) fn rows_now(
    owner: &OwnerHandle,
    client: ClientId,
    request: &RowsRequest,
) -> Result<ViewRows, String> {
    let (rows, _) = call(owner, client, "browse.rows", model::rows_params(request))?;
    parse(rows)
}

/// `session.state`: the session, whose `browse` says whether the view went stale.
pub(crate) fn session_now(owner: &OwnerHandle, client: ClientId) -> Result<ClientSession, String> {
    let (session, _) = call(owner, client, "session.state", json!({}))?;
    parse(session)
}

/// `index.refresh` of a card, or of a folder on disk with its subfolders: the job that lists it.
pub(crate) fn refresh_now(
    owner: &OwnerHandle,
    client: ClientId,
    source: &ReadSource,
) -> Result<String, String> {
    let (started, _) = call(owner, client, "index.refresh", source.refresh_params())?;
    started["job_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("index.refresh answered no job: {started}"))
}

/// `job.read` for one job.
pub(crate) fn job_now(owner: &OwnerHandle, client: ClientId, job: &str) -> Result<Value, String> {
    let (record, _) = call(
        owner,
        client,
        luxforge_core::jobs::JOB_READ,
        json!({ "job_id": job }),
    )?;
    Ok(record)
}

/// `card.list` and `volume.list`, for the sources panel's Cards and On disk.
pub(crate) fn disks_now(
    owner: &OwnerHandle,
    client: ClientId,
) -> Result<Box<(Cards, Volumes)>, String> {
    let (cards, _) = call(owner, client, "card.list", json!({}))?;
    let (volumes, _) = call(owner, client, "volume.list", json!({}))?;
    Ok(Box::new((parse(cards)?, parse(volumes)?)))
}

/// `disk.folders` for a volume or folder opened On disk.
pub(crate) fn folders_now(
    owner: &OwnerHandle,
    client: ClientId,
    path: &std::path::Path,
) -> Result<DiskFolders, String> {
    let (folders, _) = call(owner, client, "disk.folders", json!({ "path": path }))?;
    parse(folders)
}

/// `catalog.info`'s counts, behind the Catalog sources.
pub(crate) fn counts_now(owner: &OwnerHandle, client: ClientId) -> Result<CatalogCounts, String> {
    let (info, _) = call(owner, client, "catalog.info", json!({}))?;
    parse::<CatalogInfo>(info).map(|info| info.counts)
}

/// `library.journal` for the one change `sequence`, whose label the status bar says.
pub(crate) fn label_now(
    owner: &OwnerHandle,
    client: ClientId,
    sequence: u64,
) -> Result<Option<LibraryChange>, String> {
    let (page, _) = call(
        owner,
        client,
        "library.journal",
        model::journal_params(sequence),
    )?;
    let page = parse::<LibraryJournal>(page)?;
    Ok(page
        .changes
        .into_iter()
        .find(|change| change.sequence.0 == sequence))
}

/// One library change of this desktop's — `pick.set`, `library.undo` or `library.redo` — sent
/// synchronously, answered with what it recorded or its refusal with its data.
pub(crate) fn library_call(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<LibraryAnswer, CallError> {
    let answer = call_detailed(owner, client, method, params)?;
    serde_json::from_value(answer).map_err(|error| CallError {
        code: "internal".into(),
        message: format!("{method} answered unexpectedly: {error}"),
        data: None,
        job_id: None,
    })
}

/// `browse.select`, answered with the session.
pub(crate) fn select_call(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<ClientSession, String> {
    let (session, _) = call(owner, client, "browse.select", params)?;
    parse(session)
}

impl Editor {
    /// One Select message.
    pub(crate) fn select_update(&mut self, message: SelectMessage) -> Task<Message> {
        match message {
            SelectMessage::Switch(shown) => return self.switch_workspace(shown),
            SelectMessage::Search(text) => {
                self.select.state.search = text.clone();
                self.select.events.offer(text);
                return self.start_events();
            }
            SelectMessage::Events(result) => {
                self.select.events.answered();
                match result {
                    Ok(list) => {
                        self.select.state.events = Some(list);
                        self.select.state.events_error = None;
                    }
                    Err(error) => {
                        self.status.text = format!("Events unavailable: {error}");
                        self.select.state.events_error = Some(error);
                    }
                }
                return self.start_events();
            }
            SelectMessage::Source(source) => {
                self.select.state.menu = None;
                return self.evaluate(model::source_query(source));
            }
            SelectMessage::BrowseFolder => {
                if self.view_state.picker_open || self.evidence.is_some() {
                    return Task::none();
                }
                self.view_state.picker_open = true;
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .pick_folder()
                            .await
                            .map(|folder| folder.path().to_path_buf())
                    },
                    |path| Message::Select(SelectMessage::FolderPicked(path)),
                );
            }
            SelectMessage::FolderPicked(path) => {
                self.view_state.picker_open = false;
                if let Some(path) = path {
                    return self.read_source(ReadSource::Folder(path));
                }
            }
            SelectMessage::Read(source) => return self.read_source(source),
            SelectMessage::Reading(result) => match result {
                Ok(job) => {
                    if let Some(reading) = &mut self.select.reading {
                        reading.job = Some(job);
                    }
                }
                Err(error) => {
                    if let Some(reading) = self.select.reading.take() {
                        self.status.text = format!(
                            "Could not read {}: {error}",
                            reading.source.name(self.select.state.home.as_deref())
                        );
                    }
                }
            },
            SelectMessage::ReadPoll => return self.poll_reading(),
            SelectMessage::ReadAnswered(result) => return self.reading_answered(result),
            SelectMessage::Toggle(path) => return self.toggle_disk(path),
            SelectMessage::Listed { path, result } => {
                self.select.listing.remove(&path);
                match result {
                    Ok(folders) => {
                        self.select.state.disk.insert(path, folders);
                    }
                    Err(error) => {
                        self.status.text = format!(
                            "Could not list {}: {error}",
                            model::shown_path(&path, self.select.state.home.as_deref())
                        );
                        self.select.state.open.remove(&path);
                    }
                }
            }
            SelectMessage::Disks(result) => {
                self.select.disks.answered();
                match result {
                    Ok(answer) => {
                        let (cards, volumes) = *answer;
                        self.select.state.cards = Some(cards);
                        self.select.state.volumes = Some(volumes);
                    }
                    Err(error) => self.status.text = format!("Volumes unavailable: {error}"),
                }
                return self.start_disks();
            }
            SelectMessage::Counted(result) => {
                self.select.counts.answered();
                match result {
                    Ok(counts) => self.select.state.counts = Some(counts),
                    Err(error) => self.status.text = format!("Catalog counts unavailable: {error}"),
                }
                return self.start_counts();
            }
            SelectMessage::Pick => return self.pick_selection(),
            SelectMessage::PickAll(number) => return self.pick_all(number),
            SelectMessage::Undo => {
                return self.library_now(LibraryGesture::Undo, model::library_params(&request()));
            }
            SelectMessage::Redo => {
                return self.library_now(LibraryGesture::Redo, model::library_params(&request()));
            }
            SelectMessage::Labelled { sequence, result } => self.labelled(sequence, result),
            SelectMessage::Change(change) => {
                self.select.state.menu = None;
                if let Some(query) = &self.select.state.query {
                    let query = model::changed(query, &change);
                    return self.evaluate(query);
                }
            }
            SelectMessage::Menu(menu) => self.select.state.menu = menu,
            SelectMessage::Viewed { serial, result } => self.viewed(serial, result),
            SelectMessage::Faceted { serial, result } => {
                if serial == self.select.serial {
                    // A failure leaves the menus saying the counts are unavailable.
                    self.select.state.facets = result.ok();
                    self.select.facets_answered = serial;
                }
            }
            SelectMessage::Rows {
                revision,
                from,
                result,
            } => match result {
                // Rows of another revision or window than asked are no answer to this request.
                Ok(rows) if rows.revision == revision && rows.from == from => {
                    let wanted = self.wanted_items();
                    self.select
                        .state
                        .rows
                        .answered(revision, from, rows.rows, wanted);
                }
                Ok(_) => self.select.state.rows.failed(revision, from),
                Err(error) => {
                    if revision == self.select.state.rows.revision() {
                        self.status.text = format!("Rows unavailable: {error}");
                    }
                    self.select.state.rows.failed(revision, from);
                }
            },
            SelectMessage::Scrolled(offset) => {
                self.select.scroll = self
                    .select
                    .layout
                    .clamp_scroll(offset, self.select.viewport.height);
            }
            SelectMessage::Viewport(size) => {
                let relayout = (self.select.layout.width() - size.width).abs() >= 0.5;
                self.select.viewport = size;
                if relayout {
                    self.select
                        .layout
                        .relayout(metrics(&self.select.state), size.width);
                }
                self.select.scroll = self
                    .select
                    .layout
                    .clamp_scroll(self.select.scroll, size.height);
            }
            SelectMessage::Press(press) => self.press(press),
            SelectMessage::Move { step, extend } => self.move_active(step, extend),
            SelectMessage::SelectAll => {
                self.select_now(SelectGesture::All);
            }
            SelectMessage::SelectNone => {
                self.select_now(SelectGesture::Nothing);
            }
            SelectMessage::Collapse => self.collapse(),
            SelectMessage::CellWidth(width) => {
                self.select.state.set_cell_width(width);
                let width = self.select.layout.width();
                self.select
                    .layout
                    .relayout(metrics(&self.select.state), width);
                self.select.scroll = self.near_active(self.select.scroll);
            }
            SelectMessage::TogglePanel(panel) => {
                let state = &mut self.select.state;
                match panel {
                    SelectPanel::Sources => state.sources_panel = !state.sources_panel,
                    SelectPanel::Info => state.info_panel = !state.info_panel,
                }
            }
            SelectMessage::TogglePanels => {
                let state = &mut self.select.state;
                let show = !(state.sources_panel || state.info_panel);
                state.sources_panel = show;
                state.info_panel = show;
            }
            SelectMessage::Checked(result) => return self.checked(result),
            SelectMessage::Loupe(message) => return self.loupe_update(message),
            SelectMessage::Previews(message) => {
                return self
                    .select
                    .previews
                    .update(&self.owner, self.client, message)
                    .map(previews_message);
            }
        }
        Task::none()
    }

    /// Nothing Select asked the owner for is in flight or still wanted, and the cells on screen have
    /// their previews or nothing to wait for: what an evidence step settles on.
    pub(crate) fn select_quiet(&self) -> bool {
        self.select_reads_quiet() && self.select.previews.settled()
    }

    /// Nothing Select asked the owner for is in flight or still wanted: the events, the view and
    /// its facets, a staleness check and the rows near the screen have all answered.
    pub(crate) fn select_reads_quiet(&self) -> bool {
        let select = &self.select;
        select.reading.is_none()
            && !select.state.loading
            && select.events.idle()
            && select.check.idle()
            && select.disks.idle()
            && select.counts.idle()
            && select.listing.is_empty()
            && select.label.is_none()
            && (select.state.query.is_none() || select.facets_answered == select.serial)
            && select.state.rows.in_flight().is_none()
            && (select.state.summary.is_none() || !select.state.rows.wants(self.wanted_items()))
    }

    /// The Select workspace is on screen.
    pub(crate) fn select_shown(&self) -> bool {
        self.select.state.shown == Shown::Select
    }

    /// The panel at the window's left edge is on screen: the sources panel in Select, the state
    /// panel in Develop. The Performance section is pinned at its foot in both.
    pub(crate) fn left_panel_shown(&self) -> bool {
        if self.select_shown() {
            self.select.state.sources_panel
        } else {
            self.session.workspace.state_panel
        }
    }

    /// The owner woke the desktop for another client's change: while Select is shown, read the
    /// session to see whether the view went stale; otherwise do so once Select is shown again.
    pub(crate) fn select_woken(&mut self) {
        if self.select_shown() {
            self.select.check.offer(());
        } else {
            self.select.check_on_show = true;
        }
    }

    /// Switch workspaces. Nothing is committed, discarded or paused, and each keeps its state;
    /// Select is refused while a Develop draft is open, through the one start refusal.
    fn switch_workspace(&mut self, shown: Shown) -> Task<Message> {
        if self.select.state.shown == shown {
            return Task::none();
        }
        if shown == Shown::Develop {
            self.select.state.menu = None;
            self.select.state.shown = Shown::Develop;
            self.select.previews.release();
            return Task::none();
        }
        if let Some(reason) = self.gesture_refusal(Starting::Workspace) {
            self.status.text = reason;
            return Task::none();
        }
        self.palette.open = false;
        self.view_state.menu = None;
        self.select.state.shown = Shown::Select;
        self.status.text = "Showing Select".into();
        if std::mem::take(&mut self.select.check_on_show) {
            self.select.check.offer(());
        }
        // The cards and volumes, and the catalog's counts, as they are now.
        let mut tasks = vec![self.read_disks(), self.read_counts()];
        if !std::mem::replace(&mut self.select.events_read, true) {
            tasks.push(self.read_events());
        }
        Task::batch(tasks)
    }

    /// Browse a card or a folder on disk: the index lane lists it, a folder with its subfolders,
    /// and reads each file's header (`index.refresh`, a job), and it is viewed once the job has
    /// ended. The status bar says it is reading until then.
    pub(crate) fn read_source(&mut self, source: ReadSource) -> Task<Message> {
        if let ReadSource::Folder(path) = &source {
            self.select.state.folder = Some(path.clone());
        }
        self.select.state.menu = None;
        self.status.text = format!(
            "Reading {}\u{2026}",
            source.name(self.select.state.home.as_deref())
        );
        self.select.reading = Some(Reading {
            source: source.clone(),
            job: None,
        });
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || refresh_now(&owner, client, &source),
            |result| Message::Select(SelectMessage::Reading(result)),
        )
    }

    /// Open a volume or folder On disk, reading its subfolders (`disk.folders`), or close it.
    fn toggle_disk(&mut self, path: PathBuf) -> Task<Message> {
        if self.select.state.open.remove(&path) {
            return Task::none();
        }
        self.select.state.open.insert(path.clone());
        if !self.select.listing.insert(path.clone()) {
            return Task::none();
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let result = folders_now(&owner, client, &path);
                (path, result)
            },
            |(path, result)| Message::Select(SelectMessage::Listed { path, result }),
        )
    }

    /// Ask `card.list` and `volume.list` again.
    fn read_disks(&mut self) -> Task<Message> {
        self.select.disks.offer(());
        self.start_disks()
    }

    fn start_disks(&mut self) -> Task<Message> {
        if self.select.disks.start().is_none() {
            return Task::none();
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || disks_now(&owner, client),
            |result| Message::Select(SelectMessage::Disks(result)),
        )
    }

    /// Ask `catalog.info` again for the Catalog sources' counts.
    fn read_counts(&mut self) -> Task<Message> {
        self.select.counts.offer(());
        self.start_counts()
    }

    fn start_counts(&mut self) -> Task<Message> {
        if self.select.counts.start().is_none() {
            return Task::none();
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || counts_now(&owner, client),
            |result| Message::Select(SelectMessage::Counted(result)),
        )
    }

    /// Read the reading folder's job, one read at a time.
    fn poll_reading(&mut self) -> Task<Message> {
        let Some(job) = self
            .select
            .reading
            .as_ref()
            .and_then(|reading| reading.job.clone())
        else {
            return Task::none();
        };
        if std::mem::replace(&mut self.select.read_in_flight, true) {
            return Task::none();
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || job_now(&owner, client, &job),
            |result| Message::Select(SelectMessage::ReadAnswered(result)),
        )
    }

    /// The reading source's job answered: still running, it says how far it has got; ended, the
    /// card or folder is viewed; failed or cancelled, the status bar says so and nothing is viewed.
    fn reading_answered(&mut self, result: Result<Value, String>) -> Task<Message> {
        self.select.read_in_flight = false;
        let Some(reading) = self.select.reading.clone() else {
            return Task::none();
        };
        let name = reading.source.name(self.select.state.home.as_deref());
        let record = match result {
            Ok(record) => record,
            Err(error) => {
                self.select.reading = None;
                self.status.text = format!("Could not read {name}: {error}");
                return Task::none();
            }
        };
        match record["status"].as_str() {
            Some("queued" | "running") => {
                self.status.text = match record["progress"]["message"].as_str() {
                    Some(progress) => format!("Reading {name} \u{b7} {progress}"),
                    None => format!("Reading {name}\u{2026}"),
                };
                Task::none()
            }
            Some("ready") => {
                self.select.reading = None;
                let source = match reading.source {
                    // The folder as the index listed it — its canonical path, which a symbolic
                    // link in the chosen one resolves to — is the one its files are indexed under.
                    ReadSource::Folder(chosen) => {
                        let path = record["result"]["roots"][0]
                            .as_str()
                            .map_or(chosen, PathBuf::from);
                        self.select.state.folder = Some(path.clone());
                        ViewSource::Folder {
                            path,
                            subfolders: true,
                        }
                    }
                    ReadSource::Card { volume_id, .. } => ViewSource::Card { volume_id },
                };
                // A listed card now says how many files it holds.
                let disks = if matches!(source, ViewSource::Card { .. }) {
                    self.read_disks()
                } else {
                    Task::none()
                };
                Task::batch([disks, self.evaluate(model::source_query(source))])
            }
            other => {
                self.select.reading = None;
                let reason = record["error"]["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("the listing ended {}", other.unwrap_or("unknown")));
                self.status.text = format!("Could not read {name}: {reason}");
                Task::none()
            }
        }
    }

    /// Ask `event.list` again for the search text.
    fn read_events(&mut self) -> Task<Message> {
        self.select.events.offer(self.select.state.search.clone());
        self.start_events()
    }

    fn start_events(&mut self) -> Task<Message> {
        let Some(search) = self.select.events.start() else {
            return Task::none();
        };
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || events_now(&owner, client, &search),
            |result| Message::Select(SelectMessage::Events(result)),
        )
    }

    /// Evaluate `query` into this client's one view: `browse.view` with the whole query and nothing
    /// else, then the session it left, in one owner task; and the chips' `browse.facets` beside it.
    fn evaluate(&mut self, query: ViewQuery) -> Task<Message> {
        let state = &mut self.select.state;
        if state
            .query
            .as_ref()
            .is_none_or(|held| held.source != query.source)
        {
            state.facets = None;
        }
        self.select.serial += 1;
        self.select.reread = Reread::Asked;
        let serial = self.select.serial;
        state.query = Some(query.clone());
        state.loading = true;
        state.view_error = None;
        let (owner, client) = (self.owner.clone(), self.client);
        let counted_query = query.clone();
        let evaluated = owner_task(
            move || evaluate_now(&owner, client, &query),
            move |result| Message::Select(SelectMessage::Viewed { serial, result }),
        );
        let owner = self.owner.clone();
        let counted = owner_task(
            move || facets_now(&owner, client, &counted_query),
            move |result| Message::Select(SelectMessage::Faceted { serial, result }),
        );
        Task::batch([evaluated, counted])
    }

    /// Adopt an evaluation that is still the newest: its summary, the session it left, the grid laid
    /// out from its group layout, and a scroll that stays near the active item for the same source
    /// and starts at the top for a new one.
    fn viewed(&mut self, serial: u64, result: Result<Box<(ViewSummary, ClientSession)>, String>) {
        if serial != self.select.serial {
            return;
        }
        self.select.state.loading = false;
        match result {
            Ok(answer) => {
                let (summary, session) = *answer;
                self.adopt(session);
                let state = &mut self.select.state;
                let previous = state.summary.take();
                let same_source = previous
                    .as_ref()
                    .is_some_and(|held| held.query.source == summary.query.source);
                if previous
                    .as_ref()
                    .is_none_or(|held| held.query != summary.query)
                {
                    state.collapsed.clear();
                }
                if let luxforge_core::catalog_types::ViewSource::Folder { path, .. } =
                    &summary.query.source
                {
                    state.folder = Some(path.clone());
                }
                state.rows.reset(summary.revision, summary.count);
                state.query = Some(summary.query.clone());
                let count = model::thousands(summary.count);
                state.summary = Some(summary);
                state.view_error = None;
                let name = model::title(state).name;
                match std::mem::take(&mut self.select.reread) {
                    Reread::Asked => self.status.text = format!("{name} \u{b7} {count} in view"),
                    Reread::Elsewhere => {
                        self.status.text = format!(
                            "{name} changed elsewhere and was read again \u{b7} {count} in view"
                        );
                    }
                    // The status bar says the change itself, from its label.
                    Reread::Own => {}
                }
                self.rebuild_grid();
                if same_source {
                    self.select.scroll = self.near_active(self.select.scroll);
                } else {
                    self.select.scroll = 0.0;
                    self.select.anchor = None;
                }
            }
            Err(error) => {
                let state = &mut self.select.state;
                state.summary = None;
                state.rows.reset(0, 0);
                let error = format!("View unavailable: {error}");
                self.status.text = error.clone();
                state.view_error = Some(error);
                self.rebuild_grid();
                self.select.scroll = 0.0;
                self.select.anchor = None;
            }
        }
    }

    /// Make the grid's blocks from the summary and lay them out at the width the widget reported.
    fn rebuild_grid(&mut self) {
        let state = &mut self.select.state;
        state.content = match &state.summary {
            Some(summary) => model::grid_content(summary, &state.collapsed),
            None => GridContent::default(),
        };
        self.select.layout = GridLayout::new(
            grid_blocks(&state.content),
            metrics(state),
            self.select.viewport.width,
        );
    }

    /// The session's selection in the view on screen.
    fn selection(&self) -> SelectionModel {
        SelectionModel::of(&self.session.browse, self.select.state.revision())
    }

    /// `scroll` moved as little as shows the active item, or clamped to the content.
    fn near_active(&self, scroll: f32) -> f32 {
        let height = self.select.viewport.height;
        match self.selection().active {
            Some(active) => self.select.layout.reveal(active, scroll, height),
            None => self.select.layout.clamp_scroll(scroll, height),
        }
    }

    /// The items of the cells on and one screen either side of the grid's viewport: what the rows
    /// window reads.
    fn wanted_items(&self) -> Range<u32> {
        let layout = &self.select.layout;
        let height = self.select.viewport.height;
        let cells = layout.visible_cells(self.select.scroll, height, height);
        if cells.is_empty() {
            return 0..0;
        }
        let last = layout.cell(cells.end - 1);
        layout.cell(cells.start).item..last.item + last.span
    }

    /// The cell showing `item`, as its first item and span.
    fn cell_of(&self, item: u32) -> Option<(u32, u32)> {
        let layout = &self.select.layout;
        let cell = layout.cell(layout.cell_of_item(item)?);
        Some((cell.item, cell.span))
    }

    /// Where a Shift extension starts: the last click's or arrow key's cell, else the active one.
    fn anchor_cell(&self) -> Option<(u32, u32)> {
        self.select
            .anchor
            .or_else(|| self.selection().active)
            .and_then(|item| self.cell_of(item))
    }

    /// Send one selection gesture to the owner in this update and adopt the session it answers.
    /// Answers whether it was accepted.
    fn select_now(&mut self, gesture: SelectGesture) -> bool {
        let Some(revision) = self.select.state.revision() else {
            return false;
        };
        let params = model::select_params(gesture, Some(revision));
        match select_call(&self.owner, self.client, params) {
            Ok(session) => {
                self.adopt(session);
                true
            }
            Err(error) => {
                self.status.text = format!("Selection failed: {error}");
                false
            }
        }
    }

    /// The gesture a press on a cell is: Shift extends from the anchor, Cmd toggles the cell, and a
    /// plain click selects the cell alone.
    pub(crate) fn press_gesture(&self, press: &GridPress) -> SelectGesture {
        let target = (press.item, press.span);
        let anchor = press.modifiers.shift.then(|| self.anchor_cell()).flatten();
        match anchor {
            Some(anchor) => SelectGesture::Extend { anchor, target },
            None if press.modifiers.command => SelectGesture::Toggle {
                item: press.item,
                span: press.span,
                adding: !self.selection().selected(press.item, press.span),
            },
            None => SelectGesture::Only {
                item: press.item,
                span: press.span,
            },
        }
    }

    /// The gesture an arrow key is, and the cell it makes active: the neighbouring cell of the
    /// active item, or the first cell when none is active. `None` at the grid's edge. With Shift
    /// the selection extends from the anchor.
    pub(crate) fn move_gesture(
        &self,
        step: Step,
        extend: bool,
    ) -> Option<(SelectGesture, (u32, u32))> {
        let layout = &self.select.layout;
        if layout.cell_count() == 0 {
            return None;
        }
        let item = match self.selection().active {
            Some(active) => layout.neighbour(active, direction(step))?,
            None => layout.cell(0).item,
        };
        let target = self.cell_of(item)?;
        let gesture = if extend {
            SelectGesture::Extend {
                anchor: self.anchor_cell().unwrap_or(target),
                target,
            }
        } else {
            SelectGesture::Only {
                item: target.0,
                span: target.1,
            }
        };
        Some((gesture, target))
    }

    /// A click selects its cell, Cmd-click toggles it, Shift-click extends to it.
    fn press(&mut self, press: GridPress) {
        self.select.state.menu = None;
        let gesture = self.press_gesture(&press);
        if !matches!(gesture, SelectGesture::Extend { .. }) {
            self.select.anchor = Some(press.item);
        }
        self.select_now(gesture);
    }

    /// An arrow key moves the active item and scrolls it into view.
    fn move_active(&mut self, step: Step, extend: bool) {
        let Some((gesture, target)) = self.move_gesture(step, extend) else {
            return;
        };
        self.select.anchor = Some(match gesture {
            SelectGesture::Extend { anchor, .. } => anchor.0,
            _ => target.0,
        });
        if self.select_now(gesture) {
            self.select.scroll = self.select.layout.reveal(
                target.0,
                self.select.scroll,
                self.select.viewport.height,
            );
        }
    }

    /// `S` collapses the burst the active item is in to one cell, or expands it again.
    fn collapse(&mut self) {
        let Some(active) = self.selection().active else {
            return;
        };
        let Some(burst) = self.select.state.burst_of(active) else {
            return;
        };
        let collapsed = &mut self.select.state.collapsed;
        if !collapsed.remove(&burst) {
            collapsed.insert(burst);
        }
        self.rebuild_grid();
        self.select.scroll =
            self.select
                .layout
                .reveal(active, self.select.scroll, self.select.viewport.height);
    }

    /// The session read after a wake: adopt it, and evaluate a stale view again with the query it
    /// was evaluated from, reading the events again beside it.
    fn checked(&mut self, result: Result<Box<ClientSession>, String>) -> Task<Message> {
        self.select.check.answered();
        let session = match result {
            Ok(session) => *session,
            Err(error) => {
                self.status.text = format!("Live refresh failed: {error}");
                return Task::none();
            }
        };
        let stale = session.browse.stale;
        self.adopt(session);
        let mut tasks = Vec::new();
        if stale || self.select.state.summary.is_none() {
            tasks.push(self.read_events());
            tasks.push(self.read_counts());
        }
        if stale
            && !self.select.state.loading
            && let Some(query) = self
                .select
                .state
                .summary
                .as_ref()
                .map(|summary| summary.query.clone())
        {
            tasks.push(self.evaluate(query));
            self.select.reread = Reread::Elsewhere;
        }
        Task::batch(tasks)
    }

    // -- Picking and library undo ------------------------------------------------------------------

    /// `P` or the Info panel's Pick: `pick.set` of the selection, picking it or, when the desktop
    /// has read every selected row and each is picked, clearing it ([`model::pick_value`]).
    fn pick_selection(&mut self) -> Task<Message> {
        if self.select.state.summary.is_none() {
            return Task::none();
        }
        if self.select.state.over_catalog() {
            self.status.text =
                "Only files are picked: a developed photograph is already in the catalog".into();
            return Task::none();
        }
        let selection = self.selection();
        if selection.count == 0 {
            self.status.text = "Select a photograph to pick".into();
            return Task::none();
        }
        let picked = model::pick_value(&selection, &self.select.state.rows);
        let params = model::pick_params(&Targets::Selection, picked, &request());
        self.library_now(LibraryGesture::Pick { picked }, params)
    }

    /// A bracket header's Pick all: `pick.set` of its frames' files, as an agent names them.
    fn pick_all(&mut self, number: u32) -> Task<Message> {
        let state = &self.select.state;
        let Some(moment) = state.summary.as_ref().and_then(|summary| {
            let index = state.content.moment(number)?;
            summary.groups.moments.get(index as usize)
        }) else {
            return Task::none();
        };
        let Some(files) = model::frame_files(&state.rows, moment.start, moment.len) else {
            self.status.text = "Reading the frames\u{2026}".into();
            return Task::none();
        };
        let params = model::pick_params(&Targets::Files { file_ids: files }, true, &request());
        self.library_now(LibraryGesture::PickAll, params)
    }

    /// Pick the active frame alone, or clear it when it is picked: `pick.set` naming its file. The
    /// loupe's `P` (TASK-020, the design's P7) calls this, then moves on to
    /// [`model::next_moment`] with `browse.select`; the view is evaluated again as after any pick.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the loupe's P7 (TASK-020) calls it; lane B wires it"
        )
    )]
    pub(crate) fn pick_active(&mut self) -> Task<Message> {
        let Some(active) = self.selection().active else {
            return Task::none();
        };
        let Some(row) = self.select.state.rows.row(active) else {
            self.status.text = "Reading the frame\u{2026}".into();
            return Task::none();
        };
        let RowItem::File { file_id } = row.item else {
            self.status.text =
                "Only files are picked: a developed photograph is already in the catalog".into();
            return Task::none();
        };
        let picked = !row.picked;
        let params = model::pick_params(
            &Targets::Files {
                file_ids: vec![file_id],
            },
            picked,
            &request(),
        );
        self.library_now(LibraryGesture::Pick { picked }, params)
    }

    /// Send one library change synchronously, in this update, as this desktop's actor, and follow
    /// what it recorded. The request and the owner's answer are kept for evidence.
    fn library_now(&mut self, gesture: LibraryGesture, params: Value) -> Task<Message> {
        let method = gesture.method();
        let result = library_call(&self.owner, self.client, method, params.clone());
        self.select.library = Some(json!({
            "method": method,
            "params": params,
            "answer": result.as_ref().ok(),
            "error": result.as_ref().err().map(|error| json!({
                "code": error.code,
                "message": error.message,
                "data": error.data,
            })),
        }));
        match result {
            Ok(answer) => self.library_answered(gesture, &answer),
            Err(error) => {
                self.library_refused(gesture, &error);
                Task::none()
            }
        }
    }

    /// A library change answered. Nothing changed: the status bar says so and nothing is read.
    /// Otherwise the view it made stale is evaluated again (keeping the scroll and the active item),
    /// the events and the catalog's counts are read again, and the change's label is read for the
    /// status bar.
    fn library_answered(
        &mut self,
        gesture: LibraryGesture,
        answer: &LibraryAnswer,
    ) -> Task<Message> {
        let Some(sequence) = answer.change.map(|change| change.0) else {
            self.status.text = gesture.nothing().into();
            return Task::none();
        };
        self.select.change = Some((sequence, gesture));
        self.select.label = Some(sequence);
        let (owner, client) = (self.owner.clone(), self.client);
        let mut tasks = vec![
            owner_task(
                move || label_now(&owner, client, sequence),
                move |result| Message::Select(SelectMessage::Labelled { sequence, result }),
            ),
            self.read_events(),
            self.read_counts(),
        ];
        if let Some(query) = self
            .select
            .state
            .summary
            .as_ref()
            .map(|summary| summary.query.clone())
        {
            tasks.push(self.evaluate(query));
            self.select.reread = Reread::Own;
        }
        Task::batch(tasks)
    }

    /// A library change refused: the status bar says why, naming the first item a refused undo or
    /// redo found changed since. A pick refused because the view went stale reads it again, so the
    /// next `P` acts on what is shown.
    fn library_refused(&mut self, gesture: LibraryGesture, error: &CallError) {
        let conflict = error.code == "conflict";
        self.status.text = conflict
            .then(|| model::refusal_text(gesture, error.data.as_ref()))
            .flatten()
            .unwrap_or_else(|| format!("{}: {}", gesture.refused(), error.message));
        if conflict
            && matches!(
                gesture,
                LibraryGesture::Pick { .. } | LibraryGesture::PickAll
            )
        {
            self.select.check.offer(());
        }
    }

    /// The label of a change this desktop made, read from the journal: the status bar says it with
    /// the key that takes it back, while it is still the newest.
    fn labelled(&mut self, sequence: u64, result: Result<Option<LibraryChange>, String>) {
        if self.select.label == Some(sequence) {
            self.select.label = None;
        }
        let Some((newest, gesture)) = self.select.change else {
            return;
        };
        if newest != sequence {
            return;
        }
        self.status.text = match result {
            Ok(Some(change)) => model::change_text(&change),
            _ => gesture.done().into(),
        };
    }

    /// Read the next block of rows near the screen, while Select is shown.
    fn request_rows(&mut self) -> Task<Message> {
        if !self.select_shown() || self.select.state.summary.is_none() {
            return Task::none();
        }
        let wanted = self.wanted_items();
        let Some(request) = self.select.state.rows.next_request(wanted) else {
            return Task::none();
        };
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || rows_now(&owner, client, &request),
            move |result| {
                Message::Select(SelectMessage::Rows {
                    revision: request.revision,
                    from: request.from,
                    result,
                })
            },
        )
    }

    /// Start the staleness check a wake asked for, while Select is shown.
    /// Take in the cells the grid shows, and read and decode the previews they lack, while Select
    /// shows a view.
    fn want_previews(&mut self) -> Task<Message> {
        if !self.select_shown() || self.select.state.summary.is_none() {
            return Task::none();
        }
        let window = GridWindow {
            layout: &self.select.layout,
            rows: &self.select.state.rows,
            scroll: self.select.scroll,
            height: self.select.viewport.height,
            scale_factor: self.view_state.scale_factor,
        };
        self.select
            .previews
            .want(&self.owner, self.client, window)
            .map(previews_message)
    }

    fn start_check(&mut self) -> Task<Message> {
        if !self.select_shown() || self.select.check.start().is_none() {
            return Task::none();
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || session_now(&owner, client).map(Box::new),
            |result| Message::Select(SelectMessage::Checked(result)),
        )
    }

    /// What the Select workspace shows, for correlated evidence: what it asked for and what the
    /// owner answered, what the grid laid out, the session's selection and what each region says.
    /// No path is recorded: a folder source is named by its kind.
    pub(crate) fn select_summary(&self) -> Value {
        let state = &self.select.state;
        let model = &self.workspace.select;
        let source = state.query.as_ref().map(|query| {
            let mut source = serde_json::to_value(&query.source).unwrap_or_default();
            if let Some(object) = source.as_object_mut() {
                object.remove("path");
            }
            source
        });
        let summary = state.summary.as_ref();
        let blocks = |kind: fn(&Block) -> bool| {
            state
                .content
                .blocks
                .iter()
                .filter(|block| kind(block))
                .count()
        };
        let info = match &model.info {
            model::InfoModel::Nothing => json!({"kind": "nothing"}),
            model::InfoModel::Reading => json!({"kind": "reading"}),
            model::InfoModel::One(item) => json!({
                "kind": "one",
                "name": item.name,
                "pick": item.pick.as_ref().map(|band| json!({
                    "picked": band.picked,
                    "note": band.note,
                })),
                "moment": item.moment,
                "metadata": item.metadata.iter().map(|(label, _)| label).collect::<Vec<_>>(),
            }),
            model::InfoModel::Several { count, active } => {
                json!({"kind": "several", "count": count, "active": active})
            }
        };
        let listed: Vec<Value> = model
            .sources
            .months
            .iter()
            .map(|month| {
                json!({
                    "month": month.label,
                    "events": month.rows.iter().map(|row| &row.name).collect::<Vec<_>>(),
                })
            })
            .collect();
        // The sources panel's cards, volumes and catalog counts, as drawn.
        let row = |row: &model::SourceRow| {
            json!({
                "name": row.name,
                "count": match &row.count {
                    model::Count::None => Value::Null,
                    model::Count::Total(total) => json!(total),
                    model::Count::Picks { picked, total } => json!(format!("{picked}/{total}")),
                    model::Count::Unavailable(count) => json!({"unavailable": count}),
                },
                "dot": row.dot.map(|dot| match dot {
                    model::Dot::Mounted => "mounted",
                    model::Dot::Offline => "offline",
                }),
                "indent": row.indent,
                "open": row.disclosure.as_ref().map(|(open, _)| open),
                "selected": row.selected,
            })
        };
        let panel = &model.sources;
        let sources = json!({
            "cards": panel.cards.iter().map(row).collect::<Vec<_>>(),
            "on_disk": panel.on_disk.iter().map(row).collect::<Vec<_>>(),
            "catalog": panel.catalog.iter().map(row).collect::<Vec<_>>(),
        });
        // Each day's and moment's picks as the owner counted them, and as the grid's headings and
        // moment headers say them.
        let groups_picked = summary.map(|summary| {
            json!({
                "days": summary.groups.days.iter().map(|day| day.picked).collect::<Vec<_>>(),
                "moments": summary
                    .groups
                    .moments
                    .iter()
                    .enumerate()
                    .filter(|(_, moment)| moment.picked > 0)
                    .map(|(index, moment)| json!([index, moment.picked]))
                    .collect::<Vec<_>>(),
            })
        });
        let headers = json!({
            "days": state.content.blocks.iter().filter_map(|block| match block {
                Block::Day { detail, .. } => Some(detail),
                _ => None,
            }).collect::<Vec<_>>(),
            "moments": state.content.blocks.iter().filter_map(|block| match block {
                Block::Moment { index, picked: Some(picked), .. } => Some(json!([index, picked])),
                _ => None,
            }).collect::<Vec<_>>(),
            "pick_all": state.content.blocks.iter().filter_map(|block| match block {
                Block::Moment { index, action: Some(action), .. } => Some(json!([index, action])),
                _ => None,
            }).collect::<Vec<_>>(),
        });
        let facets = state.facets.as_ref().map(|facets| {
            facets
                .counts
                .iter()
                .map(|(facet, values)| (facet.as_str().to_owned(), json!(values.len())))
                .collect::<serde_json::Map<_, _>>()
        });
        json!({
            "shown": state.shown.as_str(),
            "source": source,
            "filter": state.query.as_ref().map(|query| &query.filter),
            "sort": state.query.as_ref().map(|query| query.sort),
            "grouping": state.query.as_ref().map(|query| query.grouping),
            "loading": state.loading,
            "reading_folder": self.select.reading.is_some(),
            "quiet": self.select_quiet(),
            "error": state.view_error,
            "revision": summary.map(|summary| summary.revision),
            "count": summary.map(|summary| summary.count),
            "picked": summary.map(|summary| summary.picked),
            "groups": summary.map(|summary| json!({
                "days": summary.groups.days.len(),
                "cameras": summary.groups.cameras.len(),
                "moments": summary.groups.moments.len(),
            })),
            "blocks": {
                "days": blocks(|block| matches!(block, Block::Day { .. })),
                "cameras": blocks(|block| matches!(block, Block::Camera { .. })),
                "moments": blocks(|block| matches!(block, Block::Moment { .. })),
                "singles": blocks(|block| matches!(block, Block::Singles(_))),
            },
            "labels": state.content.labels.len(),
            "events": state.events.as_ref().map(|list| list.events.len()),
            "listed": listed,
            "facets": facets,
            "rows": state.rows.len(),
            "row_blocks": state.rows.blocks(),
            "cells": self.select.layout.cell_count(),
            "items": self.select.layout.item_count(),
            "content_height": self.select.layout.height(),
            "scroll": self.select.scroll,
            "viewport": [self.select.viewport.width, self.select.viewport.height],
            "selection": self.session.browse.selection,
            "stale": self.session.browse.stale,
            "session_revision": self.session.browse.revision,
            "collapsed": state.collapsed,
            "sources_panel": state.sources_panel,
            "info_panel": state.info_panel,
            "cell_width": state.cell_width(),
            "previews": self.select.previews.summary(),
            "title": {
                "name": model.title.name,
                "summary": model.title.summary,
                "picks": model.title.picks,
            },
            "status_line": model.status.line,
            "note": model.note,
            "info": info,
            "sources": sources,
            "groups_picked": groups_picked,
            "headers": headers,
            "library": self.select.library,
        })
    }
}

/// After every message: read the rows near the screen and start a staleness check a wake asked for.
/// An evidence run also hears when nothing Select asked for is in flight any more.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let rows = editor.request_rows();
    let previews = editor.want_previews();
    let check = editor.start_check();
    if editor.evidence.is_some() && editor.select_shown() && editor.select_quiet() {
        editor.outcome(Outcome::SelectSettled);
    }
    Task::batch([rows, previews, check])
}

fn previews_message(message: SelectPreviewMessage) -> Message {
    Message::Select(SelectMessage::Previews(message))
}

/// The reading folder's job timer, which exists only while a folder browsed on disk is being read.
///
/// And the grid's decoded previews' signal, while Select is shown; a signal posted meanwhile waits
/// for it.
pub(super) fn subscription(editor: &Editor) -> iced::Subscription<Message> {
    let reading = if editor
        .select
        .reading
        .as_ref()
        .is_some_and(|reading| reading.job.is_some())
    {
        iced::time::every(READ_POLL).map(|_| Message::Select(SelectMessage::ReadPoll))
    } else {
        iced::Subscription::none()
    };
    let previews = if editor.select_shown() {
        crate::app::select_previews::subscription().map(previews_message)
    } else {
        iced::Subscription::none()
    };
    iced::Subscription::batch([reading, previews])
}
