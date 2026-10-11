//! HDR merges in the Select workspace ([HDR merge design](../../../../docs/design/hdr-merge.md),
//! "Grid workflow"): a bracket's Pick merged and Import merged, the same for a selection of frames
//! through the grid's menu, Merge options…, the one pending merge row, the result's stack and its
//! originals, and Remove merge and Put back merge. Its model is `state/select_merge.rs`; the grid
//! draws the pending row, the badges and the notes, and `view/select_merge.rs` the menu, the form
//! and the notice.
//!
//! The desktop holds no merge logic: every gesture sends the request an API client sends, and
//! every refusal, progress and result shown is the owner's.
//!
//! - **Starting a merge** is one owner task off the update loop, since planning stats each frame
//!   on disk: `merge.plan` of the frames by identity (a header's files, or the selection's rows as
//!   they are held; the selection itself when a row is not held or is no file), `folder.create`
//!   of the plan's suggested folder when it proposes none and the person chose none, then
//!   `merge.create` with the plan's identity, as this desktop's actor. A plan that refuses is
//!   said in its own words; an exact merge in Removed is offered as Put back merge. Defaults ask
//!   for nothing more: Merge options… only changes what is sent.
//! - **While the job runs** its `job.wait` reader ([`super::long_work`]) brings each record:
//!   the pending row before the frames says how far it got and offers Cancel (`job.cancel`, the
//!   activity board's Cancel too). Its end is the job's: `ready` reads the view again with the
//!   result selected and active, leading its stack collapsed; failed says the input or stage
//!   that failed in the row until it is dismissed; cancelled says nothing was added. Nothing is
//!   hidden before the owner publishes.
//! - **The stack** is the view's: `S`, a press on the result's badge and the menu send
//!   `stack.set-expanded` in an owner task, which evaluates the view again with the stack shown as
//!   asked, answered as any evaluation is. Picking an original is an ordinary pick.
//! - **Remove merge and Put back merge** are library changes (`merge.remove`, `merge.restore`)
//!   sent synchronously like every library gesture of Select, which `Cmd+Z` undoes.
//! - **Pick merged** adds the result to the owner's development set as it is published; Develop
//!   follows that set from then on, so `D` shows it beside the set's other photographs.
use crate::app::{
    Editor,
    message::{Message, select::SelectMessage, select_merge::MergeMessage},
    select::{evaluate_now, session_now},
    tasks::{CallError, call, call_detailed, owner_task, request as mutation},
};
use crate::state::select::{Block, LibraryGesture, MomentAction};
use crate::state::select_merge::{
    self as model, FolderPick, MergeAction, MergeForm, MergeMenu, MergeMenuKind, MergeMode,
    MergeOptions, MergeRequest, PendingMerge, PutBack, Started,
};
use iced::Task;
use luxforge_core::{
    ClientId, ClientSession, OwnerHandle,
    catalog_types::{
        ItemRef, MergeAnswer, MergePlan, MergeState, RowItem, StackExpansion, StackRole, Targets,
        ViewFilter, ViewSummary,
    },
};
use serde_json::{Value, json};

