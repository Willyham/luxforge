//! Pixel reads off the catalog owner ([performance rule 5]). While the owner serves a call, every
//! read a plan or a query makes of a stage is deferred rather than answered: the owner records the
//! read's identities and its evaluation ([`DeferredRead`]) and hands the read to its tile service
//! ([`crate::tiles`]), which answers it on a thread of its own with its renderer's reads. A query
//! is answered there whole ([`QueryPlan`]), through one [`TileSession`] whose reads share the
//! tiles they draw, so a neutral pick's 25 points draw one; a mutation's read comes back to the
//! owner, which keeps it in the session's [`PixelMemo`] and replays the mutation once with it.
//!
//! [performance rule 5]: ../../../../docs/engineering/performance-rules.md#rules
use super::{
    AssetRecord, EditorService, Evaluation,
    evaluate::source_of,
    masks::{Targeted, recipe_for_target, take_mask_target},
    source::RawSettingsMode,
};
use crate::{
    AssetId, Cancel, DraftId, EntryId, Error, MaskId, PixelInput, PreviewSource, Recipe, Region,
    modules::{QueryRef, Stage, StageContext, StageQuestions, check_parameters},
    tiles::{Answered, ReadAnswer, ReadStage, ReadValues, TileReads, TileSession},
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;

pub(crate) const MAX_PIXEL_MEMO: usize = 32;

/// The most pixels the plans of one call read, each parked once and the call replayed after it:
/// a plan reads one, and a mutation that collapses into the entry before it plans again against
/// that entry's parent, reading another. Past it the call is refused with `resource-limit`.
pub(crate) const MAX_PIXEL_READS: usize = 4;

/// What a read deferred to the tile service answers where its pixel would have been: the call
/// that made it is discarded whatever it answered, and runs again once the pixel is read.
pub(crate) const DEFERRED: &str = "pixel read deferred to the tile service";

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

impl PixelReadKey {
    /// Whether the two keys read the same state of the session — the same asset at the same
    /// entry and revision, the same draft at the same revision and the same source — whatever
    /// prefix of it each reads.
    fn same_state(&self, other: &Self) -> bool {
        self.asset_id == other.asset_id
            && self.entry_id == other.entry_id
            && self.revision == other.revision
            && self.draft == other.draft
            && self.source == other.source
    }
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
    /// Keep `answer`, letting go of every answer read from another state of the session and of an
    /// older answer to the same read; answers of other prefixes of the same state stay, since one
    /// call's plans may read several (a collapse plans against the entry's parent too).
    pub(crate) fn insert(&mut self, answer: PixelAnswer) {
        self.answers.retain(|held| {
            held.key.same_state(&answer.key) && (held.key != answer.key || held.read != answer.read)
        });
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
    /// Read the deferred pixel with `reads`, the tile service's renderer: its code and its linear
    /// value in the stage its layer receives, through one session, so the two are one tile.
    pub(crate) fn evaluate(
        self,
        reads: &dyn TileReads,
        cancel: &Cancel,
    ) -> Result<PixelAnswer, Error> {
        let stage = TileStage::new(&self.evaluation, reads, cancel);
        let read = self.read;
        let rgba = stage.sample_before(read.index, read.x, read.y)?;
        let linear = stage.input_before(read.index, read.x, read.y)?;
        Ok(PixelAnswer {
            key: self.key,
            read,
            rgba,
            linear,
        })
    }
}

impl EditorService {
    /// Whether the current stack of `asset` holds a spatial layer, whose draft's plan refusals
    /// `draft.set` reports as its preview would. Reads layer metadata only.
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

    /// While the catalog owner serves a call, answer a read of the stage layer `read.index`
    /// receives from the call's memo, or defer it to the tile service: record what it reads and
    /// from which entry, revision, draft and source, and answer the internal [`DEFERRED`] error,
    /// which discards the call. `Ok(None)` outside a call, where the host reads the pixel itself.
    pub(super) fn deferred_read(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        source: PreviewSource,
        input_wide: bool,
        read: PixelRead,
    ) -> Result<Option<PixelAnswer>, Error> {
        if !self.pixel_reads.borrow().enabled {
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
        Err(Error::internal(DEFERRED))
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

/// A query that reads pixels, planned on the catalog owner in `O(layers)` and answered whole by
/// the tile service ([`Self::evaluate`]).
pub(crate) struct QueryPlan {
    evaluation: Evaluation,
    query: String,
    parameters: Map<String, Value>,
    mask: Option<MaskId>,
    kind: crate::SourceTag,
}
impl QueryPlan {
    /// Answer the query with `reads`, the tile service's renderer, every read of it through one
    /// session. `mask.sample-input` names the renderer that drew its pixel.
    pub(crate) fn evaluate(self, reads: &dyn TileReads, cancel: &Cancel) -> Result<Value, Error> {
        let questions = TileStage::new(&self.evaluation, reads, cancel);
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
                let outside = || {
                    Error::validation(format!(
                        "outside the stage: ({x}, {y}) is not inside the {}x{} stage the masked \
                         layer receives",
                        stage.width, stage.height
                    ))
                };
                if x >= stage.width || y >= stage.height {
                    return Err(outside());
                }
                let [r, g, b] = questions.input_before(layer, x, y)?.ok_or_else(outside)?;
                let renderer = questions
                    .answered()
                    .ok_or_else(|| Error::internal("the sample input was read by no renderer"))?;
                serde_json::to_value(PixelInput {
                    r,
                    g,
                    b,
                    x,
                    y,
                    width: stage.width,
                    height: stage.height,
                    renderer: (&renderer).into(),
                })
                .map_err(|e| Error::internal(e.to_string()))
            }
            _ => Err(Error::internal("this query cannot be deferred")),
        }
    }
}

/// The stage questions of one call, answered off the catalog owner by the tile service: each
/// pixel of a stage a layer receives is a one-pixel read of that stage through the call's one
/// [`TileSession`], which keeps what it drew for the call, so the points of a patch draw one tile.
/// A stage's size is its prefix's compilation, `O(layers)`, and reads no pixel.
pub(crate) struct TileStage<'a> {
    evaluation: &'a Evaluation,
    session: RefCell<Box<dyn TileSession + 'a>>,
    /// The renderer that drew the latest read.
    answered: RefCell<Option<Answered>>,
}

impl<'a> TileStage<'a> {
    pub(crate) fn new(
        evaluation: &'a Evaluation,
        reads: &'a dyn TileReads,
        cancel: &'a Cancel,
    ) -> Self {
        Self {
            evaluation,
            session: RefCell::new(reads.session(evaluation, cancel)),
            answered: RefCell::new(None),
        }
    }

    /// The renderer that drew the latest read, once one was made.
    pub(crate) fn answered(&self) -> Option<Answered> {
        self.answered.borrow().clone()
    }

    /// One pixel of the stage layer `index` receives, as `values`: `None` outside that stage.
    fn read(
        &self,
        index: usize,
        x: u32,
        y: u32,
        values: ReadValues,
    ) -> Result<Option<ReadAnswer>, Error> {
        let mode = crate::render::MaskInputMode::for_layer(
            self.evaluation.registry(),
            self.evaluation.recipe(),
            index,
        );
        let answer = self.session.borrow_mut().read(
            ReadStage::Before {
                layer: index,
                mode: mode.into(),
            },
            Region {
                x0: x,
                y0: y,
                width: 1,
                height: 1,
            },
            values,
        )?;
        *self.answered.borrow_mut() = Some(answer.answered.clone());
        Ok((!answer.rect.is_empty()).then_some(answer))
    }
}

impl StageQuestions for TileStage<'_> {
    fn stage_before(&self, index: usize) -> Result<Stage, Error> {
        let recipe = self.evaluation.recipe();
        let (width, height) = self.evaluation.source().dimensions();
        Ok(self
            .evaluation
            .registry()
            .compile_layers(
                width,
                height,
                super::prefix(&recipe.layers, index)?,
                &recipe.masks,
                &recipe.strokes,
                &recipe.artifacts,
            )?
            .stage())
    }
    fn sample_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        Ok(self
            .read(index, x, y, ReadValues::Codes)?
            .and_then(|answer| answer.code(x, y)))
    }
    fn input_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        Ok(self
            .read(index, x, y, ReadValues::Linear)?
            .and_then(|answer| answer.linear(x, y))
            .map(|value| value.map(f64::from)))
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
    fn pixel_memo_is_bounded_keeps_other_prefixes_and_evicts_changed_states() {
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
        // Another prefix of the same state is kept beside the answers it holds.
        let mut prefix = key.clone();
        prefix.prefix_hash = [1; 32];
        memo.insert(PixelAnswer {
            key: prefix.clone(),
            read: PixelRead {
                index: 2,
                x: 0,
                y: 0,
            },
            rgba: None,
            linear: None,
        });
        assert_eq!(memo.answers.len(), MAX_PIXEL_MEMO);
        assert!(
            memo.find(
                &prefix,
                PixelRead {
                    index: 2,
                    x: 0,
                    y: 0
                }
            )
            .is_some()
        );
        let mut changed = key.clone();
        changed.revision += 1;
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
