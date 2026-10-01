use super::{
    AnalysisPlan, AnalysisSelection, AssetRecord, DraftStamp, EditorService, ExportPlan,
    ExportTarget, PixelSample, SamplePlan,
    source::{Evaluated, RawSettingsMode, raw_settings, validate_source_recipe},
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    AssetId, Cancel, ContentPoint, Draft, EffectStage, EntryId, Error, HistoryEntry,
    MappingDescriptor, ModuleRegistry, PreviewJob, PreviewSource, ProxyBounds, Raster, Recipe,
    Render, RenderContext, RenderOptions,
    analysis::AnalysisIdentity,
    export::CaptureMetadata,
    render::{Compiled, locate, transform_of},
    source::PreparedSource,
};
use std::{borrow::Cow, sync::Arc};

/// One stack bound for evaluation and compiled once: the entry it evaluates, the stack itself —
/// that entry's own or an open draft's effective recipe — bound with the verified bytes of every
/// artifact it references, the providers and the render context it is evaluated with, the draft
/// revision it was planned from, its compilation against the asset's content stage, and, once the
/// original is prepared, the source buffer it reads.
///
/// [`EditorService::evaluation`] builds it, on the catalog owner, and every evaluation the service
/// plans wraps it: a preview job, an analysis plan and job, a sample plan, a point plan, an export
/// plan and target. None of them derives the entry, the stack, the source or the compilation
/// again, so a preview's worker renders the compilation its owner made, and the identity an
/// analysis is filed under reads that compilation's output stage. A stack the host cannot evaluate
/// is kept with the reason it has no compilation, which each wrapper decides how to answer.
///
/// `S` is the source it reads: a [`PreviewSource`], or `()` for a question the catalog alone
/// answers — an export's output stage, a point's content pixel — which prepares nothing.
///
/// Cheap to clone: the bound stack and its compilation are shared, and the source shares its
/// pixels. It is immutable; a caller that wants another stack builds another evaluation.
#[derive(Clone)]
pub struct Evaluation<S = PreviewSource> {
    bound: Arc<Bound>,
    source: S,
}

struct Bound {
    entry: Arc<HistoryEntry>,
    /// The stack evaluated, when it is not the entry's own as stored: an open draft's effective
    /// recipe, or the entry's own with the artifacts it references bound in.
    recipe: Option<Recipe>,
    registry: Arc<ModuleRegistry>,
    context: RenderContext,
    /// The fingerprint of the source the stack is evaluated on: the asset's, which its prepared
    /// source holds.
    fingerprint: String,
    draft: Option<DraftStamp>,
    /// The stack compiled against the content stage at the exact phase, or why it cannot be.
    compiled: Result<Compiled, Error>,
}

impl<S> Evaluation<S> {
    /// The entry this evaluation shows: the one evaluated, or the current one a draft was planned
    /// over. Its identity and snapshot correlate what is evaluated with history; the stack is
    /// [`Self::recipe`], which differs from the entry's own while a draft is open.
    pub fn entry(&self) -> &HistoryEntry {
        &self.bound.entry
    }

    /// The stack evaluated, bound with the verified bytes of every artifact it references, so a
    /// worker evaluates it whatever the owner's artifact cache evicts meanwhile.
    pub fn recipe(&self) -> &Recipe {
        self.bound
            .recipe
            .as_ref()
            .unwrap_or(&self.bound.entry.snapshot.recipe)
    }

    /// The providers the stack is evaluated with; shared, never rebuilt per evaluation.
    pub fn registry(&self) -> &Arc<ModuleRegistry> {
        &self.bound.registry
    }

    /// The budgets and the estimate store every evaluation of the planning service shares.
    pub fn context(&self) -> &RenderContext {
        &self.bound.context
    }

    /// The draft, at the revision it held, whose effective recipe this evaluates; `None` for a
    /// saved entry's own stack.
    pub fn draft(&self) -> Option<&DraftStamp> {
        self.bound.draft.as_ref()
    }

    /// The revision of [`Self::draft`], for correlating a frame with the settings that produced it.
    pub fn draft_revision(&self) -> Option<u64> {
        self.bound.draft.as_ref().map(|draft| draft.draft_revision)
    }

    /// The output stage of the compiled stack, or `None` when the host cannot evaluate it.
    pub(crate) fn stage(&self) -> Option<(u32, u32)> {
        self.bound.compiled.as_ref().ok().map(|compiled| {
            let stage = compiled.stage();
            (stage.width, stage.height)
        })
    }

    /// The identity of the analysis of this evaluation: the asset, the source fingerprint, the
    /// entry and snapshot, the hash of the stack, the draft and the output stage. `O(recipe)`: it
    /// hashes the stack and reads the output stage of its one compilation, so a preview's frame
    /// and an analysis of the same stack are filed under the same identity.
    pub fn identity(&self) -> Result<AnalysisIdentity, Error> {
        let entry = self.entry();
        AnalysisIdentity::of(
            &entry.asset_id,
            &self.bound.fingerprint,
            entry,
            self.recipe(),
            self.bound.draft.clone(),
            self.stage(),
        )
    }

    /// The stack's compilation, or the reason the host cannot evaluate it.
    pub(crate) fn compiled(&self) -> Result<&Compiled, Error> {
        self.bound.compiled.as_ref().map_err(Clone::clone)
    }

    /// Whether Fit needs an exact processed settlement, read from compilation metadata only.
    pub fn settles_from_exact(&self) -> bool {
        self.bound
            .compiled
            .as_ref()
            .is_ok_and(Compiled::settles_from_exact)
    }

    /// This evaluation reading `source`.
    fn reading<T>(self, source: T) -> Evaluation<T> {
        Evaluation {
            bound: self.bound,
            source,
        }
    }
}

impl Evaluation {
    /// Bind `recipe`, shown as `entry`, to `source` and compile it once against the source's
    /// dimensions: the value [`EditorService::evaluation`] builds from a catalog, built from parts
    /// a caller already holds. `recipe` must already carry the bytes of the artifacts it references.
    pub fn new(
        registry: Arc<ModuleRegistry>,
        context: RenderContext,
        source: PreviewSource,
        entry: HistoryEntry,
        recipe: Recipe,
        draft: Option<DraftStamp>,
    ) -> Self {
        let (width, height) = source.dimensions();
        #[cfg(test)]
        context.note_compile();
        let compiled = registry.compile(width, height, &recipe);
        Self {
            bound: Arc::new(Bound {
                entry: Arc::new(entry),
                recipe: Some(recipe),
                registry,
                context,
                fingerprint: source.fingerprint().to_owned(),
                draft,
                compiled,
            }),
            source,
        }
    }

