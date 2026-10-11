//! HDR merges in the Select workspace ([HDR merge design](../../../../docs/design/hdr-merge.md),
//! "Grid workflow"), as plain data: the requests Pick merged and Import merged send, the Merge
//! options… form, the one pending merge row this desktop draws while its job runs, a merge's end
//! in words, and the words a result's stack badge and its originals' notes show. Every decision is
//! the owner's: the plan says whether the frames merge and why not, the job says how far it has
//! got and how it ended, and a view's rows say which result leads which originals.
use luxforge_core::{
    AssetId, MutationRequest,
    catalog_types::{
        CatalogFolderId, FolderChoice, MergeAnswer, MergeId, MergePlan, RowStack, StackExpansion,
        StackRole, Targets, ViewSource,
    },
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::LazyLock};

/// Pick merged's hint: what it does beyond Import merged.
pub(crate) const PICK_HINT: &str = "Merge HDR and add to Development set";
/// Import merged's hint.
pub(crate) const IMPORT_HINT: &str = "Merge HDR into the catalog only";
/// The most and fewest frames a merge takes, as the core plans them: the selection's merge
/// actions are offered for these many selected.
pub(crate) const MIN_FRAMES: u32 = 2;
pub(crate) const MAX_FRAMES: u32 = 9;

/// Which of the two creation modes a gesture asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeMode {
    /// Pick merged: the result joins the current development set as it is published.
    Pick,
    /// Import merged: the result is only imported.
    Import,
}

impl MergeMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pick => "Pick merged",
            Self::Import => "Import merged",
        }
    }

    pub(crate) fn hint(self) -> &'static str {
        match self {
            Self::Pick => PICK_HINT,
            Self::Import => IMPORT_HINT,
        }
    }
}

/// Alignment's or deghosting's choice: the core's default, or off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Toggle {
    #[default]
    Auto,
    Off,
}

impl Toggle {
    fn wire(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }
}

/// Where the result goes: the plan's proposal (or, when it proposes none, the new folder it
/// suggests, made with `folder.create` first), or a folder the person chose.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum FolderPick {
    #[default]
    Proposed,
    Existing {
        folder_id: CatalogFolderId,
        name: String,
    },
}

/// What Merge options… may change; every field left alone is the core's default, sent as nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MergeOptions {
    pub(crate) alignment: Toggle,
    pub(crate) deghost: Toggle,
    /// The reference frame's path, when the person chose one.
    pub(crate) reference: Option<PathBuf>,
    /// Each frame's EV offset the person set, by path.
    pub(crate) overrides: Vec<(PathBuf, f64)>,
    pub(crate) folder: FolderPick,
}

impl MergeOptions {
    /// The override of `path`, if one is set.
    pub(crate) fn override_of(&self, path: &std::path::Path) -> Option<f64> {
        self.overrides
            .iter()
            .find(|(at, _)| at == path)
            .map(|(_, offset)| *offset)
    }

    /// Set `path`'s EV offset, or with `None` take its override away.
    pub(crate) fn set_override(&mut self, path: PathBuf, offset: Option<f64>) {
        self.overrides.retain(|(at, _)| *at != path);
        if let Some(offset) = offset {
            self.overrides.push((path, offset));
        }
    }
}

/// One merge request as the desktop makes it: which frames, which mode, the options, and where
/// in the view it was asked for, so the pending row is drawn there.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MergeRequest {
    pub(crate) targets: Targets,
    pub(crate) mode: MergeMode,
    pub(crate) options: MergeOptions,
    /// The view the gesture was made in and the position of its first frame.
    pub(crate) source: Option<ViewSource>,
    pub(crate) anchor: u32,
    /// How many frames it names, for the pending row's words before the plan answers.
    pub(crate) frames: u32,
}

/// `merge.plan`'s parameters for `targets` with `options`: only what the person changed.
pub(crate) fn plan_params(targets: &Targets, options: &MergeOptions) -> Value {
    let mut params = json!({"targets": targets});
    add_options(&mut params, options);
    params
}

