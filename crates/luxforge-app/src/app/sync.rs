//! Opening a photograph and adopting what the owner answers: a command's read-back, the event
//! sync's poll, the displayed entry's recipe rows and module discovery. Every answer is adopted
//! only when it is newer than what the desktop holds, decided from what the answer carries.
use super::{
    Editor,
    message::{Message, sync::SyncMessage},
    tasks::{
        self, PreviewPayload, Refresh, import_task, merge_current_entry, presets_task, state_task,
        sync_task,
    },
};
use crate::app::{Before, outcome::Outcome, waker};
use crate::coalesce::Coalesce;
use crate::state::{
    self,
    fields::{self, Fields},
};
use iced::{Subscription, Task};
use luxforge_core::{ClientSession, HistoryRow, ModuleDescriptor};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::atomic::Ordering, time::Instant};

/// How many of this desktop's read-back requests the event sync remembers between polls. A poll
/// forgets each one it reads, and polls run only when another client changed something, so this
/// holds the requests made since that last happened; one that falls out is read back like another
/// client's change, which costs a refresh the other client's event needs anyway.
pub(super) const OWN_REQUESTS: usize = 64;

/// The event sync's own state: its one poll, the cursor it reads events from, this desktop's own
/// requests it skips, and the canvas mode a draft's start or end still has to tell the session.
#[derive(Debug, Default)]
pub(crate) struct EventSync {
    /// One poll in flight. A wake while one is out waits for it: the owner said another client
    /// changed something, or an answer of this desktop's own left it unknown whether its change
    /// landed, and the poll starts once nothing is in flight.
    pub(crate) poll: Coalesce<()>,
    /// The cursor: the newest event sequence a poll has read up to. Only a poll moves it, and never
    /// backwards; the sequence any other answer carries counts events of other clients' that no
    /// poll has read yet.
    pub(crate) sequence: u64,
    /// This desktop's own requests whose answers read their changes back and reached the screen,
    /// oldest first and at most [`OWN_REQUESTS`]. A poll reads their events and skips them; one
    /// that falls out of the bound is read back like another client's, which costs a refresh and
    /// loses nothing.
    pub(crate) own_requests: std::collections::VecDeque<String>,
    /// Set when a draft just started or ended by a route that does not already ask the session
    /// itself: the next `update` call folds in one `workspace.set` for this mode, unless the
    /// session already reports it, so the mode strip shows Crop selected during every draft
    /// however it was opened, and pointer again however it ended.
    pub(crate) mode: Option<String>,
}

/// Module identity for correlated evidence; descriptors carry no source paths.
pub(super) fn module_summary(modules: &[ModuleDescriptor]) -> Value {
    Value::Array(
        modules
            .iter()
            .map(|module| {
                json!({"id":module.id,"available":module.is_available(),"actions":module.actions.iter().map(|action| action.id.clone()).collect::<Vec<_>>()})
            })
            .collect(),
    )
}

