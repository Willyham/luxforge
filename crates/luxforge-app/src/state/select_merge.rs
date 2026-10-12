//! Merges in the Select workspace ([HDR merge design](../../../../docs/design/hdr-merge.md),
//! "Grid workflow"; the [merge preview](../../../../docs/design/panorama-merge.md#the-merge-preview)),
//! as plain data: the requests Merge HDR sends, the merge preview sheet that
//! opens first ([`sheet`]), the one pending merge row this desktop draws while its job runs, a
//! merge's end in words, and the words a result's stack badge and its originals' notes show. Every
//! decision is the owner's: the plan says whether the frames merge and why not, the preview job
//! draws the merge and finds its refusals, the merge job says how far it has got and how it ended,
//! and a view's rows say which result leads which originals. Every word that names a kind comes
//! from the request's, the plan's, the claim's or the row's `kind` ([`words`]); a bracket's header
//! and the selection menu's merge group are HDR's, the only kind with a provider.
pub(crate) mod sheet;
#[cfg(test)]
mod tests;

use luxforge_core::{
    AssetId, MutationRequest,
    catalog_types::{
        CatalogFolderId, FolderChoice, MergeAnswer, MergeId, MergeKind, MergePlan, MergePreviewId,
        RowStack, StackExpansion, StackRole, Targets, ViewSource,
    },
    merge::hdr::{AlignmentMode, DeghostMode},
    preferences::HdrMergePreference,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::LazyLock};

pub(crate) use sheet::{PreviewSheet, SheetModel};

/// How Select names a merge kind. HDR's are the words HDR has always shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KindWords {
    /// "HDR", "Panorama": a result's badge, its Info band, a failed merge's row and the selection
    /// menu's group.
    pub(crate) title: &'static str,
    /// "HDR", "Pano": the badge where the long words do not fit.
    pub(crate) short: &'static str,
    /// "HDR", "panorama": the kind within a sentence ("Merging HDR", "Merge HDR").
    pub(crate) noun: &'static str,
    /// "an HDR stack", "a panorama stack".
    pub(crate) stack: &'static str,
    /// "In an HDR stack": an original shown apart from its stack.
    pub(crate) separate: &'static str,
    /// "exposures", "photos": what a merge's N counts.
    pub(crate) frames: &'static str,
    /// "Original", "Frame": an original's place in its stack ("Original 2 of 3").
    pub(crate) original: &'static str,
    /// The one merge action, "Merge HDR", "Merge panorama": in a bracket's header, the selection
    /// menu and the sheet.
    pub(crate) action: &'static str,
    /// The merge action's hint in the selection's menu.
    pub(crate) hint: &'static str,
}

const HDR_WORDS: KindWords = KindWords {
    title: "HDR",
    short: "HDR",
    noun: "HDR",
    stack: "an HDR stack",
    separate: "In an HDR stack",
    frames: "exposures",
    original: "Original",
    action: "Merge HDR",
    hint: "Merge the frames into one HDR photograph in the catalog",
};

const PANORAMA_WORDS: KindWords = KindWords {
    title: "Panorama",
    short: "Pano",
    noun: "panorama",
    stack: "a panorama stack",
    separate: "In a panorama stack",
    frames: "photos",
    original: "Frame",
    action: "Merge panorama",
    hint: "Merge the photos into one panorama in the catalog",
};

/// The words Select names `kind` with.
pub(crate) const fn words(kind: MergeKind) -> &'static KindWords {
    match kind {
        MergeKind::Hdr => &HDR_WORDS,
        MergeKind::Panorama => &PANORAMA_WORDS,
    }
}

/// How a library change's merge stack is named when its row says the kind, and when no row does.
pub(crate) fn stack_name(kind: Option<MergeKind>) -> &'static str {
    kind.map_or("a merge stack", |kind| words(kind).stack)
}

/// The selection menu's merge item for `kind`: "Merge HDR…", whose ellipsis says the preview
/// opens first; without it while ⌥ is held, when a press merges at once with the last settings.
pub(crate) fn merge_item(kind: MergeKind, option: bool) -> String {
    let action = words(kind).action;
    if option {
        action.to_owned()
    } else {
        format!("{action}\u{2026}")
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

    fn alignment(mode: AlignmentMode) -> Self {
        match mode {
            AlignmentMode::Auto => Self::Auto,
            AlignmentMode::Off => Self::Off,
        }
    }

    fn deghost(mode: DeghostMode) -> Self {
        match mode {
            DeghostMode::Auto => Self::Auto,
            DeghostMode::Off => Self::Off,
        }
    }
}

/// Where the result goes: the plan's proposal (its existing folder, or the folders it suggests,
/// which `merge.create` makes), or a folder the person chose.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum FolderPick {
    #[default]
    Proposed,
    Existing {
        folder_id: CatalogFolderId,
        name: String,
    },
}

