//! Opening a photograph and adopting what the owner answers: a command's read-back, the event
//! sync's poll, the displayed entry's recipe rows and module discovery. Every answer is adopted
//! only when it is newer than what the desktop holds, decided from what the answer carries.
use super::short;
use super::tasks::{self, PreviewPayload, Refresh, import_task, merge_current_entry, state_task};
use super::{Editor, message::Message};
use crate::state::fields;
use iced::Task;
use lightwell_core::{ClientSession, HistoryRow, HistorySelection, ModuleDescriptor};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::atomic::Ordering, time::Instant};

/// How many of this desktop's read-back requests the event sync remembers between polls. A poll
/// every 500 ms forgets each one it reads, so this holds the requests of one interval.
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
            Some(history) => self.history = history,
            None => merge_current_entry(
                &mut self.history,
                HistoryRow::from(&refresh.state.current_entry),
            ),
        }
        if let Some(versions) = refresh.versions {
            self.versions = versions;
        }
        match refresh.lineage {
            Some(lineage) => {
                self.lineage = lineage
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
        self.current_recipe = Some(
            refresh
                .current_recipe
                .unwrap_or_else(|| refresh.recipe.clone()),
        );
        self.recipe = Some(refresh.recipe);
        self.masks = Some(refresh.masks);
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
        for module in &modules {
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