    /// The buffer the stack is evaluated on.
    pub fn source(&self) -> &PreviewSource {
        &self.source
    }

    /// Render the stack at the exact phase under `cancel` from its one compilation, which it
    /// borrows: nothing is compiled again, and a stack the host cannot evaluate answers with the
    /// reason it could not be compiled.
    pub(crate) fn exact(&self, cancel: &Cancel) -> Result<Render<'_>, Error> {
        Render::shared(
            self.source.input(),
            self.compiled()?,
            RenderOptions::exact(cancel),
            self.context(),
        )
    }
}

impl<S: std::fmt::Debug> std::fmt::Debug for Evaluation<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Evaluation")
            .field("entry_id", &self.entry().id)
            .field("draft", &self.bound.draft)
            .field("stage", &self.stage())
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// What an evaluation is for, which decides the one rule a RAW white balance is evaluated under.
#[derive(Clone, Copy, Debug)]
pub(super) enum Purpose {
    /// Every evaluation but a preview: strict ([`RawSettingsMode::Strict`]).
    Exact,
    /// A preview, of the first `layer_count` layers when it names a count. An open draft's preview
    /// is the one evaluation that may approximate ([`RawSettingsMode::DraftPreview`]); a preview
    /// of a saved entry is strict.
    Preview { layer_count: Option<usize> },
}

/// An [`Evaluation`] as its builder holds it on the catalog owner, before it reads a source: with
/// the asset whose original it reads and the rule a RAW white balance is evaluated under, which
/// decide what a `preparation-required` refusal of it names.
pub(super) struct Stack {
    asset: AssetRecord,
    rule: RawSettingsMode,
    evaluation: Evaluation<()>,
}

impl Stack {
    /// The stack a refusal of this evaluation names ([`EditorService::needing`]): the development a
    /// RAW source must hold is the evaluated stack's own, except for a drafted preview, which
    /// approximates on the development its entry holds.
    fn needs(&self) -> Evaluated<'_> {
        let entry = self.evaluation.entry();
        let recipe = self.evaluation.recipe();
        Evaluated {
            asset: &self.asset,
            entry_id: &entry.id,
            recipe,
            developed: match self.rule {
                RawSettingsMode::DraftPreview => &entry.snapshot.recipe,
                RawSettingsMode::Strict => recipe,
            },
        }
    }
}

impl EditorService {
    /// The one way an evaluation is built: `selection`'s entry and stack, read from the entry cache
    /// without copying the entry, and for a draft planned against the current stack at the revision
    /// it holds now; admitted as a stack of the asset's source kind; its artifacts bound; and
    /// compiled once against the asset's content stage. No pixel is read and no source is prepared:
    /// [`Self::read`] reads the source. A stack the host cannot compile is kept with its reason,
    /// for the caller to answer. `O(layers)` plus the draft's plan; a refusal to bind names what
    /// this evaluation needs prepared, under the rule `purpose` decides.
    pub(super) fn evaluation(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
        purpose: Purpose,
    ) -> Result<Stack, Error> {
        let (asset, entry, drafted) = match selection {
            AnalysisSelection::Current => {
                let (asset, entry) = self.saved_entry(asset_id, None)?;
                (asset, entry, None)
            }
            // A historical entry answers from its own immutable stack, so a later commit by any
            // client never relabels what it evaluates as current.
            AnalysisSelection::Entry(entry_id) => {
                let (asset, entry) = self.saved_entry(asset_id, Some(entry_id))?;
                (asset, entry, None)
            }
            // A framed entry is evaluated from its own immutable stack with another immutable
            // entry's geometry tail: both are frozen, so the composition is too. It is never
            // persisted and never relabelled; its identity hashes the composed stack.
            AnalysisSelection::Framed {
                entry: entry_id,
                geometry,
            } => {
                let (asset, entry) = self.saved_entry(asset_id, Some(entry_id))?;
                let (_, framing) = self.saved_entry(asset_id, Some(geometry))?;
                let recipe = framed(
                    &self.registry,
                    &entry.snapshot.recipe,
                    &framing.snapshot.recipe,
                )?;
                validate_source_recipe(&self.registry, &asset, &recipe)?;
                (asset, entry, Some((recipe, None)))
            }
            // A draft is evaluated at the revision it holds now: its effective recipe is planned
            // against the current stack and never persisted.
            AnalysisSelection::Draft(draft) => {
                let (recipe, head, entry) = self.planned_draft(asset_id, draft)?;
                validate_source_recipe(&self.registry, &head.asset, &recipe)?;
                let stamp = DraftStamp {
                    draft_id: draft.draft_id.clone(),
                    draft_revision: draft.draft_revision,
                };
                (head.asset, entry, Some((recipe, Some(stamp))))
            }
        };
        if let Purpose::Preview {
            layer_count: Some(count),
        } = purpose
        {
            let layers = drafted
                .as_ref()
                .map_or(&entry.snapshot.recipe, |(recipe, _)| recipe)
                .layers
                .len();
            if count > layers {
                return Err(Error::validation(format!(
                    "preview layer count {count} exceeds the {layers} layers of this entry"
                )));
            }
        }
        // A draft's effective recipe decides the RAW development settings too, so a drafted
        // exposure previews the value the gesture holds rather than the committed one. A drafted
        // temperature or tint the developed planes do not hold is approximated on them, and only
        // in a drafted preview: that is the one evaluation that may, because its frame is a
        // gesture's preview and is labelled so, never analysed and replaced by the exact frame once
        // the release redevelops. Every other evaluation is strict. So a drafted preview that finds
        // no development at all names the one its entry holds, which the gesture's release
        // redevelops from anyway, rather than one per drafted value.
        let rule = match (purpose, &drafted) {
            (Purpose::Preview { .. }, Some((_, Some(_)))) => RawSettingsMode::DraftPreview,
            _ => RawSettingsMode::Strict,
        };
        let needs = |recipe| Evaluated {
            asset: &asset,
            entry_id: &entry.id,
            recipe,
            developed: match rule {
                RawSettingsMode::DraftPreview => &entry.snapshot.recipe,
                RawSettingsMode::Strict => recipe,
            },
        };
        // The evaluated stack carries the verified bytes of every artifact it references, so a
        // cache eviction never breaks it on a worker. An entry whose stack already holds them is
        // evaluated as the cache shares it, uncopied.
        let (recipe, draft) = match drafted {
            Some((mut recipe, stamp)) => {
                let bound = self.bind_artifacts(&mut recipe);
                self.needing(needs(&recipe), bound)?;
                (Some(recipe), stamp)
            }
            None => match self.bound(&entry.snapshot.recipe) {
                Ok(Cow::Borrowed(_)) => (None, None),
                Ok(Cow::Owned(recipe)) => (Some(recipe), None),
                Err(error) => return self.needing(needs(&entry.snapshot.recipe), Err(error)),
            },
        };
        // The stack's one compilation. It reads the asset record's dimensions, so a stack the host
        // cannot evaluate is known without preparing the original; the source a reader prepares
        // has those dimensions ([`Self::read`]), so its renders reuse this compilation.
        let evaluated = recipe.as_ref().unwrap_or(&entry.snapshot.recipe);
        #[cfg(test)]
        self.render.note_compile();
        let compiled = self.registry.compile(asset.width, asset.height, evaluated);
        let fingerprint = asset.fingerprint.clone();
        Ok(Stack {
            asset,
            rule,
            evaluation: Evaluation {
                bound: Arc::new(Bound {
                    entry,
                    recipe,
                    registry: self.registry.clone(),
                    context: self.render.clone(),
                    fingerprint,
                    draft,
                    compiled,
                }),
                source: (),
            },
        })
    }

