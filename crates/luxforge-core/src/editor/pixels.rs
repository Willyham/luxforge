//! Deferred stage reads. The owner only records identities and plans; the existing point worker
//! evaluates spatial prefixes. A query retains one point evaluation per prefix for its lifetime.
use super::{
    AssetRecord, EditorService, Evaluation,
    evaluate::source_of,
    masks::{Targeted, recipe_for_target, take_mask_target},
    source::RawSettingsMode,
};
use crate::{
    AssetId, Cancel, DraftId, EntryId, Error, MaskId, PixelInput, PreviewSource, Recipe,
    modules::{QueryRef, Stage, StageContext, StageQuestions, check_parameters},
    render::{Compiled, StagePixels, prefix_pixels},
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, collections::HashMap};

pub(crate) const MAX_PIXEL_MEMO: usize = 32;
pub(crate) const MAX_PIXEL_READ_ROUNDS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PixelRead {
    pub index: usize,
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PixelReadKey {
    pub asset_id: AssetId,
    pub entry_id: EntryId,
    pub revision: u64,
    pub draft: Option<(DraftId, u64)>,
    pub prefix_hash: [u8; 32],
    pub input_wide: bool,
    pub input_mode: crate::render::MaskInputMode,
    pub source: crate::ProxyIdentity,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PixelAnswer {
    pub key: PixelReadKey,
    pub read: PixelRead,
    pub rgba: Option<[u8; 4]>,
    pub linear: Option<[f64; 3]>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PixelMemo {
    answers: Vec<PixelAnswer>,
}
impl PixelMemo {
    pub(crate) fn clear(&mut self) {
        self.answers.clear();
    }
    pub(crate) fn insert(&mut self, answer: PixelAnswer) {
        self.answers
            .retain(|held| held.key == answer.key && held.read != answer.read);
        if self.answers.len() == MAX_PIXEL_MEMO {
            self.answers.remove(0);
        }
        self.answers.push(answer);
    }
    pub(crate) fn find(&self, key: &PixelReadKey, read: PixelRead) -> Option<&PixelAnswer> {
        self.answers
            .iter()
            .find(|held| held.key == *key && held.read == read)
    }
    // A successful tick advances its own draft revision without changing the sampled prefix.
    pub(crate) fn advance(&mut self, draft: Option<&crate::Draft>) {
        let Some(draft) = draft else {
            self.clear();
            return;
        };
        for answer in &mut self.answers {
            if answer
                .key
                .draft
                .as_ref()
                .is_some_and(|(id, _)| *id == draft.draft_id)
            {
                answer.key.draft = Some((draft.draft_id.clone(), draft.draft_revision));
            }
        }
    }
}

#[derive(Default, Debug)]
pub(crate) struct PixelReads {
    pub enabled: bool,
    pub draft: Option<(DraftId, u64)>,
    pub memo: PixelMemo,
    pub deferred: Option<DeferredRead>,
}

#[derive(Debug)]
pub(crate) struct DeferredRead {
    pub read: PixelRead,
    pub key: PixelReadKey,
    pub evaluation: Evaluation,
}

impl DeferredRead {
    pub(crate) fn evaluate(self, cancel: &Cancel) -> Result<PixelAnswer, Error> {
        let stage = WorkerStage::new(&self.evaluation, cancel);
        let read = self.read;
        let rgba = stage.sample_before(read.index, read.x, read.y)?;
        let linear = stage.linear_before(read.index, read.x, read.y)?;
        Ok(PixelAnswer {
            key: self.key,
            read,
            rgba,
            linear,
        })
    }
}

impl EditorService {
    pub(crate) fn draft_has_spatial_inputs(&self, asset: &AssetId) -> Result<bool, Error> {
        let head = self.head(asset)?;
        let entry = self.shared_entry(asset, &head.current)?;
        Ok(entry.snapshot.recipe.layers.iter().any(|layer| {
            matches!(
                self.registry.effect_stage(&layer.effect_id),
                Some(crate::EffectStage::Restoration | crate::EffectStage::Spatial)
            )
        }))
    }

    pub(crate) fn begin_pixel_call(&self, draft: Option<&crate::Draft>, memo: PixelMemo) {
        *self.pixel_reads.borrow_mut() = PixelReads {
            enabled: true,
            draft: draft.map(|d| (d.draft_id.clone(), d.draft_revision)),
            memo,
            deferred: None,
        };
    }
    pub(crate) fn take_pixel_read(&self) -> Option<DeferredRead> {
        self.pixel_reads.borrow_mut().deferred.take()
    }
    pub(crate) fn end_pixel_call(&self) {
        self.pixel_reads.borrow_mut().enabled = false;
    }

    pub(super) fn spatial_read(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        source: PreviewSource,
        compiled: &Compiled,
        input_wide: bool,
        read: PixelRead,
    ) -> Result<Option<PixelAnswer>, Error> {
        if !compiled.evaluates_spatial() || !self.pixel_reads.borrow().enabled {
            return Ok(None);
        }
        let head = self.head(&asset.id)?;
        let entry = self.shared_entry(&asset.id, &head.current)?;
        let prefix = super::prefix(&recipe.layers, read.index)?;
        let mut hash = Sha256::new();
        let prefix_recipe = Recipe {
            layers: prefix.to_vec(),
            ..recipe.clone()
        };
        hash.update(
            serde_json::to_vec(&prefix_recipe).map_err(|e| Error::internal(e.to_string()))?,
        );
        let key = PixelReadKey {
            asset_id: asset.id.clone(),
            entry_id: entry.id.clone(),
            revision: head.revision,
            draft: self.pixel_reads.borrow().draft.clone(),
            prefix_hash: hash.finalize().into(),
            input_wide,
            input_mode: crate::render::MaskInputMode::for_layer(&self.registry, recipe, read.index),
            source: source.identity(),
        };
        if let Some(answer) = self.pixel_reads.borrow().memo.find(&key, read) {
            return Ok(Some(answer.clone()));
        }
        let evaluation = Evaluation::new(
            self.registry.clone(),
            self.render_context().clone(),
            source,
            (*entry).clone(),
            recipe.clone(),
            None,
        );
        self.pixel_reads.borrow_mut().deferred = Some(DeferredRead {
            read,
            key,
            evaluation,
        });
        Err(Error::internal("pixel read deferred to the point worker"))
    }

    pub(crate) fn pixel_key_current(
        &self,
        key: &PixelReadKey,
        draft: Option<&crate::Draft>,
    ) -> Result<bool, Error> {
        let head = self.head(&key.asset_id)?;
        if head.current != key.entry_id
            || head.revision != key.revision
            || draft.map(|d| (d.draft_id.clone(), d.draft_revision)) != key.draft
        {
            return Ok(false);
        }
        let entry = self.shared_entry(&key.asset_id, &head.current)?;
        let prepared = self.verified_prepared(&head.asset, &entry.snapshot.recipe)?;
        let source = source_of(prepared, &entry.snapshot.recipe, RawSettingsMode::Strict)?;
        Ok(source.identity() == key.source)
    }

    pub(crate) fn query_plan(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        query_id: &str,
        parameters: Value,
    ) -> Result<QueryPlan, Error> {
        let query = self
            .registry
            .resolve_query(query_id)
            .ok_or_else(|| Error::validation("unknown query"))?;
        let mut parameters = parameters;
        let mask = if matches!(query, QueryRef::Module(..)) {
            take_mask_target(&self.registry, Targeted::Query(query_id), &mut parameters)?
        } else {
            None
        };
        let checked = check_parameters(query.descriptor(), &parameters)?;
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        let bound = self.bound(&entry.snapshot.recipe)?;
        let recipe = match query {
            QueryRef::Module(module, _) if mask.is_none() => recipe_for_target(
                &self.registry,
                &bound,
                module.descriptor().effects.iter().any(|e| e.maskable),
                None,
            )
            .into_owned(),
            _ => bound.into_owned(),
        };
        let prepared = self.verified_prepared(&state.asset, &recipe)?;
        let source = source_of(prepared, &recipe, RawSettingsMode::Strict)?;
        Ok(QueryPlan {
            evaluation: Evaluation::new(
                self.registry.clone(),
                self.render_context().clone(),
                source,
                entry,
                recipe,
                None,
            ),
            query: query_id.to_owned(),
            parameters: checked,
            mask,
            kind: state.asset.source.tag(),
        })
    }
}

pub(crate) struct QueryPlan {
    evaluation: Evaluation,
    query: String,
    parameters: Map<String, Value>,
    mask: Option<MaskId>,
    kind: crate::SourceTag,
}
impl QueryPlan {
    pub(crate) fn evaluate(self, cancel: &Cancel) -> Result<Value, Error> {
        let questions = WorkerStage::new(&self.evaluation, cancel);
        let recipe = self.evaluation.recipe();
        let registry = self.evaluation.registry();
        match registry
            .resolve_query(&self.query)
            .ok_or_else(|| Error::validation("unknown query"))?
        {
            QueryRef::Module(module, _) => module.query(
                &self.query,
                &self.parameters,
                &StageContext {
                    layers: &recipe.layers,
                    registry,
                    target: self.mask.as_ref(),
                    kind: self.kind,
                    masks: &recipe.masks,
                    questions: &questions,
                },
            ),
            QueryRef::Host(_) if self.query == crate::mask::commands::SAMPLE_INPUT => {
                let mask: MaskId = serde_json::from_value(self.parameters["mask"].clone())
                    .map_err(|e| Error::validation(e.to_string()))?;
                let layer = crate::mask::commands::input_layer_index(recipe, &mask)?;
                let x = self.parameters["x"].as_u64().unwrap() as u32;
                let y = self.parameters["y"].as_u64().unwrap() as u32;
                let stage = questions.stage_before(layer)?;
                let [r, g, b] = questions
                    .linear_before(layer, x, y)?
                    .ok_or_else(|| Error::validation("outside the stage"))?;
                serde_json::to_value(PixelInput {
                    r,
                    g,
                    b,
                    x,
                    y,
                    width: stage.width,
                    height: stage.height,
                })
                .map_err(|e| Error::internal(e.to_string()))
            }
            _ => Err(Error::internal("this query cannot be deferred")),
        }
    }
}

type PointPrefix<'a> = (Stage, Box<dyn StagePixels + 'a>);

struct WorkerStage<'a> {
    evaluation: &'a Evaluation,
    cancel: &'a Cancel,
    prefixes: RefCell<HashMap<usize, PointPrefix<'a>>>,
}
impl<'a> WorkerStage<'a> {
    fn new(evaluation: &'a Evaluation, cancel: &'a Cancel) -> Self {
        Self {
            evaluation,
            cancel,
            prefixes: RefCell::new(HashMap::new()),
        }
    }
    fn with_prefix<T>(
        &self,
        index: usize,
        answer: impl FnOnce(Stage, &dyn StagePixels) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.cancel.check()?;
        if !self.prefixes.borrow().contains_key(&index) {
            let recipe = self.evaluation.recipe();
            let (width, height) = self.evaluation.source().dimensions();
            let compiled = self.evaluation.registry().compile_layers(
                width,
                height,
                super::prefix(&recipe.layers, index)?,
                &recipe.masks,
                &recipe.strokes,
                &recipe.artifacts,
            )?;
            let stage = compiled.stage();
            let full = self.evaluation.registry().compile_layers(
                width,
                height,
                &recipe.layers,
                &recipe.masks,
                &recipe.strokes,
                &recipe.artifacts,
            )?;
            let wide = full.prefix_spatial_input_wide(&compiled);
            let pixels = prefix_pixels(
                self.evaluation.source().input(),
                compiled,
                self.evaluation.context(),
                self.cancel,
                wide,
                crate::render::MaskInputMode::for_layer(self.evaluation.registry(), recipe, index),
            )?;
            self.prefixes.borrow_mut().insert(index, (stage, pixels));
        }
        let held = self.prefixes.borrow();
        let (stage, pixels) = &held[&index];
        answer(*stage, &**pixels)
    }
    fn linear_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        self.with_prefix(index, |_, pixels| pixels.linear(x, y))
    }
}
impl StageQuestions for WorkerStage<'_> {
    fn stage_before(&self, index: usize) -> Result<Stage, Error> {
        self.with_prefix(index, |stage, _| Ok(stage))
    }
    fn sample_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        self.with_prefix(index, |_, pixels| pixels.rgba(x, y))
    }
    fn input_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        self.linear_before(index, x, y)
    }
    fn sensor_neutral(&self, _: u32, _: u32) -> Result<[f32; 3], Error> {
        Err(Error::internal("sensor neutral reads never defer"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> PixelReadKey {
        PixelReadKey {
            asset_id: AssetId::new(),
            entry_id: EntryId::new(),
            revision: 1,
            draft: None,
            prefix_hash: [0; 32],
            input_wide: true,
            input_mode: crate::render::MaskInputMode::Boundary,
            source: crate::ProxyIdentity::Jpeg {
                fingerprint: "memo".into(),
                width: 100,
                height: 100,
                orientation: 1,
            },
        }
    }
    #[test]
    fn pixel_memo_is_bounded_and_evicts_changed_keys() {
        let key = key();
        let mut memo = PixelMemo::default();
        for x in 0..33 {
            memo.insert(PixelAnswer {
                key: key.clone(),
                read: PixelRead { index: 1, x, y: 0 },
                rgba: None,
                linear: None,
            });
        }
        assert_eq!(memo.answers.len(), MAX_PIXEL_MEMO);
        assert!(
            memo.find(
                &key,
                PixelRead {
                    index: 1,
                    x: 0,
                    y: 0
                }
            )
            .is_none()
        );
        let mut changed = key.clone();
        changed.input_wide = false;
        memo.insert(PixelAnswer {
            key: changed.clone(),
            read: PixelRead {
                index: 1,
                x: 32,
                y: 0,
            },
            rgba: None,
            linear: None,
        });
        assert_eq!(memo.answers.len(), 1);
        assert!(
            memo.find(
                &key,
                PixelRead {
                    index: 1,
                    x: 32,
                    y: 0
                }
            )
            .is_none()
        );
        assert!(
            memo.find(
                &changed,
                PixelRead {
                    index: 1,
                    x: 32,
                    y: 0
                }
            )
            .is_some()
        );
    }
}
