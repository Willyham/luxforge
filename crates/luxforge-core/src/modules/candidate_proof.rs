//! Developer proof of the host's candidate seam. `luxforge.candidates` declares one task,
//! `generate-proof-patches`, that publishes deterministic candidate patches of the photograph it
//! names through [`ModuleContext::publish_candidate`]: no model, no network, no resource. Each
//! candidate is reserved before anything is made, so a shape over a bound is refused before the
//! producer allocates, and a count past four ends the job `resource-limit` with the first four
//! live. It is a test fixture, registered in developer runs only, which the remaining frozen-patch
//! work and AI editing's workflow tests drive instead of a model. See
//! `docs/design/module-capabilities.md#job-candidates`.
use super::{
    ActionInput, ActionPlan, CapabilityModule, LayerReport, ModuleDescriptor, Processing,
    StageContext, ToolModule,
};
use crate::{
    Error, ParameterDescriptor,
    artifacts::candidates::{CandidatePatch, CandidateShape, PatchColour},
    capabilities::{context::ModuleContext, descriptor::TaskDescriptor},
};
use serde_json::{Map, Value, json};
#[cfg(test)]
use std::sync::Arc;

pub(crate) const CANDIDATE_PROOF_MODULE: &str = "luxforge.candidates";
pub(crate) const CANDIDATE_PROOF_TASK: &str = "generate-proof-patches";
/// The label every proof candidate carries.
pub(crate) const PROOF_PATCH_LABEL: &str = "Patch";
/// The longest side of a proof candidate's preview.
const PROOF_PREVIEW_SIDE: u32 = 64;

/// The 8-bit code of one channel of a proof patch's pixel: a ramp across the patch, offset by the
/// seed and the candidate's number.
pub(crate) fn proof_code(seed: u64, candidate: u64, column: u64, row: u64, channel: u64) -> u8 {
    ((column * 7 + row * 3 + channel * 61 + seed + candidate * 11) % 256) as u8
}

/// The linear value a `linear` proof patch holds for a code: signed and past 1, as linear light is.
pub(crate) fn proof_linear(code: u8) -> f32 {
    f32::from(code) / 255.0 * 1.5 - 0.25
}

/// The coverage of one pixel of a proof patch: a repeating step through 0, ¼, ½, ¾ and 1.
pub(crate) fn proof_coverage(candidate: u64, column: u64, row: u64) -> f32 {
    ((column + row + candidate) % 5) as f32 / 4.0
}

/// A proof preview's dimensions: the patch's, reduced to at most 64 on its longest side.
pub(crate) fn proof_preview(width: u32, height: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= PROOF_PREVIEW_SIDE {
        return (width, height);
    }
    let scale = |side: u32| {
        (u64::from(side) * u64::from(PROOF_PREVIEW_SIDE) / u64::from(longest)).max(1) as u32
    };
    (scale(width), scale(height))
}

/// The developer candidate producer. In tests it may hold each published candidate at a gate.
pub(crate) struct CandidateProofModule {
    descriptor: ModuleDescriptor,
    #[cfg(test)]
    gate: Option<Arc<luxforge_testbase::Gate>>,
}

impl CandidateProofModule {
    pub(crate) fn new() -> Self {
        let integer = |name: &str, min: i64, max: i64, default: i64, notes: &str| {
            ParameterDescriptor::integer(name, min, max)
                .default(default)
                .notes(notes)
        };
        Self {
            descriptor: ModuleDescriptor {
                id: CANDIDATE_PROOF_MODULE.into(),
                title: "Candidates proof".into(),
                hint: Some("Deterministic candidate patches for the candidate seam".into()),
                developer: true,
                tasks: vec![TaskDescriptor {
                    id: CANDIDATE_PROOF_TASK.into(),
                    title: "Generate proof patches".into(),
                    notes: "Publishes count deterministic candidate patches of width × height content pixels at x, y of this photograph, each with a preview of at most 64 pixels a side, labelled Patch with the seed in its provenance; each is reserved before it is made, so a candidate over its bound is refused resource-limit and a fifth ends the job resource-limit with four live. Read them with job.read and job.read-candidate.".into(),
                    asset: true,
                    profile: false,
                    uses: Vec::new(),
                    parameters: vec![
                        integer("count", 1, 8, 1, "how many candidates to publish"),
                        integer("width", 1, 4096, 16, "each patch's width in content pixels"),
                        integer("height", 1, 4096, 16, "each patch's height in content pixels"),
                        integer("x", 0, 65535, 0, "the patch's left edge in content pixels"),
                        integer("y", 0, 65535, 0, "the patch's top edge in content pixels"),
                        integer("seed", 0, 1_000_000, 0, "offsets every code; candidate n adds 11 n"),
                        ParameterDescriptor::enumeration("colour", ["linear", "srgb8"])
                            .default("linear")
                            .notes("linear: values code / 255 × 1.5 − 0.25 in linear light; srgb8: the codes as 8-bit sRGB"),
                    ],
                    apply: None,
                    reads: None,
                    flags: Vec::new(),
                }],
                ..ModuleDescriptor::default()
            },
            #[cfg(test)]
            gate: None,
        }
    }