    /// `stack` reading the buffer it is evaluated on, chosen by the asset's source interpretation —
    /// the decoded JPEG, or the developed RAW planes with the linear settings the stack asks for
    /// under its rule — with the capture metadata read when the original was prepared. A cached
    /// source verification; no pixel is read. A RAW stack whose white balance the prepared planes
    /// do not hold is `preparation-required` unless its rule approximates it, and every refusal
    /// names what `stack` needs.
    fn read(&self, stack: Stack) -> Result<(Evaluation, Arc<CaptureMetadata>), Error> {
        let needs = stack.needs();
        let prepared =
            self.needing(needs, self.verified_prepared(&stack.asset, needs.developed))?;
        let capture = capture_of(&prepared);
        let source = self.needing(
            needs,
            source_of(prepared, stack.evaluation.recipe(), stack.rule),
        )?;
        // The compilation was made against the asset record's content stage and is rendered
        // against this buffer, so the two must be one stage of one original.
        if source.dimensions() != (stack.asset.width, stack.asset.height)
            || source.fingerprint() != stack.asset.fingerprint
        {
            return Err(Error::internal(
                "the prepared source is not the original the asset records",
            ));
        }
        Ok((stack.evaluation.reading(source), capture))
    }

    /// [`Self::evaluation`] of `selection` under the strict rule, reading its source.
    fn exact_evaluation(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
    ) -> Result<Evaluation, Error> {
        let stack = self.evaluation(asset_id, selection, Purpose::Exact)?;
        Ok(self.read(stack)?.0)
    }

    pub fn render_current(&self, asset_id: &AssetId) -> Result<Raster, Error> {
        self.render_selected(asset_id, AnalysisSelection::Current)
    }

    /// A preview job for one entry, or for an open draft's effective recipe. `layer_count`
    /// truncates the rendered stack to its first `n` layers, which the desktop uses to show a
    /// layer's input stage while drafting it; it must not exceed the rendered stack's layer count.
    /// A draft previews the current entry, so naming a historical one beside it is refused.
    /// `proxy` offers the display bounds the frame will be shown in; the queue decides what to do
    /// with them, and this call reads no pixels either way.
    ///
    /// The job carries the stack's one compilation to its worker, which renders the exact phase
    /// from it: a stack the host cannot evaluate is not refused here but answered by that phase.
    pub fn preview_job(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
        layer_count: Option<usize>,
        draft: Option<&Draft>,
        proxy: Option<ProxyBounds>,
    ) -> Result<PreviewJob, Error> {
        let selection = match (draft, entry_id) {
            (Some(draft), Some(entry_id)) => {
                if entry_id != &self.current_entry_id(asset_id)? {
                    return Err(Error::validation(
                        "a draft previews the current entry, not a historical one",
                    ));
                }
                AnalysisSelection::Draft(draft)
            }
            (Some(draft), None) => AnalysisSelection::Draft(draft),
            (None, Some(entry_id)) => AnalysisSelection::Entry(entry_id),
            (None, None) => AnalysisSelection::Current,
        };
        self.selected_preview_job(asset_id, selection, layer_count, proxy)
    }

    /// A whole preview job for one saved entry framed by another entry's geometry
    /// ([`AnalysisSelection::Framed`]): what Compare shows. Like any preview job its identity is
    /// its evaluation's, which hashes the composed stack, so its frame and histogram are never
    /// confused with the entry's own.
    pub(crate) fn framed_preview_job(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        geometry: &EntryId,
        proxy: Option<ProxyBounds>,
    ) -> Result<PreviewJob, Error> {
        self.selected_preview_job(
            asset_id,
            AnalysisSelection::Framed {
                entry: entry_id,
                geometry,
            },
            None,
            proxy,
        )
    }

    fn selected_preview_job(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
        layer_count: Option<usize>,
        proxy: Option<ProxyBounds>,
    ) -> Result<PreviewJob, Error> {
        let stack = self.evaluation(asset_id, selection, Purpose::Preview { layer_count })?;
        // The identity is the evaluation's, exactly as an analysis job's is, so a report the
        // preview worker produces from this frame is a cache hit for a later `analysis.request`.
        let mut job = PreviewJob::new(self.read(stack)?.0)?;
        job.layer_count = layer_count;
        // A truncated job's proxy phase is planned on the worker from the layer prefix it renders,
        // never from the whole stack, so the bounds are offered to it as to any other job.
        job.proxy = proxy;
        Ok(job)
    }

    /// Everything one analysis job needs, planned on the catalog owner: the identity that names the
    /// result and the evaluation to render, or why the stack has no output stage. Costs a state
    /// read, a cached source verification and an `O(layers)` plan and compile; no frame is
    /// allocated here and nothing is persisted.
    pub(crate) fn analysis_plan(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
    ) -> Result<AnalysisPlan, Error> {
        let stack = self.evaluation(asset_id, selection, Purpose::Exact)?;
        let identity = stack.evaluation.identity()?;
        // A stack the host cannot evaluate at all is reported failed without decoding or
        // developing the original: there is no frame for that job to render. Only an evaluable
        // stack reads the prepared source, and an analysis is a number, so it is never taken from
        // an approximate white balance.
        let evaluation = match stack.evaluation.compiled() {
            Err(failure) => Err(failure),
            Ok(_) => Ok(self.read(stack)?.0),
        };
        Ok(AnalysisPlan {
            identity,
            evaluation,
        })
    }

