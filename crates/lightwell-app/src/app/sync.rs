//! Opening a photograph and adopting what the owner answers: a command's read-back, the event
//! sync's poll, the displayed entry's recipe rows and module discovery. Every answer is adopted
//! only when it is newer than what the desktop holds, decided from what the answer carries.
use super::{
    Editor,
    message::{Message, SyncMessage},
    preview::short,
    tasks::{
        self, PreviewPayload, Refresh, import_task, merge_current_entry, presets_task, state_task,
        sync_task,
    },
};
use crate::state::fields::{self, Fields};
use iced::Task;
use lightwell_core::{ClientSession, ErrorKind, HistoryRow, HistorySelection, ModuleDescriptor};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::atomic::Ordering, time::Instant};

/// How many of this desktop's read-back requests the event sync remembers between polls. A poll
/// forgets each one it reads, and polls run only when another client changed something, so this
/// holds the requests made since that last happened; one that falls out is read back like another
/// client's change, which costs a refresh the other client's event needs anyway.
pub(super) const OWN_REQUESTS: usize = 64;

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
                if self.picker_open || self.busy || self.evidence.is_some() {
                    return Task::none();
                }
                self.picker_open = true;
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
                self.picker_open = false;
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
                return Task::batch([refreshed, presets_task(self.owner.clone(), self.client)]);
            }
            SyncMessage::Refreshed(result) => {
                // Overtaken by a selection or a state this desktop already holds: whatever
                // overtook it ended the request it answered, and brought its own frame.
                if matches!(&result, Ok(refresh) if self.superseded(refresh)) {
                    return Task::none();
                }
                self.busy = false;
                let mask_command = std::mem::take(&mut self.mask_command_in_flight);
                match result {
                    Ok(refresh) => {
                        if self.activity.pending {
                            self.activity.source_dimensions =
                                Some((refresh.state.asset.width, refresh.state.asset.height));
                            self.activity.orientation = Some(refresh.job.source.orientation());
                        }
                        // A `mask.*` command that **created** a mask names none in its envelope, and
                        // the mask it made has to be the one the panel opens: the adjustments below
                        // the component list are bound to the open mask, so leaving the previous one
                        // open would put the next slider on a mask the person was not looking at.
                        // A drafted create already does this on its own commit; this is the same rule
                        // for a **typed** kind, which is created by its button rather than by a
                        // gesture and so never reaches that path.
                        let created_a_mask = mask_command
                            && self.last_mask_request.as_ref().is_some_and(|(_, request)| {
                                request.get(lightwell_core::MASK_FIELD).is_none()
                            });
                        let before = self.listed_masks();
                        self.accept(*refresh);
                        if created_a_mask {
                            self.open_created_mask(&before);
                        }
                    }
                    Err(error) => {
                        self.status = error.clone();
                        // The command may have landed before its read-back failed, and the owner
                        // wakes no client for its own changes: read the log once to find out.
                        self.resync();
                        // A refused `mask.*` command renders nothing, so the step that sent it has
                        // no pixels to settle on: the refusal itself is what ends it, recorded on
                        // the step with the frame that is on screen as its evidence. Without this
                        // a driven run waits out its whole deadline on a step already answered.
                        if mask_command {
                            self.mask_command_failed(&error);
                        }
                        // Recorded, so a refused request is visible in the evidence log even when
                        // a later frame's status line has replaced it.
                        self.event("command_failed", json!({ "error": error }));
                        // A failed Apply keeps the draft; a stale revision makes it conflicted so
                        // the user chooses Discard or Reapply rather than losing the composition.
                        if self.crop_applying.take().is_some() {
                            let conflict = error.starts_with(ErrorKind::Conflict.code());
                            if conflict && let Some(draft) = self.crop_mut() {
                                draft.mark_conflicted();
                            }
                            if conflict {
                                self.crop_changed("crop_draft_conflicted");
                            }
                        }
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
                    self.recipe_failed = false;
                    // Rows of the current entry, read when a preview returns to it, are also the
                    // rows the section dot follows.
                    if self
                        .state
                        .as_ref()
                        .is_some_and(|state| state.current_entry.id == read.recipe.entry_id)
                    {
                        *self.current_recipe = Some(read.recipe.clone());
                    }
                    *self.recipe = Some(read.recipe);
                    *self.masks = Some(read.masks);
                    self.seed_values();
                }
                Err(error) => {
                    self.recipe_failed = true;
                    self.status = format!("Recipe unavailable: {error}");
                }
            },
            // The poll itself starts once nothing is in flight ([`Editor::sync_when_wanted`]).
            SyncMessage::Changed => self.sync_wanted = true,
            SyncMessage::Synced(result) => {
                self.syncing = false;
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
                            self.sync_wanted = true;
                        } else {
                            self.api_sequence = self.api_sequence.max(sync.sequence);
                            // Read past, so never asked about again.
                            self.own_requests
                                .retain(|request| !sync.own.contains(request));
                        }
                        if sync.capabilities {
                            return self.reload_capabilities();
                        }
                    }
                    Err(error) => self.status = format!("Live refresh failed: {error}"),
                }
            }
            SyncMessage::ModulesLoaded(result) => {
                self.modules_ready = true;
                match result {
                    Ok(modules) => {
                        *self.fields = Fields::seeded(&modules);
                        self.event("modules_loaded", module_summary(&modules));
                        *self.modules = modules;
                        // A photograph that opened before discovery answered already has its
                        // recipe rows: seed the new fields from them.
                        self.seed_values();
                    }
                    Err(error) => {
                        self.status = format!("Tool discovery failed: {error}");
                        self.event("modules_failed", json!({ "message": self.status }));
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
        self.begin_request();
        if let Some(queued) = &queued {
            self.activity.request_started = queued.started;
        }
        let generation = self.activity.requested;
        self.open_generation.store(generation, Ordering::Release);
        // Preserve the last displayed photo, but prevent an older in-flight render from becoming
        // the image for this newer open request.
        self.preview_generation = self.preview_queue.cancel();
        self.busy = true;
        self.status = "Importing photograph…".into();
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.event("open_requested", json!({"file":file}));
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
            json!({"error_code":error_code,"message":message}),
        );
        self.outcome_ready(true);
    }

    /// An open request reached its outcome; evidence mode captures a frame for it.
    pub(super) fn outcome_ready(&mut self, failed: bool) {
        if let Some(evidence) = &mut self.evidence {
            evidence.had_errors |= failed;
            evidence.capture_pending = true;
        }
    }

    /// Keep the newest session the owner has reported; responses may complete out of order.
    pub(crate) fn adopt(&mut self, session: ClientSession) {
        if session.revision >= self.session.revision {
            self.session = session;
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
            || self.state.as_ref().is_some_and(|held| {
                held.asset.id == refresh.state.asset.id && held.revision > refresh.state.revision
            })
    }

    /// The same for a history selection's frame: a newer selection overtook it, or it shows the
    /// current entry and that is not the current entry this desktop holds — a commit was read after
    /// it was planned, or one it saw has not been read yet and brings its own frame when it is.
    pub(super) fn preview_superseded(&self, payload: &PreviewPayload) -> bool {
        payload.session.preview.generation < self.session.preview.generation
            || (payload.session.preview.selection == HistorySelection::Current
                && self
                    .state
                    .as_ref()
                    .is_some_and(|held| held.current_entry.id != payload.job.entry.id))
    }

    /// Start the event sync's one poll when one is wanted and nothing stands in its way: none in
    /// flight, no request of this desktop's in flight — its answer reads its own change back and
    /// names the event the poll then skips — and a photograph open. Called after every message, so
    /// a wake that arrived during a request is read as soon as the request is answered. With
    /// nothing wanted it does nothing: the sync costs nothing until the owner wakes it. An evidence
    /// run has no event sync, so what it records is what its script did.
    pub(crate) fn sync_when_wanted(&mut self) -> Task<Message> {
        if !self.sync_wanted || self.syncing || self.busy || self.evidence.is_some() {
            return Task::none();
        }
        let Some(asset) = self.state.as_ref().map(|state| state.asset.id.clone()) else {
            return Task::none();
        };
        self.sync_wanted = false;
        self.syncing = true;
        let proxy = self.proxy_bounds();
        sync_task(
            self.owner.clone(),
            self.client,
            asset,
            self.api_sequence,
            self.own_requests.iter().cloned().collect(),
            proxy,
        )
    }

    /// An answer to one of this desktop's own changes failed after the request was sent, so the
    /// change may have landed without being read back. The owner wakes no client for its own
    /// events, so the sync is asked for once: the poll reads that event like another client's.
    pub(crate) fn resync(&mut self) {
        self.sync_wanted = true;
    }

    /// This desktop's own request has read its change back onto the screen: the next poll reads
    /// its event and skips it.
    pub(crate) fn read_back(&mut self, request: String) {
        if self.own_requests.len() == OWN_REQUESTS {
            self.own_requests.pop_front();
        }
        self.own_requests.push_back(request);
    }

    pub(crate) fn accept(&mut self, refresh: Refresh) {
        if self.superseded(&refresh) {
            return;
        }
        self.controls_ui.curve_samples.clear();
        self.curve_sample_requested_source.clear();
        if let Some(request) = refresh.request {
            self.read_back(request);
        }
        self.adopt(refresh.session);
        match refresh.history {
            Some(history) => *self.history = history,
            None => merge_current_entry(
                &mut self.history,
                HistoryRow::from(&refresh.state.current_entry),
            ),
        }
        if let Some(versions) = refresh.versions {
            *self.versions = versions;
        }
        match refresh.lineage {
            Some(lineage) => {
                *self.lineage = lineage
                    .steps
                    .iter()
                    .map(|step| step.entry_id.clone())
                    .collect();
                self.lineage_floor = lineage
                    .next_entry_id
                    .as_ref()
                    .and_then(|_| lineage.steps.last().map(|step| step.sequence));
            }
            // This desktop's own commit: its entry's undo parent is the entry that was current,
            // which the loaded lineage already holds, so the chain gains exactly this entry and
            // the floor below a truncated walk stays where it was.
            None => {
                self.lineage.insert(refresh.state.current_entry.id.clone());
            }
        }
        if refresh.original.is_some() {
            self.original_entry = refresh.original;
        }
        *self.current_recipe = Some(
            refresh
                .current_recipe
                .unwrap_or_else(|| refresh.recipe.clone()),
        );
        *self.recipe = Some(refresh.recipe);
        *self.masks = Some(refresh.masks);
        self.recipe_failed = false;
        let revision = refresh.state.revision;
        let entry = refresh.state.current_entry.id.clone();
        if self.state.as_ref().map(|state| &state.asset.id) != Some(&refresh.state.asset.id) {
            self.capabilities_asset_changed(&refresh.state.asset.id);
        }
        self.state = Some(refresh.state);
        self.show_entry(refresh.job.entry.id.clone());
        self.requested_render_entry = Some(refresh.job.entry.clone());
        self.preview_generation = self.request_preview(refresh.job);
        self.status = "Rendering selected history state…".into();
        // Generated fields follow the displayed entry, so a slider shows the authoritative current
        // or historical value of the module's one layer. This reads the values already fetched with
        // the recipe: no extra request, no render.
        self.seed_values();
        self.settle_draft(revision, &entry);
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
        let Some(recipe) = self.recipe.clone() else {
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
                    if self.fields.get(&key.0, &key.1).is_none() {
                        continue;
                    }
                    if self.editing.as_ref() == Some(&key) || self.dragging.as_ref() == Some(&key) {
                        continue;
                    }
                    let reported = values
                        .and_then(|values| values.get(&parameter.name))
                        .and_then(|value| fields::value_text(parameter, value).ok());

                    match reported {
                        Some(text) => self.fields.set(&key.0, &key.1, text),
                        None if action.patch => {
                            self.fields
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

    /// A new authoritative revision arrived while a draft was open. The draft's own Apply ends it;
    /// anything else, including this desktop's undo, redo and restore, marks it conflicted and keeps
    /// it, because no history operation discards a draft implicitly.
    pub(super) fn settle_draft(&mut self, revision: u64, entry: &lightwell_core::EntryId) {
        let Some((base, conflicted, summary)) = self
            .crop()
            .map(|draft| (draft.base_revision, draft.conflicted, draft.summary()))
        else {
            return;
        };
        if let Some(request_id) = self.crop_applying.take() {
            self.end_draft();
            self.event(
                "crop_draft_applied",
                json!({"request_id":request_id,"entry_id":entry.as_str(),"revision":revision,"draft":summary}),
            );
            self.status = format!("Crop applied · entry {}", short(entry.as_str()));
            return;
        }
        if base == revision || conflicted {
            return;
        }
        if let Some(draft) = self.crop_mut() {
            draft.mark_conflicted();
        }
        self.crop_changed("crop_draft_conflicted");
        self.status = "Changed elsewhere: discard the crop draft or reapply it".into();
    }

    /// Every mutation, generated or not, takes the narrowest completion path: the command, one
    /// `asset.state` refresh and one preview job.
    pub(crate) fn command(&mut self, method: impl Into<String>, params: Value) -> Task<Message> {
        let Some(state) = &self.state else {
            return Task::none();
        };
        if self.busy {
            return Task::none();
        }
        let method = method.into();
        let asset = state.asset.id.clone();
        self.busy = true;
        self.status = format!("Running {method}…");
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
