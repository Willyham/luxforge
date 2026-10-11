//! One Generative tier job: read the photograph once, then make each candidate in turn from
//! consecutive seeds, reserving it before it is made and publishing it as soon as it is, with
//! progress per step and cancellation at every step boundary.
use super::{
    fill::{self, Context},
    module::{FillRequest, GENERATIVE_TIER, MAX_SEED},
};
use crate::{
    Error,
    artifacts::candidates::{CandidatePatch, CandidateShape, MAX_CANDIDATE_LABEL, PatchColour},
    capabilities::{context::ModuleContext, read::MAX_MODULE_READ_PIXELS},
    inference::{Conditioning, GenerateRequest},
};
use serde_json::{Value, json};
use std::hash::{BuildHasher, RandomState};

/// The label a fill without a prompt carries.
pub(crate) const FILL_LABEL: &str = "Generative fill";

/// The label a candidate carries: `Generative fill`, or `Replace "<prompt>"` with the prompt cut
/// with an ellipsis to fit the label's 32 characters.
pub(crate) fn label(prompt: Option<&str>) -> String {
    let Some(prompt) = prompt else {
        return FILL_LABEL.into();
    };
    // `Replace "` and the closing quote take ten characters.
    let room = MAX_CANDIDATE_LABEL - 10;
    let quoted: String = if prompt.chars().count() <= room {
        prompt.into()
    } else {
        let mut cut: String = prompt.chars().take(room - 1).collect();
        cut.truncate(cut.trim_end().len());
        cut.push('…');
        cut
    };
    format!("Replace \"{quoted}\"")
}

/// A seed no earlier request is likely to have used, within the seeds a request may name.
fn drawn_seed() -> u64 {
    RandomState::new().hash_one(std::time::SystemTime::now()) & MAX_SEED
}

/// Run one request: `{tier, model, candidates, seeds, next_seed, rect, prompt}` once every
/// candidate is published.
pub(crate) fn run(
    request: &FillRequest,
    context: &ModuleContext,
    hold: &dyn Fn(&ModuleContext),
) -> Result<Value, Error> {
    let model = request.model;
    let count = request.candidates;
    let first = request.seed.unwrap_or_else(drawn_seed);
    // Consecutive seeds stay within the seeds a request may name.
    let seeds: Vec<u64> = (0..u64::from(count))
        .map(|i| first.wrapping_add(i) & MAX_SEED)
        .collect();
    if let Some(encoder) = model.text_encoder
        && request.prompt.is_some()
    {
        // The host granted the request only with the text encoder installed.
        context.resource_path(encoder.resource)?;
    }

    context.progress(Some(0.0), "reading the photograph");
    let mut reader = context.stage_reader()?;
    let stage = reader.stage();
    let coverage = request
        .selection
        .coverage(stage)?
        .ok_or_else(|| Error::validation("the selection covers no pixel of the photograph"))?;
    let prepared = Context::prepare(
        coverage,
        stage,
        model.side as usize,
        MAX_MODULE_READ_PIXELS,
        &mut |region| reader.read(region).map(|read| read.linear),
        &|| context.checkpoint(),
    )?;
    drop(reader);
    let bounds = prepared.coverage.bounds();
    let (preview_width, preview_height) = fill::preview_size(bounds.width, bounds.height);

    context.progress(Some(0.0), "loading the model");
    let generator = context.generator(model.runtime)?;
    let conditioning = match &request.prompt {
        Some(prompt) => Conditioning::Prompt(prompt),
        None => Conditioning::None,
    };
    let label = label(request.prompt.as_deref());
    let mut published = Vec::with_capacity(count as usize);
    for (index, &seed) in seeds.iter().enumerate() {
        let number = index + 1;
        context.progress(
            Some(index as f64 / f64::from(count)),
            &format!("candidate {number} of {count}"),
        );
        // Admitted before anything of it is made.
        let reservation = context.reserve_candidate(CandidateShape {
            width: bounds.width,
            height: bounds.height,
            preview_width,
            preview_height,
        })?;
        let generation = GenerateRequest {
            width: model.side,
            height: model.side,
            image: &prepared.model_image,
            mask: &prepared.model_mask,
            conditioning,
            seed,
            steps: model.steps,
        };
        let generated = generator.generate(
            &generation,
            &mut |step| {
                context.progress(
                    Some((index as f64 + step.fraction()) / f64::from(count)),
                    &format!(
                        "candidate {number} of {count}, step {} of {}",
                        step.step, step.steps
                    ),
                );
            },
            context.cancel(),
        )?;
        context.checkpoint()?;
        let finished = prepared.finish(&generated.image, seed)?;
        let mut provenance = generated.provenance.fields();
        provenance.insert("tier".into(), json!(GENERATIVE_TIER));
        provenance.insert("encoding".into(), json!(fill::encode::ENCODING));
        provenance.insert("encoding_scale".into(), json!(prepared.encoding.scale));
        provenance.insert("model_side".into(), json!(model.side));
        let number = reservation.publish(CandidatePatch {
            rect: finished.rect,
            colour: PatchColour::LinearSrgb(&finished.linear),
            coverage: &finished.coverage,
            preview: &finished.preview,
            preview_width: finished.preview_width,
            preview_height: finished.preview_height,
            label: &label,
            provenance,
        })?;
        published.push(number);
        hold(context);
    }
    context.progress(Some(1.0), "published");
    Ok(json!({
        "tier": GENERATIVE_TIER,
        "model": model.id,
        "candidates": published,
        "seeds": seeds,
        "next_seed": first.wrapping_add(u64::from(count)) & MAX_SEED,
        "rect": [bounds.x0, bounds.y0, bounds.width, bounds.height],
        "prompt": request.prompt.is_some(),
    }))
}
