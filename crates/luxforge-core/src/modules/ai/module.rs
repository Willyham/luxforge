//! The `luxforge.ai` module's descriptor, its request and the hooks the capability host calls.
use super::{
    analysis::{
        self as analysis_tasks,
        models::{
            AnalysisModels, DEPTH, DEVELOPER_ANALYSIS, PRODUCT_ANALYSIS, Role, SCENE, SEGMENTER,
            SessionModel,
        },
    },
    generative,
    models::{DEVELOPER_MODELS, GenerativeModel, ModelFile, PRODUCT_MODELS},
    objects::{self, Current, MAX_PROMPT_POINTS},
    selection::{EFFECT_POINTS, EFFECT_RADIUS, SELECTION_POINTS, SELECTION_RADIUS, Selection},
};
use crate::{
    Error, ParameterDescriptor,
    artifacts::{PREPARED_ARTIFACT_BYTES, candidates::MAX_CANDIDATE_BYTES},
    capabilities::{
        context::ModuleContext,
        descriptor::{ReadBoundary, ResourceDescriptor, TaskDescriptor},
        resources::ResourceState,
    },
    inference::MAX_PROMPT_CHARS,
    modules::{
        ActionInput, ActionPlan, CapabilityModule, HostView, LayerReport, ModuleDescriptor,
        Processing, StageContext, TaskAdmission, TaskNeeds, ToolModule,
        descriptor::ActionDescriptor,
    },
    path::POSTED_POINTS_PER_STROKE,
    repair::patch::PATCH_PIXEL_BYTES,
};
use serde_json::{Map, Value, json};
use std::path::Path;

pub(crate) const AI_MODULE: &str = "luxforge.ai";
/// The Generative tier's task: `task.fill-generative`.
pub(crate) const FILL_GENERATIVE: &str = "fill-generative";
/// The experiment flag that admits the Generative tier.
pub(crate) const GENERATIVE_FLAG: &str = crate::flags::AI_GENERATIVE;
/// The tier's name, as `module.status` and a candidate's provenance give it.
pub(crate) const GENERATIVE_TIER: &str = "generative";
/// Candidates one Generate makes by default, and at most. One at a time: a
/// local candidate takes about two minutes, so Generate makes one and Regenerate adds one.
pub(crate) const DEFAULT_CANDIDATES: u32 = 1;
pub(crate) const MAX_CANDIDATES: u32 = crate::artifacts::candidates::MAX_JOB_CANDIDATES as u32;
/// The largest seed a request names: the largest integer a JSON number holds exactly.
pub(crate) const MAX_SEED: u64 = (1 << 53) - 1;
/// The segmenter's analysis: `task.analyse`.
pub(crate) const ANALYSE: &str = "analyse";
/// The scene's analysis: `task.analyse-scene`.
pub(crate) const ANALYSE_SCENE: &str = "analyse-scene";
/// The depth analysis: `task.analyse-depth`.
pub(crate) const ANALYSE_DEPTH: &str = "analyse-depth";
/// A picked object's selection: `task.select-object`.
pub(crate) const SELECT_OBJECT: &str = "select-object";
/// The picker: `query.pick-object`.
pub(crate) const PICK_OBJECT: &str = "pick-object";
/// The experiment flag that admits analysis, the picker and the selection of objects.
pub(crate) const ANALYSIS_FLAG: &str = crate::flags::AI_ANALYSIS;
/// The highest candidate index `task.select-object` names: a stroke's prompts' candidates.
const MAX_CANDIDATE_INDEX: i64 = 63;

/// One request of the Generative tier, as `task.fill-generative` checked it.
#[derive(Clone, Debug)]
pub(crate) struct FillRequest {
    pub(crate) selection: Selection,
    pub(crate) model: &'static GenerativeModel,
    pub(crate) candidates: u32,
    /// The prompt that makes the fill Replace, trimmed; a blank prompt is none.
    pub(crate) prompt: Option<String>,
    /// The first candidate's seed; candidate `i` takes `seed + i`. `None` draws one.
    pub(crate) seed: Option<u64>,
}

