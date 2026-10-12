//! Merges in the Select workspace ([HDR merge design](../../../../docs/design/hdr-merge.md),
//! "Grid workflow"; the [merge preview](../../../../docs/design/panorama-merge.md#the-merge-preview)):
//! a bracket's Pick merged and Import merged, the same for a selection of frames through the grid's
//! menu, the merge preview sheet they open, ⌥-click merging at once with the last settings, the
//! one pending merge row, the result's stack and its originals, and Remove merge and Put back
//! merge. Its model is `state/select_merge.rs`; the grid draws the pending row, the badges and the
//! notes, and `view/select_merge.rs` the menu, the sheet and the notice.
//!
//! The desktop holds no merge logic: every gesture sends the request an API client sends, and
//! every refusal, picture, progress and result shown is the owner's.
//!
//! - **The preview** opens on Pick merged or Import merged, from a header or the menu: one owner
//!   task sends `merge.plan` of the frames and then, unless the plan refuses, `merge.preview` with
//!   the plan's identity, as this desktop's actor. Its `merge-preview` job's `job.wait` reader
//!   ([`super::long_work`]) brings each record: the job's words and fraction while it reads and
//!   aligns, then the revision, whose JPEG is decoded off the update loop. A setting change sends
//!   `merge.preview-update` with the whole options; a newer one supersedes it, and the owner
//!   cancels the stale job. Show deghost overlay draws the revision's own overlay data and sends
//!   nothing. Cancel sends `merge.preview-close`.
//! - **Starting a merge** is one owner task off the update loop, since planning stats each frame
//!   on disk: `merge.plan` of the frames by identity (a header's files, or the selection's rows as
//!   they are held; the selection itself when a row is not held or is no file), `folder.create`
//!   of the plan's suggested folder when it proposes none and the person chose none, then
//!   `merge.create`: from the sheet with its `preview_id` and options, which continues the
//!   preview's work, and with ⌥ held with the plan's identity and the last settings. A plan that
//!   refuses is said in its own words; an exact merge in Removed is offered as Put back merge. A
//!   merge that starts is remembered: its alignment and deghosting are the next ⌥-click's.
//! - **While the job runs** its `job.wait` reader brings each record: the pending row before the
//!   frames says how far it got and offers Cancel (`job.cancel`, the activity board's Cancel too).
//!   Its end is the job's: `ready` reads the view again with the result selected and active,
//!   leading its stack collapsed; failed says the input or stage that failed in the row until it
//!   is dismissed; cancelled says nothing was added. Nothing is hidden before the owner publishes.
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
use crate::state::preferences::PreferenceChange;
use crate::state::select::{Block, LibraryGesture, MomentAction};
use crate::state::select_merge::{
    self as model, CreateFrom, MergeAction, MergeMenu, MergeMenuKind, MergeMode, MergeOptions,
    MergeRequest, PendingMerge, PreviewSheet, PutBack, Started, sheet::SheetContext,
};
use iced::{Task, widget::image::Handle};
use luxforge_core::{
    Cancel, ClientId, ClientSession, OwnerHandle,
    catalog_types::{
        ItemRef, MergeAnswer, MergeKind, MergePlan, MergePreview, MergePreviewId, MergeState,
        RowItem, StackExpansion, StackRole, Targets, ViewFilter, ViewSummary,
    },
};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The longest side the preview's picture is decoded at: the most a preview draws.
const PICTURE_SIDE: u32 = 4096;

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

/// A `merge-preview` job the owner started: its id, and the preview it prepares when the answer
/// names it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreviewJob {
    pub(crate) job_id: String,
    pub(crate) preview_id: Option<MergePreviewId>,
}

/// What opening the preview came to: the plan, an exact merge in Removed, the preview's job or why
/// there is none, and every request sent in order.
#[derive(Clone, Debug)]
pub(crate) struct OpenAnswer {
    pub(crate) plan: Result<MergePlan, String>,
    pub(crate) removed: Option<PutBack>,
    pub(crate) job: Option<Result<PreviewJob, String>>,
    pub(crate) sent: Vec<Value>,
}

