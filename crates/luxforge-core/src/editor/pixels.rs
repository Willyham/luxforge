//! Pixel reads off the catalog owner ([performance rule 5]). While the owner serves a call, every
//! read a plan or a query makes of a stage is deferred rather than answered: the owner records the
//! read's identities and its evaluation ([`DeferredRead`]) and hands the read to its tile service
//! ([`crate::tiles`]), which answers it on a thread of its own with its renderer's reads. A query
//! is answered there whole ([`QueryPlan`]), through one [`TileSession`] whose reads share the
//! tiles they draw, so a neutral pick's 25 points draw one; a mutation's read comes back to the
//! owner, which keeps it in the session's [`PixelMemo`] and replays the mutation once with it.
//!
//! The replay is one protocol ([`Replay`]) with two drivers: the owner's parked calls, which keep
//! their answers in the calling session's memo, and a library batch's photographs, each of which
//! keeps its own memo on the lane's worker and never touches the caller's. Both serve every pass
//! through [`EditorService::begin_pixel_call`] and [`EditorService::finish_pixel_call`], submit
//! every read as [`DeferredRead::tile_call`], check on the owner that what a read was read from is
//! still current ([`EditorService::pixel_key_current`]) before the pass that uses it, and stop at
//! [`MAX_PIXEL_READS`].
//!
//! [performance rule 5]: ../../../../docs/engineering/performance-rules.md#rules
use super::{
    AssetRecord, EditorService, Evaluation,
    evaluate::{sensor_of, source_of},
    masks::{Targeted, recipe_for_target, take_mask_target},
    source::RawSettingsMode,
};
use crate::{
    AssetId, Cancel, DraftId, EntryId, Error, MaskId, PixelInput, PreviewSource, Recipe, Region,
    modules::{QueryRef, Stage, StageContext, StageQuestions, check_parameters},
    tiles::{Answered, ReadAnswer, ReadStage, ReadValues, Step, TileReads, TileSession},
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

pub(crate) const MAX_PIXEL_MEMO: usize = 32;

/// The most pixels the plans of one call read, each parked once and the call replayed after it:
/// a plan reads one, and a mutation that collapses into the entry before it plans again against
/// that entry's parent, reading another. Past it the call is refused with `resource-limit`.
pub(crate) const MAX_PIXEL_READS: usize = 4;

/// What a read deferred to the tile service answers where its pixel would have been: the call
/// that made it is discarded whatever it answered, and runs again once the pixel is read.
pub(crate) const DEFERRED: &str = "pixel read deferred to the tile service";

/// Which pass of a call that reads pixels is served: its first, a replay once its `n`th parked
/// read was answered with the pixel in its memo, or a replay without it because what the pixel
/// was read from changed meanwhile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Replay {
    First,
    Read(usize),
    Stale,
}

impl Replay {
    /// A pass of this replay deferred another read: the number that read is among the call's,
    /// which the caller parks and has read; or the call's answer instead — `resource-limit` once
    /// its plans have read [`MAX_PIXEL_READS`] pixels, and `conflict` when what its last read was
    /// read from changed and the replay needs a pixel again.
    pub(crate) fn park(self) -> Result<usize, Error> {
        match self {
            Self::First => Ok(1),
            // The read it parked for is in its memo, so its plans ask for another, such as a
            // collapse planning against the entry's parent: parked again, a bounded number of
            // times.
            Self::Read(reads) if reads < MAX_PIXEL_READS => Ok(reads + 1),
            Self::Read(_) => Err(Error::resource_limit(format!(
                "the plan read more than {MAX_PIXEL_READS} pixels"
            ))),
            Self::Stale => Err(Error::conflict(
                "the stack changed while its pixels were read; retry",
            )),
        }
    }

