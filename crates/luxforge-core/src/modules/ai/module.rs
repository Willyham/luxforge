//! The `luxforge.ai` module's descriptor, its request and the hooks the capability host calls.
use super::{
    generative,
    models::{DEVELOPER_MODELS, GenerativeModel, PRODUCT_MODELS},
    selection::{EFFECT_POINTS, EFFECT_RADIUS, SELECTION_POINTS, SELECTION_RADIUS, Selection},
};
use crate::{
    Error, ParameterDescriptor,
    artifacts::{PREPARED_ARTIFACT_BYTES, candidates::MAX_CANDIDATE_BYTES},
    capabilities::{
        context::ModuleContext,
        descriptor::{ReadBoundary, TaskDescriptor},
        resources::ResourceState,
    },
    inference::MAX_PROMPT_CHARS,
    modules::{
        ActionInput, ActionPlan, CapabilityModule, HostView, LayerReport, ModuleDescriptor,
        Processing, StageContext, TaskAdmission, TaskNeeds, ToolModule,
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
/// Candidates one Generate makes by default, and at most. One at a time (owner, 2026-10-11): a
/// local candidate takes about two minutes, so Generate makes one and Regenerate adds one.
pub(crate) const DEFAULT_CANDIDATES: u32 = 1;
pub(crate) const MAX_CANDIDATES: u32 = crate::artifacts::candidates::MAX_JOB_CANDIDATES as u32;
/// The largest seed a request names: the largest integer a JSON number holds exactly.
pub(crate) const MAX_SEED: u64 = (1 << 53) - 1;

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
    #[cfg(test)]
    pub(crate) gate: Option<std::sync::Arc<luxforge_testbase::Gate>>,
}

impl AiModule {
    /// The module a run serves: the developer models in a developer run, the product models
    /// otherwise.
    pub(crate) fn for_run(developer: bool) -> Self {
        Self::with_models(if developer {
            DEVELOPER_MODELS
        } else {
            PRODUCT_MODELS
        })
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

    fn with_models(models: &'static [GenerativeModel]) -> Self {
        let mut capabilities = Vec::new();
        let mut resources = Vec::new();
        for model in models {
            capabilities.extend(model.capabilities());
            resources.extend(model.files().map(|file| file.descriptor()));
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
                "Fills the region a brush selection paints on this photograph with a generative model and publishes each fill as a candidate patch, from consecutive seeds, one after another in one job: Generate is one request, Regenerate another for the same selection. The region is each stroke's footprint as painted, feathered over 15% of its radius; the effect stroke adds what the object casts. A prompt makes the fill Replace, which needs the model's text encoder installed: without it the request is not-ready naming the resource, and nothing is generated. Refused resource-limit before anything is read when the region's bounds would not fit one candidate ({MAX_CANDIDATE_BYTES} bytes of patch at {PATCH_PIXEL_BYTES} bytes a pixel and its preview), cover more than half the frame, or would not bind with the stack's artifacts within {PREPARED_ARTIFACT_BYTES} bytes. While it runs, the photograph is locked: every edit of it is refused conflict (data.reason locked) until the job ends. Offered only while the ai.generative flag is on. Each candidate is labelled Generative fill or Replace \"…\" and carries the model's provenance; accept one with edit.accept-correction-patch. Result: {{tier, model, candidates, seeds, next_seed, rect, prompt}}."
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
        };
        Self {
            descriptor: ModuleDescriptor {
                id: AI_MODULE.into(),
                title: "AI editing".into(),
                hint: Some("Generative fill over the Corrections band's brush".into()),
                developer: false,
                // Its section holds the model cards, which the Models tab also lists: it starts
                // collapsed, so a launch reads nothing for it until it is opened.
                collapsed: true,
                tasks: vec![task],
                capabilities,
                resources,
                ..ModuleDescriptor::default()
            },
            models,
            #[cfg(test)]
            gate: None,
        }
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
            .models
            .iter()
            .flat_map(GenerativeModel::files)
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
        let request = self.request(task_id, parameters)?;
        Ok(TaskNeeds {
            uses: Some(request.uses()?),
            flags: vec![GENERATIVE_FLAG],
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