    /// What `export.plan` answers about one saved entry, the current one unless `entry_id` names
    /// another: its evaluation, which names its identity with the output stage, and where its
    /// original lives. The same `O(layers)` planning an analysis does, without the prepared source:
    /// nothing is read but the catalog, so an unprepared original is no reason to refuse. A stack
    /// the host cannot evaluate is refused with the reason it has no output stage.
    pub(crate) fn export_target(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<ExportTarget, Error> {
        let Stack {
            asset, evaluation, ..
        } = self.evaluation(asset_id, saved(entry_id), Purpose::Exact)?;
        evaluation.compiled()?;
        Ok(ExportTarget {
            evaluation,
            original: asset.locator,
        })
    }

    /// Everything one export job needs, frozen on the catalog owner: the saved entry's identity,
    /// its evaluation — the stack bound with the verified bytes of the artifacts it references, the
    /// verified prepared source evaluated exactly (a RAW development must hold the entry's own
    /// white balance) and its compilation — and the original's capture metadata. Costs what
    /// [`Self::analysis_plan`] costs; no frame is allocated here. A stack the host cannot evaluate
    /// is refused with its reason, a missing or changed original with `source-unavailable`, and an
    /// unprepared one with `preparation-required` naming its needs.
    pub(crate) fn export_plan(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<ExportPlan, Error> {
        let stack = self.evaluation(asset_id, saved(entry_id), Purpose::Exact)?;
        stack.evaluation.compiled()?;
        let identity = stack.evaluation.identity()?;
        let (evaluation, capture) = self.read(stack)?;
        Ok(ExportPlan {
            identity,
            evaluation,
            capture,
        })
    }

    /// Render one saved entry exactly, as an export does.
    pub fn render_entry(&self, asset_id: &AssetId, entry_id: &EntryId) -> Result<Raster, Error> {
        self.render_selected(asset_id, AnalysisSelection::Entry(entry_id))
    }

    /// Render one saved entry's stack exactly from its one compilation.
    fn render_selected(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
    ) -> Result<Raster, Error> {
        let evaluation = self.exact_evaluation(asset_id, selection)?;
        evaluation
            .exact(&Cancel::never())?
            .frame(evaluation.entry().snapshot.id.clone())
    }

    /// Evaluate one output pixel of a saved entry without rasterizing the image.
    pub fn sample_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        x: u32,
        y: u32,
    ) -> Result<PixelSample, Error> {
        self.point_entry(asset_id, entry_id, x, y)?.evaluate()
    }

    /// Plan one output pixel of a saved entry for evaluation here or on another thread: a state
    /// read, a cached source verification, the entry's artifacts bound and one `O(layers)` compile
    /// that refuses a stack the host cannot evaluate now. No pixel is read.
    pub(crate) fn point_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        x: u32,
        y: u32,
    ) -> Result<PointPlan, Error> {
        self.point_selected(asset_id, AnalysisSelection::Entry(entry_id), x, y)
    }

    /// [`Self::point_entry`] for any saved selection, a framed one included.
    pub(crate) fn point_selected(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
        x: u32,
        y: u32,
    ) -> Result<PointPlan, Error> {
        let evaluation = self.exact_evaluation(asset_id, selection)?;
        PointPlan::new(evaluation, x, y)
    }

    /// Bind the asset's current entry for sampling off the catalog owner: its evaluation, compiled
    /// once here so a stack the host cannot evaluate is refused now. It binds the stack like
    /// [`Self::sample_entry`], so an unprepared source or artifact is `preparation-required`. A
    /// state read, a cached source verification and an `O(layers)` compile; no pixel is read.
    pub(crate) fn sample_plan(&self, asset_id: &AssetId) -> Result<SamplePlan, Error> {
        // Samples are numbers sent to a provider, so a white balance the planes do not hold is
        // `preparation-required` here, as it is for `render.sample`.
        let evaluation = self.exact_evaluation(asset_id, AnalysisSelection::Current)?;
        evaluation.compiled()?;
        Ok(SamplePlan { evaluation })
    }

    /// Plan one output pixel of an open draft's effective recipe, as [`Self::point_entry`] plans a
    /// saved entry's: the draft's action is planned against the current stack at the revision the
    /// draft holds now, and the plan names the current entry and that revision.
    pub(crate) fn point_draft(
        &self,
        asset_id: &AssetId,
        draft: &Draft,
        x: u32,
        y: u32,
    ) -> Result<PointPlan, Error> {
        // A sampled code is a number, so a drafted white balance the planes do not hold is
        // `preparation-required` here even while the draft's preview approximates it, and the
        // refusal names a development at the drafted white balance.
        let evaluation = self.exact_evaluation(asset_id, AnalysisSelection::Draft(draft))?;
        PointPlan::new(evaluation, x, y)
    }

    /// Map one output pixel of a saved entry back to the pixel of the content stage it shows: the
    /// source after EXIF orientation, which is the stage a pixel-stage edit addresses. Like
    /// `sample_entry` it answers from the compiled stack and rasterizes nothing, and it reads no
    /// source.
    pub fn locate_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        x: u32,
        y: u32,
    ) -> Result<ContentPoint, Error> {
        self.locate_selected(asset_id, AnalysisSelection::Entry(entry_id), x, y)
    }

    /// [`Self::locate_entry`] for any saved selection, a framed one included.
    pub(crate) fn locate_selected(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
        x: u32,
        y: u32,
    ) -> Result<ContentPoint, Error> {
        let stack = self.evaluation(asset_id, selection, Purpose::Exact)?;
        locate(
            stack.evaluation.compiled()?,
            stack.asset.width,
            stack.asset.height,
            x,
            y,
        )
    }

    /// The content-to-output map of a saved entry's geometry tail, both ways. `locate_entry`
    /// answers one point; this answers all of them at once, so a gesture over the photograph maps
    /// pointer positions itself instead of asking per move. Like `locate_entry` it reads the compiled
    /// stack only and rasterizes nothing.
    pub fn transform_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
    ) -> Result<MappingDescriptor, Error> {
        self.transform_selected(asset_id, AnalysisSelection::Entry(entry_id))
    }

    /// [`Self::transform_entry`] for any saved selection, a framed one included.
    pub(crate) fn transform_selected(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
    ) -> Result<MappingDescriptor, Error> {
        let stack = self.evaluation(asset_id, selection, Purpose::Exact)?;
        let geometry = transform_of(
            stack.evaluation.compiled()?,
            stack.asset.width,
            stack.asset.height,
        )?;
        let evaluation = &stack.evaluation;
        let entry = evaluation.entry();
        Ok(MappingDescriptor {
            entry_id: entry.id.clone(),
            snapshot_id: entry.snapshot.id.clone(),
            source_fingerprint: evaluation.bound.fingerprint.clone(),
            draft: evaluation.draft().cloned(),
            geometry,
        })
    }
}