fn add_options(params: &mut Value, options: &MergeOptions) {
    if options.alignment != Toggle::Auto {
        params["alignment"] = json!(options.alignment.wire());
    }
    if options.deghost != Toggle::Auto {
        params["deghost"] = json!(options.deghost.wire());
    }
    if let Some(reference) = &options.reference {
        params["reference"] = json!(reference);
    }
    if !options.overrides.is_empty() {
        params["exposure_overrides"] = Value::Array(
            options
                .overrides
                .iter()
                .map(|(path, offset)| json!({"path": path, "offset_ev": offset}))
                .collect(),
        );
    }
}

/// `merge.create`'s parameters: an HDR merge of `request`'s frames as planned (`plan_id`), into
/// `folder`, joining the current development set for Pick merged.
pub(crate) fn create_params(
    request: &MergeRequest,
    plan_id: &str,
    folder: &CatalogFolderId,
    mutation: &MutationRequest,
) -> Value {
    let mut params = json!({
        "kind": "hdr",
        "targets": request.targets,
        "plan_id": plan_id,
        "folder_id": folder,
        "mutation": mutation,
    });
    add_options(&mut params, &request.options);
    if request.mode == MergeMode::Pick {
        params["add_to_development_set"] = json!(true);
    }
    params
}

/// `folder.create`'s parameters for the new folder a plan suggests.
pub(crate) fn suggested_folder_params(
    plan: &MergePlan,
    mutation: &MutationRequest,
) -> Option<Value> {
    match plan.suggested_folder.as_ref()? {
        FolderChoice::New { name, parent_id } => Some(
            crate::state::select_catalog::folder_create_params(name, parent_id.as_ref(), mutation),
        ),
        FolderChoice::Existing { .. } => None,
    }
}

/// `merge.remove`'s or `merge.restore`'s parameters for one merge.
pub(crate) fn lifecycle_params(merge: &MergeId, mutation: &MutationRequest) -> Value {
    json!({"merge_ids": [merge], "mutation": mutation})
}

/// `stack.set-expanded`'s parameters for the view at `revision`.
pub(crate) fn expanded_params(merge: &MergeId, expansion: StackExpansion, revision: u64) -> Value {
    json!({"merge_id": merge, "expansion": expansion.as_str(), "revision": revision})
}

/// How a plan refuses: its refusal's message, or what keeps it from merging.
pub(crate) fn plan_refusal(plan: &MergePlan) -> Option<String> {
    if !plan.suitable {
        return Some(
            plan.refusal
                .as_ref()
                .and_then(|refusal| refusal["message"].as_str())
                .unwrap_or("These frames are not a bracket the merge can take")
                .to_owned(),
        );
    }
    if !plan.provider.available {
        return Some(
            plan.provider
                .reason
                .clone()
                .unwrap_or_else(|| "HDR merging is not available".to_owned()),
        );
    }
    // An exact active match claims its own inputs: that is a reuse, not an overlap.
    let reused = plan.existing.as_ref().map(|existing| &existing.merge_id);
    if let Some(claim) = plan
        .claimed
        .iter()
        .find(|claim| Some(&claim.merge_id) != reused)
    {
        let name = claim.path.file_name().map_or_else(
            || claim.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        return Some(format!(
            "{name} is in an HDR stack already: remove that merge first"
        ));
    }
    None
}

/// What a start of a merge came to, from its owner task.
#[derive(Clone, Debug)]
pub(crate) enum Started {
    /// The job runs (or, for an exact match, answered at once), named by its id.
    Job { job_id: String },
    /// The plan refuses, with the core's reason.
    Refused(String),
    /// This exact merge is in Removed: offered as Put back merge.
    Removed {
        merge_id: MergeId,
        result_asset_id: AssetId,
    },
}

/// The one merge this desktop started and is following: its job and request, how far its job
/// has got, and, once it ended without its result, why.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingMerge {
    pub(crate) job: String,
    pub(crate) request: MergeRequest,
    pub(crate) fraction: Option<f64>,
    pub(crate) message: Option<String>,
    /// Cancel was pressed: the row says so until the job's end arrives.
    pub(crate) cancelling: bool,
    /// The job ended without a result: the row says why until it is dismissed.
    pub(crate) failed: Option<String>,
}

impl PendingMerge {
    pub(crate) fn new(job: String, request: MergeRequest) -> Self {
        Self {
            job,
            request,
            fraction: None,
            message: None,
            cancelling: false,
            failed: None,
        }
    }