/// The settled picture the sheet draws: the revision's JPEG decoded once.
#[derive(Clone, Debug)]
pub(crate) struct SheetPicture {
    pub(crate) path: PathBuf,
    pub(crate) handle: Handle,
}

// -- The owner calls, each the body of one owner task. --

/// `merge.plan` for the menu.
pub(crate) fn plan_now(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<Box<MergePlan>, String> {
    let (plan, _) = call(owner, client, "merge.plan", params)?;
    parse(plan).map(Box::new)
}

/// Open the preview of `request`: plan it, and unless the plan refuses or finds this exact merge
/// in Removed, start its `merge-preview` job. Each request is sent as an API client writes it.
pub(crate) fn open_now(
    owner: &OwnerHandle,
    client: ClientId,
    request: &MergeRequest,
) -> OpenAnswer {
    let mut sent = Vec::new();
    let params = model::plan_params(request.kind, &request.targets, &request.options);
    sent.push(json!({"method": "merge.plan", "params": params}));
    let plan = match call_detailed(owner, client, "merge.plan", params) {
        Ok(plan) => parse::<MergePlan>(plan),
        Err(error) => Err(error.message),
    };
    let mut answer = OpenAnswer {
        plan,
        removed: None,
        job: None,
        sent,
    };
    let Ok(plan) = &answer.plan else {
        return answer;
    };
    if model::plan_refusal(plan).is_some() {
        return answer;
    }
    if let Some(existing) = plan
        .existing
        .as_ref()
        .filter(|existing| existing.state == MergeState::Removed)
    {
        answer.removed = Some(PutBack {
            merge_id: existing.merge_id.clone(),
            result_asset_id: existing.result_asset_id.clone(),
        });
        return answer;
    }
    let params = model::preview_params(request, Some(&plan.plan_id));
    answer
        .sent
        .push(json!({"method": "merge.preview", "params": params}));
    answer.job = Some(preview_now(owner, client, "merge.preview", params));
    answer
}

/// `merge.preview` or `merge.preview-update`: the job it started.
pub(crate) fn preview_now(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<PreviewJob, String> {
    let answer = call_detailed(owner, client, method, params).map_err(|error| error.message)?;
    let job_id = answer["job_id"]
        .as_str()
        .ok_or_else(|| format!("{method} answered no job: {answer}"))?
        .to_owned();
    let preview_id = answer
        .get("preview_id")
        .and_then(|id| serde_json::from_value(id.clone()).ok());
    Ok(PreviewJob { job_id, preview_id })
}

/// `merge.preview-close`.
pub(crate) fn close_now(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<(), String> {
    call(owner, client, "merge.preview-close", params).map(|_| ())
}

/// The revision's picture, decoded for display.
pub(crate) fn picture_now(path: &std::path::Path) -> Result<Handle, String> {
    let decoded = luxforge_core::decode_preview(path, PICTURE_SIDE, &Cancel::new())
        .map_err(|error| error.to_string())?;
    Ok(Handle::from_rgba(
        decoded.width,
        decoded.height,
        decoded.rgba,
    ))
}

/// Start a merge of `request`: plan it, make the folder it suggests when it needs one, and create
/// it, continuing `preview` when it comes from the sheet. Each request is sent as an API client
/// writes it.
pub(crate) fn start_now(
    owner: &OwnerHandle,
    client: ClientId,
    request: &MergeRequest,
    preview: Option<&MergePreviewId>,
) -> Result<Box<StartAnswer>, String> {
    let mut sent = Vec::new();
    let params = model::plan_params(request.kind, &request.targets, &request.options);
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
        (model::FolderPick::Existing { folder_id, .. }, _) => folder_id.clone(),
        (model::FolderPick::Proposed, Some(proposal)) => proposal.folder_id.clone(),
        (model::FolderPick::Proposed, None) => {
            let Some(params) = model::suggested_folder_params(&plan, &mutation()) else {
                return answer(Started::Refused(plan.folder_reason.clone().unwrap_or_else(
                    || "Choose a catalog folder for the merge".to_owned(),
                )));
            };
            sent.push(json!({"method": "folder.create", "params": params}));
            let created = call_detailed(owner, client, "folder.create", params)
                .map_err(|error| format!("Could not make the folder: {}", error.message))?;
            parse(created["folder"]["id"].clone())?
        }
    };
    let from = match preview {
        Some(preview) => CreateFrom::Preview(preview),
        None => CreateFrom::Plan(&plan.plan_id),
    };
    let params = model::create_params(request, from, &folder, &mutation());
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
            MergeMessage::Option(held) => self.select.state.merge.option = held,
            MergeMessage::Planned { serial, result } => {
                if serial != self.select.merge_plans {
                    return Task::none();
                }
                if let Some(MergeMenu {
                    kind: MergeMenuKind::Frames { refusal, .. },
                    ..
                }) = &mut self.select.state.merge.menu
                {
                    *refusal = match result {
                        Ok(plan) => model::plan_refusal(&plan),
                        Err(error) => Some(error),
                    };
                }
            }
            MergeMessage::Opened { serial, answer } => return self.sheet_opened(serial, *answer),
            MergeMessage::Updated { serial, result } => return self.sheet_updated(serial, result),
            MergeMessage::PreviewWatched { job, result } => {
                return self.preview_watched(&job, result);
            }
            MergeMessage::Picture { path, result } => self.sheet_picture(path, result),
            MergeMessage::Closed(result) => {
                if let Err(error) = result {
                    self.event(
                        "select_merge_preview_close_failed",
                        || json!({"reason": error}),
                    );
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
            MergeAction::Start(kind, mode) => {
                merge.menu = None;
                let Some((request, context)) =
                    self.selection_request(kind, mode, self.last_options())
                else {
                    return Task::none();
                };
                return self.merge_or_preview(request, context);
            }
            MergeAction::SheetClose => return self.close_sheet(),
            MergeAction::SheetMerge(mode) => return self.merge_from_sheet(mode),
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
            MergeAction::Overlay(shown) => {
                if let Some(sheet) = &mut merge.sheet {
                    sheet.overlay = shown;
                }
            }
            MergeAction::FolderMenu(open) => {
                if let Some(sheet) = &mut merge.sheet {
                    sheet.folder_menu = open;
                }
            }
            MergeAction::Folder(pick) => {
                if let Some(sheet) = &mut merge.sheet {
                    sheet.folder_menu = false;
                    sheet.request.options.folder = pick;
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

    /// The last HDR merge's settings, as the desktop remembers them: what ⌥-click merges with and
    /// the sheet opens with.
    pub(crate) fn last_options(&self) -> MergeOptions {
        MergeOptions::last(
            self.preferences
                .applied()
                .and_then(|preferences| preferences.hdr_merge),
        )
    }

    /// A merge action pressed: with ⌥ held, a merge at once with the last settings; otherwise the
    /// preview sheet, with that action primary.
    fn merge_or_preview(&mut self, request: MergeRequest, context: SheetContext) -> Task<Message> {
        if self.select.state.merge.option {
            self.event(
                "select_merge_last_settings",
                || json!({"mode": request.mode.label(), "frames": request.frames}),
            );
            return self.start_merge(request, None);
        }
        self.open_sheet(request, context)
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
        // A bracket's header merges are HDR's.
        let request = MergeRequest {
            kind: MergeKind::Hdr,
            targets: Targets::Files { file_ids: files },
            mode,
            options: self.last_options(),
            source: Some(summary.query.source.clone()),
            anchor: moment.start,
            frames: moment.len,
        };
        let context = self.sheet_context(moment.start..moment.start + moment.len);
        self.merge_or_preview(request, context)
    }

    /// The view's words for the frames at `positions`: their files, and the first one's camera
    /// and capture time.
    fn sheet_context(&self, positions: impl IntoIterator<Item = u32>) -> SheetContext {
        let mut context = SheetContext::default();
        for position in positions {
            let Some(row) = self.select.state.rows.row(position) else {
                continue;
            };
            if context.names.is_empty() {
                context.camera = row.camera.clone();
                context.capture = row.capture.clone();
            }
            context.names.push(row.file_name.clone());
        }
        context
    }

    /// The selection as a merge's frames: by identity when every selected row is held and is a
    /// file (or every one a photograph), otherwise the selection itself, which the owner resolves
    /// and refuses with its own reason; a merge of `kind`, with the view's words for its frames.
    fn selection_request(
        &mut self,
        kind: MergeKind,
        mode: MergeMode,
        options: MergeOptions,
    ) -> Option<(MergeRequest, SheetContext)> {
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
        let request = MergeRequest {
            kind,
            targets,
            mode,
            options,
            source: Some(summary.query.source.clone()),
            anchor: selection.ranges.first().map_or(0, |range| range.0),
            frames: selection.count,
        };
        let context =
            self.sheet_context(selection.ranges.iter().flat_map(|&(start, end)| start..end));
        Some((request, context))
    }

    // -- The preview sheet --

    /// Open the preview sheet for `request`, with its action primary: plan it and start the
    /// preview in one owner task. A sheet already open closes first.
    fn open_sheet(&mut self, request: MergeRequest, context: SheetContext) -> Task<Message> {
        let close = self.close_sheet();
        let merge = &mut self.select.state.merge;
        merge.put_back = None;
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
        self.select.state.merge.sheet = Some(PreviewSheet::new(request, context, folders));
        self.event("select_merge_preview_opened", || json!({}));
        let open = self.reopen_sheet();
        let lists = self.read_counts();
        Task::batch([close, open, lists])
    }

    /// Plan the sheet's frames and start their preview again, with the sheet's settings.
    fn reopen_sheet(&mut self) -> Task<Message> {
        self.select.merge_serial += 1;
        let serial = self.select.merge_serial;
        let Some(sheet) = &mut self.select.state.merge.sheet else {
            return Task::none();
        };
        sheet.serial = serial;
        sheet.asking = true;
        sheet.stale = false;
        sheet.refusal = None;
        let request = sheet.request.clone();
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || open_now(&owner, client, &request),
            move |answer| {
                merge_message(MergeMessage::Opened {
                    serial,
                    answer: Box::new(answer),
                })
            },
        )
    }

    /// The sheet's plan and preview answered.
    fn sheet_opened(&mut self, serial: u64, answer: OpenAnswer) -> Task<Message> {
        let Some(sheet) = self
            .select
            .state
            .merge
            .sheet
            .as_mut()
            .filter(|sheet| sheet.serial == serial)
        else {
            // A sheet closed before its preview answered: close what it opened.
            return match answer.job {
                Some(Ok(job)) => self.close_preview(job.preview_id, Some(job.job_id)),
                _ => Task::none(),
            };
        };
        sheet.asking = false;
        sheet.sent.extend(answer.sent);
        match answer.plan {
            Ok(plan) => sheet.planned(plan),
            Err(error) => sheet.failed(error),
        }
        if let Some(put_back) = answer.removed {
            self.select.state.merge.sheet = None;
            self.select.merge_picture = None;
            self.select.state.merge.put_back = Some(put_back);
            self.status.text =
                "This exact merge is in Removed \u{b7} Put back merge to see it again".into();
            return Task::none();
        }
        match answer.job {
            Some(Ok(job)) => sheet.following(job.job_id, job.preview_id),
            Some(Err(error)) => sheet.failed(error),
            None => {}
        }
        self.sheet_next()
    }

    /// The sheet's settings changed: draw them.
    fn change_options(&mut self, change: impl FnOnce(&mut MergeOptions)) -> Task<Message> {
        let Some(sheet) = &mut self.select.state.merge.sheet else {
            return Task::none();
        };
        change(&mut sheet.request.options);
        sheet.stale = true;
        self.send_stale()
    }

    /// Send the sheet's settings when they are newer than its preview's: `merge.preview-update`
    /// once the preview is known; nothing yet while it is being opened, which sends them when it
    /// answers; and the whole opening again when there is no preview to update.
    fn send_stale(&mut self) -> Task<Message> {
        let starting = self.select.state.merge.starting.is_some();
        let Some(sheet) = &mut self.select.state.merge.sheet else {
            return Task::none();
        };
        if !sheet.stale || starting {
            return Task::none();
        }
        if let Some(preview_id) = sheet.preview_id.clone() {
            self.select.merge_serial += 1;
            let serial = self.select.merge_serial;
            sheet.serial = serial;
            sheet.asking = true;
            sheet.stale = false;
            sheet.refusal = None;
            let params = model::preview_update_params(
                sheet.request.kind,
                &preview_id,
                &sheet.request.options,
            );
            sheet
                .sent
                .push(json!({"method": "merge.preview-update", "params": params}));
            let (owner, client) = (self.owner.clone(), self.client);
            return owner_task(
                move || preview_now(&owner, client, "merge.preview-update", params),
                move |result| merge_message(MergeMessage::Updated { serial, result }),
            );
        }
        if sheet.asking || sheet.job.is_some() {
            // The preview is being opened: its identity is not known yet.
            return Task::none();
        }
        self.reopen_sheet()
    }

    /// A `merge.preview-update` answered with its job, or why not.
    fn sheet_updated(&mut self, serial: u64, result: Result<PreviewJob, String>) -> Task<Message> {
        let Some(sheet) = self
            .select
            .state
            .merge
            .sheet
            .as_mut()
            .filter(|sheet| sheet.serial == serial)
        else {
            return Task::none();
        };
        sheet.asking = false;
        match result {
            Ok(job) => sheet.following(job.job_id, job.preview_id),
            Err(error) => sheet.failed(error),
        }
        self.sheet_next()
    }

    /// The open preview's job record: its progress, or its revision.
    fn preview_watched(&mut self, job: &str, result: Result<Value, String>) -> Task<Message> {
        let Some(sheet) = self
            .select
            .state
            .merge
            .sheet
            .as_mut()
            .filter(|sheet| sheet.job.as_deref() == Some(job))
        else {
            return Task::none();
        };
        let record = match result {
            Ok(record) => record,
            Err(error) => {
                sheet.failed(error);
                return Task::none();
            }
        };
        self.select.preview_record = Some(record.clone());
        let task = match record["status"].as_str() {
            Some("queued" | "running") => {
                sheet.observe(&record);
                Task::none()
            }
            Some("ready") => match parse::<MergePreview>(record["result"].clone()) {
                Ok(preview) => {
                    let path = preview.path.clone();
                    sheet.adopt(preview);
                    self.event(
                        "select_merge_preview_ready",
                        || json!({"revision": record["result"]["revision"], "path": path}),
                    );
                    match path {
                        Some(path)
                            if self
                                .select
                                .merge_picture
                                .as_ref()
                                .is_none_or(|picture| picture.path != path) =>
                        {
                            self.decode_picture(path)
                        }
                        _ => Task::none(),
                    }
                }
                Err(error) => {
                    sheet.failed(format!("The preview answered unexpectedly: {error}"));
                    Task::none()
                }
            },
            Some("cancelled") => {
                sheet.failed("The preview was cancelled".to_owned());
                Task::none()
            }
            _ => {
                sheet.failed(model::failure_text(&record));
                Task::none()
            }
        };
        Task::batch([task, self.sheet_next()])
    }

    /// Decode a revision's picture off the update loop.
    fn decode_picture(&mut self, path: PathBuf) -> Task<Message> {
        self.select.merge_picture_wanted = Some(path.clone());
        let decoding = path.clone();
        owner_task(
            move || picture_now(&decoding),
            move |result| merge_message(MergeMessage::Picture { path, result }),
        )
    }

    /// A revision's picture decoded: drawn while it is still the sheet's.
    fn sheet_picture(&mut self, path: PathBuf, result: Result<Handle, String>) {
        if self.select.merge_picture_wanted.as_ref() != Some(&path) {
            return;
        }
        self.select.merge_picture_wanted = None;
        match result {
            Ok(handle) => self.select.merge_picture = Some(SheetPicture { path, handle }),
            Err(error) => {
                if let Some(sheet) = &mut self.select.state.merge.sheet {
                    sheet.failed(format!("Could not show the preview: {error}"));
                }
            }
        }
    }

    /// The sheet put away: its preview closed, or its job cancelled while the preview is not
    /// known yet. Nothing is published.
    fn close_sheet(&mut self) -> Task<Message> {
        let Some(sheet) = self.select.state.merge.sheet.take() else {
            return Task::none();
        };
        self.select.merge_picture = None;
        self.select.merge_picture_wanted = None;
        self.close_preview(sheet.preview_id, sheet.job)
    }

    fn close_preview(
        &mut self,
        preview_id: Option<MergePreviewId>,
        job: Option<String>,
    ) -> Task<Message> {
        if let Some(preview_id) = preview_id {
            let params = model::preview_close_params(&preview_id);
            self.select.preview_closed = Some(params.clone());
            self.event("select_merge_preview_closed", || params.clone());
            let (owner, client) = (self.owner.clone(), self.client);
            return owner_task(
                move || close_now(&owner, client, params),
                move |result| merge_message(MergeMessage::Closed(result)),
            );
        }
        match job {
            Some(job) => self.cancel_job(job),
            None => Task::none(),
        }
    }

    /// The sheet's Pick merged or Import merged: a merge of its frames with its settings,
    /// continuing its preview.
    fn merge_from_sheet(&mut self, mode: MergeMode) -> Task<Message> {
        let Some(sheet) = &mut self.select.state.merge.sheet else {
            return Task::none();
        };
        if sheet.refusal.is_some() || sheet.merging.is_some() {
            return Task::none();
        }
        sheet.merging = Some(mode);
        self.queued_merge()
    }

    /// Start the merge pressed on the sheet once its preview can take it: the preview's identity
    /// is known (the owner names it with its first revision) and its settings have been sent. A
    /// merge pressed while the preview still reads and aligns waits for that revision and then
    /// continues its work; one the preview refuses is dropped, the sheet saying why.
    fn queued_merge(&mut self) -> Task<Message> {
        let merge = &mut self.select.state.merge;
        if merge.starting.is_some() {
            return Task::none();
        }
        let Some(sheet) = &mut merge.sheet else {
            return Task::none();
        };
        let Some(mode) = sheet.merging else {
            return Task::none();
        };
        if sheet.refusal.is_some() {
            sheet.merging = None;
            return Task::none();
        }
        match sheet.preview_id.clone() {
            Some(preview) if !sheet.stale => {
                let mut request = sheet.request.clone();
                request.mode = mode;
                self.start_merge(request, Some(preview))
            }
            _ => Task::none(),
        }
    }

    /// After an answer for the sheet: its settings sent if they are newer than its preview's, and
    /// a merge pressed meanwhile started once it can be.
    fn sheet_next(&mut self) -> Task<Message> {
        let send = self.send_stale();
        let merge = self.queued_merge();
        Task::batch([send, merge])
    }

    // -- Merging --

    /// Start `request` in an owner task, continuing `preview` when there is one, unless a merge
    /// of this desktop's is starting or running.
    fn start_merge(
        &mut self,
        request: MergeRequest,
        preview: Option<MergePreviewId>,
    ) -> Task<Message> {
        let merge = &mut self.select.state.merge;
        if merge.busy() {
            if let Some(sheet) = &mut merge.sheet {
                sheet.merging = None;
            }
            self.status.text = model::BUSY.into();
            return Task::none();
        }
        // A failed merge's row gives way to the next.
        merge.pending = None;
        merge.put_back = None;
        merge.starting = Some(request.clone());
        let noun = model::words(request.kind).noun;
        self.status.text = match request.mode {
            MergeMode::Pick => format!("Merging {noun} to add to the Development set\u{2026}"),
            MergeMode::Import => format!("Merging {noun}\u{2026}"),
        };
        self.rebuild_grid();
        let (owner, client) = (self.owner.clone(), self.client);
        let sent = request.clone();
        owner_task(
            move || start_now(&owner, client, &sent, preview.as_ref()),
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
        if let Some(sheet) = &mut merge.sheet {
            sheet.merging = None;
        }
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
        let mut tasks = Vec::new();
        match answer.started {
            Started::Refused(reason) => {
                if let Some(sheet) = &mut merge.sheet {
                    sheet.failed(reason.clone());
                }
                self.status.text = format!("Cannot merge: {reason}");
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
                tasks.push(self.close_sheet());
            }
            Started::Job { job_id, .. } => {
                // The merge continues the preview: the sheet goes, its preview with the merge.
                merge.sheet = None;
                self.select.merge_picture = None;
                self.select.merge_picture_wanted = None;
                merge.pending = Some(PendingMerge::new(job_id.clone(), request.clone()));
                self.event("select_merge_started", || json!({"job_id": job_id}));
                tasks.push(self.remember_merge(&request.options));
            }
        }
        self.rebuild_grid();
        if folder_made {
            tasks.push(self.read_counts());
        }
        Task::batch(tasks)
    }

    /// A merge of `options` started: they are the last settings from now on.
    fn remember_merge(&mut self, options: &MergeOptions) -> Task<Message> {
        let now = options.remembered();
        let remembered = self
            .preferences
            .applied()
            .map(|preferences| preferences.hdr_merge.unwrap_or_default());
        if remembered == Some(now) {
            return Task::none();
        }
        self.store_preferences(PreferenceChange {
            hdr_merge: Some(Some(now)),
            ..PreferenceChange::default()
        })
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
                kind: stack.kind,
                size: stack.size,
                expansion: stack.expansion.unwrap_or_default(),
                filtered,
            },
            (0 | 1, _) => return Task::none(),
            // The selection menu's merge group is HDR's, the one kind with a provider.
            (count, _) => MergeMenuKind::Frames {
                kind: MergeKind::Hdr,
                count,
                refusal: None,
            },
        };
        let frames = match kind {
            MergeMenuKind::Frames { kind, .. } => Some(kind),
            MergeMenuKind::Stack { .. } => None,
        };
        self.select.state.merge.menu = Some(MergeMenu { x, y, kind });
        let Some(kind) = frames else {
            return Task::none();
        };
        let Some((request, _)) =
            self.selection_request(kind, MergeMode::Pick, MergeOptions::default())
        else {
            return Task::none();
        };
        self.select.merge_plans += 1;
        let serial = self.select.merge_plans;
        let params = model::plan_params(request.kind, &request.targets, &request.options);
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
        let sheet = merge.sheet.as_ref().map(|sheet| {
            let model = crate::state::select_merge::sheet::model(sheet, None);
            json!({
                "title": model.title,
                "subtitle": model.subtitle,
                "primary": sheet.request.mode.label(),
                "preview_id": sheet.preview_id,
                "job": sheet.job,
                "message": sheet.message,
                "fraction": sheet.fraction,
                "revision": sheet.preview.as_ref().map(|preview| preview.revision),
                "current": sheet.current(),
                "picture": sheet.preview.as_ref().and_then(|preview| preview.path.clone()),
                "picture_shown": self.select.merge_picture.as_ref().map(|picture| picture.path.clone()),
                "status": model.status,
                "output": model.output,
                "refusal": sheet.refusal,
                "overlay": sheet.overlay,
                "regions": match &model.picture {
                    crate::state::select_merge::sheet::Picture::Ready { regions, .. } => {
                        regions.iter().map(|region| region.label.clone()).collect::<Vec<_>>()
                    }
                    _ => Vec::new(),
                },
                "alignment": format!("{:?}", sheet.request.options.alignment),
                "deghost": format!("{:?}", sheet.request.options.deghost),
                "frames": model.frames.iter().map(|frame| json!({
                    "name": frame.name,
                    "detail": frame.detail,
                    "value": frame.value,
                    "reference": frame.reference,
                })).collect::<Vec<_>>(),
                "folder": model.folder,
                "enabled": model.enabled,
                "sent": sheet.sent,
            })
        });
        json!({
            "starting": merge.starting.is_some(),
            "option": merge.option,
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
            "sheet": sheet,
            "closed": self.select.preview_closed,
            "menu": merge.menu.is_some(),
            "sent": self.select.merge_sent,
            "stack": self.select.stack_sent,
            "plan": self.select.plan_sent,
            "record": self.select.merge_record,
            "preview_record": self.select.preview_record,
        })
    }
}