/// What a merge is asked with beside its frames; every field left alone is the core's default,
/// sent as nothing.
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
    /// The options of the person's last HDR merge, remembered: its alignment and deghosting, or
    /// the defaults when none is remembered. The folder comes from each merge's plan.
    pub(crate) fn last(remembered: Option<HdrMergePreference>) -> Self {
        let remembered = remembered.unwrap_or_default();
        Self {
            alignment: Toggle::alignment(remembered.alignment),
            deghost: Toggle::deghost(remembered.deghost),
            ..Self::default()
        }
    }

    /// What the desktop remembers of these options once a merge of them starts.
    pub(crate) fn remembered(&self) -> HdrMergePreference {
        HdrMergePreference {
            alignment: match self.alignment {
                Toggle::Auto => AlignmentMode::Auto,
                Toggle::Off => AlignmentMode::Off,
            },
            deghost: match self.deghost {
                Toggle::Auto => DeghostMode::Auto,
                Toggle::Off => DeghostMode::Off,
            },
        }
    }

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

    /// The kind's options object (`hdr`) as an agent writes it: every choice the person made,
    /// and nothing for a default. The same object goes to `merge.preview`, every
    /// `merge.preview-update` (whole, as the preview's options from then on) and `merge.create`,
    /// so a merge from a preview names exactly the options its newest revision drew.
    pub(crate) fn wire(&self) -> serde_json::Map<String, Value> {
        let mut chosen = serde_json::Map::new();
        if self.alignment != Toggle::Auto {
            chosen.insert("alignment".into(), json!(self.alignment.wire()));
        }
        if self.deghost != Toggle::Auto {
            chosen.insert("deghost".into(), json!(self.deghost.wire()));
        }
        if let Some(reference) = &self.reference {
            chosen.insert("reference".into(), json!(reference));
        }
        if !self.overrides.is_empty() {
            chosen.insert(
                "exposure_overrides".into(),
                Value::Array(
                    self.overrides
                        .iter()
                        .map(|(path, offset)| json!({"path": path, "offset_ev": offset}))
                        .collect(),
                ),
            );
        }
        chosen
    }
}

/// One merge request as the desktop makes it: which frames, the options, and where
/// in the view it was asked for, so the pending row is drawn there.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MergeRequest {
    /// The kind of merge the gesture asked for.
    pub(crate) kind: MergeKind,
    pub(crate) targets: Targets,
    pub(crate) options: MergeOptions,
    /// The view the gesture was made in and the position of its first frame.
    pub(crate) source: Option<ViewSource>,
    pub(crate) anchor: u32,
    /// How many frames it names, for the pending row's words before the plan answers.
    pub(crate) frames: u32,
}

/// `merge.plan`'s parameters for a merge of `kind` of `targets` with `options`: only what the
/// person changed.
pub(crate) fn plan_params(kind: MergeKind, targets: &Targets, options: &MergeOptions) -> Value {
    let mut params = json!({"kind": kind, "targets": targets});
    add_options(&mut params, kind, options);
    params
}

/// The options the person changed, under the kind's own field (`hdr`): none is sent as nothing.
fn add_options(params: &mut Value, kind: MergeKind, options: &MergeOptions) {
    let chosen = options.wire();
    if !chosen.is_empty() {
        params[kind.as_str()] = Value::Object(chosen);
    }
}

/// The folder choice `merge.create` is sent: the folder the person chose, otherwise the plan's
/// proposal — its folder when every folder of it exists, else the choice it suggests, whose
/// folders the merge makes. A plan proposing neither sends none, and the core decides.
pub(crate) fn folder_choice(options: &MergeOptions, plan: &MergePlan) -> Option<FolderChoice> {
    match &options.folder {
        FolderPick::Existing { folder_id, .. } => Some(FolderChoice::Existing {
            folder_id: folder_id.clone(),
            days: false,
        }),
        FolderPick::Proposed => match (&plan.folder, &plan.suggested_folder) {
            (Some(proposal), _) => Some(FolderChoice::Existing {
                folder_id: proposal.folder_id.clone(),
                days: false,
            }),
            (None, suggested) => suggested.clone(),
        },
    }
}

/// `merge.create`'s parameters: a merge of `request`'s kind and frames into `folder`, or the
/// plan's proposal without one. From a preview it names the preview, which it continues, and no
/// plan: the preview's latest revision is what it merges. Otherwise it names the plan it was
/// planned as.
pub(crate) fn create_params(
    request: &MergeRequest,
    from: CreateFrom<'_>,
    folder: Option<&FolderChoice>,
    mutation: &MutationRequest,
) -> Value {
    let mut params = json!({
        "kind": request.kind,
        "targets": request.targets,
        "mutation": mutation,
    });
    match from {
        CreateFrom::Plan(plan_id) => params["plan_id"] = json!(plan_id),
        CreateFrom::Preview(preview_id) => params["preview_id"] = json!(preview_id),
    }
    if let Some(folder) = folder {
        params["folder"] = json!(folder);
    }
    add_options(&mut params, request.kind, &request.options);
    params
}