    /// The row's title: what is merged, in the mode asked.
    pub(crate) fn title(&self) -> String {
        format!(
            "{} \u{b7} {} exposures",
            if self.failed.is_some() {
                "HDR merge failed"
            } else {
                "Merging HDR"
            },
            self.request.frames
        )
    }

    /// The row's detail: the job's own words for where it is, its failure, or that it waits.
    pub(crate) fn detail(&self) -> String {
        if let Some(failed) = &self.failed {
            return failed.clone();
        }
        if self.cancelling {
            return "Cancelling\u{2026}".to_owned();
        }
        self.message
            .clone()
            .unwrap_or_else(|| "Waiting to start".to_owned())
    }

    /// Take in a running job's record: its progress.
    pub(crate) fn observe(&mut self, record: &Value) {
        let progress = &record["progress"];
        self.fraction = progress["fraction"].as_f64();
        self.message = progress["message"].as_str().map(str::to_owned);
    }
}

/// Why a job record ended without a result, naming the input or the stage it failed at: the file
/// the error's data names, or else the stage its last progress reported.
pub(crate) fn failure_text(record: &Value) -> String {
    let error = &record["error"];
    let message = error["message"].as_str().unwrap_or("the merge failed");
    if let Some(path) = error["data"]["path"].as_str() {
        let name = std::path::Path::new(path)
            .file_name()
            .map_or(path.to_owned(), |name| name.to_string_lossy().into_owned());
        if !message.contains(&name) {
            return format!("{name}: {message}");
        }
        return message.to_owned();
    }
    match record["result"]["stage"]["stage"].as_str() {
        Some(stage) => format!("While {stage}: {message}"),
        None => message.to_owned(),
    }
}

/// What the status bar says once a merge's job ended `ready`.
pub(crate) fn finished_text(answer: &MergeAnswer, mode: MergeMode) -> String {
    match (answer.reused, mode) {
        (true, MergeMode::Pick) => format!(
            "{} is this merge already \u{b7} in the Development set",
            answer.file_name
        ),
        (true, MergeMode::Import) => format!("{} is this merge already", answer.file_name),
        (false, MergeMode::Pick) => format!(
            "Merged {} \u{b7} added to the Development set \u{b7} Undo \u{2318}Z",
            answer.file_name
        ),
        (false, MergeMode::Import) => {
            format!("Merged {} \u{b7} Undo \u{2318}Z", answer.file_name)
        }
    }
}

/// The Merge options… form: the frames and mode it was opened for, the plan it shows, what the
/// person changed, and a refusal from planning again.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MergeForm {
    pub(crate) request: MergeRequest,
    /// The plan for the options as they are; `None` while it is being made.
    pub(crate) plan: Option<MergePlan>,
    pub(crate) refusal: Option<String>,
    /// The catalog's folders, for choosing another, and whether their menu is open.
    pub(crate) folders: Vec<(CatalogFolderId, String)>,
    pub(crate) folder_menu: bool,
}

/// A merge in Removed that the person can put back: offered after a merge request finds it, and
/// in a removed result's menu.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PutBack {
    pub(crate) merge_id: MergeId,
    pub(crate) result_asset_id: AssetId,
}

/// The menu a right-click on the grid opened over files: the merge actions for a selection of
/// frames, or a stack's actions for a result.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MergeMenu {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) kind: MergeMenuKind,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MergeMenuKind {
    /// `count` frames selected, and once their plan answers, why they cannot merge, in the core's
    /// words.
    Frames { count: u32, refusal: Option<String> },
    /// A result leading its stack, as its row says.
    Stack {
        merge_id: MergeId,
        exposures: u32,
        expansion: StackExpansion,
        filtered: bool,
    },
}

/// The Select workspace's merge state.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SelectMerge {
    pub(crate) form: Option<MergeForm>,
    /// The merge being started: its plan, folder and create in one owner task.
    pub(crate) starting: Option<MergeRequest>,
    pub(crate) pending: Option<PendingMerge>,
    pub(crate) put_back: Option<PutBack>,
    pub(crate) menu: Option<MergeMenu>,
}

