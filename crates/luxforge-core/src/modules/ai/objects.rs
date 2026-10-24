//! Objects (`docs/design/ai-editing.md`, "Picking: hover, click, box and brush"): `task.analyse`,
//! which runs the segmenter's encoder over the analysis input, `query.pick-object`, the picker's
//! programmatic form, and `task.select-object`, which publishes a chosen object as a selection.
//!
//! One picker serves them all, and the desktop's hover too, which sends the same query: a request
//! names one prompt in the content stage's normalized coordinates — a `point` (hover or click),
//! labelled `positive` and `negative` points with an optional `box`, or a brush `stroke` with its
//! `radius` — and the cached embedding is decoded for it on the host's query worker. A stroke
//! becomes the prototype's prompts and decision ([`super::pick::decide`]); any other prompt is
//! decoded once and its candidate chosen as hover chooses ([`super::pick::pick_hover`]), biased to
//! the whole object. Identical model outputs and prompts give identical answers, so the selection
//! task, which decodes the same prompt on the module lane, selects exactly what the query answered.
use super::{
    analysis::{
        Guide, cached_guide, key,
        models::{AnalysisModels, SEGMENTER, SessionModel},
        models_value, prepared,
    },
    object_region::ObjectRaster,
    pick::{self, Decision, Params, Stroke},
    picked::{PickedObject, PickedObjects},
    segment::{Candidate, Embedding, Point, Prompt, Segmenter},
};
use crate::{
    Error,
    artifacts::{ArtifactMeta, candidates::base64},
    capabilities::{
        context::ModuleContext,
        host::{Requirement, RequirementKind},
        read::StageIdentity,
    },
    mask::ObjectPrompt,
    mask::guided,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// The most labelled points one prompt names, positive and negative each.
pub(crate) const MAX_PROMPT_POINTS: usize = 16;
/// The most pixels one candidate's coverage preview holds: what a hover overlay uploads.
pub(crate) const PREVIEW_PIXELS: u32 = 1 << 16;
/// The class a selection of an object records.
pub(crate) const OBJECT_CLASS: &str = crate::mask::OBJECT_CLASS;

/// The segmenter's analysis of one stage, as the cache holds it.
#[derive(Debug)]
pub(crate) struct SegmenterAnalysis {
    pub(crate) embedding: Embedding,
}

/// One request's prompt, in analysis pixels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    /// One prompt decoded once: a point, labelled points, a box or a box with points.
    Prompt(Prompt),
    /// A painted stroke: the prototype's prompts and decision.
    Stroke(Stroke),
}

