//! Remove, the Corrections band's fill mode (`docs/design/ai-editing.md`, "Desktop surface"): the
//! selection a person paints, the tiers a fill module offers, each fill job this desktop started
//! and the candidates it published, and the band model drawn from them.
//!
//! Nothing here names a tool. The module that fills is whichever available module declares a task
//! whose id starts `fill-` ([`fill_module`]); the tiers, whether each is offered and why not, come
//! from its own `module.status` answer (`fill_tiers`), and every candidate's label from the job
//! that published it. Everything here is plain data the app layer turns into requests; nothing is
//! a request until a gesture sends one.
use crate::state::{
    Inputs,
    fill_setup::{self, SetupLine, SetupModel},
    settings,
};
use luxforge_core::{
    AssetId, CandidateRead, CandidateState, ModuleDescriptor, ParameterKind, flags::FlagList,
    jobs::JobStatus,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// The prefix of every fill tier's task id: `fill-generative`.
pub(crate) const FILL_TASK_PREFIX: &str = "fill-";
/// The most characters a prompt holds when the task declares no bound of its own.
const PROMPT_FALLBACK: usize = 256;
/// The most candidates the strip lists: the newest, across the selection's jobs.
pub(crate) const STRIP_MAX: usize = 12;

/// The first available module that declares a fill task the desktop offers, the one the Remove
/// mode drives. A fill task behind an experiment flag that is off is not offered
/// ([`TaskDescriptor::flags`](luxforge_core::capabilities::descriptor::TaskDescriptor::flags)), so
/// with every fill task's flag off there is no Remove at all: no segment, no gestures.
pub(crate) fn fill_module<'a>(
    modules: &'a [ModuleDescriptor],
    flags: Option<&FlagList>,
) -> Option<&'a ModuleDescriptor> {
    modules
        .iter()
        .filter(|module| module.is_available())
        .find(|module| {
            module.tasks.iter().any(|task| {
                task.id.starts_with(FILL_TASK_PREFIX) && settings::task_offered(flags, task)
            })
        })
}

/// The tiers of `module`'s status the desktop lists: those whose fill task it offers. A tier
/// whose task is behind a flag that is off is left out rather than named as off: the flag is the
/// person's choice not to see the experiment.
pub(crate) fn shown_tiers(
    module: &ModuleDescriptor,
    tiers: &[FillTier],
    flags: Option<&FlagList>,
) -> Vec<FillTier> {
    tiers
        .iter()
        .filter(|tier| {
            module
                .task(task_id(&tier.task))
                .is_some_and(|task| settings::task_offered(flags, task))
        })
        .cloned()
        .collect()
}

/// A tier's task as its module declares it: `fill-generative` for `task.fill-generative`.
fn task_id(method: &str) -> &str {
    method
        .strip_prefix(luxforge_core::capabilities::host::TASK_PREFIX)
        .unwrap_or(method)
}

/// How many characters the fill task's `prompt` takes, as it declares.
pub(crate) fn prompt_max(module: &ModuleDescriptor, task: &str) -> usize {
    module
        .task(task_id(task))
        .and_then(|task| task.parameters.iter().find(|p| p.name == "prompt"))
        .and_then(|parameter| match &parameter.kind {
            ParameterKind::String { max_length } => Some(*max_length),
            _ => None,
        })
        .unwrap_or(PROMPT_FALLBACK)
}

/// One thing a tier lacks, as `fill_tiers[].missing` names it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Missing {
    pub(crate) kind: String,
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) state: String,
}

/// One file of a tier's model: its resource and install state.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct TierFile {
    pub(crate) resource: String,
    pub(crate) state: String,
}

/// One model a tier fills with.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct TierModel {
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) title: String,
    pub(crate) resource: String,
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) text_encoder: Option<TierFile>,
}

/// One entry of `module.status`'s `fill_tiers`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct FillTier {
    pub(crate) tier: String,
    /// The method that fills with it: `task.fill-generative`.
    pub(crate) task: String,
    pub(crate) offered: bool,
    #[serde(default)]
    pub(crate) missing: Vec<Missing>,
    #[serde(default)]
    pub(crate) models: Vec<TierModel>,
}