    /// The pass after the call's `reads`th read came back with `answer`: kept in `memo` and
    /// replayed with it when what it was read from is still `current`, which the owner decides
    /// ([`EditorService::pixel_key_current`]) just before the replay; otherwise `memo` is cleared
    /// and the call replayed without it, so a replay that needs the pixel again answers
    /// `conflict` ([`Self::park`]) and one that no longer does answers as it would have.
    pub(crate) fn answered(
        reads: usize,
        answer: PixelAnswer,
        current: bool,
        memo: &mut PixelMemo,
    ) -> Self {
        if current {
            memo.insert(answer);
            Self::Read(reads)
        } else {
            memo.clear();
            Self::Stale
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PixelRead {
    Point {
        index: usize,
        x: u32,
        y: u32,
    },
    Query {
        id: String,
        parameters: Map<String, Value>,
    },
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
    pub value: PixelValue,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PixelValue {
    Point {
        rgba: Option<[u8; 4]>,
        linear: Option<[f64; 3]>,
    },
    Query(Result<Value, QueryRefusal>),
}

/// Only validation refusals can be replayed as module answers. Cancellation and failures abort
/// the parked call; a composite may skip a documented validation refusal without losing patches.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QueryRefusal {
    detail: String,
    data: Option<Box<Value>>,
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
    pub(crate) fn find(&self, key: &PixelReadKey, read: &PixelRead) -> Option<&PixelAnswer> {
        self.answers
            .iter()
            .find(|held| held.key == *key && held.read == *read)
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
    /// The answer of each declared analysis query ([`crate::AnalysisAction`]) an action's plans
    /// were given, by query, which [`EditorService::run_action`] reports beside its result.
    pub analysed: BTreeMap<String, Value>,
}

#[derive(Debug)]
pub(crate) struct DeferredRead {
    pub read: PixelRead,
    pub key: PixelReadKey,
    pub evaluation: Evaluation,
}

impl DeferredRead {
    /// The tile call that reads this pixel for `client` under `cancel` and hands the answer to
    /// `deliver`, which returns it to the owner: every parked read, the owner's and a batch's, is
    /// submitted as one of these. A query that reads a sample grid reads it on the tile service
    /// and is finished on the analysis worker ([`QueryPlan::evaluate_steps`]), under the same
    /// `cancel` and answering the same `deliver`.
    pub(crate) fn tile_call(
        self,
        client: crate::ClientId,
        cancel: Cancel,
        deliver: impl FnOnce(Result<PixelAnswer, Error>) + Send + 'static,
    ) -> crate::tiles::TileCall {
        crate::tiles::TileCall::pixels(
            client,
            cancel,
            move |reads, cancel| self.evaluate(reads, cancel),
            deliver,
        )
    }

    /// Read the deferred pixel with `reads`, the tile service's renderer: its code and its linear
    /// value in the stage its layer receives, through one session, so the two are one tile; or
    /// the deferred query, whose analysis, if it reads a grid, the analysis worker finishes.
    pub(crate) fn evaluate(
        self,
        reads: &dyn TileReads,
        cancel: &Cancel,
    ) -> Result<Step<PixelAnswer>, Error> {
        let Self {
            read,
            key,
            evaluation,
        } = self;
        match read {
            PixelRead::Point { index, x, y } => {
                let stage = TileStage::new(&evaluation, reads, cancel);
                let value = PixelValue::Point {
                    rgba: stage.sample_before(index, x, y)?,
                    linear: stage.input_before(index, x, y)?,
                };
                Ok(Step::Done(PixelAnswer {
                    key,
                    read: PixelRead::Point { index, x, y },
                    value,
                }))
            }
            PixelRead::Query { id, parameters } => {
                let kind = match evaluation.source() {
                    PreviewSource::Jpeg(_) => crate::SourceTag::Jpeg,
                    PreviewSource::Raw { .. } => crate::SourceTag::Raw,
                };
                let read = PixelRead::Query {
                    id: id.clone(),
                    parameters: parameters.clone(),
                };
                let plan = QueryPlan {
                    evaluation,
                    query: id,
                    parameters,
                    mask: None,
                    kind,
                };
                Step::map_result(plan.evaluate_steps(reads, cancel), move |result| {
                    let value = PixelValue::Query(match result {
                        Ok(value) => Ok(value),
                        Err(error) if error.kind == crate::ErrorKind::Validation => {
                            Err(QueryRefusal {
                                detail: error.detail,
                                data: error.data,
                            })
                        }
                        Err(error) => return Err(error),
                    });
                    Ok(PixelAnswer { key, read, value })
                })
            }
        }
    }
}

impl EditorService {
    pub(super) fn deferred_query(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        (source, sensor): (PreviewSource, Option<Arc<dyn crate::tiles::SensorClip>>),
        id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<Value, Error> {
        let query = self
            .registry
            .resolve_query(id)
            .ok_or_else(|| Error::validation("unknown deferred query"))?;
        let parameters = check_parameters(query.descriptor(), &Value::Object(parameters.clone()))?;
        let read = PixelRead::Query {
            id: id.into(),
            parameters: parameters.clone(),
        };
        if let Some(answer) =
            self.deferred_read(asset, recipe, source.clone(), sensor.clone(), false, read)?
        {
            return match answer.value {
                PixelValue::Query(Ok(value)) => {
                    self.analysed(id, &value);
                    Ok(value)
                }
                PixelValue::Query(Err(refusal)) => {
                    let mut error = Error::validation(refusal.detail);
                    error.data = refusal.data;
                    Err(error)
                }
                _ => Err(Error::internal("query read returned a point")),
            };
        }
        // Direct EditorService callers run on their own thread. The owner always takes the
        // deferred branch above, including presets, and can never execute this fallback.
        super::plan::refuse_on_owner()?;
        let head = self.head(&asset.id)?;
        let entry = self.entry(&asset.id, &head.current)?;
        let value = QueryPlan {
            evaluation: Evaluation::new(
                self.registry.clone(),
                self.render_context().clone(),
                source,
                entry,
                recipe.clone(),
                None,
            )
            .with_sensor(sensor),
            query: id.into(),
            parameters,
            mask: None,
            kind: asset.source.tag(),
        }
        .evaluate(&crate::tiles::ReferenceReads, &Cancel::never())?;
        self.analysed(id, &value);
        Ok(value)
    }

    /// Keep a declared analysis query's answer for the action being planned to report.
    fn analysed(&self, id: &str, value: &Value) {
        if self.registry.analysis_query(id) {
            self.pixel_reads
                .borrow_mut()
                .analysed
                .insert(id.to_owned(), value.clone());
        }
    }

    /// The declared analysis answers the plans since the last take were given, by query.
    pub(crate) fn take_analysed(&self) -> BTreeMap<String, Value> {
        std::mem::take(&mut self.pixel_reads.borrow_mut().analysed)
    }

    /// Whether `draft`'s plan reads a pixel when it is planned: a stroke drafted with a colour
    /// limit, whose seed it reads. `draft.set` plans such a draft, so its read is parked once,
    /// off the owner, before the draft is accepted, and every preview of it finds the pixel in the
    /// session's memo. Reads the draft's fields alone.
    pub(crate) fn draft_reads_pixels(&self, draft: &crate::Draft) -> bool {
        crate::mask::commands::asks_colour_limit(&draft.fields)
    }

    /// Whether the current stack of `asset` holds a spatial layer, whose drafts `draft.set` plans
    /// so their refusals are its own, as a preview's would be. Reads layer metadata only.
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

    /// Begin one pass of a call on the owner: every read its plans make of a stage is answered
    /// from `memo` or deferred ([`Self::deferred_read`]), keyed with `draft`'s identity.
    pub(crate) fn begin_pixel_call(&self, draft: Option<&crate::Draft>, memo: PixelMemo) {
        *self.pixel_reads.borrow_mut() = PixelReads {
            enabled: true,
            draft: draft.map(|d| (d.draft_id.clone(), d.draft_revision)),
            memo,
            deferred: None,
            analysed: BTreeMap::new(),
        };
    }

    /// Take the read a step of the pass deferred, leaving the pass open: a query that answers it
    /// whole on the tile service instead.
    pub(crate) fn take_pixel_read(&self) -> Option<DeferredRead> {
        self.pixel_reads.borrow_mut().deferred.take()
    }

    /// End the pass [`Self::begin_pixel_call`] began, answering the read it deferred, if any, for
    /// its caller to park and replay the call after ([`Replay`]).
    pub(crate) fn finish_pixel_call(&self) -> Option<DeferredRead> {
        let mut reads = self.pixel_reads.borrow_mut();
        reads.enabled = false;
        reads.deferred.take()
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
        sensor: Option<Arc<dyn crate::tiles::SensorClip>>,
        input_wide: bool,
        read: PixelRead,
    ) -> Result<Option<PixelAnswer>, Error> {
        if !self.pixel_reads.borrow().enabled {
            return Ok(None);
        }
        let head = self.head(&asset.id)?;
        let entry = self.shared_entry(&asset.id, &head.current)?;
        let index = match &read {
            PixelRead::Point { index, .. } => *index,
            PixelRead::Query { .. } => recipe.layers.len(),
        };
        let prefix = super::prefix(&recipe.layers, index)?;
        let mut hash = Sha256::new();
        let prefix_recipe = Recipe {
            layers: prefix.to_vec(),
            ..recipe.clone()
        };
        hash.update(crate::tiles::grid::content_hash(&prefix_recipe)?);
        let key = PixelReadKey {
            asset_id: asset.id.clone(),
            entry_id: entry.id.clone(),
            revision: head.revision,
            draft: self.pixel_reads.borrow().draft.clone(),
            prefix_hash: hash.finalize().into(),
            input_wide,
            input_mode: crate::render::MaskInputMode::for_layer(&self.registry, recipe, index),
            source: source.identity(),
        };
        if let Some(answer) = self.pixel_reads.borrow().memo.find(&key, &read) {
            return Ok(Some(answer.clone()));
        }
        let evaluation = Evaluation::new(
            self.registry.clone(),
            self.render_context().clone(),
            source,
            (*entry).clone(),
            recipe.clone(),
            None,
        )
        .with_sensor(sensor);
        self.pixel_reads.borrow_mut().deferred = Some(DeferredRead {
            read,
            key,
            evaluation,
        });
        Err(Error::internal(DEFERRED))
    }

    /// Whether what `key` was read from is still current: the asset's entry and revision, `draft`
    /// (the calling session's, `None` for a batch) at the revision read, and the verified source.
    pub(crate) fn pixel_key_current(
        &self,
        key: &PixelReadKey,
        draft: Option<&crate::Draft>,
    ) -> Result<bool, Error> {
        if !self.pixel_head_current(key, draft)? {
            return Ok(false);
        }
        let head = self.head(&key.asset_id)?;
        self.holds_verified_source(&head.asset, &key.source)
    }

    /// [`Self::pixel_key_current`] without the source check, which may read the file's metadata:
    /// what the owner asks of every parked read after each message, so a read whose stack or draft
    /// has moved on is cancelled rather than finished.
    pub(crate) fn pixel_head_current(
        &self,
        key: &PixelReadKey,
        draft: Option<&crate::Draft>,
    ) -> Result<bool, Error> {
        let head = self.head(&key.asset_id)?;
        Ok(head.current == key.entry_id
            && head.revision == key.revision
            && draft.map(|d| (d.draft_id.clone(), d.draft_revision)) == key.draft)
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
        if let QueryRef::Module(module, _) = query {
            super::plan::check_askable(
                &self.registry,
                module,
                state.asset.source.tag(),
                mask.as_ref(),
                None,
            )?;
        }
        super::source::validate_source_recipe(
            &self.registry,
            &state.asset,
            &entry.snapshot.recipe,
        )?;
        super::masks::resolve_mask_target(&entry.snapshot.recipe, mask.as_ref())?;
        let bound = self.bound(&entry.snapshot.recipe)?;
        let recipe = match query {
            QueryRef::Module(module, _)
                if mask.is_none() && !self.registry.analysis_query(query_id) =>
            {
                recipe_for_target(
                    &self.registry,
                    &bound,
                    module.descriptor().effects.iter().any(|e| e.maskable),
                    None,
                )
                .into_owned()
            }
            _ => bound.into_owned(),
        };
        let (source, sensor) = self.needing(
            super::source::Evaluated::exactly(&state.asset, &entry.id, &recipe),
            self.verified_prepared(&state.asset, &recipe)
                .and_then(|prepared| {
                    let sensor = sensor_of(&prepared);
                    source_of(prepared, &recipe, RawSettingsMode::Strict)
                        .map(|source| (source, sensor))
                }),
        )?;
        Ok(QueryPlan {
            evaluation: Evaluation::new(
                self.registry.clone(),
                self.render_context().clone(),
                source,
                entry,
                recipe,
                None,
            )
            .with_sensor(sensor),
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
    /// Whether this is a declared analysis query ([`crate::AnalysisAction`]), such as
    /// `auto-tone`, which its caller can supersede.
    pub(crate) fn analysis(&self) -> bool {
        self.evaluation.registry().analysis_query(&self.query)
    }

    /// The photograph the query reads.
    pub(crate) fn asset(&self) -> &crate::AssetId {
        &self.evaluation.entry().asset_id
    }

    /// Answer the query with `reads`, the tile service's renderer, every read of it through one
    /// session, here and now. Every answer that read pixels names the renderer that drew them.
    pub(crate) fn evaluate(self, reads: &dyn TileReads, cancel: &Cancel) -> Result<Value, Error> {
        let questions = TileStage::new(&self.evaluation, reads, cancel);
        self.answer(&questions, &|| questions.answered())
    }

    /// Answer the query on the tile service's thread with `reads`, or, when it reads a sample
    /// grid, read only the grid here and hand the rest of the query — Auto tone's solve — to the
    /// analysis worker ([`crate::tiles`]), so the service's next call does not wait for it. The
    /// query is asked again there over the grid read here: planning it again is `O(layers)`, and
    /// it reads no other pixel after its grid.
    pub(crate) fn evaluate_steps(
        self,
        reads: &dyn TileReads,
        cancel: &Cancel,
    ) -> Result<Step<Value>, Error> {
        let held = {
            let questions = TileStage::new(&self.evaluation, reads, cancel).holding_grids();
            let answer = self.answer(&questions, &|| questions.answered());
            match questions.held.take() {
                Some(held) => held,
                None => return answer.map(Step::Done),
            }
        };
        let context = self.evaluation.context().clone();
        let answered = held.1.answered.clone();
        Ok(Step::then(context, move |_cancel| {
            let questions = HeldGrid {
                evaluation: &self.evaluation,
                held: RefCell::new(Some(held)),
            };
            self.answer(&questions, &|| Some(answered.clone()))
        }))
    }

    /// The query's answer to `questions`; `answered` names the renderer of its latest read.
    fn answer(
        &self,
        questions: &dyn StageQuestions,
        answered: &dyn Fn() -> Option<Answered>,
    ) -> Result<Value, Error> {
        let recipe = self.evaluation.recipe();
        let registry = self.evaluation.registry();
        match registry
            .resolve_query(&self.query)
            .ok_or_else(|| Error::validation("unknown query"))?
        {
            QueryRef::Module(module, _) => {
                let mut answer = module.query(
                    &self.query,
                    &self.parameters,
                    &StageContext {
                        layers: &recipe.layers,
                        registry,
                        target: self.mask.as_ref(),
                        kind: self.kind,
                        masks: &recipe.masks,
                        questions,
                    },
                )?;
                // A module query that read pixels names the renderer that drew them, as
                // `mask.sample-input` does: the neutral picker's patch, Auto tone's grid.
                if let (Some(answered), Some(object)) = (answered(), answer.as_object_mut()) {
                    let renderer = serde_json::to_value(crate::Renderer::from(&answered))
                        .map_err(|e| Error::internal(e.to_string()))?;
                    object.insert("renderer".into(), renderer);
                }
                Ok(answer)
            }
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
                let renderer = answered()
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
    reads: &'a dyn TileReads,
    cancel: &'a Cancel,
    session: RefCell<Box<dyn TileSession + 'a>>,
    /// The renderer that drew the latest read.
    answered: RefCell<Option<Answered>>,
    /// Whether a grid read is held here for the analysis worker rather than answered
    /// ([`Self::holding_grids`]), and the grid held.
    holding: bool,
    held: RefCell<Option<(usize, crate::tiles::GridRead)>>,
}

/// What a query's first pass answers in place of the grid it reads when the grid is held for the
/// analysis worker ([`QueryPlan::evaluate_steps`]): the pass's answer is discarded.
const GRID_HELD: &str = "sample grid read; its analysis continues on the analysis worker";

impl<'a> TileStage<'a> {
    pub(crate) fn new(
        evaluation: &'a Evaluation,
        reads: &'a dyn TileReads,
        cancel: &'a Cancel,
    ) -> Self {
        Self {
            evaluation,
            reads,
            cancel,
            session: RefCell::new(reads.session(evaluation, cancel)),
            answered: RefCell::new(None),
            holding: false,
            held: RefCell::new(None),
        }
    }

    /// These questions, keeping the first sample grid a query reads rather than answering it, so
    /// the query stops there and is asked again over the grid on the analysis worker.
    fn holding_grids(mut self) -> Self {
        self.holding = true;
        self
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
    fn grid_before(&self, index: usize) -> Result<crate::tiles::GridRead, Error> {
        let read = crate::tiles::grid::read(self.evaluation, index, self.reads, self.cancel)?;
        *self.answered.borrow_mut() = Some(read.answered.clone());
        if self.holding {
            *self.held.borrow_mut() = Some((index, read));
            return Err(Error::internal(GRID_HELD));
        }
        Ok(read)
    }

    fn stage_before(&self, index: usize) -> Result<Stage, Error> {
        stage_before(self.evaluation, index)
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

/// The stage the layer at `index` receives: its prefix's compilation, `O(layers)`.
fn stage_before(evaluation: &Evaluation, index: usize) -> Result<Stage, Error> {
    let recipe = evaluation.recipe();
    let (width, height) = evaluation.source().dimensions();
    Ok(evaluation
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

/// The questions a query is asked again on the analysis worker ([`QueryPlan::evaluate_steps`]):
/// the sample grid the tile service read for it, once, and stages, which read no pixel. It reads
/// nothing more.
struct HeldGrid<'a> {
    evaluation: &'a Evaluation,
    held: RefCell<Option<(usize, crate::tiles::GridRead)>>,
}

impl StageQuestions for HeldGrid<'_> {
    fn grid_before(&self, index: usize) -> Result<crate::tiles::GridRead, Error> {
        match self.held.take() {
            Some((read, grid)) if read == index => Ok(grid),
            _ => Err(Error::internal(
                "an analysis asked again for a sample grid the tile service did not read for it",
            )),
        }
    }
    fn stage_before(&self, index: usize) -> Result<Stage, Error> {
        stage_before(self.evaluation, index)
    }
    fn sample_before(&self, _: usize, _: u32, _: u32) -> Result<Option<[u8; 4]>, Error> {
        Err(Error::internal(
            "an analysis reads no pixel after its sample grid",
        ))
    }
    fn input_before(&self, _: usize, _: u32, _: u32) -> Result<Option<[f64; 3]>, Error> {
        Err(Error::internal(
            "an analysis reads no pixel after its sample grid",
        ))
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
                read: PixelRead::Point { index: 1, x, y: 0 },
                value: PixelValue::Point {
                    rgba: None,
                    linear: None,
                },
            });
        }
        assert_eq!(memo.answers.len(), MAX_PIXEL_MEMO);
        assert!(
            memo.find(
                &key,
                &PixelRead::Point {
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
            read: PixelRead::Point {
                index: 2,
                x: 0,
                y: 0,
            },
            value: PixelValue::Point {
                rgba: None,
                linear: None,
            },
        });
        assert_eq!(memo.answers.len(), MAX_PIXEL_MEMO);
        assert!(
            memo.find(
                &prefix,
                &PixelRead::Point {
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
            read: PixelRead::Point {
                index: 1,
                x: 32,
                y: 0,
            },
            value: PixelValue::Point {
                rgba: None,
                linear: None,
            },
        });
        assert_eq!(memo.answers.len(), 1);
        assert!(
            memo.find(
                &key,
                &PixelRead::Point {
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
                &PixelRead::Point {
                    index: 1,
                    x: 32,
                    y: 0
                }
            )
            .is_some()
        );
    }
}