/// A request's normalized points.
fn points(parameters: &Map<String, Value>, name: &str) -> Vec<[f64; 2]> {
    parameters
        .get(name)
        .and_then(Value::as_array)
        .map(|points| {
            points
                .iter()
                .filter_map(|point| {
                    let pair = point.as_array()?;
                    Some([pair.first()?.as_f64()?, pair.get(1)?.as_f64()?])
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Request {
    /// The prompt `parameters` name, in the pixels of an analysis input of `size`: exactly one of
    /// `point`, `stroke` (with `radius`, in content heights) or labelled `positive` and `negative`
    /// points with an optional `box`.
    pub(crate) fn parse(parameters: &Map<String, Value>, size: (u32, u32)) -> Result<Self, Error> {
        let at = |p: [f64; 2]| Point::from_normalized(p, size);
        let (point, stroke) = (points(parameters, "point"), points(parameters, "stroke"));
        let (positive, negative, bbox) = (
            points(parameters, "positive"),
            points(parameters, "negative"),
            points(parameters, "box"),
        );
        let labelled = !positive.is_empty() || !negative.is_empty() || !bbox.is_empty();
        let forms = usize::from(!point.is_empty())
            + usize::from(!stroke.is_empty())
            + usize::from(labelled);
        if forms != 1 {
            return Err(Error::validation(
                "name exactly one prompt: point, stroke with radius, or positive and negative points with an optional box",
            ));
        }
        if let Some(&p) = point.first() {
            return Ok(Self::Prompt(Prompt {
                positive: vec![at(p)],
                ..Prompt::default()
            }));
        }
        if !stroke.is_empty() {
            let radius = parameters
                .get("radius")
                .and_then(Value::as_f64)
                .ok_or_else(|| Error::validation("a stroke needs its radius"))?;
            return Ok(Self::Stroke(Stroke {
                points: stroke.into_iter().map(at).collect(),
                radius: (radius * f64::from(size.1)) as f32,
            }));
        }
        let bbox = match bbox.as_slice() {
            [] => None,
            [a, b] => Some((at(*a), at(*b))),
            _ => return Err(Error::validation("a box is its two corners")),
        };
        Ok(Self::Prompt(Prompt {
            positive: positive.into_iter().map(at).collect(),
            negative: negative.into_iter().map(at).collect(),
            bbox,
        }))
    }
}

/// What a decode answered for a request: every candidate with its scores, and the decision.
pub(crate) struct Picked {
    /// Each candidate, by prompt and index, with its scores; stroke coverage only for a stroke.
    pub(crate) candidates: Vec<PickedCandidate>,
    pub(crate) decision: Decision,
}

pub(crate) struct PickedCandidate {
    pub(crate) prompt: usize,
    pub(crate) index: usize,
    pub(crate) candidate: Candidate,
    pub(crate) stability: f32,
    pub(crate) coverage: Option<(f32, f32)>,
}

/// Decode `request` against `embedding` and decide, as hover, a click and a stroke decide.
pub(crate) fn pick(
    decoder: &dyn crate::inference::Session,
    embedding: &mut Embedding,
    request: &Request,
    cancel: &crate::Cancel,
) -> Result<Picked, Error> {
    let params = Params::default();
    match request {
        Request::Prompt(prompt) => {
            let mut results =
                Segmenter::decode_many(decoder, embedding, std::slice::from_ref(prompt), cancel)?;
            let candidates = results.pop().unwrap_or_default();
            let decision = match pick::pick_hover(&candidates, &params) {
                Some(index) => Decision::One(index),
                None => Decision::Nothing,
            };
            Ok(Picked {
                candidates: candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| PickedCandidate {
                        prompt: 0,
                        index,
                        stability: pick::stability(&candidate.logits, 1.0),
                        candidate,
                        coverage: None,
                    })
                    .collect(),
                decision,
            })
        }
        Request::Stroke(stroke) => {
            let prompts = pick::prompts(stroke, embedding.source, &params);
            if prompts.is_empty() {
                return Ok(Picked {
                    candidates: Vec::new(),
                    decision: Decision::Nothing,
                });
            }
            let results = Segmenter::decode_many(decoder, embedding, &prompts, cancel)?;
            let resolution = pick::decide(stroke, &results, &params);
            let candidates = resolution
                .scored
                .iter()
                .map(|scored| PickedCandidate {
                    prompt: scored.prompt,
                    index: scored.candidate,
                    candidate: results[scored.prompt][scored.candidate].clone(),
                    stability: scored.stability,
                    coverage: Some((scored.stroke_coverage, scored.object_coverage)),
                })
                .collect();
            Ok(Picked {
                candidates,
                decision: resolution.decision,
            })
        }
    }
}

impl Picked {
    /// The candidates the decision chose, by position in [`Self::candidates`].
    pub(crate) fn chosen(&self) -> Vec<usize> {
        match &self.decision {
            Decision::Nothing => Vec::new(),
            Decision::One(index) => vec![*index],
            Decision::Several(indices) => indices.clone(),
        }
    }

    /// The query's answer. `previews` is `chosen`, `all` or `none`: which candidates carry a
    /// coverage preview of at most [`PREVIEW_PIXELS`].
    pub(crate) fn value(&self, previews: &str) -> Value {
        let chosen = self.chosen();
        let candidates: Vec<Value> = self
            .candidates
            .iter()
            .enumerate()
            .map(|(position, picked)| {
                let logits = &picked.candidate.logits;
                let (w, h) = (f64::from(logits.source.0), f64::from(logits.source.1));
                let bounds = pick::bounds(logits);
                let area = bounds.map_or(0.0, |_| area_share(logits));
                let preview = bounds
                    .filter(|_| {
                        previews == "all" || (previews == "chosen" && chosen.contains(&position))
                    })
                    .map(|rect| {
                        let ((pw, ph), codes) =
                            pick::coverage_preview(logits, rect, PREVIEW_PIXELS);
                        json!({"width": pw, "height": ph, "codes": base64(&codes)})
                    });
                json!({
                    "candidate": position,
                    "prompt": picked.prompt,
                    "index": picked.index,
                    "predicted_iou": rounded(picked.candidate.predicted_iou),
                    "stability": rounded(picked.stability),
                    "stroke_coverage": picked.coverage.map(|(stroke, _)| rounded(stroke)),
                    "object_coverage": picked.coverage.map(|(_, object)| rounded(object)),
                    "area": (area * 10_000.0).round() / 10_000.0,
                    "bounds": bounds.map(|[x0, y0, x1, y1]| [
                        f64::from(x0) / w, f64::from(y0) / h, f64::from(x1) / w, f64::from(y1) / h,
                    ]),
                    "preview": preview,
                })
            })
            .collect();
        json!({
            "decision": self.decision.name(),
            "chosen": chosen,
            "candidates": candidates,
        })
    }
}

/// Four decimals of a score.
fn rounded(value: f32) -> f64 {
    (f64::from(value) * 10_000.0).round() / 10_000.0
}

/// The share of the analysis input a candidate covers: its logit cells above 0 among those inside
/// the input.
fn area_share(logits: &super::segment::Logits) -> f64 {
    let (vw, vh) = pick::valid_cells(logits);
    if vw == 0 || vh == 0 {
        return 0.0;
    }
    let inside: usize = logits
        .data
        .chunks_exact(logits.width)
        .take(vh)
        .map(|row| row[..vw].iter().filter(|&&v| v > 0.0).count())
        .sum();
    inside as f64 / (vw * vh) as f64
}

/// The run's segmenter, or `validation` when it offers none.
pub(crate) fn segmenter(models: &AnalysisModels) -> Result<[&'static SessionModel; 2], Error> {
    let set = models
        .set(&SEGMENTER)
        .ok_or_else(|| Error::validation("this run offers no segmenter"))?;
    Ok([set[0], set[1]])
}

/// The segmenter's analysis and the guide of the context's stage: from the cache, or computed now
/// (reading the stage and running the encoder only for what is missing) and stored. Answers
/// whether both came from the cache.
fn analysed(
    context: &ModuleContext,
    encoder: &SessionModel,
) -> Result<(Arc<SegmenterAnalysis>, Arc<Guide>, bool), Error> {
    let identity = context.identity()?;
    let cache = context.analysis()?;
    let embedding_key = key(identity, encoder.file.resource, encoder.file.version);
    if let (Some(found), Some(guide)) = (
        cache.get::<SegmenterAnalysis>(&embedding_key),
        cached_guide(context)?,
    ) {
        return Ok((found, guide, true));
    }
    context.progress(Some(0.0), "reading the photograph");
    let (raster, guide, _) = prepared(context)?;
    if let Some(found) = cache.get::<SegmenterAnalysis>(&embedding_key) {
        return Ok((found, guide, false));
    }
    context.progress(Some(0.3), "finding objects");
    let session = context.session(encoder.runtime)?;
    let embedding = Segmenter::encode(
        &*session,
        &raster.codes(),
        (raster.width, raster.height),
        context.cancel(),
    )?;
    let found = Arc::new(SegmenterAnalysis { embedding });
    cache.insert(embedding_key, found.clone(), found.embedding.bytes())?;
    Ok((found, guide, false))
}

/// `task.analyse`: the segmenter's embedding and the guide of the context's stage.
pub(crate) fn analyse(context: &ModuleContext, models: &AnalysisModels) -> Result<Value, Error> {
    let [encoder, decoder] = segmenter(models)?;
    let (found, guide, cached) = analysed(context, encoder)?;
    context.progress(Some(1.0), "ready");
    Ok(json!({
        "cached": cached,
        "analysed": analysed_value(context.identity()?),
        "input": [found.embedding.source.0, found.embedding.source.1],
        "guide": [guide.width, guide.height],
        "models": models_value(&[encoder, decoder]),
    }))
}

/// The input an analysis is of, as its result and a refusal for want of it name it: the
/// photograph, its source fingerprint and its repair prefix (the `prefix_hash` a selection
/// records), so a client can tell whether the analysis it ran is the one a pick needs.
fn analysed_value(identity: &StageIdentity) -> Value {
    json!({
        "asset_id": identity.asset,
        "source": identity.source,
        "prefix_hash": identity.prefix,
    })
}

/// The refusal of a pick on a photograph the segmenter has not analysed as it is now: the
/// requirement, the task that meets it and the input it must analyse.
fn not_analysed(identity: &StageIdentity) -> Error {
    Error::not_ready(format!(
        "photograph {} is not analysed as it is now: run task.analyse first",
        identity.asset
    ))
    .with_data(json!({
        "requirements": [Requirement {
            kind: RequirementKind::Analysis,
            id: "analyse".into(),
            state: "missing".into(),
        }],
        "task": "task.analyse",
        "analysis": analysed_value(identity),
    }))
}

/// `query.pick-object` on the query worker: decode the request against the cached embedding and
/// answer the candidates and the decision. `not-ready` naming `task.analyse` when the photograph is
/// not analysed as it is now.
///
/// The decode moves the embedding into the decoder and takes it back, so the worker decodes from
/// the cache's held copy of the entry ([`crate::capabilities::analysis`]): copied out of the entry
/// by the first pick of a photograph, given back after each decode, and dropped by the cache with
/// its entry, so successive hovers over one photograph copy nothing and the copy never outlives
/// the analysis it copies.
pub(crate) fn pick_object(
    context: &ModuleContext,
    models: &AnalysisModels,
    parameters: &Map<String, Value>,
) -> Result<Value, Error> {
    let [encoder, decoder] = segmenter(models)?;
    let identity = context.identity()?;
    let cache = context.analysis()?;
    let embedding_key = key(identity, encoder.file.resource, encoder.file.version);
    let Some(found) = cache.get::<SegmenterAnalysis>(&embedding_key) else {
        drop(cache.take_held::<Embedding>(&embedding_key));
        return Err(not_analysed(identity));
    };
    let mut embedding = cache
        .take_held::<Embedding>(&embedding_key)
        .unwrap_or_else(|| found.embedding.clone());
    drop(found);
    let answered = (|| {
        let request = Request::parse(parameters, embedding.source)?;
        let session = context.session(decoder.runtime)?;
        pick(&*session, &mut embedding, &request, context.cancel())
    })();
    let source = embedding.source;
    let bytes = embedding.bytes();
    cache.hold(embedding_key, embedding, bytes);
    let previews = parameters
        .get("previews")
        .and_then(Value::as_str)
        .unwrap_or("chosen");
    let mut value = answered?.value(previews);
    value["analysis"] = json!({
        "input": [source.0, source.1],
        "models": models_value(&[encoder, decoder]),
    });
    Ok(value)
}

/// `task.select-object`: decode the request as the query does, choose its candidate (the one
/// `candidate` names, or the decision's one object), refine it against the guide to the artifact's
/// resolution and publish it as a `mask-alpha` artifact, recorded in `picked` with the photograph
/// it was computed from and the prompt it was picked with, which the result carries.
pub(crate) fn select_object(
    context: &ModuleContext,
    models: &AnalysisModels,
    parameters: &Map<String, Value>,
    picked_objects: &PickedObjects,
) -> Result<Value, Error> {
    let prompt = ObjectPrompt::of_request(parameters)?;
    let [encoder, decoder] = segmenter(models)?;
    let (found, guide, _) = analysed(context, encoder)?;
    let mut embedding = found.embedding.clone();
    drop(found);
    let request = Request::parse(parameters, embedding.source)?;
    context.progress(Some(0.6), "outlining the object");
    let session = context.session(decoder.runtime)?;
    let picked = pick(&*session, &mut embedding, &request, context.cancel())?;
    let chosen = picked.chosen();
    let position = match parameters.get("candidate").and_then(Value::as_u64) {
        Some(named) => {
            let named = named as usize;
            if named >= picked.candidates.len() {
                return Err(Error::validation(format!(
                    "candidate {named} is not one of the prompt's {} candidates",
                    picked.candidates.len()
                )));
            }
            named
        }
        None => match chosen.as_slice() {
            [one] if matches!(picked.decision, Decision::One(_)) => *one,
            [] => {
                return Err(Error::validation(
                    "the prompt found no object; pick another or paint over it",
                )
                .with_data(json!({"decision": "nothing"})));
            }
            several => {
                return Err(Error::validation(format!(
                    "the prompt found {} objects; name one with candidate",
                    several.len()
                ))
                .with_data(json!({"decision": "several", "chosen": several})));
            }
        },
    };
    let logits = &picked.candidates[position].candidate.logits;
    let prior_size = pick::prior_size(logits);
    let prior = pick::prior(logits, prior_size);
    let guide_values = guide.values();
    context.progress(Some(0.8), "refining the edge");
    let upsampled = guided::upsample(
        guided::Plane {
            width: prior_size.0,
            height: prior_size.1,
            values: &prior,
        },
        guided::Plane {
            width: guide.width,
            height: guide.height,
            values: &guide_values,
        },
        guided::Params::for_sizes(prior_size, (guide.width, guide.height)),
        context.cancel(),
    )?
    .ok_or_else(|| Error::validation("the chosen candidate covers no pixel of the photograph"))?;
    let raster = upsampled.into_raster(guide.width, guide.height);
    let artifact = context.publish_artifact(
        &raster,
        ArtifactMeta {
            kind: crate::mask::ALPHA_KIND.into(),
            width: Some(guide.width),
            height: Some(guide.height),
            colour: None,
        },
    )?;
    let identity = context.identity()?;
    picked_objects.record(PickedObject {
        artifact: artifact.clone(),
        asset: identity.asset.clone(),
        source: identity.source.clone(),
        prefix: identity.prefix.clone(),
        prompt: prompt.clone(),
        raster: ObjectRaster::of(&raster, guide.width, guide.height),
    });
    context.progress(Some(1.0), "published");
    let mut answer = picked.value("none");
    let candidate = answer["candidates"][position].clone();
    answer = json!({
        "artifact": artifact,
        "width": guide.width,
        "height": guide.height,
        "class": OBJECT_CLASS,
        "model": encoder.file.resource,
        "model_version": encoder.file.version,
        "input_fingerprint": identity.source,
        "prefix_hash": identity.prefix,
        "prompt": prompt,
        "decision": picked.decision.name(),
        "candidate": candidate,
        "models": models_value(&[encoder, decoder]),
    });
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetId,
        capabilities::{analysis::AnalysisCache, secrets::MemorySecretStore},
        inference::{Tensor, TensorData},
        jobs::JobControl,
        modules::{
            Stage,
            ai::{analysis::models::DEVELOPER_ANALYSIS, module::AI_MODULE},
        },
    };

    /// The picker over a context holding only the cache and the stage identity: no runtime, so a
    /// pick that gets past the cache fails at its decoder.
    fn context(cache: &Arc<AnalysisCache>) -> (ModuleContext, StageIdentity) {
        let identity = StageIdentity {
            asset: AssetId::new(),
            source: "source".into(),
            prefix: "prefix".into(),
            size: Stage {
                width: 480,
                height: 320,
            },
        };
        let context = ModuleContext::new(
            AI_MODULE,
            Arc::new(MemorySecretStore::new()),
            JobControl::new(),
        )
        .with_analysis(cache.clone())
        .with_identity(identity.clone());
        (context, identity)
    }

    fn analysis() -> Arc<SegmenterAnalysis> {
        let shape = vec![1, 256, 64, 64];
        let values = vec![0.0; 256 * 64 * 64];
        let tensor = Tensor::new("x", shape, TensorData::F32(values)).unwrap();
        Arc::new(SegmenterAnalysis {
            embedding: Embedding::new((480, 320), tensor).unwrap(),
        })
    }

    #[test]
    fn a_pick_answers_only_from_the_analysis_of_the_models_it_runs() {
        let cache = Arc::new(AnalysisCache::default());
        let (context, identity) = context(&cache);
        let [encoder, _] = segmenter(&DEVELOPER_ANALYSIS).unwrap();
        let point = json!({"point": [[0.5, 0.5]]});
        let point = point.as_object().unwrap();
        // An embedding of another version of the encoder is another key: not analysed, naming the
        // input it needs.
        let found = analysis();
        let bytes = found.embedding.bytes();
        cache
            .insert(key(&identity, encoder.file.resource, "0"), found, bytes)
            .unwrap();
        let error = pick_object(&context, &DEVELOPER_ANALYSIS, point).unwrap_err();
        assert_eq!(error.kind.code(), "not-ready", "{error:?}");
        let data = error.data.as_deref().unwrap();
        assert_eq!(data["task"], "task.analyse");
        assert_eq!(data["analysis"]["prefix_hash"], "prefix");
        assert_eq!(data["analysis"]["source"], "source");
        // The current version's embedding is answered: the pick reaches its decoder, which this
        // context lacks, and gives its working copy back to the cache whatever the decode answers.
        let current = key(&identity, encoder.file.resource, encoder.file.version);
        cache.insert(current.clone(), analysis(), bytes).unwrap();
        let error = pick_object(&context, &DEVELOPER_ANALYSIS, point).unwrap_err();
        assert_ne!(error.kind.code(), "not-ready", "{error:?}");
        assert_eq!(cache.status()["held_bytes"], bytes);
        // The entry gone, the copy goes with it.
        cache.forget_module(AI_MODULE);
        assert_eq!(cache.status()["held_bytes"], 0);
        assert!(cache.take_held::<Embedding>(&current).is_none());
    }
}