impl SelectMerge {
    /// The running merge's job, for its `job.wait` reader.
    pub(crate) fn running_job(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .filter(|pending| pending.failed.is_none())
            .map(|pending| pending.job.as_str())
    }
}

// -- The model the views draw -------------------------------------------------------------------

/// A merge gesture of the menu, the form, the pending row or the Info panel.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MergeAction {
    /// The selection's Pick merged or Import merged, with the defaults.
    Start(MergeMode),
    /// Merge options… for the selection.
    Options,
    /// The form's Pick merged or Import merged, with its options.
    FormStart(MergeMode),
    FormClose,
    Alignment(Toggle),
    Deghost(Toggle),
    Reference(PathBuf),
    /// A frame's EV offset stepped to this, or its override taken away.
    Exposure {
        path: PathBuf,
        offset: Option<f64>,
    },
    FolderMenu(bool),
    Folder(FolderPick),
    /// The pending row's Cancel, or Dismiss once it failed.
    Cancel,
    /// Show a stack collapsed, expanded or with every original.
    Expand(MergeId, StackExpansion),
    /// Remove merge and Put back merge.
    Remove(MergeId),
    PutBack(MergeId),
    DismissPutBack,
    CloseMenu,
}

/// One entry of the merge menu: its label, its hint under it, and what it does or why not.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MenuEntryModel {
    pub(crate) label: String,
    pub(crate) hint: Option<&'static str>,
    pub(crate) trailing: Option<String>,
    pub(crate) action: Option<MergeAction>,
    pub(crate) reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MenuModel {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) entries: Vec<MenuEntryModel>,
}

/// One frame of the form: its name, its exposure as planned or set, whether it is the reference.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FrameRow {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    /// "−2 EV", or "measured from the pixels" while the metadata cannot say.
    pub(crate) exposure: String,
    /// The offset the steppers move from: the override, or the planned one.
    pub(crate) offset: Option<f64>,
    pub(crate) overridden: bool,
    pub(crate) reference: bool,
}

/// The Merge options… form as drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FormModel {
    pub(crate) title: String,
    pub(crate) frames: Vec<FrameRow>,
    pub(crate) alignment: Toggle,
    pub(crate) deghost: Toggle,
    pub(crate) folder: String,
    /// The folders to choose from while the folder menu is open.
    pub(crate) folder_choices: Option<Vec<(String, FolderPick)>>,
    /// Why the frames cannot merge with these options, from the plan.
    pub(crate) refusal: Option<String>,
    pub(crate) planning: bool,
    /// The buttons work: a suitable plan, no merge already running.
    pub(crate) enabled: bool,
}

/// The notice offering Put back merge for a merge found in Removed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PutBackModel {
    pub(crate) merge_id: MergeId,
    pub(crate) body: String,
}

/// What the views draw of merges.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MergeModel {
    pub(crate) menu: Option<MenuModel>,
    pub(crate) form: Option<FormModel>,
    pub(crate) put_back: Option<PutBackModel>,
}

fn entry(label: impl Into<String>, action: Option<MergeAction>) -> MenuEntryModel {
    MenuEntryModel {
        label: label.into(),
        hint: None,
        trailing: None,
        action,
        reason: None,
    }
}