/// The buffer a prepared source is evaluated on under `mode`: the decoded JPEG, or the developed
/// RAW planes with the linear settings `recipe` asks for ([`raw_settings`]). `O(1)`; it reads no
/// pixel.
pub(super) fn source_of(
    prepared: PreparedSource,
    recipe: &Recipe,
    mode: RawSettingsMode,
) -> Result<PreviewSource, Error> {
    match prepared {
        PreparedSource::Jpeg(image) => Ok(PreviewSource::Jpeg(image)),
        PreparedSource::Raw(raw) => {
            let settings = raw_settings(&raw, recipe, mode)?;
            Ok(PreviewSource::Raw {
                image: raw
                    .linear
                    .ok_or_else(|| Error::preparation_required("RAW development required"))?,
                settings,
            })
        }
    }
}

/// One output pixel planned on the catalog owner and evaluated wherever its caller chooses: the
/// evaluation it reads, whose entry, snapshot and draft revision the answer names whatever is
/// committed before it is evaluated. It shares the evaluation and owns nothing that scales with the
/// image.
pub(crate) struct PointPlan {
    evaluation: Evaluation,
    /// Whether the point evaluates a spatial tile: the only point that costs more than
    /// `O(layers)`, and the one the catalog owner hands to its point worker.
    spatial: bool,
    x: u32,
    y: u32,
}

impl PointPlan {
    /// The point `(x, y)` of `evaluation`, refused now when no evaluation of it could answer: a
    /// stack the host cannot compile, or a source the stack cannot be rendered against.
    fn new(evaluation: Evaluation, x: u32, y: u32) -> Result<Self, Error> {
        let spatial = evaluation.exact(&Cancel::never())?.evaluates_spatial();
        Ok(Self {
            evaluation,
            spatial,
            x,
            y,
        })
    }

    /// Whether evaluating this point evaluates a spatial tile, the declared exception to point
    /// queries costing `O(layers)`.
    pub(crate) fn evaluates_spatial(&self) -> bool {
        self.spatial
    }

    /// Evaluate the point from the plan's own compilation: `O(layers)`, and through a spatial
    /// layer the one tile that contains it, whose byte is the byte a render writes there. A point
    /// outside the rendered image is a validation error naming the stage it missed.
    pub(crate) fn evaluate(self) -> Result<PixelSample, Error> {
        self.evaluate_cancelled(&Cancel::never())
    }

    pub(crate) fn evaluate_cancelled(self, cancel: &Cancel) -> Result<PixelSample, Error> {
        let Self {
            evaluation, x, y, ..
        } = self;
        let sampled = evaluation.exact(cancel)?.sample(x, y)?;
        let rgba = sampled.rgba.ok_or_else(|| {
            Error::validation(format!(
                "sample ({x}, {y}) is outside the {}x{} rendered image",
                sampled.width, sampled.height
            ))
        })?;
        let entry = evaluation.entry();
        Ok(PixelSample {
            entry_id: entry.id.clone(),
            snapshot_id: entry.snapshot.id.clone(),
            source_fingerprint: evaluation.source().fingerprint().to_owned(),
            width: sampled.width,
            height: sampled.height,
            x,
            y,
            rgba,
            draft: evaluation.draft().cloned(),
        })
    }
}

/// `entry` framed by `framing`'s geometry: `entry`'s stack with its geometry layers removed and
/// `framing`'s geometry layers, in their stored order, placed where the host places a geometry
/// layer — before the first finish layer. Every other layer, the mask table and the strokes are
/// `entry`'s own, so only the framing changes. A geometry layer carries no mask, so no mask
/// reference crosses entries. `O(layers)` descriptor lookups; no pixel is read.
///
/// A layer of `framing` whose effect no provider declares is refused rather than guessed at or
/// left out: whether it frames the photograph is exactly what its missing descriptor would say.
fn framed(registry: &ModuleRegistry, entry: &Recipe, framing: &Recipe) -> Result<Recipe, Error> {
    let stage = |layer: &crate::Layer| registry.effect_stage(&layer.effect_id);
    if let Some(unknown) = framing.layers.iter().find(|layer| stage(layer).is_none()) {
        return Err(Error::validation(format!(
            "the framing entry holds a layer of {}, which no provider declares, so its geometry cannot be applied",
            unknown.effect_id
        )));
    }
    let mut recipe = entry.clone();
    recipe
        .layers
        .retain(|layer| stage(layer) != Some(EffectStage::Geometry));
    let at = registry.insertion_index(&recipe.layers, EffectStage::Geometry, u16::MAX);
    recipe.layers.splice(
        at..at,
        framing
            .layers
            .iter()
            .filter(|layer| stage(layer) == Some(EffectStage::Geometry))
            .cloned(),
    );
    Ok(recipe)
}

/// A saved entry of an asset: the named one, or its current entry.
fn saved(entry_id: Option<&EntryId>) -> AnalysisSelection<'_> {
    match entry_id {
        Some(entry_id) => AnalysisSelection::Entry(entry_id),
        None => AnalysisSelection::Current,
    }
}