/// What a `merge.create` continues: the plan it was planned as, or the open preview.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CreateFrom<'a> {
    Plan(&'a str),
    Preview(&'a MergePreviewId),
}

/// `merge.preview`'s parameters: a preview of `request`'s kind and frames with its options, of
/// the plan `plan_id` made for them.
pub(crate) fn preview_params(request: &MergeRequest, plan_id: Option<&str>) -> Value {
    let mut params = json!({"kind": request.kind, "targets": request.targets});
    if let Some(plan_id) = plan_id {
        params["plan_id"] = json!(plan_id);
    }
    add_options(&mut params, request.kind, &request.options);
    params
}

/// `merge.preview-update`'s parameters: the preview's options from now on, whole, under the
/// kind's field; an empty object is every default.
pub(crate) fn preview_update_params(
    kind: MergeKind,
    preview_id: &MergePreviewId,
    options: &MergeOptions,
) -> Value {
    let mut params = json!({"preview_id": preview_id});
    params[kind.as_str()] = Value::Object(options.wire());
    params
}

/// `merge.preview-close`'s parameters.
pub(crate) fn preview_close_params(preview_id: &MergePreviewId) -> Value {
    json!({"preview_id": preview_id})
}

/// `merge.remove`'s or `merge.restore`'s parameters for one merge.
pub(crate) fn lifecycle_params(merge: &MergeId, mutation: &MutationRequest) -> Value {
    json!({"merge_ids": [merge], "mutation": mutation})
}

/// `stack.set-expanded`'s parameters for the view at `revision`.
pub(crate) fn expanded_params(merge: &MergeId, expansion: StackExpansion, revision: u64) -> Value {
    json!({"merge_id": merge, "expansion": expansion.as_str(), "revision": revision})
}

/// The message of a typed refusal's `data` or object, or of a refusal with none.
pub(crate) fn refusal_message(refusal: Option<&Value>, fallback: &str) -> String {
    refusal
        .and_then(|refusal| refusal["message"].as_str())
        .unwrap_or(fallback)
        .to_owned()
}

/// How a plan refuses: its refusal's message, or what keeps it from merging.
pub(crate) fn plan_refusal(plan: &MergePlan) -> Option<String> {
    if !plan.suitable {
        return Some(refusal_message(
            plan.refusal.as_ref(),
            "These frames are not a bracket the merge can take",
        ));
    }
    if !plan.provider.available {
        return Some(
            plan.provider
                .reason
                .clone()
                .unwrap_or_else(|| format!("{} merging is not available", plan.kind.noun())),
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
            "{name} is in {} already: remove that merge first",
            words(claim.kind).stack
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

    /// The row's title: what is merged.
    pub(crate) fn title(&self) -> String {
        let words = words(self.request.kind);
        let doing = if self.failed.is_some() {
            format!("{} merge failed", words.title)
        } else {
            format!("Merging {}", words.noun)
        };
        format!("{doing} \u{b7} {} {}", self.request.frames, words.frames)
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
pub(crate) fn finished_text(answer: &MergeAnswer) -> String {
    if answer.reused {
        format!("{} is this merge already", answer.file_name)
    } else {
        format!("Merged {} \u{b7} Undo \u{2318}Z", answer.file_name)
    }
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
    /// `count` frames selected for a merge of `kind`, and once their plan answers, why they
    /// cannot merge, in the core's words.
    Frames {
        kind: MergeKind,
        count: u32,
        refusal: Option<String>,
    },
    /// A result leading its stack of `size` originals, as its row says.
    Stack {
        merge_id: MergeId,
        kind: MergeKind,
        size: u32,
        expansion: StackExpansion,
        filtered: bool,
    },
}

/// The Select workspace's merge state.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SelectMerge {
    /// The merge preview sheet, while it is open.
    pub(crate) sheet: Option<PreviewSheet>,
    /// The merge being started: its plan, folder and create in one owner task.
    pub(crate) starting: Option<MergeRequest>,
    pub(crate) pending: Option<PendingMerge>,
    pub(crate) put_back: Option<PutBack>,
    pub(crate) menu: Option<MergeMenu>,
    /// ⌥ is held: the menu's items drop their ellipsis, and a press on a merge action merges at
    /// once with the last settings rather than opening the preview.
    pub(crate) option: bool,
}

impl SelectMerge {
    /// The running merge's job, for its `job.wait` reader.
    pub(crate) fn running_job(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .filter(|pending| pending.failed.is_none())
            .map(|pending| pending.job.as_str())
    }

    /// The open preview's job while it prepares or redraws, for its `job.wait` reader.
    pub(crate) fn preview_job(&self) -> Option<&str> {
        self.sheet.as_ref().and_then(|sheet| sheet.job.as_deref())
    }

    /// A merge of this desktop's is starting or running: one at a time.
    pub(crate) fn busy(&self) -> bool {
        self.starting.is_some() || self.running_job().is_some()
    }
}

/// Why a merge cannot start while another of this desktop's runs.
pub(crate) const BUSY: &str = "A merge is running: wait for it or cancel it";

// -- The model the views draw -------------------------------------------------------------------

/// A merge gesture of the menu, the header, the sheet, the pending row or the Info panel.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MergeAction {
    /// The selection's Merge HDR of a kind: the preview, or with ⌥ held a merge at once with the
    /// last settings.
    Start(MergeKind),
    /// The sheet's Merge, with its options.
    SheetMerge,
    /// The sheet's Cancel, its close button, Escape or a press beside it.
    SheetClose,
    Alignment(Toggle),
    Deghost(Toggle),
    Reference(PathBuf),
    /// A frame's EV offset stepped to this, or its override taken away.
    Exposure {
        path: PathBuf,
        offset: Option<f64>,
    },
    /// Show deghost overlay.
    Overlay(bool),
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
    /// The group's name over its entries ("HDR"), for a selection's merges.
    pub(crate) heading: Option<&'static str>,
    pub(crate) entries: Vec<MenuEntryModel>,
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
    pub(crate) sheet: Option<SheetModel>,
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
    let busy = merge.busy();
    let menu = merge.menu.as_ref().map(|menu| {
        let mut heading = None;
        let entries = match &menu.kind {
            MergeMenuKind::Frames { kind, refusal, .. } => {
                heading = Some(words(*kind).title);
                let reason = if busy {
                    Some(BUSY.to_owned())
                } else {
                    refusal.clone()
                };
                let mut item = entry(
                    merge_item(*kind, merge.option),
                    reason.is_none().then_some(MergeAction::Start(*kind)),
                );
                item.hint = Some(words(*kind).hint);
                item.reason = reason;
                vec![item]
            }
            MergeMenuKind::Stack {
                merge_id,
                size,
                expansion,
                filtered,
                ..
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
                        format!("Show all {size} originals"),
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
            heading,
            entries,
        }
    });
    let sheet = merge
        .sheet
        .as_ref()
        .map(|sheet| sheet::model(sheet, busy.then_some(BUSY)));
    let put_back = merge.put_back.as_ref().map(|put_back| PutBackModel {
        merge_id: put_back.merge_id.clone(),
        body: "This exact merge is in Removed: put it back rather than merging it again".to_owned(),
    });
    MergeModel {
        menu,
        sheet,
        put_back,
    }
}

// -- What a result and its originals show ----------------------------------------------------------

/// Each kind's "HDR · N exposures" and "HDR · N" for N up to the most inputs the kind takes, in
/// [`MergeKind::ALL`]'s order, made once.
static STACK_WORDS: LazyLock<Vec<Vec<(String, String)>>> = LazyLock::new(|| {
    MergeKind::ALL
        .into_iter()
        .map(|kind| {
            let words = words(kind);
            (0..=*kind.inputs().end())
                .map(|size| {
                    (
                        format!("{} \u{b7} {size} {}", words.title, words.frames),
                        format!("{} \u{b7} {size}", words.short),
                    )
                })
                .collect()
        })
        .collect()
});

/// A stack's badge words for a merge of `kind` of `size` originals: long and short.
pub(crate) fn stack_words(kind: MergeKind, size: u32) -> (&'static str, &'static str) {
    let at = MergeKind::ALL.iter().position(|each| *each == kind);
    match at.and_then(|at| STACK_WORDS[at].get(size as usize)) {
        Some((long, short)) => (long.as_str(), short.as_str()),
        None => (words(kind).title, words(kind).short),
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
        StackRole::Separate => Some(words(stack.kind).separate),
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
            let (long, short) = stack_words(stack.kind, stack.size);
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
    /// Whether the stack's originals are shown: always, for an original.
    pub(crate) expanded: bool,
    /// The expansion its Expand or Collapse stack button asks for.
    pub(crate) toggle: StackExpansion,
}

/// The stack band of a row with `stack`.
pub(crate) fn stack_band(stack: &RowStack, reason: Option<&str>) -> StackBand {
    let (long, _) = stack_words(stack.kind, stack.size);
    let leader = stack.role == StackRole::Leader;
    let title = if leader {
        long.to_owned()
    } else {
        let mut title = format!(
            "{} {} of {}",
            words(stack.kind).original,
            stack.ordinal.map_or(0, |ordinal| ordinal + 1),
            stack.size
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