/// The merge state's model.
pub(crate) fn derive(merge: &SelectMerge) -> MergeModel {
    // A merge already starting or running: one at a time from this desktop.
    let busy = merge.starting.is_some() || merge.running_job().is_some();
    let busy_reason = || "A merge is running: wait for it or cancel it".to_owned();
    let menu = merge.menu.as_ref().map(|menu| {
        let entries = match &menu.kind {
            MergeMenuKind::Frames { count, refusal } => {
                let mut entries = Vec::new();
                let reason = if busy {
                    Some(busy_reason())
                } else {
                    refusal.clone()
                };
                for mode in [MergeMode::Pick, MergeMode::Import] {
                    let mut item = entry(
                        mode.label(),
                        reason.is_none().then_some(MergeAction::Start(mode)),
                    );
                    item.hint = Some(mode.hint());
                    item.reason = reason.clone();
                    entries.push(item);
                }
                let mut options = entry(
                    "Merge options\u{2026}",
                    refusal.is_none().then_some(MergeAction::Options),
                );
                options.trailing = Some(format!("{count} frames"));
                entries.push(options);
                entries
            }
            MergeMenuKind::Stack {
                merge_id,
                exposures,
                expansion,
                filtered,
            } => {
                let open = *expansion != StackExpansion::Collapsed;
                let mut toggle = entry(
                    if open {
                        "Collapse stack"
                    } else {
                        "Expand stack"
                    },
                    Some(MergeAction::Expand(
                        merge_id.clone(),
                        if open {
                            StackExpansion::Collapsed
                        } else {
                            StackExpansion::Expanded
                        },
                    )),
                );
                toggle.trailing = Some("S".to_owned());
                let mut entries = vec![toggle];
                if *filtered && *expansion != StackExpansion::All {
                    entries.push(entry(
                        format!("Show all {exposures} originals"),
                        Some(MergeAction::Expand(merge_id.clone(), StackExpansion::All)),
                    ));
                }
                entries.push(entry(
                    "Remove merge",
                    Some(MergeAction::Remove(merge_id.clone())),
                ));
                entries
            }
        };
        MenuModel {
            x: menu.x,
            y: menu.y,
            entries,
        }
    });
    let form = merge.form.as_ref().map(|form| form_model(form, busy));
    let put_back = merge.put_back.as_ref().map(|put_back| PutBackModel {
        merge_id: put_back.merge_id.clone(),
        body: "This exact merge is in Removed: put it back rather than merging it again".to_owned(),
    });
    MergeModel {
        menu,
        form,
        put_back,
    }
}

fn form_model(form: &MergeForm, busy: bool) -> FormModel {
    let options = &form.request.options;
    let plan = form.plan.as_ref();
    let frames = plan.map_or_else(Vec::new, |plan| {
        plan.inputs
            .iter()
            .enumerate()
            .map(|(at, input)| {
                let overridden = options.override_of(&input.path);
                let planned = plan
                    .exposures
                    .as_ref()
                    .and_then(|exposures| exposures.inputs.get(at))
                    .map(|exposure| exposure.offset_ev);
                let offset = overridden.or(planned);
                let exposure = match offset {
                    Some(offset) => {
                        ev_words(offset).map_or_else(|| format!("{offset:+.2} EV"), str::to_owned)
                    }
                    None => "measured from the pixels".to_owned(),
                };
                let reference = match &options.reference {
                    Some(reference) => *reference == input.path,
                    None => plan.reference == Some(input.ordinal),
                };
                FrameRow {
                    path: input.path.clone(),
                    name: input.file_name.clone(),
                    exposure,
                    offset,
                    overridden: overridden.is_some(),
                    reference,
                }
            })
            .collect()
    });
    let proposed = plan.map(|plan| match (&plan.folder, &plan.suggested_folder) {
        (Some(folder), _) => folder.name.clone(),
        (None, Some(FolderChoice::New { name, .. })) => {
            format!("New folder \u{201c}{name}\u{201d}")
        }
        _ => plan
            .folder_reason
            .clone()
            .unwrap_or_else(|| "Choose a folder".to_owned()),
    });
    let folder = match &options.folder {
        FolderPick::Existing { name, .. } => name.clone(),
        FolderPick::Proposed => proposed.clone().unwrap_or_else(|| "\u{2026}".to_owned()),
    };
    let folder_choices = form.folder_menu.then(|| {
        let mut choices = Vec::with_capacity(form.folders.len() + 1);
        if let Some(proposed) = &proposed {
            choices.push((proposed.clone(), FolderPick::Proposed));
        }
        for (folder_id, name) in &form.folders {
            choices.push((
                name.clone(),
                FolderPick::Existing {
                    folder_id: folder_id.clone(),
                    name: name.clone(),
                },
            ));
        }
        choices
    });
    let refusal = form.refusal.clone().or_else(|| plan.and_then(plan_refusal));
    FormModel {
        title: format!("Merge options \u{b7} {} exposures", form.request.frames),
        frames,
        alignment: options.alignment,
        deghost: options.deghost,
        folder,
        folder_choices,
        planning: plan.is_none() && form.refusal.is_none(),
        enabled: plan.is_some() && refusal.is_none() && !busy,
        refusal,
    }
}