/// The capture metadata the source worker read from the original when it prepared it.
fn capture_of(prepared: &PreparedSource) -> Arc<CaptureMetadata> {
    match prepared {
        PreparedSource::Jpeg(image) => image.capture.clone(),
        PreparedSource::Raw(raw) => raw.capture.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewQueue;
    use crate::editor::test_support::{
        SHRINK_ACTION, ShrinkModule, fixture, mutation, shrink, temp,
    };

    fn geometry_photo() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/geometry/z6-24-70-35mm-grid.jpg")
    }

    fn select_lens(service: &mut EditorService, asset: &AssetId) {
        let row = luxforge_testbase::wait_for("the offline lens index", || {
            let entry = service.state(asset).unwrap().current_entry.id;
            match service.run_query(
                asset,
                &entry,
                "lens-profiles",
                serde_json::json!({"assume-uncorrected":true}),
            ) {
                Ok(rows) => {
                    let suggestion = rows["status"]["suggestion"].clone();
                    assert!(
                        suggestion["match"] == "lens-model" && suggestion["eligible"] == true,
                        "{rows}"
                    );
                    Some(suggestion)
                }
                Err(error) if error.kind == ErrorKind::NotReady => None,
                Err(error) => panic!("{error}"),
            }
        });
        let revision = service.state(asset).unwrap().revision;
        service
            .apply_action(
                asset,
                mutation(revision, "lens"),
                "select-lens-profile",
                serde_json::json!({"profile":row["key"],"assume-uncorrected":true}),
            )
            .unwrap();
    }

    #[test]
    fn orientation_carries_perspective_and_crop_in_one_action() {
        use crate::{Orientation, Transform};
        let orientations: Vec<_> = [false, true]
            .into_iter()
            .flat_map(|mirror| (0..4).map(move |turns| Orientation { mirror, turns }))
            .collect();
        for (index, target) in orientations.iter().enumerate() {
            let catalog = temp(&format!("carry-service-{index}.sqlite"));
            let mut service = EditorService::open(&catalog).unwrap();
            let imported = service.import(&geometry_photo()).unwrap();
            let asset = imported.asset.id;
            let original = imported.current_entry.id;
            select_lens(&mut service, &asset);
            service
                .apply_action(
                    &asset,
                    mutation(1, "perspective"),
                    "set-perspective",
                    serde_json::json!({"horizontal":40,"vertical":-25}),
                )
                .unwrap();
            service
                .apply_action(
                    &asset,
                    mutation(2, "crop"),
                    "crop",
                    serde_json::json!({"angle":2.5,"x":0.1,"y":0.1,"width":0.7,"height":0.7}),
                )
                .unwrap();
            let action = [
                Transform::RotateRight,
                Transform::RotateLeft,
                Transform::MirrorHorizontal,
                Transform::FlipVertical,
            ][index % 4];
            let ahead = orientations
                .iter()
                .find(|ahead| ahead.then(action) == *target)
                .unwrap();
            let mut revision = 3;
            if ahead.mirror {
                service
                    .apply_transform(
                        &asset,
                        mutation(revision, "setup-mirror"),
                        Transform::MirrorHorizontal,
                    )
                    .unwrap();
                revision += 1;
            }
            for turn in 0..ahead.turns {
                service
                    .apply_transform(
                        &asset,
                        mutation(revision, &format!("setup-turn-{turn}")),
                        Transform::RotateRight,
                    )
                    .unwrap();
                revision += 1;
            }
            let before = service.state(&asset).unwrap();
            let history = service.history(&asset, None, 100).unwrap().entries.len();
            let lens = before
                .current_entry
                .snapshot
                .recipe
                .layers
                .iter()
                .find(|layer| layer.effect_id == crate::LENS_EFFECT)
                .unwrap();
            let crop = before
                .current_entry
                .snapshot
                .recipe
                .layers
                .iter()
                .find(|layer| layer.effect_id == crate::CROP_EFFECT)
                .unwrap();
            let perspective = before
                .current_entry
                .snapshot
                .recipe
                .layers
                .iter()
                .find(|layer| layer.effect_id == crate::PERSPECTIVE_EFFECT)
                .unwrap();
            service
                .apply_transform(&asset, mutation(revision, "carry"), action)
                .unwrap();
            let after = service.state(&asset).unwrap();
            assert_eq!(after.revision, revision + 1);
            assert_eq!(
                service.history(&asset, None, 100).unwrap().entries.len(),
                history + 1
            );
            let layers = &after.current_entry.snapshot.recipe.layers;
            assert_eq!(
                layers
                    .iter()
                    .find(|layer| layer.effect_id == crate::LENS_EFFECT)
                    .unwrap(),
                lens
            );
            assert_eq!(
                layers
                    .iter()
                    .find(|layer| layer.effect_id == crate::CROP_EFFECT)
                    .unwrap()
                    .id,
                crop.id
            );
            assert_eq!(
                layers
                    .iter()
                    .find(|layer| layer.effect_id == crate::PERSPECTIVE_EFFECT)
                    .unwrap()
                    .id,
                perspective.id
            );
            let held: Orientation = serde_json::from_value(
                layers
                    .iter()
                    .find(|layer| layer.effect_id == crate::ORIENTATION_EFFECT)
                    .unwrap()
                    .payload
                    .clone(),
            )
            .unwrap();
            assert_eq!(held, *target);
            let output = service
                .transform_entry(&asset, &after.current_entry.id)
                .unwrap();
            let expected = if target.turns % 2 == 0 {
                (600, 400)
            } else {
                (400, 600)
            };
            assert_eq!((output.content.width, output.content.height), (600, 400));
            assert_eq!(
                service
                    .registry
                    .compile(
                        600,
                        400,
                        &Recipe {
                            layers: layers
                                .iter()
                                .filter(|layer| layer.effect_id != crate::CROP_EFFECT)
                                .cloned()
                                .collect(),
                            ..Recipe::default()
                        }
                    )
                    .unwrap()
                    .stage(),
                crate::Stage {
                    width: expected.0,
                    height: expected.1
                }
            );
            let untouched = service.render_entry(&asset, &original).unwrap();
            assert_eq!((untouched.width, untouched.height), (600, 400));
            assert!(
                service
                    .entry(&asset, &original)
                    .unwrap()
                    .snapshot
                    .recipe
                    .layers
                    .is_empty()
            );
            drop(service);
            std::fs::remove_file(catalog).unwrap();
        }
    }

    #[test]
    fn crop_input_preview_includes_warps() {
        let catalog = temp("warp-crop-prefix.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&geometry_photo()).unwrap().asset.id;
        select_lens(&mut service, &asset);
        service
            .apply_action(
                &asset,
                mutation(1, "perspective"),
                "set-perspective",
                serde_json::json!({"horizontal":40,"vertical":-25}),
            )
            .unwrap();
        let covered = service.render_current(&asset).unwrap();
        service
            .apply_action(
                &asset,
                mutation(2, "crop"),
                "crop",
                serde_json::json!({"x":0.1,"y":0.1,"width":0.7,"height":0.7}),
            )
            .unwrap();
        let layers = &service
            .state(&asset)
            .unwrap()
            .current_entry
            .snapshot
            .recipe
            .layers;
        let count = layers
            .iter()
            .position(|layer| layer.effect_id == crate::CROP_EFFECT)
            .unwrap();
        let job = service
            .preview_job(
                &asset,
                None,
                Some(count),
                None,
                Some(ProxyBounds {
                    width: 120,
                    height: 80,
                }),
            )
            .unwrap();
        assert!(matches!(
            job.evaluation
                .exact(&Cancel::never())
                .unwrap()
                .transform()
                .unwrap()
                .mapping,
            crate::MappingShape::Warp { .. }
        ));
        let mut queue = PreviewQueue::default();
        queue.request(job);
        let proxy = luxforge_testbase::wait_for("warp prefix proxy", || queue.poll());
        assert_eq!(proxy.phase(), crate::PreviewPhase::Proxy);
        assert_eq!(
            (
                proxy.raster().unwrap().width,
                proxy.raster().unwrap().height
            ),
            (120, 80)
        );
        let exact = luxforge_testbase::wait_for("warp prefix exact", || queue.poll())
            .into_raster()
            .unwrap();
        assert_eq!((exact.width, exact.height), (600, 400));
        assert_eq!(exact.rgba, covered.rgba);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn keep_geometry_copies_the_whole_optional_warp_tail_and_original_excludes_it() {
        let registry = ModuleRegistry::builtin();
        let make = |id: &str, payload: serde_json::Value| crate::Layer {
            id: crate::LayerId::new(),
            effect_id: id.into(),
            effect_format: crate::EFFECT_FORMAT,
            payload,
            mask: None,
            artifacts: Vec::new(),
        };
        let original = Recipe::default();
        let framing = Recipe {
            layers: vec![
                make(crate::BASIC_EFFECT, serde_json::json!({"exposure":1.0})),
                crate::Layer::orientation(crate::Orientation::of(crate::Transform::RotateRight)),
                crate::render::testing::frozen_lens(320, 480, 35.0),
                make(
                    "luxforge.perspective",
                    serde_json::json!({"horizontal":35,"vertical":-25}),
                ),
                crate::Layer::crop(crate::render::tests::fitted_crop(
                    320,
                    480,
                    7.0,
                    [0.1, 0.1, 0.7, 0.7],
                )),
            ],
            ..Recipe::default()
        };
        let kept = framed(&registry, &original, &framing).unwrap();
        assert_eq!(kept.layers, framing.layers[1..]);
        assert!(original.layers.is_empty());
        let map = crate::stage_transform(&registry, 480, 320, &kept).unwrap();
        assert!(matches!(map.mapping, crate::MappingShape::Warp { .. }));
        assert!(matches!(
            crate::stage_transform(&registry, 480, 320, &original)
                .unwrap()
                .mapping,
            crate::MappingShape::Affine { .. }
        ));
    }

    #[test]
    fn a_truncated_preview_job_renders_the_layer_prefix_and_rejects_an_out_of_range_count() {
        let catalog = temp("truncated-preview.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.render_current(&asset).unwrap().pixel(0, 0).unwrap();
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 0, 0, [1, 2, 3])
            .unwrap();
        service
            .apply_action(
                &asset,
                mutation(1, "shrink"),
                SHRINK_ACTION,
                shrink(100, 100),
            )
            .unwrap();
        let rendered = |service: &EditorService, layer_count: Option<usize>| -> Raster {
            let job = service
                .preview_job(&asset, None, layer_count, None, None)
                .unwrap();
            let mut queue = PreviewQueue::default();
            queue.request(job);
            luxforge_testbase::wait_for("the preview worker's answer", || queue.poll())
                .into_raster()
                .unwrap()
        };
        let full = rendered(&service, None);
        assert_eq!((full.width, full.height), (100, 100));
        let prefix = rendered(&service, Some(1));
        assert_eq!(
            (prefix.width, prefix.height),
            (480, 320),
            "one layer renders the crop's input stage"
        );
        assert_eq!(prefix.pixel(0, 0), Some([1, 2, 3, 255]));
        let none = rendered(&service, Some(0));
        assert_eq!((none.width, none.height), (480, 320));
        assert_eq!(none.pixel(0, 0), Some(original), "no layer, no edit");
        assert_ne!(none.pixel(0, 0), prefix.pixel(0, 0));
        let error = service
            .preview_job(&asset, None, Some(3), None, None)
            .expect_err("an out-of-range layer count");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .contains("preview layer count 3 exceeds the 2 layers"),
            "{error}"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A truncated preview job keeps the display bounds it was offered, so the crop's input stage
    /// at Fit has a proxy phase: the layer prefix at display size, then the prefix exactly. The
    /// whole stack's output (100 × 100 after the shrink) never sizes it.
    #[test]
    fn a_truncated_preview_job_with_bounds_gets_a_proxy_phase_of_its_prefix() {
        let catalog = temp("truncated-proxy.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_action(
                &asset,
                mutation(0, "shrink"),
                SHRINK_ACTION,
                shrink(100, 100),
            )
            .unwrap();
        let display = ProxyBounds {
            width: 120,
            height: 120,
        };
        let job = service
            .preview_job(&asset, None, Some(0), None, Some(display))
            .unwrap();
        assert_eq!(job.proxy, Some(display), "the bounds reach the worker");
        let mut queue = PreviewQueue::default();
        queue.request(job);
        let proxy = luxforge_testbase::wait_for("the proxy phase", || queue.poll());
        assert_eq!(proxy.phase(), crate::PreviewPhase::Proxy);
        let frame = proxy.into_raster().unwrap();
        assert_eq!(
            (frame.width, frame.height),
            (120, 80),
            "the 480 × 320 input stage fitted to the bounds"
        );
        let exact = luxforge_testbase::wait_for("the exact phase", || queue.poll());
        assert_eq!(exact.phase(), crate::PreviewPhase::Exact);
        let frame = exact.into_raster().unwrap();
        assert_eq!((frame.width, frame.height), (480, 320));
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Compare's framed Original: the Original's stack with the displayed entry's geometry. Its
    /// frame is exactly the Original's own frame cut to that geometry — the same bytes, only the
    /// framing changed — its size is the framing entry's output size, and its identity is its own.
    #[test]
    fn a_framed_preview_renders_the_entry_with_the_other_entrys_geometry() {
        let catalog = temp("framed-preview.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let imported = service.import(&fixture()).unwrap();
        let asset = imported.asset.id;
        let original = imported.current_entry.id;
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 0, 0, [1, 2, 3])
            .unwrap();
        service
            .apply_action(
                &asset,
                mutation(1, "shrink"),
                SHRINK_ACTION,
                shrink(100, 60),
            )
            .unwrap();
        let current = service.current_entry_id(&asset).unwrap();
        let rendered = |job: PreviewJob| -> Raster {
            let mut queue = PreviewQueue::default();
            queue.request(job);
            luxforge_testbase::wait_for("the preview worker's answer", || queue.poll())
                .into_raster()
                .unwrap()
        };
        let whole = service.render_entry(&asset, &original).unwrap();
        assert_eq!((whole.width, whole.height), (480, 320));
        let edited = rendered(service.preview_job(&asset, None, None, None, None).unwrap());
        assert_eq!((edited.width, edited.height), (100, 60));

        let job = service
            .framed_preview_job(&asset, &original, &current, None)
            .unwrap();
        assert_eq!(job.evaluation.entry().id, original, "it shows the Original");
        assert_eq!(
            (job.identity.width, job.identity.height),
            (100, 60),
            "the frame is laid out at the framing entry's size"
        );
        let own = service
            .preview_job(&asset, Some(&original), None, None, None)
            .unwrap();
        assert_ne!(
            job.identity, own.identity,
            "a framed frame is never taken for the entry's own"
        );
        let effects: Vec<&str> = job
            .evaluation
            .recipe()
            .layers
            .iter()
            .map(|layer| layer.effect_id.as_str())
            .collect();
        assert_eq!(
            effects,
            [crate::editor::test_support::SHRINK_EFFECT],
            "the Original's adjustments, none, with the current geometry"
        );
        let framed = rendered(job);
        assert_eq!((framed.width, framed.height), (100, 60));
        for y in 0..60 {
            for x in 0..100 {
                assert_eq!(framed.pixel(x, y), whole.pixel(x, y), "({x}, {y})");
            }
        }
        assert_ne!(
            framed.pixel(0, 0),
            edited.pixel(0, 0),
            "only the adjustments differ"
        );

        // The point, locate and transform questions answer for the same composed stack.
        let selection = AnalysisSelection::framed(&original, Some(&current));
        let transform = service.transform_selected(&asset, selection).unwrap();
        assert_eq!((transform.output.width, transform.output.height), (100, 60));
        let sample = service
            .point_selected(&asset, selection, 0, 0)
            .unwrap()
            .evaluate()
            .unwrap();
        assert_eq!(sample.entry_id, original);
        assert_eq!(Some(sample.rgba), whole.pixel(0, 0));
        assert!(
            service
                .point_selected(&asset, selection, 100, 0)
                .unwrap()
                .evaluate()
                .is_err(),
            "a point outside the framed output is outside the image"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A drafted preview job compiles its stack once, on the catalog owner, and its worker renders
    /// that compilation: the whole frame, and a viewport region that declines to the whole-frame
    /// path, compile nothing more. Planning the draft's action asks its lazy stage context for no
    /// stage, so the evaluation's compile of the drafted stack is the only one the owner makes. The
    /// frame is byte for byte the one a fresh compilation of the same stack renders.
    #[test]
    fn a_drafted_preview_job_compiles_its_stack_once_and_its_worker_compiles_nothing() {
        let catalog = temp("drafted-preview-compiles.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let mut draft = Draft::new(
            "set-basic",
            asset.clone(),
            service.revision(&asset).unwrap(),
        );
        draft.merge(serde_json::Map::from_iter([(
            "exposure".to_owned(),
            serde_json::json!(0.5),
        )]));
        let context = service.render_context().clone();
        let before = context.compiles();
        crate::modules::stack_compiles::take();
        let job = service
            .preview_job(&asset, None, None, Some(&draft), None)
            .unwrap();
        assert_eq!(
            crate::modules::stack_compiles::take(),
            1,
            "the owner compiled the drafted stack once, for its evaluation, and nothing to plan it"
        );
        assert_eq!(
            context.compiles() - before,
            1,
            "the evaluated stack was compiled once"
        );
        let evaluation = &job.evaluation;
        let reference = crate::render(
            evaluation.registry(),
            evaluation.source(),
            evaluation.recipe(),
            RenderOptions::default(),
            &RenderContext::new(),
        )
        .unwrap()
        .frame(evaluation.entry().snapshot.id.clone())
        .unwrap();
        // A region wholly outside the stage clips to nothing, which declines to the whole frame.
        let outside = crate::Region {
            x0: 10_000,
            y0: 10_000,
            width: 4,
            height: 4,
        };
        let mut queue = PreviewQueue::default();
        for viewport in [None, Some(outside)] {
            let mut job = job.clone();
            job.viewport = viewport;
            job.intent = crate::PreviewIntent::Settle;
            queue.request(job);
            let result = luxforge_testbase::wait_for("the preview worker's exact answer", || {
                queue.poll().filter(|result| result.exact().is_some())
            });
            assert_eq!(result.viewport_declined.is_some(), viewport.is_some());
            let frame = result.into_raster().unwrap();
            assert_eq!(frame.rgba.as_ref(), reference.rgba.as_ref(), "{viewport:?}");
        }
        assert_eq!(
            context.compiles() - before,
            1,
            "the worker rendered the owner's compilation, the region's fallback included"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// With the original not prepared, as on the catalog owner after a reopen: a refusal names the
    /// stack that was evaluated — a historical entry's sample names that entry, a draft's planning
    /// and a sampling commit name the current one — and a plan that samples nothing needs nothing.
    #[test]
    fn a_refusal_names_the_entry_it_evaluated_and_a_plan_that_samples_nothing_needs_nothing() {
        let catalog = temp("preparation-needs.sqlite");
        let asset = {
            let mut service =
                EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
            let asset = service.import(&fixture()).unwrap().asset.id;
            service
                .apply_action(
                    &asset,
                    mutation(0, "exposure"),
                    "set-basic",
                    serde_json::json!({"exposure": 0.5}),
                )
                .unwrap();
            asset
        };
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let state = service.state(&asset).unwrap();
        let original = service.history(&asset, None, 10).unwrap().entries[1].clone();
        assert_eq!(original.action_id, "original");
        let needs = |entry_id: &EntryId| crate::PreparationNeeds {
            asset_id: asset.clone(),
            entry_id: entry_id.clone(),
            gains: None,
            artifacts: Vec::new(),
        };

        let refused = service
            .sample_entry(&asset, &original.id, 0, 0)
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::PreparationRequired);
        assert_eq!(refused.needs(), Some(&needs(&original.id)));

        // A pixel replacement compares the pixel it would replace, so its plan samples.
        let refused = service
            .apply_pixel(&asset, mutation(1, "pixel"), 0, 0, [1, 2, 3])
            .unwrap_err();
        assert_eq!(refused.needs(), Some(&needs(&state.current_entry.id)));
        let mut draft = Draft::new("set-pixel", asset.clone(), 1);
        draft.merge(serde_json::Map::from_iter([
            ("x".to_owned(), serde_json::json!(0)),
            ("y".to_owned(), serde_json::json!(0)),
            ("rgb".to_owned(), serde_json::json!([1, 2, 3])),
        ]));
        let refused = service.draft_recipe(&asset, &draft).unwrap_err();
        assert_eq!(refused.needs(), Some(&needs(&state.current_entry.id)));

        // A Basic patch and a transform read no pixel, so they commit without the original.
        service
            .apply_action(
                &asset,
                mutation(1, "contrast"),
                "set-basic",
                serde_json::json!({"contrast": 10}),
            )
            .expect("a field patch plans without the original");
        service
            .apply_transform(&asset, mutation(2, "turn"), crate::Transform::RotateRight)
            .expect("a transform plans without the original");
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