impl Editor {
    /// One message about opening a photograph or an owner answer the editor adopts.
    pub(super) fn sync_update(&mut self, message: SyncMessage) -> Task<Message> {
        match message {
            SyncMessage::Open => {
                if self.view_state.picker_open || self.busy || self.evidence.is_some() {
                    return Task::none();
                }
                self.view_state.picker_open = true;
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .add_filter(
                                "Photos",
                                &[
                                    "jpg", "jpeg", "nef", "raf", "dng", "arw", "cr2", "cr3", "nrw",
                                    "rw2", "orf", "pef",
                                ],
                            )
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    |value| Message::Sync(SyncMessage::Picked(value)),
                );
            }
            SyncMessage::Picked(path) => {
                self.view_state.picker_open = false;
                if let Some(path) = path {
                    return self.open(path);
                }
            }
            SyncMessage::ImportRefreshed(generation, result) => {
                if self.open_generation.load(Ordering::Acquire) != generation {
                    return Task::none();
                }
                // The event sync polls only while a photograph is open, so a library change
                // another client made while none was reaches the screen with the photo rather
                // than a poll later: list the library again beside it.
                let opened = result.is_ok();
                let refreshed = self.dispatch(Message::Sync(SyncMessage::Refreshed(result)));
                if !opened {
                    return refreshed;
                }
                // An open is what happened even when it is the photograph already on screen.
                if let Some(state) = &self.document.state {
                    self.status.happened = Some(state::status::Happened::opened(state));
                }
                return Task::batch([refreshed, presets_task(self.owner.clone(), self.client)]);
            }
            SyncMessage::Refreshed(result) => {
                // Overtaken by a selection or a state this desktop already holds: whatever
                // overtook it ended the request it answered, and brought its own frame.
                if matches!(&result, Ok(refresh) if self.superseded(refresh)) {
                    return Task::none();
                }
                self.busy = false;
                let mask_command = std::mem::take(&mut self.mask_panel.command_in_flight);
                match result {
                    Ok(refresh) => {
                        if self.activity.pending {
                            self.activity.source_dimensions =
                                Some((refresh.state.asset.width, refresh.state.asset.height));
                            self.activity.orientation =
                                Some(refresh.job.evaluation.source().orientation());
                        }
                        // A `mask.*` command that **created** a mask names none in its envelope, and
                        // the mask it made has to be the one the panel opens: the adjustments below
                        // the component list are bound to the open mask, so leaving the previous one
                        // open would put the next slider on a mask the person was not looking at.
                        // A drafted create already does this on its own commit; this is the same rule
                        // for a **typed** kind, which is created by its button rather than by a
                        // gesture and so never reaches that path.
                        let created_a_mask =
                            mask_command
                                && self.mask_panel.last_request.as_ref().is_some_and(
                                    |(_, request)| request.get(luxforge_core::MASK_FIELD).is_none(),
                                );
                        let before = self.listed_masks();
                        self.accept(*refresh);
                        if created_a_mask {
                            self.open_created_mask(&before);
                        }
                    }
                    Err(error) => {
                        self.status.text = error.clone();
                        // The command may have landed before its read-back failed, and the owner
                        // wakes no client for its own changes: read the log once to find out.
                        self.resync();
                        // A refused `mask.*` command renders nothing, so the step that sent it has
                        // no pixels to settle on: the refusal itself is what ends it, recorded on
                        // the step with the frame that is on screen as its evidence. Without this
                        // a driven run waits out its whole deadline on a step already answered.
                        if mask_command {
                            self.outcome(Outcome::MaskCommandFailed(&error));
                        }
                        // Recorded, so a refused request is visible in the evidence log even when
                        // a later frame's status line has replaced it.
                        self.event("command_failed", || json!({ "error": error }));
                        if self.activity.pending {
                            let (code, message) =
                                error.split_once(": ").unwrap_or(("internal", &error));
                            self.open_failed(code, message);
                        }
                    }
                }
            }
            SyncMessage::RecipeDescribed(result) => match result {
                Ok(read) => {
                    let read = *read;
                    self.document.recipe_failed = false;
                    // Rows of the current entry, read when a preview returns to it, are also the
                    // rows the section dot follows.
                    if self
                        .document
                        .state
                        .as_ref()
                        .is_some_and(|state| state.current_entry.id == read.recipe.entry_id)
                    {
                        self.document.current_recipe = Some(read.recipe.clone());
                    }
                    self.document.recipe = Some(read.recipe);
                    self.document.masks = Some(read.masks);
                    self.seed_values();
                }
                Err(error) => {
                    self.document.recipe_failed = true;
                    self.status.text = format!("Recipe unavailable: {error}");
                }
            },
            // The poll itself starts once nothing is in flight ([`Editor::sync_when_wanted`]).
            SyncMessage::Changed => {
                self.sync.poll.offer(());
            }
            SyncMessage::Synced(result) => {
                self.sync.poll.answered();
                match result {
                    Ok(sync) => {
                        // Another client's preset change reaches the library in the same poll that
                        // brings its asset changes, and costs no asset refresh of its own; a
                        // capability event re-reads the modules and renders nothing.
                        if let Some((presets, sequence)) = sync.presets {
                            self.adopt_presets(presets, sequence);
                        }
                        // A poll whose state read was overtaken leaves the sequence where it was,
                        // so the next poll reads the events it answered again rather than
                        // skipping a change that never reached the screen.
                        let superseded = sync
                            .refresh
                            .as_ref()
                            .is_some_and(|refresh| self.superseded(refresh));
                        if let Some(refresh) = sync.refresh.filter(|_| !superseded) {
                            self.accept(*refresh);
                        }
                        if superseded {
                            // Nothing wakes the sync on a timer, so it reads those events again
                            // itself, once what overtook it has landed.
                            self.sync.poll.offer(());
                        } else {
                            self.sync.sequence = self.sync.sequence.max(sync.sequence);
                            // Read past, so never asked about again.
                            self.sync
                                .own_requests
                                .retain(|request| !sync.own.contains(request));
                        }
                        if sync.capabilities {
                            return self.reload_capabilities();
                        }
                    }
                    Err(error) => self.status.text = format!("Live refresh failed: {error}"),
                }
            }
            SyncMessage::ModulesLoaded(result) => {
                self.modules_ready = true;
                match result {
                    Ok(modules) => {
                        self.controls.fields = Fields::seeded(&modules);
                        self.event("modules_loaded", || module_summary(&modules));
                        self.modules = modules;
                        // A photograph that opened before discovery answered already has its
                        // recipe rows: seed the new fields from them.
                        self.seed_values();
                    }
                    Err(error) => {
                        self.status.text = format!("Tool discovery failed: {error}");
                        self.event("modules_failed", || json!({ "message": self.status.text }));
                    }
                }
            }
        }
        Task::none()
    }

    /// One request whose outcome a frame is captured for: the next generation is pending until its
    /// pixels are on screen or it fails.
    pub(crate) fn begin_request(&mut self) {
        self.activity.requested += 1;
        self.activity.pending = true;
        self.activity.phase = "loading";
        self.activity.error_code = None;
        self.activity.request_started = Instant::now();
    }

    /// Import a file through the same API call the Open button uses, tracked as one open request.
    pub(super) fn open(&mut self, path: PathBuf) -> Task<Message> {
        self.open_queued(path, None)
    }

    pub(super) fn open_queued(
        &mut self,
        path: PathBuf,
        queued: Option<tasks::StartupImport>,
    ) -> Task<Message> {
        if let Some(reason) = self.mask_tool_refusal() {
            self.status.text = reason;
            return Task::none();
        }
        self.begin_request();
        if let Some(queued) = &queued {
            self.activity.request_started = queued.started;
        }
        let generation = self.activity.requested;
        self.open_generation.store(generation, Ordering::Release);
        // Preserve the last displayed photo, but prevent an older in-flight render from becoming
        // the image for this newer open request.
        self.presentation.preview_generation = self.cancel_preview_queue();
        self.busy = true;
        self.status.text = "Importing photograph…".into();
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.event("open_requested", || json!({"file":file}));
        let proxy = self.proxy_bounds();
        import_task(
            self.owner.clone(),
            self.client,
            path,
            generation,
            self.open_generation.clone(),
            proxy,
            queued.map(|queued| queued.result),
        )
    }

    pub(super) fn open_failed(&mut self, error_code: &str, message: &str) {
        self.activity.pending = false;
        self.activity.phase = "error";
        self.activity.error_code = Some(error_code.into());
        self.event(
            "open_failed",
            || json!({"error_code":error_code,"message":message}),
        );
        self.outcome(Outcome::RequestEnded { failed: true });
    }

    /// Keep the newest session the owner has reported; responses may complete out of order.
    pub(crate) fn adopt(&mut self, session: ClientSession) {
        if session.revision >= self.session.revision {
            self.session = session;
            if self.presentation.compare_after.is_some()
                && self.session.preview.comparison.is_none()
            {
                self.presentation.compare_after = None;
                self.document.compare_return = None;
                self.document.compare_hold = false;
            }
        }
    }

    /// A refresh read before something this desktop already holds: a history selection made after
    /// it read the session, whose preview generation is then behind the one held, or a newer state
    /// of the same asset. Its frame and panels describe what the screen has since moved on from, so
    /// it is dropped whole; whatever overtook it brought its own. This is the currency check, made
    /// from what the answer carries, with no request of its own: the preview queue's generation then
    /// keeps any frame requested earlier from following a newer one on screen.
    pub(super) fn superseded(&self, refresh: &Refresh) -> bool {
        refresh.session.preview.generation < self.session.preview.generation
            || self.document.state.as_ref().is_some_and(|held| {
                held.asset.id == refresh.state.asset.id && held.revision > refresh.state.revision
            })
    }

    /// The same for a history selection's frame: a newer selection overtook it, or it shows the
    /// current entry and that is not the current entry this desktop holds — a commit was read after
    /// it was planned, or one it saw has not been read yet and brings its own frame when it is.
    pub(super) fn preview_superseded(&self, payload: &PreviewPayload) -> bool {
        payload.session.preview.generation < self.session.preview.generation
            || (payload
                .session
                .preview
                .can_edit(&payload.job.evaluation.entry().asset_id)
                && self
                    .document
                    .state
                    .as_ref()
                    .is_some_and(|held| held.current_entry.id != payload.job.evaluation.entry().id))
    }

    /// Start the event sync's one poll when one is wanted and nothing stands in its way: none in
    /// flight, no request of this desktop's in flight — its answer reads its own change back and
    /// names the event the poll then skips — and a photograph open. Called after every message, so
    /// a wake that arrived during a request is read as soon as the request is answered. With
    /// nothing wanted it does nothing: the sync costs nothing until the owner wakes it. An evidence
    /// run syncs exactly as a session does, which is how its `agent` step's change reaches the
    /// screen.
    pub(crate) fn sync_when_wanted(&mut self) -> Task<Message> {
        if self.busy {
            return Task::none();
        }
        let Some(held) = self
            .document
            .state
            .as_ref()
            .map(|state| (state.asset.id.clone(), state.revision))
        else {
            return Task::none();
        };
        if self.sync.poll.start().is_none() {
            return Task::none();
        }
        let proxy = self.proxy_bounds();
        sync_task(
            self.owner.clone(),
            self.client,
            held,
            self.sync.sequence,
            self.sync.own_requests.iter().cloned().collect(),
            proxy,
        )
    }

    /// An answer to one of this desktop's own changes failed after the request was sent, so the
    /// change may have landed without being read back. The owner wakes no client for its own
    /// events, so the sync is asked for once: the poll reads that event like another client's.
    pub(crate) fn resync(&mut self) {
        self.sync.poll.offer(());
    }

    /// This desktop's own request has read its change back onto the screen: the next poll reads
    /// its event and skips it.
    pub(crate) fn read_back(&mut self, request: String) {
        if self.sync.own_requests.len() == OWN_REQUESTS {
            self.sync.own_requests.pop_front();
        }
        self.sync.own_requests.push_back(request);
    }

    pub(crate) fn accept(&mut self, refresh: Refresh) {
        if self.superseded(&refresh) {
            return;
        }
        self.controls.ui.clear_curve_samples();
        self.curve_sampling.requested_source.clear();
        // What happened is read against the state and the history rows held before this one: a
        // current entry the rows already held is a redo rather than a new entry.
        let known = self
            .document
            .history
            .entries
            .iter()
            .any(|row| row.id == refresh.state.current_entry.id);
        let happened =
            state::status::Happened::between(self.document.state.as_ref(), &refresh.state, known);
        // A composite that skipped settings says so beside what it did, and one that applied
        // nothing at all says that, since no entry moved to say anything else.
        self.status.skipped = (!refresh.skipped.is_empty()).then(|| {
            state::status::skipped(refresh.skipped.len(), refresh.state.asset.source.tag())
        });
        if let Some(happened) = happened {
            self.status.happened = Some(happened);
        } else if self.status.skipped.is_some() {
            self.status.happened = Some(state::status::Happened::NothingApplied);
        }
        if let Some(request) = refresh.request {
            self.read_back(request);
        }
        self.adopt(refresh.session);
        match refresh.history {
            Some(history) => self.document.history = history,
            None => merge_current_entry(
                &mut self.document.history,
                HistoryRow::from(&refresh.state.current_entry),
            ),
        }
        if let Some(versions) = refresh.versions {
            self.document.versions = versions;
        }
        match refresh.lineage {
            Some(lineage) => {
                self.document.lineage = lineage
                    .steps
                    .iter()
                    .map(|step| step.entry_id.clone())
                    .collect();
                self.document.lineage_floor = lineage
                    .next_entry_id
                    .as_ref()
                    .and_then(|_| lineage.steps.last().map(|step| step.sequence));
            }
            // This desktop's own commit: its entry's undo parent is the entry that was current,
            // which the loaded lineage already holds, so the chain gains exactly this entry and
            // the floor below a truncated walk stays where it was.
            None => {
                self.document
                    .lineage
                    .insert(refresh.state.current_entry.id.clone());
            }
        }
        if refresh.original.is_some() {
            self.document.original_entry = refresh.original;
        }
        self.document.current_recipe = Some(
            refresh
                .current_recipe
                .unwrap_or_else(|| refresh.recipe.clone()),
        );
        self.document.recipe = Some(refresh.recipe);
        self.document.masks = Some(refresh.masks);
        self.document.recipe_failed = false;
        let revision = refresh.state.revision;
        if self.document.state.as_ref().map(|state| &state.asset.id)
            != Some(&refresh.state.asset.id)
        {
            self.capabilities_asset_changed(&refresh.state.asset.id);
        }
        self.document.state = Some(refresh.state);
        self.show_entry(refresh.job.evaluation.entry().id.clone());
        self.outcome(Outcome::EntryRequested(refresh.job.evaluation.entry()));
        self.presentation.preview_generation = self.request_preview(refresh.job);
        self.status.text = "Rendering selected history state…".into();
        // Generated fields follow the displayed entry, so a slider shows the authoritative current
        // or historical value of the module's one layer. This reads the values already fetched with
        // the recipe: no extra request, no render.
        self.seed_values();
        self.gesture_revision(revision);
    }

    /// Seed every generated field of every module from the displayed entry's values for that
    /// module's one layer, leaving the field being typed or dragged exactly as it is.
    ///
    /// A **patch** action's fields mirror one persistent layer: that is what a patch is, a merge
    /// into the state the module already holds. So they follow that layer wherever it goes, and a
    /// module without one shows its declared defaults. Every other action's fields are request
    /// inputs, not a mirror: they take a reported value when the layer offers one and are never
    /// reset by a refresh, so a typed coordinate survives somebody else's edit.
    pub(crate) fn seed_values(&mut self) {
        let Some(recipe) = self.document.recipe.clone() else {
            return;
        };
        // The target the generated sections are bound to. The global layer and each mask are
        // distinct targets of the same module, so seeding filters by it: without that, a stack
        // holding both a global Basic layer and a masked one would look like "two layers of that
        // module" and nothing would be seeded at all.
        let target = self.section_target().cloned();
        let modules = std::mem::take(&mut self.modules);
        for module in modules.iter() {
            let mut layers = recipe
                .layers
                .iter()
                .filter(|layer| layer.module.as_deref() == Some(module.id.as_str()))
                .filter(|layer| layer.mask.as_ref() == target.as_ref());
            // "The one layer of that module": a stack holding two of them says nothing about which
            // one the controls represent, so nothing is seeded rather than guessing.
            let values = match (layers.next(), layers.next()) {
                (Some(layer), None) => Some(&layer.values),
                (Some(_), Some(_)) => continue,
                (None, _) => None,
            };
            for action in &module.actions {
                for parameter in &action.parameters {
                    let key = (action.id.clone(), parameter.name.clone());
                    if self.controls.fields.get(&key.0, &key.1).is_none() {
                        continue;
                    }
                    if self.controls.editing.as_ref() == Some(&key)
                        || self.controls.dragging.as_ref() == Some(&key)
                    {
                        continue;
                    }
                    let reported = values
                        .and_then(|values| values.get(&parameter.name))
                        .and_then(|value| fields::value_text(parameter, value).ok());

                    match reported {
                        Some(text) => self.controls.fields.set(&key.0, &key.1, text),
                        None if action.patch => {
                            self.controls
                                .fields
                                .set(&key.0, &key.1, fields::seed_text(parameter));
                        }
                        None => {}
                    }
                }
            }
        }
        self.modules = modules;
        self.seed_mask_fields();
    }

    /// Every mutation, generated or not, takes the narrowest completion path: the command, one
    /// `asset.state` refresh and one preview job.
    pub(crate) fn command(&mut self, method: impl Into<String>, params: Value) -> Task<Message> {
        let Some(state) = &self.document.state else {
            return Task::none();
        };
        if self.busy {
            return Task::none();
        }
        let method = method.into();
        let asset = state.asset.id.clone();
        self.busy = true;
        self.status.text = format!("Running {method}…");
        let proxy = self.proxy_bounds();
        state_task(
            self.owner.clone(),
            self.client,
            asset,
            method,
            params,
            proxy,
        )
    }
}

/// After every message: a wake that arrived while a request was in flight is read once it has
/// been answered, and a draft's start or end tells the session its mode.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let synced = editor.sync_when_wanted();
    Task::batch([synced, editor.sync_mode()])
}

/// The owner's wake for another client's change. The event sync needs no timer: the owner posts a
/// signal when another client's change reaches its log, and this carries it in as a `Changed`. An
/// open photograph with nothing happening to it wakes nothing, and a signal posted while no
/// photograph is open is buffered and read once one is. An evidence run is woken the same way.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    if editor.document.state.is_none() {
        return Subscription::none();
    }
    waker::events_subscription()
}