impl FillTier {
    /// The model a request fills with: the first installed one.
    pub(crate) fn installed_model(&self) -> Option<&TierModel> {
        self.models.iter().find(|model| model.state == "installed")
    }
}

/// A tier as the Fill setting titles it: its name with the first letter upper-cased.
pub(crate) fn tier_title(tier: &str) -> String {
    crate::state::corrections::kind_title(tier)
}

/// The painted selection: one stroke of the Corrections brush on one photograph, the radius it was
/// painted at, and the seed its first Generate starts from, drawn once per selection so Generate is
/// reproducible and its request copyable.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoveSelection {
    pub(crate) asset: AssetId,
    /// The stroke as posted: the host's own stored path at `radius`.
    pub(crate) points: Vec<[f64; 2]>,
    pub(crate) radius: f64,
    pub(crate) seed: u64,
    /// Which selection this is, counted per window: a new stroke starts a new strip.
    pub(crate) serial: u64,
}

/// One fill job this desktop started, as its last read said.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FillJob {
    pub(crate) job_id: String,
    pub(crate) asset: AssetId,
    /// The selection it filled.
    pub(crate) serial: u64,
    pub(crate) prompt: Option<String>,
    pub(crate) seed: u64,
    pub(crate) status: JobStatus,
    pub(crate) fraction: Option<f64>,
    pub(crate) message: Option<String>,
    pub(crate) candidates: Vec<CandidateRead>,
    /// The seed Regenerate continues from, once the job is ready.
    pub(crate) next_seed: Option<u64>,
    /// Why it failed, in the core's words.
    pub(crate) error: Option<String>,
    /// Cancel was pressed; its answer is out.
    pub(crate) cancelling: bool,
}

impl FillJob {
    pub(crate) fn running(&self) -> bool {
        !self.status.is_finished()
    }
}

/// What a start in flight was for: the selection it fills, from which seed, with which prompt.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Start {
    pub(crate) asset: AssetId,
    pub(crate) serial: u64,
    pub(crate) seed: u64,
    pub(crate) prompt: Option<String>,
}

/// Why the last Remove request was refused, as the band shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoveNotice {
    pub(crate) text: String,
    pub(crate) code: String,
}

/// The Remove mode's view state. The selection and the strip are this window's own; the jobs
/// and candidates are the owner's, read back.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RemovePanel {
    /// Remove is the kind the next press paints.
    pub(crate) chosen: bool,
    /// The tier the Fill setting chose; the first offered one until then.
    pub(crate) tier: Option<String>,
    pub(crate) selection: Option<RemoveSelection>,
    /// How many selections this window has painted.
    pub(crate) serial: u64,
    /// The Replace prompt as typed.
    pub(crate) prompt: String,
    /// Every fill job this desktop started that it still shows or locks on, oldest first.
    pub(crate) jobs: Vec<FillJob>,
    /// The candidate previewed on the canvas: its job and number.
    pub(crate) candidate: Option<(String, u8)>,
    pub(crate) notice: Option<RemoveNotice>,
    /// Owner calls of Remove's in flight: a start or a cancel.
    pub(crate) pending: usize,
    /// The start in flight, until the owner names its job or refuses it.
    pub(crate) starting: Option<Start>,
    /// An accept of this candidate is in flight, made against this revision.
    pub(crate) accepting: Option<(String, u8, u64)>,
}

impl RemovePanel {
    /// The job of this desktop's still queued or running on `asset`: the photograph it locks.
    pub(crate) fn running_on(&self, asset: &AssetId) -> Option<&FillJob> {
        self.jobs
            .iter()
            .find(|job| job.running() && &job.asset == asset)
    }

    /// The selection on `asset`, if it is the one painted there.
    pub(crate) fn selection_on(&self, asset: &AssetId) -> Option<&RemoveSelection> {
        self.selection
            .as_ref()
            .filter(|selection| &selection.asset == asset)
    }