impl FillRequest {
    pub(crate) fn parse(
        models: &'static [GenerativeModel],
        parameters: &Map<String, Value>,
    ) -> Result<Self, Error> {
        let model = parameters
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::validation("model is required"))?;
        let model = models
            .iter()
            .find(|candidate| candidate.id == model)
            .ok_or_else(|| Error::validation(format!("unknown model {model}")))?;
        let candidates = parameters
            .get("candidates")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_CANDIDATES, |count| count as u32);
        if !(1..=MAX_CANDIDATES).contains(&candidates) {
            return Err(Error::validation(format!(
                "candidates is 1..={MAX_CANDIDATES}, not {candidates}"
            )));
        }
        let prompt = parameters
            .get("prompt")
            .and_then(Value::as_str)
            .map(|prompt| prompt.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|prompt| !prompt.is_empty());
        if let Some(prompt) = &prompt
            && prompt.chars().count() > MAX_PROMPT_CHARS
        {
            return Err(Error::validation(format!(
                "parameter prompt must be at most {MAX_PROMPT_CHARS} characters"
            )));
        }
        let seed = parameters.get("seed").and_then(Value::as_u64);
        Ok(Self {
            selection: Selection::parse(parameters)?,
            model,
            candidates,
            prompt,
            seed,
        })
    }

    /// The capabilities the request uses: its model's runtime and, with a prompt, what loads the
    /// text encoder, so a missing text encoder is an unmet requirement.
    fn uses(&self) -> Result<Vec<String>, Error> {
        let uses = self.model.uses(self.prompt.is_some()).ok_or_else(|| {
            Error::validation(format!(
                "model {} takes no prompt: it has no text encoder",
                self.model.id
            ))
        })?;
        Ok(uses.into_iter().map(str::to_owned).collect())
    }
}

/// The AI editing module: the qualified model in every run, and the developer fake before it in a
/// developer run (see [`super::models`]). The `ai.generative` flag gates the tier.
pub(crate) struct AiModule {
    descriptor: ModuleDescriptor,
    models: &'static [GenerativeModel],
    analysis: AnalysisModels,
    /// The query worker's held embedding ([`Current`]).
    current: Current,
    #[cfg(test)]
    pub(crate) gate: Option<std::sync::Arc<luxforge_testbase::Gate>>,
}

impl AiModule {
    /// The module a run serves: the developer models in a developer run, the product models
    /// otherwise.
    pub(crate) fn for_run(developer: bool) -> Self {
        if developer {
            Self::with_models(DEVELOPER_MODELS, DEVELOPER_ANALYSIS)
        } else {
            Self::with_models(PRODUCT_MODELS, PRODUCT_ANALYSIS)
        }
    }

    /// The module with the developer models.
    #[cfg(test)]
    pub(crate) fn developer() -> Self {
        Self::for_run(true)
    }

    /// The developer module whose jobs pass `gate` after publishing each candidate.
    #[cfg(test)]
    pub(crate) fn gated(gate: std::sync::Arc<luxforge_testbase::Gate>) -> Self {
        Self {
            gate: Some(gate),
            ..Self::developer()
        }
    }

