//! Developer proof of a model selection without a model. `luxforge.selections` declares one task,
//! `publish-proof-selection`, that publishes the checked-in alpha raster
//! `fixtures/masks/proof-selection.pgm` as a `mask-alpha` artifact, exactly as a selection task
//! publishes the raster its model and the guided upsampler made. The bytes are fixed, so the
//! artifact's identity is fixed ([`PROOF_SELECTION_ARTIFACT`]) and a script can name it in the
//! `mask.create-model` that follows: the GPU qualification corpus reproduces its model-mask rows this
//! way. A test fixture, registered in developer runs only.
use super::{
    ActionInput, ActionPlan, CapabilityModule, LayerReport, ModuleDescriptor, Processing,
    StageContext, ToolModule,
};
use crate::{
    Error,
    artifacts::ArtifactMeta,
    capabilities::{context::ModuleContext, descriptor::TaskDescriptor},
};
use serde_json::{Map, Value, json};

pub(crate) const SELECTION_PROOF_MODULE: &str = "luxforge.selections";
pub(crate) const SELECTION_PROOF_TASK: &str = "publish-proof-selection";

/// The checked-in raster: a binary PGM of 96 × 64 codes, a soft ellipse, a hard rectangle and a
/// band of stripes over zero.
const FIXTURE: &[u8] = include_bytes!("../../../../fixtures/masks/proof-selection.pgm");
const HEADER: &[u8] = b"P5\n96 64\n255\n";
pub(crate) const PROOF_SELECTION_WIDTH: u32 = 96;
pub(crate) const PROOF_SELECTION_HEIGHT: u32 = 64;

/// The identity the published raster takes, the SHA-256 of its codes: what a script names.
pub const PROOF_SELECTION_ARTIFACT: &str =
    "artifact-d808f41015876053c0122c8e78285f9a56389199669be0479850efe5e37b1f3f";

/// The raster's codes, row-major.
pub(crate) fn proof_selection_codes() -> &'static [u8] {
    assert!(FIXTURE.starts_with(HEADER), "the fixture is a 96 x 64 PGM");
    &FIXTURE[HEADER.len()..]
}

/// The developer selection producer.
pub(crate) struct SelectionProofModule {
    descriptor: ModuleDescriptor,
}

impl SelectionProofModule {
    pub(crate) fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: SELECTION_PROOF_MODULE.into(),
                title: "Selections proof".into(),
                hint: Some("A fixed model-selection raster for the model mask kind".into()),
                developer: true,
                tasks: vec![TaskDescriptor {
                    id: SELECTION_PROOF_TASK.into(),
                    title: "Publish proof selection".into(),
                    notes: "Publishes the checked-in 96 × 64 alpha raster as a mask-alpha \
                            artifact, whose identity is always \
                            artifact-d808f41015876053c0122c8e78285f9a56389199669be0479850efe5e37b1f3f; \
                            mask.create-model and mask.add-model bind it. Answers {artifact, width, \
                            height}."
                        .into(),
                    asset: true,
                    profile: false,
                    uses: Vec::new(),
                    parameters: Vec::new(),
                    apply: None,
                    reads: None,
                    flags: Vec::new(),
                }],
                ..ModuleDescriptor::default()
            },
        }
    }
}

impl ToolModule for SelectionProofModule {
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

impl CapabilityModule for SelectionProofModule {
    /// Publish the fixture and answer `{artifact, width, height}`.
    fn run_task(
        &self,
        task_id: &str,
        _: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        if task_id != SELECTION_PROOF_TASK {
            return Err(Error::validation(format!("unknown task {task_id}")));
        }
        let artifact = context.publish_artifact(
            proof_selection_codes(),
            ArtifactMeta {
                kind: crate::mask::ALPHA_KIND.into(),
                width: Some(PROOF_SELECTION_WIDTH),
                height: Some(PROOF_SELECTION_HEIGHT),
                colour: None,
            },
        )?;
        context.progress(Some(1.0), "published");
        Ok(json!({
            "artifact": artifact,
            "width": PROOF_SELECTION_WIDTH,
            "height": PROOF_SELECTION_HEIGHT,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// The fixture is the raster its dimensions say, and the identity a script names is its hash.
    #[test]
    fn the_proof_selection_is_a_fixed_raster_with_a_fixed_identity() {
        let codes = proof_selection_codes();
        assert_eq!(
            codes.len(),
            (PROOF_SELECTION_WIDTH * PROOF_SELECTION_HEIGHT) as usize
        );
        assert_eq!(
            PROOF_SELECTION_ARTIFACT,
            format!("artifact-{:x}", Sha256::digest(codes))
        );
        crate::mask::check_alpha(
            crate::mask::ALPHA_KIND,
            Some(PROOF_SELECTION_WIDTH),
            Some(PROOF_SELECTION_HEIGHT),
            codes.len() as u64,
        )
        .unwrap();
        assert!(codes.contains(&0) && codes.contains(&255));
    }
}