    /// The jobs of the selection on screen, oldest first.
    pub(crate) fn strip_jobs<'a>(
        &'a self,
        asset: &'a AssetId,
    ) -> impl Iterator<Item = &'a FillJob> + 'a {
        let serial = self.selection_on(asset).map(|selection| selection.serial);
        self.jobs
            .iter()
            .filter(move |job| Some(job.serial) == serial && &job.asset == asset)
    }

    /// The candidates the strip lists, oldest first: at most [`STRIP_MAX`], the newest.
    pub(crate) fn strip<'a>(&'a self, asset: &'a AssetId) -> Vec<(&'a FillJob, &'a CandidateRead)> {
        let all: Vec<_> = self
            .strip_jobs(asset)
            .flat_map(|job| job.candidates.iter().map(move |row| (job, row)))
            .collect();
        let skip = all.len().saturating_sub(STRIP_MAX);
        all.into_iter().skip(skip).collect()
    }

    /// The newest job of the selection on screen.
    pub(crate) fn newest<'a>(&'a self, asset: &'a AssetId) -> Option<&'a FillJob> {
        self.strip_jobs(asset).last()
    }

    pub(crate) fn job(&self, job_id: &str) -> Option<&FillJob> {
        self.jobs.iter().find(|job| job.job_id == job_id)
    }

    pub(crate) fn job_mut(&mut self, job_id: &str) -> Option<&mut FillJob> {
        self.jobs.iter_mut().find(|job| job.job_id == job_id)
    }

    /// The chosen candidate's row, while it is one of the strip's on `asset`.
    pub(crate) fn chosen_on<'a>(
        &'a self,
        asset: &'a AssetId,
    ) -> Option<(&'a FillJob, &'a CandidateRead)> {
        let (job, number) = self.candidate.as_ref()?;
        self.strip(asset)
            .into_iter()
            .find(|(held, row)| &held.job_id == job && row.candidate == *number)
    }

    /// What the tools panel pins to its bottom while a generation this desktop asked for is in
    /// flight, on any photograph, from its start until it ends, fails or is cancelled: generating
    /// is heavy enough to slow the rest of the editor. It names whichever is running, a fill or a
    /// prompted Replace.
    pub(crate) fn in_flight_notice(&self) -> Option<String> {
        let prompted = self
            .starting
            .iter()
            .map(|start| start.prompt.is_some())
            .chain(
                self.jobs
                    .iter()
                    .filter(|job| job.running())
                    .map(|job| job.prompt.is_some()),
            );
        let (mut fill, mut replace) = (false, false);
        for prompted in prompted {
            if prompted {
                replace = true;
            } else {
                fill = true;
            }
        }
        let (what, runs) = match (fill, replace) {
            (false, false) => return None,
            (true, false) => ("Generative fill", "it runs"),
            (false, true) => ("Replace", "it runs"),
            (true, true) => ("Generative fill and Replace", "they run"),
        };
        Some(format!(
            "{what} in progress. The editor may be slow while {runs}."
        ))
    }

    /// The selection and strip go: a new stroke, Escape, an accept that committed.
    pub(crate) fn clear_draft(&mut self) {
        self.selection = None;
        self.candidate = None;
        self.notice = None;
        // Finished jobs are forgotten with their strip; a running one stays until it ends, since
        // it still locks its photograph.
        self.jobs.retain(FillJob::running);
    }
}

/// One option of the Fill setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TierOption {
    pub(crate) tier: String,
    pub(crate) title: String,
}

/// Why no tier is offered: a few plain words for the status bar and Generate, and the setup's
/// main line when setting AI editing up is what is missing ([`fill_setup`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Unoffered {
    pub(crate) reason: String,
    pub(crate) setup: Option<SetupLine>,
}

/// The generation running on the photograph on screen.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Running {
    pub(crate) fraction: Option<f64>,
    /// `Generative fill · candidate 2 of 3, step 4 of 8`.
    pub(crate) sentence: String,
    pub(crate) cancelling: bool,
}