    fn with_models(models: &'static [GenerativeModel], analysis: AnalysisModels) -> Self {
        let mut capabilities = Vec::new();
        let mut resources = Vec::new();
        for model in models {
            capabilities.extend(model.capabilities());
            // Every file is the Generative tier's alone: a client offers it only while the tier's
            // experiment is on.
            resources.extend(model.files().map(|file| ResourceDescriptor {
                flag: Some(GENERATIVE_FLAG.into()),
                ..file.descriptor()
            }));
        }
        let mut uses: Vec<String> = Vec::new();
        for model in models {
            for used in [false, true]
                .into_iter()
                .filter_map(|prompt| model.uses(prompt))
                .flatten()
            {
                if !uses.iter().any(|known| known == used) {
                    uses.push(used.to_owned());
                }
            }
        }
        let radius = |name: &str, notes: &str| {
            ParameterDescriptor::number(
                name,
                crate::modules::corrections::RADIUS_MIN,
                crate::modules::corrections::RADIUS_MAX,
            )
            .unit("content heights")
            .notes(notes)
        };
        let path = |name: &str, notes: &str| {
            ParameterDescriptor::points(name, 1, POSTED_POINTS_PER_STROKE).notes(notes)
        };
        let task = TaskDescriptor {
            id: FILL_GENERATIVE.into(),
            title: "Generative fill".into(),
            notes: format!(
                "Fills the region a brush selection paints on this photograph with a generative model and publishes each fill as a candidate patch, from consecutive seeds, one after another in one job: Generate is one request, Regenerate another for the same selection. The region is each stroke's footprint as painted at full coverage, feathered over 15% of its radius past its edge; the effect stroke adds what the object casts. A prompt makes the fill Replace, which needs the model's text encoder installed: without it the request is not-ready naming the resource, and nothing is generated. Refused resource-limit before anything is read when the region's bounds would not fit one candidate ({MAX_CANDIDATE_BYTES} bytes of patch at {PATCH_PIXEL_BYTES} bytes a pixel and its preview), cover more than half the frame, or would not bind with the stack's artifacts within {PREPARED_ARTIFACT_BYTES} bytes. While it runs, the photograph is locked: every edit of it is refused conflict (data.reason locked) until the job ends. Offered only while the ai.generative flag is on. With ai.generative-gpu-sharing on as it is requested, its generations run their GPU work in short slices so the interface keeps drawing, and result.share_gpu says so. Each candidate is labelled Generative fill or Replace \"…\" and carries the model's provenance; accept one with edit.accept-correction-patch. Result: {{tier, model, candidates, seeds, next_seed, rect, prompt, share_gpu}}."
            ),
            asset: true,
            profile: false,
            uses,
            parameters: vec![
                path(SELECTION_POINTS, "the selection: a brush stroke's path in the content stage's normalized coordinates, as a correction's points").required(true),
                radius(SELECTION_RADIUS, "the selection's brush radius").required(true),
                path(EFFECT_POINTS, "the effect region: a second stroke over what the object casts, such as its shadow"),
                radius(EFFECT_RADIUS, "the effect stroke's radius; default the selection's"),
                ParameterDescriptor::enumeration("model", models.iter().map(|model| model.id))
                    .required(true)
                    .notes("the installed generative model to fill with"),
                ParameterDescriptor::integer("candidates", 1, i64::from(MAX_CANDIDATES))
                    .default(i64::from(DEFAULT_CANDIDATES))
                    .notes("how many candidates to generate, one after another"),
                ParameterDescriptor::string("prompt", MAX_PROMPT_CHARS)
                    .notes("what to fill the region with instead of removing; makes the fill Replace and needs the model's text encoder; a blank prompt is none"),
                ParameterDescriptor::integer("seed", 0, MAX_SEED as i64)
                    .notes("the first candidate's seed, candidate n taking seed + n; default a new one each request"),
            ],
            apply: None,
            reads: Some(ReadBoundary::RepairInput),
            flags: vec![GENERATIVE_FLAG.into()],
        };
        for model in analysis.models {
            capabilities.extend(model.capabilities());
            resources.push(model.descriptor(ANALYSIS_FLAG));
        }
        let mut tasks = vec![task];
        tasks.extend(analysis_task_descriptors(&analysis));
        let queries = if analysis.set(&SEGMENTER).is_some() {
            vec![pick_query_descriptor()]
        } else {
            Vec::new()
        };
        Self {
            descriptor: ModuleDescriptor {
                id: AI_MODULE.into(),
                title: "AI editing".into(),
                hint: Some(
                    "Generative fill over the Corrections band's brush, and picking objects".into(),
                ),
                developer: false,
                // The desktop draws no section for it: its setup is the Corrections band's and its
                // models are in the Models tab. A client that draws one starts it collapsed, so a
                // launch reads nothing for it until it is opened.
                collapsed: true,
                tasks,
                queries,
                capabilities,
                resources,
                ..ModuleDescriptor::default()
            },
            models,
            analysis,
            current: Current::default(),
            #[cfg(test)]
            gate: None,
        }
    }

    /// The analysis models an analysis task or the picker runs with, in role order; `None` when
    /// the id is none of them.
    fn analysis_models(&self, id: &str) -> Option<Result<Vec<&'static SessionModel>, Error>> {
        let roles: &[Role] = match id {
            ANALYSE | SELECT_OBJECT | PICK_OBJECT => &SEGMENTER,
            ANALYSE_SCENE => &SCENE,
            ANALYSE_DEPTH => &DEPTH,
            _ => return None,
        };
        Some(
            self.analysis
                .set(roles)
                .ok_or_else(|| Error::validation(format!("this run offers no models for {id}"))),
        )
    }

    /// Every file of the module's models.
    fn files(&self) -> impl Iterator<Item = ModelFile> + '_ {
        self.models
            .iter()
            .flat_map(GenerativeModel::files)
            .chain(self.analysis.models.iter().map(|model| model.file))
    }

    fn request(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<FillRequest, Error> {
        if task_id != FILL_GENERATIVE {
            return Err(Error::validation(format!("unknown task {task_id}")));
        }
        FillRequest::parse(self.models, parameters)
    }
}

