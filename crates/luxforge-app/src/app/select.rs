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
//!
//! Which workspace is shown, the panels, the collapsed bursts, the size slider and the selection's
//! anchor are this desktop's own view state, like the developer gallery page: no other client sees
//! them.
use crate::app::{
    Before, Editor,
    gesture::Starting,
    message::{
        Message,
        select::{SelectMessage, Step},
    },
    tasks::{call, owner_task},
};
use crate::coalesce::Coalesce;
use crate::state::select::{
    self as model, Block, GridContent, SelectGesture, SelectPanel, SelectState, SelectionModel,
    Shown,
};
use iced::{Size, Task};
use luxforge_core::{
    ClientSession,
    catalog_types::{EventList, Facets, ViewQuery, ViewRows, ViewSummary},
};
use luxforge_ui::{
    GridBlock, GridDirection, GridHeading, GridLayout, GridMetrics, GridPress, MomentHeader,
    MomentKind,
};
use serde_json::{Value, json};
use std::{ops::Range, path::PathBuf};

/// The Select workspace's own state in the editor: its view-model state, the grid's layout, scroll
/// and viewport, and what is in flight.
#[derive(Clone, Debug)]
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
}

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
                frames,
                collapsed,
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
                    // Per-moment pick counts and Pick all come with picks.
                    picked: None,
                    action: None,
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
                    self.select.state.folder = Some(path.clone());
                    return self.evaluate(model::source_query(
                        luxforge_core::catalog_types::ViewSource::Folder {
                            path,
                            subfolders: true,
                        },
                    ));
                }
            }
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
        }
        Task::none()
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
            return Task::none();
        }
        if let Some(reason) = self.gesture_refusal(Starting::Workspace) {
            self.status.text = reason;
            return Task::none();
        }
        self.palette.open = false;
        self.view_state.menu = None;
        self.select.state.shown = Shown::Select;
        if std::mem::take(&mut self.select.check_on_show) {
            self.select.check.offer(());
        }
        if !std::mem::replace(&mut self.select.events_read, true) {
            return self.read_events();
        }
        Task::none()
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
            move || {
                let (list, _) = call(&owner, client, "event.list", model::events_params(&search))?;
                parse::<EventList>(list)
            },
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
        let serial = self.select.serial;
        let view = model::view_params(&query);
        let facets = model::facets_params(&query);
        state.query = Some(query);
        state.loading = true;
        state.view_error = None;
        let (owner, client) = (self.owner.clone(), self.client);
        let evaluated = owner_task(
            move || {
                let (summary, _) = call(&owner, client, "browse.view", view)?;
                let summary = parse::<ViewSummary>(summary)?;
                let (session, _) = call(&owner, client, "session.state", json!({}))?;
                Ok(Box::new((summary, parse::<ClientSession>(session)?)))
            },
            move |result| Message::Select(SelectMessage::Viewed { serial, result }),
        );
        let owner = self.owner.clone();
        let counted = owner_task(
            move || {
                let (counts, _) = call(&owner, client, "browse.facets", facets)?;
                parse::<Facets>(counts)
            },
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
                state.summary = Some(summary);
                state.view_error = None;
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
        match call(&self.owner, self.client, "browse.select", params)
            .and_then(|(session, _)| parse::<ClientSession>(session))
        {
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
        }
        Task::batch(tasks)
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
            move || {
                let (rows, _) = call(&owner, client, "browse.rows", model::rows_params(&request))?;
                parse::<ViewRows>(rows)
            },
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
    fn start_check(&mut self) -> Task<Message> {
        if !self.select_shown() || self.select.check.start().is_none() {
            return Task::none();
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let (session, _) = call(&owner, client, "session.state", json!({}))?;
                parse::<ClientSession>(session).map(Box::new)
            },
            |result| Message::Select(SelectMessage::Checked(result)),
        )
    }

    /// What the Select workspace shows, for correlated evidence. No path is recorded: a folder
    /// source is named by its kind.
    pub(crate) fn select_summary(&self) -> Value {
        let state = &self.select.state;
        let source = state.query.as_ref().map(|query| {
            let mut source = serde_json::to_value(&query.source).unwrap_or_default();
            if let Some(object) = source.as_object_mut() {
                object.remove("path");
            }
            source
        });
        let summary = state.summary.as_ref();
        json!({
            "shown": state.shown.as_str(),
            "source": source,
            "filter": state.query.as_ref().map(|query| &query.filter),
            "sort": state.query.as_ref().map(|query| query.sort),
            "grouping": state.query.as_ref().map(|query| query.grouping),
            "loading": state.loading,
            "error": state.view_error,
            "revision": summary.map(|summary| summary.revision),
            "count": summary.map(|summary| summary.count),
            "picked": summary.map(|summary| summary.picked),
            "events": state.events.as_ref().map(|list| list.events.len()),
            "rows": state.rows.len(),
            "row_blocks": state.rows.blocks(),
            "cells": self.select.layout.cell_count(),
            "content_height": self.select.layout.height(),
            "scroll": self.select.scroll,
            "viewport": [self.select.viewport.width, self.select.viewport.height],
            "selection": self.session.browse.selection,
            "stale": self.session.browse.stale,
            "collapsed": state.collapsed,
            "sources_panel": state.sources_panel,
            "info_panel": state.info_panel,
            "cell_width": state.cell_width(),
        })
    }
}

/// After every message: read the rows near the screen and start a staleness check a wake asked for.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let rows = editor.request_rows();
    Task::batch([rows, editor.start_check()])
}