/// One cell of the candidate strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StripCell {
    pub(crate) job_id: String,
    pub(crate) candidate: u8,
    /// Its place in the strip, from 1.
    pub(crate) number: usize,
    pub(crate) chosen: bool,
    /// `live`, `accepted` or `released`.
    pub(crate) state: String,
    pub(crate) label: String,
}

/// The Remove part of the Corrections band, while Remove is chosen.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RemoveModel {
    pub(crate) module: String,
    /// Remove is the chosen kind.
    pub(crate) chosen: bool,
    pub(crate) enabled: bool,
    pub(crate) tiers: Vec<TierOption>,
    pub(crate) tier: usize,
    /// The fill module's status has not been read yet.
    pub(crate) reading: bool,
    pub(crate) unoffered: Option<Unoffered>,
    pub(crate) prompt: String,
    pub(crate) prompt_max: usize,
    /// The setup's add-on, while a prompt is typed and Replace is not added yet.
    pub(crate) addon: Option<SetupLine>,
    /// Nothing is in flight for the fill module, so a setup button may send; Cancel always may.
    pub(crate) setup_enabled: bool,
    /// Remove's setup has not run: the kind control marks Remove with the download glyph.
    pub(crate) needs_setup: bool,
    /// `Generate`, or `Replace` with a prompt.
    pub(crate) action: String,
    /// Why Generate cannot be pressed now.
    pub(crate) generate_reason: Option<String>,
    pub(crate) selection: bool,
    pub(crate) running: Option<Running>,
    pub(crate) strip: Vec<StripCell>,
    /// Candidates the running job has still to publish.
    pub(crate) coming: usize,
    pub(crate) can_regenerate: bool,
    pub(crate) can_accept: bool,
    pub(crate) accepting: bool,
    pub(crate) notice: Option<RemoveNotice>,
    pub(crate) hint: String,
    pub(crate) menu_open: bool,
}

/// The tiers a module's status lists, as read; empty before its first read.
fn tiers_of<'a>(inputs: &'a Inputs<'_>, module: &str) -> Option<&'a [FillTier]> {
    inputs
        .capabilities
        .module(module)
        .status
        .as_ref()
        .map(|status| status.fill_tiers.as_slice())
}

/// The tier the Fill setting has chosen: the panel's choice while it is offered, else the first
/// offered tier.
pub(crate) fn chosen_tier<'a>(tiers: &'a [FillTier], choice: Option<&str>) -> Option<&'a FillTier> {
    let offered = || tiers.iter().filter(|tier| tier.offered);
    choice
        .and_then(|choice| offered().find(|tier| tier.tier == choice))
        .or_else(|| offered().next())
}

/// Why `tier` is not offered. A missing runtime or model is the setup's to say: its main line,
/// with what to press. A tier is listed only while its flags are on, so a flag is never the
/// reason.
fn unoffered(tier: &FillTier, setup: Option<&SetupModel>) -> Unoffered {
    let missing = |kind: &str| tier.missing.iter().any(|missing| missing.kind == kind);
    match setup.filter(|_| missing("runtime") || missing("model")) {
        Some(setup) => Unoffered {
            reason: setup.main.reason.clone(),
            setup: Some(setup.main.clone()),
        },
        None => Unoffered {
            reason: format!("{} fill is not offered", tier_title(&tier.tier)),
            setup: None,
        },
    }
}

/// `Generative fill · candidate 2 of 3, step 4 of 8`, from the job's own progress message.
fn running_sentence(job: &FillJob) -> String {
    let what = match &job.prompt {
        Some(prompt) => format!("Replace \u{201c}{prompt}\u{201d}"),
        None => "Generative fill".into(),
    };
    match &job.message {
        Some(message) => format!("{what} \u{b7} {message}"),
        None => format!("{what} \u{b7} queued"),
    }
}

