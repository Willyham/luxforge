//! Frozen patches for the core's tests: a candidate of the photograph as it stands, published,
//! pinned, published as an artifact and committed exactly as the catalog owner's accept does
//! (`crate::api::owner`'s `accept.rs`), but on the calling thread. The content is the developer
//! producer's (`luxforge.candidates`): linear values from the proof ramp and coverage stepping
//! through 0, ¼, ½, ¾ and 1, so a test can compute what each pixel becomes.
use super::module::ACCEPT;
use crate::{
    AcceptedCandidate, ActionResult, ArtifactId, AssetId, EditorService, Error, JobId,
    artifacts::candidates::{CandidatePatch, CandidateShape, CandidateSink, PatchColour},
    editor::mutation,
    jobs::JobControl,
    modules::{proof_code, proof_coverage, proof_linear},
};
use serde_json::{Map, Value, json};
use std::sync::{Arc, atomic::AtomicBool};

/// One patch's content: the producer's ramp for `seed` at `rect`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PatchSpec {
    pub rect: [u32; 4],
    pub seed: u64,
    pub opacity: Option<u8>,
}

impl PatchSpec {
    pub(crate) fn at(rect: [u32; 4]) -> Self {
        Self {
            rect,
            seed: 0,
            opacity: None,
        }
    }

    /// The linear colour and coverage of patch pixel `(column, row)`.
    pub(crate) fn pixel(&self, column: u32, row: u32) -> ([f32; 3], f32) {
        let (column, row) = (u64::from(column), u64::from(row));
        (
            [0, 1, 2].map(|channel| proof_linear(proof_code(self.seed, 0, column, row, channel))),
            proof_coverage(0, column, row),
        )
    }
}

/// A candidate of `asset` as it stands now, live in the service's store: its job and number.
pub(crate) fn candidate(
    service: &EditorService,
    asset: &AssetId,
    spec: PatchSpec,
) -> Result<(JobId, u8), Error> {
    let [x, y, width, height] = spec.rect;
    let sink = CandidateSink {
        store: service.candidates().clone(),
        job_id: JobId::new(),
        module_id: "luxforge.candidates".into(),
        task_id: "generate-proof-patches".into(),
        basis: service.candidate_basis(asset)?,
        control: JobControl::new(),
    };
    let mut linear = Vec::new();
    let mut coverage = Vec::new();
    for row in 0..height {
        for column in 0..width {
            let (rgb, alpha) = spec.pixel(column, row);
            linear.extend_from_slice(&rgb);
            coverage.push(alpha);
        }
    }
    let preview = vec![128u8; 3];
    let shape = CandidateShape {
        width,
        height,
        preview_width: 1,
        preview_height: 1,
    };
    let number = sink.store.reserve(&sink, shape)?.publish(CandidatePatch {
        rect: [x, y, width, height],
        colour: PatchColour::LinearSrgb(&linear),
        coverage: &coverage,
        preview: &preview,
        preview_width: 1,
        preview_height: 1,
        label: "Patch",
        provenance: Map::from_iter([("seed".to_owned(), json!(spec.seed))]),
    })?;
    sink.store.finished(&sink.job_id, false);
    Ok((sink.job_id, number))
}

/// Accept candidate `number` of `job` as the owner does: pin it, publish its bytes, record the
/// artifact and commit `accept-correction-patch` at `revision` under `request`, settling the pin.
pub(crate) fn accept(
    service: &mut EditorService,
    asset: &AssetId,
    revision: u64,
    request: &str,
    job: &JobId,
    number: u8,
    opacity: Option<u8>,
) -> Result<(ActionResult, ArtifactId), Error> {
    let pin = service.candidates().pin(job, number)?;
    let read = pin.candidate().clone();
    let (record, prepared) = pin.publish(&service.artifact_writer()?, &AtomicBool::new(false))?;
    let artifact = record.id.clone();
    service.register_artifact(record, prepared, true)?;
    let mut parameters = json!({"job_id": job, "candidate": number});
    if let Some(opacity) = opacity {
        parameters["opacity"] = json!(opacity);
    }
    let accepted = Arc::new(AcceptedCandidate {
        artifact: artifact.clone(),
        rect: read.rect,
        label: read.label.clone(),
        provenance: read.provenance.clone(),
        input: read
            .input
            .clone()
            .expect("a candidate of a stack that says its input"),
    });
    let result = service.accept_candidate(
        asset,
        mutation(revision, request),
        ACCEPT,
        parameters,
        accepted,
    );
    match &result {
        Ok(_) => pin.accepted(),
        Err(_) => drop(pin),
    }
    result.map(|result| (result, artifact))
}

/// A candidate of `spec` made now and accepted at once at the current revision.
pub(crate) fn add_patch(
    service: &mut EditorService,
    asset: &AssetId,
    request: &str,
    spec: PatchSpec,
) -> Result<(ActionResult, ArtifactId), Error> {
    let (job, number) = candidate(service, asset, spec)?;
    let revision = service.state(asset)?.revision;
    accept(
        service,
        asset,
        revision,
        request,
        &job,
        number,
        spec.opacity,
    )
}

/// The sRGB code a linear value is drawn as in an 8-bit frame, rounded.
pub(crate) fn encode(linear: f64) -> f64 {
    let value = linear.clamp(0.0, 1.0);
    let encoded = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    encoded * 255.0
}

/// The linear value of an 8-bit sRGB code.
pub(crate) fn decode(code: u8) -> f64 {
    let value = f64::from(code) / 255.0;
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// What a patch pixel makes of an 8-bit input pixel at `opacity`, in codes before rounding: the
/// frozen composite in linear light, `in + c · (patch − in)` with `c = α · opacity / 100`.
pub(crate) fn composited(input: [u8; 3], rgb: [f32; 3], alpha: f32, opacity: u8) -> [f64; 3] {
    let c = f64::from(alpha) * f64::from(opacity) / 100.0;
    [0, 1, 2].map(|channel| {
        let i = decode(input[channel]);
        encode(i + c * (f64::from(rgb[channel]) - i))
    })
}

/// The parameters of an accept, for a test that sends one itself.
#[allow(dead_code)]
pub(crate) fn accept_parameters(job: &JobId, number: u8) -> Value {
    json!({"job_id": job, "candidate": number})
}