fn merge_message(message: MergeMessage) -> Message {
    Message::Select(SelectMessage::Merge(message))
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// What a merge's start came to, and every request it sent in order, for evidence.
#[derive(Clone, Debug)]
pub(crate) struct StartAnswer {
    pub(crate) started: Started,
    pub(crate) sent: Vec<Value>,
}

// -- The owner calls, each the body of one owner task. --

/// `merge.plan` for the form.
pub(crate) fn plan_now(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<Box<MergePlan>, String> {
    let (plan, _) = call(owner, client, "merge.plan", params)?;
    parse(plan).map(Box::new)
}

/// Start a merge of `request`: plan it, make the folder it suggests when it needs one, and create
/// it. Each request is sent as an API client writes it.
pub(crate) fn start_now(
    owner: &OwnerHandle,
    client: ClientId,
    request: &MergeRequest,
) -> Result<Box<StartAnswer>, String> {
    let mut sent = Vec::new();
    let params = model::plan_params(&request.targets, &request.options);
    sent.push(json!({"method": "merge.plan", "params": params}));
    let plan = match call_detailed(owner, client, "merge.plan", params) {
        Ok(plan) => parse::<MergePlan>(plan)?,
        // A selection the core cannot plan (a provenance row, a count it refuses) says why.
        Err(error) => {
            return Ok(Box::new(StartAnswer {
                started: Started::Refused(error.message),
                sent,
            }));
        }
    };
    let answer = |started| {
        Ok(Box::new(StartAnswer {
            started,
            sent: sent.clone(),
        }))
    };
    if let Some(reason) = model::plan_refusal(&plan) {
        return answer(Started::Refused(reason));
    }
    if let Some(existing) = plan
        .existing
        .as_ref()
        .filter(|existing| existing.state == MergeState::Removed)
    {
        return answer(Started::Removed {
            merge_id: existing.merge_id.clone(),
            result_asset_id: existing.result_asset_id.clone(),
        });
    }
    let folder = match (&request.options.folder, &plan.folder) {
        (FolderPick::Existing { folder_id, .. }, _) => folder_id.clone(),
        (FolderPick::Proposed, Some(proposal)) => proposal.folder_id.clone(),
        (FolderPick::Proposed, None) => {
            let Some(params) = model::suggested_folder_params(&plan, &mutation()) else {
                return answer(Started::Refused(plan.folder_reason.clone().unwrap_or_else(
                    || "Choose a catalog folder for the merge in Merge options\u{2026}".to_owned(),
                )));
            };
            sent.push(json!({"method": "folder.create", "params": params}));
            let created = call_detailed(owner, client, "folder.create", params)
                .map_err(|error| format!("Could not make the folder: {}", error.message))?;
            parse(created["folder"]["id"].clone())?
        }
    };
    let params = model::create_params(request, &plan.plan_id, &folder, &mutation());
    sent.push(json!({"method": "merge.create", "params": params}));
    match call_detailed(owner, client, "merge.create", params) {
        Ok(created) => {
            let job_id = created["job_id"]
                .as_str()
                .ok_or_else(|| format!("merge.create answered no job: {created}"))?
                .to_owned();
            Ok(Box::new(StartAnswer {
                started: Started::Job { job_id },
                sent,
            }))
        }
        Err(CallError {
            data: Some(data), ..
        }) if data.get("put_back").is_some() => {
            let put_back = &data["put_back"];
            Ok(Box::new(StartAnswer {
                started: Started::Removed {
                    merge_id: parse(put_back["merge_id"].clone())?,
                    result_asset_id: parse(put_back["result_asset_id"].clone())?,
                },
                sent,
            }))
        }
        Err(error) => Ok(Box::new(StartAnswer {
            started: Started::Refused(error.message),
            sent,
        })),
    }
}

/// `stack.set-expanded`, then the session it left: the view evaluated again with the stack shown
/// as asked, answered as an evaluation is. A view the stack's change finds evaluated again since
/// is read again from its query.
pub(crate) fn expanded_now(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
    query: &luxforge_core::catalog_types::ViewQuery,
) -> Result<Box<(ViewSummary, ClientSession)>, String> {
    let summary = match call(owner, client, "stack.set-expanded", params) {
        Ok((summary, _)) if !summary.is_null() => parse::<ViewSummary>(summary)?,
        Ok(_) => return evaluate_now(owner, client, query),
        Err(error) => return Err(error),
    };
    Ok(Box::new((summary, session_now(owner, client)?)))
}

impl Editor {
    /// One merge message.
    pub(crate) fn merge_update(&mut self, message: MergeMessage) -> Task<Message> {
        match message {
            MergeMessage::Act(action) => return self.merge_act(action),
            MergeMessage::Header { moment, action } => return self.moment_action(moment, action),
            MergeMessage::Disclosure(item) => return self.toggle_stack_at(item),
            MergeMessage::Pending => return self.merge_act(MergeAction::Cancel),
            MergeMessage::Planned { serial, result } => {
                if serial != self.select.merge_plans {
                    return Task::none();
                }
                let merge = &mut self.select.state.merge;
                if let Some(form) = &mut merge.form {
                    match result {
                        Ok(plan) => {
                            form.plan = Some(*plan);
                            form.refusal = None;
                        }
                        Err(error) => {
                            form.plan = None;
                            form.refusal = Some(error);
                        }
                    }
                } else if let Some(MergeMenu {
                    kind: MergeMenuKind::Frames { refusal, .. },
                    ..
                }) = &mut merge.menu
                {
                    *refusal = match result {
                        Ok(plan) => model::plan_refusal(&plan),
                        Err(error) => Some(error),
                    };
                }
            }
            MergeMessage::Started { request, result } => {
                return self.merge_started(*request, result);
            }
            MergeMessage::Watched { job, result } => return self.merge_watched(&job, result),
        }
        Task::none()
    }

    fn merge_act(&mut self, action: MergeAction) -> Task<Message> {
        let merge = &mut self.select.state.merge;
        match action {
            MergeAction::CloseMenu => merge.menu = None,
            MergeAction::Start(mode) => {
                merge.menu = None;
                let Some(request) = self.selection_request(mode, MergeOptions::default()) else {
                    return Task::none();
                };
                return self.start_merge(request);
            }
            MergeAction::Options => {
                merge.menu = None;
                let Some(request) =
                    self.selection_request(MergeMode::Pick, MergeOptions::default())
                else {
                    return Task::none();
                };
                let folders = self
                    .select
                    .state
                    .catalog
                    .folders
                    .as_ref()
                    .map(|folders| {
                        folders
                            .folders
                            .iter()
                            .map(|folder| (folder.id.clone(), folder.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                self.select.state.merge.form = Some(MergeForm {
                    request,
                    plan: None,
                    refusal: None,
                    folders,
                    folder_menu: false,
                });
                let lists = self.read_counts();
                return Task::batch([lists, self.plan_form()]);
            }
            MergeAction::FormClose => merge.form = None,
            MergeAction::FormStart(mode) => {
                let Some(form) = &merge.form else {
                    return Task::none();
                };
                let mut request = form.request.clone();
                request.mode = mode;
                return self.start_merge(request);
            }
            MergeAction::Alignment(toggle) => {
                return self.change_options(|options| options.alignment = toggle);
            }
            MergeAction::Deghost(toggle) => {
                return self.change_options(|options| options.deghost = toggle);
            }
            MergeAction::Reference(path) => {
                return self.change_options(|options| options.reference = Some(path));
            }
            MergeAction::Exposure { path, offset } => {
                return self.change_options(|options| options.set_override(path, offset));
            }
            MergeAction::FolderMenu(open) => {
                if let Some(form) = &mut merge.form {
                    form.folder_menu = open;
                }
            }
            MergeAction::Folder(pick) => {
                if let Some(form) = &mut merge.form {
                    form.folder_menu = false;
                    form.request.options.folder = pick;
                }
            }
            MergeAction::Cancel => {
                let Some(pending) = &mut merge.pending else {
                    return Task::none();
                };
                if pending.failed.is_some() {
                    merge.pending = None;
                    self.rebuild_grid();
                    return Task::none();
                }
                if pending.cancelling {
                    return Task::none();
                }
                pending.cancelling = true;
                let job = pending.job.clone();
                self.rebuild_grid();
                return self.cancel_job(job);
            }
            MergeAction::Expand(merge_id, expansion) => {
                merge.menu = None;
                return self.expand_stack(&merge_id, expansion);
            }
            MergeAction::Remove(merge_id) => {
                merge.menu = None;
                let params = model::lifecycle_params(&merge_id, &mutation());
                return self.library_now(LibraryGesture::RemoveMerge, params);
            }
            MergeAction::PutBack(merge_id) => {
                let result = merge
                    .put_back
                    .take()
                    .filter(|put_back| put_back.merge_id == merge_id)
                    .map(|put_back| put_back.result_asset_id);
                let params = model::lifecycle_params(&merge_id, &mutation());
                let task = self.library_now(LibraryGesture::RestoreMerge, params);
                let restored = self
                    .select
                    .library
                    .as_ref()
                    .is_some_and(|last| last["error"].is_null());
                if restored && let Some(asset_id) = result {
                    // The result put back is selected and active once the view lands.
                    self.select.held_click = Some(vec![ItemRef::Photo { asset_id }]);
                    self.select.reveal_held = true;
                }
                return task;
            }
            MergeAction::DismissPutBack => merge.put_back = None,
        }
        Task::none()
    }

    /// A moment header's button: Pick all, or a merge of the bracket's frames.
    fn moment_action(&mut self, number: u32, action: u32) -> Task<Message> {
        let state = &self.select.state;
        let Some((index, action)) = state.content.action(number, action) else {
            return Task::none();
        };
        let mode = match action {
            MomentAction::PickAll(_) => {
                return self.pick_all(number);
            }
            MomentAction::Merge(mode) => mode,
        };
        let Some((summary, moment)) = state
            .summary
            .as_ref()
            .and_then(|summary| Some((summary, summary.groups.moments.get(index as usize)?)))
        else {
            return Task::none();
        };
        let Some(files) = crate::state::select::frame_files(&state.rows, moment.start, moment.len)
        else {
            self.status.text = "Reading the frames\u{2026}".into();
            return Task::none();
        };
        let request = MergeRequest {
            targets: Targets::Files { file_ids: files },
            mode,
            options: MergeOptions::default(),
            source: Some(summary.query.source.clone()),
            anchor: moment.start,
            frames: moment.len,
        };
        self.start_merge(request)
    }

    /// The selection as a merge's frames: by identity when every selected row is held and is a
    /// file (or every one a photograph), otherwise the selection itself, which the owner resolves
    /// and refuses with its own reason.
    fn selection_request(
        &mut self,
        mode: MergeMode,
        options: MergeOptions,
    ) -> Option<MergeRequest> {
        let summary = self.select.state.summary.as_ref()?;
        let selection = self.selection();
        if selection.count == 0 {
            self.status.text = "Select the frames of a bracket to merge".into();
            return None;
        }
        let rows = &self.select.state.rows;
        let mut files = Vec::new();
        let mut assets = Vec::new();
        let mut held = true;
        for &(start, end) in &selection.ranges {
            for position in start..end {
                match rows.row(position).map(|row| &row.item) {
                    Some(RowItem::File { file_id }) => files.push(*file_id),
                    Some(RowItem::Photo { asset_id }) => assets.push(asset_id.clone()),
                    _ => held = false,
                }
            }
        }
        let targets = if held && assets.is_empty() {
            Targets::Files { file_ids: files }
        } else if held && files.is_empty() {
            Targets::Assets { asset_ids: assets }
        } else {
            Targets::Selection
        };
        Some(MergeRequest {
            targets,
            mode,
            options,
            source: Some(summary.query.source.clone()),
            anchor: selection.ranges.first().map_or(0, |range| range.0),
            frames: selection.count,
        })
    }

    /// Start `request` in an owner task, unless a merge of this desktop's is starting or running.
    fn start_merge(&mut self, request: MergeRequest) -> Task<Message> {
        let merge = &mut self.select.state.merge;
        if merge.starting.is_some() || merge.running_job().is_some() {
            self.status.text = "A merge is running: wait for it or cancel it".into();
            return Task::none();
        }
        // A failed merge's row gives way to the next.
        merge.pending = None;
        merge.put_back = None;
        merge.starting = Some(request.clone());
        self.status.text = format!(
            "{}\u{2026}",
            match request.mode {
                MergeMode::Pick => "Merging HDR to add to the Development set",
                MergeMode::Import => "Merging HDR",
            }
        );
        self.rebuild_grid();
        let (owner, client) = (self.owner.clone(), self.client);
        let sent = request.clone();
        owner_task(
            move || start_now(&owner, client, &sent),
            move |result| {
                merge_message(MergeMessage::Started {
                    request: Box::new(request),
                    result,
                })
            },
        )
    }

    /// A merge's start answered.
    fn merge_started(
        &mut self,
        request: MergeRequest,
        result: Result<Box<StartAnswer>, String>,
    ) -> Task<Message> {
        let merge = &mut self.select.state.merge;
        merge.starting = None;
        let answer = match result {
            Ok(answer) => *answer,
            Err(error) => {
                self.status.text = format!("Could not merge: {error}");
                self.rebuild_grid();
                return Task::none();
            }
        };
        self.select.merge_sent = Some(Value::Array(answer.sent.clone()));
        let folder_made = answer
            .sent
            .iter()
            .any(|sent| sent["method"] == "folder.create");
        let task = match answer.started {
            Started::Refused(reason) => {
                if let Some(form) = &mut merge.form {
                    form.refusal = Some(reason.clone());
                }
                self.status.text = format!("Cannot merge: {reason}");
                Task::none()
            }
            Started::Removed {
                merge_id,
                result_asset_id,
            } => {
                merge.put_back = Some(PutBack {
                    merge_id,
                    result_asset_id,
                });
                self.status.text =
                    "This exact merge is in Removed \u{b7} Put back merge to see it again".into();
                Task::none()
            }
            Started::Job { job_id, .. } => {
                merge.form = None;
                merge.pending = Some(PendingMerge::new(job_id.clone(), request));
                self.event("select_merge_started", || json!({"job_id": job_id}));
                Task::none()
            }
        };
        self.rebuild_grid();
        if folder_made {
            return Task::batch([task, self.read_counts()]);
        }
        task
    }

    /// The running merge's job record: its progress, or its end.
    fn merge_watched(&mut self, job: &str, result: Result<Value, String>) -> Task<Message> {
        let Some(pending) = self
            .select
            .state
            .merge
            .pending
            .as_mut()
            .filter(|pending| pending.job == job && pending.failed.is_none())
        else {
            return Task::none();
        };
        let record = match result {
            Ok(record) => record,
            Err(error) => {
                pending.failed = Some(error.clone());
                self.status.text = format!("Could not follow the merge: {error}");
                self.rebuild_grid();
                return Task::none();
            }
        };
        self.select.merge_record = Some(record.clone());
        match record["status"].as_str() {
            Some("queued" | "running") => {
                pending.observe(&record);
                self.rebuild_grid();
                Task::none()
            }
            Some("ready") => {
                let mode = pending.request.mode;
                self.select.state.merge.pending = None;
                let answer = match parse::<MergeAnswer>(record["result"].clone()) {
                    Ok(answer) => answer,
                    Err(error) => {
                        self.status.text = format!("The merge answered unexpectedly: {error}");
                        self.rebuild_grid();
                        return Task::none();
                    }
                };
                self.merge_finished(&answer, mode)
            }
            Some("cancelled") => {
                self.select.state.merge.pending = None;
                self.status.text = "Cancelled the merge \u{b7} nothing was added".into();
                self.rebuild_grid();
                Task::none()
            }
            _ => {
                let failed = model::failure_text(&record);
                self.status.text = format!("Could not merge: {failed}");
                pending.failed = Some(failed);
                self.rebuild_grid();
                Task::none()
            }
        }
    }

    /// A merge ended `ready`: its result, new or reused, is selected and made active in the view
    /// read again, where it leads its stack; Pick merged's result is in the owner's development
    /// set, which Develop follows from now on.
    fn merge_finished(&mut self, answer: &MergeAnswer, mode: MergeMode) -> Task<Message> {
        self.event("select_merge_finished", || {
            json!({
                "merge_id": answer.merge_id,
                "result_asset_id": answer.result_asset_id,
                "reused": answer.reused,
            })
        });
        self.status.text = model::finished_text(answer, mode);
        if let Some(change) = answer.change {
            self.select.own_change = Some(change.0);
        }
        self.select.held_click = Some(vec![ItemRef::Photo {
            asset_id: answer.result_asset_id.clone(),
        }]);
        self.select.reveal_held = true;
        let mut tasks = vec![self.read_events(), self.read_counts()];
        if mode == MergeMode::Pick {
            tasks.push(self.follow_owner_set());
        }
        if let Some(query) = self
            .select
            .state
            .summary
            .as_ref()
            .map(|summary| summary.query.clone())
        {
            tasks.push(self.evaluate(query));
            self.select.reread = super::select::Reread::Own;
        } else {
            self.rebuild_grid();
        }
        Task::batch(tasks)
    }

    /// The form's options changed: plan them again.
    fn change_options(&mut self, change: impl FnOnce(&mut MergeOptions)) -> Task<Message> {
        let Some(form) = &mut self.select.state.merge.form else {
            return Task::none();
        };
        change(&mut form.request.options);
        form.plan = None;
        form.refusal = None;
        self.plan_form()
    }

    /// `merge.plan` of the form's frames and options, in an owner task.
    fn plan_form(&mut self) -> Task<Message> {
        let Some(form) = &self.select.state.merge.form else {
            return Task::none();
        };
        let params = model::plan_params(&form.request.targets, &form.request.options);
        self.select.merge_plans += 1;
        let serial = self.select.merge_plans;
        self.select.plan_sent = Some(params.clone());
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || plan_now(&owner, client, params),
            move |result| merge_message(MergeMessage::Planned { serial, result }),
        )
    }

    /// Show the stack of `merge_id` as `expansion`: `stack.set-expanded` in an owner task, its
    /// answer adopted as the view's evaluation.
    pub(crate) fn expand_stack(
        &mut self,
        merge_id: &luxforge_core::catalog_types::MergeId,
        expansion: StackExpansion,
    ) -> Task<Message> {
        let Some(summary) = &self.select.state.summary else {
            return Task::none();
        };
        let params = model::expanded_params(merge_id, expansion, summary.revision);
        let query = summary.query.clone();
        self.select.stack_sent = Some(params.clone());
        self.select.serial += 1;
        self.select.asked_at = Some(std::time::Instant::now());
        self.select.reread = super::select::Reread::Quiet;
        self.select.state.loading = true;
        let serial = self.select.serial;
        // The chips count the query's items, which showing a stack otherwise does not change: the
        // facets held stay this evaluation's.
        if self.select.facets_answered == serial - 1 {
            self.select.facets_answered = serial;
        }
        self.event(
            "select_stack_expanded",
            || json!({"merge_id": merge_id, "expansion": expansion.as_str()}),
        );
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || expanded_now(&owner, client, params, &query),
            move |result| Message::Select(SelectMessage::Viewed { serial, result }),
        )
    }

    /// `S` or a press on the badge, for the item at `item`: a result's stack, or the stack of the
    /// original in it, collapsed when it is open and expanded when it is not. `None` when the
    /// item is in no stack this view shows.
    pub(crate) fn toggle_stack_at(&mut self, item: u32) -> Task<Message> {
        let Some(stack) = self
            .select
            .state
            .rows
            .row(item)
            .and_then(|row| row.stack.clone())
        else {
            return Task::none();
        };
        let expansion = match (stack.role, stack.expansion) {
            (StackRole::Leader, Some(StackExpansion::Collapsed) | None) => StackExpansion::Expanded,
            (StackRole::Leader, Some(_)) | (StackRole::Original, _) => StackExpansion::Collapsed,
            (StackRole::Separate, _) => return Task::none(),
        };
        self.expand_stack(&stack.merge_id, expansion)
    }

    /// `Delete` over files: Remove merge of the active result, when one is active alone.
    pub(crate) fn remove_active_merge(&mut self) -> Task<Message> {
        let selection = self.selection();
        let merge_id = selection
            .active
            .filter(|_| selection.count == 1)
            .and_then(|active| self.select.state.rows.row(active))
            .and_then(|row| row.stack.as_ref())
            .filter(|stack| stack.role == StackRole::Leader)
            .map(|stack| stack.merge_id.clone());
        match merge_id {
            Some(merge_id) => self.merge_act(MergeAction::Remove(merge_id)),
            None => Task::none(),
        }
    }

    /// Whether the item at `item` is in a stack `S` toggles.
    pub(crate) fn in_shown_stack(&self, item: u32) -> bool {
        self.select
            .state
            .rows
            .row(item)
            .and_then(|row| row.stack.as_ref())
            .is_some_and(|stack| stack.role != StackRole::Separate)
    }

    /// A right-click on a cell of a view over files: the merge actions for a selection of
    /// frames, planned at once so that frames that cannot merge say why in the core's words; a
    /// result's stack actions; or nothing for one frame.
    pub(crate) fn merge_menu(&mut self, x: f32, y: f32) -> Task<Message> {
        let selection = self.selection();
        let filtered = self
            .select
            .state
            .summary
            .as_ref()
            .is_some_and(|summary| summary.query.filter != ViewFilter::default());
        let leader = selection
            .active
            .and_then(|active| self.select.state.rows.row(active))
            .and_then(|row| row.stack.clone())
            .filter(|stack| stack.role == StackRole::Leader);
        let kind = match (selection.count, leader) {
            (1, Some(stack)) => MergeMenuKind::Stack {
                merge_id: stack.merge_id,
                exposures: stack.exposures,
                expansion: stack.expansion.unwrap_or_default(),
                filtered,
            },
            (0 | 1, _) => return Task::none(),
            (count, _) => MergeMenuKind::Frames {
                count,
                refusal: None,
            },
        };
        let frames = matches!(kind, MergeMenuKind::Frames { .. });
        self.select.state.merge.menu = Some(MergeMenu { x, y, kind });
        if !frames {
            return Task::none();
        }
        let Some(request) = self.selection_request(MergeMode::Pick, MergeOptions::default()) else {
            return Task::none();
        };
        self.select.merge_plans += 1;
        let serial = self.select.merge_plans;
        let params = model::plan_params(&request.targets, &request.options);
        self.select.plan_sent = Some(params.clone());
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || plan_now(&owner, client, params),
            move |result| merge_message(MergeMessage::Planned { serial, result }),
        )
    }

    /// The pending row's block, for the grid of a view of the source the merge was asked in.
    pub(crate) fn pending_block(&self) -> Option<(u32, Block)> {
        let merge = &self.select.state.merge;
        let summary = self.select.state.summary.as_ref()?;
        let pending = merge.pending.as_ref()?;
        if pending.request.source.as_ref() != Some(&summary.query.source) {
            return None;
        }
        Some((
            pending.request.anchor,
            Block::Pending {
                title: pending.title(),
                detail: pending.detail(),
                progress: pending.fraction.map(|fraction| fraction as f32),
                action: if pending.failed.is_some() {
                    "Dismiss"
                } else {
                    "Cancel"
                }
                .to_owned(),
                failed: pending.failed.is_some(),
            },
        ))
    }

    /// The merge state for a captured frame's record.
    pub(crate) fn merge_summary(&self) -> Value {
        let merge = &self.select.state.merge;
        json!({
            "starting": merge.starting.is_some(),
            "pending": merge.pending.as_ref().map(|pending| json!({
                "job_id": pending.job,
                "mode": pending.request.mode.label(),
                "frames": pending.request.frames,
                "fraction": pending.fraction,
                "message": pending.message,
                "cancelling": pending.cancelling,
                "failed": pending.failed,
            })),
            "put_back": merge.put_back.as_ref().map(|put_back| put_back.merge_id.clone()),
            "form": merge.form.is_some(),
            "menu": merge.menu.is_some(),
            "sent": self.select.merge_sent,
            "stack": self.select.stack_sent,
            "plan": self.select.plan_sent,
            "record": self.select.merge_record,
        })
    }
}