// -- What a result and its originals show ----------------------------------------------------------

/// "HDR · N exposures" and "HDR · N" for N from 2 to [`MAX_FRAMES`], made once.
static STACK_WORDS: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    (0..=MAX_FRAMES)
        .map(|count| {
            (
                format!("HDR \u{b7} {count} exposures"),
                format!("HDR \u{b7} {count}"),
            )
        })
        .collect()
});

/// A stack's badge words for `exposures` frames: long and short.
pub(crate) fn stack_words(exposures: u32) -> (&'static str, &'static str) {
    match STACK_WORDS.get(exposures as usize) {
        Some((long, short)) => (long.as_str(), short.as_str()),
        None => ("HDR", "HDR"),
    }
}

/// An original's EV offset in sixths of a stop from −10 to +10 EV ("−2 EV", two thirds of a stop
/// "+0.7 EV"), as a bracket frame's footer says it, made once; an offset off that grid has none.
static EV_WORDS: LazyLock<Vec<String>> = LazyLock::new(|| {
    (-60..=60)
        .map(|sixths: i32| format!("{} EV", super::select::ev_step(sixths as f32 / 6.0)))
        .collect()
});

/// An original's footer label: its EV offset from the reference.
pub(crate) fn ev_words(offset: f64) -> Option<&'static str> {
    let sixths = (offset * 6.0).round();
    if (offset * 6.0 - sixths).abs() > 1e-6 || sixths.abs() > 60.0 {
        return None;
    }
    EV_WORDS
        .get((sixths as i32 + 60) as usize)
        .map(String::as_str)
}

/// What a stacked row's cell says beside its photograph: an original revealed by Show all is
/// outside the filter, an original with no file says why (`reason`, the row's), and an original
/// whose leader the filter left out says it is one.
pub(crate) fn stack_note<'r>(stack: &RowStack, reason: Option<&'r str>) -> Option<&'r str> {
    match stack.role {
        StackRole::Leader => None,
        StackRole::Original if reason.is_some() => reason,
        StackRole::Original if stack.outside_filter => Some("Outside the filter"),
        StackRole::Original => None,
        StackRole::Separate => Some("In an HDR stack"),
    }
}

/// What a grid cell draws for its row's stack: a leader's badge, an original's note and its EV
/// offset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct StackCell<'r> {
    /// A leader's badge words, long and short, and whether its originals are shown.
    pub(crate) badge: Option<(&'static str, &'static str, bool)>,
    pub(crate) note: Option<&'r str>,
    /// An original's EV offset from the reference, in words.
    pub(crate) exposure: Option<&'static str>,
}

/// The cell words of a row with `stack`, whose reason is `reason`.
pub(crate) fn stack_cell<'r>(stack: Option<&RowStack>, reason: Option<&'r str>) -> StackCell<'r> {
    let Some(stack) = stack else {
        return StackCell::default();
    };
    StackCell {
        badge: (stack.role == StackRole::Leader).then(|| {
            let (long, short) = stack_words(stack.exposures);
            (long, short, expanded(stack))
        }),
        note: stack_note(stack, reason),
        exposure: (stack.role == StackRole::Original)
            .then_some(stack.offset_ev)
            .flatten()
            .and_then(ev_words),
    }
}

/// The Info panel's stack band for a row: what the stack is and what can be done with it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StackBand {
    pub(crate) merge_id: MergeId,
    /// "HDR · 3 exposures", or for an original "Original 2 of 3 · −2 EV".
    pub(crate) title: String,
    pub(crate) note: Option<String>,
    pub(crate) leader: bool,
    /// A leader in the current development set.
    pub(crate) in_set: bool,
    /// Whether the stack's originals are shown: always, for an original.
    pub(crate) expanded: bool,
    /// The expansion its Expand or Collapse stack button asks for.
    pub(crate) toggle: StackExpansion,
}