/// The Remove model for these inputs, or `None` when no module fills.
pub(crate) fn derive(inputs: &Inputs<'_>, enabled: bool) -> Option<RemoveModel> {
    let flags = inputs.settings.flags.as_ref();
    let module = fill_module(inputs.modules, flags)?;
    let panel = inputs.remove;
    let read = tiers_of(inputs, &module.id);
    let reading = read.is_none();
    let tiers = read.map_or_else(Vec::new, |tiers| shown_tiers(module, tiers, flags));
    let tiers = tiers.as_slice();
    let chosen = chosen_tier(tiers, panel.tier.as_deref());
    let options: Vec<TierOption> = tiers
        .iter()
        .filter(|tier| tier.offered)
        .map(|tier| TierOption {
            tier: tier.tier.clone(),
            title: tier_title(&tier.tier),
        })
        .collect();
    let tier_index = chosen
        .and_then(|chosen| options.iter().position(|option| option.tier == chosen.tier))
        .unwrap_or(0);
    let setup = fill_setup::derive(module, inputs.capabilities.module(&module.id), flags);
    let unoffered = match (chosen, tiers.first()) {
        (None, Some(tier)) => Some(unoffered(tier, setup.as_ref())),
        _ => None,
    };
    let asset = inputs.document.state.as_ref().map(|state| &state.asset.id);
    let prompt = panel.prompt.trim();
    // Replace is offered once a prompt asks for it.
    let addon = setup
        .as_ref()
        .filter(|_| chosen.is_some() && !prompt.is_empty())
        .and_then(|setup| setup.addon.clone())
        .filter(|addon| !addon.installed);
    let running_job = asset.and_then(|asset| panel.running_on(asset));
    let running = running_job.map(|job| Running {
        fraction: job.fraction,
        sentence: running_sentence(job),
        cancelling: job.cancelling,
    });
    let selection = asset.is_some_and(|asset| panel.selection_on(asset).is_some());
    let strip_rows = asset.map(|asset| panel.strip(asset)).unwrap_or_default();
    let chosen_candidate = panel.candidate.as_ref();
    let strip: Vec<StripCell> = strip_rows
        .iter()
        .enumerate()
        .map(|(index, (job, row))| StripCell {
            job_id: job.job_id.clone(),
            candidate: row.candidate,
            number: index + 1,
            chosen: chosen_candidate == Some(&(job.job_id.clone(), row.candidate)),
            state: match row.state {
                CandidateState::Live => "live",
                CandidateState::Accepted => "accepted",
                CandidateState::Released => "released",
            }
            .into(),
            label: row.label.clone(),
        })
        .collect();
    let coming = running_job
        .filter(|job| {
            asset.is_some_and(|asset| {
                panel.selection_on(asset).map(|s| s.serial) == Some(job.serial)
            })
        })
        .map(|job| {
            let asked = job_count(job);
            asked.saturating_sub(job.candidates.len())
        })
        .unwrap_or(0);
    let action = if prompt.is_empty() {
        "Generate".to_owned()
    } else {
        "Replace".to_owned()
    };
    let busy = panel.pending > 0 || panel.accepting.is_some();
    let generate_reason = if !enabled {
        inputs.edit_refusal.clone()
    } else if running.is_some() {
        Some("A generation is running".into())
    } else if chosen.is_none() {
        Some(
            unoffered
                .as_ref()
                .map_or("No fill tier is offered".to_owned(), |unoffered| {
                    unoffered.reason.clone()
                }),
        )
    } else if !selection {
        Some("Paint over what to remove first".into())
    } else if let Some(addon) = &addon {
        Some(addon.reason.clone())
    } else if busy {
        Some(crate::state::IN_FLIGHT.into())
    } else {
        None
    };
    let live_chosen = asset
        .and_then(|asset| panel.chosen_on(asset))
        .is_some_and(|(_, row)| row.state == CandidateState::Live);
    let newest_ready = asset
        .and_then(|asset| panel.newest(asset))
        .is_some_and(|job| job.status == JobStatus::Ready && job.next_seed.is_some());
    // Why no tier is offered: what the person can do next.
    let why = unoffered.as_ref().map(|unoffered| unoffered.reason.clone());
    let hint = match (&running, selection, strip.is_empty()) {
        (Some(running), _, _) => format!("{} \u{b7} Esc cancels", running.sentence),
        (None, _, _) if why.is_some() => why.unwrap_or_default(),
        (None, false, _) => "Paint over what to remove \u{b7} [ ] sizes the brush".into(),
        (None, true, true) => format!("{action} fills the selection \u{b7} Esc clears it"),
        (None, true, false) => {
            "Choose a candidate \u{b7} Accept commits it \u{b7} Esc clears the selection".into()
        }
    };
    Some(RemoveModel {
        module: module.id.clone(),
        chosen: panel.chosen,
        enabled,
        tiers: options,
        tier: tier_index,
        reading,
        unoffered,
        prompt: panel.prompt.clone(),
        prompt_max: chosen.map_or(PROMPT_FALLBACK, |tier| prompt_max(module, &tier.task)),
        addon,
        setup_enabled: setup.as_ref().is_some_and(|setup| setup.enabled),
        needs_setup: setup
            .as_ref()
            .is_some_and(|setup| setup.main.state.is_some() && !setup.main.installed),
        action,
        generate_reason,
        selection,
        can_regenerate: enabled
            && running.is_none()
            && !busy
            && newest_ready
            && chosen.is_some()
            && selection,
        can_accept: enabled && running.is_none() && !busy && live_chosen,
        accepting: panel.accepting.is_some(),
        running,
        strip,
        coming,
        notice: panel.notice.clone(),
        hint,
        menu_open: inputs.view_state.menu.as_ref() == Some(&crate::state::MenuTarget::Remove),
    })
}