impl ToolModule for AiModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Err(Error::validation(format!("unknown action {action_id}")))
    }
    fn plan(&self, input: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Err(Error::validation(format!(
            "unknown action {}",
            input.action_id
        )))
    }
    fn validate_payload(&self, effect_id: &str, _: u32, _: &Value) -> Result<(), Error> {
        Err(Error::unavailable_effect(effect_id, &[]))
    }
    fn describe(&self, effect_id: &str, _: u32, _: &Value) -> Result<LayerReport, Error> {
        Err(Error::unavailable_effect(effect_id, &[]))
    }
    fn compile(
        &self,
        effect_id: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
        Err(Error::unavailable_effect(effect_id, &[]))
    }
    fn capabilities(&self) -> Option<&dyn CapabilityModule> {
        Some(self)
    }
}

impl CapabilityModule for AiModule {
    /// A developer fixture must be exactly its pinned bytes; a published model file must open as
    /// its declared format (`super::formats`), read from its first bytes only. The install has
    /// already checked every file's length and SHA-256.
    fn validate_resource(&self, resource_id: &str, path: &Path) -> Result<(), Error> {
        let file = self
            .files()
            .find(|file| file.resource == resource_id)
            .ok_or_else(|| Error::validation(format!("unknown resource {resource_id}")))?;
        if file.published.is_none() {
            let bytes = std::fs::read(path).map_err(|error| {
                Error::file_access(format!("reading staged {resource_id}: {error}"))
            })?;
            if bytes != file.bytes {
                return Err(Error::validation(format!(
                    "{resource_id} is not a {} file",
                    file.format
                )));
            }
            return Ok(());
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let format = file.format_of(name).ok_or_else(|| {
            Error::validation(format!("resource {resource_id} declares no file {name}"))
        })?;
        super::formats::check(path, format)
            .map_err(|detail| Error::validation(format!("{resource_id} file {name}: {detail}")))
    }

    fn run_task(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        if let Some(models) = self.analysis_models(task_id) {
            let models = models?;
            return match task_id {
                ANALYSE => objects::analyse(context, &self.analysis),
                SELECT_OBJECT => objects::select_object(context, &self.analysis, parameters),
                ANALYSE_SCENE => analysis_tasks::analyse_scene(context, &models),
                ANALYSE_DEPTH => analysis_tasks::analyse_depth(context, &models),
                _ => Err(Error::validation(format!("unknown task {task_id}"))),
            };
        }
        let request = self.request(task_id, parameters)?;
        generative::run(&request, context, &|context| {
            #[cfg(test)]
            if let Some(gate) = &self.gate {
                gate.pass_unless(|| context.is_cancelled());
            }
            #[cfg(not(test))]
            let _ = context;
        })
    }

    fn task_needs(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<TaskNeeds, Error> {
        if let Some(models) = self.analysis_models(task_id) {
            if task_id == PICK_OBJECT {
                return Err(Error::validation(format!("unknown task {task_id}")));
            }
            return Ok(TaskNeeds {
                uses: Some(models?.iter().map(|m| m.runtime.to_owned()).collect()),
                flags: vec![ANALYSIS_FLAG],
                reads_flags: Vec::new(),
                locks_asset: false,
            });
        }
        let request = self.request(task_id, parameters)?;
        Ok(TaskNeeds {
            uses: Some(request.uses()?),
            flags: vec![GENERATIVE_FLAG],
            reads_flags: vec![crate::flags::AI_GENERATIVE_GPU_SHARING],
            locks_asset: true,
        })
    }

    /// Refuse a region past a candidate, past half the frame, or past the stack's artifact budget,
    /// from the strokes' supports alone (`O(segments)`).
    fn admit_task(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
        admission: &TaskAdmission,
    ) -> Result<(), Error> {
        // An analysis reads the stage a band at a time and holds outputs of fixed bounds.
        if self.analysis_models(task_id).is_some() {
            return Ok(());
        }
        let request = self.request(task_id, parameters)?;
        let stage = admission
            .stage
            .ok_or_else(|| Error::internal("a fill is admitted with the stage it reads"))?;
        let support = request.selection.support(stage);
        if support.is_empty() {
            return Err(Error::validation(
                "the selection covers no pixel of the photograph",
            ));
        }
        let (preview_width, preview_height) =
            super::fill::preview_size(support.width, support.height);
        let patch = support.pixels() * PATCH_PIXEL_BYTES;
        let preview = u64::from(preview_width) * u64::from(preview_height) * 3;
        if patch + preview > MAX_CANDIDATE_BYTES {
            return Err(Error::resource_limit(format!(
                "the region's bounds are {} x {} pixels, whose patch and preview would take {} bytes; one candidate holds at most {MAX_CANDIDATE_BYTES}",
                support.width,
                support.height,
                patch + preview
            ))
            .with_data(json!({"limit": "candidate", "bounds": [support.x0, support.y0, support.width, support.height]})));
        }
        if support.pixels() * 2 > u64::from(stage.width) * u64::from(stage.height) {
            return Err(Error::resource_limit(format!(
                "the region's bounds are {} x {} pixels, more than half of the {} x {} frame",
                support.width, support.height, stage.width, stage.height
            ))
            .with_data(json!({"limit": "half-frame", "bounds": [support.x0, support.y0, support.width, support.height]})));
        }
        if admission.artifact_bytes + patch > PREPARED_ARTIFACT_BYTES {
            return Err(Error::resource_limit(format!(
                "the stack's artifacts take {} bytes; with a {patch}-byte patch they would not bind within {PREPARED_ARTIFACT_BYTES}",
                admission.artifact_bytes
            ))
            .with_data(json!({"limit": "artifacts", "artifact_bytes": admission.artifact_bytes, "patch_bytes": patch})));
        }
        Ok(())
    }

    fn worker_query(&self, query_id: &str) -> bool {
        query_id == PICK_OBJECT && self.analysis.set(&SEGMENTER).is_some()
    }

    /// The picker decodes with the segmenter's decoder alone: the encoder ran in `task.analyse`.
    fn query_needs(&self, query_id: &str, _: &Map<String, Value>) -> Result<TaskNeeds, Error> {
        if query_id != PICK_OBJECT {
            return Err(Error::validation(format!("unknown query {query_id}")));
        }
        let [_, decoder] = objects::segmenter(&self.analysis)?;
        Ok(TaskNeeds {
            uses: Some(vec![decoder.runtime.to_owned()]),
            flags: vec![ANALYSIS_FLAG],
            reads_flags: Vec::new(),
            locks_asset: false,
        })
    }

    fn run_query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        if query_id != PICK_OBJECT {
            return Err(Error::validation(format!("unknown query {query_id}")));
        }
        objects::pick_object(context, &self.analysis, &self.current, parameters)
    }

    /// `fill_tiers`: each tier the module has, whether it is offered now and why not, and its
    /// models with their files' states, so a client lists the tiers it can fill with and offers
    /// a missing text encoder's download.
    fn status(&self, view: &HostView<'_>) -> Map<String, Value> {
        let flag = view.flag(GENERATIVE_FLAG);
        let installed = |resource: &str| view.resource(resource) == ResourceState::Installed;
        let models: Vec<Value> = self
            .models
            .iter()
            .map(|model| {
                json!({
                    "model": model.id,
                    "title": model.generator.title,
                    "resource": model.generator.resource,
                    "state": view.resource(model.generator.resource).name(),
                    "runtime": model.runtime,
                    "text_encoder": model.text_encoder.map(|encoder| json!({
                        "resource": encoder.resource,
                        "state": view.resource(encoder.resource).name(),
                    })),
                })
            })
            .collect();
        let any_installed = self
            .models
            .iter()
            .any(|model| installed(model.generator.resource));
        let mut missing = Vec::new();
        if !flag {
            missing.push(json!({"kind": "flag", "id": GENERATIVE_FLAG, "state": "off"}));
        }
        if !view.runtime() {
            missing.push(json!({"kind": "runtime", "id": "inference", "state": "unavailable"}));
        }
        if !any_installed {
            missing.push(json!({"kind": "model", "id": GENERATIVE_TIER, "state": "not-installed"}));
        }
        let mut fields = Map::new();
        fields.insert("analysis".into(), self.analysis_status(view));
        fields.insert(
            "fill_tiers".into(),
            json!([{
                "tier": GENERATIVE_TIER,
                "task": format!("task.{FILL_GENERATIVE}"),
                "offered": missing.is_empty(),
                "missing": missing,
                "flag": {"id": GENERATIVE_FLAG, "on": flag},
                "models": models,
            }]),
        );
        fields
    }
}

impl AiModule {
    /// `analysis`: the flag, each analysis task with whether it is offered now and what it lacks,
    /// and every analysis model with its file's state.
    fn analysis_status(&self, view: &HostView<'_>) -> Value {
        let flag = view.flag(ANALYSIS_FLAG);
        let tasks: Vec<Value> = [
            (ANALYSE, &SEGMENTER[..]),
            (SELECT_OBJECT, &SEGMENTER[..]),
            (PICK_OBJECT, &SEGMENTER[..]),
            (ANALYSE_SCENE, &SCENE[..]),
            (ANALYSE_DEPTH, &DEPTH[..]),
        ]
        .into_iter()
        .filter_map(|(id, roles)| {
            let models = self.analysis.set(roles)?;
            let mut missing = Vec::new();
            if !flag {
                missing.push(json!({"kind": "flag", "id": ANALYSIS_FLAG, "state": "off"}));
            }
            if !view.runtime() {
                missing.push(json!({"kind": "runtime", "id": "inference", "state": "unavailable"}));
            }
            for model in models {
                let state = view.resource(model.file.resource);
                if state != ResourceState::Installed {
                    missing.push(
                        json!({"kind": "resource", "id": model.file.resource, "state": state.name()}),
                    );
                }
            }
            let kind = if id == PICK_OBJECT { "query" } else { "task" };
            Some(json!({"id": format!("{kind}.{id}"), "offered": missing.is_empty(), "missing": missing}))
        })
        .collect();
        let models: Vec<Value> = self
            .analysis
            .models
            .iter()
            .map(|model| {
                json!({
                    "role": role_name(model.role),
                    "resource": model.file.resource,
                    "version": model.file.version,
                    "state": view.resource(model.file.resource).name(),
                    "runtime": model.runtime,
                })
            })
            .collect();
        json!({
            "flag": {"id": ANALYSIS_FLAG, "on": flag},
            "tasks": tasks,
            "models": models,
        })
    }
}

/// A role's name, as `module.status` gives it.
fn role_name(role: Role) -> &'static str {
    match role {
        Role::SegmenterEncoder => "segmenter-encoder",
        Role::SegmenterDecoder => "segmenter-decoder",
        Role::Semantic => "semantic",
        Role::Subject => "subject",
        Role::People => "people",
        Role::Depth => "depth",
    }
}