    /// The producer holding every published candidate at `gate`, for tests that act while it runs.
    #[cfg(test)]
    pub(crate) fn gated(gate: Arc<luxforge_testbase::Gate>) -> Self {
        Self {
            gate: Some(gate),
            ..Self::new()
        }
    }
}

impl ToolModule for CandidateProofModule {
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

impl CapabilityModule for CandidateProofModule {
    /// Publish `count` candidates, each reserved first, and answer `{candidates: [n, …]}`.
    fn run_task(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        if task_id != CANDIDATE_PROOF_TASK {
            return Err(Error::validation(format!("unknown task {task_id}")));
        }
        let number = |name: &str, default: u64| {
            parameters
                .get(name)
                .and_then(Value::as_u64)
                .unwrap_or(default)
        };
        let side = |name: &str| u32::try_from(number(name, 16)).unwrap_or(u32::MAX);
        let (count, width, height) = (number("count", 1), side("width"), side("height"));
        let (x, y) = (
            u32::try_from(number("x", 0)).unwrap_or(u32::MAX),
            u32::try_from(number("y", 0)).unwrap_or(u32::MAX),
        );
        let seed = number("seed", 0);
        let srgb = parameters.get("colour").and_then(Value::as_str) == Some("srgb8");
        let (preview_width, preview_height) = proof_preview(width, height);
        let mut published = Vec::new();
        for candidate in 0..count {
            context.progress(
                Some(candidate as f64 / count as f64),
                &format!("candidate {} of {count}", candidate + 1),
            );
            let reservation = context.reserve_candidate(CandidateShape {
                width,
                height,
                preview_width,
                preview_height,
            })?;
            let pixels = (0..u64::from(height))
                .flat_map(|row| (0..u64::from(width)).map(move |column| (column, row)));
            let codes: Vec<u8> = pixels
                .clone()
                .flat_map(|(column, row)| {
                    (0..3).map(move |channel| proof_code(seed, candidate, column, row, channel))
                })
                .collect();
            let coverage: Vec<f32> = pixels
                .map(|(column, row)| proof_coverage(candidate, column, row))
                .collect();
            let linear: Vec<f32> = if srgb {
                Vec::new()
            } else {
                codes.iter().map(|code| proof_linear(*code)).collect()
            };
            let preview: Vec<u8> = (0..u64::from(preview_height))
                .flat_map(|row| (0..u64::from(preview_width)).map(move |column| (column, row)))
                .flat_map(|(column, row)| {
                    let source_column = column * u64::from(width) / u64::from(preview_width);
                    let source_row = row * u64::from(height) / u64::from(preview_height);
                    (0..3).map(move |channel| {
                        proof_code(seed, candidate, source_column, source_row, channel)
                    })
                })
                .collect();
            let mut provenance = Map::new();
            provenance.insert("seed".into(), json!(seed + candidate * 11));
            let colour = if srgb {
                PatchColour::Srgb8(&codes)
            } else {
                PatchColour::LinearSrgb(&linear)
            };
            let number = reservation.publish(CandidatePatch {
                rect: [x, y, width, height],
                colour,
                coverage: &coverage,
                preview: &preview,
                preview_width,
                preview_height,
                label: PROOF_PATCH_LABEL,
                provenance,
            })?;
            published.push(number);
            #[cfg(test)]
            if let Some(gate) = &self.gate {
                gate.pass_unless(|| context.is_cancelled());
            }
        }
        context.progress(Some(1.0), "published");
        Ok(json!({"candidates": published}))
    }
}