/// The stack band of a row with `stack`.
pub(crate) fn stack_band(stack: &RowStack, in_set: bool, reason: Option<&str>) -> StackBand {
    let (long, _) = stack_words(stack.exposures);
    let leader = stack.role == StackRole::Leader;
    let title = if leader {
        long.to_owned()
    } else {
        let mut title = format!(
            "Original {} of {}",
            stack.ordinal.map_or(0, |ordinal| ordinal + 1),
            stack.exposures
        );
        if let Some(words) = stack.offset_ev.and_then(ev_words) {
            title = format!("{title} \u{b7} {words}");
        }
        title
    };
    let note = match stack.role {
        StackRole::Leader if stack.collapsed > 0 => Some(format!(
            "{} {} beneath it",
            stack.collapsed,
            if stack.collapsed == 1 {
                "original"
            } else {
                "originals"
            }
        )),
        _ => stack_note(stack, reason).map(str::to_owned),
    };
    let expanded = !leader || expanded(stack);
    StackBand {
        merge_id: stack.merge_id.clone(),
        title,
        note,
        leader,
        in_set: leader && in_set,
        expanded,
        toggle: if expanded {
            StackExpansion::Collapsed
        } else {
            StackExpansion::Expanded
        },
    }
}

/// Whether a stack's originals are shown beside its leader.
fn expanded(stack: &RowStack) -> bool {
    stack
        .expansion
        .is_some_and(|expansion| expansion != StackExpansion::Collapsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::catalog_types::FileId;

    fn request(mode: MergeMode) -> MergeRequest {
        MergeRequest {
            targets: Targets::Files {
                file_ids: vec![FileId(1), FileId(2), FileId(3)],
            },
            mode,
            options: MergeOptions::default(),
            source: None,
            anchor: 0,
            frames: 3,
        }
    }

    /// The defaults send nothing but the frames; every option changed is sent as an agent writes
    /// it, and Pick merged alone joins the development set.
    #[test]
    fn hdr_merge_requests_send_only_what_was_chosen() {
        let mutation = MutationRequest {
            request_id: "r".into(),
            actor: "desktop".into(),
        };
        let folder = CatalogFolderId::new();
        let pick = request(MergeMode::Pick);
        assert_eq!(
            plan_params(&pick.targets, &pick.options),
            json!({"targets": {"kind": "files", "file_ids": [1, 2, 3]}})
        );
        assert_eq!(
            create_params(&pick, "plan", &folder, &mutation),
            json!({
                "kind": "hdr",
                "targets": {"kind": "files", "file_ids": [1, 2, 3]},
                "plan_id": "plan",
                "folder_id": folder,
                "add_to_development_set": true,
                "mutation": {"request_id": "r", "actor": "desktop"},
            })
        );
        let mut import = request(MergeMode::Import);
        import.options = MergeOptions {
            alignment: Toggle::Off,
            deghost: Toggle::Off,
            reference: Some("/a/2.NEF".into()),
            overrides: vec![("/a/1.NEF".into(), -2.0)],
            folder: FolderPick::Proposed,
        };
        let params = create_params(&import, "plan", &folder, &mutation);
        assert_eq!(params["alignment"], "off");
        assert_eq!(params["deghost"], "off");
        assert_eq!(params["reference"], "/a/2.NEF");
        assert_eq!(
            params["exposure_overrides"],
            json!([{"path": "/a/1.NEF", "offset_ev": -2.0}])
        );
        assert!(params.get("add_to_development_set").is_none());
    }

    #[test]
    fn hdr_merge_words_are_made_once_and_name_the_input_or_stage() {
        assert_eq!(stack_words(3), ("HDR \u{b7} 3 exposures", "HDR \u{b7} 3"));
        assert_eq!(stack_words(40), ("HDR", "HDR"));
        assert_eq!(ev_words(-2.0), Some("\u{2212}2 EV"));
        assert_eq!(ev_words(0.0), Some("0 EV"));
        assert_eq!(ev_words(0.123), None);
        let failed = json!({
            "status": "failed",
            "error": {"code": "unsupported-input", "message": "it could not be developed",
                      "data": {"path": "/x/DSC_0002.NEF"}},
        });
        assert_eq!(
            failure_text(&failed),
            "DSC_0002.NEF: it could not be developed"
        );
        let staged = json!({
            "status": "failed",
            "error": {"code": "internal", "message": "out of memory"},
            "result": {"stage": {"stage": "aligning"}},
        });
        assert_eq!(failure_text(&staged), "While aligning: out of memory");
    }
}