/// The parameters that name one prompt, which the picker and `task.select-object` share.
fn prompt_parameters() -> Vec<ParameterDescriptor> {
    let points = |name: &str, min: usize, max: usize, notes: &str| {
        ParameterDescriptor::points(name, min, max).notes(notes)
    };
    vec![
        points(
            "point",
            1,
            1,
            "one position, as hover or a click names it, in the content stage's normalized coordinates",
        ),
        points("positive", 1, MAX_PROMPT_POINTS, "positions on the object"),
        points("negative", 1, MAX_PROMPT_POINTS, "positions off the object"),
        points(
            "box",
            2,
            2,
            "a box around the object: its top-left and bottom-right corners",
        ),
        points(
            "stroke",
            1,
            POSTED_POINTS_PER_STROKE,
            "a brush stroke's path over one or more objects",
        ),
        ParameterDescriptor::number(
            "radius",
            crate::modules::corrections::RADIUS_MIN,
            crate::modules::corrections::RADIUS_MAX,
        )
        .unit("content heights")
        .notes("the stroke's brush radius; required with stroke"),
    ]
}

/// The prompt rules every request states.
const PROMPT_NOTES: &str = "A request names exactly one prompt, in the content stage's normalized coordinates: point (hover or a click), positive and negative points with an optional box, or a brush stroke with its radius. A stroke is split into prompts along its length and its candidates scored by how much of the stroke and of themselves they share, then the decision is one object, several (each well covered and none nested in another) or nothing; any other prompt is decoded once and its candidate chosen for stability then predicted IoU, biased to the whole object. Ties break in a fixed order, so the same analysis and prompt always choose the same candidates.";