/// How many candidates a job was asked for: the count its result names, or what a running job's
/// progress says it is making.
fn job_count(job: &FillJob) -> usize {
    job.message
        .as_deref()
        .and_then(|message| {
            let rest = message.strip_prefix("candidate ")?;
            let (_, total) = rest.split_once(" of ")?;
            total
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(job.candidates.len())
}

impl RemoveModel {
    /// What a captured frame records of Remove.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "chosen": self.chosen,
            "enabled": self.enabled,
            "reading": self.reading,
            "tiers": self.tiers.iter().map(|tier| tier.tier.clone()).collect::<Vec<_>>(),
            "tier": self.tiers.get(self.tier).map(|tier| tier.tier.clone()),
            "unoffered": self.unoffered.as_ref().map(|unoffered| json!({
                "reason": unoffered.reason,
                "setup": unoffered.setup.as_ref().map(SetupLine::summary),
            })),
            "prompt": self.prompt,
            "addon": self.addon.as_ref().map(SetupLine::summary),
            "needs_setup": self.needs_setup,
            "action": self.action,
            "generate_reason": self.generate_reason,
            "selection": self.selection,
            "running": self.running.as_ref().map(|running| json!({
                "fraction": running.fraction,
                "sentence": running.sentence,
                "cancelling": running.cancelling,
            })),
            "strip": self.strip.iter().map(|cell| json!({
                "job_id": cell.job_id,
                "candidate": cell.candidate,
                "number": cell.number,
                "chosen": cell.chosen,
                "state": cell.state,
                "label": cell.label,
            })).collect::<Vec<_>>(),
            "coming": self.coming,
            "can_regenerate": self.can_regenerate,
            "can_accept": self.can_accept,
            "accepting": self.accepting,
            "notice": self.notice.as_ref().map(|notice| json!({"text": notice.text, "code": notice.code})),
            "hint": self.hint,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(missing: Value, models: Value) -> FillTier {
        serde_json::from_value(json!({
            "tier": "generative",
            "task": "task.fill-generative",
            "offered": missing.as_array().is_some_and(Vec::is_empty),
            "missing": missing,
            "models": models,
        }))
        .unwrap()
    }

    fn model(state: &str) -> Value {
        json!([{"model": "m", "title": "Model", "resource": "m-file", "state": state, "runtime": "run-m", "text_encoder": {"resource": "m-text", "state": "not-installed"}}])
    }

    /// A missing runtime or model is the setup's to say, with its main line; anything else names
    /// the tier as not offered. A flag is never the reason: a tier behind one that is off is not
    /// listed at all.
    #[test]
    fn an_unoffered_tier_shows_the_setup_when_a_model_or_runtime_is_missing() {
        let main = SetupLine {
            setup: "remove".into(),
            state: Some(luxforge_core::capabilities::setups::SetupState::Missing),
            text: "Remove objects and fill in parts of a photo.".into(),
            detail: Some("One-time download \u{b7} 20 B".into()),
            progress: None,
            parts: Vec::new(),
            action: None,
            installed: false,
            reason: "Set up AI editing first".into(),
        };
        let setup = SetupModel {
            module_id: "ai".into(),
            main: main.clone(),
            addon: None,
            enabled: true,
            covers: Vec::new(),
        };
        for missing in [
            json!([{"kind": "model", "id": "generative", "state": "not-installed"}]),
            json!([{"kind": "runtime", "id": "inference", "state": "unavailable"}]),
        ] {
            let shown = unoffered(&tier(missing, model("not-installed")), Some(&setup));
            assert_eq!(shown.reason, "Set up AI editing first");
            assert_eq!(shown.setup, Some(main.clone()));
        }
        let other = unoffered(
            &tier(
                json!([{"kind": "other", "id": "x", "state": "off"}]),
                model("installed"),
            ),
            Some(&setup),
        );
        assert_eq!(other.reason, "Generative fill is not offered");
        assert_eq!(other.setup, None);
    }

    #[test]
    fn the_chosen_tier_is_an_offered_one() {
        let offered = tier(json!([]), model("installed"));
        let off = FillTier {
            tier: "fast".into(),
            offered: false,
            ..offered.clone()
        };
        let tiers = [off, offered];
        assert_eq!(chosen_tier(&tiers, None).unwrap().tier, "generative");
        assert_eq!(
            chosen_tier(&tiers, Some("fast")).unwrap().tier,
            "generative"
        );
        assert!(chosen_tier(&tiers[..1], None).is_none());
    }

    #[test]
    fn a_running_jobs_count_comes_from_its_progress() {
        let job = FillJob {
            job_id: "j".into(),
            asset: AssetId::new(),
            serial: 1,
            prompt: None,
            seed: 0,
            status: JobStatus::Running,
            fraction: None,
            message: Some("candidate 2 of 3, step 4 of 8".into()),
            candidates: Vec::new(),
            next_seed: None,
            error: None,
            cancelling: false,
        };
        assert_eq!(job_count(&job), 3);
        assert_eq!(
            running_sentence(&job),
            "Generative fill \u{b7} candidate 2 of 3, step 4 of 8"
        );
    }

    /// The tools panel's notice names whichever is in flight on any photograph, a start the owner
    /// has not answered or a job still queued or running, and is gone once each has ended.
    #[test]
    fn the_in_flight_notice_names_what_is_generating_until_it_ends() {
        let job = |prompt: Option<&str>, status| FillJob {
            job_id: "j".into(),
            asset: AssetId::new(),
            serial: 1,
            prompt: prompt.map(str::to_owned),
            seed: 0,
            status,
            fraction: None,
            message: None,
            candidates: Vec::new(),
            next_seed: None,
            error: None,
            cancelling: false,
        };
        let mut panel = RemovePanel::default();
        assert_eq!(panel.in_flight_notice(), None);
        panel.starting = Some(Start {
            asset: AssetId::new(),
            serial: 1,
            seed: 0,
            prompt: None,
        });
        assert_eq!(
            panel.in_flight_notice().as_deref(),
            Some("Generative fill in progress. The editor may be slow while it runs.")
        );
        panel.starting = None;
        panel.jobs.push(job(Some("a boat"), JobStatus::Queued));
        assert_eq!(
            panel.in_flight_notice().as_deref(),
            Some("Replace in progress. The editor may be slow while it runs.")
        );
        panel.jobs.push(job(None, JobStatus::Running));
        assert_eq!(
            panel.in_flight_notice().as_deref(),
            Some("Generative fill and Replace in progress. The editor may be slow while they run.")
        );
        for ended in [JobStatus::Ready, JobStatus::Failed, JobStatus::Cancelled] {
            for held in &mut panel.jobs {
                held.status = ended;
            }
            assert_eq!(panel.in_flight_notice(), None, "{ended:?}");
        }
    }
}