/// `query.pick-object`.
fn pick_query_descriptor() -> ActionDescriptor {
    let mut parameters = prompt_parameters();
    parameters.push(
        ParameterDescriptor::enumeration("previews", ["chosen", "all", "none"])
            .default("chosen")
            .notes("which candidates carry a coverage preview"),
    );
    ActionDescriptor {
        preset: false,
        parameters,
        ..ActionDescriptor::new(
            PICK_OBJECT,
            "Pick object",
            format!(
                "the objects one prompt picks on this photograph, from the segmenter's cached analysis of it as it is now; not-ready naming task.analyse (data.requirements kind analysis) when it is not analysed. Answered by the host's query worker, one running and one waiting: a newer request replaces the waiting one, which is answered cancelled. {PROMPT_NOTES} Offered only while the ai.analysis flag is on. Result: {{decision (one, several or nothing), chosen (candidate positions), candidates [{{candidate, prompt, index, predicted_iou, stability, stroke_coverage, object_coverage, area, bounds (normalized), preview {{width, height, codes (base64 8-bit coverage over bounds, at most {} pixels)}}}}], analysis {{input, models}}}}",
                objects::PREVIEW_PIXELS
            ),
        )
    }
}

/// The analysis tasks the run's models serve: `task.analyse` and `task.select-object` with a
/// segmenter, `task.analyse-scene` with every scene model, `task.analyse-depth` with a depth model.
fn analysis_task_descriptors(analysis: &AnalysisModels) -> Vec<TaskDescriptor> {
    let task = |id: &str,
                title: &str,
                notes: String,
                models: &[&SessionModel],
                parameters: Vec<ParameterDescriptor>| TaskDescriptor {
        id: id.into(),
        title: title.into(),
        notes: format!(
            "{notes} Reads the repair stage's output through the host-planned reference read, reduced to the analysis input (at most {} px on its long side) and the guide (the stage's luminance, at most {} px); everything it computes is kept in the analysis cache under the photograph's source, its repair prefix and the models' versions, and a request over what the cache already holds answers from it. Runs on the module lane; the photograph is not locked. Offered only while the ai.analysis flag is on.",
            analysis_tasks::ANALYSIS_SIDE,
            analysis_tasks::GUIDE_SIDE
        ),
        asset: true,
        profile: false,
        uses: models
            .iter()
            .map(|model| model.runtime.to_owned())
            .collect(),
        parameters,
        apply: None,
        reads: Some(ReadBoundary::RepairOutput),
        flags: vec![ANALYSIS_FLAG.into()],
    };
    let mut tasks = Vec::new();
    if let Some(models) = analysis.set(&SEGMENTER) {
        tasks.push(task(
            ANALYSE,
            "Analyse objects",
            "Runs the segmenter's image encoder over this photograph, so query.pick-object can answer. Result: {cached, input, guide, models}.".into(),
            &models,
            Vec::new(),
        ));
        let mut parameters = prompt_parameters();
        parameters.push(
            ParameterDescriptor::integer("candidate", 0, MAX_CANDIDATE_INDEX)
                .notes("the candidate position query.pick-object answered for the same prompt; default the decision's one object"),
        );
        tasks.push(task(
            SELECT_OBJECT,
            "Select object",
            format!(
                "Decodes one prompt as query.pick-object does, analysing the photograph first when it is not, chooses its candidate and publishes it as a mask-alpha artifact at the guide's resolution, refined against the guide's edges. Refused validation when the prompt picks nothing or several objects and names no candidate. {PROMPT_NOTES} Result: {{artifact, width, height, class, model, model_version, input_fingerprint, prefix_hash, decision, candidate, models}}, the artifact and provenance mask.add-model takes."
            ),
            &models,
            parameters,
        ));
    }
    if let Some(models) = analysis.set(&SCENE) {
        tasks.push(task(
            ANALYSE_SCENE,
            "Analyse scene",
            "Runs the semantic model, the salient-subject model and the people detector over this photograph. Result: {cached, classes [{class, area}], subject {area}, people [{box, score}], models}.".into(),
            &models,
            Vec::new(),
        ));
    }
    if let Some(models) = analysis.set(&DEPTH) {
        tasks.push(task(
            ANALYSE_DEPTH,
            "Analyse depth",
            "Runs the depth model over this photograph. Result: {cached, grid, range {near, far}, models}.".into(),
            &models,
            Vec::new(),
        ));
    }
    tasks
}
